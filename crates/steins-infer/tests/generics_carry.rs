//! ADR-0032 tier 3 / issue #10 — class-level generics carry.
//!
//! Class-level generic type arguments (`Box<int>`, `NamedBox<User>`) are read as
//! **state, not solving**: a `new Class(args)` carries the per-`@template` values
//! that flow into it (tier-1 propagation — `T` *is* whatever flowed in), and a
//! declared `@param Class<A> $p` judges an incoming object's carried arguments
//! against `A`. No call-site template solver is introduced (ADR-0030 "won't
//! build"): a template binds only from a *direct* `@param T $p` constructor
//! parameter, and where knowledge is absent acceptance stays `Maybe`.
//!
//! The two `firing` tests are the in-crate twins of the conformance fixtures
//! `generics_template_box` and `generics_template_bound`.

use steins_infer::{Diagnostic, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn param_count(src: &str) -> usize {
    findings(src).into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).count()
}

// ==========================================================================
// 1. The conformance fixtures, in-crate.
// ==========================================================================

/// `generics_template_box`: `new Box('x')` is a `Box<string>` — rejected where a
/// `Box<int>` is required; `new Box(1)` is accepted.
#[test]
fn box_int_rejects_string_element() {
    let base = "<?php\n\
        /** @template T */\n\
        final class Box {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
        }\n\
        /** @param Box<int> $box */\n\
        function takesIntBox(Box $box): void {}\n";
    assert_eq!(param_count(&format!("{base}takesIntBox(new Box(1));")), 0, "Box<int> accepts int element");
    assert_eq!(
        param_count(&format!("{base}takesIntBox(new Box('x'));")),
        1,
        "Box<string> rejected where Box<int> required",
    );
}

/// `generics_template_bound`: `new NamedBox(new AnonymousUser())` carries an
/// `AnonymousUser` element — rejected where `NamedBox<User>` is required; a `User`
/// element is accepted.
#[test]
fn named_box_user_rejects_unrelated_element() {
    let base = "<?php\n\
        interface HasName { public function name(): string; }\n\
        final class User implements HasName { public function name(): string { return 'u'; } }\n\
        final class AnonymousUser {}\n\
        /** @template T of HasName */\n\
        final class NamedBox {\n\
            /** @param T $value */\n\
            public function __construct(public object $value) {}\n\
        }\n\
        /** @param NamedBox<User> $box */\n\
        function takesNamedBox(NamedBox $box): void {}\n";
    assert_eq!(
        param_count(&format!("{base}takesNamedBox(new NamedBox(new User()));")),
        0,
        "NamedBox<User> accepts a User element",
    );
    assert_eq!(
        param_count(&format!("{base}takesNamedBox(new NamedBox(new AnonymousUser()));")),
        1,
        "AnonymousUser element rejected where NamedBox<User> required",
    );
}

// ==========================================================================
// 2. Element-add envelope over a project collection shape (issue #10 criterion).
// ==========================================================================

/// A declared class-level type argument (`TypedList<User>`) is read as an envelope
/// on the element added at construction: a matching element is accepted, a
/// non-matching one rejected.
#[test]
fn collection_shape_element_add_envelope() {
    let base = "<?php\n\
        class Animal {}\n\
        class Dog extends Animal {}\n\
        final class Cat {}\n\
        /** @template T */\n\
        final class TypedList {\n\
            /** @param T $first */\n\
            public function __construct(public mixed $first) {}\n\
        }\n\
        /** @param TypedList<Animal> $list */\n\
        function needsAnimals(TypedList $list): void {}\n";
    // Element-add of a subtype element is within the envelope (is-a Yes).
    assert_eq!(
        param_count(&format!("{base}needsAnimals(new TypedList(new Dog()));")),
        0,
        "Dog element inhabits TypedList<Animal>",
    );
    // Element-add of an unrelated element violates the envelope.
    assert_eq!(
        param_count(&format!("{base}needsAnimals(new TypedList(new Cat()));")),
        1,
        "Cat element rejected against TypedList<Animal>",
    );
}

// ==========================================================================
// 3. Adversarial / honesty bounds — every one must stay silent (zero-FP).
// ==========================================================================

/// Nested generics: `list<Box<int>>` with a `Box<string>` element fires.
#[test]
fn nested_generic_fires_on_inner_mismatch() {
    let base = "<?php\n\
        /** @template T */\n\
        final class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        /** @param list<Box<int>> $xs */\n\
        function f(array $xs): void {}\n";
    assert_eq!(
        param_count(&format!("{base}f([new Box(1), new Box(2)]);")),
        0,
        "every element is a Box<int>",
    );
    assert_eq!(
        param_count(&format!("{base}f([new Box(1), new Box('x')]);")),
        1,
        "a Box<string> element breaks list<Box<int>>",
    );
}

/// An unresolvable / unknown argument class stays `Maybe` (never a manufactured
/// `No`): the declared generic argument may be a `@template` param or type alias.
#[test]
fn unknown_arg_class_stays_silent() {
    let src = "<?php\n\
        /** @template T */\n\
        final class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        final class Thing {}\n\
        /** @param Box<Unresolved> $box */\n\
        function f(Box $box): void {}\n\
        f(new Box(new Thing()));";
    assert_eq!(param_count(src), 0, "unresolved arg class → Maybe → silent");
}

/// A template/argument count mismatch is a thin library-author concern, silent
/// here: two templates declared, one argument written.
#[test]
fn arity_mismatch_stays_silent() {
    let src = "<?php\n\
        /** @template K\n * @template V */\n\
        final class Pair {\n\
            /** @param K $k\n * @param V $v */\n\
            public function __construct(public mixed $k, public mixed $v) {}\n\
        }\n\
        /** @param Pair<int> $p */\n\
        function f(Pair $p): void {}\n\
        f(new Pair('x', 'y'));";
    assert_eq!(param_count(src), 0, "declared-arg arity ≠ carried arity → silent");
}

/// The class half only gates: an object that is NOT the required class stays
/// silent under a generic spelling (generic-class class-mismatch reporting is
/// deferred; the sole `No` comes from the argument half).
#[test]
fn class_half_mismatch_is_deferred_silent() {
    let src = "<?php\n\
        /** @template T */\n\
        final class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        final class Unrelated {}\n\
        /** @param Box<int> $box */\n\
        function f($box): void {}\n\
        f(new Unrelated());";
    assert_eq!(param_count(src), 0, "wrong-class object vs generic spelling → Maybe (deferred)");
}

/// A template with no direct `@param T` constructor parameter yields no carry, so
/// the argument half stays silent even when a mismatching element exists elsewhere.
#[test]
fn no_direct_template_param_no_carry() {
    let src = "<?php\n\
        /** @template T */\n\
        final class Wrapper {\n\
            /** @param array<T> $items */\n\
            public function __construct(public array $items) {}\n\
        }\n\
        /** @param Wrapper<int> $w */\n\
        function f(Wrapper $w): void {}\n\
        f(new Wrapper(['x']));";
    assert_eq!(param_count(src), 0, "nested @param array<T> does not bind T (no solver) → silent");
}

/// A non-generic class used with a generic spelling still accepts a right-class
/// object (no false positive from an empty carry).
#[test]
fn non_generic_class_object_accepted() {
    let src = "<?php\n\
        final class Plain {}\n\
        /** @param Plain<int> $p */\n\
        function f(Plain $p): void {}\n\
        f(new Plain());";
    assert_eq!(param_count(src), 0, "empty carry on a non-generic class → argument half silent");
}
