//! ADR-0078 / issue #184: the overriding family — `override.final`,
//! `override.static-mismatch`, `override.visibility-weakened`,
//! `override.parameter-variance` and `override.return-variance`. Each claims a
//! fatal PHP raises **at class load**, read off the declaration graph alone (no
//! flow analysis, no value domain, no receiver, no sidecar), on the closure
//! discipline issue #183's tracer establishes and this slice reuses verbatim.
//!
//! Every runtime claim below is `php -r`-witnessed on PHP 8.5.9 and the witness is
//! quoted at the fixture that rests on it — the firing row AND its legal
//! counterpart, since a silence leg is only worth as much as the witness that it is
//! really legal. The harness runs on `NoFold` (no boot surface), which is itself the
//! no-sidecar-leg evidence.

use steins_infer::{
    Diagnostic, OVERRIDE_FINAL_ID, OVERRIDE_PARAMETER_VARIANCE_ID, OVERRIDE_RETURN_VARIANCE_ID,
    OVERRIDE_STATIC_MISMATCH_ID, OVERRIDE_VISIBILITY_WEAKENED_ID, check,
};
use steins_syntax::SourceTree;

/// Every finding this family emits, for the precedence and silence legs.
const FAMILY: [&str; 5] = [
    OVERRIDE_FINAL_ID,
    OVERRIDE_STATIC_MISMATCH_ID,
    OVERRIDE_VISIBILITY_WEAKENED_ID,
    OVERRIDE_PARAMETER_VARIANCE_ID,
    OVERRIDE_RETURN_VARIANCE_ID,
];

fn family(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "test.php").into_iter().filter(|d| FAMILY.contains(&d.id)).collect()
}

fn run(src: &str, id: &str) -> Vec<Diagnostic> {
    family(src).into_iter().filter(|d| d.id == id).collect()
}

fn only(src: &str, id: &str) -> Diagnostic {
    let all = family(src);
    assert_eq!(all.len(), 1, "expected exactly one family finding: {all:?}");
    assert_eq!(all[0].id, id, "{all:?}");
    all.into_iter().next().expect("len checked")
}

fn silent(src: &str) {
    let all = family(src);
    assert!(all.is_empty(), "{all:?}");
}

// ---------------------------------------------------------------------------
// `override.final`.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_overriding_a_final_method() {
    // php -r 'class P { final public function m() {} } class C extends P { public function m() {} }'
    // → Fatal error: Cannot override final method P::m()
    let d = only(
        "<?php\nclass P { final public function m() {} }\nclass C extends P { public function m() {} }\n",
        OVERRIDE_FINAL_ID,
    );
    assert!(d.message.contains("C::m() overrides final method P::m()"), "{}", d.message);
    assert!(d.message.contains("fatal when the class is loaded"), "{}", d.message);
    assert_eq!(d.line, 3, "positioned at the overriding method: {d:?}");
}

#[test]
fn fires_without_a_sidecar() {
    // The whole harness runs on `NoFold` (no boot surface), so every firing fixture
    // is also this leg's evidence: no id here has an `absence_family_available` gate.
    assert_eq!(
        run(
            "<?php\nclass P { final public function m() {} }\nclass C extends P { public function m() {} }\n",
            OVERRIDE_FINAL_ID
        )
        .len(),
        1
    );
}

#[test]
fn fires_on_a_grandparents_final_method() {
    // php -r 'class A { final public function m() {} } class B extends A {} class C extends B { public function m() {} }'
    // → Fatal error: Cannot override final method A::m()
    let d = only(
        "<?php\nclass A { final public function m() {} }\nclass B extends A {}\nclass C extends B { public function m() {} }\n",
        OVERRIDE_FINAL_ID,
    );
    assert!(d.message.contains("final method A::m()"), "{}", d.message);
}

