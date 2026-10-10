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

/// A timestamp shown not to be `null` is supplied, whatever produced it: the caller's value is
/// read where it came from (`time()` carries `nondet.time` itself). An `int` parameter, a
/// property declared `int`, arithmetic, a cast, a float or string literal, a call whose declared
/// return excludes `null`, and a conditional whose branches all do, drop the date function's
/// clock and keep its zone.
#[test]
fn a_timestamp_shown_non_null_drops_the_clock() {
    for call in [
        "date('Y', $ts)",
        "date('Y', $ts + 1)",
        "date('Y', (int) $m)",
        "date('Y', 1.5)",
        "date('Y', '0')",
        "date('Y', $m ?? 0)",
        "date('Y', $flag ? $ts : 0)",
        "date('Y', $ts ?: 0)",
        "date('Y', time())",
        "date('Y', strtotime('now'))",
    ] {
        let s = summary("int $ts, bool $flag, mixed $m", &format!("return {call};"));
        assert!(s.labels.iter().any(|l| l == ZONE), "{call}: {s:?}");
        // The clock stays only where `time()` is read in the argument itself.
        let from_time = call.contains("time(");
        assert_eq!(s.labels.iter().any(|l| l == CLOCK), from_time, "{call}: {s:?}");
    }
    proves("int $ts", "date('Y', $ts)", &[ZONE]);
    proves("int $ts", "gmdate('Y', $ts)", &[]);
    proves("int $ts", "idate('Y', $ts)", &[ZONE]);
    proves("int $ts", "getdate($ts)", &[ZONE]);
    proves("int $ts", "localtime($ts)", &[ZONE]);
    proves("int $ts", "strtotime('now', $ts)", &[ZONE]);
    proves("int $ts", "strftime('%Y', $ts)", &[ZONE]);
    proves("int $ts", "gmstrftime('%Y', $ts)", &[]);
    proves("int $ts", "gmmktime(0, 0, 0, 1, 1, $ts)", &[]);
    proves("int $a, int $b", "gmmktime($a, 0, $b, 1, 1, 2020)", &[]);
    // The clock comes from `time()` and not from the date function.
    proves("", "date('Y', time())", &[ZONE, CLOCK]);
    proves("", "gmdate('Y', time())", &[CLOCK]);
    // A property declared `int`.
    let src = "<?php\nclass C { private int $at = 0; function f() { return gmdate('Y', $this->at); } }\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    let s = effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol.ends_with("::f"))
        .expect("summary for C::f");
    assert!(s.labels.is_empty(), "{s:?}");
}

/// A timestamp whose type may include `null`, or is unknown, keeps the clock, and so does one
/// the scan cannot place; none of these is a gap.
#[test]
fn a_timestamp_that_may_be_null_keeps_the_clock_and_is_no_gap() {
    for (params, call) in [
        ("?int $ts", "date('Y', $ts)"),
        ("int|null $ts", "date('Y', $ts)"),
        ("int $ts = null", "date('Y', $ts)"),
        ("mixed $ts", "date('Y', $ts)"),
        ("$ts", "date('Y', $ts)"),
        ("int $ts", "date('Y', null)"),
        ("int $ts", "date('Y', $ts ?? null)"),
        ("?int $ts", "date('Y', $ts ?: null)"),
        ("bool $f, ?int $ts", "date('Y', $f ? $ts : 0)"),
        ("?int $ts", "gmdate('Y', $ts)"),
        ("int $ts", "gmdate('Y', $undefined)"),
        ("int $ts", "gmdate('Y', $ts = null)"),
        ("int $ts", "gmdate('Y', unknown_function())"),
        ("int $ts", "gmmktime(0, 0, 0, 1, 1, null)"),
        ("?int $y", "gmmktime(0, 0, 0, 1, 1, $y)"),
    ] {
        let s = summary(params, &format!("return {call};"));
        assert!(s.labels.iter().any(|l| l == CLOCK), "{params} / {call}: {s:?}");
        assert!(!s.gaps.contains(&"value-dependent-read"), "{params} / {call}: {s:?}");
    }
    // A parameter the frame rebinds is not the value its declaration holds.
    let s = summary("int $ts", "$ts = null; return date('Y', $ts);");
    assert!(s.labels.iter().any(|l| l == CLOCK), "{s:?}");
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
}

/// A default PHP folds to `null` makes the parameter implicitly nullable, whatever its spelling;
/// only a plain scalar literal default shows it non-`null`. `has_null_default` sees the bare
/// `null` alone, so the other spellings must leave the parameter unplaced.
#[test]
fn a_null_default_in_any_spelling_keeps_the_clock() {
    for param in [
        "int $ts = \\null",
        "int $ts = \\NULL",
        "int $ts = null ?? null",
        "int $ts = true ? null : 0",
        "int $ts = [null][0]",
    ] {
        let s = summary(param, "return date('Y', $ts);");
        assert!(s.labels.iter().any(|l| l == CLOCK), "{param}: {s:?}");
        assert!(!s.gaps.contains(&"value-dependent-read"), "{param}: {s:?}");
    }
    // The control: a non-null literal default is supplied, and a parameter with none.
    proves("int $ts = 0", "date('Y', $ts)", &[ZONE]);
    proves("string $ts = '0'", "date('Y', $ts)", &[ZONE]);
    proves("int $ts", "date('Y', $ts)", &[ZONE]);
}

/// The zone accessors, the clock readers and `checkdate`.
#[test]
fn the_accessors_and_the_unconditional_clock_readers() {
    proves("", "date_default_timezone_get()", &[ZONE]);
    let s = summary("", "date_default_timezone_set('Asia/Tokyo');");
    assert_eq!(s.labels, [ZONE_WRITE], "{s:?}");
    proves("", "time()", &[CLOCK]);
    proves("", "microtime(true)", &[CLOCK]);
    // The constructors' function spellings decide both reads by their string (S6b-2, §3.17).
    proves("", "date_create('2020-01-01')", &[ZONE]);
    proves("", "date_create('@0')", &[]);
    proves("", "date_create()", &[ZONE, CLOCK]);
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
