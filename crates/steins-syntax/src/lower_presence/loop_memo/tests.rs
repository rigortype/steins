//! The loop-body cache must be invisible (issue #793): the same source, lowered with
//! the cache and with it forced off, answers the same reads.
//!
//! The comparison is the whole claim — [`loop_memo`](super) is a cost-only change, so
//! "byte-identical by construction" is checked here as "identical on every program
//! this file can write", over hand-picked nests that exercise each way out of a loop
//! and over a few hundred generated ones. Each corpus also asserts it *used* the
//! cache (`hits() > 0`), since a comparison in which the cache never answers compares
//! the walk with itself.

use std::cell::Cell;

use crate::{SourceTree, UnsetSeedFacts};

thread_local! {
    static OFF: Cell<bool> = const { Cell::new(false) };
    static HITS: Cell<usize> = const { Cell::new(0) };
    /// The most records the table held at once.
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn forced_off() -> bool {
    OFF.get()
}

pub(super) fn count_hit() {
    HITS.set(HITS.get() + 1);
}

pub(super) fn note_records(live: usize) {
    PEAK.set(PEAK.get().max(live));
}

/// What one lowering answered: every function scope's `variable.maybe-undefined`
/// reads, and the `unset` pseudo-type's candidates for the script scope.
#[derive(Debug, PartialEq)]
struct Answer {
    maybe_undefined: Vec<(String, u32)>,
    seeds: UnsetSeedFacts,
}

fn lower(src: &str) -> Answer {
    let tree = SourceTree::parse(src);
    let errors = tree.parse_errors();
    assert!(errors.is_empty(), "the fixture must parse: {errors:?}\n{src}");
    let mut maybe_undefined: Vec<(String, u32)> = tree
        .scopes()
        .iter()
        .flat_map(|s| s.maybe_undefined_reads.iter().map(|r| (r.name.clone(), r.span.start)))
        .collect();
    maybe_undefined.sort();
    Answer { maybe_undefined, seeds: tree.unset_seed_facts().clone() }
}

/// The answer with the cache on and off, and how many times the cache answered.
fn both(src: &str) -> (Answer, Answer, usize) {
    HITS.set(0);
    let on = lower(src);
    let hits = HITS.get();
    OFF.set(true);
    let off = lower(src);
    OFF.set(false);
    (on, off, hits)
}

fn assert_same(src: &str) -> (Answer, usize) {
    let (on, off, hits) = both(src);
    assert_eq!(on, off, "the cache changed the answer for:\n{src}");
    (on, hits)
}

fn in_function(body: &str) -> String {
    format!("<?php\nfunction f($c, $d, $a) {{\n{body}\n}}\n")
}

#[test]
fn the_cache_is_off_when_forced_and_answers_otherwise() {
    let src = in_function(
        "foreach ($a as $v) { while ($c) { for ($i = 0; $i < 3; $i++) { echo $x; if ($d) { $x = 1; } } } }",
    );
    let (on, _, hits) = both(&src);
    assert!(hits > 0, "a three-deep nest repeats its inner bodies and must hit");
    assert_eq!(on.maybe_undefined.len(), 1, "the read before the bind is `Maybe`: {on:?}");
}

/// The table is dropped when the outermost loop's reporting walk returns, so it holds
/// one top-level loop's silent walks and not every loop in the function: 2,000
/// sequential `while { for { } }` pairs once held a record each for the whole run,
/// peaking at 2.1 GB where the run without the cache peaks at 61 MB.
#[test]
fn sequential_loops_do_not_accumulate_records() {
    let peak_for = |pairs: usize| {
        let body: String = (0..pairs)
            .map(|i| {
                format!(
                    "while ($c) {{ for ($j = 0; $j < 3; $j++) {{ $v{i} = $j; echo $v{i}; }} }}\n"
                )
            })
            .collect();
        PEAK.set(0);
        let (answer, _) = assert_same(&in_function(&body));
        assert!(answer.maybe_undefined.is_empty(), "{answer:?}");
        PEAK.get()
    };
    let (few, many) = (peak_for(5), peak_for(200));
    assert!(few > 0, "the table must have been used");
    assert_eq!(few, many, "the peak must not grow with the number of sequential loops");
}

#[test]
fn every_way_out_of_a_loop_replays_identically() {
    let bodies = [
        // `break` and `continue` at two levels, with binds on only some of them.
        "foreach ($a as $v) { while ($c) { if ($d) { $x = 1; break; } if ($c) { continue; } echo $x; $y = 1; } echo $y; } echo $x; echo $y;",
        "for ($i = 0; $i < 3; $i++) { foreach ($a as $v) { if ($d) { continue 2; } $p = 1; } echo $p; }",
        // `unset` inside the loops, before and after the read.
        "foreach ($a as $v) { unset($x); for ($i = 0; $i < 3; $i++) { echo $x; $x = $i; unset($y); echo $y; } echo $x; }",
        "$x = 1; while ($c) { unset($x); while ($d) { echo $x; $x = 2; } echo $x; }",
        // Assignments in the conditions.
        "while (($x = g()) !== null) { if (($y = g($x)) && $d) { echo $y; } else { echo $y; } do { echo $z; } while (($z = g())); } echo $x; echo $y; echo $z;",
        "for ($i = ($j = 0); ($k = $i) < 3; $i++) { foreach ($a as $v) { echo $k; echo $j; } } echo $k;",
        // A switch and a try in a loop: `break` means the switch, not the loop.
        "foreach ($a as $v) { switch ($c) { case 1: $x = 1; break; case 2: $y = 1; break; default: $x = 2; } echo $x; echo $y; while ($d) { echo $x; } }",
        "while ($c) { try { $x = g(); foreach ($a as $v) { echo $x; $y = 1; } } catch (\\Exception $e) { echo $y; } finally { echo $x; } echo $y; }",
        // `while (true)` is left by `break` only; a guard narrows the body.
        "while (true) { foreach ($a as $v) { if ($c) { $x = 1; break 2; } } echo $x; } echo $x;",
        "foreach ($a as $v) { if (isset($x)) { foreach ($a as $w) { echo $x; } } else { $x = 1; } echo $x; }",
        // The same inner body reached with different entry states.
        "if ($c) { $x = 1; } foreach ($a as $v) { foreach ($a as $w) { echo $x; echo $y; $y = 1; } } if ($d) { $x = 1; } foreach ($a as $v) { foreach ($a as $w) { echo $x; } }",
    ];
    let mut total_hits = 0;
    let mut total_reads = 0;
    for body in bodies {
        let (answer, hits) = assert_same(&in_function(body));
        total_hits += hits;
        total_reads += answer.maybe_undefined.len();
    }
    assert!(total_hits > 0, "the nests must exercise the cache");
    assert!(total_reads > 0, "and some of them must report, or nothing is compared");
}

/// The seeded run's own state — `seeded_at` — is a cache input and an output.
#[test]
fn the_unset_run_replays_its_seeds_identically() {
    let scripts = [
        // A seed declared before the nest, and one declared inside it.
        "<?php\n/** @var \\DateTime|unset $x */\n$y = 1;\nforeach ($a as $v) {\n    foreach ($b as $w) {\n        echo $x;\n        /** @var \\DateTime|unset $z */\n        $y = 2;\n        echo $z;\n    }\n    echo $z;\n}\necho $x;\n",
        // Two declarations of one name: the later seed wins on the way out.
        "<?php\nwhile ($c) {\n    /** @var \\DateTime|unset $x */\n    $a = 1;\n    while ($d) {\n        echo $x;\n        /** @var \\DateTime|unset $x */\n        $b = 1;\n        echo $x;\n    }\n    echo $x;\n}\necho $x;\n",
        // A `break` and a `continue` between the seed and the read.
        "<?php\nforeach ($a as $v) {\n    /** @var \\DateTime|unset $x */\n    $m = 1;\n    for ($i = 0; $i < 3; $i++) {\n        if ($c) { continue; }\n        if ($d) { break; }\n        $x = new \\DateTime();\n    }\n    echo $x;\n}\n",
    ];
    let mut total_hits = 0;
    let mut total_candidates = 0;
    for src in scripts {
        let (answer, hits) = assert_same(src);
        total_hits += hits;
        total_candidates += answer.seeds.reads.len();
    }
    assert!(total_hits > 0, "the nests must exercise the cache");
    assert!(total_candidates > 0, "and some of them must yield candidates, or nothing is compared");
}

/// A tiny xorshift: the corpus is fixed, so a failure reproduces from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Gen {
    rng: Rng,
    /// Whether a docblock seed may be written: only the script-scope corpus.
    seeds: bool,
}

