//! The headroom guard over real deep sources (issue #264).
//!
//! Runs in-process on libtest's 2 MiB thread, smaller than the 8 MiB stack
//! issue #246 found fatal at ~520 `->` levels — deliberate, so the guard, not
//! the OS, stops the walk with a named refusal. A fixture parsed without a
//! budget belongs in `crates/steins-cli/tests/deep_nesting.rs` (subprocess).
//!
//! Budget is small and byte-sized because the margin must hold under both
//! profiles' frame sizes — ~16 KiB/level debug, ~2.7 KiB release — which a
//! depth constant provably cannot (`docs/notes/20260808-deep-nesting-stack-budget.md`).

use steins_syntax::{SourceTree, stack_guard};

/// A small slice of libtest's 2 MiB; the rest is reserve for the work below the
/// check — chiefly Mago's `HasSpan::span`, recursing once per level.
const BUDGET: usize = 128 * 1024;

/// phpstan-src's own `tests/bench/data/nullsafe-chain-walk.php` is 1,000 levels
/// deep — a real file, not synthetic — and exhausts `BUDGET` under both frame sizes.
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
    // Names the file, not the line (`stack_guard::REFUSAL`), and sorts first —
    // the site ADR-0079's dam keys on.
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
    // Native contract (issue #246): where stack can be bought, nothing is
    // refused — the CLI's arrangement in miniature. Own thread because
    // libtest's 2 MiB can't survive ~4,000 debug frames at ~16 KiB each.
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
