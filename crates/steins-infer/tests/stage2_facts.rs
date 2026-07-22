//! Stage-2 abstract-fact tests (ADR-0031 negative facts, ADR-0035 four-layer
//! domain): native-type parameter seeding (Feature B), guard refinements that
//! produce Refined/General facts (Feature C), `@phpstan-assert` application
//! (Feature D), and contract acceptance consuming abstract facts (Feature E).
//!
//! The through-line: an argument that resolves to an *abstract* fact (not a
//! proven value) is now judged by the domain's **set** acceptance
//! (`steins_contract::admits_fact`) — only a definite `No` reports, `Maybe` is
//! silent, so the zero-FP bar holds. Includes two adversarial counterexamples.

use steins_infer::{
    CALL_ON_NULL_ID, Diagnostic, PARAM_MISMATCH_ID, RETURN_MISMATCH_ID, check,
};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn param_count(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == PARAM_MISMATCH_ID).count()
}

fn return_count(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == RETURN_MISMATCH_ID).count()
}

fn null_count(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == CALL_ON_NULL_ID).count()
}

// ==========================================================================
// B. Native-type parameter seeding feeds the abstract-fact contract check.
// ==========================================================================

#[test]
fn seeded_int_param_violates_string_contract() {
    // `int $x` seeds `General{Int}`; flowing it into a `@param string` param is a
    // definite contract violation (set semantics: no int is a string).
    let src = "<?php
/** @param string $s */ function g($s): void {}
function f(int $x): void { g($x); }
";
    assert_eq!(param_count(src), 1, "seeded int flowing into @param string → No");
}

#[test]
fn seeded_int_param_satisfies_int_contract_silently() {
    // The sound side: a seeded int into a param that accepts int is silent.
    let src = "<?php
/** @param int $n */ function g($n): void {}
function f(int $x): void { g($x); }
";
    assert_eq!(param_count(src), 0, "seeded int into @param int → Yes → silent");
}

#[test]
fn seeded_int_into_wider_types_silent() {
    // `int|string`, `scalar`, `mixed`, `numeric`, `float` (int→float core) all
    // admit an int → Maybe/Yes → silent (no false positive from seeding).
    for ty in ["int|string", "scalar", "mixed", "numeric", "float", "positive-int"] {
        let src = format!(
            "<?php\n/** @param {ty} $n */ function g($n): void {{}}\nfunction f(int $x): void {{ g($x); }}\n"
        );
        assert_eq!(param_count(&src), 0, "seeded int into @param {ty} must be silent");
    }
}

#[test]
fn seeded_nullable_string_does_not_fire_on_class_or_template() {
    // A seeded scalar fact vs a class-shaped @param stays silent (a bare
    // identifier may be a template / type-alias) — consistent with the proven-
    // value path treating scalar-vs-class as Maybe.
    let src = "<?php
/** @param T $x */ function identity($x) { return $x; }
function f(string $s): void { identity($s); }
";
    assert_eq!(param_count(src), 0, "scalar fact vs class/template identifier → silent");
}

#[test]
fn nullable_class_param_is_not_seeded_null_guard_still_works() {
    // `?U $u` is not a scalar → not seeded; the existing null-guard proof still
    // rides on the `=== null` refinement (Singleton null), while `!== null` and a
    // bare call stay silent.
    let base = "<?php
class U { public function m(): void {} }
";
    assert_eq!(null_count(&format!("{base}function f(?U $u): void {{ $u->m(); }}")), 0);
    assert_eq!(
        null_count(&format!("{base}function f(?U $u): void {{ if ($u !== null) {{ $u->m(); }} }}")),
        0,
        "!== null path: receiver not proven null → silent",
    );
    assert_eq!(
        null_count(&format!("{base}function f(?U $u): void {{ if ($u === null) {{ $u->m(); }} }}")),
        1,
        "=== null then-branch: receiver proven null → fires",
    );
}

