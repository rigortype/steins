//! Tolerated effects (ADR-0084): policy-declared discharge at judgment time.
//!
//! The three invariants under test are the ADR's own. **The catalog never lies**:
//! every fixture asserts that the proven labels a summary shows are exactly what
//! they were before any policy existed. **The concealment is auditable**: a policy
//! with no tolerance discharges nothing, and emptying the tolerance
//! (`--no-tolerated-effects`) brings every finding back. **Reversible per
//! question**: only the envelope judgment and the purity oracle consult the
//! policy; the labels are all still there to answer "what touches the clock".
//!
//! Leg 2 — attribution — is must-semantics over paths, so the fixtures that
//! matter most are the ones where an effect reaches a declaration *twice*.

use steins_db::{EffectsPolicy, PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    check_project, effect_summary, Diagnostic, EffectSummary, NoFold, EFFECT_ID, EFFECT_LISKOV_ID,
    PARAM_MISMATCH_ID,
};
use steins_syntax::SourceTree;

/// Every diagnostic a one-file project earns under `policy`.
fn check(src: &str, policy: EffectsPolicy) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "test.php".to_owned(), src.to_owned());
    let project = Project::builder(vec![file], ProjectLayout::fallback(), PluginFacts::none())
        .effects(policy)
        .new(&db);
    check_project(&db, project, &mut NoFold)
}

/// The `effect.envelope-exceeded` findings only.
fn effects(src: &str, policy: EffectsPolicy) -> Vec<Diagnostic> {
    check(src, policy).into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

/// The proven-label summary of one declaration — the `annotate` margin, which no
/// policy is allowed to move.
fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// A policy tolerating `labels` and attributing nothing.
fn tolerate(labels: &[&str]) -> EffectsPolicy {
    EffectsPolicy::new(labels.iter().map(|l| (*l).to_owned()).collect(), Vec::new())
}

/// A policy tolerating `telemetry` and attributing it to `keys`.
fn telemetry(keys: &[&str]) -> EffectsPolicy {
    EffectsPolicy::new(vec!["telemetry".to_owned()], attribution(keys))
}

/// `keys` attributed `telemetry`, in config shape.
fn attribution(keys: &[&str]) -> Vec<(String, Vec<String>)> {
    keys.iter().map(|k| ((*k).to_owned(), vec!["telemetry".to_owned()])).collect()
}

// ---------------------------------------------------------------------------
// Leg 1: the label itself is tolerated.
// ---------------------------------------------------------------------------

/// The whole `mutate.local` mechanism, generalized: a project may name the labels
/// its envelopes tolerate.
const ECHOES: &str = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): void { echo $s; }\n";

#[test]
fn tolerating_a_label_discharges_the_finding_that_carries_it() {
    assert_eq!(effects(ECHOES, EffectsPolicy::none()).len(), 1, "the finding exists");
    assert_eq!(effects(ECHOES, tolerate(&["io.output"])).len(), 0, "and is discharged");
}

#[test]
fn tolerance_follows_the_taxonomy_downward_not_upward() {
    // `io.output` subsumes the proven `io.output.buffer` (ADR-0018), so tolerating
    // the parent covers the child. Tolerating a sibling covers nothing.
    assert_eq!(effects(ECHOES, tolerate(&["io.output.buffer"])).len(), 0, "exact");
    assert_eq!(effects(ECHOES, tolerate(&["io"])).len(), 0, "an ancestor covers it");
    assert_eq!(effects(ECHOES, tolerate(&["io.fs"])).len(), 1, "a sibling does not");
}

#[test]
fn a_proven_child_discharges_under_a_tolerated_parent() {
    // The proven label is `io.fs.write` (the path argument is a literal, so the
    // stream row narrows), and the policy names only `io.fs`.
    const SRC: &str = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): void { file_put_contents('/tmp/x', $s); }\n";
    assert_eq!(summary(SRC, "f").labels, vec!["io.fs.write".to_owned()]);
    assert_eq!(effects(SRC, EffectsPolicy::none()).len(), 1);
    assert_eq!(effects(SRC, tolerate(&["io.fs"])).len(), 0);
}

