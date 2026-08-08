//! The untyped surface (ADR-0078 / issue #200): the contract-layer `untyped.*`
//! family, P9 of the rule-port map.
//!
//! Six ids, one premise shape: **a claim the code does not make**. Every id gets a
//! firing fixture *and* a typed counterpart — a native type, a docblock type, and
//! (for the iterable arm) a docblock type that narrows a native one — because a
//! family this broad is only trustworthy if each silence is pinned as deliberately
//! as each finding.
//!
//! The boundary these tests defend is **presence, not agreement**: an `Asserted`
//! docblock claim, even one that disagrees with the code, makes a declaration typed
//! here. A wrong claim is `phpdoc.*`'s finding (ADR-0078 §2); this family's subject
//! is the claim that was never made.

use std::collections::BTreeMap;

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    Diagnostic, Floor, Layer, NoFold, UNTYPED_CLASS_CONSTANT_ID, UNTYPED_GENERICS_ID,
    UNTYPED_ITERABLE_VALUE_ID, UNTYPED_PARAMETER_ID, UNTYPED_PROPERTY_ID, UNTYPED_RETURN_ID, check,
    check_project, layer, surface_floor,
};
use steins_syntax::SourceTree;

/// Every `untyped.*` finding a source produces.
fn untyped(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php").into_iter().filter(|d| d.id.starts_with("untyped.")).collect()
}

/// The findings carrying exactly `id`.
fn of(src: &str, id: &str) -> Vec<Diagnostic> {
    untyped(src).into_iter().filter(|d| d.id == id).collect()
}

/// How many findings carry `id`.
fn n(src: &str, id: &str) -> usize {
    of(src, id).len()
}

/// Assert `src` produces no `untyped.*` finding at all — the strongest silence,
/// used wherever a fixture is meant to be fully typed.
fn silent(src: &str) {
    let ds = untyped(src);
    assert!(ds.is_empty(), "expected a fully typed fixture to be silent, got: {ds:?}");
}

/// Whether the built-in profile `name` surfaces `d` (the `s6_routing.rs` harness).
fn surfaced(name: &str, d: &Diagnostic) -> bool {
    ProfileConfigs(BTreeMap::new()).resolve(Some(name)).unwrap().is_surfaced(d)
}

// ---------------------------------------------------------------------------
// The registry contract: six contract-layer ids, five at the `Contracts` floor
// and one at `Strict`.
// ---------------------------------------------------------------------------

#[test]
fn every_id_is_a_contract_layer_id_at_the_contracts_floor() {
    // `untyped.iterable-value` and `untyped.generics` are the ADR's remaining
    // `Contracts→Strict by measurement` rows. They ship at the family's floor;
    // moving either is a one-line registry edit, and this test is where the move
    // must be recorded.
    for id in [
        UNTYPED_PARAMETER_ID,
        UNTYPED_RETURN_ID,
        UNTYPED_PROPERTY_ID,
        UNTYPED_ITERABLE_VALUE_ID,
        UNTYPED_GENERICS_ID,
    ] {
        assert_eq!(layer(id), Some(Layer::Contract), "{id}");
        assert_eq!(surface_floor(id), Some(Floor::Contracts), "{id}");
    }
    // The arm that already made that move (2026-08-09): a class constant's
    // initializer is a constant expression, so the type is pinned with or without a
    // written one. The layer is unchanged — declared debt is still declared debt —
    // and only the rung that asks for it moved.
    assert_eq!(layer(UNTYPED_CLASS_CONSTANT_ID), Some(Layer::Contract));
    assert_eq!(surface_floor(UNTYPED_CLASS_CONSTANT_ID), Some(Floor::Strict));
}

// ---------------------------------------------------------------------------
// `untyped.parameter`
// ---------------------------------------------------------------------------

#[test]
fn a_parameter_with_no_native_type_and_no_param_tag_reports() {
    let ds = of("<?php\nfunction f($x) { return 1; }\n", UNTYPED_PARAMETER_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("$x"), "{}", ds[0].message);
}

