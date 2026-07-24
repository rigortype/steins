//! ADR-0043 amendment — LSB return-position minimum-bound check.
//!
//! A method declared `: static` / `: self` / `: parent` (nullable variants
//! included) returning a value whose *exact* class provenly is-a-No against the
//! declaring class's bound is an unconditional runtime `TypeError`: every valid
//! late-bound class `T` satisfies `is_a(T, C) = Yes`, so `is_a(V, C) = No` implies
//! `is_a(V, T) = No` for every possible `T` (verified PHP 8.5.8, both modes). The
//! check reuses the existing `type.return-mismatch` pipeline — steins-infer needs
//! no changes; the bound is synthesized at lowering (steins-syntax).
//!
//! This file pins the firing shape and the full silence matrix (amendment §6):
//! anything conditional (`new self()` in an open class, sibling subclass),
//! anything uncertain (open hierarchy), and every non-lowered shape (union with
//! `static`, `?static`+null) stays silent.

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
// Firing: the conformance fixture shape.
// ==========================================================================

#[test]
fn static_return_unrelated_class_fires() {
    // objects_static_return_mismatch: a `: static` method returning an instance
    // of an unrelated final class fails is-a against the declaring class — an
    // unconditional TypeError on every receiver.
    let src = "<?php declare(strict_types=1);
final class Builder {}
final class BrokenBuilder {
    public function rebuild(): static { return new Builder(); }
}";
    assert_eq!(ids(src), vec!["type.return-mismatch"], "static returning unrelated Builder");
}

#[test]
fn static_return_message_names_the_bound() {
    // PHPStan message parity: the diagnostic renders the source-cased bound
    // ("should return BrokenBuilder" — the enclosing class the `static` resolves
    // to as its minimum bound).
    let src = "<?php declare(strict_types=1);
final class Builder {}
final class BrokenBuilder {
    public function rebuild(): static { return new Builder(); }
}";
    let f = findings(src);
    assert_eq!(f.len(), 1);
    assert!(
        f[0].message.contains("BrokenBuilder"),
        "message should name the resolved bound: {}",
        f[0].message
    );
}

#[test]
fn self_return_unrelated_class_fires() {
    // `: self` — bound = declaring class directly (not even late-bound).
    let src = "<?php declare(strict_types=1);
final class Widget {}
final class Gadget {
    public function make(): self { return new Widget(); }
}";
    assert_eq!(ids(src), vec!["type.return-mismatch"], "self returning unrelated Widget");
}

#[test]
fn parent_return_unrelated_class_fires() {
    // `: parent` — bound = resolved `extends` parent. Returning an instance not
    // is-a the parent fails on every receiver.
    let src = "<?php declare(strict_types=1);
class Base {}
final class Other {}
final class Child extends Base {
    public function up(): parent { return new Other(); }
}";
    assert_eq!(ids(src), vec!["type.return-mismatch"], "parent returning unrelated Other");
}

#[test]
fn static_return_scalar_fires() {
    // `return 42` from `: static`: no scalar coerces to an object — an
    // unconditional TypeError in both modes (Instance member rejects scalars).
    let src = "<?php declare(strict_types=1);
final class C {
    public function m(): static { return 42; }
}";
    assert_eq!(ids(src), vec!["type.return-mismatch"], "scalar returned from : static");
}

// ==========================================================================
// Silence matrix (amendment §6).
// ==========================================================================

#[test]
fn return_this_is_silent() {
    // `$this` is a late-bound instance — satisfies `: static` on every receiver.
    // It resolves to no exact allocation, so the exact-class arm never fires.
    let src = "<?php declare(strict_types=1);
final class C {
    public function m(): static { return $this; }
}";
    assert_eq!(n(src), 0, "return $this satisfies : static");
}

#[test]
fn new_self_in_open_class_is_silent() {
    // The point-4 refusal: `new self()` under `: static` in an OPEN class runs
    // clean on the declaring class and breaks only on proper-descendant
    // receivers (a works-but-worst-case shape). Silent by construction:
    // is_a(C, C) = Yes, and the check tests only the necessary bound.
    let src = "<?php declare(strict_types=1);
class C {
    public function m(): static { return new self(); }
}";
    assert_eq!(n(src), 0, "new self() in an open : static class is not reported");
}

#[test]
fn new_static_bound_class_is_silent() {
    // Returning `new C()` from `C::m(): static` where the returned class IS the
    // declaring class: is_a Yes, silent.
    let src = "<?php declare(strict_types=1);
class C {
    public function m(): static { return new C(); }
}";
    assert_eq!(n(src), 0, "returning the declaring class is is-a Yes");
}

#[test]
fn subclass_return_is_silent() {
    // A subclass instance may be a valid late-bound class (or `$this`-like); is-a
    // Yes, silent. (Sufficiency is never checked — necessary-bound only.)
    let src = "<?php declare(strict_types=1);
class Base {
    public function m(): static { return new Derived(); }
}
final class Derived extends Base {}";
    assert_eq!(n(src), 0, "subclass instance satisfies the : static bound");
}

#[test]
fn open_hierarchy_returned_class_is_silent() {
    // The returned class extends an uncatalogued/undefined parent: the is-a
    // oracle cannot complete the hierarchy → Unknown → silent.
    let src = "<?php declare(strict_types=1);
final class C {
    public function m(): static { return new Foreign(); }
}
class Foreign extends \\Some\\Uncatalogued\\Vendor\\Base {}";
    assert_eq!(n(src), 0, "uncatalogued parent on the returned class → Unknown → silent");
}

#[test]
fn nullable_static_returning_null_is_silent() {
    // `?static` makes null acceptable: `return null` is silent.
    let src = "<?php declare(strict_types=1);
final class C {
    public function m(): ?static { return null; }
}";
    assert_eq!(n(src), 0, "?static accepts null");
}

#[test]
fn nullable_static_unrelated_class_fires() {
    // `?static` still rejects an unrelated non-null exact class.
    let src = "<?php declare(strict_types=1);
final class Unrelated {}
final class C {
    public function m(): ?static { return new Unrelated(); }
}";
    assert_eq!(ids(src), vec!["type.return-mismatch"], "?static rejects unrelated Unrelated");
}

#[test]
fn union_containing_static_is_silent() {
    // `static|Foo` (legal PHP) is not lowered this slice — the keyword inside a
    // union keeps §1's silence. No bound is synthesized.
    let src = "<?php declare(strict_types=1);
final class Foo {}
final class Unrelated {}
final class C {
    public function m(): static|Foo { return new Unrelated(); }
}";
    assert_eq!(n(src), 0, "union-containing-static is unlowered → silent");
}

#[test]
fn parent_without_parent_is_silent() {
    // `: parent` on a class with no `extends`: the bound is unresolvable, the
    // hint stays unlowered → silent (illegal PHP at runtime, but not this
    // check's concern; zero-FP).
    let src = "<?php declare(strict_types=1);
final class Other {}
final class C {
    public function m(): parent { return new Other(); }
}";
    assert_eq!(n(src), 0, "parent with no extends → no bound → silent");
}

#[test]
fn parent_returning_parent_instance_is_silent() {
    // `: parent` returning an instance of the resolved parent: is-a Yes, silent.
    let src = "<?php declare(strict_types=1);
class Base {}
final class Child extends Base {
    public function up(): parent { return new Base(); }
}";
    assert_eq!(n(src), 0, "returning the parent class satisfies : parent");
}