#[test]
fn the_proven_lane_is_untouched_by_the_tolerance() {
    // The catalog never lies (ADR-0084 invariant 1): the label is still there, and
    // `annotate` still shows it. Only the judgment tolerated it.
    assert_eq!(summary(ECHOES, "f").labels, vec!["io.output.buffer".to_owned()]);
}

// ---------------------------------------------------------------------------
// Leg 2: every path the effect arrived by is attributed.
// ---------------------------------------------------------------------------

/// The logger-pollution shape the ADR was written for: a pure-declared function
/// reaches the clock and a stream through a logging facade and nothing else.
const FACADE: &str = "<?php\nclass Logger {\n    public static function debug(string $m): void { fwrite(STDERR, $m . time()); }\n}\nfunction helper(string $m): void { Logger::debug($m); }\n#[\\Steins\\Pure]\nfunction f(string $s): int { helper($s); return 1; }\n";

#[test]
fn an_attributed_facade_discharges_the_effects_that_flow_through_it() {
    // Two findings today: the stream write and the clock read, both honest, both
    // arriving only through the logger.
    assert_eq!(effects(FACADE, EffectsPolicy::none()).len(), 2, "the pollution");
    assert_eq!(effects(FACADE, telemetry(&["Logger"])).len(), 0, "discharged as telemetry");
}

#[test]
fn the_attributed_labels_stay_visible_in_the_margin() {
    // The audit question "what touches the clock" is still answerable: the proven
    // set is what it was, tolerance or no tolerance.
    let mut labels = summary(FACADE, "f").labels;
    labels.sort();
    assert_eq!(labels, vec!["io.output.stderr".to_owned(), "nondet.time".to_owned()]);
}

#[test]
fn a_class_key_covers_every_method_and_a_method_key_covers_one() {
    const TWO: &str = "<?php\nclass Logger {\n    public static function debug(string $m): void { fwrite(STDERR, $m); }\n    public static function today(): int { return time(); }\n}\n#[\\Steins\\Pure]\nfunction f(string $s): int { Logger::debug($s); return Logger::today(); }\n";
    assert_eq!(effects(TWO, EffectsPolicy::none()).len(), 2);
    assert_eq!(effects(TWO, telemetry(&["Logger"])).len(), 0, "the class covers both methods");
    // The method key is the precision half: the log line is telemetry, and the
    // business-logic clock read in the same class is not.
    let one = effects(TWO, telemetry(&["Logger::debug"]));
    assert_eq!(one.len(), 1, "{one:#?}");
    assert!(one[0].message.contains("nondet.time"), "{}", one[0].message);
}

#[test]
fn a_global_function_is_attributable_by_name() {
    const SRC: &str = "<?php\nfunction app_trace(string $m): void { fwrite(STDERR, $m); }\n#[\\Steins\\Pure]\nfunction f(string $s): int { app_trace($s); return 1; }\n";
    assert_eq!(effects(SRC, EffectsPolicy::none()).len(), 1);
    assert_eq!(effects(SRC, telemetry(&["app_trace"])).len(), 0);
}

#[test]
fn a_direct_arrival_keeps_its_report_while_the_facade_path_discharges() {
    // Must-semantics over paths (ADR-0084 §2): the same declaration reads the clock
    // through the logger AND in its own body. The second arrival is not telemetry,
    // was never attributed, and is exactly the finding the author needs to see.
    const BOTH: &str = "<?php\nclass Logger {\n    public static function debug(string $m): void { fwrite(STDERR, $m . time()); }\n}\n#[\\Steins\\Pure]\nfunction f(string $s): int { Logger::debug($s); return time(); }\n";
    let kept = effects(BOTH, telemetry(&["Logger"]));
    assert_eq!(kept.len(), 1, "exactly the direct read survives: {kept:#?}");
    assert!(kept[0].message.contains("time() has effect nondet.time"), "{}", kept[0].message);
    // And with no policy at all, all three arrivals report.
    assert_eq!(effects(BOTH, EffectsPolicy::none()).len(), 3);
}

