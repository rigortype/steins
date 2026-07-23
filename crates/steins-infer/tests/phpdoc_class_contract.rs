//! ADR-0043 stage 4 — phpdoc-side class contracts (the `contract_touches_class`
//! opening). The acceptance matrix for **class-typed phpdoc contracts**
//! (`@param`/`@return`) against proven object values, enum cases, `::class`
//! strings, and abstract scalar facts — riding the same trinary is-a oracle as the
//! native object-world (ADR-0043 stage 3, `tests/object_acceptance.rs`).
//!
//! The phpdoc relation is **pure set membership, no coercion** (ADR-0030 relation
//! #1): a proven scalar is *never* a member of a class type, in either mode. A
//! definite `No` (proven object is-a-No a known class, or a scalar against a known
//! class) reports; any `Unknown` (incomplete hierarchy) or unresolved identifier
//! (a `@template` / `@phpstan-type` alias that may denote a scalar) stays silent.

use steins_infer::{Diagnostic, PARAM_MISMATCH_ID, RETURN_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn param_count(src: &str) -> usize {
    findings(src).into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).count()
}

fn return_count(src: &str) -> usize {
    findings(src).into_iter().filter(|d| d.id == RETURN_MISMATCH_ID).count()
}

fn ids(src: &str) -> Vec<String> {
    findings(src).into_iter().map(|d| d.id.to_owned()).collect()
}

// ==========================================================================
// 1. Proven object value vs class-typed @param — is-a Yes / No / Unknown.
// ==========================================================================

#[test]
fn object_vs_class_definite_no() {
    let src = "<?php final class User {} final class Robot {}\n\
        /** @param User $u */ function f($u): void {}\n\
        f(new Robot());";
    assert_eq!(ids(src), vec![PARAM_MISMATCH_ID], "Robot is-a-No User (both final)");
}

#[test]
fn object_vs_class_subclass_accepts() {
    let src = "<?php class Animal {} class Dog extends Animal {}\n\
        /** @param Animal $a */ function f($a): void {}\n\
        f(new Dog());";
    assert_eq!(param_count(src), 0, "Dog is-a Animal (Yes) → silent");
}

#[test]
fn object_vs_interface_no_and_yes() {
    let base = "<?php interface HasName {} final class Named implements HasName {} final class Anon {}\n\
        /** @param HasName $x */ function f($x): void {}\n";
    assert_eq!(param_count(&format!("{base}f(new Named());")), 0, "Named implements HasName");
    assert_eq!(param_count(&format!("{base}f(new Anon());")), 1, "Anon does not implement HasName");
}

#[test]
fn object_vs_class_unknown_stays_silent() {
    // Hierarchy leaves the project into an uncatalogued external → Unknown → silent.
    let src = "<?php interface Target {} class Mystery extends \\Vendor\\External {}\n\
        /** @param Target $x */ function f($x): void {}\n\
        f(new Mystery());";
    assert_eq!(param_count(src), 0, "incomplete hierarchy → Unknown → silent");
}

#[test]
fn object_vs_unresolved_name_stays_silent() {
    // The target name is undefined (could be a @template / type-alias the object
    // satisfies) — even though the object's own hierarchy is closed, gate on known.
    let src = "<?php final class Bar {}\n\
        /** @param Foo $a */ function f($a): void {}\n\
        f(new Bar());";
    assert_eq!(param_count(src), 0, "unresolved target → no manufactured violation");
}

// ==========================================================================
// 2. Proven scalar vs class-typed @param — pure membership, no coercion.
// ==========================================================================

#[test]
fn scalar_vs_known_class_is_no() {
    let f = "<?php class Foo {}\n/** @param Foo $x */ function f($x): void {}\n";
    assert_eq!(param_count(&format!("{f}f(5);")), 1, "int is never a Foo");
    assert_eq!(param_count(&format!("{f}f(\"x\");")), 1, "string is never a Foo");
    assert_eq!(param_count(&format!("{f}f(true);")), 1, "bool is never a Foo");
}

#[test]
fn scalar_vs_unknown_class_stays_silent() {
    // `Foo` is undefined — it may be a @template param or @phpstan-type alias that
    // denotes a scalar, so a scalar-vs-`Foo` verdict must stay silent (FP-safe).
    let f = "<?php /** @param Foo $x */ function f($x): void {}\n";
    assert_eq!(param_count(&format!("{f}f(5);")), 0, "unknown class → silent");
}

#[test]
fn scalar_vs_class_or_null() {
    // `Foo|null`: a scalar is neither, null is accepted.
    let f = "<?php final class Foo {}\n/** @param Foo|null $x */ function f($x): void {}\n";
    assert_eq!(param_count(&format!("{f}f(5);")), 1, "int is neither Foo nor null");
    assert_eq!(param_count(&format!("{f}f(null);")), 0, "null accepted by Foo|null");
}