#[test]
fn a_native_typed_parameter_is_silent() {
    silent("<?php\nfunction f(int $x): int { return $x; }\n");
}

#[test]
fn a_native_type_the_engine_does_not_model_still_counts_as_typed() {
    // The whole reason the check reads a hint SPAN rather than the lowered
    // `Param::ty`: `mixed`, `callable`, `object`, `self` and `void` all lower to
    // `None` there for modeling reasons that have nothing to do with the source
    // having written a type.
    silent("<?php\nfunction f(mixed $a, callable $b, object $c): void {}\n");
    silent("<?php\nclass C { public function f(self $s): static { return $this; } }\n");
    silent("<?php\nfunction f(int $x): never { throw new \\Exception(); }\n");
}

#[test]
fn a_docblock_param_makes_the_parameter_typed() {
    silent("<?php\n/** @param int $x\n * @return int */\nfunction f($x) { return 1; }\n");
}

#[test]
fn a_prefixed_docblock_param_makes_the_parameter_typed() {
    // `@phpstan-param` / `@psalm-param` fold into the same tag kind.
    assert_eq!(
        n("<?php\n/** @phpstan-param int $x */\nfunction f($x): int { return 1; }\n", UNTYPED_PARAMETER_ID),
        0
    );
    assert_eq!(
        n("<?php\n/** @psalm-param int $x */\nfunction f($x): int { return 1; }\n", UNTYPED_PARAMETER_ID),
        0
    );
}

#[test]
fn a_docblock_param_that_disagrees_with_the_code_is_still_a_claim() {
    // ADR-0078's boundary, stated as a test: an ABSENT claim is this family's debt,
    // a WRONG one is `phpdoc.*`'s finding. `f("s")` violates the `@param int`, and
    // whatever else reports it, `untyped.parameter` must not.
    let src = "<?php\n/** @param int $x\n * @return int */\nfunction f($x) { return 1; }\nf(\"s\");\n";
    assert_eq!(n(src, UNTYPED_PARAMETER_ID), 0);
}

#[test]
fn a_param_tag_for_another_parameter_leaves_this_one_untyped() {
    let ds = of(
        "<?php\n/** @param int $a\n * @return int */\nfunction f($a, $b) { return 1; }\n",
        UNTYPED_PARAMETER_ID,
    );
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("$b"), "{}", ds[0].message);
}

#[test]
fn variadic_and_by_ref_spellings_still_name_the_parameter() {
    // Both spellings declare a real parameter, so both are subjects — and both are
    // silenced by a `@param` naming them.
    assert_eq!(n("<?php\nfunction f(...$args): void {}\n", UNTYPED_PARAMETER_ID), 1);
    assert_eq!(n("<?php\nfunction f(&$out): void {}\n", UNTYPED_PARAMETER_ID), 1);
    assert_eq!(
        n("<?php\n/** @param int ...$args */\nfunction f(...$args): void {}\n", UNTYPED_PARAMETER_ID),
        0
    );
    assert_eq!(
        n("<?php\n/** @param int &$out */\nfunction f(&$out): void {}\n", UNTYPED_PARAMETER_ID),
        0
    );
}

#[test]
fn a_param_tag_naming_nobody_makes_the_unclaimed_parameters_decline() {
    // A `@param int` with no `$name` could be about any parameter, so guessing that
    // it is about none would convict annotated code. Every parameter the docblock
    // does not visibly claim declines.
    let src = "<?php\n/** @param int */\nfunction f($x): int { return 1; }\n";
    assert_eq!(n(src, UNTYPED_PARAMETER_ID), 0, "{:?}", untyped(src));
    // A parameter with its OWN readable `@param` is unaffected either way, and its
    // sibling still declines.
    let mixed = "<?php\n/** @param array<int> $a\n * @param int */\nfunction f(array $a, $b): int { return 1; }\n";
    assert_eq!(n(mixed, UNTYPED_PARAMETER_ID), 0, "{:?}", untyped(mixed));
    assert_eq!(n(mixed, UNTYPED_ITERABLE_VALUE_ID), 0, "{:?}", untyped(mixed));
}