#[test]
fn one_undischarged_group_emits_exactly_one_diagnostic() {
    // Attribution variants of one finding are copies, not findings: a declaration
    // the logger reaches down two paths — one attributed, one not — must not be
    // told about it twice.
    const TWO_PATHS: &str = "<?php\nfunction clock(): int { return time(); }\nclass Logger {\n    public static function debug(): int { return clock(); }\n}\n#[\\Steins\\Pure]\nfunction f(): int { return Logger::debug() + clock(); }\n";
    let bare = effects(TWO_PATHS, EffectsPolicy::none());
    assert_eq!(bare.len(), 2, "one per call site, as before: {bare:#?}");
    // Attributing the logger splits the callee's own finding into two copies; the
    // bare `clock()` call site is the one that still reports, once.
    let tolerated = effects(TWO_PATHS, telemetry(&["Logger"]));
    assert_eq!(tolerated.len(), 1, "no duplicate from the attribution copy: {tolerated:#?}");
}

// ---------------------------------------------------------------------------
// Builtin production sites: the boundary is the call, not an edge.
// ---------------------------------------------------------------------------

/// The telemetry shape a codebase reaches for before anyone writes a facade:
/// PHP's own logging builtin, called straight from the body.
const ERROR_LOG: &str = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): int { error_log($s); return 1; }\n";

#[test]
fn an_attributed_builtin_discharges_at_its_caller() {
    // A builtin draws no edge in the effect graph — its findings go straight into
    // the caller's direct set — so the attribution is stamped where the effect is
    // produced. Every path to it passes through this call by construction.
    assert_eq!(effects(ERROR_LOG, EffectsPolicy::none()).len(), 1);
    assert_eq!(effects(ERROR_LOG, telemetry(&["error_log"])).len(), 0);
}

#[test]
fn an_attributed_builtin_discharges_transitively() {
    // Born attributed at the production site, the copy carries its attribution up
    // every edge above it without any of those callees being attributed.
    const VIA: &str = "<?php\nfunction helper(string $s): void { error_log($s); }\n#[\\Steins\\Pure]\nfunction f(string $s): int { helper($s); return 1; }\n";
    assert_eq!(effects(VIA, EffectsPolicy::none()).len(), 1);
    assert_eq!(effects(VIA, telemetry(&["error_log"])).len(), 0);
}

#[test]
fn an_attributed_builtin_without_tolerance_stays_reported() {
    // Attribution is fact; only the tolerance is policy, for a builtin exactly as
    // for a project symbol.
    let inert = EffectsPolicy::new(Vec::new(), attribution(&["error_log"]));
    assert_eq!(effects(ERROR_LOG, inert).len(), 1);
}

#[test]
fn a_clock_read_beside_an_attributed_builtin_still_reports() {
    // Must-semantics are untouched by where the attribution was stamped.
    const BOTH: &str = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): int { error_log($s); return time(); }\n";
    let kept = effects(BOTH, telemetry(&["error_log"]));
    assert_eq!(kept.len(), 1, "{kept:#?}");
    assert!(kept[0].message.contains("nondet.time"), "{}", kept[0].message);
    assert_eq!(effects(BOTH, EffectsPolicy::none()).len(), 2);
}

#[test]
fn an_attributed_builtin_keeps_its_transport_label_in_the_margin() {
    // The catalog never lies: `error_log` is still `io`, whoever tolerates it.
    assert_eq!(summary(ERROR_LOG, "f").labels, vec!["io".to_owned()]);
}

#[test]
fn a_second_attribution_label_carries_its_own_tolerance() {
    // The ADR's survey guidance, mechanized: "safe to stop watching" and "must
    // always fire" are different risk profiles, so `audit` is its own label with
    // its own tolerance rather than riding `telemetry`.
    const SRC: &str = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): int { error_log($s); syslog(LOG_INFO, $s); return 1; }\n";
    let split = EffectsPolicy::new(
        vec!["telemetry".to_owned()],
        vec![
            ("error_log".to_owned(), vec!["telemetry".to_owned()]),
            ("syslog".to_owned(), vec!["audit".to_owned()]),
        ],
    );
    let kept = effects(SRC, split);
    assert_eq!(kept.len(), 1, "audit is attributed but not tolerated: {kept:#?}");
    assert!(kept[0].message.contains("syslog"), "{}", kept[0].message);
}