#[test]
fn fires_on_an_abstract_child_over_a_final_parent() {
    // php -r 'class P { final public function m() {} } abstract class C extends P { abstract public function m(); }'
    // → Fatal error: Cannot override final method P::m() — finality outranks the
    // "cannot make non abstract method abstract" shape.
    only(
        "<?php\nclass P { final public function m() {} }\nabstract class C extends P { abstract public function m(); }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn silent_when_the_child_is_the_final_one() {
    // php -r 'class P { public function m() {} } class C extends P { final public function m() {} }'
    // → runs clean: sealing an override is legal.
    silent("<?php\nclass P { public function m() {} }\nclass C extends P { final public function m() {} }\n");
}

// ---------------------------------------------------------------------------
// `override.static-mismatch`.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_a_static_override_of_an_instance_method() {
    // php -r 'class P { public function m() {} } class C extends P { public static function m() {} }'
    // → Fatal error: Cannot make non static method P::m() static in class C
    let d = only(
        "<?php\nclass P { public function m() {} }\nclass C extends P { public static function m() {} }\n",
        OVERRIDE_STATIC_MISMATCH_ID,
    );
    assert!(d.message.contains("makes P::m() static"), "{}", d.message);
    assert!(d.message.contains("fatal when the class is loaded"), "{}", d.message);
}

#[test]
fn fires_on_an_instance_override_of_a_static_method() {
    // php -r 'class P { public static function m() {} } class C extends P { public function m() {} }'
    // → Fatal error: Cannot make static method P::m() non static in class C
    let d = only(
        "<?php\nclass P { public static function m() {} }\nclass C extends P { public function m() {} }\n",
        OVERRIDE_STATIC_MISMATCH_ID,
    );
    assert!(d.message.contains("makes P::m() non-static"), "{}", d.message);
}

#[test]
fn silent_when_both_sides_agree_on_staticness() {
    // php -r 'class P { public static function m() {} } class C extends P { public static function m() {} }'
    // → runs clean.
    silent("<?php\nclass P { public static function m() {} }\nclass C extends P { public static function m() {} }\n");
}

// ---------------------------------------------------------------------------
// `override.visibility-weakened`.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_public_weakened_to_protected() {
    // php -r 'class P { public function m() {} } class C extends P { protected function m() {} }'
    // → Fatal error: Access level to C::m() must be public (as in class P)
    let d = only(
        "<?php\nclass P { public function m() {} }\nclass C extends P { protected function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("from public to protected"), "{}", d.message);
    assert!(d.message.contains("fatal when the class is loaded"), "{}", d.message);
}

