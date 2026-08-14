//! Class-level generic carry (ADR-0032, issue #10).
//!
//! Generic arguments are **state, not solving**: `new Class(args)` carries values
//! through direct `@param T` ctor params, judged against `@param Class<A>` at call
//! sites. No call-site template solver (ADR-0030); unknown stays `Maybe`.

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

/// Conformance case `generics_template_box`.
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

/// Conformance case `generics_template_bound`: a bound template (`T of HasName`) gates it.
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

// 2. Element-add envelope over a project collection shape (issue #10 criterion): a
// class-level type argument reads as an envelope on the element added at construction.

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
    assert_eq!(
        param_count(&format!("{base}needsAnimals(new TypedList(new Dog()));")),
        0,
        "Dog element inhabits TypedList<Animal>",
    );
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

/// An unresolvable argument class stays `Maybe` — it may be a `@template` param or alias.
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

/// A template/argument arity mismatch is a thin library-author concern — silent.
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

/// The class half only gates: a wrong-class object stays silent (class-mismatch
/// reporting is deferred).
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

/// No direct `@param T` ctor param ⇒ no carry — silent even with a mismatch elsewhere.
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

/// A non-generic class under a generic spelling accepts a right-class object (empty carry).
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

/// `generics_template_bound_array`: an abstract array fact is accepted, not doubted
/// into a finding.
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

/// Bounds beyond `array` read the same way: a union bound, and a function's own `@template`.
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

    // A free function's own `@template`, not a class-level one.
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

/// **The class-bound decline.** `T of HasName` is a class bound; `T` stays opaque
/// here (deliberate scope line, issue #293).
#[test]
fn class_bound_declines() {
    let src = "<?php\n\
        interface HasName { public function name(): string; }\n\
        /** @template T of HasName */\n\
        final class Named { /** @param T $v */ public function __construct(public $v) {} }\n\
        new Named(1);";
    assert_eq!(param_count(src), 0, "a class bound is not read — T stays opaque → Maybe");
    // `of object`/`of mixed` decline too — neither constrains anything actionable.
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

/// A bound never resurrects a template *name* as a class (issue #5's shadow); a
/// method redeclaring the name unbounded loses the class-level bound.
#[test]
fn bound_does_not_leak_past_its_declaration() {
    let shadowed = "<?php\n\
        final class Model {}\n\
        /** @template Model */\n\
        final class Repo { /** @param Model $m */ public function __construct($m) {} }\n\
        new Repo(1);";
    assert_eq!(param_count(shadowed), 0, "an unbounded template stays opaque");
    let redeclared = "<?php\n\
        /** @template T of int */\n\
        final class Outer {\n\
            /** @template T\n * @param T $v */\n\
            public function m($v): void {}\n\
        }\n\
        (new Outer())->m('x');";
    assert_eq!(param_count(redeclared), 0, "the member's own unbounded T wins");
}

// 5. Type arguments on INHERITANCE EDGES (ADR-0032 amendment, issue #294):
// `@extends Box<int>` names the declaring class. Variance is unmodeled, so
// covariant/contravariant positions always answer `Maybe`.

/// Conformance case `generics_extends_implements`: a template-free `@extends Box<int>`
/// subclass behaves as `Box<int>`.
#[test]
fn extends_edge_parameterizes_the_ancestor() {
    let base = "<?php\n\
        /** @template T */\n\
        class Box {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
        }\n\
        /** @extends Box<int> */\n\
        final class IntBox extends Box {\n\
            public function __construct(int $value) { parent::__construct($value); }\n\
        }\n\
        /** @param Box<int> $box */\n\
        function takesIntBox(Box $box): void {}\n\
        /** @param Box<string> $box */\n\
        function takesStringBox(Box $box): void {}\n";
    assert_eq!(
        param_count(&format!("{base}takesIntBox(new IntBox(1));")),
        0,
        "IntBox is a Box<int>",
    );
    assert_eq!(
        param_count(&format!("{base}takesStringBox(new IntBox(1));")),
        1,
        "Box<int> does not satisfy Box<string>",
    );
}