#[test]
fn a_catalogued_builtin_class_method_is_attributable() {
    // The same production-site argument for `builtin_method_findings`: a
    // catalogued external class colors the call, so the call is the boundary.
    const SRC: &str = "<?php\n#[\\Steins\\Pure]\nfunction f(PDO $db): int { (new PDO('sqlite::memory:'))->exec('x'); return 1; }\n";
    assert_eq!(effects(SRC, EffectsPolicy::none()).len(), 1);
    let policy = EffectsPolicy::new(
        vec!["telemetry".to_owned()],
        vec![("PDO::exec".to_owned(), vec!["telemetry".to_owned()])],
    );
    assert_eq!(effects(SRC, policy).len(), 0);
}

// ---------------------------------------------------------------------------
// Attribution is inert on its own; tolerance is inert without a match.
// ---------------------------------------------------------------------------

#[test]
fn attribution_without_tolerance_discharges_nothing() {
    // Attribution is fact, not policy. A plugin (or a person) may state what a
    // symbol's effects are for without thereby silencing anything (ADR-0084's
    // "not a plugin power").
    let inert = EffectsPolicy::new(Vec::new(), attribution(&["Logger"]));
    assert_eq!(effects(FACADE, inert).len(), 2, "the facts changed no verdict");
}

#[test]
fn tolerating_a_label_nothing_carries_discharges_nothing() {
    // `telemetry` is not a transport label and never appears in the proven lane —
    // no plugin puts a semantic label there today — so tolerating it without an
    // attribution table is a policy about nothing.
    assert_eq!(effects(FACADE, tolerate(&["telemetry"])).len(), 2);
}

#[test]
fn emptying_the_tolerance_restores_every_finding() {
    // The `--no-tolerated-effects` audit switch: the unconcealed world on demand.
    let audited = telemetry(&["Logger"]).without_tolerance();
    assert_eq!(effects(FACADE, audited).len(), 2);
}

// ---------------------------------------------------------------------------
// Both envelope strata (ADR-0082), and the Liskov conjunction beside them.
// ---------------------------------------------------------------------------

#[test]
fn an_interop_envelope_discharges_the_same_way_the_attribute_does() {
    // Discharge is a property of the judgment, not of the spelling: upstream's
    // `@phpstan-pure` is held to the same bound by the same rule.
    const INTEROP: &str = "<?php\nclass Logger {\n    public static function debug(string $m): void { fwrite(STDERR, $m . time()); }\n}\n/** @phpstan-pure */\nfunction f(string $s): int { Logger::debug($s); return 1; }\n";
    let bare = effects(INTEROP, EffectsPolicy::none());
    assert_eq!(bare.len(), 2, "{bare:#?}");
    assert!(bare[0].message.contains("@phpstan-pure"), "the interop stratum: {}", bare[0].message);
    assert_eq!(effects(INTEROP, telemetry(&["Logger"])).len(), 0);
}

#[test]
fn a_tolerated_label_does_not_widen_an_abstraction() {
    // `effect.liskov-widened` judges the same proven set against the abstraction's
    // bound; the subtraction is on the proven side and the conjunction on the
    // declared side, so a discharged effect cannot widen anything.
    const IMPL: &str = "<?php\ninterface Reader {\n    #[\\Steins\\Pure]\n    public function read(): string;\n}\nclass FileReader implements Reader {\n    public function read(): string { echo 'x'; return 'y'; }\n}\n";
    let bare: Vec<Diagnostic> =
        check(IMPL, EffectsPolicy::none()).into_iter().filter(|d| d.id == EFFECT_LISKOV_ID).collect();
    assert_eq!(bare.len(), 1, "{bare:#?}");
    let tolerated: Vec<Diagnostic> = check(IMPL, tolerate(&["io.output"]))
        .into_iter()
        .filter(|d| d.id == EFFECT_LISKOV_ID)
        .collect();
    assert!(tolerated.is_empty(), "{tolerated:#?}");
}

