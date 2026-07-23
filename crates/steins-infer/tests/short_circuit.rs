//! Short-circuit refinement acceptance tests (ADR-0052 §6 / slice N3): env-threaded
//! `&&`/`||` verdicts, retained guard calls with sequenced invalidation, nested
//! `-if-true`/`-if-false` consumption, ternary-arm threading, and the `$a ?? $b`
//! rvalue fact.
//!
//! The through-line: the RIGHT operand of `&&`/`||` evaluates under the env the
//! LEFT operand establishes (`then_refinements(a)` for `&&`, `else_refinements(a)`
//! for `||`), exactly as PHP sequences them — so a contradiction (`$x===5 && $x===7`)
//! is proven dead, a tautology (`$x===5 || $x===7` over `{5,7}`) proves its else
//! dead, and a guard method call keeps its receiver on the guarded path. Every
//! new visibility is checked against the zero-FP discipline: the threaded env is
//! walk-local (never leaks past the verdict), and an Asserted `-if-true` narrowing
//! consumed in a nested position still cannot premise a proof-layer id.

use steins_infer::{Diagnostic, ID, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "demo.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

fn arg_mismatch(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == ID).count()
}

/// `function width(int $w)` header + a bad string local `$bad = "abc"`.
const HDR: &str = "<?php\nfunction width(int $w): int { return $w; }\n";

// ==========================================================================
// `&&` verdict threading: the right operand sees `then_refinements(left)`.
// ==========================================================================

#[test]
fn and_threading_prunes_contradiction() {
    // `$x === 5 && $x === 6`: the right operand evaluates under `$x = 5` (the left's
    // then-refinement), so `$x === 6` is a decided No → the whole `&&` is No → the
    // then-branch is dead → the propagated `width($bad)` inside it is never walked.
    // Before threading, both operands saw the unknown param `$x` (Maybe/Maybe →
    // Maybe), the branch was walked, and the finding fired inside dead code.
    let src = format!(
        "{HDR}function f($x): void {{ $bad = \"abc\"; if ($x === 5 && $x === 6) {{ width($bad); }} }}"
    );
    assert_eq!(n(&src), 0, "&& contradiction is proven dead by threading → silent");
}

#[test]
fn and_threading_control_non_contradiction_stays_live() {
    // The control: `$x === 5 && $x === 5` is NOT a contradiction — under threading
    // the right operand is Yes, the `&&` is Maybe, the then-branch is LIVE, and the
    // finding fires. This proves the prune above is the contradiction, not a bug.
    let src = format!(
        "{HDR}function f($x): void {{ $bad = \"abc\"; if ($x === 5 && $x === 5) {{ width($bad); }} }}"
    );
    assert_eq!(n(&src), 1, "&& non-contradiction stays live → flagged");
}

// ==========================================================================
// `||` verdict threading: the right operand sees `else_refinements(left)`.
// ==========================================================================

#[test]
fn or_threading_prunes_tautology_else() {
    // `$x === 5 || $x === 7` over `$x ∈ {5,7}`: the right operand evaluates under the
    // left's else-refinement (`$x !== 5` → `$x = 7`), so `$x === 7` is Yes → the
    // `||` is Yes → the else-branch is dead. Before threading the `||` was Maybe and
    // the else was walked, firing inside dead code.
    let src = format!(
        "{HDR}function f($c): void {{ $bad = \"abc\"; $x = $c ? 5 : 7; if ($x === 5 || $x === 7) {{ }} else {{ width($bad); }} }}"
    );
    assert_eq!(n(&src), 0, "|| tautology over a finite fact proves its else dead → silent");
}

#[test]
fn or_threading_control_non_tautology_else_stays_live() {
    // Control: `$x === 5 || $x === 9` over `{5,7}` is NOT exhaustive — `$x = 7`
    // reaches the else, which stays live and fires.
    let src = format!(
        "{HDR}function f($c): void {{ $bad = \"abc\"; $x = $c ? 5 : 7; if ($x === 5 || $x === 9) {{ }} else {{ width($bad); }} }}"
    );
    assert_eq!(n(&src), 1, "|| non-tautology → else reachable → flagged");
}

// ==========================================================================
// Ternary arm env threading (ADR-0052 §6): arms resolve under then/else refinements.
// ==========================================================================