#[test]
fn fires_on_public_weakened_to_private() {
    // php -r 'class P { public function m() {} } class C extends P { private function m() {} }'
    // → Fatal error: Access level to C::m() must be public (as in class P)
    let d = only(
        "<?php\nclass P { public function m() {} }\nclass C extends P { private function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("from public to private"), "{}", d.message);
}

#[test]
fn fires_on_protected_weakened_to_private() {
    // php -r 'class P { protected function m() {} } class C extends P { private function m() {} }'
    // → Fatal error: Access level to C::m() must be protected (as in class P) or weaker
    let d = only(
        "<?php\nclass P { protected function m() {} }\nclass C extends P { private function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("from protected to private"), "{}", d.message);
}

#[test]
fn silent_on_widened_visibility() {
    // php -r 'class P { protected function m() {} } class C extends P { public function m() {} }'
    // → runs clean; widening is exactly what LSP permits.
    silent("<?php\nclass P { protected function m() {} }\nclass C extends P { public function m() {} }\n");
}

// ---------------------------------------------------------------------------
// `override.parameter-variance` (contravariance).
// ---------------------------------------------------------------------------

#[test]
fn fires_on_a_narrowed_parameter_type() {
    // php -r 'class P { public function m(int|string $x) {} } class C extends P { public function m(int $x) {} }'
    // → Fatal error: Declaration of C::m(int $x) must be compatible with P::m(string|int $x)
    let d = only(
        "<?php\nclass P { public function m(int|string $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
    assert!(d.message.contains("narrows parameter $x"), "{}", d.message);
    assert!(d.message.contains("to `int`"), "{}", d.message);
    assert!(d.message.contains("fatal when the class is loaded"), "{}", d.message);
}

#[test]
fn fires_on_a_parameter_narrowed_out_of_nullability() {
    // php -r 'class P { public function m(?int $x) {} } class C extends P { public function m(int $x) {} }'
    // → Fatal error: Declaration of C::m(int $x) must be compatible with P::m(?int $x)
    only(
        "<?php\nclass P { public function m(?int $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn fires_on_a_disjoint_parameter_type() {
    // php -r 'class P { public function m(string $x) {} } class C extends P { public function m(int $x) {} }'
    // → Fatal error: Declaration of C::m(int $x) must be compatible with P::m(string $x)
    only(
        "<?php\nclass P { public function m(string $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn silent_on_a_widened_parameter_type() {
    // php -r 'class P { public function m(int $x) {} } class C extends P { public function m(int|string $x) {} }'
    // → runs clean; contravariance is what PHP asks for.
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(int|string $x) {} }\n",
    );
}

#[test]
fn silent_on_a_renamed_parameter_and_an_added_optional_one() {
    // php -r 'class P { public function m(int $x) {} } class C extends P { public function m(int $z, int $y = 0) {} }'
    // → runs clean: names do not participate, and an extra OPTIONAL parameter is legal.
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(int $z, int $y = 0) {} }\n",
    );
}

#[test]
fn silent_when_a_parameter_type_is_dropped() {
    // php -r 'class P { public function m(int $x) {} } class C extends P { public function m($x) {} }'
    // → runs clean: an untyped parameter accepts everything.
    silent("<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m($x) {} }\n");
}

#[test]
fn silent_on_an_arity_change_alone() {
    // Both directions ARE fatals PHP raises —
    // php -r 'class P { public function m(int $x, int $y) {} } class C extends P { public function m(int $x) {} }'
    // → Declaration of C::m(int $x) must be compatible with P::m(int $x, int $y) —
    // but the shape is an arity change, not a variance one, and this id's name would
    // misname it. A deliberate v1 silence with its own deferred id.
    silent(
        "<?php\nclass P { public function m(int $x, int $y) {} }\nclass C extends P { public function m(int $x) {} }\n",
    );
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(int $x, int $y) {} }\n",
    );
}

// ---------------------------------------------------------------------------
// `override.return-variance` (covariance).
// ---------------------------------------------------------------------------

#[test]
fn fires_on_a_widened_return_type() {
    // php -r 'class P { public function m(): int {} } class C extends P { public function m(): int|string {} }'
    // → Fatal error: Declaration of C::m(): string|int must be compatible with P::m(): int
    let d = only(
        "<?php\nclass P { public function m(): int {} }\nclass C extends P { public function m(): int|string {} }\n",
        OVERRIDE_RETURN_VARIANCE_ID,
    );
    assert!(d.message.contains("widens the return type of P::m()"), "{}", d.message);
    assert!(d.message.contains("from `int`"), "{}", d.message);
    assert!(d.message.contains("fatal when the class is loaded"), "{}", d.message);
}

#[test]
fn fires_on_a_return_made_nullable() {
    // php -r 'class P { public function m(): int {} } class C extends P { public function m(): ?int {} }'
    // → Fatal error: Declaration of C::m(): ?int must be compatible with P::m(): int
    only(
        "<?php\nclass P { public function m(): int {} }\nclass C extends P { public function m(): ?int {} }\n",
        OVERRIDE_RETURN_VARIANCE_ID,
    );
}

#[test]
fn silent_on_a_narrowed_return_type() {
    // php -r 'class P { public function m(): int|string {} } class C extends P { public function m(): int {} }'
    // → runs clean; covariance is what PHP asks for.
    silent(
        "<?php\nclass P { public function m(): int|string {} }\nclass C extends P { public function m(): int {} }\n",
    );
}