impl Gen {
    fn var(&mut self) -> String {
        format!("$v{}", self.rng.below(4))
    }

    fn block(&mut self, depth: usize, in_loop: bool) -> String {
        let n = 1 + self.rng.below(3);
        (0..n).map(|_| self.stmt(depth, in_loop)).collect::<Vec<_>>().join("\n")
    }

    fn stmt(&mut self, depth: usize, in_loop: bool) -> String {
        if self.seeds && self.rng.below(6) == 0 {
            let v = self.var();
            return format!("/** @var \\DateTime|unset {v} */\n$tmp = 1;");
        }
        let leaf_only = depth == 0;
        let pick = self.rng.below(if leaf_only { 4 } else { 14 });
        match pick {
            0 => format!("{} = 1;", self.var()),
            1 | 2 => format!("echo {};", self.var()),
            3 => match self.rng.below(if in_loop { 4 } else { 2 }) {
                0 => format!("unset({});", self.var()),
                2 => "break;".to_owned(),
                3 => "continue;".to_owned(),
                _ => format!("{} = g();", self.var()),
            },
            4 | 5 => {
                let (b1, b2) = (self.block(depth - 1, in_loop), self.block(depth - 1, in_loop));
                let guard = match self.rng.below(3) {
                    0 => "$c".to_owned(),
                    1 => format!("isset({})", self.var()),
                    _ => format!("({} = g())", self.var()),
                };
                if self.rng.below(2) == 0 {
                    format!("if ({guard}) {{\n{b1}\n}} else {{\n{b2}\n}}")
                } else {
                    format!("if ({guard}) {{\n{b1}\n}}")
                }
            }
            6 | 7 => format!("while ($c) {{\n{}\n}}", self.block(depth - 1, true)),
            8 => format!("foreach ($a as $k) {{\n{}\n}}", self.block(depth - 1, true)),
            9 => format!("for ($i = 0; $i < 3; $i++) {{\n{}\n}}", self.block(depth - 1, true)),
            10 => format!("do {{\n{}\n}} while ($d);", self.block(depth - 1, true)),
            11 => format!(
                "switch ($c) {{\ncase 1:\n{}\nbreak;\ndefault:\n{}\n}}",
                self.block(depth - 1, true),
                self.block(depth - 1, true)
            ),
            12 => format!(
                "try {{\n{}\n}} catch (\\Exception $e) {{\n{}\n}}",
                self.block(depth - 1, in_loop),
                self.block(depth - 1, in_loop)
            ),
            _ => {
                let (value, block) = (self.var(), self.block(depth - 1, true));
                format!("foreach ($a as $k => {value}) {{\n{block}\n}}")
            }
        }
    }
}

