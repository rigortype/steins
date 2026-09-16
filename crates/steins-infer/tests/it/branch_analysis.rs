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
    // `untyped.*` (ADR-0078, #200) reports on the fixtures' own deliberately-untyped
    // declarations, not the behaviour under test; dropped to keep counts meaningful.
    check(&tree, &functions, "demo.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn n(src: &str) -> usize {
    findings(src).len()
}

/// `function width(int $w)` header + a bad string local `$bad = "abc"`.
const HDR: &str = "<?php\nfunction width(int $w): int { return $w; }\n$bad = \"abc\";\n";

// cond-decided pruning, both directions

#[test]
fn cond_true_walks_then_branch() {
    let src = format!("{HDR}$x = 5;\nif ($x === 5) {{ width($bad); }}\n");
    assert_eq!(n(&src), 1, "decided-true guard → then-branch live → flagged");
}

#[test]
fn cond_false_prunes_then_branch() {
    let src = format!("{HDR}$x = 5;\nif ($x === 6) {{ width($bad); }}\n");
    assert_eq!(n(&src), 0, "decided-false guard → then-branch dead → silent");
}

#[test]
fn unreachable_after_terminating_then_emits_nothing() {
    let src = "<?php
function width(int $w): int { return $w; }
function f(): void {
    $bad = \"abc\";
    if (true) { return; }
    width($bad);
}
";
    assert_eq!(n(src), 0, "code after a terminating decided-true then is unreachable → silent");
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

// fall-through joins

#[test]
fn join_agree_keeps_fact() {
    let src = format!(
        "{HDR}if ($cond) {{ $w = \"abc\"; }} else {{ $w = \"abc\"; }}\nwidth($w);"
    );
    assert_eq!(n(&src), 1, "agreeing branches → Singleton survives → flagged");
}

#[test]
fn join_differ_becomes_oneof_and_is_silent() {
    let src = format!(
        "{HDR}if ($cond) {{ $w = \"abc\"; }} else {{ $w = \"xyz\"; }}\nwidth($w);"
    );
    assert_eq!(n(&src), 0, "differing branches → OneOf → not one proven value → silent");
}

#[test]
fn join_absent_in_one_branch_drops_fact() {
    let src = format!("{HDR}if ($cond) {{ $w = \"abc\"; }}\nwidth($w);");
    assert_eq!(n(&src), 0, "fact absent on the else path → dropped → silent");
}

// positive refinement

#[test]
fn positive_refinement_binds_then_branch() {
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
    let src = "<?php
function width(int $w): int { return $w; }
function f($x): void {
    if ($x !== \"abc\") { return; }
    width($x);
}
f(\"anything\");
";
    // Direct pass (variable, not descent): guard filtering narrows $x to the literal.
    assert_eq!(n(src), 1, "else of !== narrows to the literal on the fall-through → flagged");
}

// early-exit pruning

#[test]
fn early_exit_unknown_stays_silent_no_negative_facts() {
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
    // Contrast with the UNKNOWN case above: $x here is BOUND null (via descent).
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

// the loose null guard, one direction (issue #391)

#[test]
fn a_loose_null_guard_proves_non_null_on_the_branch_where_it_fails() {
    // `null == null` is true, so a branch reached only because `$x == null` was
    // FALSE cannot be holding null. `if ($x == null) { return; }` is the idiom.
    let src = "<?php
declare(strict_types=1);
function width(int $w): int { return $w; }
function f(?int $x): void {
    if ($x == null) { return; }
    \\PHPStan\\dumpType($x);
    width($x);
}
";
    let ds = findings(src);
    let dumps: Vec<&str> = ds.iter().filter(|d| d.id == "debug.type").map(|d| d.message.as_str()).collect();
    assert_eq!(dumps, vec!["dumped type: int"], "{ds:?}");
    assert!(ds.iter().all(|d| d.id == "debug.type"), "and nothing is reported: {ds:?}");
}

#[test]
fn a_loose_null_guard_proves_nothing_on_the_branch_where_it_holds() {
    // The other direction, refused: `0 == null` is true too, so the branch where
    // the guard holds knows only that the value is falsy — not that it is null.
    let src = "<?php
declare(strict_types=1);
function f(?int $x): void {
    if ($x == null) { \\PHPStan\\dumpType($x); }
}
";
    let ds = findings(src);
    let dumps: Vec<&str> =
        ds.iter().filter(|d| d.id == "debug.type").map(|d| d.message.as_str()).collect();
    assert_eq!(dumps, vec!["dumped type: int|null"], "the true branch narrows nothing");
}

#[test]
fn the_not_loose_spelling_is_the_same_rule_with_the_branches_swapped() {
    let src = "<?php
declare(strict_types=1);
function f(?int $x): void {
    if ($x != null) { \\PHPStan\\dumpType($x); }
}
";
    let ds = findings(src);
    let dumps: Vec<&str> =
        ds.iter().filter(|d| d.id == "debug.type").map(|d| d.message.as_str()).collect();
    assert_eq!(dumps, vec!["dumped type: int"]);
}

// elseif chains + nested ifs

#[test]
fn elseif_chain_selects_matching_arm() {
    let src = format!(
        "{HDR}$x = 2;\nif ($x === 1) {{ $w = \"ok1\"; }} elseif ($x === 2) {{ $w = \"abc\"; }} else {{ $w = \"okz\"; }}\nwidth($w);"
    );
    assert_eq!(n(&src), 1, "elseif ($x === 2) is the live arm → $w = \"abc\" → flagged");
}

#[test]
fn nested_ifs_preserve_untouched_fact() {
    let src = format!("{HDR}if ($a) {{ if ($b) {{ echo 1; }} }}\nwidth($bad);");
    assert_eq!(n(&src), 1, "nested ifs not writing $bad → fact survives → flagged");
}

// loop inside if stays conservative

#[test]
fn loop_inside_if_still_opaque() {
    // The ratchet: a loop nested in a then-branch stays `Opaque`, so it drops $bad.
    let src = format!(
        "{HDR}if ($cond) {{ while ($x) {{ $bad = 5; }} }}\nwidth($bad);"
    );
    assert_eq!(n(&src), 0, "loop-in-if writes $bad → dropped on that path → join drops it → silent");
}

// ternary values

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
    let src = format!("{HDR}$w = $c ? \"abc\" : \"abc\";\nwidth($w);");
    assert_eq!(n(&src), 1, "undecided ternary with equal arms → Singleton → flagged");
}

// call.on-null

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
    let src = "<?php
class U { public function name(): string { return \"x\"; } }
function f($c): void {
    $u = $c ? null : \"s\";
    $u->name();
}
";
    assert_eq!(n(src), 0, "OneOf of null and a string receiver → not proven null → silent");
}

// call.on-null on a depth-1 property-fetch receiver (ADR-0052 §7, Gap B)

#[test]
fn call_on_null_fires_on_proven_null_prop_receiver() {
    let src = "<?php
class A { public function m(): void {} }
class H { public ?A $p = null; }
$h = new H();
$h->p = null;
$h->p->m();
";
    let f = findings(src);
    assert_eq!(f.len(), 1, "proven-null prop receiver → call.on-null: {f:#?}");
    assert_eq!(f[0].id, "call.on-null");
    assert_eq!(
        f[0].message,
        "method call $h->p->m() — $h->p is proven null on this path — proven Error (Call to a member function on null)"
    );
}

#[test]
fn call_on_null_prop_receiver_silent_after_escape() {
    // Escape to an unknown call sweeps non-readonly props; read via the surviving
    // alias `$a` (not `$h`) to isolate the sweep from call-arg unbinding.
    let src = "<?php
class A { public function m(): void {} }
class H { public ?A $p = null; }
$h = new H();
$h->p = null;
$a = $h;
sink($h);
unknownFn();
$a->p->m();
";
    assert_eq!(n(src), 0, "swept prop → no proven-null fact → silent");
}

#[test]
fn call_on_null_prop_receiver_silent_for_nullsafe() {
    let src = "<?php
class A { public function m(): void {} }
class H { public ?A $p = null; }
$h = new H();
$h->p = null;
$h->p?->m();
";
    assert_eq!(n(src), 0, "nullsafe prop-receiver call on proven null → silent");
}

#[test]
fn call_on_null_prop_receiver_silent_on_asserted_stratum() {
    // An `Asserted` null prop fact (written from a `@phpstan-assert null` claim) is a
    // claim, not a proof — it must NOT premise the proof-layer `call.on-null` (N2).
    let src = "<?php
/** @phpstan-assert null $x */
function claimNull($x): void {}
class A { public function m(): void {} }
class H { public ?A $p = null; }
function f($x): void {
    claimNull($x);
    $h = new H();
    $h->p = $x;
    $h->p->m();
}
";
    assert_eq!(n(src), 0, "an Asserted-null prop receiver must NOT premise call.on-null");
}

#[test]
fn call_on_null_depth_2_chain_stays_silent() {
    // Depth stays exactly 1: a chained receiver is `Dynamic`, not represented.
    let src = "<?php
class H { public ?H $p = null; }
$h = new H();
$h->p = null;
$h->p->q->m();
";
    assert_eq!(n(src), 0, "depth-2 receiver chain → not represented → silent");
}

// empirical `==` cells (PHP 8.5.8; see php_loose_eq rustdoc)

/// Whether the then-branch of `if ($x <op> <rhs>)` is LIVE: a decided guard
/// reads Yes → 1 or No → 0, so this reads off the `==` verdict.
fn cell_live(x_lit: &str, cmp: &str) -> bool {
    let src = format!("{HDR}$x = {x_lit};\nif ($x {cmp}) {{ width($bad); }}\n");
    n(&src) == 1
}

#[test]
fn empirical_loose_eq_cells_decide_branches() {
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

/// Review counterexample (ADR-0002 live-path discipline): the direct pass must
/// not report inside proven-dead regions.
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
