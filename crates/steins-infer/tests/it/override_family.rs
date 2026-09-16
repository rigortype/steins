//! ADR-0078 / issue #184: the overriding family — `override.final`,
//! `override.static-mismatch`, `override.visibility-weakened`,
//! `override.parameter-variance` and `override.return-variance`. Each claims a
//! fatal PHP raises **at class load**, read off the declaration graph alone (no
//! flow analysis, value domain, receiver, or sidecar) — reusing issue #183's
//! closure-discipline tracer verbatim.
//!
//! Every claim is `php -r`-witnessed on PHP 8.5.9, quoted at the test that
//! consumes it — firing row AND its legal counterpart. The harness runs on
//! `NoFold` (no boot surface), itself the no-sidecar-leg evidence.

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

// `override.final`.

#[test]
fn fires_on_overriding_a_final_method() {
    // php -r → Fatal: Cannot override final method P::m()
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
    // NoFold (no boot surface): no id here gates on `absence_family_available`.
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
    // php -r → Fatal: Cannot override final method A::m()
    let d = only(
        "<?php\nclass A { final public function m() {} }\nclass B extends A {}\nclass C extends B { public function m() {} }\n",
        OVERRIDE_FINAL_ID,
    );
    assert!(d.message.contains("final method A::m()"), "{}", d.message);
}

#[test]
fn fires_on_an_abstract_child_over_a_final_parent() {
    // php -r → Fatal: finality outranks "cannot make non abstract method abstract".
    only(
        "<?php\nclass P { final public function m() {} }\nabstract class C extends P { abstract public function m(); }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn silent_when_the_child_is_the_final_one() {
    // php -r → runs clean: sealing an override is legal.
    silent("<?php\nclass P { public function m() {} }\nclass C extends P { final public function m() {} }\n");
}

// `override.static-mismatch`.

#[test]
fn fires_on_a_static_override_of_an_instance_method() {
    // php -r → Fatal: Cannot make non static method P::m() static in class C
    let d = only(
        "<?php\nclass P { public function m() {} }\nclass C extends P { public static function m() {} }\n",
        OVERRIDE_STATIC_MISMATCH_ID,
    );
    assert!(d.message.contains("makes P::m() static"), "{}", d.message);
    assert!(d.message.contains("fatal when the class is loaded"), "{}", d.message);
}

#[test]
fn fires_on_an_instance_override_of_a_static_method() {
    // php -r → Fatal: Cannot make static method P::m() non static in class C
    let d = only(
        "<?php\nclass P { public static function m() {} }\nclass C extends P { public function m() {} }\n",
        OVERRIDE_STATIC_MISMATCH_ID,
    );
    assert!(d.message.contains("makes P::m() non-static"), "{}", d.message);
}

#[test]
fn silent_when_both_sides_agree_on_staticness() {
    silent("<?php\nclass P { public static function m() {} }\nclass C extends P { public static function m() {} }\n");
}

// `override.visibility-weakened`.

#[test]
fn fires_on_public_weakened_to_protected() {
    // php -r → Fatal: Access level to C::m() must be public (as in class P)
    let d = only(
        "<?php\nclass P { public function m() {} }\nclass C extends P { protected function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("from public to protected"), "{}", d.message);
    assert!(d.message.contains("fatal when the class is loaded"), "{}", d.message);
}

#[test]
fn fires_on_public_weakened_to_private() {
    // php -r → Fatal: Access level to C::m() must be public (as in class P)
    let d = only(
        "<?php\nclass P { public function m() {} }\nclass C extends P { private function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("from public to private"), "{}", d.message);
}

#[test]
fn fires_on_protected_weakened_to_private() {
    // php -r → Fatal: Access level to C::m() must be protected (as in class P) or weaker
    let d = only(
        "<?php\nclass P { protected function m() {} }\nclass C extends P { private function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("from protected to private"), "{}", d.message);
}

#[test]
fn silent_on_widened_visibility() {
    // php -r → runs clean; widening is exactly what LSP permits.
    silent("<?php\nclass P { protected function m() {} }\nclass C extends P { public function m() {} }\n");
}

// `override.parameter-variance` (contravariance).

#[test]
fn fires_on_a_narrowed_parameter_type() {
    // php -r → Fatal: Declaration of C::m(int $x) incompatible with P::m(string|int $x)
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
    // php -r → Fatal: Declaration of C::m(int $x) incompatible with P::m(?int $x)
    only(
        "<?php\nclass P { public function m(?int $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn fires_on_a_disjoint_parameter_type() {
    // php -r → Fatal: Declaration of C::m(int $x) incompatible with P::m(string $x)
    only(
        "<?php\nclass P { public function m(string $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn silent_on_a_widened_parameter_type() {
    // php -r → runs clean; contravariance is what PHP asks for.
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(int|string $x) {} }\n",
    );
}

#[test]
fn silent_on_a_renamed_parameter_and_an_added_optional_one() {
    // php -r → runs clean: names don't participate; an extra OPTIONAL parameter is legal.
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(int $z, int $y = 0) {} }\n",
    );
}

#[test]
fn silent_when_a_parameter_type_is_dropped() {
    // php -r → runs clean: an untyped parameter accepts everything.
    silent("<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m($x) {} }\n");
}

#[test]
fn silent_on_an_arity_change_alone() {
    // Both directions fatal in PHP, but arity change isn't variance — v1 silence.
    silent(
        "<?php\nclass P { public function m(int $x, int $y) {} }\nclass C extends P { public function m(int $x) {} }\n",
    );
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(int $x, int $y) {} }\n",
    );
}

