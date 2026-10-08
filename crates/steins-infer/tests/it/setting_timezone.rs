//! The time family in the effect lane (ADR-0101 §3.14, issue #1000, S6b-1): the functions that
//! format or build a local time read the timezone cell on every call and the clock only where
//! the timestamp is left out; the `gm*` spellings read the clock alone; a literal integer
//! timestamp drops the clock and nothing else; a timestamp the scan cannot read keeps the clock,
//! and no call is a `value-dependent-read` gap.
//!
//! The witness rows of `steins-catalog`'s `setting_timezone_oracle` decide each verdict against
//! PHP; these tests pin that the call site reaches it through every call shape.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

const ZONE: &str = "global.read.setting.timezone";
const ZONE_WRITE: &str = "global.write.setting.timezone";
const LOCALE: &str = "global.read.setting.locale";
const CLOCK: &str = "nondet.time";

fn summary(params: &str, body: &str) -> EffectSummary {
    let src = format!("<?php\nfunction f({params}) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == "f")
        .unwrap_or_else(|| panic!("no summary for f in {src}"))
}

/// The call carries exactly `labels` (sorted as the summary sorts them) and the body stays
/// exhaustive, with no gap.
fn proves(params: &str, call: &str, labels: &[&str]) {
    let s = summary(params, &format!("return {call};"));
    assert_eq!(s.labels, labels, "{call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// A literal integer timestamp drops the clock and keeps the zone; an omitted one and a `null`
/// keep both.
#[test]
fn a_literal_timestamp_drops_the_clock_and_keeps_the_zone() {
    for (supplied, omitted) in [
        ("date('Y', 0)", "date('Y')"),
        ("idate('Y', 0)", "idate('Y')"),
        ("strtotime('+1 day', 0)", "strtotime('+1 day')"),
        ("getdate(0)", "getdate()"),
        ("localtime(0)", "localtime()"),
        ("localtime(0, true)", "localtime(null, true)"),
    ] {
        proves("", supplied, &[ZONE]);
        proves("", omitted, &[ZONE, CLOCK]);
    }
    proves("", "date('Y', null)", &[ZONE, CLOCK]);
    proves("", "date('Y', -86400)", &[ZONE]);
    // A constant the scan evaluates is an integer too (positions 1 to 3).
    proves("", "date('Y', PHP_INT_MAX)", &[ZONE]);
    proves("", "getdate(null)", &[ZONE, CLOCK]);
    proves("", "strtotime('now', null)", &[ZONE, CLOCK]);
}

/// The `gm*` spellings read UTC: no zone, and the clock only without the timestamp. `gmmktime`
/// needs all six fields.
#[test]
fn the_gm_spellings_read_the_clock_alone() {
    proves("", "gmdate('Y', 0)", &[]);
    proves("", "gmdate('Y')", &[CLOCK]);
    proves("", "gmdate('Y', null)", &[CLOCK]);
    proves("", "gmmktime(0, 0, 0, 1, 1, 2020)", &[]);
    proves("", "gmmktime(0)", &[CLOCK]);
    proves("", "gmmktime(0, 0, 0, 1, 1)", &[CLOCK]);
    proves("", "gmmktime(0, 0, 0, 1, 1, null)", &[CLOCK]);
}

/// `mktime` reads the zone and keeps the clock at every arity: the seed's DST flag survives into
/// the repeated hour of a fall-back transition.
#[test]
fn mktime_keeps_the_clock_at_every_arity() {
    proves("", "mktime(0, 0, 0, 1, 1, 2020)", &[ZONE, CLOCK]);
    proves("", "mktime(12)", &[ZONE, CLOCK]);
}

/// `strftime` reads the zone beside S4's locale verdict, which the format decides; `gmstrftime`
/// reads no zone.
#[test]
fn strftime_adds_the_zone_to_the_locale_verdict() {
    proves("", "strftime('%Y', 0)", &[ZONE]);
    proves("", "strftime('%A', 0)", &[LOCALE, ZONE]);
    proves("", "strftime('%Y')", &[ZONE, CLOCK]);
    proves("", "gmstrftime('%Y', 0)", &[]);
    proves("", "gmstrftime('%A', 0)", &[LOCALE]);
    proves("", "gmstrftime('%A')", &[LOCALE, CLOCK]);
}

/// A timestamp the scan cannot read keeps the clock: a variable, an expression, a constant,
/// a float or string literal, and a named or spread argument list. None of these is a gap.
#[test]
fn an_unreadable_timestamp_keeps_the_clock_and_is_no_gap() {
    for call in [
        "date('Y', $ts)",
        "date('Y', $ts + 1)",
        "date('Y', 1.5)",
        "date('Y', '0')",
    ] {
        proves("int $ts, array $a", call, &[ZONE, CLOCK]);
    }
    // A named or spread list has no positions to read: the labels stay, beside the reach gap
    // those argument lists already carry.
    for call in [
        "date('Y', timestamp: 0)",
        "date(...$a)",
        "date('Y', ...$a)",
        "date(format: 'Y', timestamp: 0)",
    ] {
        let s = summary("array $a", &format!("return {call};"));
        assert_eq!(s.labels, [ZONE, CLOCK], "{call}: {s:?}");
        assert!(!s.gaps.contains(&"value-dependent-read"), "{call}: {s:?}");
    }
    proves("int $ts", "gmdate('Y', $ts)", &[CLOCK]);
    proves("int $ts", "getdate($ts)", &[ZONE, CLOCK]);
    proves("int $ts", "gmmktime(0, 0, 0, 1, 1, $ts)", &[CLOCK]);
}

/// The zone accessors, the clock readers and `checkdate`.
#[test]
fn the_accessors_and_the_unconditional_clock_readers() {
    proves("", "date_default_timezone_get()", &[ZONE]);
    let s = summary("", "date_default_timezone_set('Asia/Tokyo');");
    assert_eq!(s.labels, [ZONE_WRITE], "{s:?}");
    proves("", "time()", &[CLOCK]);
    proves("", "microtime(true)", &[CLOCK]);
    // The constructors' function spellings keep the argument-blind clock until S6b-2.
    proves("", "date_create('2020-01-01')", &[CLOCK]);
    proves("", "date_create('@0')", &[CLOCK]);
}

/// A function of the project's own namespace that shares a time-family name is that function
/// and not the builtin, so it carries neither the zone nor the clock.
#[test]
fn a_namespaced_twin_is_not_the_builtin() {
    let src = "<?php\nnamespace A;\nfunction date(string $f, int $t): string { return ''; }\n\
               function f() { return date('Y', 0); }\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    let s = effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol.ends_with('f'))
        .expect("summary for f");
    assert!(!s.labels.iter().any(|l| l == ZONE || l == CLOCK), "{s:?}");
}