// ==========================================================================
// C. Guard refinements produce Refined/General facts.
// ==========================================================================

#[test]
fn range_guard_narrows_to_positive_int() {
    // `$n > 0` on a seeded `General{Int}` intersects `int<1, max>`; the resulting
    // positive-int is provably disjoint from `negative-int` → No.
    let neg = "<?php
/** @param negative-int $x */ function want($x): void {}
function f(int $n): void { if ($n > 0) { want($n); } }
";
    assert_eq!(param_count(neg), 1, "$n > 0 → positive-int, disjoint from negative-int");

    // …and it satisfies positive-int → silent (no report where the guard proves it).
    let pos = "<?php
/** @param positive-int $x */ function want($x): void {}
function f(int $n): void { if ($n > 0) { want($n); } }
";
    assert_eq!(param_count(pos), 0, "$n > 0 → positive-int satisfies positive-int");

    // Without the guard, General{Int} vs negative-int is Maybe → silent.
    let ungated = "<?php
/** @param negative-int $x */ function want($x): void {}
function f(int $n): void { want($n); }
";
    assert_eq!(param_count(ungated), 0, "no guard: General int vs negative-int → Maybe");
}

#[test]
fn ge_guard_symmetric_and_flipped_operands() {
    // `>= 1` is positive-int; `0 < $n` (literal on the left) flips to `$n > 0`.
    for guard in ["$n >= 1", "0 < $n"] {
        let src = format!(
            "<?php\n/** @param negative-int $x */ function want($x): void {{}}\nfunction f(int $n): void {{ if ({guard}) {{ want($n); }} }}\n"
        );
        assert_eq!(param_count(&src), 1, "guard `{guard}` should prove positive → disjoint");
    }
}

#[test]
fn non_empty_refinement_from_ne_empty_string() {
    // `$s !== ''` adds NON_EMPTY to a seeded String fact; that refined fact is
    // provably not the empty-string literal type.
    let src = "<?php
/** @param '' $x */ function wantsEmpty($x): void {}
function f(string $s): void { if ($s !== '') { wantsEmpty($s); } }
";
    assert_eq!(param_count(src), 1, "!== '' → non-empty-string, disjoint from ''");

    // Outside the guard the seeded General{String} vs '' is Maybe → silent.
    let ungated = "<?php
/** @param '' $x */ function wantsEmpty($x): void {}
function f(string $s): void { wantsEmpty($s); }
";
    assert_eq!(param_count(ungated), 0, "no guard: General string vs '' → Maybe");
}

#[test]
fn ne_zero_string_does_not_add_non_empty() {
    // `$s !== '0'` must NOT add NON_EMPTY (NonFalsy needs both `''` and `'0'`
    // excluded) — so it stays Maybe against `''`.
    let src = "<?php
/** @param '' $x */ function wantsEmpty($x): void {}
function f(string $s): void { if ($s !== '0') { wantsEmpty($s); } }
";
    assert_eq!(param_count(src), 0, "!== '0' adds no predicate → Maybe → silent");
}

#[test]
fn truthy_string_guard_adds_non_falsy() {
    // `if ($s)` on a seeded string adds NON_FALSY; a non-falsy string is provably
    // not the falsy literal `'0'`.
    let src = "<?php
/** @param '0' $x */ function wantsZero($x): void {}
function f(string $s): void { if ($s) { wantsZero($s); } }
";
    assert_eq!(param_count(src), 1, "truthy string → non-falsy, disjoint from '0'");
}

