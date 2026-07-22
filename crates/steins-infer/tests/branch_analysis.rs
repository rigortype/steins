//! Acceptance tests for branch-sensitive analysis stage 1 (ADR-0031): structured
//! `if`/`elseif`/`else`, unified `Certainty` condition evaluation, positive
//! refinement, fall-through joins, early-exit pruning, ternary values, and the
//! `call.on-null` proof.
//!
//! Note the two-pass interaction: the env-free **direct** pass checks every
//! literal call argument in the file regardless of reachability, so these tests
//! drive the reachability-sensitive **propagation** pass by flowing a bad value
//! through a *variable* (`bad($v)`), which only the propagation walk checks.

use steins_infer::{Diagnostic, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "demo.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

/// `function width(int $w)` header + a bad string local `$bad = "abc"`.
const HDR: &str = "<?php\nfunction width(int $w): int { return $w; }\n$bad = \"abc\";\n";

// ---- cond-decided pruning, both directions --------------------------------

#[test]
fn cond_true_walks_then_branch() {
    // `$x === 5` with `$x = 5` is Yes → the then-branch is live → `width($bad)`
    // (bad string via a variable) is FLAGGED.
    let src = format!("{HDR}$x = 5;\nif ($x === 5) {{ width($bad); }}\n");
    assert_eq!(n(&src), 1, "decided-true guard → then-branch live → flagged");
}

#[test]
fn cond_false_prunes_then_branch() {
    // `$x === 6` with `$x = 5` is No → the then-branch is DEAD → the propagated
    // `width($bad)` inside it is never walked → silent.
    let src = format!("{HDR}$x = 5;\nif ($x === 6) {{ width($bad); }}\n");
    assert_eq!(n(&src), 0, "decided-false guard → then-branch dead → silent");
}

#[test]
fn unreachable_after_terminating_then_emits_nothing() {
    // A decided-true guard whose then-branch terminates makes the remainder
    // unreachable: `width($bad)` after the `if` is not walked.
    let src = "<?php
function width(int $w): int { return $w; }
function f(): void {
    $bad = \"abc\";
    if (true) { return; }
    width($bad);
}
";
    assert_eq!(n(src), 0, "code after a terminating decided-true then is unreachable → silent");
    // Control: without the early return, the tail is reachable and flagged.
    let live = "<?php
function width(int $w): int { return $w; }
function f(): void {
    $bad = \"abc\";
    if (true) { $y = 1; }
    width($bad);
}
";
    assert_eq!(n(live), 1, "reachable tail → flagged (proves the pruning is real)");
}

// ---- fall-through joins ----------------------------------------------------

#[test]
fn join_agree_keeps_fact() {
    // Both branches assign the SAME bad value → the join keeps a Singleton →
    // `width($w)` after the `if` is flagged.
    let src = format!(
        "{HDR}if ($cond) {{ $w = \"abc\"; }} else {{ $w = \"abc\"; }}\nwidth($w);"
    );
    assert_eq!(n(&src), 1, "agreeing branches → Singleton survives → flagged");
}

#[test]
fn join_differ_becomes_oneof_and_is_silent() {
    // Disagreeing branches → a OneOf, which never resolves to one proven value →
    // silent (stage 1 does not flag OneOf at call sites).
    let src = format!(
        "{HDR}if ($cond) {{ $w = \"abc\"; }} else {{ $w = \"xyz\"; }}\nwidth($w);"
    );
    assert_eq!(n(&src), 0, "differing branches → OneOf → not one proven value → silent");
}

#[test]
fn join_absent_in_one_branch_drops_fact() {
    // A fact set in only one branch does not survive the join.
    let src = format!("{HDR}if ($cond) {{ $w = \"abc\"; }}\nwidth($w);");
    assert_eq!(n(&src), 0, "fact absent on the else path → dropped → silent");
}

// ---- positive refinement ---------------------------------------------------

#[test]
fn positive_refinement_binds_then_branch() {
    // `if ($x === "abc")` binds `$x = "abc"` in the then-branch, so `width($x)`
    // there is a proven TypeError (a NEW finding from refinement).
    let src = "<?php
function width(int $w): int { return $w; }
function f($x): void {
    if ($x === \"abc\") { width($x); }
}
";
    assert_eq!(n(src), 1, "then-branch of === <literal> narrows $x to the literal → flagged");
}

#[test]
fn else_refinement_of_not_identical() {
    // The else-branch of `$x !== "abc"` proves `$x === "abc"`.
    let src = "<?php
function width(int $w): int { return $w; }
function f($x): void {
    if ($x !== \"abc\") { return; }
    width($x);
}
f(\"anything\");
";
    // Direct pass: $x is a variable, unchanged in the reachable tail after the
    // negated guard is filtered → $x === "abc" holds → width($x) flagged.
    assert_eq!(n(src), 1, "else of !== narrows to the literal on the fall-through → flagged");
}

// ---- early-exit pruning ----------------------------------------------------

#[test]
fn early_exit_unknown_stays_silent_no_negative_facts() {
    // `if ($x === null) return;` with $x UNKNOWN: the fall-through carries no
    // negative fact (stage 1), so $x stays unknown → silent.
    let src = "<?php
function width(int $w): int { return $w; }
function f($x): void {
    if ($x === null) { return; }
    width($x);
}
";
    assert_eq!(n(src), 0, "no negative facts yet → unknown $x on the tail → silent");
}

#[test]
fn early_exit_bound_null_makes_tail_dead() {
    // The same guard when $x is BOUND null (via descent): the guard is Yes, the
    // then-branch returns, and the tail is dead → silent (dead-path proof).
    let src = "<?php
declare(strict_types=1);
function width(int $w): int { return $w; }
function f(?int $x): void {
    if ($x === null) { return; }
    width($x);
}
f(null);
";
    assert_eq!(n(src), 0, "bound-null guard → then returns → tail unreachable → silent");
}

// ---- elseif chains + nested ifs -------------------------------------------

#[test]
fn elseif_chain_selects_matching_arm() {
    // `$x = 2` selects the second arm (`elseif ($x === 2)`), binding `$w = "abc"`.
    let src = format!(
        "{HDR}$x = 2;\nif ($x === 1) {{ $w = \"ok1\"; }} elseif ($x === 2) {{ $w = \"abc\"; }} else {{ $w = \"okz\"; }}\nwidth($w);"
    );
    assert_eq!(n(&src), 1, "elseif ($x === 2) is the live arm → $w = \"abc\" → flagged");
}

#[test]
fn nested_ifs_preserve_untouched_fact() {
    // Nested ifs that never write `$bad` leave it known across both levels.
    let src = format!("{HDR}if ($a) {{ if ($b) {{ echo 1; }} }}\nwidth($bad);");
    assert_eq!(n(&src), 1, "nested ifs not writing $bad → fact survives → flagged");
}

// ---- loop inside if stays conservative ------------------------------------

#[test]
fn loop_inside_if_still_opaque() {
    // A loop nested in a then-branch stays `Opaque`: it writes `$bad`, so on the
    // then path the fact is dropped; the join with the else path then drops it
    // entirely → silent (the ratchet: loops are still conservative).
    let src = format!(
        "{HDR}if ($cond) {{ while ($x) {{ $bad = 5; }} }}\nwidth($bad);"
    );
    assert_eq!(n(&src), 0, "loop-in-if writes $bad → dropped on that path → join drops it → silent");
}

// ---- ternary values --------------------------------------------------------

#[test]
fn ternary_decided_true_picks_then_arm() {
    let src = format!("{HDR}$w = true ? \"abc\" : 5;\nwidth($w);");
    assert_eq!(n(&src), 1, "decided-true ternary → then arm \"abc\" → flagged");
}

#[test]
fn ternary_decided_false_picks_else_arm() {
    let src = format!("{HDR}$w = false ? 5 : \"abc\";\nwidth($w);");
    assert_eq!(n(&src), 1, "decided-false ternary → else arm \"abc\" → flagged");
}

#[test]
fn ternary_undecided_is_oneof_and_silent() {
    let src = format!("{HDR}$w = $c ? \"abc\" : \"xyz\";\nwidth($w);");
    assert_eq!(n(&src), 0, "undecided ternary of two literals → OneOf → silent");
}

#[test]
fn ternary_undecided_agreeing_arms_is_singleton() {
    // Both arms the SAME bad literal → a Singleton even when undecided → flagged.
    let src = format!("{HDR}$w = $c ? \"abc\" : \"abc\";\nwidth($w);");
    assert_eq!(n(&src), 1, "undecided ternary with equal arms → Singleton → flagged");
}

// ---- call.on-null ----------------------------------------------------------

#[test]
fn call_on_null_fires_inside_null_guard() {
    let src = "<?php
class U { public function name(): string { return \"x\"; } }
function f($u): void {
    if ($u === null) { $u->name(); }
}
";
    let f = findings(src);
    assert_eq!(f.len(), 1, "proven-null receiver → call.on-null: {f:#?}");
    let d = &f[0];
    assert_eq!(d.id, "call.on-null");
    assert_eq!(
        d.message,
        "method call $u->name() — $u is proven null on this path — proven Error (Call to a member function on null)"
    );
}

#[test]
fn call_on_null_silent_for_nullsafe() {
    // `?->` on null is defined (short-circuits) → never fires.
    let src = "<?php
class U { public function name(): string { return \"x\"; } }
function f($u): void {
    if ($u === null) { $u?->name(); }
}
";
    assert_eq!(n(src), 0, "nullsafe call on proven null → silent");
}

#[test]
fn call_on_null_silent_for_oneof_including_null() {
    // A OneOf that merely *includes* null is Maybe → silent.
    let src = "<?php
class U { public function name(): string { return \"x\"; } }
function f($c): void {
    $u = $c ? null : \"s\";
    $u->name();
}
";
    assert_eq!(n(src), 0, "OneOf of null and a string receiver → not proven null → silent");
}

// ---- empirical `==` cells (PHP 8.5.8; see php_loose_eq rustdoc) ------------

/// Whether the then-branch of `if ($x <op> <rhs>)` is LIVE, observed by whether a
/// bad propagated value inside it is flagged. A *decided* guard (both operands
/// known literals) is Yes → 1 or No → 0, so this reads off the `==` verdict.
fn cell_live(x_lit: &str, cmp: &str) -> bool {
    let src = format!("{HDR}$x = {x_lit};\nif ($x {cmp}) {{ width($bad); }}\n");
    n(&src) == 1
}

#[test]
fn empirical_loose_eq_cells_decide_branches() {
    // Each cell was measured against PHP 8.5.8 (see the `php_loose_eq` table).
    assert!(cell_live("null", "== null"), "null == null → T");
    assert!(cell_live("null", "== 0"), "null == 0 → T");
    assert!(cell_live("null", "== \"\""), "null == \"\" → T");
    assert!(!cell_live("null", "== \"0\""), "null == \"0\" → F (the PHP 8 trap)");
    assert!(cell_live("null", "== false"), "null == false → T");
    assert!(cell_live("null", "== []"), "null == [] → T");
    assert!(cell_live("false", "== \"0\""), "false == \"0\" → T");
    assert!(!cell_live("false", "== \"abc\""), "false == \"abc\" → F");
    assert!(cell_live("true", "== \"abc\""), "true == \"abc\" → T");
    assert!(cell_live("true", "== \"5\""), "true == \"5\" → T");
    assert!(!cell_live("true", "== 0"), "true == 0 → F");
    assert!(cell_live("0", "== \"0\""), "0 == \"0\" → T");
    assert!(!cell_live("0", "== \"\""), "0 == \"\" → F");
    assert!(!cell_live("0", "== \"abc\""), "0 == \"abc\" → F (PHP 8, not 7)");
    assert!(!cell_live("\"0\"", "== \"\""), "\"0\" == \"\" → F");
    assert!(cell_live("\"5\"", "== \"5\""), "\"5\" == \"5\" → T");
    assert!(cell_live("[]", "== false"), "[] == false → T");
    assert!(!cell_live("[]", "== 0"), "[] == 0 → F");
}

/// Review counterexample (ADR-0002 live-path discipline): the env-free direct
/// pass must not report inside proven-dead regions — a decided guard's skipped
/// side and the tail after a terminating decided branch are not live paths.
#[test]
fn direct_pass_respects_proven_dead_regions() {
    // Tail after `if (<decided-true>) { return; }` is dead in three spellings.
    for case in [
        "<?php function width(int $w): int { return $w; }\n$c = 1; if ($c === 1) { return; } width(\"abc\");",
        "<?php function width(int $w): int { return $w; }\nif (true) { return; } width(\"abc\");",
        "<?php function width(int $w): int { return $w; }\nif (1 === 1) { return; } width(\"abc\");",
    ] {
        assert_eq!(n(case), 0, "dead tail must be silent: {case}");
    }
    // The decided-false skipped branch is dead; the fall-through stays live.
    let skipped = "<?php function width(int $w): int { return $w; }\nif (1 === 2) { width(\"abc\"); } width(\"def\");";
    let found = findings(skipped);
    assert_eq!(found.len(), 1, "only the live call fires: {found:?}");
    assert!(found[0].message.contains("\"def\""), "the live finding is the fall-through one: {found:?}");
    // An UNDECIDED guard keeps both sides live.
    let live = "<?php function width(int $w): int { return $w; }\nfunction f(int $c): void { if ($c === 999) { return; } width(\"abc\"); }";
    assert_eq!(n(live), 1, "maybe-live fall-through must still fire");
}