// `override.return-variance` (covariance).

#[test]
fn fires_on_a_widened_return_type() {
    // php -r → Fatal: Declaration of C::m(): string|int incompatible with P::m(): int
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
    // php -r → Fatal: Declaration of C::m(): ?int incompatible with P::m(): int
    only(
        "<?php\nclass P { public function m(): int {} }\nclass C extends P { public function m(): ?int {} }\n",
        OVERRIDE_RETURN_VARIANCE_ID,
    );
}

#[test]
fn silent_on_a_narrowed_return_type() {
    // php -r → runs clean; covariance is what PHP asks for.
    silent(
        "<?php\nclass P { public function m(): int|string {} }\nclass C extends P { public function m(): int {} }\n",
    );
}

#[test]
fn silent_when_the_parent_declares_no_return_type() {
    // php -r → runs clean: adding a return type narrows, which is legal.
    silent("<?php\nclass P { public function m() {} }\nclass C extends P { public function m(): int {} }\n");
}

#[test]
fn silent_when_the_child_drops_the_return_type() {
    // Real fatal (php -r); `void`/`mixed`/DNF lower to the same `None` as ABSENT (yield loss).
    silent("<?php\nclass P { public function m(): int {} }\nclass C extends P { public function m() {} }\n");
}

#[test]
fn silent_on_a_self_returning_pair() {
    // Runs clean; `self` synthesizes per DECLARING class (ADR-0043) — comparing is wrong.
    silent("<?php\nclass P { public function m(): self {} }\nclass C extends P { public function m(): self {} }\n");
}

// The acceptance relation: `Maybe` is silence, and only that relation judges.

#[test]
fn silent_when_the_acceptance_answer_is_maybe() {
    // `Class(A)` vs `Class(B)` judges `Maybe`, never `No` (reflexive is-a floor)
    // — a real fatal (php -r witnessed), deliberately unreported: zero-FP rule.
    silent(
        "<?php\nclass A {}\nclass B {}\nclass P { public function m(A $x) {} }\nclass C extends P { public function m(B $x) {} }\n",
    );
}

#[test]
fn silent_when_a_covariant_class_pair_is_only_maybe() {
    // The mirror: PHP runs clean here, so `Maybe` costs nothing (return covariance).
    silent(
        "<?php\nclass S {}\nclass T extends S {}\nclass P { public function m(): S {} }\nclass C extends P { public function m(): T {} }\n",
    );
}

#[test]
fn silent_when_a_bool_arm_is_only_partly_covered() {
    // Real fatal; `bool`'s two members fold to `Maybe` (proven-partial reads as `Maybe`).
    silent("<?php\nclass P { public function m(bool $x) {} }\nclass C extends P { public function m(true $x) {} }\n");
}

#[test]
fn silent_when_the_pair_differs_only_by_the_int_float_allowance() {
    // Real fatal; PHP's check has no coercion, but int->float widens weakly (yield loss).
    silent("<?php\nclass P { public function m(int $x) {} }\nclass C extends P { public function m(float $x) {} }\n");
}

#[test]
fn silent_on_an_asserted_only_premise() {
    // v1 judges NATIVE signatures only (PHP ignores `@param`, Asserted, ADR-0037/0052
    // N2) — a docblock cannot forge a proof-layer finding.
    silent(
        "<?php\nclass P {\n  /** @param int|string $x */\n  public function m($x) {}\n}\nclass C extends P {\n  /** @param int $x */\n  public function m($x) {}\n}\n",
    );
}

