//! ADR-0043 stage 3 — native object acceptance (definite-No opening).
//!
//! The acceptance matrix for object values against native (scalar/union/object)
//! parameter and return types, riding the trinary is-a oracle. Only a **definite
//! No** (every union member provenly rejects the value's exact class) fires; any
//! `Unknown` (incomplete hierarchy, unresolvable class) stays silent.
//!
//! The object↔scalar coercion cells were verified against PHP 8.5.8 (`php -r`):
//! - a `__toString` object *coerces* to a `string` parameter in **coercive** mode
//!   (no error) but `TypeError`s in **strict** mode; a plain object errors in both.
//! - no object (even `__toString`) ever coerces to `int`/`float`/`bool`.
//! - an enum case is an **object**, never its backing scalar.

use steins_infer::{Diagnostic, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

fn ids(src: &str) -> Vec<String> {
    findings(src).into_iter().map(|d| d.id.to_owned()).collect()
}

// ==========================================================================
// 1. Object value vs object (Instance) member — is-a Yes / No / Unknown.
// ==========================================================================

#[test]
fn object_vs_instance_definite_no() {
    // unions_object_member_rejection: Robot is-a-No against User|Guest (all final).
    let src = "<?php declare(strict_types=1);
final class User {}
final class Guest {}
final class Robot {}
function f(User|Guest $v): void {}
f(new Robot());";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "Robot rejected by User|Guest");
}

#[test]
fn object_vs_instance_accepts_yes() {
    let src = "<?php declare(strict_types=1);
final class User {}
final class Guest {}
function f(User|Guest $v): void {}
f(new User());";
    assert_eq!(n(src), 0, "User accepted by User|Guest");
}

#[test]
fn object_vs_interface_no_and_yes() {
    // objects_interface_compat: AnonymousUser does not implement HasName.
    let src = "<?php declare(strict_types=1);
interface HasName {}
final class User implements HasName {}
final class AnonymousUser {}
function f(HasName $v): void {}
f(new User());
f(new AnonymousUser());";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "only AnonymousUser rejected");
}

#[test]
fn object_vs_instance_unknown_stays_silent() {
    // The class extends an uncatalogued external → hierarchy incomplete → Unknown.
    let src = "<?php declare(strict_types=1);
interface Target {}
class Mystery extends \\Some\\Vendor\\External {}
function f(Target $v): void {}
f(new Mystery());";
    assert_eq!(n(src), 0, "incomplete hierarchy → Unknown → silent");
}

#[test]
fn object_vs_instance_subclass_accepts() {
    let src = "<?php declare(strict_types=1);
class Animal {}
class Dog extends Animal {}
function f(Animal $v): void {}
f(new Dog());";
    assert_eq!(n(src), 0, "Dog is-a Animal (Yes) → silent");
}

// ==========================================================================
// 2. Object value vs scalar member (object → scalar rejection).
// ==========================================================================

#[test]
fn object_vs_int_rejected_both_modes() {
    // No object coerces to int (verified php 8.5.8) — fires in coercive too.
    let strict = "<?php declare(strict_types=1);
final class Box {}
function f(int $v): void {}
f(new Box());";
    assert_eq!(n(strict), 1, "object vs int strict → flagged");
    let coercive = "<?php
final class Box {}
function f(int $v): void {}
f(new Box());";
    assert_eq!(n(coercive), 1, "object vs int coercive → flagged");
}

#[test]
fn object_vs_string_strict_rejected_coercive_silent() {
    // Verified: object → string param errors in strict, may coerce (__toString) in
    // coercive → coercive stays silent (FP-safe; __toString absence not proven).
    let strict = "<?php declare(strict_types=1);
final class Box {}
function f(string $v): void {}
f(new Box());";
    assert_eq!(n(strict), 1, "object vs string strict → flagged");
    let coercive = "<?php
final class Box {}
function f(string $v): void {}
f(new Box());";
    assert_eq!(n(coercive), 0, "object vs string coercive → silent (may __toString)");
}

// ==========================================================================
// 3. Scalar value vs object (Instance) member — the reverse rejection.
// ==========================================================================

#[test]
fn scalar_vs_enum_instance_rejected() {
    // enums_backed_cases: a raw string where the enum type is required.
    let src = "<?php declare(strict_types=1);
enum Status: string { case Active = 'active'; }
function f(Status $s): void {}
f('active');";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "raw string rejected by enum");
}

#[test]
fn scalar_vs_instance_union_rests_on_scalar_member() {
    // int|Foo: 5 satisfies int → silent; "abc" (strict) satisfies neither → flagged.
    let base = "<?php declare(strict_types=1);
final class Foo {}
function f(int|Foo $v): void {}
";
    assert_eq!(n(&format!("{base}f(5);")), 0, "int member accepts 5");
    assert_eq!(n(&format!("{base}f(\"abc\");")), 1, "no member accepts abc (strict)");
}

// ==========================================================================
// 4. Enum cases are objects, not their backing scalar.
// ==========================================================================

#[test]
fn enum_case_is_object_not_backing_scalar() {
    // enums_case_object_vs_backing_scalar: Status::Active (object) vs string param.
    let src = "<?php declare(strict_types=1);
enum Status: string { case Active = 'active'; }
function f(string $v): void {}
f(Status::Active);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "enum case object ≠ string");
}