#[test]
fn a_method_parameter_reports_like_a_function_one() {
    let ds = of(
        "<?php\nclass C { public function m($x): int { return 1; } }\n",
        UNTYPED_PARAMETER_ID,
    );
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("C::m()"), "{}", ds[0].message);
}

// ---------------------------------------------------------------------------
// `untyped.return`
// ---------------------------------------------------------------------------

#[test]
fn a_function_with_no_native_return_and_no_return_tag_reports() {
    let ds = of("<?php\nfunction f(int $x) { return $x; }\n", UNTYPED_RETURN_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("f()"), "{}", ds[0].message);
}

#[test]
fn a_native_return_type_is_silent() {
    silent("<?php\nfunction f(int $x): int { return $x; }\n");
    silent("<?php\nfunction f(int $x): void {}\n");
}

#[test]
fn a_docblock_return_is_silent() {
    silent("<?php\n/** @return int */\nfunction f(int $x) { return $x; }\n");
}

#[test]
fn construct_and_destruct_are_excluded_by_construction() {
    // PHP forbids a return type on either, so their silence is a language rule and
    // not information withheld. The parameter arm still applies to `__construct`.
    let src = "<?php\nclass C {\n  public function __construct(int $x) {}\n  public function __destruct() {}\n}\n";
    silent(src);
}