#[test]
fn silent_when_a_docblock_contradicts_a_compatible_native_pair() {
    // Sharper: the native pair is legal (widening); only the docblock narrows — silence.
    silent(
        "<?php\nclass P { public function m(int $x) {} }\nclass C extends P {\n  /** @param int $x */\n  public function m(int|string $x) {}\n}\n",
    );
}

// Interface implementation — the same path as class inheritance.

#[test]
fn fires_on_an_interface_method_narrowed_by_its_implementation() {
    // php -r → Fatal: Declaration of C::m(int $x) incompatible with I::m(string|int $x)
    let d = only(
        "<?php\ninterface I { public function m(int|string $x); }\nclass C implements I { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
    assert!(d.message.contains("I::m()"), "{}", d.message);
}

#[test]
fn fires_on_an_interface_implementation_that_weakens_visibility() {
    // php -r → Fatal: Access level to C::m() must be public (as in class I)
    only(
        "<?php\ninterface I { public function m(); }\nclass C implements I { protected function m() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

#[test]
fn fires_on_an_interface_implementation_that_widens_the_return() {
    // php -r → Fatal: Declaration of C::m(): string|int incompatible with I::m(): int
    only(
        "<?php\ninterface I { public function m(): int; }\nclass C implements I { public function m(): int|string {} }\n",
        OVERRIDE_RETURN_VARIANCE_ID,
    );
}

#[test]
fn fires_on_an_interface_method_narrowed_by_an_abstract_ancestor() {
    // Transitive: the interface is implemented by the PARENT, and C still owes it.
    only(
        "<?php\ninterface I { public function m(int|string $x); }\nabstract class P implements I {}\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn silent_on_a_legal_interface_implementation() {
    silent(
        "<?php\ninterface I { public function m(int $x); }\nclass C implements I { public function m(int|string $x) {} }\n",
    );
}

#[test]
fn silent_on_an_interface_subject() {
    // Real fatal (php -r); the walk is class-shaped (refuses interface nodes) — v1 silence.
    silent(
        "<?php\ninterface J { public function m(int|string $x); }\ninterface I extends J { public function m(int $x); }\n",
    );
}

// `__construct` — the exemption, pinned in both directions.

#[test]
fn silent_on_a_constructor_narrowing_against_a_concrete_parent() {
    // php -r → runs clean: __construct is excluded from PHP's LSP signature check.
    silent(
        "<?php\nclass P { public function __construct(int|string $x) {} }\nclass C extends P { public function __construct(int $x) {} }\n",
    );
}

#[test]
fn silent_on_a_constructor_weakening_visibility_against_a_concrete_parent() {
    // php -r → runs clean; a private ctor beneath a public one is the singleton idiom.
    silent(
        "<?php\nclass P { public function __construct() {} }\nclass C extends P { private function __construct() {} }\n",
    );
}

#[test]
fn silent_on_a_static_constructor() {
    // Fatal (php -r witnessed) but standalone; `override.static-mismatch` would misname it.
    silent(
        "<?php\nclass P { public function __construct() {} }\nclass C extends P { public static function __construct() {} }\n",
    );
}

#[test]
fn fires_on_overriding_a_final_constructor() {
    // php -r → Fatal: Cannot override final method P::__construct() (the one exception).
    only(
        "<?php\nclass P { final public function __construct() {} }\nclass C extends P { public function __construct() {} }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn fires_on_a_constructor_narrowing_against_an_interface() {
    // The exemption ends at an ABSTRACT parent constructor (php -r witnessed fatal).
    only(
        "<?php\ninterface I { public function __construct(int|string $x); }\nclass C implements I { public function __construct(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn fires_on_a_constructor_weakening_visibility_against_an_abstract_parent() {
    // php -r → Fatal: Access level to C::__construct() must be public (as in class P)
    only(
        "<?php\nabstract class P { abstract public function __construct(); }\nclass C extends P { protected function __construct() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

#[test]
fn fires_on_a_destructor_weakening_visibility() {
    // Only __construct is special; __destruct gets the ordinary fatal (php -r witnessed).
    only(
        "<?php\nclass P { public function __destruct() {} }\nclass C extends P { private function __destruct() {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

// A private parent method is not inherited — silence for every member.

#[test]
fn silent_on_a_private_parent_method() {
    // php -r → runs clean: a private method isn't inherited, so nothing is overridden.
    silent(
        "<?php\nclass P { private function m(int $x) {} }\nclass C extends P { public static function m(string $y) {} }\n",
    );
}

#[test]
fn a_private_declaration_shadowing_an_ancestor_is_reported_at_the_shadower() {
    // The fatal is at **B** (php -r); C's question is silenced by B's private declaration.
    let d = only(
        "<?php\nclass A { public function m(int|string $x) {} }\nclass B extends A { private function m() {} }\nclass C extends B { public function m(int $x) {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
    assert!(d.message.contains("B::m() weakens the visibility of A::m()"), "{}", d.message);
    assert_eq!(d.line, 3, "positioned at B's declaration, not C's: {d:?}");
}

// The closure conditions inherited from issue #183's tracer, one leg each.

#[test]
fn silent_on_an_unresolvable_parent() {
    // The signature could be anything in a class Steins can't enumerate end to end.
    silent("<?php\nclass C extends \\Vendor\\Base { public function m(int $x) {} }\n");
}

#[test]
fn silent_on_a_trait_using_class() {
    // Real fatal (php -r witnessed); trait members aren't flattened (ADR-0049 leg (e)).
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
    // Live-path discipline (ADR-0002/0031): a proven-dead declaration never loads or fatals.
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
    // Same fixture without the dynamism site — A2i is about the dam, not conditionality.
    only(
        "<?php\nif (defined('X')) {\n  class P { final public function m() {} }\n}\nclass C extends P { public function m() {} }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn silent_on_an_anonymous_class() {
    // Real fatal (php -r witnessed); `new class` lowers EDGE-ONLY (ADR-0049 A4, no members).
    silent(
        "<?php\nclass P { final public function m() {} }\n$x = new class extends P { public function m() {} };\n",
    );
}

#[test]
fn silent_on_an_enum_subject() {
    // Real fatal (php -r witnessed), but enum members aren't lowered at all (ADR-0043).
    silent(
        "<?php\ninterface I { public function m(int|string $x); }\nenum E implements I { case A; public function m(int $x) {} }\n",
    );
}

#[test]
fn silent_on_an_abstract_child_over_a_concrete_parent() {
    // A DIFFERENT fatal ("non abstract method abstract", php -r witnessed) — not these ids.
    silent(
        "<?php\nclass P { public function m(int|string $x) {} }\nabstract class C extends P { abstract public function m(int $x); }\n",
    );
}

// Precedence — one runtime fatal, one finding, in PHP's own witnessed order
// (final ≻ static ≻ visibility ≻ parameter ≻ return).

#[test]
fn final_outranks_every_other_member() {
    // php -r → Fatal: Cannot override final method P::m()
    only(
        "<?php\nclass P { final public function m(int|string $x) {} }\nclass C extends P { protected static function m(int $x) {} }\n",
        OVERRIDE_FINAL_ID,
    );
}

#[test]
fn static_outranks_visibility_and_variance() {
    // php -r → Fatal: Cannot make non static method P::m() static in class C
    only(
        "<?php\nclass P { public function m(int|string $x) {} }\nclass C extends P { protected static function m(int $x) {} }\n",
        OVERRIDE_STATIC_MISMATCH_ID,
    );
}

#[test]
fn visibility_outranks_variance() {
    // php -r → Fatal: Access level to C::m() must be public (as in class P)
    only(
        "<?php\nclass P { public function m(int|string $x): int {} }\nclass C extends P { protected function m(int $x): int|string {} }\n",
        OVERRIDE_VISIBILITY_WEAKENED_ID,
    );
}

#[test]
fn a_parameter_violation_outranks_a_return_one() {
    // php -r → ONE fatal, one finding (parameter beats return in PHP's own check order).
    only(
        "<?php\nclass P { public function m(int|string $x): int {} }\nclass C extends P { public function m(int $x): int|string {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
}

#[test]
fn a_parent_and_an_interface_declaring_the_same_method_report_once() {
    // php -r → ONE fatal, named against P (the nearest declaration).
    let d = only(
        "<?php\ninterface I { public function m(int|string $x); }\nclass P implements I { public function m(int|string $x) {} }\nclass C extends P { public function m(int $x) {} }\n",
        OVERRIDE_PARAMETER_VARIANCE_ID,
    );
    assert!(d.message.contains("P::m()"), "{}", d.message);
}

// Namespaced rendering.

#[test]
fn renders_namespaced_declarations_with_qualified_names() {
    // php -r → Fatal: Cannot override final method App\P::m()
    let d = only(
        "<?php\nnamespace App;\nclass P { final public function m() {} }\nclass C extends P { public function m() {} }\n",
        OVERRIDE_FINAL_ID,
    );
    assert!(d.message.contains("App\\C::m()"), "{}", d.message);
    assert!(d.message.contains("App\\P::m()"), "{}", d.message);
}
