//! ADR-0069 / issue #73 — the Asserted declared-envelope floor.
//!
//! The bottom rung of the return ladder: where the engine says nothing about a
//! builtin's return type, the catalog's mined declaration speaks instead, at the
//! `Asserted` stratum. Everything worth pinning about it is a *boundary*:
//!
//! * it answers where the engine is silent — **per name**, not per run;
//! * the engine's answer wins wherever the engine has one;
//! * the fact it seeds is `Asserted`, which is the bit the proof layer's
//!   all-Verified premise rule reads;
//! * the absence family neither consumes it nor is suppressed by it.
//!
//! The observable for "which rung answered" is the dump surface's `(asserted)`
//! marker (ADR-0053 §2 / ADR-0052 §5): the reflected envelope is a native
//! declaration and carries none, the floor is a catalog claim and always carries
//! one. That single bit *is* the firewall, so the tests read it directly rather
//! than through prose.

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{
    CALL_UNDEFINED_FUNCTION_ID, DEBUG_TYPE_ID, Diagnostic, Folder, Layer, RETURN_MISMATCH_ID, check,
    check_with, layer,
};
use steins_syntax::{ArgValue, SourceTree};

// ---------------------------------------------------------------------------
// Mocks. There is no PHP in a unit test, so a live engine is a `Folder` that
// answers `builtin_return_fact` (and, for the absence legs, the boot surface).
// ---------------------------------------------------------------------------

/// An engine that reflects a return type for the names it was given and is silent
/// for every other name — the shape of a real PHP whose extension set does not
/// cover the whole call graph.
#[derive(Default)]
struct Engine {
    facts: HashMap<String, Fact>,
    /// Names the boot surface reports as resident functions. Only consulted when
    /// [`Self::absence`] is set.
    resident: Vec<String>,
    absence: bool,
}

impl Engine {
    fn reflecting(name: &str, fact: Fact) -> Self {
        let mut e = Engine::default();
        e.facts.insert(name.to_ascii_lowercase(), fact);
        e
    }
    fn and_reflecting(mut self, name: &str, fact: Fact) -> Self {
        self.facts.insert(name.to_ascii_lowercase(), fact);
        self
    }
    /// A live, monkey-patch-free engine whose boot surface knows exactly `resident`.
    fn with_boot_surface(mut self, resident: &[&str]) -> Self {
        self.absence = true;
        self.resident = resident.iter().map(|n| n.to_ascii_lowercase()).collect();
        self
    }
}