fn corpus(seed: u64, count: usize, script: bool) -> (usize, usize) {
    let mut writer = Gen { rng: Rng(seed), seeds: script };
    let (mut hits, mut reads) = (0, 0);
    for _ in 0..count {
        let body = writer.block(5, false);
        let src = if script {
            format!("<?php\n/** @var \\DateTime|unset $v0 */\n$tmp = 1;\n{body}\n")
        } else {
            in_function(&body)
        };
        let (answer, h) = assert_same(&src);
        hits += h;
        reads += answer.maybe_undefined.len() + answer.seeds.reads.len();
    }
    (hits, reads)
}

#[test]
fn generated_function_bodies_answer_the_same_with_and_without_the_cache() {
    let (hits, reads) = corpus(0x9e37_79b9_7f4a_7c15, 300, false);
    assert!(hits > 0, "the generated nests must exercise the cache");
    assert!(reads > 0, "and some of them must report, or nothing is compared");
}

#[test]
fn generated_script_scopes_answer_the_same_with_and_without_the_cache() {
    let (hits, reads) = corpus(0xd1b5_4a32_d192_ed03, 300, true);
    assert!(hits > 0, "the generated nests must exercise the cache");
    assert!(reads > 0, "and some of them must yield candidates, or nothing is compared");
}
