//! The strict floor of the envelope checks (ADR-0100): `effect.maybe-envelope-exceeded`
//! and `throw.maybe-undeclared`. Units, direct and inherited reporting, the five
//! discharges and what they deliberately leave. The per-gap-kind fixtures live
//! beside the emitter, with the totality check over `GapKind::ALL`.

use steins_infer::{
    Diagnostic, EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID, EFFECT_ID, THROW_MAYBE_UNDECLARED_ID, check,
};
use steins_syntax::SourceTree;

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn of(src: &str, id: &str) -> Vec<Diagnostic> {
    run(src).into_iter().filter(|d| d.id == id).collect()
}

fn effect(src: &str) -> Vec<Diagnostic> {
    of(src, EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID)
}

fn throw(src: &str) -> Vec<Diagnostic> {
    of(src, THROW_MAYBE_UNDECLARED_ID)
}

/// The findings whose message names `unit` as the declaration judged.
fn about<'a>(ds: &'a [Diagnostic], unit: &str) -> Vec<&'a Diagnostic> {
    let pat = format!("but {unit}() ");
    ds.iter().filter(|d| d.message.contains(&pat)).collect()
}

// ---- units and granularity -------------------------------------------------

#[test]
fn a_declaration_with_no_envelope_is_no_unit() {
    let src = "<?php\nfunction f(callable $c): mixed { return $c(); }\n";
    assert!(effect(src).is_empty());
    assert!(throw(src).is_empty());
}

#[test]
fn a_gap_in_the_proven_lane_s_silence_is_named_and_the_definite_id_stays_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(callable $c): mixed { return $c(); }\n";
    assert_eq!(effect(src).len(), 1);
    assert!(of(src, EFFECT_ID).is_empty(), "nothing was proven");
}

#[test]
fn one_finding_per_site_and_kind() {
    // Two sites of one kind.
    let two = "<?php\n#[\\Steins\\Pure]\nfunction f(callable $c): mixed {\n    $c();\n    return $c();\n}\n";
    let ds = effect(two);
    assert_eq!(ds.len(), 2, "{ds:#?}");
    assert_eq!((ds[0].line, ds[1].line), (4, 5));
    // One site of two kinds: a spread names the argument list and the iteration.
    let kinds = "<?php\n#[\\Steins\\Pure]\nfunction f(array $a): array { return array_keys(...$a); }\n";
    let ds = effect(kinds);
    assert_eq!(ds.len(), 2, "{ds:#?}");
    assert!(ds.iter().any(|d| d.message.contains("(argument-list:")));
    assert!(ds.iter().any(|d| d.message.contains("(operator-iteration:")));
    // The throw lane the same.
    let throws = "<?php\n/** @throws \\RuntimeException */\nfunction f(callable $c): mixed {\n    $c();\n    return $c();\n}\n";
    assert_eq!(throw(throws).len(), 2);
}

#[test]
fn a_method_unit_is_named_class_and_method() {
    let src = "<?php\nclass K {\n    #[\\Steins\\Pure]\n    public function f(callable $c): mixed { return $c(); }\n}\n";
    let ds = effect(src);
    assert_eq!(ds.len(), 1);
    assert_eq!(
        ds[0].message,
        "effects at this site are unbounded (dynamic-callee: the callee is computed at run time), but K::f() is declared #[\\Steins\\Pure]"
    );
    assert_eq!((ds[0].line, ds[0].column), (4, 52));
}

#[test]
fn a_labelled_envelope_is_quoted_back() {
    let src = "<?php\n#[\\Steins\\Effect('io.db')]\nfunction f(callable $c): mixed { return $c(); }\n";
    assert!(effect(src)[0].message.ends_with("but f() is declared #[\\Steins\\Effect('io.db')]"));
    let interop = "<?php\n/** @phpstan-impure io.db */\nfunction g(callable $c): mixed { return $c(); }\n";
    assert!(effect(interop)[0].message.ends_with("but g() is declared @phpstan-impure io.db"));
}