#[test]
fn enum_case_accepted_by_its_enum_type() {
    let src = "<?php declare(strict_types=1);
enum Status: string { case Active = 'active'; case Off = 'off'; }
function f(Status $s): void {}
f(Status::Active);";
    assert_eq!(n(src), 0, "enum case accepted by its own enum type");
}

#[test]
fn unit_enum_case_vs_int_rejected() {
    let src = "<?php declare(strict_types=1);
enum Dir { case N; case S; }
function f(int $v): void {}
f(Dir::N);";
    assert_eq!(n(src), 1, "unit enum case (object) vs int → flagged");
}

// ==========================================================================
// 5. Class constants — literal resolution and hierarchy walk.
// ==========================================================================

#[test]
fn class_const_int_vs_string_rejected() {
    // constants_class_constant_type: HttpStatus::OK is int 200, param is string.
    let src = "<?php declare(strict_types=1);
final class HttpStatus { public const int OK = 200; }
function f(string $v): void {}
f(HttpStatus::OK);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "int const vs string param");
}

#[test]
fn class_const_matching_member_silent() {
    let src = "<?php declare(strict_types=1);
final class C { public const int LIMIT = 10; }
function f(int $v): void {}
f(C::LIMIT);";
    assert_eq!(n(src), 0, "int const accepted by int param");
}

#[test]
fn class_const_resolves_through_parent_chain() {
    let src = "<?php declare(strict_types=1);
class Base { public const int CODE = 7; }
class Derived extends Base {}
function f(string $v): void {}
f(Derived::CODE);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "inherited const int vs string");
}

#[test]
fn interface_const_resolves() {
    let src = "<?php declare(strict_types=1);
interface HasCode { public const int CODE = 7; }
final class Impl implements HasCode {}
function f(string $v): void {}
f(Impl::CODE);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "interface const int vs string");
}

#[test]
fn non_literal_const_stays_unproven() {
    // A const with a non-literal initializer is not lowered → unproven → silent.
    let src = "<?php declare(strict_types=1);
final class C { public const FOO = PHP_INT_MAX; }
function f(string $v): void {}
f(C::FOO);";
    assert_eq!(n(src), 0, "non-literal const → unproven → silent");
}

#[test]
fn class_class_constant_is_a_string() {
    // Foo::class is the FQN string — accepted by a string param (silent), rejected
    // by an int param (a class-string is never numeric).
    let ok = "<?php declare(strict_types=1);
final class Foo {}
function f(string $v): void {}
f(Foo::class);";
    assert_eq!(n(ok), 0, "::class string accepted by string param");
    let bad = "<?php declare(strict_types=1);
final class Foo {}
function f(int $v): void {}
f(Foo::class);";
    assert_eq!(ids(bad), vec!["type.argument-mismatch"], "::class string vs int param");
}

// ==========================================================================
// 6. Nullable interplay (preserve existing logic; null-vs-object stays silent).
// ==========================================================================

#[test]
fn nullable_object_param_accepts_matching_and_rejects_foreign() {
    let src = "<?php declare(strict_types=1);
final class A {}
final class B {}
function f(?A $v): void {}
f(new A());
f(new B());";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "?A accepts A, rejects B");
}

#[test]
fn null_vs_object_type_silent() {
    // null against a non-nullable object type stays silent this stage (out of scope
    // + sidesteps has_null_default implicit-nullable interplay).
    let src = "<?php declare(strict_types=1);
final class A {}
function f(A $v): void {}
f(null);";
    assert_eq!(n(src), 0, "null-vs-object stays silent");
}

// ==========================================================================
// 7. Return path.
// ==========================================================================

#[test]
fn return_object_definite_no() {
    let src = "<?php declare(strict_types=1);
final class User {}
final class Robot {}
function make(): User { return new Robot(); }";
    assert_eq!(ids(src), vec!["type.return-mismatch"], "returning Robot as User");
}

