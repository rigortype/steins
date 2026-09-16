//! Class-typed phpdoc contracts (`@param`/`@return`) against proven object values,
//! enum cases, `::class` strings, and scalar facts, via the trinary is-a oracle
//! (ADR-0043; see `tests/object_acceptance.rs`).
//!
//! The phpdoc relation is pure set membership with no coercion (ADR-0030): a proven
//! scalar is never a class-type member, in either mode. A definite `No` reports;
//! `Unknown` (incomplete hierarchy) or an unresolved `@template`/`@phpstan-type` stays silent.

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

// 1. Proven object value vs class-typed @param — is-a Yes / No / Unknown.

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
    // Target undefined (could be @template/alias) — gate on known even though closed.
    let src = "<?php final class Bar {}\n\
        /** @param Foo $a */ function f($a): void {}\n\
        f(new Bar());";
    assert_eq!(param_count(src), 0, "unresolved target → no manufactured violation");
}

// 2. Proven scalar vs class-typed @param — pure membership, no coercion.

#[test]
fn scalar_vs_known_class_is_no() {
    let f = "<?php class Foo {}\n/** @param Foo $x */ function f($x): void {}\n";
    assert_eq!(param_count(&format!("{f}f(5);")), 1, "int is never a Foo");
    assert_eq!(param_count(&format!("{f}f(\"x\");")), 1, "string is never a Foo");
    assert_eq!(param_count(&format!("{f}f(true);")), 1, "bool is never a Foo");
}

#[test]
fn scalar_vs_unknown_class_stays_silent() {
    // `Foo` undefined — may be @template/@phpstan-type denoting a scalar, so silent (FP-safe).
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

// 3. Abstract scalar fact vs class contract — the contract_touches_class valve.

#[test]
fn abstract_scalar_fact_opens_pure_class_valve() {
    // Abstract fact (not a proven value) vs a class contract: definite mismatch, valve opens.
    let src = "<?php class Foo {}\n\
        /** @param Foo $x */ function f($x): void {}\n\
        function g(string $s): void { f($s); }";
    assert_eq!(param_count(src), 1, "string fact vs Foo → No (valve open)");
}

#[test]
fn abstract_scalar_fact_vs_template_stays_closed() {
    // `@template T` lowers to a class node but isn't known — valve stays shut (FP guard).
    let src = "<?php /** @template T @param T $x */ function f($x): void {}\n\
        function g(int $i): void { f($i); }";
    assert_eq!(param_count(src), 0, "template T → valve closed → silent");
}

// 4. Enum cases (objects) and ::class strings (ADR-0043 §4).

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
    // `class-string` is a refinement (issue #236) but CONTEXTUAL: whether `'Foo'`
    // names a class is the class table's answer, not the characters' — `::class`
    // stays silent, exactly as under `StrOpaque`.
    let src = "<?php class Foo {}\n\
        /** @param class-string $c */ function f($c): void {}\n\
        f(Foo::class);";
    assert_eq!(param_count(src), 0, "Foo::class vs class-string → Maybe (locked)");
}

#[test]
fn class_string_refutes_the_strings_no_class_like_can_be() {
    // Extensional half of the refinement (issue #236) IS decidable — identifier
    // grammar: a class-like is never `''` or `'0'`; both were `Maybe` before.
    let base = "<?php /** @param class-string $c */ function f($c): void {}\n";
    assert_eq!(param_count(&format!("{base}f('');")), 1, "'' names no class-like");
    assert_eq!(param_count(&format!("{base}f('0');")), 1, "'0' names no class-like");
    assert_eq!(param_count(&format!("{base}f('123');")), 1, "a decimal int is no identifier");
    // …and an identifier-shaped string still stays silent: not refuted, not proven.
    assert_eq!(param_count(&format!("{base}f('App\\\\User');")), 0);
}

#[test]
fn class_string_satisfies_the_refinements_it_entails() {
    // `class-string ⇒ non-falsy-string ⇒ non-empty-string` (identifier grammar as
    // implication): accepted by weaker string contracts, no word from the class table.
    for weaker in ["non-empty-string", "non-falsy-string", "string"] {
        let src = format!(
            "<?php /** @param class-string $c */ function g($c): void {{ h($c); }}\n\
             /** @param {weaker} $s */ function h($s): void {{}}\n"
        );
        assert_eq!(param_count(&src), 0, "class-string is a {weaker}");
    }
}

