//! The declared parameter seed (ADR-0032's 2026-08-16 amendment, issue #388): a
//! parameter that is an object **by declaration** enters its scope on the heap
//! wherever no ADR-0086 copy landed.
//!
//! Four things the seed opens, one section each: the guarded dispatch road (a
//! `final` method resolves on a lower-bound receiver, an overridable one does not),
//! the `@param`'s type arguments as a sweep-immune carry, the readers that index
//! that carry (acceptance, #362's receiver read, #363's argument binder), and the
//! non-interference between the declared-arm lane and the heap. Then the declines,
//! one fixture each, because what the seed refuses is the larger half of the rule.
//!
//! Arity is silent under the pure `NoFold` subset (no sidecar for ADR-0049's A2ii
//! homonym leg), so its fixtures drive the same [`Boot`] mock `arity.rs` uses.

use steins_infer::{
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, Folder, ID,
    PARAM_MISMATCH_ID, check, check_with,
};
use steins_syntax::SourceTree;

/// A ready boot surface: the absence family is available and the project's own
/// symbols are never runtime homonyms (`arity.rs`'s `Boot::ready`, minimal).
struct Boot;

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_function(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// Findings with the boot surface the absence/arity family needs.
fn booted(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn ids(src: &str) -> Vec<String> {
    findings(src).into_iter().map(|d| d.id.to_owned()).collect()
}

/// The single dump a fixture asks for.
fn dumped(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].message.clone()
}

/// Every dump a fixture asks for, in source order.
fn dumps(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. The guarded dispatch road: what a lower-bound receiver may and may not
//    conclude (ADR-0036 audit G1; ADR-0049 §6's refusal and its complement).
// ---------------------------------------------------------------------------

/// A `final` class's method cannot be overridden by anything, so the signature the
/// chain walk finds is the one every instance runs — the arity is provable.
#[test]
fn final_class_method_arity_fires_on_a_declared_parameter() {
    let d = booted(
        "<?php\n\
         final class Box { public function three(int $a, int $b, int $c): void {} }\n\
         function f(Box $b): void { $b->three(1, 2); }\n",
    );
    let arity: Vec<&Diagnostic> = d.iter().filter(|d| d.id == CALL_TOO_FEW_ARGUMENTS_ID).collect();
    assert_eq!(arity.len(), 1, "{d:?}");
    assert_eq!(
        arity[0].message,
        "too few arguments to Box::three(): 2 passed, 3 required — provable ArgumentCountError"
    );
}

/// A `final` method on an *open* class is equally un-overridable.
#[test]
fn final_method_arity_fires_on_a_declared_parameter() {
    let d = booted(
        "<?php\n\
         class Sealed { final public function three(int $a, int $b, int $c): void {} }\n\
         function f(Sealed $b): void { $b->three(1, 2); }\n",
    );
    assert_eq!(d.iter().filter(|d| d.id == CALL_TOO_FEW_ARGUMENTS_ID).count(), 1, "{d:?}");
}

/// The refusal's own shape, pinned: an override may ADD optional parameters, so a
/// declared `Open` holding a subclass satisfies a signature this walk never sees.
#[test]
fn overridable_method_arity_still_declines() {
    let d = booted(
        "<?php\n\
         class Open { public function three(int $a, int $b, int $c): void {} }\n\
         function f(Open $b): void { $b->three(1, 2); }\n",
    );
    assert!(d.iter().all(|d| d.id != CALL_TOO_FEW_ARGUMENTS_ID), "{d:?}");
}

/// The argument half of the same road: a `final` method's declared parameter types
/// are checked against what the call passes.
#[test]
fn final_method_argument_is_checked_on_a_declared_parameter() {
    let d = findings(
        "<?php\n\
         final class Box { public function take(int $a): void {} }\n\
         function f(Box $b): void { $b->take('s'); }\n",
    );
    assert_eq!(d.iter().filter(|d| d.id == ID).count(), 1, "{d:?}");
}

#[test]
fn overridable_method_argument_still_declines() {
    let d = findings(
        "<?php\n\
         class Open { public function take(int $a): void {} }\n\
         function f(Open $b): void { $b->take('s'); }\n",
    );
    assert!(d.iter().all(|d| d.id != ID), "{d:?}");
}

/// Member **absence** is not this seed's: the exact-receiver lane (ADR-0049 §4 leg
/// (a)) keys on `class_exact` and a lower bound never reaches it, while the S6
/// declared-receiver lane already answered from the arms and answers unchanged. The
/// pin is on the message, which names the lane that owns the claim.
#[test]
fn undefined_method_still_comes_from_the_declared_arm_lane() {
    let d = booted(
        "<?php\n\
         final class Box { public function take(int $a): void {} }\n\
         function f(Box $b): void { $b->nosuch(); }\n",
    );
    let m: Vec<&Diagnostic> = d.iter().filter(|d| d.id == CALL_UNDEFINED_METHOD_ID).collect();
    assert_eq!(m.len(), 1, "{d:?}");
    assert!(m[0].message.contains("declared receiver $b narrowed to {Box}"), "{}", m[0].message);
}

// ---------------------------------------------------------------------------
// 2. The carry: the `@param`'s own type arguments, as `CArg::Ty`.
// ---------------------------------------------------------------------------

/// A generic `Box`, its two typed sinks, and an unwrapper carrying a
/// function-level `@template`.
const GENERIC: &str = "<?php\n\
    /** @template T */\n\
    final class Box {\n\
        /** @param T $v */\n\
        public function __construct(public mixed $v) {}\n\
        public function touch(): void {}\n\
    }\n\
    /** @param Box<string> $box */ function takesStringBox(Box $box): void {}\n\
    /** @param Box<int> $box */ function takesIntBox(Box $box): void {}\n\
    /**\n\
     * @template T\n\
     * @param Box<T> $box\n\
     * @return T\n\
     */\n\
    function unwrapT(Box $box) { return $box->v; }\n";

#[test]
fn declared_carry_judges_the_argument_half() {
    let d = findings(&format!(
        "{GENERIC}/** @param Box<int> $b */ function f(Box $b): void {{ takesStringBox($b); }}\n"
    ));
    let m: Vec<&Diagnostic> = d.iter().filter(|d| d.id == PARAM_MISMATCH_ID).collect();
    assert_eq!(m.len(), 1, "{d:?}");
    assert_eq!(
        m[0].message,
        "argument $b to takesStringBox() violates declared @param Box<string> $box \
         — declared contract violation",
    );
}

#[test]
fn declared_carry_accepts_its_own_spelling() {
    let d = findings(&format!(
        "{GENERIC}/** @param Box<int> $b */ function f(Box $b): void {{ takesIntBox($b); }}\n"
    ));
    assert!(d.iter().all(|d| d.id != PARAM_MISMATCH_ID), "{d:?}");
}

/// The class half stays `Maybe` (ADR-0032 tier 3's standing deferral): only the
/// argument half of a parameterization ever answers `No` over a lower bound.
#[test]
fn class_half_stays_silent_over_a_lower_bound() {
    let d = findings(
        "<?php\n\
         final class Box {}\n\
         final class Widget {}\n\
         /** @param Widget $w */ function takesWidget(Widget $w): void {}\n\
         function f(Box $b): void { takesWidget($b); }\n",
    );
    assert!(d.iter().all(|d| d.id != PARAM_MISMATCH_ID), "{d:?}");
}

/// #363's binder reads a declared argument's carry: `@return T` names what sits at
/// `T`'s position in `Box`'s own `@template` list. Asserted, never stronger.
#[test]
fn function_template_binds_from_a_declared_argument() {
    let src = format!(
        "{GENERIC}/** @param Box<int> $b */\n\
         function f(Box $b): void {{ $v = unwrapT($b); \\PHPStan\\dumpType($v); }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: int (asserted)");
}

/// A declared carry is `CArg::Ty` and therefore **sweep-immune** (ADR-0032's #295
/// amendment): a receiver method call and an argument pass both leave it standing,
/// where a `new`-proven value carry would be gone.
#[test]
fn declared_carry_survives_a_receiver_call_and_an_argument_pass() {
    let src = format!(
        "{GENERIC}/** @param Box<int> $b */\n\
         function f(Box $b): void {{ $b->touch(); takesIntBox($b); $v = unwrapT($b); \\PHPStan\\dumpType($v); }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: int (asserted)");
    let bad = format!(
        "{GENERIC}/** @param Box<int> $b */\n\
         function f(Box $b): void {{ $b->touch(); takesStringBox($b); }}\n"
    );
    assert_eq!(
        findings(&bad).iter().filter(|d| d.id == PARAM_MISMATCH_ID).count(),
        1,
        "a declared carry is not swept by the receiver call before it",
    );
}

/// A `@param` whose argument is the declaration's own `@template` name knows
/// nothing about what sits there, so the whole carry drops rather than lowering `T`
/// to a class named `T`.
#[test]
fn a_template_named_argument_drops_the_carry() {
    let src = format!(
        "{GENERIC}/**\n * @template U\n * @param Box<U> $b\n */\n\
         function f(Box $b): void {{ takesStringBox($b); }}\n"
    );
    assert!(findings(&src).iter().all(|d| d.id != PARAM_MISMATCH_ID), "{src}");
}

/// Arity that disagrees with the owner's own `@template` list mints nothing — the
/// all-or-nothing alignment rule every carry is built under.
#[test]
fn an_arity_disagreement_seeds_the_object_without_carries() {
    let src = format!(
        "{GENERIC}/** @param Box<int, string> $b */\n\
         function f(Box $b): void {{ takesStringBox($b); $b->touch(); }}\n"
    );
    let d = findings(&src);
    assert!(d.iter().all(|d| d.id != PARAM_MISMATCH_ID), "no carry, so no argument-half verdict");
    assert_eq!(dumps(&src).len(), 0);
}

// ---------------------------------------------------------------------------
// 3. #362's receiver read on a declared receiver — divergence-registry entry 13's
//    first consequence, retired.
// ---------------------------------------------------------------------------

/// The phpstan/phpstan#9053 shape, with the receiver **declared** rather than
/// constructed: `template-type<T, ModelInterface, 'TChild'>` reads `T` off the
/// `@param Helper<Model>` carry, then `TChild` off `Model`'s own edge.
const HELPER: &str = "<?php\n\
    /** @template TChild */\n\
    interface ModelInterface {}\n\
    final class Child {}\n\
    /** @implements ModelInterface<Child> */\n\
    final class Model implements ModelInterface {}\n\
    /** @template T of ModelInterface */\n\
    final class Helper {\n\
        /** @param T $model */\n\
        public function __construct(public object $model) {}\n\
        /** @return template-type<T, ModelInterface, 'TChild'> */\n\
        public function getFirstChildren() { return null; }\n\
    }\n";

#[test]
fn template_type_reads_off_a_declared_receiver() {
    let src = format!(
        "{HELPER}/** @param Helper<Model> $h */\n\
         function f(Helper $h): void {{ $c = $h->getFirstChildren(); \\PHPStan\\dumpType($c); }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: Child (asserted)");
}

/// Without the `@param` there is no carry to index, and the read floors — the
/// declined leg, kept beside the firing one.
#[test]
fn template_type_declines_without_a_declared_carry() {
    let src = format!(
        "{HELPER}function f(Helper $h): void {{ $c = $h->getFirstChildren(); \\PHPStan\\dumpType($c); }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// 4. Non-interference: the arm lane narrows, the heap class does not.
// ---------------------------------------------------------------------------

/// `instanceof Sub` binds the guard lane; the heap keeps the declared class. The
/// dump reads the stronger of the two at each point, which is the guard inside its
/// own branch and the declaration outside it.
#[test]
fn a_guard_narrows_the_arms_and_leaves_the_heap_class_alone() {
    let src = "<?php\n\
        class Base {}\n\
        class Sub extends Base {}\n\
        function f(Base $b): void {\n\
            if ($b instanceof Sub) { \\PHPStan\\dumpType($b); }\n\
            \\PHPStan\\dumpType($b);\n\
        }\n";
    assert_eq!(dumps(src), vec!["dumped type: Sub", "dumped type: Base"]);
}

/// The `Member` implication a prior guard bound still decides a later `instanceof`
/// — the lower-bound heap class must not shadow the lane that can answer.
#[test]
fn a_bound_member_still_decides_a_later_instanceof() {
    let src = "<?php\n\
        class Base {}\n\
        class Sub extends Base {}\n\
        function f(Base $b): void {\n\
            if ($b instanceof Sub) { if ($b instanceof Sub) { \\PHPStan\\dumpType($b); } }\n\
        }\n";
    assert_eq!(dumped(src), "dumped type: Sub");
}

/// A parameter with no heap object of its own is untouched by all of this: the
/// declared-arm lane still spells it, `(asserted)` marker included.
#[test]
fn an_untyped_parameter_still_spells_from_its_arms() {
    let src = "<?php\n\
        final class Box {}\n\
        /** @param Box $b */\n\
        function f($b): void { \\PHPStan\\dumpType($b); }\n";
    assert_eq!(dumped(src), "dumped type: Box (asserted)");
}

// ---------------------------------------------------------------------------
// 5. The declines, one fixture each. Every one is a silence, and every silence is
//    the rule rather than an omission.
// ---------------------------------------------------------------------------

/// The shared shape: a `final` class whose method would report arity if the
/// parameter were seeded. Each declaration below must leave the **dispatch** ids
/// silent — the three the seed can add, and no others: what the declared-arm lane
/// says about the same fixture (`class.undefined` for an unknown class, the S6
/// `phpdoc.undefined-method` for a class that has no such method) is another lane's
/// answer and is unmoved by this slice.
fn declines(decl: &str) -> Vec<Diagnostic> {
    booted(&format!(
        "<?php\n\
         final class Box {{ public function three(int $a, int $b, int $c): void {{}} }}\n\
         final class Widget {{}}\n\
         {decl} {{ $b->three(1, 2); }}\n"
    ))
    .into_iter()
    .filter(|d| d.id == CALL_TOO_FEW_ARGUMENTS_ID || d.id == ID || d.id == PARAM_MISMATCH_ID)
    .collect()
}

#[test]
fn nullable_native_seeds_nothing() {
    assert!(declines("function f(?Box $b): void").is_empty());
    assert!(declines("function f(Box|null $b): void").is_empty());
}

#[test]
fn a_null_default_seeds_nothing() {
    assert!(declines("function f(Box $b = null): void").is_empty());
}

#[test]
fn a_union_seeds_nothing() {
    assert!(declines("function f(Box|Widget $b): void").is_empty());
}

#[test]
fn an_intersection_seeds_nothing() {
    assert!(declines("function f(Box&Widget $b): void").is_empty());
}

#[test]
fn an_unknown_class_seeds_nothing() {
    assert!(declines("function f(Nope $b): void").is_empty());
}

#[test]
fn a_by_ref_or_variadic_parameter_seeds_nothing() {
    assert!(declines("function f(Box &$b): void").is_empty());
}

/// A `@param` naming a different class than the native hint: one of the two has
/// drifted, and nothing here can tell which.
#[test]
fn a_native_phpdoc_class_disagreement_seeds_nothing() {
    assert!(declines("/** @param Widget $b */ function f(Box $b): void").is_empty());
}

/// A `@param` this reader cannot read as a plain (parameterized) class declines the
/// whole seed rather than falling back to the native hint alone.
#[test]
fn an_unreadable_param_declines_the_seed() {
    assert!(declines("/** @param Box|null $b */ function f(Box $b): void").is_empty());
}

/// The class comes from the native hint alone: `HeapObj::class` carries no stratum,
/// and a docblock reaching the dispatch would premise a proof-layer finding.
#[test]
fn a_phpdoc_only_class_seeds_nothing() {
    assert!(declines("/** @param Box $b */ function f($b): void").is_empty());
}

// ---------------------------------------------------------------------------
// 6. The descent leg: the ADR-0086 copy always wins where it landed.
// ---------------------------------------------------------------------------

/// A callee whose argument resolved to **no object** gets the declared seed, so its
/// own `final`-method call is dispatched and its declared carry reads.
#[test]
fn a_descent_with_no_argument_object_gets_the_declared_seed() {
    let src = format!(
        "{GENERIC}/** @param Box<int> $b */\n\
         function h(Box $b): void {{ takesStringBox($b); }}\n\
         function outer(Box $x): void {{ h($x); }}\n"
    );
    // One report per site the walk reaches; what matters is that the declared carry
    // fires inside `h` rather than going silent.
    assert!(
        findings(&src).iter().any(|d| d.id == PARAM_MISMATCH_ID),
        "the declared seed reaches a descent whose argument proved no object",
    );
}

/// An argument that **did** resolve to an object keeps ADR-0086's copy — props and
/// all — rather than being overwritten by the declaration's prop-free shell.
#[test]
fn a_descent_with_an_argument_object_keeps_the_copy() {
    let src = "<?php\n\
        declare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Box { public function __construct(public mixed $value) {} }\n\
        function h(Box $b): void { needString($b->value); }\n\
        h(new Box(1));\n";
    let d = findings(src);
    assert_eq!(
        d.iter().filter(|d| d.id == ID).count(),
        1,
        "the copy's props survived the declared seed: {d:?}",
    );
}

// ---------------------------------------------------------------------------
// 7. `$this` is untouched: the seed runs after it and never over it.
// ---------------------------------------------------------------------------

#[test]
fn a_parameter_named_this_is_not_a_thing_the_seed_can_shadow() {
    let src = "<?php\n\
        final class Box { public int $n = 1; public function m(Box $b): void { \\PHPStan\\dumpType($this); } }\n";
    assert_eq!(dumped(src), "dumped type: Box");
}

#[test]
fn ids_are_stable_for_a_plain_typed_parameter() {
    // A typed parameter that does nothing suspicious reports nothing at all.
    assert!(ids("<?php\nfinal class Box { public function m(): void {} }\nfunction f(Box $b): void { $b->m(); }\n").is_empty());
}