/// One effect origin, reached by an implementation both through an attributed
/// facade and by a bare call — the residue leg 2's `every` is written for. The
/// two arrivals share label, origin, line and path, so they are one finding
/// group whose members differ only in attribution.
fn shared_origin(also_direct: bool) -> String {
    let body = if also_direct { "Logger::debug() + clock()" } else { "Logger::debug()" };
    format!(
        "<?php\nfunction clock(): int {{ return time(); }}\nclass Logger {{\n    public static function debug(): int {{ return clock(); }}\n}}\ninterface Reader {{\n    #[\\Steins\\Pure]\n    public function read(): int;\n}}\nclass Impl implements Reader {{\n    public function read(): int {{ return {body}; }}\n}}\n"
    )
}

#[test]
fn a_group_reached_by_an_unattributed_path_too_is_discharged_for_neither() {
    let liskov = |src: &str, policy: EffectsPolicy| -> Vec<Diagnostic> {
        check(src, policy).into_iter().filter(|d| d.id == EFFECT_LISKOV_ID).collect()
    };
    // Only the facade reaches the clock: one group, one member, attributed.
    assert_eq!(liskov(&shared_origin(false), EffectsPolicy::none()).len(), 1, "the fixture bites");
    assert!(liskov(&shared_origin(false), telemetry(&["Logger"])).is_empty());
    // The same clock read now also arrives bare. Both copies sit in the
    // implementation's own set under one key, and one of them carries no
    // attribution — so the group survives and the widening is reported.
    let kept = liskov(&shared_origin(true), telemetry(&["Logger"]));
    assert_eq!(kept.len(), 1, "{kept:#?}");
    assert!(kept[0].message.contains("nondet.time"), "{}", kept[0].message);
}

// ---------------------------------------------------------------------------
// The purity oracle reads the same rule.
// ---------------------------------------------------------------------------

/// A `pure-callable` obligation and a closure that routes its one effect through
/// an attributed facade.
const CALLBACK: &str = "<?php\nclass Logger {\n    public static function debug(string $m): void { fwrite(STDERR, $m . time()); }\n}\n/** @param pure-callable $cb */\nfunction takes($cb): void {}\ntakes(static function (string $s): string { Logger::debug($s); return $s; });\n";

#[test]
fn a_callback_whose_only_effect_is_discharged_satisfies_pure_callable() {
    // Otherwise a purity query and an envelope judgment would disagree about one
    // function (ADR-0084 §3).
    let mismatches = |policy: EffectsPolicy| -> Vec<Diagnostic> {
        check(CALLBACK, policy).into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).collect()
    };
    assert_eq!(mismatches(EffectsPolicy::none()).len(), 1, "provably impure without a policy");
    assert!(mismatches(telemetry(&["Logger"])).is_empty(), "accepted under the policy");
}

// ---------------------------------------------------------------------------
// Config validation (ADR-0084 §5).
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_tolerated_label_is_named_with_the_nearest_suggestion() {
    let notices =
        tolerate(&["io.netw"]).label_notices(PluginFacts::none().registry());
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("io.netw"), "{}", notices[0]);
    assert!(notices[0].contains("did you mean 'io.net'"), "{}", notices[0]);
}

#[test]
fn a_label_the_attribution_table_introduces_validates_clean() {
    // `telemetry` is in no builtin taxonomy and no plugin registered it. It is
    // known because the project declared it — by writing it in the attribution
    // table, which is what makes that table a vocabulary as well as a fact.
    let policy = telemetry(&["Logger"]);
    assert!(policy.label_notices(PluginFacts::none().registry()).is_empty());
    // Without the attribution row there is nothing declaring it, and the same
    // spelling is unknown.
    assert_eq!(tolerate(&["telemetry"]).label_notices(PluginFacts::none().registry()).len(), 1);
}