impl Folder for Engine {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn absence_family_available(&mut self) -> bool {
        self.absence
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        self.absence.then(|| self.resident.iter().any(|n| n.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        self.absence.then(|| "PHP 8.5.8 (32 extensions)".to_owned())
    }
}

fn general(base: Base) -> Fact {
    Fact::General { base, nullable: false }
}

/// The `debug.type` message bodies a source produces under the sound subset
/// (`NoFold` — the `--no-php` posture), in source order.
fn no_php_dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// The same, with an engine standing behind the folder seam.
fn dumps_with(src: &str, folder: &mut dyn Folder) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
}

// ---------------------------------------------------------------------------
// The acceptance criterion: `--no-php` gains declared types.
// ---------------------------------------------------------------------------

#[test]
fn str_repeat_of_variables_dumps_string_under_no_php() {
    // ADR-0069 §1's own worked example. Before the floor both of these dumped
    // `unknown`: the fold cannot reach a call with variable operands, and every
    // remaining rung is engine-gated. The `(asserted)` marker is not decoration —
    // it is the grade, and it is what tells a reader the catalog answered.
    let src = "<?php\nfunction f(string $s, int $n): void {\n\
               $r = str_repeat($s, $n);\n\
               \\PHPStan\\dumpType($r);\n\
               \\PHPStan\\dumpType(str_repeat($s, $n));\n}\n";
    assert_eq!(
        no_php_dumps(src),
        vec!["dumped type: string (asserted)".to_owned(), "dumped type: string (asserted)".to_owned()],
        "both the assignment rung and the dump rung must reach the floor",
    );
}

#[test]
fn the_floor_covers_the_scalar_bases_and_nothing_else() {
    // One row per envelope shape the table can carry, so a lowering regression in
    // `envelope_fact` shows up here rather than as a silent loss of rows.
    let probe = |call: &str| {
        let src = format!(
            "<?php\nfunction f(string $s, int $n, $h): void {{ \\PHPStan\\dumpType({call}); }}\n"
        );
        no_php_dumps(&src).first().cloned().unwrap_or_default()
    };
    assert_eq!(probe("str_pad($s, $n)"), "dumped type: string (asserted)");
    assert_eq!(probe("curl_errno($h)"), "dumped type: int (asserted)");
    assert_eq!(probe("acos($n)"), "dumped type: float (asserted)");
    assert_eq!(probe("array_key_exists($s, [])"), "dumped type: bool (asserted)");
    // A `?T` row keeps its nullability — the floor states an envelope, not a base.
    assert_eq!(probe("curl_multi_getcontent($h)"), "dumped type: string|null (asserted)");
    // A name with no admitted row stays honestly unknown; the floor invents nothing.
    assert_eq!(probe("sodium_add($s, $s)"), "dumped type: unknown");
}

#[test]
fn a_project_function_shadows_the_floor() {
    // Same refusal the reflected-envelope rung makes: a project function of the
    // builtin's simple name shadows (or makes ambiguous) the call, and the
    // project's own definition is the better answer than a catalog row.
    let src = "<?php\nfunction str_repeat($a, $b) { return []; }\n\
               function f(string $s, int $n): void { \\PHPStan\\dumpType(str_repeat($s, $n)); }\n";
    assert_eq!(no_php_dumps(src), vec!["dumped type: unknown".to_owned()]);
}

// ---------------------------------------------------------------------------
// Which rung answered: the engine wins wherever it speaks.
// ---------------------------------------------------------------------------

#[test]
fn an_engine_answer_wins_over_the_floor() {
    // The engine reflects `int` for `str_repeat` — deliberately NOT the catalog's
    // `string`, so the rendered type alone says which rung answered. The engine's
    // answer stands, and it carries no `(asserted)` marker: it is a native
    // declaration read off the running engine's own arginfo.
    let src = "<?php\nfunction f(string $s, int $n): void { \\PHPStan\\dumpType(str_repeat($s, $n)); }\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::Int));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: int".to_owned()]);
    // And through the assignment rung, which is a separate call site.
    let assigned = "<?php\nfunction f(string $s, int $n): void {\n\
                    $r = str_repeat($s, $n); \\PHPStan\\dumpType($r); }\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::Int));
    assert_eq!(dumps_with(assigned, &mut engine), vec!["dumped type: int".to_owned()]);
}

#[test]
fn the_floor_fills_the_engines_silence_per_name_not_per_run() {
    // ADR-0069 §2 as amended: the rung's condition is per NAME. A live engine that
    // reflects `str_repeat` but knows nothing of `gmp_intval` (no gmp extension on
    // the analyzing PHP) leaves exactly that one name to the floor — and the marker
    // separates the two answers in the same run.
    let src = "<?php\nfunction f(string $s, int $n, $g): void {\n\
               \\PHPStan\\dumpType(str_repeat($s, $n));\n\
               \\PHPStan\\dumpType(gmp_intval($g));\n}\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    assert_eq!(
        dumps_with(src, &mut engine),
        vec!["dumped type: string".to_owned(), "dumped type: int (asserted)".to_owned()],
    );
}

// ---------------------------------------------------------------------------
// The proof-layer firewall.
// ---------------------------------------------------------------------------

#[test]
fn the_floor_seeds_asserted_and_never_verified() {
    // The stratum bit itself, read straight off the binding through the dump
    // surface (which renders `known.stratum == Asserted`). This is the premise the
    // proof layer's all-Verified rule consults, so pinning it pins the firewall at
    // its mechanism rather than at one downstream symptom.
    let src = "<?php\nfunction f(string $s, int $n): void { $r = str_repeat($s, $n); \\PHPStan\\dumpType($r); }\n";
    assert_eq!(no_php_dumps(src), vec!["dumped type: string (asserted)".to_owned()]);
    // The identical fact from the engine is Verified — no marker. Same fact, same
    // rendering, different grade: one lowering, two provenances (ADR-0069 §2).
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: string".to_owned()]);
}

