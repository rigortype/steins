//! Class-level generic carry (ADR-0032, issue #10).
//!
//! Generic arguments are **state, not solving**: `new Class(args)` carries values
//! flowing through direct `@param T` constructor parameters, and
//! `@param Class<A>` judges the carried arguments against `A`. There is no
//! call-site template solver (ADR-0030); absent knowledge stays `Maybe`.

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

// 1. The conformance fixtures, in-crate.

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

// 2. Element-add envelope over a project collection shape (issue #10 criterion).

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

// 3. Adversarial / honesty bounds — every one must stay silent (zero-FP).

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

// 4. Template BOUNDS as upper-bound contracts (ADR-0032 tier 1, issue #293).
//
// A declared bound is a promise about every value that ever binds to the template,
// so a bound parameter can be judged without solving anything: whatever reaches
// `@param T $x` inhabits `T`, and `T` is at most its bound. Only **vocabulary**
// bounds participate in this slice — a class bound declines (see below).

/// `generics_template_bound_array`: `@template T of array` makes `@param T $items`
/// an `array` contract at the call site. A literal `1` violates it; an array does
/// not, and an abstract array fact is accepted rather than doubted into a finding.
#[test]
fn vocabulary_bound_array_gates_the_constructor_argument() {
    let base = "<?php\n\
        /** @template T of array */\n\
        final class Collection {\n\
            /** @param T $items */\n\
            public function __construct(public $items) {}\n\
        }\n";
    assert_eq!(
        param_count(&format!("{base}new Collection(1);")),
        1,
        "1 is outside the `of array` bound on T",
    );
    assert_eq!(
        param_count(&format!("{base}new Collection([1, 2]);")),
        0,
        "an array satisfies the `of array` bound",
    );
    // The abstract-fact path: `$row` is a declared parameter carrying no proven
    // value, so the bound answers Maybe — the silence conformance line 51 pins.
    let abstract_arg = format!(
        "{base}/** @param array{{id: int}} $row */\n\
         function fill(array $row): void {{ new Collection($row); }}"
    );
    assert_eq!(
        param_count(&abstract_arg),
        0,
        "a declared array param satisfies the bound abstractly → silent",
    );
}

/// Bounds other than `array` read the same way, including a union bound and a
/// bound reached through a *function*'s own `@template` rather than a class's.
#[test]
fn vocabulary_bounds_beyond_array() {
    let int_bound = "<?php\n\
        /** @template T of int */\n\
        final class Counter { /** @param T $n */ public function __construct(public $n) {} }\n";
    assert_eq!(param_count(&format!("{int_bound}new Counter('x');")), 1, "string outside `of int`");
    assert_eq!(param_count(&format!("{int_bound}new Counter(3);")), 0, "int satisfies `of int`");

    let union_bound = "<?php\n\
        /** @template T of int|list<int> */\n\
        final class Nums { /** @param T $v */ public function __construct(public $v) {} }\n";
    assert_eq!(
        param_count(&format!("{union_bound}new Nums('x');")),
        1,
        "string outside the union bound",
    );
    assert_eq!(
        param_count(&format!("{union_bound}new Nums(7);")),
        0,
        "int inhabits the union bound",
    );

    // A free function's own `@template` declaration, not a class-level one.
    let fn_bound = "<?php\n\
        /** @template T of string\n * @param T $s */\n\
        function takesStringy($s): void {}\n";
    assert_eq!(param_count(&format!("{fn_bound}takesStringy(1);")), 1, "1 outside `of string`");
    assert_eq!(
        param_count(&format!("{fn_bound}takesStringy('a');")),
        0,
        "string satisfies the bound",
    );
}

/// **The class-bound decline.** `@template T of HasName` is a class bound, and this
/// slice does not read it: `T` stays opaque and `new Named(1)` is silent, exactly as
/// before. Declining is the deliberate scope line (issue #293) — class bounds are
/// the common corpus shape and reading them is a follow-up, not a half-check here.
#[test]
fn class_bound_declines() {
    let src = "<?php\n\
        interface HasName { public function name(): string; }\n\
        /** @template T of HasName */\n\
        final class Named { /** @param T $v */ public function __construct(public $v) {} }\n\
        new Named(1);";
    assert_eq!(param_count(src), 0, "a class bound is not read — T stays opaque → Maybe");
    // `of object` and `of mixed` decline too: neither is information a check can
    // act on, and the first is the class direction in disguise.
    let obj = "<?php\n\
        /** @template T of object */\n\
        final class Holder { /** @param T $v */ public function __construct(public $v) {} }\n\
        new Holder(1);";
    assert_eq!(param_count(obj), 0, "`of object` declines");
    let mixed = "<?php\n\
        /** @template T of mixed */\n\
        final class Any { /** @param T $v */ public function __construct(public $v) {} }\n\
        new Any(1);";
    assert_eq!(param_count(mixed), 0, "`of mixed` constrains nothing");
}

/// A bound never resurrects a template *name* as a class: an unbounded template
/// whose name collides with a real class stays opaque (issue #5's shadow), and a
/// template redeclared on a method without a bound loses the class-level bound.
#[test]
fn bound_does_not_leak_past_its_declaration() {
    // Unbounded template named like a real class — the #5 shadow, unchanged.
    let shadowed = "<?php\n\
        final class Model {}\n\
        /** @template Model */\n\
        final class Repo { /** @param Model $m */ public function __construct($m) {} }\n\
        new Repo(1);";
    assert_eq!(param_count(shadowed), 0, "an unbounded template stays opaque");
    // A method redeclaring the class-level name without a bound shadows the bound.
    let redeclared = "<?php\n\
        /** @template T of int */\n\
        final class Outer {\n\
            /** @template T\n * @param T $v */\n\
            public function m($v): void {}\n\
        }\n\
        (new Outer())->m('x');";
    assert_eq!(param_count(redeclared), 0, "the member's own unbounded T wins");
}