#[test]
fn ternary_then_arm_sees_then_refinement() {
    // `($x === "abc") ? $x : "abc"`: the undecided guard joins the two arms; the
    // THEN arm `$x` resolves under `then_refinements` (`$x = "abc"`), so both arms
    // are `"abc"` → the join collapses to `Singleton("abc")` → `width($w)` fires.
    // Without arm threading the then arm `$x` was unknown → no fact → silent.
    let src = format!(
        "{HDR}function f($x): void {{ $w = ($x === \"abc\") ? $x : \"abc\"; width($w); }}"
    );
    assert_eq!(n(&src), 1, "ternary then-arm sees the guard's then-refinement → Singleton → flagged");
}

// ==========================================================================
// Retained guard calls: the method receiver survives (issue #9 regression shape).
// ==========================================================================

#[test]
fn guard_method_call_preserves_receiver() {
    // Issue #9 / §6 payoff (i): `$u !== null && $u->name()` — the guard method call
    // `$u->name()` does NOT rebind its receiver `$u`, so `$u` (an exact `U`) survives
    // into the then-branch and `$u->m("abc")` resolves and fires (a real TypeError).
    // The OLD blanket `cond_invalidations` forgot `$u` (the call's read-set included
    // the receiver), so the body could not resolve the call — the over-invalidation
    // the sequenced version fixes.
    let src = "<?php
class U {
    public function name(): string { return \"x\"; }
    public function m(int $w): void {}
}
function f(): void {
    $u = new U();
    if ($u !== null && $u->name()) { $u->m(\"abc\"); }
}
";
    assert_eq!(n(src), 1, "guard method call keeps its receiver → body resolves → flagged");
}

#[test]
fn guard_method_call_no_fp_on_typed_param() {
    // The named regression shape in its bare form: `$x !== null && $x->foo()` on a
    // (non-seeded) typed param stays SILENT — the threading adds visibility, never a
    // false positive.
    let src = "<?php
class U { public function foo(): bool { return true; } }
function f(?U $x): void {
    if ($x !== null && $x->foo()) { $x->foo(); }
}
";
    assert_eq!(n(src), 0, "$x !== null && $x->foo() is silent (no manufactured finding)");
}

// ==========================================================================
// Sequenced by-ref invalidation (obligation #2): f's effect lands at its position.
// ==========================================================================

#[test]
fn sequenced_by_ref_invalidation_forgets_receiver() {
    // `nuke($x)` takes `$x` by reference and nulls it. In `nuke($x) && cond()`, the
    // by-ref invalidation lands at the call's position, so the then-branch no longer
    // sees the stale `Foo` class — `$x->m("abc")` cannot resolve → silent. This is
    // the sequencing counterpart of the receiver-preservation test.
    let src = "<?php
class Foo { public function m(int $w): void {} }
function nuke(&$x): bool { $x = null; return true; }
function cond(): bool { return true; }
function f(): void {
    $x = new Foo();
    if (nuke($x) && cond()) { $x->m(\"abc\"); }
}
";
    assert_eq!(n(src), 0, "by-ref call invalidates $x before the then-branch → no stale resolution");
}

#[test]
fn sequenced_control_receiver_call_keeps_receiver() {
    // Control / contrast: when `$x` is the guard call's RECEIVER (`$x->check()`)
    // rather than an argument, it is NOT forgotten (a method call does not rebind
    // its receiver), so the body resolves and fires. This isolates the silence above
    // as the by-ref ARGUMENT effect, not a blanket guard-call forget.
    let src = "<?php
class Foo {
    public function m(int $w): void {}
    public function check(): bool { return true; }
}
function cond(): bool { return true; }
function f(): void {
    $x = new Foo();
    if ($x->check() && cond()) { $x->m(\"abc\"); }
}
";
    assert_eq!(n(src), 1, "receiver-position guard call keeps $x → body resolves → flagged");
}

// ==========================================================================
// Nested `-if-true`/`-if-false` consumption (§6 payoff (ii)).
// ==========================================================================

#[test]
fn nested_if_true_fires_contract_layer() {
    // `if ($c && isInt($x))` — the guard call `isInt` sits in a NESTED `&&` position.
    // N3 consumes its `@phpstan-assert-if-true int` on the then-branch (Asserted),
    // so `takesString($x)` fires the CONTRACT layer (`phpdoc.param-mismatch` accepts
    // Asserted). Before N3 only a TOP-LEVEL guard call was consumed, so this was 0.
    let src = "<?php
/** @phpstan-assert-if-true int $x */
function isInt($x): bool { return true; }
/** @param string $s */
function takesString($s): void {}
function f($c, mixed $x): void {
    if ($c && isInt($x)) { takesString($x); }
}
";
    assert_eq!(
        findings(src).iter().filter(|d| d.id == PARAM_MISMATCH_ID).count(),
        1,
        "nested -if-true narrows $x → contract-layer finding fires"
    );
}

#[test]
fn nested_if_true_cannot_premise_proof() {
    // The stratum gate survives the nested position (N2 rule, §5): a nested
    // `@phpstan-assert-if-true null` narrows `$x` at the Asserted stratum, so the
    // downstream `takesInt($x)` (a native `int` param — proof layer) stays SILENT.
    let src = "<?php
/** @phpstan-assert-if-true null $x */
function isNull($x): bool { return true; }
function takesInt(int $n): void {}
function f($c, mixed $x): void {
    if ($c && isNull($x)) { takesInt($x); }
}
";
    assert_eq!(arg_mismatch(src), 0, "an Asserted -if-true in a nested && cannot forge a proof");
}

// ==========================================================================
// Short-circuit: right-operand facts must not leak onto the short path.
// ==========================================================================

#[test]
fn or_short_path_does_not_leak_right_operand_fact() {
    // `if ($c || $x === "abc")` — the `||` is true via `$c` WITHOUT testing `$x`, so
    // the then-branch must NOT assume `$x === "abc"`. `then_refinements` of an `||`
    // is empty (De Morgan attributes only on the false path), so `width($x)` sees an
    // unrefined `$x` → silent. A leak would flag it.
    let src = format!(
        "{HDR}function f($c, $x): void {{ if ($c || $x === \"abc\") {{ width($x); }} }}"
    );
    assert_eq!(n(&src), 0, "right-operand fact does not leak onto the || short path");
}

// ==========================================================================
// `$a ?? $b` — clear_null(fact($a)) join fact($b) (§6).
// ==========================================================================

#[test]
fn coalesce_null_lhs_collapses_to_rhs() {
    // `null ?? "abc"`: `clear_null(null)` empties, so the value is exactly `"abc"`
    // (a `Singleton`) → `width($x)` fires.
    let src = format!(
        "{HDR}function f(): void {{ $a = null; $x = $a ?? \"abc\"; width($x); }}"
    );
    assert_eq!(n(&src), 1, "null ?? \"abc\" → Singleton(\"abc\") → flagged");
}

#[test]
fn coalesce_equal_operands_is_singleton() {
    // `$a ?? $b` where both are the same non-null bad value → `Singleton` → fires.
    let src = format!(
        "{HDR}function f(): void {{ $a = \"abc\"; $b = \"abc\"; $x = $a ?? $b; width($x); }}"
    );
    assert_eq!(n(&src), 1, "\"abc\" ?? \"abc\" → Singleton(\"abc\") → flagged");
}

#[test]
fn coalesce_differing_operands_widens_and_is_silent() {
    // `$a ?? $b` with differing operands → a widening OneOf, which never resolves to
    // one proven value → silent. The widening is the FP-safe side: a fact `??` cannot
    // narrow to a single bad value it is not sure of.
    let src = format!(
        "{HDR}function f(): void {{ $a = \"abc\"; $b = 5; $x = $a ?? $b; width($x); }}"
    );
    assert_eq!(n(&src), 0, "\"abc\" ?? 5 → OneOf → silent (widening)");
}

#[test]
fn coalesce_on_array_offset_manufactures_nothing() {
    // Adversarial: `$arr['k'] ?? "abc"` — the offset lowers to `Other` (no offset
    // machinery yet), so the `??` sees no fact for the left operand and yields NO
    // fact. `width($x)` stays silent — `??` never manufactures certainty for a value
    // it cannot spell.
    let src = format!(
        "{HDR}function f(array $arr): void {{ $x = $arr['k'] ?? \"abc\"; width($x); }}"
    );
    assert_eq!(n(&src), 0, "?? on an unseen array offset manufactures no fact");
}
