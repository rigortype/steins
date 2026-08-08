//! The headroom guard over real deep sources (issue #264).
//!
//! These run **in process**, on libtest's 2 MiB thread — a stack smaller than the
//! 8 MiB one issue #246 already found fatal at ~520 levels of `->`. That is
//! deliberate and it is the point: every test here sets a budget first, so the
//! guard is what stops the walk, and the assertion is that the walk stops with a
//! named refusal instead of killing the process. A test that parses a deep
//! fixture *without* setting a budget must run the binary as a subprocess
//! (`crates/steins-cli/tests/deep_nesting.rs`) instead.
//!
//! The budget below is small on purpose: a stack overflow is not a catchable
//! panic, so the margin between "the guard fires" and "libtest's thread dies" has
//! to hold under **both** profiles' frame sizes — ~16 KiB per level in debug,
//! ~2.7 KiB in release. A budget calibrated in bytes does that with one number;
//! a depth constant provably cannot (see
//! `docs/notes/20260808-deep-nesting-stack-budget.md`).

use steins_syntax::{SourceTree, stack_guard};

/// A small slice of libtest's 2 MiB, leaving the rest as the reserve the guard
/// needs for the work that happens *below* the check — chiefly Mago's
/// `HasSpan::span`, which recurses once per remaining nesting level.
const BUDGET: usize = 128 * 1024;

/// phpstan-src's own `tests/bench/data/nullsafe-chain-walk.php` is 1,000 levels
/// deep: the depth here is a real file from a real repository, not a synthetic
/// worst case. It exhausts `BUDGET` under both profiles' frame sizes.
const DEEP: usize = 1_000;

fn refusals(tree: &SourceTree) -> Vec<&str> {
    tree.parse_errors()
        .iter()
        .map(|e| e.message.as_str())
        .filter(|m| m.contains("nests deeper than the analyzer can walk"))
        .collect()
}

fn parse_with_budget(src: &str) -> SourceTree {
    stack_guard::set_budget(BUDGET);
    let tree = SourceTree::parse(src);
    stack_guard::set_budget(0);
    tree
}

#[test]
fn a_property_chain_past_the_budget_is_refused_not_a_trap() {
    let src = format!("<?php\n$x = $n{};\n", "->next".repeat(DEEP));
    let tree = parse_with_budget(&src);
    assert_eq!(refusals(&tree).len(), 1, "one refusal, named once: {:?}", tree.parse_errors());
}

#[test]
fn an_index_chain_and_a_concat_chain_are_refused_too() {
    // The two shapes that reach the Box-based value IR, unlike a `->` chain.
    let index = format!("<?php\nf($a{});\n", "[0]".repeat(DEEP));
    assert_eq!(refusals(&parse_with_budget(&index)).len(), 1, "index chain");

    let concat = format!("<?php\nf({}'a');\n", "'a' . ".repeat(DEEP));
    assert_eq!(refusals(&parse_with_budget(&concat)).len(), 1, "concat chain");
}

#[test]
fn the_refusal_is_the_files_first_error_and_resolves_to_a_position() {
    // It names the file rather than the offending line — see `stack_guard::REFUSAL`
    // for why a position for the deep node cannot be bought here — so it sorts
    // first, which is also the site ADR-0079's dam keys on.
    let src = format!("<?php\n$x = $n{};\n", "->next".repeat(DEEP));
    let tree = parse_with_budget(&src);
    let first = tree.parse_errors().first().expect("a refusal");
    assert!(first.message.contains("nests deeper than the analyzer can walk"), "{first:?}");
    assert!((first.span.start as usize) <= src.len(), "the span points into the file");
    assert_eq!(tree.position(first.span.start).line, 1);
}

#[test]
fn a_shallow_file_under_the_same_budget_is_untouched() {
    let src = "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n";
    let tree = parse_with_budget(src);
    assert!(tree.parse_errors().is_empty(), "no refusal: {:?}", tree.parse_errors());
    assert_eq!(tree.functions().len(), 1, "and the file still lowers in full");
    assert_eq!(tree.calls().len(), 1);
}

#[test]
fn bought_headroom_answers_the_whole_question() {
    // The native contract (issue #246): where stack can be bought, nothing is
    // refused. This is the CLI's arrangement in miniature — a worker thread with
    // real headroom and no budget — over a chain past every 8 MiB ceiling in the
    // note's table. It has to be its own thread because libtest's is 2 MiB, which
    // ~4,000 debug frames of ~16 KiB would not survive.
    assert_eq!(stack_guard::budget(), 0, "no budget on a fresh thread");
    let worker = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            let src = format!("<?php\n$x = $n{};\n", "->next".repeat(DEEP));
            let tree = SourceTree::parse(&src);
            tree.parse_errors().to_vec()
        })
        .expect("spawn");
    assert_eq!(worker.join().expect("join"), Vec::new(), "no refusal on a stack that can answer");
}