#[test]
fn a_generator_body_with_no_declared_return_still_reports() {
    // The DECISION (issue #200): a `yield` body is not an implicit claim. `Generator`
    // is a type the code could have written and did not, and inferring it here would
    // be inference — which this family does not do. So the ordinary rule applies,
    // and this test is what pins it against a later "generators are obvious" drift.
    let ds = of("<?php\nfunction gen() { yield 1; }\n", UNTYPED_RETURN_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    // And writing the claim silences it, by either spelling.
    silent("<?php\nfunction gen(): \\Generator { yield 1; }\n");
    silent("<?php\n/** @return \\Generator<int, int, mixed, void> */\nfunction gen() { yield 1; }\n");
}

// ---------------------------------------------------------------------------
// `untyped.property`
// ---------------------------------------------------------------------------

#[test]
fn a_property_with_no_native_type_and_no_var_tag_reports() {
    let ds = of("<?php\nclass C { public $p; }\n", UNTYPED_PROPERTY_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("C::$p"), "{}", ds[0].message);
}

#[test]
fn a_native_typed_property_is_silent() {
    silent("<?php\nclass C { public int $p = 0; }\n");
}

#[test]
fn a_var_docblock_property_is_silent() {
    silent("<?php\nclass C {\n  /** @var int */\n  public $p;\n}\n");
}

#[test]
fn every_item_of_a_multi_item_declaration_is_its_own_subject() {
    assert_eq!(n("<?php\nclass C { public $a, $b; }\n", UNTYPED_PROPERTY_ID), 2);
    // The declaration's one `@var` covers the whole declaration, as PHP writes it.
    assert_eq!(n("<?php\nclass C {\n  /** @var int */\n  public $a, $b;\n}\n", UNTYPED_PROPERTY_ID), 0);
}

#[test]
fn a_promoted_constructor_property_is_reported_once_on_the_parameter_arm() {
    // One declaration, one finding. The parameter arm owns promoted properties, so
    // the property arm must stay out of the way entirely — otherwise a single
    // `public function __construct(public $x)` would earn two findings for one
    // missing type.
    let src = "<?php\nclass C { public function __construct(public $x) {} }\n";
    assert_eq!(n(src, UNTYPED_PARAMETER_ID), 1, "{:?}", untyped(src));
    assert_eq!(n(src, UNTYPED_PROPERTY_ID), 0, "{:?}", untyped(src));
    // And a typed promotion is silent on both.
    silent("<?php\nclass C { public function __construct(public int $x) {} }\n");
    // The ctor's `@param` types the promotion, exactly as it types any parameter.
    silent("<?php\nclass C {\n  /** @param int $x */\n  public function __construct(public $x) {}\n}\n");
}

// ---------------------------------------------------------------------------
// `untyped.class-constant`
// ---------------------------------------------------------------------------

#[test]
fn a_class_constant_with_no_native_type_and_no_var_tag_reports() {
    let ds = of("<?php\nclass C { const K = 1; }\n", UNTYPED_CLASS_CONSTANT_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("C::K"), "{}", ds[0].message);
}

#[test]
fn a_native_typed_class_constant_is_silent() {
    // PHP 8.3 typed constants.
    silent("<?php\nclass C { const int K = 1; }\n");
}

#[test]
fn a_var_docblock_class_constant_is_silent() {
    silent("<?php\nclass C {\n  /** @var int */\n  const K = 1;\n}\n");
}

#[test]
fn an_interface_constant_is_a_subject_too() {
    assert_eq!(n("<?php\ninterface I { const K = 1; }\n", UNTYPED_CLASS_CONSTANT_ID), 1);
}

#[test]
fn an_enum_case_is_never_a_class_constant_finding() {
    // A case's type IS its enum — there is no claim to withhold. The enum's own
    // ordinary constants are still subjects, which is what tells this exclusion
    // apart from the enum simply not being walked.
    let src = "<?php\nenum Suit: string {\n  case Hearts = 'H';\n  case Spades = 'S';\n}\n";
    assert_eq!(n(src, UNTYPED_CLASS_CONSTANT_ID), 0, "{:?}", untyped(src));
    let with_const = "<?php\nenum Suit: string {\n  case Hearts = 'H';\n  const K = 1;\n}\n";
    let ds = of(with_const, UNTYPED_CLASS_CONSTANT_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("Suit::K"), "{}", ds[0].message);
}

// ---------------------------------------------------------------------------
// `untyped.iterable-value` — the noisy one, kept mechanically exact.
// ---------------------------------------------------------------------------

#[test]
fn a_bare_native_array_parameter_reports_the_value_type() {
    let ds = of("<?php\nfunction f(array $a): void {}\n", UNTYPED_ITERABLE_VALUE_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("$a"), "{}", ds[0].message);
    // A native `array` IS a type, so the plain parameter arm stays quiet.
    assert_eq!(n("<?php\nfunction f(array $a): void {}\n", UNTYPED_PARAMETER_ID), 0);
}

#[test]
fn a_bare_native_iterable_reports_too_and_a_nullable_or_union_spelling_still_counts() {
    assert_eq!(n("<?php\nfunction f(iterable $a): void {}\n", UNTYPED_ITERABLE_VALUE_ID), 1);
    assert_eq!(n("<?php\nfunction f(?array $a): void {}\n", UNTYPED_ITERABLE_VALUE_ID), 1);
    assert_eq!(n("<?php\nfunction f(array|null $a): void {}\n", UNTYPED_ITERABLE_VALUE_ID), 1);
    assert_eq!(n("<?php\nfunction f(int|array $a): void {}\n", UNTYPED_ITERABLE_VALUE_ID), 1);
}

#[test]
fn a_non_iterable_native_type_is_not_this_ids_subject() {
    // The keyword set is `array`/`iterable` and nothing else — a `Traversable` or a
    // userland collection is a different rule than the one being ported.
    silent("<?php\nfunction f(int $a): void {}\n");
    silent("<?php\nfunction f(\\Traversable $a): void {}\n");
}

#[test]
fn every_narrowing_docblock_spelling_silences_a_native_array() {
    for decl in [
        "array<int>",
        "array<string, int>",
        "int[]",
        "list<int>",
        "non-empty-list<int>",
        "iterable<int>",
        "array{a: int, b: string}",
    ] {
        let src = format!("<?php\n/** @param {decl} $a */\nfunction f(array $a): void {{}}\n");
        assert_eq!(n(&src, UNTYPED_ITERABLE_VALUE_ID), 0, "`{decl}` must narrow the native array");
    }
}

#[test]
fn a_docblock_that_restates_the_bare_array_does_not_narrow_it() {
    assert_eq!(
        n("<?php\n/** @param array $a */\nfunction f(array $a): void {}\n", UNTYPED_ITERABLE_VALUE_ID),
        1
    );
    // A union whose ARRAY arm is still bare is still unstated — the narrowed arm
    // does not excuse the bare one.
    assert_eq!(
        n("<?php\n/** @param int[]|array $a */\nfunction f(array $a): void {}\n", UNTYPED_ITERABLE_VALUE_ID),
        1
    );
}

#[test]
fn a_bare_docblock_array_on_an_otherwise_untyped_parameter_reports() {
    // A docblock claim replaces the native side, so the claim's own bareness is the
    // finding — and the parameter is NOT `untyped.parameter`, because a claim exists.
    let src = "<?php\n/** @param array $a */\nfunction f($a): void {}\n";
    assert_eq!(n(src, UNTYPED_ITERABLE_VALUE_ID), 1, "{:?}", untyped(src));
    assert_eq!(n(src, UNTYPED_PARAMETER_ID), 0, "{:?}", untyped(src));
}

#[test]
fn the_return_and_property_positions_carry_the_iterable_arm_too() {
    let ret = "<?php\nfunction f(): array { return []; }\n";
    assert_eq!(n(ret, UNTYPED_ITERABLE_VALUE_ID), 1, "{:?}", untyped(ret));
    silent("<?php\n/** @return list<int> */\nfunction f(): array { return []; }\n");

    let prop = "<?php\nclass C { public array $p = []; }\n";
    assert_eq!(n(prop, UNTYPED_ITERABLE_VALUE_ID), 1, "{:?}", untyped(prop));
    silent("<?php\nclass C {\n  /** @var array<string, int> */\n  public array $p = [];\n}\n");
}

// ---------------------------------------------------------------------------
// `untyped.generics`
// ---------------------------------------------------------------------------

/// A same-file `@template`-carrying class, plus a consumer to annotate.
fn generic_fixture(param: &str) -> String {
    format!(
        "<?php\n/** @template T */\nclass Collection {{}}\n\
         /** @param {param} $c */\nfunction f($c): void {{}}\n"
    )
}

#[test]
fn a_generic_class_used_without_type_arguments_reports() {
    let src = generic_fixture("Collection");
    let ds = of(&src, UNTYPED_GENERICS_ID);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("Collection"), "{}", ds[0].message);
    assert!(ds[0].message.contains("@template T"), "{}", ds[0].message);
}