/// `@implements` reads like `@extends`; an *invariant* interface template reaches a verdict.
#[test]
fn implements_edge_parameterizes_the_interface() {
    let base = "<?php\n\
        /** @template T */\n\
        interface Producer { /** @return T */ public function get(): mixed; }\n\
        /** @implements Producer<int> */\n\
        final class IntProducer implements Producer {\n\
            public function get(): int { return 1; }\n\
        }\n\
        /** @param Producer<string> $p */\n\
        function takesStringProducer(Producer $p): void {}\n";
    assert_eq!(
        param_count(&format!("{base}takesStringProducer(new IntProducer());")),
        1,
        "Producer<int> does not satisfy Producer<string>",
    );
}

/// **The variance regression pins** — correct today only because the carry is absent;
/// an invariant reading of the edge would false-positive here.
#[test]
fn covariant_and_contravariant_positions_stay_silent() {
    // Dog producer standing in for an Animal producer — correct under `@template-covariant`.
    let covariant = "<?php\n\
        class Animal {}\n\
        class Dog extends Animal {}\n\
        /** @template-covariant T */\n\
        interface Producer { /** @return T */ public function get(): mixed; }\n\
        /** @implements Producer<Dog> */\n\
        final class DogProducer implements Producer {\n\
            public function get(): Dog { return new Dog(); }\n\
        }\n\
        /** @param Producer<Animal> $producer */\n\
        function takesAnimalProducer(Producer $producer): void {}\n\
        takesAnimalProducer(new DogProducer());";
    assert_eq!(param_count(covariant), 0, "a covariant position never reaches a verdict");
    let covariant_disjoint = "<?php\n\
        class Animal {}\n\
        class Dog extends Animal {}\n\
        /** @template-covariant T */\n\
        interface Producer { /** @return T */ public function get(): mixed; }\n\
        /** @implements Producer<Dog> */\n\
        final class DogProducer implements Producer {\n\
            public function get(): Dog { return new Dog(); }\n\
        }\n\
        /** @param Producer<int> $producer */\n\
        function takesIntProducer(Producer $producer): void {}\n\
        takesIntProducer(new DogProducer());";
    assert_eq!(param_count(covariant_disjoint), 0, "variance gates before the comparison");
    // A consumer of Animal standing in for a consumer of Dog.
    let contravariant = "<?php\n\
        class Animal {}\n\
        class Dog extends Animal {}\n\
        /** @template-contravariant T */\n\
        interface Consumer { /** @param T $value */ public function consume($value): void; }\n\
        /** @implements Consumer<Animal> */\n\
        final class AnimalConsumer implements Consumer {\n\
            public function consume($value): void {}\n\
        }\n\
        /** @param Consumer<Dog> $consumer */\n\
        function takesDogConsumer(Consumer $consumer): void {}\n\
        takesDogConsumer(new AnimalConsumer());";
    assert_eq!(param_count(contravariant), 0, "a contravariant position never reaches a verdict");
}

/// A subclass declaring its **own** `@template` is unaffected: the value carry wins
/// and the edge is never read.
#[test]
fn own_templates_win_over_the_edge() {
    let src = "<?php\n\
        /** @template T */\n\
        class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        /** @template T\n * @extends Box<T> */\n\
        final class SubBox extends Box {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) { parent::__construct($value); }\n\
        }\n\
        /** @param SubBox<int> $b */\n\
        function f(SubBox $b): void {}\n";
    assert_eq!(
        param_count(&format!("{src}f(new SubBox('x'));")),
        1,
        "the subclass's own value carry judges its own templates",
    );
    // The edge's `T` is never lowered as a class named `T` either.
    assert_eq!(
        param_count(&format!(
            "{src}/** @param Box<int> $b */\nfunction g(Box $b): void {{}}\ng(new SubBox('x'));"
        )),
        0,
        "no ancestor edge is read from a class with its own templates",
    );
}