#[test]
fn return_object_accepts() {
    let src = "<?php declare(strict_types=1);
class Animal {}
class Dog extends Animal {}
function make(): Animal { return new Dog(); }";
    assert_eq!(n(src), 0, "returning Dog as Animal is fine");
}

#[test]
fn return_enum_case_vs_string() {
    let src = "<?php declare(strict_types=1);
enum Status: string { case Active = 'active'; }
function make(): string { return Status::Active; }";
    assert_eq!(ids(src), vec!["type.return-mismatch"], "enum case object returned as string");
}

// ==========================================================================
// 8. Variable bound to a proven object (ADR-0036 heap).
// ==========================================================================

#[test]
fn var_bound_object_rejected() {
    let src = "<?php declare(strict_types=1);
final class User {}
final class Robot {}
function f(User $u): void {}
$r = new Robot();
f($r);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "$r holds Robot, rejected by User");
}

// ==========================================================================
// 9. Negative — Unknown / unresolved stays silent everywhere.
// ==========================================================================

#[test]
fn unresolved_class_new_silent() {
    let src = "<?php declare(strict_types=1);
function f(\\Vendor\\Iface $v): void {}
f(new \\Vendor\\Unknown());";
    assert_eq!(n(src), 0, "unknown classes on both sides → Unknown → silent");
}

#[test]
fn trait_use_adds_no_type_so_hierarchy_stays_closed() {
    // A `use`d trait adds methods, never *types* (a trait cannot `implements`), so
    // the is-a oracle keeps the hierarchy fully enumerated and still proves `No`.
    let src = "<?php declare(strict_types=1);
interface I {}
trait T {}
class Uses { use T; }
function f(I $v): void {}
f(new Uses());";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "trait adds no type → No still fires");
}

#[test]
fn genuinely_unknown_parent_stays_silent() {
    // An unresolved *external* parent taints the closure → Unknown → silent.
    let src = "<?php declare(strict_types=1);
interface I {}
class Uses extends \\Some\\External {}
function f(I $v): void {}
f(new Uses());";
    assert_eq!(n(src), 0, "external parent → incomplete hierarchy → silent");
}

// ==========================================================================
// 10. Diagnostic rendering — object types show source-declared casing.
// ==========================================================================

#[test]
fn message_renders_object_union_with_declared_casing() {
    // The rendered type is the source-cased FQN (`User|Guest`), not the
    // lowercase-normalized matching key (`user|guest`).
    let src = "<?php declare(strict_types=1);
final class User {}
final class Guest {}
final class Robot {}
function f(User|Guest $v): void {}
f(new Robot());";
    let ds = findings(src);
    assert_eq!(ds.len(), 1);
    assert!(
        ds[0].message.contains("cannot become User|Guest $v"),
        "source-cased union in message: {}",
        ds[0].message
    );
}

#[test]
fn message_renders_namespaced_object_with_declared_casing() {
    // An unqualified name in a namespace renders as the resolved, source-cased
    // FQN (`App\Logger`), matching how the FQN was declared/written.
    let src = "<?php declare(strict_types=1);
namespace App;
final class Logger {}
final class Robot {}
function f(Logger $l): void {}
f(new Robot());";
    let ds = findings(src);
    assert_eq!(ds.len(), 1);
    assert!(
        ds[0].message.contains("cannot become App\\Logger $l"),
        "source-cased namespaced FQN in message: {}",
        ds[0].message
    );
}

#[test]
fn message_casing_does_not_change_matching() {
    // Casing is display-only: a hint written in a different case than the
    // declaration still matches (resolution stays case-insensitive) and the
    // message shows the casing as written at the hint site.
    let src = "<?php declare(strict_types=1);
final class LogicBox {}
final class Robot {}
function f(LOGICBOX $v): void {}
f(new LogicBox());
f(new Robot());";
    let ds = findings(src);
    assert_eq!(ds.len(), 1, "LogicBox accepted despite hint casing; only Robot rejected");
    assert!(
        ds[0].message.contains("cannot become LOGICBOX $v"),
        "hint-site casing preserved in message: {}",
        ds[0].message
    );
}

#[test]
fn return_message_renders_object_with_declared_casing() {
    let src = "<?php declare(strict_types=1);
final class User {}
final class Robot {}
function make(): User { return new Robot(); }";
    let ds = findings(src);
    assert_eq!(ds.len(), 1);
    assert!(
        ds[0].message.contains("cannot become User (return type of make())"),
        "source-cased return type in message: {}",
        ds[0].message
    );
}