#[test]
fn the_same_class_with_type_arguments_is_silent() {
    assert_eq!(n(&generic_fixture("Collection<int>"), UNTYPED_GENERICS_ID), 0);
}

#[test]
fn a_class_that_declares_no_template_is_never_this_ids_subject() {
    let src = "<?php\nclass Plain {}\n/** @param Plain $c */\nfunction f($c): void {}\n";
    assert_eq!(n(src, UNTYPED_GENERICS_ID), 0, "{:?}", untyped(src));
}

#[test]
fn a_generic_class_nested_inside_another_type_is_still_bare() {
    // `array<Collection>` names `Collection` without arguments just as surely as a
    // top-level occurrence does.
    assert_eq!(n(&generic_fixture("array<Collection>"), UNTYPED_GENERICS_ID), 1);
    assert_eq!(n(&generic_fixture("Collection|null"), UNTYPED_GENERICS_ID), 1);
}

#[test]
fn a_template_name_of_the_declaration_itself_is_not_a_class() {
    // A bare `@param T` names the declaration's own template parameter (issue #5's
    // shadow set), not a class — so it is never a bare generic use.
    let src = "<?php\n/** @template T */\nclass Collection {}\n\
               /** @template Collection\n * @param Collection $c */\nfunction f($c): void {}\n";
    assert_eq!(n(src, UNTYPED_GENERICS_ID), 0, "{:?}", untyped(src));
}

#[test]
fn a_class_level_template_shadows_the_name_in_every_member_docblock() {
    let src = "<?php\n/** @template T */\nclass Collection {}\n\
               /** @template Collection */\nclass Holder {\n\
               \x20 /** @param Collection $c */\n  public function m($c): void {}\n}\n";
    assert_eq!(n(src, UNTYPED_GENERICS_ID), 0, "{:?}", untyped(src));
}