/// Adversarial edges, all silent: wrong owner, arity disagreement, unparameterized
/// `@extends`, and an unresolvable identifier (may be a `@phpstan-type` alias).
#[test]
fn adversarial_edges_stay_silent() {
    let ancestor = "<?php\n\
        /** @template T */\n\
        class Box { public function __construct() {} }\n";
    let wrong_owner = format!(
        "{ancestor}/** @extends Box<int> */\n\
         final class NotABox {{ public function __construct() {{}} }}\n\
         /** @param Box<string> $b */\n\
         function f($b): void {{}}\n\
         f(new NotABox());"
    );
    assert_eq!(param_count(&wrong_owner), 0, "an edge to a non-ancestor says nothing");
    let arity = format!(
        "{ancestor}/** @extends Box<int, string> */\n\
         final class Weird extends Box {{ public function __construct() {{}} }}\n\
         /** @param Box<string> $b */\n\
         function f(Box $b): void {{}}\n\
         f(new Weird());"
    );
    assert_eq!(param_count(&arity), 0, "edge/template arity disagreement stays a thin lint");
    let bare = format!(
        "{ancestor}/** @extends Box */\n\
         final class Plain extends Box {{ public function __construct() {{}} }}\n\
         /** @param Box<string> $b */\n\
         function f(Box $b): void {{}}\n\
         f(new Plain());"
    );
    assert_eq!(param_count(&bare), 0, "an unparameterized edge carries nothing");
    let alias = format!(
        "{ancestor}/** @extends Box<SomeAlias> */\n\
         final class AliasBox extends Box {{ public function __construct() {{}} }}\n\
         /** @param Box<string> $b */\n\
         function f(Box $b): void {{}}\n\
         f(new AliasBox());"
    );
    assert_eq!(param_count(&alias), 0, "an unknown class name in an edge → Maybe");
}

// 6. The carry through a VARIABLE BINDING, and the sweep that keeps it sound
// (ADR-0032 binding amendment, issue #295). The carry lives on the allocation
// (`HeapObj::targs`); a receiver call must sweep it or a stale carry false-positives.

/// The carry reaches a call through a `$box = new Box(1)` binding, not just a direct `new`.
#[test]
fn carry_survives_a_variable_binding() {
    let base = "<?php\n\
        /** @template T */\n\
        final class MutableBox {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
        }\n\
        /** @param MutableBox<int> $box */\n\
        function takesIntBox(MutableBox $box): void {}\n\
        /** @param MutableBox<string> $box */\n\
        function takesStringBox(MutableBox $box): void {}\n\
        $box = new MutableBox(1);\n";
    assert_eq!(
        param_count(&format!("{base}takesIntBox($box);")),
        0,
        "the bound object is a MutableBox<int>",
    );
    assert_eq!(
        param_count(&format!("{base}takesStringBox($box);")),
        1,
        "an int box does not satisfy a string box, through the binding",
    );
}

/// Argument passing keeps the carry when the callee provably cannot reach the object:
/// `takesIntBox` never touches its parameter.
#[test]
fn passing_the_object_as_an_argument_keeps_the_carry() {
    let src = "<?php\n\
        /** @template T */\n\
        final class MutableBox {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
        }\n\
        /** @param MutableBox<int> $box */\n\
        function takesIntBox(MutableBox $box): void {}\n\
        /** @param MutableBox<string> $box */\n\
        function takesStringBox(MutableBox $box): void {}\n\
        $box = new MutableBox(1);\n\
        takesIntBox($box);\n\
        takesStringBox($box);";
    assert_eq!(param_count(src), 1, "the carry survives an intervening call that only reads it");
}

/// **The line-57 pin.** `@phpstan-self-out self<U>` on `replace()` re-parameterizes
/// the receiver, so the carried `int` is stale after the call — both spellings must
/// stay silent. Without the sweep, `takesStringBox` here is a **false positive**.
#[test]
fn receiver_method_call_sweeps_the_stale_value_carry() {
    let base = "<?php\n\
        /** @template T */\n\
        final class MutableBox {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
            /**\n\
             * @template U\n\
             * @param U $value\n\
             * @phpstan-self-out self<U>\n\
             */\n\
            public function replace(mixed $value): void {}\n\
        }\n\
        /** @param MutableBox<int> $box */\n\
        function takesIntBox(MutableBox $box): void {}\n\
        /** @param MutableBox<string> $box */\n\
        function takesStringBox(MutableBox $box): void {}\n\
        $next = 'x';\n\
        $box = new MutableBox(1);\n\
        $box->replace($next);\n";
    assert_eq!(
        param_count(&format!("{base}takesStringBox($box);")),
        0,
        "the post-replace carry is stale — silence, not a false positive",
    );
    assert_eq!(
        param_count(&format!("{base}takesIntBox($box);")),
        0,
        "and the original parameterization is no longer claimed either",
    );
}