#[test]
fn relative_class_const_is_a_class_string() {
    // `self`/`parent`/`static::class` resolve to a class-like the index knows but
    // can't spell (ADR-0043 casing deferral) — the claim the refinement carries
    // (issue #236): silent vs `@param class-string`, not where a name can't go.
    let src = "<?php class Base {} class Child extends Base {\n\
        /** @param class-string $c */ function f($c): void {}\n\
        function go(): void { $this->f(self::class); $this->f(parent::class); $this->f(static::class); }\n\
        }";
    assert_eq!(param_count(src), 0, "every relative ::class is a class-string");
}

#[test]
fn class_string_literal_vs_real_class_is_no() {
    // A `::class` value is a *string*; vs a class-typed contract it's a scalar non-member.
    let src = "<?php class Foo {} class Bar {}\n\
        /** @param Bar $x */ function f($x): void {}\n\
        f(Foo::class);";
    assert_eq!(param_count(src), 1, "the string \"Foo\" is never a Bar object");
}

// 5. @return class contracts.

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

// 6. Descent guard-blindness: suppressed in descent (mirror: object_world_guard_blind).

#[test]
fn direct_class_verdict_fires_but_descent_is_blind() {
    // Directly: a scalar into a known class-typed @param is a definite No.
    let direct = "<?php final class S1 {}\n\
        /** @param S1 $x */ function inner($x): void {}\n\
        inner(5);";
    assert_eq!(param_count(direct), 1, "direct scalar-vs-class fires");

    // Through a descent: `outer(5)` rebinds $y=5, re-checking `inner($y)`; callee
    // guards are unmodeled, so a rebound class-touching verdict is guard-blind → suppressed.
    let descent = "<?php final class S1 {}\n\
        /** @param S1 $x */ function inner($x): void {}\n\
        function outer($y): void { inner($y); }\n\
        outer(5);";
    assert_eq!(param_count(descent), 0, "descent-bound class verdict is guard-blind");
}

// 5b. Const-fetch phpdoc types (`self::CONST`, `Enum::Case`) are unresolved — must
//     stay silent, never manufacture a No against the value they name (regression:
//     pxxxx `@return self::CONST`, and enum-case returns vs enum-case-typed unions).

#[test]
fn return_of_named_class_const_against_its_own_const_type_is_silent() {
    // Tautologically correct vs `@return self::C`; const-fetch type unresolved, no finding.
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

// 6b. Implicit `Stringable`: a class with `__toString` (no `implements \Stringable`)
//     IS Stringable in PHP 8+; oracle mustn't say No (regression: symfony ChoiceQuestionTest).

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
    // A trait may supply `__toString`; merged methods unmodeled, verdict is Unknown, not No.
    let src = "<?php trait T {} class TU { use T; }\n\
        /** @param \\Stringable $x */ function f($x): void {}\n\
        f(new TU());";
    assert_eq!(param_count(src), 0, "trait-using class vs Stringable → Unknown → silent");
}

#[test]
fn stringable_in_array_union_accepts_to_string_object() {
    // Mirror of symfony's array<string|bool|int|float|\Stringable>: __toString ok, null not.
    let ok = "<?php class SC { public function __toString(): string { return 'x'; } }\n\
        /** @param array<string|bool|int|float|\\Stringable> $a */ function f($a): void {}\n\
        f(['a', new SC()]);";
    assert_eq!(param_count(ok), 0, "__toString object is a valid union element");
    let bad = "<?php /** @param array<string|bool|int|float|\\Stringable> $a */ function f($a): void {}\n\
        f(['a', null]);";
    assert_eq!(param_count(bad), 1, "null is not a member of the union");
}

// 7. Liskov interplay: override + parent class @param must not double-fire (ADR-0033).

#[test]
fn overridden_method_class_param_reports_once() {
    let src = "<?php class Animal {} class Robot {}\n\
        class Base { /** @param Animal $a */ public function m($a): void {} }\n\
        class Sub extends Base { /** @param Animal $a */ public function m($a): void {} }\n\
        $s = new Sub(); $s->m(new Robot());";
    assert_eq!(param_count(src), 1, "exactly one finding — no envelope double-fire");
}

// 8. @template name shadowing a real class (issue #5): a `@template X` in scope
//    makes X opaque, never the class, inside that declaration's docblock types, so a
//    same-named real class no longer manufactures a param/return-mismatch FP. Shadow
//    is per-declaration (own + enclosing class-like docblock); qualified refs opt out.