#[test]
fn silent_when_the_parent_declares_no_return_type() {
    // php -r 'class P { public function m() {} } class C extends P { public function m(): int {} }'
    // → runs clean: adding a return type narrows, which is legal.
    silent("<?php\nclass P { public function m() {} }\nclass C extends P { public function m(): int {} }\n");
}

#[test]
fn silent_when_the_child_drops_the_return_type() {
    // php -r 'class P { public function m(): int {} } class C extends P { public function m() {} }'
    // → Fatal error: Declaration of C::m() must be compatible with P::m(): int — real,
    // but the syntax layer lowers an unrepresentable hint (`void`, `mixed`, a DNF
    // form) to the same `None` an ABSENT hint lowers to, so "declares nothing" is not
    // distinguishable from "declares something Steins does not carry". A yield loss,
    // never a false positive.
    silent("<?php\nclass P { public function m(): int {} }\nclass C extends P { public function m() {} }\n");
}

#[test]
fn silent_on_a_self_returning_pair() {
    // php -r 'class P { public function m(): self {} } class C extends P { public function m(): self {} }'
    // → runs clean, and PHP re-binds `self` per declarer. Steins synthesizes the
    // keyword to an `Instance` of the DECLARING class (ADR-0043 amendment), so
    // comparing the two sides would compare `P` against `C` — the pair is skipped.
    silent("<?php\nclass P { public function m(): self {} }\nclass C extends P { public function m(): self {} }\n");
}

// ---------------------------------------------------------------------------
// The acceptance relation: `Maybe` is silence, and only that relation judges.
// ---------------------------------------------------------------------------

#[test]
fn silent_when_the_acceptance_answer_is_maybe() {
    // The variance verdict is `steins_contract::normalize::subsumes`, whose class
    // arms judge through the reflexive is-a floor: `Class(A)` vs `Class(B)` is
    // `Maybe`, never `No`, so an unrelated-class narrowing does not convict —
    // php -r 'class A {} class B {} class P { public function m(A $x) {} } class C extends P { public function m(B $x) {} }'
    // → Fatal error: Declaration of C::m(B $x) must be compatible with P::m(A $x).
    // A real fatal, deliberately unreported: `Maybe` is silence (the standing
    // zero-FP rule), and this is the leg that pins it.
    silent(
        "<?php\nclass A {}\nclass B {}\nclass P { public function m(A $x) {} }\nclass C extends P { public function m(B $x) {} }\n",
    );
}

#[test]
fn silent_when_a_covariant_class_pair_is_only_maybe() {
    // The mirror, where PHP runs clean and `Maybe` costs nothing:
    // php -r 'class S {} class T extends S {} class P { public function m(): S {} } class C extends P { public function m(): T {} }'
    // → runs clean (return covariance).
    silent(
        "<?php\nclass S {}\nclass T extends S {}\nclass P { public function m(): S {} }\nclass C extends P { public function m(): T {} }\n",
    );
}

#[test]
fn silent_when_a_bool_arm_is_only_partly_covered() {
    // php -r 'class P { public function m(bool $x) {} } class C extends P { public function m(true $x) {} }'
    // → Fatal error: Declaration of C::m(true $x) must be compatible with P::m(bool $x).
    // Real, and deliberately unreported: the parent's `bool` is ONE arm, and the
    // acceptance relation folds its two finite members to `Maybe` (`true` is
    // admitted, `false` is not). Proven-partial reads as `Maybe` there, and `Maybe`
    // is silence.
    silent("<?php\nclass P { public function m(bool $x) {} }\nclass C extends P { public function m(true $x) {} }\n");
}