/// A receiver call sweeps only its own receiver — an unrelated object's carry is untouched.
#[test]
fn the_sweep_is_receiver_local() {
    let src = "<?php\n\
        /** @template T */\n\
        final class MutableBox {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
            public function touch(): void {}\n\
        }\n\
        /** @param MutableBox<string> $box */\n\
        function takesStringBox(MutableBox $box): void {}\n\
        $a = new MutableBox(1);\n\
        $b = new MutableBox(2);\n\
        $a->touch();\n\
        takesStringBox($b);";
    assert_eq!(param_count(src), 1, "sweeping $a leaves $b's carry intact");
}

/// **The sweep-immune path.** A *declared* edge carry (`@extends Box<int>`) survives
/// a receiver call, like a `readonly` prop survives a sweep.
#[test]
fn inheritance_edge_carry_survives_a_receiver_call() {
    let src = "<?php\n\
        /** @template T */\n\
        class Box {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
            public function touch(): void {}\n\
        }\n\
        /** @extends Box<int> */\n\
        final class IntBox extends Box {\n\
            public function __construct(int $value) { parent::__construct($value); }\n\
        }\n\
        /** @param Box<string> $box */\n\
        function takesStringBox(Box $box): void {}\n\
        $box = new IntBox(1);\n\
        $box->touch();\n\
        takesStringBox($box);";
    assert_eq!(param_count(src), 1, "a declared edge is not a mutable fact — it survives the sweep");
}

/// Aliasing and cloning both preserve the carry (a `Box<int>` clone is still a `Box<int>`).
#[test]
fn alias_and_clone_carry_the_arguments() {
    let base = "<?php\n\
        /** @template T */\n\
        final class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        /** @param Box<string> $box */\n\
        function takesStringBox(Box $box): void {}\n\
        $a = new Box(1);\n";
    assert_eq!(
        param_count(&format!("{base}$b = $a;\ntakesStringBox($b);")),
        1,
        "an alias shares the allocation, and so the carry",
    );
    assert_eq!(
        param_count(&format!("{base}$c = clone $a;\ntakesStringBox($c);")),
        1,
        "a clone copies the carry",
    );
}

/// A branch join **intersects**: a carry swept in one arm is gone for the successor.
#[test]
fn a_branch_that_sweeps_erases_the_carry_after_the_join() {
    let base = "<?php\n\
        /** @template T */\n\
        final class MutableBox {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
            public function touch(): void {}\n\
        }\n\
        /** @param MutableBox<string> $box */\n\
        function takesStringBox(MutableBox $box): void {}\n\
        function run(bool $c): void {\n\
            $box = new MutableBox(1);\n";
    assert_eq!(
        param_count(&format!(
            "{base}    if ($c) {{ $box->touch(); }}\n    takesStringBox($box);\n}}"
        )),
        0,
        "one arm swept it → the join drops it",
    );
    assert_eq!(
        param_count(&format!("{base}    if ($c) {{ $c = false; }}\n    takesStringBox($box);\n}}")),
        1,
        "neither arm swept it → the carry survives the join",
    );
}

// 7. The ARGUMENT-PASS gate (ADR-0032 binding amendment, argument-pass ruling): the
// carry survives a pass only where the callee provably cannot reach the object — PHP
// locals are lexical, so an unspelled parameter is untouchable. Every uncertainty sweeps.

/// **The false positive this gate prevents.** `mutate()` calls `$b->replace('s')`
/// internally, so `takesStringBox($box)` next is *correct code* — must stay silent.
#[test]
fn a_callee_that_mutates_its_parameter_sweeps_the_carry() {
    let base = "<?php\n\
        /** @template T */\n\
        final class MutableBox {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
            /**\n\
             * @template U\n\
             * @param U $value\n\
             * @phpstan-self-out self<U>\n\
             */\n\
            public function replace(mixed $value): void {}\n\
        }\n\
        function mutate(MutableBox $b): void { $b->replace('s'); }\n\
        /** @param MutableBox<int> $box */\n\
        function takesIntBox(MutableBox $box): void {}\n\
        /** @param MutableBox<string> $box */\n\
        function takesStringBox(MutableBox $box): void {}\n\
        $box = new MutableBox(1);\n\
        mutate($box);\n";
    assert_eq!(
        param_count(&format!("{base}takesStringBox($box);")),
        0,
        "the callee could have re-parameterized the box — silence, not a report",
    );
    assert_eq!(
        param_count(&format!("{base}takesIntBox($box);")),
        0,
        "and the original parameterization is not claimed afterwards either",
    );
}

