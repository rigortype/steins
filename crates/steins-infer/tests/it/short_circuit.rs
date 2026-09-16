//! Short-circuit refinement tests (ADR-0052 §6 / N3): env-threaded `&&`/`||`
//! verdicts, retained guard calls with sequenced invalidation, nested
//! `-if-true`/`-if-false` consumption, ternary-arm threading, and `$a ?? $b`.
//!
//! Through-line: the RIGHT operand of `&&`/`||` evaluates under the env the LEFT
//! establishes (`then_refinements(a)` for `&&`, `else_refinements(a)` for `||`),
//! as PHP sequences them — so `$x===5 && $x===7` proves dead, `$x===5 || $x===7`
//! over `{5,7}` proves its else dead, and a guard method call keeps its receiver.
//! Zero-FP discipline: the threaded env is walk-local, and an Asserted `-if-true`
//! narrowing in a nested position still cannot premise a proof-layer id.

use steins_infer::{Diagnostic, ID, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    // Drop `untyped.*` (ADR-0078, #200): it flags the fixtures' own deliberately
    // untyped signatures, not the behavior under test.
    check(&tree, &functions, "demo.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn n(src: &str) -> usize {
    findings(src).len()
}

fn arg_mismatch(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == ID).count()
}

/// `function width(int $w)` header + a bad string local `$bad = "abc"`.
const HDR: &str = "<?php\nfunction width(int $w): int { return $w; }\n";

// `&&` verdict threading: the right operand sees `then_refinements(left)`.

#[test]
fn and_threading_prunes_contradiction() {
    // `$x === 5 && $x === 6`: right operand evaluates under `$x = 5` (left's
    // then-refinement) → decided No → then-branch dead, no finding inside it.
    let src = format!(
        "{HDR}function f($x): void {{ $bad = \"abc\"; if ($x === 5 && $x === 6) {{ width($bad); }} }}"
    );
    assert_eq!(n(&src), 0, "&& contradiction is proven dead by threading → silent");
}

#[test]
fn and_threading_control_non_contradiction_stays_live() {
    // Control: `$x === 5 && $x === 5` is not a contradiction — threading makes the
    // right operand Yes, the branch stays LIVE, proving the prune above isn't a bug.
    let src = format!(
        "{HDR}function f($x): void {{ $bad = \"abc\"; if ($x === 5 && $x === 5) {{ width($bad); }} }}"
    );
    assert_eq!(n(&src), 1, "&& non-contradiction stays live → flagged");
}

// `||` verdict threading: the right operand sees `else_refinements(left)`.

#[test]
fn or_threading_prunes_tautology_else() {
    // `$x === 5 || $x === 7` over `$x ∈ {5,7}`: right operand evaluates under the
    // left's else-refinement (`$x !== 5` → `$x = 7`) → Yes → else branch dead.
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

// Ternary arm env threading (ADR-0052 §6): arms resolve under then/else refinements.

#[test]
fn ternary_then_arm_sees_then_refinement() {
    // `($x === "abc") ? $x : "abc"`: the THEN arm `$x` resolves under
    // `then_refinements` → both arms `"abc"` → join collapses to `Singleton` →
    // fires. Without arm threading `$x` was unknown → no fact → silent.
    let src = format!(
        "{HDR}function f($x): void {{ $w = ($x === \"abc\") ? $x : \"abc\"; width($w); }}"
    );
    assert_eq!(n(&src), 1, "ternary then-arm sees the guard's then-refinement → Singleton → flagged");
}

// Retained guard calls: the method receiver survives (issue #9 regression shape).

#[test]
fn guard_method_call_preserves_receiver() {
    // Issue #9 / §6 payoff (i): a guard method call `$u->name()` does NOT rebind
    // its receiver, so `$u` survives into the then-branch and resolves. The OLD
    // blanket `cond_invalidations` forgot `$u` — the over-invalidation this fixes.
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
    // The named regression shape, bare: `$x !== null && $x->foo()` on a typed param
    // stays SILENT — threading adds visibility, never a false positive.
    let src = "<?php
class U { public function foo(): bool { return true; } }
function f(?U $x): void {
    if ($x !== null && $x->foo()) { $x->foo(); }
}
";
    assert_eq!(n(src), 0, "$x !== null && $x->foo() is silent (no manufactured finding)");
}

// Sequenced by-ref invalidation (obligation #2): f's effect lands at its position.

#[test]
fn sequenced_by_ref_invalidation_forgets_receiver() {
    // `nuke(&$x)` nulls `$x` by reference; in `nuke($x) && cond()` the invalidation
    // lands at the call's position, so the stale `Foo` class can't resolve → silent.
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
    // Control: `$x` as the guard call's RECEIVER (not an argument) is NOT forgotten,
    // isolating the silence above as the by-ref ARGUMENT effect, not a blanket one.
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

// Nested `-if-true`/`-if-false` consumption (§6 payoff (ii)).

#[test]
fn nested_if_true_fires_contract_layer() {
    // `isInt`'s guard sits in a NESTED `&&` position; its `@phpstan-assert-if-true
    // int` is consumed on the then branch (Asserted) → contract-layer mismatch fires.
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
    // Stratum gate survives nesting (N2, §5): a nested `@phpstan-assert-if-true null`
    // narrows at the Asserted stratum, so a native `int` param (proof layer) is silent.
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

// Short-circuit: right-operand facts must not leak onto the short path.

#[test]
fn or_short_path_does_not_leak_right_operand_fact() {
    // `$c || $x === "abc"` is true via `$c` without testing `$x`; `then_refinements`
    // of `||` is empty (De Morgan attributes only the false path) → unrefined, silent.
    let src = format!(
        "{HDR}function f($c, $x): void {{ if ($c || $x === \"abc\") {{ width($x); }} }}"
    );
    assert_eq!(n(&src), 0, "right-operand fact does not leak onto the || short path");
}

// `$a ?? $b` — clear_null(fact($a)) join fact($b) (§6).

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
fn coalesce_differing_operands_take_the_settled_left() {
    // A *proven* left operand is not one member of a widening — it is the answer.
    // PHP evaluates the right arm of `??` only when the left is unset or null, so
    // `$b` here is never reached and `$x` is exactly `"abc"`. Measured at
    // `PINNED_PHP` 8.5.9 rather than reasoned:
    //
    // ```
    // php -r '$a = "abc"; $b = 5; $x = $a ?? $b; var_dump($x);'
    // string(3) "abc"
    // php -r 'function side() { echo "RAN\n"; return 9; }
    //         $a = "abc"; var_dump($a ?? side());'
    // string(3) "abc"          // "RAN" never printed
    // ```
    //
    // So `width($x)` fires, and the finding is a true positive. Before issue #630
    // the settled short-circuit was computed only for a projection arm, this
    // widened to `OneOf`, and the silence was accidental rather than FP-safe.
    let src = format!(
        "{HDR}function f(): void {{ $a = \"abc\"; $b = 5; $x = $a ?? $b; width($x); }}"
    );
    assert_eq!(n(&src), 1, "\"abc\" ?? 5 → the left arm settles → Singleton(\"abc\") → flagged");
}

#[test]
fn coalesce_on_array_offset_manufactures_nothing() {
    // Adversarial: `$arr['k']` lowers to `Other` (no offset machinery yet), so `??`
    // sees no left-operand fact and yields none — never manufactures certainty.
    let src = format!(
        "{HDR}function f(array $arr): void {{ $x = $arr['k'] ?? \"abc\"; width($x); }}"
    );
    assert_eq!(n(&src), 0, "?? on an unseen array offset manufactures no fact");
}
