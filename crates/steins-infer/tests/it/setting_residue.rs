//! The residue cell's readers decide their read at the call site (ADR-0101 §3.16, issue #1000,
//! S6e): a bcmath call reads `bcmath.scale` only when its `$scale` is omitted or `null`, a value
//! shown not to be `null` (a literal, a typed parameter) drops the read, and an undecided scale
//! keeps the label as the clock gate does. `bcscale` and `error_reporting` read on every call and
//! write only when given a value; `set_include_path` reads and writes its entry, `set_time_limit`
//! writes `max_execution_time`, and `ini_get_all` reads the parent `global.read.setting`.
//!
//! Each verdict is one that `php` 8.5 shows (`crates/steins-catalog/tests/it/setting_ini_oracle.rs`
//! runs the probes); the tests here pin what the infer pass does with the call.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

const INI: (&str, &str) = ("global.read.setting.ini", "global.write.setting.ini");
const SETTING_READ: &str = "global.read.setting";

fn summary(src: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == "f")
        .unwrap_or_else(|| panic!("no summary for f in {src}"))
}

fn row(signature: &str, body: &str) -> EffectSummary {
    summary(&format!("<?php\nfunction f({signature}) {{ {body} }}\n"))
}

/// The call carries exactly `labels`, and the body stays exhaustive with no gap.
fn proves(signature: &str, call: &str, labels: &[&str]) {
    let s = row(signature, &format!("return {call};"));
    assert_eq!(s.labels, labels, "{call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// The call carries exactly `labels`, whatever else the body leaves open.
fn labels(signature: &str, call: &str, expected: &[&str]) {
    let s = row(signature, &format!("return {call};"));
    assert_eq!(s.labels, expected, "{call}: {s:?}");
}

/// Every bcmath call whose `$scale` decides the read: omitted or `null` reads, a literal does not.
#[test]
fn a_bcmath_scale_reads_the_cell_only_when_omitted_or_null() {
    for call in [
        "bcadd('1', '2')", "bcsub('1', '2')", "bcmul('1', '2')", "bcdiv('1', '3')",
        "bcmod('10', '3')", "bcpow('2', '-1')", "bccomp('1.001', '1')", "bcdivmod('10.5', '3')",
    ] {
        labels("", call, &[INI.0]);
        labels("", &call.replace(')', ", null)"), &[INI.0]);
        proves("", &call.replace(')', ", 2)"), &[]);
        proves("", &call.replace(')', ", '2')"), &[]);
        proves("", &call.replace(')', ", true)"), &[]);
    }
    labels("", "bcsqrt('2')", &[INI.0]);
    proves("", "bcsqrt('2', 5)", &[]);
    labels("", "bcpowmod('4', '13', '497')", &[INI.0]);
    proves("", "bcpowmod('4', '13', '497', 0)", &[]);
    // `bcceil`, `bcfloor` and `bcround` take no scale and read nothing (witnessed): no read label.
    for call in ["bcceil('1.5')", "bcfloor('1.5')", "bcround('1.2345', 2)"] {
        let s = row("", &format!("return {call};"));
        assert!(!s.labels.iter().any(|l| l == INI.0), "{call}: {s:?}");
    }
}

/// A scale shown not to be `null` through a typed parameter drops the read; a mixed one keeps
/// it, with no gap.
#[test]
fn a_typed_scale_drops_the_read_and_an_undecided_one_keeps_it() {
    proves("int $s", "bcadd('1', '2', $s)", &[]);
    proves("int $s", "bcsqrt('2', $s + 1)", &[]);
    labels("mixed $m", "bcadd('1', '2', $m)", &[INI.0]);
    let s = row("mixed $m", "return bcadd('1', '2', $m);");
    assert!(s.gaps.is_empty(), "an undecided scale keeps the label, no gap: {s:?}");
    labels("", "bcadd(num1: '1', num2: '2', scale: 2)", &[INI.0]);
}

/// `bcscale` reads the cell on every call; with a value it writes it too, and a `null` value
/// writes nothing (witnessed: `bcscale(null)` returns the cell and leaves it).
#[test]
fn bcscale_reads_always_and_writes_when_given_a_value() {
    labels("", "bcscale()", &[INI.0]);
    labels("", "bcscale(null)", &[INI.0]);
    proves("", "bcscale(3)", &[INI.0, INI.1]);
    labels("mixed $m", "bcscale($m)", &[INI.0, INI.1]);
    labels("", "bcscale(...[3])", &[INI.0, INI.1]);
}

/// `error_reporting` has the same shape as `bcscale`: the old value is a read whatever it is
/// given, and only a non-`null` value writes.
#[test]
fn error_reporting_reads_always_and_writes_when_given_a_value() {
    labels("", "error_reporting()", &[INI.0]);
    labels("", "error_reporting(null)", &[INI.0]);
    proves("", "error_reporting(32767)", &[INI.0, INI.1]);
    labels("int $l", "error_reporting($l)", &[INI.0, INI.1]);
}

/// The include path is read by `get_include_path`, and `set_include_path` returns the old value
/// it rewrites, so it reads and writes the entry.
#[test]
fn the_include_path_pair_reads_and_writes_the_entry() {
    proves("", "get_include_path()", &[INI.0]);
    proves("", "set_include_path('/x')", &[INI.0, INI.1]);
}

/// `set_time_limit` writes `max_execution_time`, which is the residue cell's entry; it reads
/// nothing (it returns a boolean). `ini_get` and `ini_set` of that name narrow to the cell.
#[test]
fn set_time_limit_writes_the_residue_entry() {
    proves("", "set_time_limit(5)", &[INI.1]);
    proves("", "ini_get('max_execution_time')", &[INI.0]);
    proves("", "ini_set('max_execution_time', '1')", &[INI.0, INI.1]);
}

/// `ini_get_all` lists every entry, `precision` and `date.timezone` among them, so it reads the
/// parent setting label rather than one cell.
#[test]
fn ini_get_all_reads_the_setting_parent() {
    proves("", "ini_get_all('date')", &[SETTING_READ]);
    proves("", "ini_get_all()", &[SETTING_READ]);
}
