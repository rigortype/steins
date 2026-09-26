//! `unset()` is a write of the construct it sits in (issue #685).
//!
//! A structured loop forgets its `writes` at the body entry and at the
//! fall-through, and keeps its `reads` at both (#653, #651). An `unset` target
//! was in neither write set, so a variable the body removes stayed in `reads` and
//! kept its pre-loop value after the loop — a stale fact rather than a missing
//! one. PHP at 8.5.10:
//!
//! ```text
//! $s = 'abc'; for ($i = 0; $i < 1; $i++) { unset($s); } var_dump(intdiv($s, 1));
//! // Warning: Undefined variable $s; Deprecated: Passing null … — int(0), no TypeError
//! ```
//!
//! An `unset` target is now collected as an assignment lvalue is, so the name is
//! forgotten wherever the loop's writes are. The straight-line `if` twin never
//! needed this — an `unset($s)` statement is a barrier in the branch that runs
//! it — and is pinned beside the loops as the answer they now share.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, ID, check};
use steins_syntax::SourceTree;

/// Every `debug.type` body a source produces, in source order.
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// `body` in a function scope after `$s = 'abc'; $t = 'k';`, dumping `$s` then `$t`.
fn after(body: &str) -> Vec<String> {
    types(&format!(
        "<?php\nfunction f(array $xs): void {{ $s = 'abc'; $t = 'k'; {body} \
         \\PHPStan\\dumpType($s); \\PHPStan\\dumpType($t); }}\n"
    ))
}

#[test]
fn an_unset_in_a_loop_body_is_forgotten_after_every_loop_form() {
    let forgotten = vec!["dumped type: unknown".to_owned(), "dumped type: 'k'".to_owned()];
    // The issue's shape.
    assert_eq!(after("while (rand() > 0) { unset($s); }"), forgotten, "`while`");
    assert_eq!(after("for ($i = 0; $i < 3; $i++) { unset($s); }"), forgotten, "`for`");
    assert_eq!(after("foreach ($xs as $x) { unset($s); }"), forgotten, "`foreach`");
    assert_eq!(after("do { unset($s); } while (rand() > 0);"), forgotten, "`do`-`while`");
    // One target among several is still a target.
    assert_eq!(after("while (rand() > 0) { unset($u, $s); }"), forgotten, "multi-target");
}

#[test]
fn an_unset_in_a_branch_is_forgotten_after_the_branch() {
    // The straight-line twin: the branch that runs `unset($s)` has no `$s`. `$t`
    // goes with it — a bare `unset($s)` statement still lowers to a full barrier —
    // so only the first dump is this pin's.
    let got = after("if (rand() > 0) { unset($s); }");
    assert_eq!(got[0], "dumped type: unknown");
}

/// The entry half already answered `unknown` before the fix; pinned beside the
/// exit rows so the write set keeps it that way.
#[test]
fn an_unset_is_forgotten_at_the_next_iteration_entry() {
    let src = "<?php
function f(): void {
    $s = 'abc';
    while (rand() > 0) {
        \\PHPStan\\dumpType($s);
        unset($s);
    }
}
";
    assert_eq!(types(src), vec!["dumped type: unknown".to_owned()]);
}

#[test]
fn an_offset_unset_in_a_loop_body_rewrites_the_array() {
    // `unset($a['k'])` removes the key on some iterations and not others; the
    // shape the loop entered with is no longer a fact after it.
    let src = "<?php
function f(): void {
    $a = ['k' => 1, 'j' => 2];
    while (rand() > 0) { unset($a['k']); }
    \\PHPStan\\dumpType($a);
}
";
    assert_eq!(types(src), vec!["dumped type: unknown".to_owned()]);
}

/// The stale value a loop kept convicted a call PHP accepts: an unset `$s` reads
/// `null`, which `?int` takes, where the pre-loop `'abc'` is a coercive-mode
/// `TypeError`.
#[test]
fn a_loop_unset_no_longer_proves_a_stale_argument_mismatch() {
    let src = "<?php
function need(?int $n): void {}
function f(): void {
    $s = 'abc';
    while (rand() > 0) { unset($s); }
    need($s);
}
";
    let tree = SourceTree::parse(src);
    let found: Vec<Diagnostic> =
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == ID).collect();
    assert!(found.is_empty(), "`$s` may be unset after the loop: {found:?}");
}
