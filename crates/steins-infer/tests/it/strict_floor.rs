//! The strict floor of the envelope checks (ADR-0100): `effect.maybe-envelope-exceeded`
//! and `throw.maybe-undeclared`. Units, direct and inherited reporting, the five
//! discharges and what they deliberately leave. The per-gap-kind fixtures live
//! beside the emitter, with the totality check over `GapKind::ALL`.

use steins_db::{EffectsPolicy, PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    Diagnostic, EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID, EFFECT_ID, NoFold, THROW_MAYBE_UNDECLARED_ID,
    check, check_project,
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

// ---- discharge 2: a call through a flagged parameter of a free function ----

/// A free function flagged `@pure-unless-callable-is-impure $f`, whose `$f()` the
/// call sites decide (ADR-0063).
fn flagged(doc: &str, sig: &str, body: &str) -> String {
    format!("<?php\n/**\n * {doc}\n */\n#[\\Steins\\Pure]\nfunction f({sig}): mixed {{ {body} }}\n")
}

#[test]
fn a_call_through_a_flagged_parameter_is_discharged() {
    for doc in ["@pure-unless-callable-is-impure $f", "@phpstan-pure-unless-callable-is-impure $f"] {
        let src = flagged(doc, "callable $f", "return $f();");
        assert!(effect(&src).is_empty(), "{doc}: {:#?}", effect(&src));
    }
}

#[test]
fn the_typed_spellings_are_not_discharged() {
    // The call-site obligation check proves impurity of a closure or a first-class
    // callable only, so a string or array callable is never decided by the type.
    for doc in ["@param pure-callable $f", "@param pure-closure $f", "@param static-pure-closure $f"] {
        let src = flagged(doc, "callable $f", "return $f();");
        let ds = effect(&src);
        assert_eq!(ds.len(), 1, "{doc}: {ds:#?}");
        assert!(ds[0].message.contains("(dynamic-callee:"));
    }
}

#[test]
fn only_the_flagged_parameter_is_discharged() {
    let src = flagged(
        "@pure-unless-callable-is-impure $f",
        "callable $f, callable $g",
        "$f(); return $g();",
    );
    let ds = effect(&src);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    // A plain `callable` is no contract, and neither is no docblock.
    let plain = "<?php\n/** @param callable $f */\n#[\\Steins\\Pure]\nfunction f(callable $f): mixed { return $f(); }\n";
    assert_eq!(effect(plain).len(), 1);
}

#[test]
fn a_rebound_flagged_parameter_is_not_discharged() {
    let tag = "@pure-unless-callable-is-impure $f";
    let src = flagged(tag, "callable $f, callable $g", "$f = $g; return $f();");
    assert_eq!(effect(&src).len(), 1);
    let by_ref = format!(
        "<?php\nfunction swap(callable &$x): void {{}}\n{}",
        flagged(tag, "callable $f", "swap($f); return $f();").trim_start_matches("<?php\n")
    );
    assert_eq!(effect(&by_ref).iter().filter(|d| d.message.contains("(dynamic-callee:")).count(), 1);
}

#[test]
fn a_flagged_parameter_with_a_default_is_not_discharged() {
    // `f()` fills the slot with the declaration's own default, which no call site decides.
    let src = flagged(
        "@pure-unless-callable-is-impure $f",
        "callable $f = 'impure_fn'",
        "return $f();",
    );
    assert_eq!(effect(&src).len(), 1);
}

#[test]
fn the_tag_is_honoured_on_free_functions_only() {
    let src = "<?php\nclass K {\n    /**\n     * @pure-unless-callable-is-impure $f\n     */\n    #[\\Steins\\Pure]\n    public function f(callable $f): mixed { return $f(); }\n}\n";
    assert_eq!(effect(src).len(), 1, "a method is a unit with no discharge 2");
}

#[test]
fn the_purity_contract_says_nothing_about_throws() {
    let src = "<?php\n/**\n * @pure-unless-callable-is-impure $f\n * @throws \\RuntimeException\n */\nfunction f(callable $f): mixed { return $f(); }\n";
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

// ---- discharge 4, relative to the callee's envelope --------------------------

const IO_GATE: &str =
    "<?php\ninterface Gate { /** @phpstan-impure io.db */ public function open(): int; }\n";

#[test]
fn an_enveloped_callee_is_trusted_only_through_an_envelope_that_fits() {
    // `k` is bounded by `io.db` (discharge 3 at its own site), so its gap is not its caller's
    // when the caller admits `io.db`, and is named at the call when the caller does not.
    let k = "#[\\Steins\\Effect('io.db')]\nfunction k(Gate $g): int { return $g->open(); }\n";
    let wide = format!("{IO_GATE}{k}#[\\Steins\\Effect('io.db')]\nfunction f(Gate $g): int {{ return k($g); }}\n");
    assert!(effect(&wide).is_empty(), "{:#?}", effect(&wide));
    let narrow = format!("{IO_GATE}{k}#[\\Steins\\Pure]\nfunction f(Gate $g): int {{ return k($g); }}\n");
    let ds = effect(&narrow);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert!(ds[0].message.starts_with("k() bounds its effects"), "{}", ds[0].message);
    assert!(ds[0].message.contains("allowing io.db"), "{}", ds[0].message);
    assert_eq!(ds[0].line, 6, "at the call, not at k");
    // Without k's envelope the generic inherited finding names it.
    let bare = format!("{IO_GATE}function k(Gate $g): int {{ return $g->open(); }}\n#[\\Steins\\Pure]\nfunction f(Gate $g): int {{ return k($g); }}\n");
    assert!(effect(&bare)[0].message.contains("declares no envelope of its own"));
}

#[test]
fn a_tainting_edge_into_a_flagged_function_is_named_at_the_call() {
    let callee = "<?php\n/**\n * @pure-unless-callable-is-impure $f\n */\n#[\\Steins\\Pure]\nfunction f(callable $f): mixed { return $f(); }\n";
    // The callable is whatever the caller was handed: the call does not decide the contract.
    let tainting = format!("{callee}#[\\Steins\\Pure]\nfunction g(callable $c): mixed {{ return f($c); }}\n");
    let ds = effect(&tainting);
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert!(ds[0].message.starts_with("f() is pure only if the callable bound to its parameter is"));
    assert_eq!(ds[0].line, 8);
    // A visible callback decides it: an untainting edge, nothing at the call.
    let decided = format!("{callee}#[\\Steins\\Pure]\nfunction g(): mixed {{ return f(fn() => 1); }}\n");
    assert!(effect(&decided).is_empty(), "{:#?}", effect(&decided));
}

// ---- discharge 6: the project's tolerance policy (ADR-0084) ------------------

/// Every `effect.maybe-envelope-exceeded` finding of a one-file project under a policy
/// tolerating `telemetry` and attributing it to `Trace` and `Logger`.
fn effect_under_telemetry(src: &str) -> Vec<Diagnostic> {
    let attribution: Vec<(String, Vec<String>)> = ["Trace", "Logger"]
        .iter()
        .map(|k| ((*k).to_owned(), vec!["telemetry".to_owned()]))
        .collect();
    let policy = EffectsPolicy::new(vec!["telemetry".to_owned()], attribution);
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "test.php".to_owned(), src.to_owned());
    let project = Project::builder(vec![file], ProjectLayout::fallback(), PluginFacts::none())
        .effects(policy)
        .new(&db);
    check_project(&db, project, &mut NoFold)
        .into_iter()
        .filter(|d| d.id == EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID)
        .collect()
}

#[test]
fn an_edge_into_an_attributed_callee_is_discharged_by_the_policy() {
    let src = "<?php\nfinal class Trace { public static function debug(mixed $line): void { error_log('x' . $line); } }\n#[\\Steins\\Pure]\nfunction f(int $a): int { Trace::debug($a); return $a + 1; }\n";
    assert_eq!(effect(src).len(), 1, "with no policy the edge is named");
    assert!(effect_under_telemetry(src).is_empty(), "{:#?}", effect_under_telemetry(src));
}

#[test]
fn a_call_on_an_attributed_declared_receiver_is_discharged_by_the_policy() {
    let src = "<?php\ninterface Logger { public function info(string $m): void; }\n#[\\Steins\\Pure]\nfunction f(Logger $l, int $a): int { $l->info('x'); return $a + 1; }\n";
    assert_eq!(effect(src).len(), 1);
    assert!(effect_under_telemetry(src).is_empty(), "{:#?}", effect_under_telemetry(src));
    // A receiver the policy does not attribute stays a gap.
    let other = src.replace("Logger", "Mailer");
    assert_eq!(effect_under_telemetry(&other).len(), 1);
}

#[test]
fn an_interop_bound_the_policy_tolerates_is_discharged() {
    // The imported label is tolerated, so the claim cannot break the envelope.
    let src = format!("{IO_GATE}#[\\Steins\\Pure]\nfunction f(Gate $g): int {{ return $g->open(); }}\n");
    assert_eq!(effect(&src).len(), 1, "with no policy the bound exceeds Pure");
    let policy = EffectsPolicy::new(vec!["io.db".to_owned()], Vec::new());
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "test.php".to_owned(), src);
    let project = Project::builder(vec![file], ProjectLayout::fallback(), PluginFacts::none())
        .effects(policy)
        .new(&db);
    let found: Vec<_> = check_project(&db, project, &mut NoFold)
        .into_iter()
        .filter(|d| d.id == EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID)
        .collect();
    assert!(found.is_empty(), "{found:#?}");
    // An interop tag naming a label the registry does not know is ⊤, and its receiver is
    // a declared receiver the policy attributes (or not) like any other.
    let unknown = "<?php\ninterface Gate { /** @phpstan-impure telemetry */ public function open(): int; }\n#[\\Steins\\Pure]\nfunction f(Gate $g): int { return $g->open(); }\n";
    assert_eq!(effect_under_telemetry(unknown).len(), 1, "Gate is not attributed");
    let attributed = unknown.replace("Gate", "Logger");
    assert!(effect_under_telemetry(&attributed).is_empty());
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