/// The gate is **reachability, not mutation**: a callee that only *reads* sweeps too
/// (the ADR-0055 Part II non-mutation judgment this would need is not built).
#[test]
fn a_callee_that_merely_mentions_its_parameter_sweeps() {
    let src = "<?php\n\
        /** @template T */\n\
        final class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        function readIt(Box $b): void { $x = $b->value; }\n\
        /** @param Box<string> $box */\n\
        function takesStringBox(Box $box): void {}\n\
        $box = new Box(1);\n\
        readIt($box);\n\
        takesStringBox($box);";
    assert_eq!(param_count(src), 0, "a mentioned parameter is not a proven-unreachable one");
}

/// **Unknown is not proof of non-mutation:** unresolvable, by-ref, variadic, and
/// poisoned-body callees all sweep.
#[test]
fn an_unprovable_callee_sweeps() {
    let base = "<?php\n\
        /** @template T */\n\
        final class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        /** @param Box<string> $box */\n\
        function takesStringBox(Box $box): void {}\n\
        $box = new Box(1);\n";
    assert_eq!(
        param_count(&format!("{base}undeclared_helper($box);\ntakesStringBox($box);")),
        0,
        "an unresolvable callee sweeps",
    );
    // By-ref: the callee can rebind the caller's variable outright.
    assert_eq!(
        param_count(&format!(
            "{base}function byRef(Box &$b): void {{}}\nbyRef($box);\ntakesStringBox($box);"
        )),
        0,
        "a by-ref position sweeps",
    );
    // Variadic: no parameter to index.
    assert_eq!(
        param_count(&format!(
            "{base}function variadic(Box ...$bs): void {{}}\nvariadic($box);\ntakesStringBox($box);"
        )),
        0,
        "a variadic position sweeps",
    );
    // Poisoned body: `extract()` can reach a binding without spelling it.
    assert_eq!(
        param_count(&format!(
            "{base}function poisoned(Box $b): void {{ extract(['b' => 1]); }}\n\
             poisoned($box);\ntakesStringBox($box);"
        )),
        0,
        "a poisoned callee body sweeps",
    );
}

/// The mention test is **token-exact** (`$boxes` ≠ `$box`); a comment mention still sweeps.
#[test]
fn the_mention_test_respects_token_boundaries() {
    let src = "<?php\n\
        /** @template T */\n\
        final class Box { /** @param T $value */ public function __construct(public mixed $value) {} }\n\
        function nearName(Box $box, array $boxes): void { $n = count($boxes); }\n\
        /** @param Box<string> $b */\n\
        function takesStringBox(Box $b): void {}\n\
        $box = new Box(1);\n\
        nearName($box, []);\n\
        takesStringBox($box);";
    assert_eq!(param_count(src), 1, "$boxes is not $box — the carry survives");
}

/// A **declared** edge carry is no more swept by an argument pass than a receiver call.
#[test]
fn inheritance_edge_carry_survives_an_argument_pass() {
    let src = "<?php\n\
        /** @template T */\n\
        class Box {\n\
            /** @param T $value */\n\
            public function __construct(public mixed $value) {}\n\
            public function touch(): void {}\n\
        }\n\
        /** @extends Box<int> */\n\
        final class IntBox extends Box {\n\
            public function __construct(int $value) { parent::__construct($value); }\n\
        }\n\
        function mutate(Box $b): void { $b->touch(); }\n\
        /** @param Box<string> $box */\n\
        function takesStringBox(Box $box): void {}\n\
        $box = new IntBox(1);\n\
        mutate($box);\n\
        takesStringBox($box);";
    assert_eq!(param_count(src), 1, "a declared edge survives an argument pass");
}