// ---- discharge 1: a top envelope bounds nothing ----------------------------

#[test]
fn a_top_envelope_is_no_unit() {
    let bare = "<?php\n/** @phpstan-impure */\nfunction f(callable $c): mixed { return $c(); }\n";
    assert!(effect(bare).is_empty());
    let class = "<?php\n/** @phpstan-all-methods-impure */\nclass K {\n    public function f(callable $c): mixed { return $c(); }\n}\n";
    assert!(effect(class).is_empty());
    let throws = "<?php\n/** @throws \\Throwable */\nfunction f(callable $c): mixed { return $c(); }\n";
    assert!(throw(throws).is_empty());
    let union = "<?php\n/** @throws \\RuntimeException|\\Throwable */\nfunction f(callable $c): mixed { return $c(); }\n";
    assert!(throw(union).is_empty());
}

#[test]
fn a_class_level_pure_tag_makes_each_method_a_unit() {
    let src = "<?php\n/** @phpstan-all-methods-pure */\nclass K {\n    public function f(callable $c): mixed { return $c(); }\n}\n";
    assert_eq!(effect(src).len(), 1);
}

// ---- discharge 2: a call through a parameter with a purity contract --------

#[test]
fn a_call_through_a_pure_callable_parameter_is_discharged() {
    for doc in [
        "@param pure-callable $f",
        "@param pure-closure $f",
        "@param static-pure-closure $f",
        "@pure-unless-callable-is-impure $f",
        "@phpstan-pure-unless-callable-is-impure $f",
    ] {
        let src = format!(
            "<?php\n/**\n * {doc}\n */\n#[\\Steins\\Pure]\nfunction f(callable $f): mixed {{ return $f(); }}\n"
        );
        assert!(effect(&src).is_empty(), "{doc}: {:#?}", effect(&src));
    }
}

#[test]
fn only_the_flagged_parameter_is_discharged() {
    let src = "<?php\n/**\n * @param pure-callable $f\n * @param callable $g\n */\n#[\\Steins\\Pure]\nfunction f(callable $f, callable $g): mixed { $f(); return $g(); }\n";
    let ds = effect(src);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    // A plain `callable` is no contract, and neither is no docblock.
    let plain = "<?php\n/** @param callable $f */\n#[\\Steins\\Pure]\nfunction f(callable $f): mixed { return $f(); }\n";
    assert_eq!(effect(plain).len(), 1);
}

#[test]
fn a_rebound_parameter_is_not_discharged() {
    let src = "<?php\n/**\n * @param pure-callable $f\n */\n#[\\Steins\\Pure]\nfunction f(callable $f, callable $g): mixed { $f = $g; return $f(); }\n";
    assert_eq!(effect(src).len(), 1);
    let by_ref = "<?php\nfunction swap(callable &$x): void {}\n/**\n * @param pure-callable $f\n */\n#[\\Steins\\Pure]\nfunction f(callable $f): mixed { swap($f); return $f(); }\n";
    assert_eq!(effect(by_ref).iter().filter(|d| d.message.contains("(dynamic-callee:")).count(), 1);
}

#[test]
fn the_purity_contract_says_nothing_about_throws() {
    let src = "<?php\n/**\n * @param pure-callable $f\n * @throws \\RuntimeException\n */\nfunction f(callable $f): mixed { return $f(); }\n";
    assert_eq!(throw(src).len(), 1, "a pure callable may still throw");
}

// ---- discharge 3: an interop envelope whose bound fits ----------------------

const GATE: &str = "<?php
interface Gate {
    /** @phpstan-impure io.db */
    public function open(): int;
    /** @phpstan-pure */
    public function peek(): int;
}
";

