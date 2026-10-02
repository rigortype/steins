//! Loop nests lower in time that does not grow with the depth (issue #793, and
//! #655's lowering half).
//!
//! The binding-presence pass walks a loop body up to three times, and a body nested
//! in another is walked from each of those: without a cache a nest `d` deep costs
//! `2^d` to `3^d` body walks — `foreach` 20 deep took 11 s, 22 deep 43 s in a release
//! build. These tests parse such nests and fail on a clock rather than on a count,
//! because the cost is the thing the cache exists to remove. Each also checks the
//! answer, since a nest that lowers fast and reports nothing proves nothing.
//!
//! The budget is wall time on a debug build (CI runs `cargo test` unoptimized); the
//! cached lowering takes a small fraction of it, while the uncached one does not
//! finish at these depths at all. The parse runs on a worker thread so a regression
//! is reported as a missed budget instead of hanging the suite.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use steins_syntax::{ScopeOwner, SourceTree};

/// Generous against the measured debug time, far under the uncached one.
const BUDGET: Duration = Duration::from_secs(20);

/// A worker with room for a 200-deep nest's frames in a debug build (~16 KiB a
/// level) on top of the parser's own recursion.
const STACK: usize = 512 * 1024 * 1024;

/// The opening line of the `i`th nest level.
fn open(kind: &str, i: usize) -> String {
    match kind {
        "foreach" => format!("foreach ($a{i} as $v{i}) {{"),
        "while" => format!("while ($c{i}) {{"),
        "for" => format!("for ($i{i} = 0; $i{i} < 10; $i{i}++) {{"),
        "if" => format!("if ($c{i}) {{"),
        "do" => "do {".to_owned(),
        other => unreachable!("{other}"),
    }
}

fn close(kind: &str, i: usize) -> String {
    if kind == "do" { format!("}} while ($c{i});") } else { "}".to_owned() }
}

/// A function whose body is `depth` nested constructs, cycling through `kinds`. `$x`
/// is read at every fourth level, before the innermost `if` binds it, and once more
/// at the innermost level: each read is bound on the back edge and not on entry, so
/// each is a `variable.maybe-undefined`.
fn nest(kinds: &[&str], depth: usize) -> (String, usize) {
    let levels: Vec<&str> = (0..depth).map(|i| kinds[i % kinds.len()]).collect();
    let mut src = String::from("<?php\nfunction f(array $a0, bool $c0) {\n");
    let mut reads = 0;
    for (i, kind) in levels.iter().enumerate() {
        src.push_str(&open(kind, i + 1));
        src.push('\n');
        if i % 4 == 0 {
            src.push_str("echo $x;\n");
            reads += 1;
        }
    }
    src.push_str("if ($c0) { $x = 1; }\necho $x;\n");
    reads += 1;
    for (i, kind) in levels.iter().enumerate().rev() {
        src.push_str(&close(kind, i + 1));
        src.push('\n');
    }
    src.push_str("}\n");
    (src, reads)
}

/// Lower `src` on a worker thread and answer the `variable.maybe-undefined` reads
/// of `f` with the time the lowering took, or fail if it overruns [`BUDGET`].
fn lower_within_budget(src: String) -> (Vec<String>, Duration) {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            let started = Instant::now();
            let tree = SourceTree::parse(&src);
            let elapsed = started.elapsed();
            let names = tree
                .scopes()
                .iter()
                .filter(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f"))
                .flat_map(|s| s.maybe_undefined_reads.iter().map(|r| r.name.clone()))
                .collect();
            let errors = tree.parse_errors().len();
            assert_eq!(errors, 0, "the nest must parse: {:?}", tree.parse_errors());
            let _ = tx.send((names, elapsed));
        })
        .expect("spawn the lowering worker");
    rx.recv_timeout(BUDGET).unwrap_or_else(|_| {
        panic!("lowering the nest overran {BUDGET:?}: the loop-body cache is not answering")
    })
}

fn assert_flat(kinds: &[&str], depth: usize) {
    let (src, reads) = nest(kinds, depth);
    let (names, elapsed) = lower_within_budget(src);
    eprintln!("{kinds:?} x{depth}: lowered in {elapsed:?}");
    assert_eq!(names, vec!["x"; reads], "every read of `$x` is a maybe-undefined one");
}

#[test]
fn twenty_four_nested_foreach_lower_in_bounded_time() {
    assert_flat(&["foreach"], 24);
}

#[test]
fn twenty_four_nested_while_lower_in_bounded_time() {
    assert_flat(&["while"], 24);
}

#[test]
fn twenty_four_nested_for_lower_in_bounded_time() {
    assert_flat(&["for"], 24);
}

#[test]
fn a_two_hundred_deep_mixed_nest_lowers_in_bounded_time() {
    assert_flat(&["foreach", "while", "for", "if", "do"], 200);
}