#[test]
fn a_floor_fact_premises_a_contract_finding_but_never_a_proof_one() {
    // The floor's `string` is a real premise: it decides `phpdoc.return-mismatch`,
    // a CONTRACT-layer finding, which is exactly the "contracts-tier reasoning"
    // ADR-0069 §2 admits. What must never happen is the same premise reaching the
    // proof layer, and the assertion is made over the whole diagnostic set rather
    // than one id — a future proof-layer consumer of abstract facts would break
    // this test, which is the point of writing it this way.
    let src = "<?php\n/** @return int */\nfunction f(string $s, int $n) { $r = str_repeat($s, $n); return $r; }\n";
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    assert!(
        ds.iter().any(|d| d.id == RETURN_MISMATCH_ID),
        "the floor fact must actually be consumed, or this test proves nothing: {ds:?}"
    );
    let proof: Vec<&Diagnostic> =
        ds.iter().filter(|d| layer(d.id) == Some(Layer::Proof)).collect();
    assert!(proof.is_empty(), "a floor fact reached the proof layer: {proof:?}");

    // The same snippet with the SAME envelope from a live engine — Verified, the
    // grade the floor is denied. The contract finding still fires (the fact is the
    // same set of values either way), and the proof layer is silent for a reason
    // that has nothing to do with the floor: no proof-layer id consumes an abstract
    // fact today. The firewall is therefore belt AND braces, and this pin records
    // which brace is which.
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    let verified = run(src, &mut engine);
    assert!(
        verified.iter().any(|d| d.id == RETURN_MISMATCH_ID),
        "the engine's envelope must premise the same contract finding: {verified:?}"
    );
    assert!(
        verified.iter().all(|d| layer(d.id) != Some(Layer::Proof)),
        "no proof-layer id consumes an abstract return fact yet: {verified:?}"
    );
}

// ---------------------------------------------------------------------------
// The absence family: not a consumer, and not suppressed.
// ---------------------------------------------------------------------------

#[test]
fn the_absence_family_stays_silent_under_no_php() {
    // Existence is a boot-surface fact. Without an engine there is no boot surface,
    // so the family is silent — unchanged by the floor, which answers only about
    // return types and never about whether a name exists.
    let src = "<?php\nfunction f($g): void { gmp_intval($g); typo_that_does_not_exist(); }\n";
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    assert!(
        ds.iter().all(|d| d.id != CALL_UNDEFINED_FUNCTION_ID),
        "the absence family must stay silent under the sound subset: {ds:?}"
    );
}

#[test]
fn an_absence_finding_and_a_floor_fact_are_complementary() {
    // ADR-0069 §2 as amended. A live engine without gmp: the boot surface proves
    // `gmp_intval` absent (a real finding — the call fails on the analyzing PHP),
    // and the floor still states the shape it declares where it DOES exist. Both
    // are true at once; neither suppresses the other, and the floor must never be
    // read as an existence answer in either direction.
    let src = "<?php\nfunction f($g): void { $r = gmp_intval($g); \\PHPStan\\dumpType($r); }\n";
    let mut engine = Engine::default().with_boot_surface(&[]);
    let ds = run(src, &mut engine);
    assert!(
        ds.iter().any(|d| d.id == CALL_UNDEFINED_FUNCTION_ID),
        "the absence proof must still fire: {ds:?}"
    );
    let dumped: Vec<&str> =
        ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).map(|d| d.message.as_str()).collect();
    assert_eq!(dumped, vec!["dumped type: int (asserted)"], "the floor still states the declared shape");
}

#[test]
fn a_floor_row_is_not_an_existence_vouch() {
    // The other direction of the same rule. `gmp_intval` carries a floor row and no
    // effect row; `typo_nope` carries neither. Against a boot surface that reports
    // both absent, the family judges them identically — coverage by this table buys
    // a name exactly nothing on the existence question.
    let covered = "<?php\nfunction f($g): void { gmp_intval($g); }\n";
    let uncovered = "<?php\nfunction f($g): void { typo_nope($g); }\n";
    let mut engine = Engine::default().with_boot_surface(&[]);
    let count = |src: &str, e: &mut Engine| {
        run(src, e).iter().filter(|d| d.id == CALL_UNDEFINED_FUNCTION_ID).count()
    };
    assert_eq!(count(covered, &mut engine), 1, "a floor row must not vouch for existence");
    assert_eq!(count(uncovered, &mut engine), 1);
}

// ---------------------------------------------------------------------------
// Precedence against the rungs above.
// ---------------------------------------------------------------------------

#[test]
fn a_more_precise_rung_still_wins() {
    // The floor is the FLOOR: an engine that folds the call to a literal, or that
    // reflects an envelope, is consulted first and its answer stands. Here the
    // engine reflects `bool` for `array_key_exists` (whose floor row also says
    // `bool`), and the absent marker proves the engine rung answered.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(array_key_exists($s, [])); }\n";
    let mut engine =
        Engine::reflecting("array_key_exists", general(Base::Bool)).and_reflecting("noop", general(Base::Int));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: bool".to_owned()]);
}