#[test]
fn an_interop_bound_that_fits_is_discharged_and_one_that_exceeds_is_named() {
    let fits = format!("{GATE}#[\\Steins\\Effect('io.db')]\nfunction f(Gate $g): int {{ return $g->open(); }}\n");
    assert!(effect(&fits).is_empty(), "{:#?}", effect(&fits));
    let empty = format!("{GATE}#[\\Steins\\Pure]\nfunction f(Gate $g): int {{ return $g->peek(); }}\n");
    assert!(effect(&empty).is_empty(), "the empty bound fits every envelope");
    let exceeds = format!("{GATE}#[\\Steins\\Pure]\nfunction f(Gate $g): int {{ return $g->open(); }}\n");
    let ds = effect(&exceeds);
    assert_eq!(ds.len(), 1);
    assert!(ds[0].message.contains("(interop-envelope:"));
    assert!(of(&exceeds, EFFECT_ID).is_empty(), "a declared bound is never a proven effect");
}

// ---- discharge 4: edges to an enveloped callee -----------------------------

#[test]
fn an_edge_to_an_enveloped_callee_is_owed_to_the_callee_s_own_finding() {
    let effects = "<?php\n#[\\Steins\\Pure]\nfunction callee(callable $f): mixed { return $f(); }\n#[\\Steins\\Pure]\nfunction caller(callable $f): mixed { return callee($f); }\n";
    let ds = effect(effects);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert!(ds[0].message.contains("but callee() is declared"), "reported once, at the callee");
    let throws = "<?php\n/** @throws \\RuntimeException */\nfunction callee(callable $f): mixed { return $f(); }\n/** @throws \\RuntimeException */\nfunction caller(callable $f): mixed { return callee($f); }\n";
    let ds = throw(throws);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert!(ds[0].message.contains("but callee() declares"));
}

#[test]
fn an_edge_to_a_callee_with_no_envelope_is_named_with_its_gap_kinds() {
    let effects = "<?php\nfunction callee(callable $f): mixed { return $f(); }\n#[\\Steins\\Pure]\nfunction caller(callable $f): mixed { return callee($f); }\n";
    let ds = effect(effects);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert_eq!(
        ds[0].message,
        "callee() has effects the analysis cannot bound (dynamic-callee) and declares no envelope of its own, but caller() is declared #[\\Steins\\Pure]"
    );
    assert_eq!(ds[0].line, 4);
    let throws = "<?php\nfunction callee(callable $f): mixed { return $f(); }\n/** @throws \\RuntimeException */\nfunction caller(callable $f): mixed { return callee($f); }\n";
    let ds = throw(throws);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert!(ds[0].message.starts_with("callee() can throw what the analysis cannot bound (dynamic-callee)"));
}

#[test]
fn a_top_envelope_callee_is_not_an_envelope_for_its_callers() {
    let effects = "<?php\n/** @phpstan-impure */\nfunction callee(callable $f): mixed { return $f(); }\n#[\\Steins\\Pure]\nfunction caller(callable $f): mixed { return callee($f); }\n";
    assert_eq!(effect(effects).len(), 1);
    let throws = "<?php\n/** @throws \\Throwable */\nfunction callee(callable $f): mixed { return $f(); }\n/** @throws \\RuntimeException */\nfunction caller(callable $f): mixed { return callee($f); }\n";
    assert_eq!(throw(throws).len(), 1);
}

#[test]
fn a_closure_has_no_envelope_so_its_gap_is_named_at_the_edge() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $a, callable $c): array { return array_map(fn($x) => $c(), $a); }\n";
    let ds = effect(src);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert!(ds[0].message.starts_with("closure (line 3) has effects"), "{}", ds[0].message);
}

#[test]
fn an_untainting_edge_carries_no_inherited_finding() {
    let src = "<?php\n/**\n * @pure-unless-callable-is-impure $f\n */\nfunction apply(callable $f): mixed { return $f(); }\n#[\\Steins\\Pure]\nfunction caller(): mixed { return apply(fn() => 1); }\n";
    assert!(effect(src).is_empty(), "{:#?}", effect(src));
}