#[test]
fn silent_when_the_pair_differs_only_by_the_int_float_allowance() {
    // php -r 'class P { public function m(int $x) {} } class C extends P { public function m(float $x) {} }'
    // → Fatal error: Declaration of C::m(float $x) must be compatible with P::m(int $x).
    // PHP's inheritance check is a pure subtype test with no coercion, while the
    // acceptance relation carries PHP's weak-mode int→float widening (`float` admits
    // an `int` fact) — so it answers `Yes` and the check stays silent. A yield loss
    // by construction, in the direction that can only lose findings.
    silent("<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(float $x) {} }\n");
}

#[test]
fn silent_on_an_asserted_only_premise() {
    // v1 judges NATIVE signatures only. A `@param` claim is Asserted (ADR-0037/0052
    // N2) and PHP does not read it when it decides this fatal —
    // php -r 'class P { /** @param int|string $x */ public function m($x) {} } class C extends P { /** @param int $x */ public function m($x) {} }'
    // → runs clean. A docblock cannot forge a proof-layer finding.
    silent(
        "<?php\nclass P {\n  /** @param int|string $x */\n  public function m($x) {}\n}\nclass C extends P {\n  /** @param int $x */\n  public function m($x) {}\n}\n",
    );
}

#[test]
fn silent_when_a_docblock_contradicts_a_compatible_native_pair() {
    // The sharper face: the native pair is legal (widening), and only the docblock
    // narrows. Still silence — the Asserted stratum does not participate at all.
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P {\n  /** @param int $x */\n  public function m(int|string $x) {}\n}\n",
    );
}

// ---------------------------------------------------------------------------
// Interface implementation — the same path as class inheritance.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_an_interface_method_narrowed_by_its_implementation() {
    // php -r 'interface I { public function m(int|string $x); } class C implements I { public function m(int $x) {} }'
    // → Fatal error: Declaration of C::m(int $x) must be compatible with I::m(string|int $x)
    let d = only(
        "<?php\ninterface I { public function m(int|string $x); }\nclass C implements I { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
    assert!(d.message.contains("I::m()"), "{}", d.message);
}