// ==========================================================================
// 3. Abstract scalar fact vs class contract — the contract_touches_class valve.
// ==========================================================================

#[test]
fn abstract_scalar_fact_opens_pure_class_valve() {
    // A native-`string` param carries an abstract fact (not a proven value); passed
    // to a pure known-class contract it is a definite mismatch (the valve opens).
    let src = "<?php class Foo {}\n\
        /** @param Foo $x */ function f($x): void {}\n\
        function g(string $s): void { f($s); }";
    assert_eq!(param_count(src), 1, "string fact vs Foo → No (valve open)");
}

#[test]
fn abstract_scalar_fact_vs_template_stays_closed() {
    // `@template T` lowers to a class node, but T is not a known class — the valve
    // stays shut, so an int fact against T is NOT reported (the critical FP guard).
    let src = "<?php /** @template T @param T $x */ function f($x): void {}\n\
        function g(int $i): void { f($i); }";
    assert_eq!(param_count(src), 0, "template T → valve closed → silent");
}

// ==========================================================================
// 4. Enum cases (objects) and ::class strings (ADR-0043 §4).
// ==========================================================================

#[test]
fn enum_case_accepted_by_own_enum() {
    let src = "<?php enum Suit { case Hearts; case Spades; }\n\
        /** @param Suit $s */ function f($s): void {}\n\
        f(Suit::Hearts);";
    assert_eq!(param_count(src), 0, "Suit::Hearts is-a Suit (Yes)");
}

#[test]
fn enum_case_accepted_by_unitenum_and_backedenum() {
    // A pure enum is-a UnitEnum; a backed enum additionally is-a BackedEnum.
    let unit = "<?php enum Suit { case Hearts; }\n\
        /** @param UnitEnum $x */ function f($x): void {}\n\
        f(Suit::Hearts);";
    assert_eq!(param_count(unit), 0, "pure enum is-a UnitEnum");
    let backed = "<?php enum Suit: string { case Hearts = 'h'; }\n\
        /** @param BackedEnum $x */ function f($x): void {}\n\
        f(Suit::Hearts);";
    assert_eq!(param_count(backed), 0, "backed enum is-a BackedEnum");
}

#[test]
fn enum_case_rejected_by_unrelated_class() {
    let src = "<?php enum Suit { case Hearts; } final class Other {}\n\
        /** @param Other $x */ function f($x): void {}\n\
        f(Suit::Hearts);";
    assert_eq!(param_count(src), 1, "Suit case is-a-No Other (closed hierarchy)");
}

#[test]
fn class_string_literal_vs_class_string_stays_maybe() {
    // `class-string` lowers to StrOpaque (non-extensional, ADR-0038): a proven
    // `::class` string must NOT be forced to decide membership — it stays silent.
    let src = "<?php class Foo {}\n\
        /** @param class-string $c */ function f($c): void {}\n\
        f(Foo::class);";
    assert_eq!(param_count(src), 0, "Foo::class vs class-string → Maybe (locked)");
}

#[test]
fn class_string_literal_vs_real_class_is_no() {
    // A `::class` value is a *string*; against an actual class-typed contract it is
    // a scalar, hence a definite non-member.
    let src = "<?php class Foo {} class Bar {}\n\
        /** @param Bar $x */ function f($x): void {}\n\
        f(Foo::class);";
    assert_eq!(param_count(src), 1, "the string \"Foo\" is never a Bar object");
}

// ==========================================================================
// 5. @return class contracts.
// ==========================================================================

#[test]
fn return_object_vs_class_no() {
    // No native return type → the phpdoc @return path owns the check.
    let src = "<?php final class Foo {} final class Bar {}\n\
        /** @return Foo */ function f() { return new Bar(); }";
    assert_eq!(return_count(src), 1, "returning Bar violates @return Foo");
}

#[test]
fn return_scalar_vs_class_no() {
    let src = "<?php final class Foo {}\n/** @return Foo */ function f() { return 5; }";
    assert_eq!(return_count(src), 1, "returning 5 violates @return Foo");
}

#[test]
fn return_object_subclass_accepts() {
    let src = "<?php class Animal {} class Dog extends Animal {}\n\
        /** @return Animal */ function f() { return new Dog(); }";
    assert_eq!(return_count(src), 0, "Dog is-a Animal → silent");
}

#[test]
fn return_template_stays_silent() {
    let src = "<?php /** @template T @return T */ function f() { return 5; }";
    assert_eq!(return_count(src), 0, "template @return T → no FP");
}

// ==========================================================================
// 6. Descent guard-blindness — a class-touching verdict is suppressed inside a
//    binding descent (mirror of the native object_world_guard_blind).
// ==========================================================================