#[test]
fn a_callee_with_no_gap_reports_nothing() {
    let src = "<?php\nfunction callee(int $a): int { return $a + 1; }\n#[\\Steins\\Pure]\nfunction caller(int $a): int { return callee($a); }\n/** @throws \\RuntimeException */\nfunction t(int $a): int { return callee($a); }\n";
    assert!(effect(src).is_empty());
    assert!(throw(src).is_empty());
}

// ---- discharge 5: a catch that absorbs Throwable ---------------------------

#[test]
fn a_site_under_a_throwable_catch_is_discharged() {
    for catch in ["\\Throwable", "\\LogicException | \\Throwable", "\\throwable"] {
        let src = format!(
            "<?php\n/** @throws \\RuntimeException */\nfunction f(callable $f): mixed {{\n    try {{ return $f(); }} catch ({catch} $e) {{ return null; }}\n}}\n"
        );
        assert!(throw(&src).is_empty(), "{catch}: {:#?}", throw(&src));
    }
    let nested = "<?php\n/** @throws \\RuntimeException */\nfunction f(callable $f): mixed {\n    try {\n        try { return $f(); } catch (\\LogicException $e) { return null; }\n    } catch (\\Throwable $e) { return null; }\n}\n";
    assert!(throw(nested).is_empty(), "an outer guard counts");
}

#[test]
fn a_narrower_catch_or_a_site_outside_it_discharges_nothing() {
    let narrow = "<?php\n/** @throws \\RuntimeException */\nfunction f(callable $f): mixed {\n    try { return $f(); } catch (\\Exception $e) { return null; }\n}\n";
    assert_eq!(throw(narrow).len(), 1);
    let outside = "<?php\n/** @throws \\RuntimeException */\nfunction f(callable $f, callable $g): mixed {\n    try { $g(); } catch (\\Throwable $e) { }\n    return $f();\n}\n";
    let ds = throw(outside);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].line, 5);
}

#[test]
fn an_edge_under_a_throwable_catch_is_discharged_too() {
    let src = "<?php\nfunction callee(callable $f): mixed { return $f(); }\n/** @throws \\RuntimeException */\nfunction f(callable $f): mixed {\n    try { return callee($f); } catch (\\Throwable $e) { return null; }\n}\n";
    assert!(throw(src).is_empty());
}

#[test]
fn a_catch_does_not_discharge_an_effect_gap() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(callable $f): mixed {\n    try { return $f(); } catch (\\Throwable $e) { return null; }\n}\n";
    assert_eq!(effect(src).len(), 1, "a catch bounds what is thrown, never what is done");
}

// ---- deliberately not discharged -------------------------------------------

#[test]
fn a_missing_catalog_row_is_still_named() {
    let effects = "<?php\n#[\\Steins\\Pure]\nfunction f(array $a): int { return array_sum($a); }\n";
    assert!(effect(effects)[0].message.contains("(no-effect-row:"));
    let throws = "<?php\n/** @throws \\RuntimeException */\nfunction f(): int { return mb_strlen('x'); }\n";
    assert!(throw(throws)[0].message.contains("(no-throw-row:"));
}

// ---- nothing else moves ----------------------------------------------------

#[test]
fn the_floor_adds_findings_and_changes_no_other_id() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(callable $c): mixed { return $c(); }\n/** @throws \\RuntimeException */\nfunction g(callable $c): mixed { return $c(); }\n";
    let ids: Vec<&str> = run(src).iter().map(|d| d.id).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID));
    assert!(ids.contains(&THROW_MAYBE_UNDECLARED_ID));
}

#[test]
fn about_filters_by_the_judged_unit() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(callable $c): mixed { return $c(); }\n#[\\Steins\\Pure]\nfunction g(callable $c): mixed { return $c(); }\n";
    let ds = effect(src);
    assert_eq!(about(&ds, "f").len(), 1);
    assert_eq!(about(&ds, "g").len(), 1);
}