#[test]
fn template_shadows_real_class_param_proven() {
    // Issue's exact repro: real class + same-name `@template`/`@param`, called non-member.
    let src = "<?php class Foo {}\n\
        /** @template Foo\n * @param Foo $x */ function f($x): void {}\n\
        f(5);";
    assert_eq!(param_count(src), 0, "@template Foo shadows class Foo → f(5) silent");
    // Control: without the `@template`, the same call is a genuine violation.
    let control = "<?php class Foo {}\n\
        /** @param Foo $x */ function f($x): void {}\n\
        f(5);";
    assert_eq!(param_count(control), 1, "no template → class contract fires");
}

#[test]
fn template_shadows_real_class_param_abstract_fact() {
    // Abstract-fact arm (int param → int fact): shadow must keep the valve shut too.
    let src = "<?php class Model {}\n\
        /** @template Model\n * @param Model $x */ function f($x): void {}\n\
        function g(int $i): void { f($i); }";
    assert_eq!(param_count(src), 0, "int fact vs shadowed Model → valve stays shut");
}

#[test]
fn class_level_template_shadows_method_param() {
    // Class-level `@template Model` shadows every member docblock, even a bare method.
    let src = "<?php class Model {}\n\
        /** @template Model */\n\
        class Repo { /** @param Model $m */ public function set($m): void {} }\n\
        $r = new Repo(); $r->set(5);";
    assert_eq!(param_count(src), 0, "class-level @template Model shadows the method @param");
    // Control: drop the class-level template and the method @param binds the class.
    let control = "<?php class Model {}\n\
        class Repo { /** @param Model $m */ public function set($m): void {} }\n\
        $r = new Repo(); $r->set(5);";
    assert_eq!(param_count(control), 1, "no class template → method @param Model fires");
}

#[test]
fn qualified_reference_is_never_shadowed() {
    // A `\`-qualified ref opts out of the template namespace; `\Foo` resolves and still fires.
    let src = "<?php class Foo {}\n\
        /** @template Foo\n * @param \\Foo $x */ function f($x): void {}\n\
        f(5);";
    assert_eq!(param_count(src), 1, "\\Foo is qualified → resolves to the class → fires");
}

#[test]
fn template_shadowing_nothing_is_unchanged() {
    // Template colliding with no class: unchanged behavior (was already unresolved → silent).
    let src = "<?php /** @template TValue\n * @param TValue $x */ function f($x): void {}\n\
        f(5);";
    assert_eq!(param_count(src), 0, "template naming no class → silent (unchanged)");
}

#[test]
fn template_scope_is_per_declaration() {
    // `@template Model` on function `a` does not shadow class Model in sibling `b`.
    let src = "<?php class Model {}\n\
        /** @template Model\n * @param Model $x */ function a($x): void {}\n\
        /** @param Model $y */ function b($y): void {}\n\
        a(5); b(5);";
    assert_eq!(param_count(src), 1, "only b() fires — a's template does not leak to b");
}

#[test]
fn prefixed_template_variant_shadows() {
    // `@phpstan-template` (and the variance/psalm variants) declare a template too.
    let src = "<?php class Foo {}\n\
        /** @phpstan-template Foo\n * @param Foo $x */ function f($x): void {}\n\
        f(5);";
    assert_eq!(param_count(src), 0, "@phpstan-template Foo shadows class Foo");
}

#[test]
fn template_shadows_a_real_class_in_an_unsealed_shape_tail() {
    // Issue #374: the shadow used to walk an array shape's *items* and stop, so a
    // template named in the unsealed tail stayed a class reference and judged every
    // extra key against it. One walk now decides where the shadow goes, and the tail
    // is one of the positions it goes to.
    let src = "<?php final class T {}\n\
        /** @template T\n * @param array{a: int, ...<T>} $a */ function f($a): void {}\n\
        f(['a' => 1, 'z' => 2]);";
    assert_eq!(param_count(src), 0, "@template T shadows the tail → the extra key is silent");
    // Control: without the template the tail names the real class and still fires.
    let control = "<?php final class T {}\n\
        /** @param array{a: int, ...<T>} $a */ function f($a): void {}\n\
        f(['a' => 1, 'z' => 2]);";
    assert_eq!(param_count(control), 1, "no template → the tail's class contract fires");
}

#[test]
fn template_shadows_real_class_return() {
    // `@return` path: `@template Foo` shadows the return contract too; scalar no longer fires.
    let src = "<?php class Foo {}\n\
        /** @template Foo\n * @return Foo */ function f() { return 5; }";
    assert_eq!(return_count(src), 0, "@template Foo shadows @return Foo → silent");
    // Control: without the template, returning 5 against @return Foo is a violation.
    let control = "<?php final class Foo {}\n/** @return Foo */ function f() { return 5; }";
    assert_eq!(return_count(control), 1, "no template → @return Foo fires");
}