#[test]
fn message_renders_enum_type_with_declared_casing() {
    let src = "<?php declare(strict_types=1);
enum Status: string { case Active = 'active'; }
function f(Status $s): void {}
f('active');";
    let ds = findings(src);
    assert_eq!(ds.len(), 1);
    assert!(
        ds[0].message.contains("cannot become Status $s"),
        "source-cased enum type in message: {}",
        ds[0].message
    );
}

// ==========================================================================
// 11. Native intersection types (`A&B&…`, ADR-0043 conjunctive member).
//     An object satisfies the intersection only when it is-a EVERY conjunct;
//     it is a definite No the moment the oracle proves `IsA::No` against ANY
//     one conjunct. Any conjunct that stays Unknown (with no proven No) keeps
//     the whole intersection silent.
// ==========================================================================

#[test]
fn intersection_missing_one_conjunct_rejected() {
    // intersections_interface_merge (conformance shape): AnonymousUser is-a HasId
    // (Yes) but not HasName (No, closed hierarchy) → the intersection rejects it.
    let src = "<?php declare(strict_types=1);
interface HasId {}
interface HasName {}
final class User implements HasId, HasName {}
final class AnonymousUser implements HasId {}
function f(HasId&HasName $v): void {}
f(new User());
f(new AnonymousUser());";
    assert_eq!(
        ids(src),
        vec!["type.argument-mismatch"],
        "only AnonymousUser (missing HasName) rejected; User satisfies both"
    );
}

#[test]
fn intersection_all_conjuncts_satisfied_silent() {
    let src = "<?php declare(strict_types=1);
interface HasId {}
interface HasName {}
final class User implements HasId, HasName {}
function f(HasId&HasName $v): void {}
f(new User());";
    assert_eq!(n(src), 0, "User is-a both HasId and HasName → accepted");
}

#[test]
fn intersection_unknown_conjunct_stays_silent() {
    // The object's own hierarchy is open (it extends an uncatalogued external),
    // so its is-a against the second conjunct (`HasName`) is Unknown — the
    // external base might supply it — while it provenly is-a the first (`HasId`).
    // No conjunct yields a proven `No`, so the whole intersection stays silent.
    let src = "<?php declare(strict_types=1);
interface HasId {}
interface HasName {}
class Thing extends \\Vendor\\Base implements HasId {}
function f(HasId&HasName $v): void {}
f(new Thing());";
    assert_eq!(n(src), 0, "open hierarchy → no proven No → silent");
}

#[test]
fn intersection_scalar_value_rejected() {
    // No scalar satisfies an interface intersection (strict mode).
    let src = "<?php declare(strict_types=1);
interface HasId {}
interface HasName {}
function f(HasId&HasName $v): void {}
f('nope');";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "scalar never satisfies an intersection");
}

#[test]
fn intersection_dnf_union_of_intersection_and_class() {
    // `(A&B)|C`: a C is accepted via the C arm; an object that is neither the
    // A&B intersection nor C is rejected by every union member.
    let src = "<?php declare(strict_types=1);
interface A {}
interface B {}
final class C {}
final class Both implements A, B {}
final class Neither {}
function f((A&B)|C $v): void {}
";
    assert_eq!(n(&format!("{src}f(new C());")), 0, "C satisfies the C arm");
    assert_eq!(n(&format!("{src}f(new Both());")), 0, "Both satisfies the A&B arm");
    assert_eq!(
        ids(&format!("{src}f(new Neither());")),
        vec!["type.argument-mismatch"],
        "Neither is rejected by both the A&B arm and the C arm"
    );
}

#[test]
fn intersection_return_definite_no() {
    let src = "<?php declare(strict_types=1);
interface HasId {}
interface HasName {}
final class AnonymousUser implements HasId {}
function make(): HasId&HasName { return new AnonymousUser(); }";
    assert_eq!(
        ids(src),
        vec!["type.return-mismatch"],
        "returning AnonymousUser (no HasName) violates the intersection return type"
    );
}

#[test]
fn intersection_message_renders_with_ampersand_and_casing() {
    let src = "<?php declare(strict_types=1);
interface HasId {}
interface HasName {}
final class AnonymousUser implements HasId {}
function f(HasId&HasName $v): void {}
f(new AnonymousUser());";
    let ds = findings(src);
    assert_eq!(ds.len(), 1);
    assert!(
        ds[0].message.contains("cannot become HasId&HasName $v"),
        "intersection rendered with `&` and source casing: {}",
        ds[0].message
    );
}