#[test]
fn fires_on_an_interface_implementation_that_weakens_visibility() {
    // php -r 'interface I { public function m(); } class C implements I { protected function m() {} }'
    // → Fatal error: Access level to C::m() must be public (as in class I)
    only(
        "<?php\ninterface I { public function m(); }\nclass C implements I { protected function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

#[test]
fn fires_on_an_interface_implementation_that_widens_the_return() {
    // php -r 'interface I { public function m(): int; } class C implements I { public function m(): int|string {} }'
    // → Fatal error: Declaration of C::m(): string|int must be compatible with I::m(): int
    only(
        "<?php\ninterface I { public function m(): int; }\nclass C implements I { public function m(): int|string {} }\n",
        OVERRIDE_RETURN_VARIANCE_ID,
    );
}

#[test]
fn fires_on_an_interface_method_narrowed_by_an_abstract_ancestor() {
    // The transitive collection: the interface is implemented by the PARENT, and the
    // subject still owes it —
    // php -r 'interface I { public function m(int|string $x); } abstract class P implements I {} class C extends P { public function m(int $x) {} }'
    // → Fatal error: Declaration of C::m(int $x) must be compatible with I::m(string|int $x)
    only(
        "<?php\ninterface I { public function m(int|string $x); }\nabstract class P implements I {}\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn silent_on_a_legal_interface_implementation() {
    // php -r 'interface I { public function m(int $x); } class C implements I { public function m(int|string $x) {} }'
    // → runs clean.
    silent(
        "<?php\ninterface I { public function m(int $x); }\nclass C implements I { public function m(int|string $x) {} }\n",
    );
}

#[test]
fn silent_on_an_interface_subject() {
    // php -r 'interface J { public function m(int|string $x); } interface I extends J { public function m(int $x); }'
    // → Fatal error: Declaration of I::m(int $x) must be compatible with J::m(string|int $x).
    // Real, but the ancestry walk is class-shaped (it refuses an interface node
    // outright) — a recorded v1 silence, never a false positive.
    silent(
        "<?php\ninterface J { public function m(int|string $x); }\ninterface I extends J { public function m(int $x); }\n",
    );
}

// ---------------------------------------------------------------------------
// `__construct` — the exemption, pinned in both directions.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_a_constructor_narrowing_against_a_concrete_parent() {
    // php -r 'class P { public function __construct(int|string $x) {} } class C extends P { public function __construct(int $x) {} }'
    // → runs clean: `__construct` is excluded from PHP's LSP signature check.
    silent(
        "<?php\nclass P { public function __construct(int|string $x) {} }\nclass C extends P { public function __construct(int $x) {} }\n",
    );
}

#[test]
fn silent_on_a_constructor_weakening_visibility_against_a_concrete_parent() {
    // php -r 'class P { public function __construct() {} } class C extends P { private function __construct() {} }'
    // → runs clean; a private constructor beneath a public one is the singleton idiom.
    silent(
        "<?php\nclass P { public function __construct() {} }\nclass C extends P { private function __construct() {} }\n",
    );
}

#[test]
fn silent_on_a_static_constructor() {
    // php -r 'class P { public function __construct() {} } class C extends P { public static function __construct() {} }'
    // → Fatal error: Method C::__construct() cannot be static — a standalone fatal
    // that needs no parent at all, so `override.static-mismatch` would misname it.
    silent(
        "<?php\nclass P { public function __construct() {} }\nclass C extends P { public static function __construct() {} }\n",
    );
}

#[test]
fn fires_on_overriding_a_final_constructor() {
    // php -r 'class P { final public function __construct() {} } class C extends P { public function __construct() {} }'
    // → Fatal error: Cannot override final method P::__construct() — the ONE member
    // of this family a constructor does not escape.
    only(
        "<?php\nclass P { final public function __construct() {} }\nclass C extends P { public function __construct() {} }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn fires_on_a_constructor_narrowing_against_an_interface() {
    // The exemption ends at an ABSTRACT parent constructor —
    // php -r 'interface I { public function __construct(int|string $x); } class C implements I { public function __construct(int $x) {} }'
    // → Fatal error: Declaration of C::__construct(int $x) must be compatible with I::__construct(string|int $x)
    only(
        "<?php\ninterface I { public function __construct(int|string $x); }\nclass C implements I { public function __construct(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn fires_on_a_constructor_weakening_visibility_against_an_abstract_parent() {
    // php -r 'abstract class P { abstract public function __construct(); } class C extends P { protected function __construct() {} }'
    // → Fatal error: Access level to C::__construct() must be public (as in class P)
    only(
        "<?php\nabstract class P { abstract public function __construct(); }\nclass C extends P { protected function __construct() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

#[test]
fn fires_on_a_destructor_weakening_visibility() {
    // Only `__construct` is special —
    // php -r 'class P { public function __destruct() {} } class C extends P { private function __destruct() {} }'
    // → Fatal error: Access level to C::__destruct() must be public (as in class P)
    only(
        "<?php\nclass P { public function __destruct() {} }\nclass C extends P { private function __destruct() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

// ---------------------------------------------------------------------------
// A private parent method is not inherited — silence for every member.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_a_private_parent_method() {
    // php -r 'class P { private function m(int $x) {} } class C extends P { public static function m(string $y, array $z): void {} }'
    // → runs clean: a private method is not inherited, so nothing is overridden —
    // not the signature, not the staticness, not the visibility.
    silent(
        "<?php\nclass P { private function m(int $x) {} }\nclass C extends P { public static function m(string $y) {} }\n",
    );
}

#[test]
fn a_private_declaration_shadowing_an_ancestor_is_reported_at_the_shadower() {
    // php -r 'class A { public function m(int|string $x) {} } class B extends A { private function m() {} } class C extends B { public function m(int $x) {} }'
    // → Fatal error: Access level to B::m() must be public (as in class A). The fatal
    // is at **B**, and exactly one finding is emitted, naming B: C's own override
    // question is silenced by B's private declaration (a private method is not
    // inherited, so C overrides nothing through the chain), which is what keeps this
    // from double-reporting one runtime fatal.
    let d = only(
        "<?php\nclass A { public function m(int|string $x) {} }\nclass B extends A { private function m() {} }\nclass C extends B { public function m(int $x) {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("B::m() weakens the visibility of A::m()"), "{}", d.message);
    assert_eq!(d.line, 3, "positioned at B's declaration, not C's: {d:?}");
}

// ---------------------------------------------------------------------------
// The closure conditions inherited from issue #183's tracer, one leg each.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_an_unresolvable_parent() {
    // The signature could be anything (or the method absent) in a class Steins cannot
    // enumerate — the chain must be enumerable end to end.
    silent("<?php\nclass C extends \\Vendor\\Base { public function m(int $x) {} }\n");
}

#[test]
fn silent_on_a_trait_using_class() {
    // php -r 'class P { public function m(int|string $x) {} } trait T {} class C extends P { use T; public function m(int $x) {} }'
    // → Fatal error: Declaration of C::m(int $x) must be compatible with P::m(string|int $x).
    // Real, but trait members are not flattened, so a trait could be the member
    // source — the tracer's obstacle (ADR-0049 leg (e)), inherited verbatim.
    silent(
        "<?php\nclass P { public function m(int|string $x) {} }\ntrait T {}\nclass C extends P { use T; public function m(int $x) {} }\n",
    );
}

#[test]
fn silent_when_an_ancestor_uses_a_trait() {
    // The obstacle is per-node over the WHOLE chain, not just the subject.
    silent(
        "<?php\ntrait T {}\nclass A { public function m(int|string $x) {} }\nclass B extends A { use T; }\nclass C extends B { public function m(int $x) {} }\n",
    );
}

#[test]
fn silent_on_an_ambiguous_subject_name() {
    // Two declarations of the same FQN: which signature binds is load order's business.
    silent(
        "<?php\nclass P { public function m(int|string $x) {} }\nclass C extends P { public function m(int $x) {} }\nclass C extends P { public function m(int|string $x) {} }\n",
    );
}

#[test]
fn silent_on_an_ambiguous_parent_name() {
    silent(
        "<?php\nclass P { public function m(int|string $x) {} }\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
    );
}

#[test]
fn silent_in_a_dead_branch() {
    // Live-path discipline (ADR-0002/0031): a declaration inside a proven-dead region
    // never loads, so it never fatals.
    silent(
        "<?php\nclass P { final public function m() {} }\nif (false) {\n  class C extends P { public function m() {} }\n}\n",
    );
}

#[test]
fn silent_on_a_conditional_ancestor_under_a_standing_dam() {
    // A2i: a guarded declaration leaves which signature binds to load order.
    silent(
        "<?php\neval($code);\nif (defined('X')) {\n  class P { final public function m() {} }\n}\nclass C extends P { public function m() {} }\n",
    );
}

#[test]
fn fires_on_a_conditional_ancestor_with_the_dam_clear() {
    // The same fixture without the dynamism site: A2i is about the dam, not about
    // conditionality by itself.
    only(
        "<?php\nif (defined('X')) {\n  class P { final public function m() {} }\n}\nclass C extends P { public function m() {} }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn silent_on_an_anonymous_class() {
    // php -r 'class P { final public function m() {} } $x = new class extends P { public function m() {} };'
    // → Fatal error: Cannot override final method P::m(). Real, but `new class`
    // lowers EDGE-ONLY (ADR-0049 A4: parent + implements refs, no members), so the
    // subject's own methods are invisible and the claim would be unfounded.
    silent(
        "<?php\nclass P { final public function m() {} }\n$x = new class extends P { public function m() {} };\n",
    );
}

#[test]
fn silent_on_an_enum_subject() {
    // php -r 'interface I { public function m(int|string $x); } enum E implements I { case A; public function m(int $x) {} }'
    // → Fatal error: Declaration of E::m(int $x) must be compatible with I::m(string|int $x).
    // Real, but enum members are not lowered at all (ADR-0043).
    silent(
        "<?php\ninterface I { public function m(int|string $x); }\nenum E implements I { case A; public function m(int $x) {} }\n",
    );
}

#[test]
fn silent_on_an_abstract_child_over_a_concrete_parent() {
    // php -r 'class P { public function m(int|string $x) {} } abstract class C extends P { abstract public function m(int $x); }'
    // → Fatal error: Cannot make non abstract method P::m() abstract in class C — a
    // DIFFERENT fatal, which none of these ids may claim.
    silent(
        "<?php\nclass P { public function m(int|string $x) {} }\nabstract class C extends P { abstract public function m(int $x); }\n",
    );
}

// ---------------------------------------------------------------------------
// Precedence — one runtime fatal, one finding, in PHP's own witnessed order
// (final ≻ static ≻ visibility ≻ parameter ≻ return).
// ---------------------------------------------------------------------------

#[test]
fn final_outranks_every_other_member() {
    // php -r 'class P { final public function m(int|string $x) {} } class C extends P { protected static function m(int $x) {} }'
    // → Fatal error: Cannot override final method P::m()
    only(
        "<?php\nclass P { final public function m(int|string $x) {} }\nclass C extends P { protected static function m(int $x) {} }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn static_outranks_visibility_and_variance() {
    // php -r 'class P { public function m(int|string $x) {} } class C extends P { protected static function m(int $x) {} }'
    // → Fatal error: Cannot make non static method P::m() static in class C
    only(
        "<?php\nclass P { public function m(int|string $x) {} }\nclass C extends P { protected static function m(int $x) {} }\n",
        OVERRIDE_STATIC_MISMATCH_ID,
    );
}

#[test]
fn visibility_outranks_variance() {
    // php -r 'class P { public function m(int|string $x): int {} } class C extends P { protected function m(int $x): int|string {} }'
    // → Fatal error: Access level to C::m() must be public (as in class P)
    only(
        "<?php\nclass P { public function m(int|string $x): int {} }\nclass C extends P { protected function m(int $x): int|string {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

#[test]
fn a_parameter_violation_outranks_a_return_one() {
    // php -r 'class P { public function m(int|string $x): int {} } class C extends P { public function m(int $x): int|string {} }'
    // → ONE fatal: Declaration of C::m(int $x): string|int must be compatible with
    // P::m(string|int $x): int. One fatal, one finding.
    only(
        "<?php\nclass P { public function m(int|string $x): int {} }\nclass C extends P { public function m(int $x): int|string {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn a_parent_and_an_interface_declaring_the_same_method_report_once() {
    // php -r 'interface I { public function m(int|string $x); } class P implements I { public function m(int|string $x) {} } class C extends P { public function m(int $x) {} }'
    // → ONE fatal, named against P (the nearest declaration).
    let d = only(
        "<?php\ninterface I { public function m(int|string $x); }\nclass P implements I { public function m(int|string $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
    assert!(d.message.contains("P::m()"), "{}", d.message);
}

// ---------------------------------------------------------------------------
// Namespaced rendering.
// ---------------------------------------------------------------------------

#[test]
fn renders_namespaced_declarations_with_qualified_names() {
    // php -r 'namespace App; class P { final public function m() {} } class C extends P { public function m() {} }'
    // → Fatal error: Cannot override final method App\P::m()
    let d = only(
        "<?php\nnamespace App;\nclass P { final public function m() {} }\nclass C extends P { public function m() {} }\n",
        OVERRIDE_FINAL_ID,
    );
    assert!(d.message.contains("App\\C::m()"), "{}", d.message);
    assert!(d.message.contains("App\\P::m()"), "{}", d.message);
}