#[test]
fn the_template_lookup_reaches_across_files() {
    // The boundary worth pinning: the lookup runs off the resident whole-project
    // class index (`Cx::find_class`), which is the same read the class-level
    // `@template` shadow already uses — so a generic class declared in ANOTHER file
    // is found, with no new index and no new pass. Nothing here is narrowed to the
    // current file.
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = [
        ("lib.php", "<?php\n/** @template T */\nclass Collection {}\n"),
        ("main.php", "<?php\n/** @param Collection $c */\nfunction f($c): void {}\n"),
    ]
    .into_iter()
    .map(|(p, t)| SourceFile::new(&db, p.to_owned(), t.to_owned()))
    .collect();
    let project =
        Project::new(&db, inputs, steins_db::ProjectLayout::fallback(), steins_db::PluginFacts::none());
    let ds: Vec<Diagnostic> = check_project(&db, project, &mut NoFold)
        .into_iter()
        .filter(|d| d.id == UNTYPED_GENERICS_ID)
        .collect();
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].path, "main.php");
}

#[test]
fn the_return_and_property_positions_carry_the_generics_arm_too() {
    let ret = "<?php\n/** @template T */\nclass Collection {}\n\
               /** @return Collection */\nfunction f() {}\n";
    assert_eq!(n(ret, UNTYPED_GENERICS_ID), 1, "{:?}", untyped(ret));

    let prop = "<?php\n/** @template T */\nclass Collection {}\n\
                class C {\n  /** @var Collection */\n  public $p;\n}\n";
    assert_eq!(n(prop, UNTYPED_GENERICS_ID), 1, "{:?}", untyped(prop));
}

// ---------------------------------------------------------------------------
// Profile surfacing (ADR-0050 §5 / ADR-0062 A-G10).
// ---------------------------------------------------------------------------

#[test]
fn the_family_is_absent_from_the_default_surface_and_present_from_contracts() {
    // The whole point of the contract floor: a bare `steins check` on ordinary
    // untyped code stays quiet, and the family is reached only by opting up.
    let src = "<?php\nclass C {\n  const K = 1;\n  public $p;\n  public function m($x, array $a) { return 1; }\n}\n";
    let ds = untyped(src);
    assert!(!ds.is_empty(), "the fixture must exercise the family");
    for d in &ds {
        assert!(!surfaced("default", d), "`{}` must not reach a bare check", d.id);
        assert!(surfaced("strict", d), "the ladder is cumulative for `{}`", d.id);
        if d.id == UNTYPED_CLASS_CONSTANT_ID {
            // The one arm that opted up: a constant's initializer already pins its
            // type, so the missing declaration is a covariance-contract concern
            // rather than untyped surface. `strict` only.
            assert!(!surfaced("contracts", d), "`{}` is the strict-floor arm", d.id);
        } else {
            assert!(surfaced("contracts", d), "`{}` must reach --profile contracts", d.id);
        }
    }
    // All five non-generics ids are exercised by that one fixture.
    for id in [
        UNTYPED_PARAMETER_ID,
        UNTYPED_RETURN_ID,
        UNTYPED_PROPERTY_ID,
        UNTYPED_CLASS_CONSTANT_ID,
        UNTYPED_ITERABLE_VALUE_ID,
    ] {
        assert!(ds.iter().any(|d| d.id == id), "`{id}` missing from {ds:?}");
    }
}

#[test]
fn a_fully_typed_file_produces_nothing_at_all() {
    // The composite silence: every arm, every position, one file.
    silent(
        "<?php\n\
         /** @template T */\n\
         class Collection {}\n\
         class C {\n\
        \x20 const int K = 1;\n\
        \x20 /** @var array<string, int> */\n\
        \x20 public array $p = [];\n\
        \x20 public function __construct(public int $x) {}\n\
        \x20 /**\n\
        \x20  * @param list<int> $a\n\
        \x20  * @return Collection<int>\n\
        \x20  */\n\
        \x20 public function m(array $a, Collection $unused): Collection { return new Collection(); }\n\
         }\n",
    );
}