#[test]
fn oneof_member_removal_via_ne_enables_null_proof() {
    // A `OneOf[null, \"s\"]` with `$x !== \"s\"` removes the string member, leaving
    // `Singleton(null)` — which the null-dereference proof then fires on. Without
    // member removal the receiver would stay a OneOf (Maybe) → silent.
    let src = "<?php
class U { public function m(): void {} }
function f($c): void { $x = $c ? null : \"s\"; if ($x !== \"s\") { $x->m(); } }
";
    assert_eq!(null_count(src), 1, "OneOf member removal → Singleton(null) → call.on-null");
}

// ==========================================================================
// D. `@phpstan-assert` application (Always).
// ==========================================================================

#[test]
fn always_assert_narrows_caller_var_then_contract_fires() {
    // The headline: `Util::assertInt($v)` binds `General{Int}` to `$v`, which then
    // flows into a `@param string` param → definite contract violation.
    let src = "<?php
class Util { /** @phpstan-assert int $v */ public static function assertInt($v): void {} }
/** @param string $x */ function needsString($x): void {}
function g($v): void { Util::assertInt($v); needsString($v); }
";
    assert_eq!(param_count(src), 1, "asserted int flowing into @param string → No");
}

#[test]
fn always_assert_satisfies_matching_contract_silently() {
    let src = "<?php
class Util { /** @phpstan-assert int $v */ public static function assertInt($v): void {} }
/** @param int $x */ function needsInt($x): void {}
function g($v): void { Util::assertInt($v); needsInt($v); }
";
    assert_eq!(param_count(src), 0, "asserted int into @param int → silent");
}

#[test]
fn function_always_assert_string_predicate() {
    // A free-function assertion helper asserting `non-empty-string`; the refined
    // fact is disjoint from the empty-string literal type.
    let src = "<?php
/** @phpstan-assert non-empty-string $s */ function assertNonEmpty($s): void {}
/** @param '' $x */ function wantsEmpty($x): void {}
function g($s): void { assertNonEmpty($s); wantsEmpty($s); }
";
    assert_eq!(param_count(src), 1, "asserted non-empty-string, disjoint from ''");
}

// ==========================================================================
// E. Contract acceptance on abstract facts — @return path.
// ==========================================================================

#[test]
fn seeded_param_returned_violates_return_contract() {
    let src = "<?php
/** @return string */ function f(int $x) { return $x; }
";
    assert_eq!(return_count(src), 1, "returning a seeded int under @return string → No");
}

#[test]
fn seeded_param_returned_satisfies_return_contract() {
    let src = "<?php
/** @return int */ function f(int $x) { return $x; }
";
    assert_eq!(return_count(src), 0, "returning a seeded int under @return int → silent");
}

// ==========================================================================
// Adversarial counterexamples (self-constructed).
// ==========================================================================

#[test]
fn counterexample_by_ref_param_is_not_seeded() {
    // A by-reference parameter must NOT be seeded: the caller aliases the variable
    // and may rebind it, so its entry type is not a fact we may propagate. If we
    // seeded `General{Int}` here, `g($x)` would spuriously report against
    // `@param string`. The value-typed sibling below *is* seeded (and fires),
    // isolating the guard.
    let byref = "<?php
/** @param string $s */ function g($s): void {}
function f(int &$x): void { g($x); }
";
    assert_eq!(param_count(byref), 0, "by-ref param not seeded → no abstract-fact report");

    let byval = "<?php
/** @param string $s */ function g($s): void {}
function f(int $x): void { g($x); }
";
    assert_eq!(param_count(byval), 1, "by-value sibling IS seeded → fires (isolates the guard)");
}

#[test]
fn counterexample_assert_does_not_override_stronger_singleton() {
    // Assertion application is replace-if-weaker: it must never override a proven
    // `Singleton`. Here `$v` is proven `\"5\"` (a valid string); asserting `int`
    // must keep the singleton, so `needsString($v)` stays silent. Were the assert
    // to override with `General{Int}`, `needsString(@param string)` would falsely
    // fire.
    let src = "<?php
class Util { /** @phpstan-assert int $v */ public static function assertInt($v): void {} }
/** @param string $x */ function needsString($x): void {}
function g(): void { $v = \"5\"; Util::assertInt($v); needsString($v); }
";
    assert_eq!(param_count(src), 0, "proven Singleton(\"5\") kept over weaker asserted int");
}