#[test]
fn direct_class_verdict_fires_but_descent_is_blind() {
    // Directly: a scalar into a known class-typed @param is a definite No.
    let direct = "<?php final class S1 {}\n\
        /** @param S1 $x */ function inner($x): void {}\n\
        inner(5);";
    assert_eq!(param_count(direct), 1, "direct scalar-vs-class fires");

    // Through a descent: `outer(5)` rebinds $y=5 and re-checks `inner($y)` with the
    // hypothetical value. The callee's in-body guards are unmodeled, so a class-
    // touching verdict on the rebound value is guard-blind → suppressed.
    let descent = "<?php final class S1 {}\n\
        /** @param S1 $x */ function inner($x): void {}\n\
        function outer($y): void { inner($y); }\n\
        outer(5);";
    assert_eq!(param_count(descent), 0, "descent-bound class verdict is guard-blind");
}

// ==========================================================================
// 5b. Const-fetch phpdoc types (`self::CONST`, `Enum::Case` as a type) are
//     unresolved — they must stay silent, never manufacture a No against the very
//     value they name (regression: pxxxx `@return self::CONST { return self::CONST; }`
//     and enum-case returns against enum-case-typed unions).
// ==========================================================================

#[test]
fn return_of_named_class_const_against_its_own_const_type_is_silent() {
    // The array constant is returned against `@return self::C` — tautologically
    // correct; the const-fetch type is unresolved, so no finding.
    let src = "<?php class K {\n\
        const C = [1, 2, 3];\n\
        /** @return self::C */ public static function f(): array { return self::C; }\n\
        }";
    assert_eq!(return_count(src), 0, "returning the very const named by the type → silent");
}

#[test]
fn enum_case_return_against_enum_case_typed_union_is_silent() {
    let src = "<?php enum E { case A; case B; }\n\
        class K {\n\
        /** @return E::A|E::B|null */ public function g(): E|null { return E::A; }\n\
        }";
    assert_eq!(return_count(src), 0, "enum case vs enum-case-typed union → silent (unresolved const type)");
}

// ==========================================================================
// 6b. Implicit `Stringable` — a class with `__toString` (but no explicit
//     `implements \Stringable`) IS a Stringable in PHP 8+; the is-a oracle must
//     not manufacture a `No` against it (regression: symfony ChoiceQuestionTest).
// ==========================================================================

#[test]
fn class_with_to_string_is_implicitly_stringable() {
    let src = "<?php class SC { public function __toString(): string { return 'x'; } }\n\
        /** @param \\Stringable $x */ function f($x): void {}\n\
        f(new SC());";
    assert_eq!(param_count(src), 0, "__toString ⇒ implicit Stringable ⇒ accepted");
}

#[test]
fn class_without_to_string_rejects_stringable() {
    let src = "<?php final class NS {}\n\
        /** @param \\Stringable $x */ function f($x): void {}\n\
        f(new NS());";
    assert_eq!(param_count(src), 1, "no __toString, closed hierarchy ⇒ is-a-No Stringable");
}

#[test]
fn trait_using_class_vs_stringable_is_unknown() {
    // A trait may supply `__toString`; the merged methods are unmodeled, so the
    // verdict must be Unknown (silent), never an unsound No.
    let src = "<?php trait T {} class TU { use T; }\n\
        /** @param \\Stringable $x */ function f($x): void {}\n\
        f(new TU());";
    assert_eq!(param_count(src), 0, "trait-using class vs Stringable → Unknown → silent");
}

#[test]
fn stringable_in_array_union_accepts_to_string_object() {
    // Mirror of symfony's `array<string|bool|int|float|\Stringable>` — a __toString
    // object element is accepted; a null element is not.
    let ok = "<?php class SC { public function __toString(): string { return 'x'; } }\n\
        /** @param array<string|bool|int|float|\\Stringable> $a */ function f($a): void {}\n\
        f(['a', new SC()]);";
    assert_eq!(param_count(ok), 0, "__toString object is a valid union element");
    let bad = "<?php /** @param array<string|bool|int|float|\\Stringable> $a */ function f($a): void {}\n\
        f(['a', null]);";
    assert_eq!(param_count(bad), 1, "null is not a member of the union");
}

// ==========================================================================
// 7. Liskov interplay — an overridden method carrying a class @param must not
//    double-fire between the override's and the parent's envelopes (ADR-0033).
// ==========================================================================

#[test]
fn overridden_method_class_param_reports_once() {
    let src = "<?php class Animal {} class Robot {}\n\
        class Base { /** @param Animal $a */ public function m($a): void {} }\n\
        class Sub extends Base { /** @param Animal $a */ public function m($a): void {} }\n\
        $s = new Sub(); $s->m(new Robot());";
    assert_eq!(param_count(src), 1, "exactly one finding — no envelope double-fire");
}
