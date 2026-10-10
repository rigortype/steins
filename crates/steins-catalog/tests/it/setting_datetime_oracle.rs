//! The S6b-2 witness table of ADR-0101 §3.17 against the engine: each constructor-side call the
//! catalog reads (`new DateTime`, `new DateTimeImmutable`, `date_create*`, `createFromFormat`)
//! moves with the default zone where the catalog keeps or proves the read and never where it
//! drops it, and moves with the clock where the catalog keeps the clock and never where it drops
//! it (issue #1000).
//!
//! * **The zone half.** One `php` process evaluates every probe under `date.timezone=UTC`, then
//!   again after `date_default_timezone_set('Asia/Tokyo')`, and compares. A probe renders the
//!   result's offset and zone name and its distance from `time()` in minutes, so a value whose
//!   wall clock was copied from the default zone's `now` (`'now GMT'`) moves though its zone name
//!   does not. **Soundness**: a probe that moved is never a call whose zone read the gate drops.
//!   **Precision**: a call the gate proves moved.
//! * **The clock half.** One process evaluates every probe twice, 1.1 seconds apart, as the
//!   S6b-1 oracle does: a call whose clock read the gate drops gives one answer. The DST seed that
//!   keeps the clock on `mktime` is not consulted here (`do_adjust_timezone` reads `tz->dst` only
//!   under `have_zone`); a moved clock (libfaketime, ADR-0101 §3.17) witnessed the fall-back hour
//!   of 2024-10-27 in `Europe/London`, which a second's sleep cannot reach.
//!
//! The rows also pin why the design's lexical blacklist is unsound: `'2020-01-01 est'`, `'… a'`,
//! `'… Japan'`, `'…-05'` and `'… +1'` name zones the blacklist would have proven reads of. Like
//! the other oracles, the test skips loudly without `php` unless `CI` is set.

use std::process::{Command, Stdio};

use steins_catalog::{DateGate, GateArg, date_gate, date_method_gate, effect_labels};

use super::locale_oracle::oracle_unavailable;

const ZONE: &str = "global.read.setting.timezone";
const CLOCK: &str = "nondet.time";

/// Which gate a row's call answers from.
#[derive(Clone, Copy)]
enum Callee {
    /// A builtin function.
    Function(&'static str),
    /// An engine method.
    Method(&'static str, &'static str),
}

impl Callee {
    fn gate(self) -> DateGate {
        match self {
            Self::Function(name) => {
                let row = effect_labels(name);
                assert!(row.is_some_and(|l| l.contains(&ZONE) && l.contains(&CLOCK)), "{name}");
                date_gate(name).unwrap_or_else(|| panic!("{name} has no gate"))
            }
            Self::Method(class, method) => {
                let row = steins_catalog::method_effect_labels(class, method);
                assert!(row.is_some_and(|l| l.contains(&ZONE) && l.contains(&CLOCK)), "{class}");
                date_method_gate(class, method).unwrap_or_else(|| panic!("{class}::{method}"))
            }
        }
    }
}

struct Row {
    label: &'static str,
    /// A PHP expression whose value is a `DateTimeInterface` (or `false`).
    probe: &'static str,
    callee: Callee,
    /// What the call shows of the gate's deciding arguments, in [`DateGate::positions`] order.
    args: &'static [Option<GateArg<'static>>],
    /// Whether the value moves with the default zone.
    zone_moves: bool,
    /// Whether the value moves with the clock within a second.
    clock_moves: bool,
}

const fn row(
    label: &'static str,
    probe: &'static str,
    callee: Callee,
    args: &'static [Option<GateArg<'static>>],
    (zone_moves, clock_moves): (bool, bool),
) -> Row {
    Row { label, probe, callee, args, zone_moves, clock_moves }
}

use Callee::{Function, Method};
use GateArg::{NonNull, Null, Omitted, Str};

const NEW: Callee = Method("DateTime", "__construct");
const NEW_IMMUTABLE: Callee = Method("DateTimeImmutable", "__construct");
const FORMAT: Callee = Method("DateTime", "createFromFormat");

/// `$z` is a `DateTimeZone('Asia/Kolkata')` the script holds, a zone object the call shows passed,
/// and `$s` is `'now'`, a string the call does not show.
const ROWS: &[Row] = &[
    row("D1 now", "new DateTime()", NEW, &[Some(Omitted), Some(Omitted)], (true, true)),
    row(
        "D1b now spelled",
        "new DateTime('now')",
        NEW,
        &[Some(Str("now")), Some(Omitted)],
        (true, true),
    ),
    row("D1c empty", "new DateTime('')", NEW, &[Some(Str("")), Some(Omitted)], (true, true)),
    row(
        "D1d today",
        "new DateTime('today')",
        NEW,
        &[Some(Str("today")), Some(Omitted)],
        (true, false),
    ),
    row(
        "D1e tomorrow",
        "date_create('tomorrow')",
        Function("date_create"),
        &[Some(Str("tomorrow")), Some(Omitted)],
        (true, false),
    ),
    row(
        "D2 a date",
        "new DateTime('2020-01-01')",
        NEW,
        &[Some(Str("2020-01-01")), Some(Omitted)],
        (true, false),
    ),
    row(
        "D2b a date and time",
        "new DateTimeImmutable('2020-01-01 10:00:00')",
        NEW_IMMUTABLE,
        &[Some(Str("2020-01-01 10:00:00")), Some(Omitted)],
        (true, false),
    ),
    row(
        "D2c ISO with a fraction",
        "date_create_immutable('2020-01-01T10:00:00.123456')",
        Function("date_create_immutable"),
        &[Some(Str("2020-01-01T10:00:00.123456")), Some(Omitted)],
        (true, false),
    ),
    row(
        "D2d a null zone",
        "new DateTime('2020-01-01', null)",
        NEW,
        &[Some(Str("2020-01-01")), Some(Null)],
        (true, false),
    ),
    row(
        "D3 an epoch",
        "new DateTime('@0')",
        NEW,
        &[Some(Str("@0")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D3b a negative fraction",
        "new DateTime('@-1.25')",
        NEW,
        &[Some(Str("@-1.25")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D3c an epoch with a zone",
        "new DateTime('@0', $z)",
        NEW,
        &[Some(Str("@0")), Some(NonNull)],
        (false, false),
    ),
    row(
        "D4 UTC named",
        "new DateTime('2024-01-18 00:00 UTC')",
        NEW,
        &[Some(Str("2024-01-18 00:00 UTC")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D4b gmt in lower case",
        "new DateTime('2020-01-01 gmt')",
        NEW,
        &[Some(Str("2020-01-01 gmt")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D4c Z",
        "new DateTime('2020-01-01T10:00:00Z')",
        NEW,
        &[Some(Str("2020-01-01T10:00:00Z")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D4d an offset",
        "new DateTime('2016-01-21T21:11:30.123456+00:00')",
        NEW,
        &[Some(Str("2016-01-21T21:11:30.123456+00:00")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D4e a short offset",
        "new DateTime('2013-03-29T05:13:35-05')",
        NEW,
        &[Some(Str("2013-03-29T05:13:35-05")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D4f a one-digit offset after a space",
        "new DateTime('2013-03-29 05:13:35 +1')",
        NEW,
        &[Some(Str("2013-03-29 05:13:35 +1")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D5 now UTC",
        "new DateTime('now UTC')",
        NEW,
        &[Some(Str("now UTC")), Some(Omitted)],
        (false, true),
    ),
    row(
        "D5b today UTC",
        "new DateTime('today UTC')",
        NEW,
        &[Some(Str("today UTC")), Some(Omitted)],
        (false, false),
    ),
    // A zone that is not an identifier builds `now` in the default zone and copies its wall clock.
    row(
        "D6 now GMT",
        "new DateTime('now GMT')",
        NEW,
        &[Some(Str("now GMT")), Some(Omitted)],
        (true, true),
    ),
    row(
        "D6b now utc",
        "new DateTime('now utc')",
        NEW,
        &[Some(Str("now utc")), Some(Omitted)],
        (true, true),
    ),
    row(
        "D6c now +01:00",
        "new DateTime('now +01:00')",
        NEW,
        &[Some(Str("now +01:00")), Some(Omitted)],
        (true, true),
    ),
    // The blacklist's misses: each names a zone, and the grammar leaves it undecided.
    row(
        "D7 est",
        "new DateTime('2020-01-01 est')",
        NEW,
        &[Some(Str("2020-01-01 est")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D7b a military zone",
        "new DateTime('2020-01-01 10:00 a')",
        NEW,
        &[Some(Str("2020-01-01 10:00 a")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D7c a trailing T",
        "new DateTime('2020-01-01T')",
        NEW,
        &[Some(Str("2020-01-01T")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D7d Japan",
        "new DateTime('2020-01-01 Japan')",
        NEW,
        &[Some(Str("2020-01-01 Japan")), Some(Omitted)],
        (false, false),
    ),
    row(
        "D7e a relative",
        "new DateTime('+1 day')",
        NEW,
        &[Some(Str("+1 day")), Some(Omitted)],
        (true, true),
    ),
    // A zone object, shown passed.
    row(
        "D8 now in a zone",
        "new DateTime('now', $z)",
        NEW,
        &[Some(Str("now")), Some(NonNull)],
        (false, true),
    ),
    row(
        "D8b a date in a zone",
        "date_create('2020-01-01', $z)",
        Function("date_create"),
        &[Some(Str("2020-01-01")), Some(NonNull)],
        (false, false),
    ),
    row(
        "D8c an unknown string in a zone",
        "new DateTime($s, $z)",
        NEW,
        &[None, Some(NonNull)],
        (false, true),
    ),
    // `createFromFormat`: the format's reset decides the clock, the zone object the zone.
    row(
        "F1 no reset",
        "DateTime::createFromFormat('Y-m-d', '2020-01-01')",
        FORMAT,
        &[Some(Str("Y-m-d")), Some(Omitted)],
        (true, true),
    ),
    row(
        "F1b a reset",
        "DateTime::createFromFormat('!Y-m-d', '2020-01-01')",
        FORMAT,
        &[Some(Str("!Y-m-d")), Some(Omitted)],
        (true, false),
    ),
    row(
        "F1c a trailing reset",
        "DateTime::createFromFormat('Y-m-d|', '2020-01-01')",
        FORMAT,
        &[Some(Str("Y-m-d|")), Some(Omitted)],
        (true, false),
    ),
    row(
        "F1d an escaped reset",
        "DateTime::createFromFormat('\\\\!Y-m-d', '!2020-01-01')",
        FORMAT,
        &[Some(Str("\\!Y-m-d")), Some(Omitted)],
        (true, true),
    ),
    row(
        "F2 a zone",
        "DateTime::createFromFormat('!Y-m-d', '2020-01-01', $z)",
        FORMAT,
        &[Some(Str("!Y-m-d")), Some(NonNull)],
        (false, false),
    ),
    row(
        "F2b the function spelling",
        "date_create_from_format('Y-m-d H:i:s', '2020-01-01 10:00:00')",
        Function("date_create_from_format"),
        &[Some(Str("Y-m-d H:i:s")), Some(Omitted)],
        (true, false),
    ),
    // A `!` after a zone conversion clears the parsed zone (the default fills it back in) and keeps
    // the clock's DST flag: undecided, both kept. A second's sleep cannot reach the repeated hour,
    // so the faketime test below witnesses the clock.
    row(
        "F3 a reset after a zone conversion",
        "DateTime::createFromFormat('e !Y-m-d H:i', 'Europe/London 2024-10-27 01:30')",
        FORMAT,
        &[Some(Str("e !Y-m-d H:i")), Some(Omitted)],
        (true, false),
    ),
    // A `|` anywhere does not undo it.
    row(
        "F3b and a trailing reset",
        "DateTime::createFromFormat('e !Y-m-d H:i|', 'Europe/London 2024-10-27 01:30')",
        FORMAT,
        &[Some(Str("e !Y-m-d H:i|")), Some(Omitted)],
        (true, false),
    ),
    row(
        "F3c and a leading reset",
        "DateTime::createFromFormat('|e !Y-m-d H:i', 'Europe/London 2024-10-27 01:30')",
        FORMAT,
        &[Some(Str("|e !Y-m-d H:i")), Some(Omitted)],
        (true, false),
    ),
    // A `|` with no `!` after the zone conversion resets, and the parsed zone stays.
    row(
        "F4 a leading reset before a zone",
        "DateTime::createFromFormat('|e Y-m-d H:i', 'Europe/London 2024-10-27 01:30')",
        FORMAT,
        &[Some(Str("|e Y-m-d H:i")), Some(Omitted)],
        (false, false),
    ),
];

/// The bare ini the table runs under.
const BASE: [&str; 3] = ["error_reporting=0", "display_errors=0", "date.timezone=UTC"];

fn run(script: &str) -> String {
    let mut cmd = Command::new("php");
    for entry in BASE {
        cmd.args(["-d", entry]);
    }
    let out = cmd.args(["-r", script]).stdout(Stdio::piped()).output().expect("run php");
    assert!(out.status.success(), "php failed on {script}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The probes as closures, with `$z` and `$s` in scope and `r()` rendering a result.
fn script(body: &str) -> String {
    let probes: Vec<String> = ROWS.iter().map(|row| format!("fn() => {}", row.probe)).collect();
    format!(
        "$z = new DateTimeZone('Asia/Kolkata'); $s = 'now'; $p = [{}]; {body}",
        probes.join(", ")
    )
}

fn php_ready() -> bool {
    if Command::new("php").arg("--version").output().is_err() {
        oracle_unavailable("php is not on PATH");
        return false;
    }
    true
}

fn verdicts(out: &str) -> Vec<bool> {
    assert_eq!(out.len(), ROWS.len(), "one verdict per row: {out:?}");
    out.chars().map(|c| c == '1').collect()
}

/// Every row's zone verdict against the engine, in both directions.
#[test]
fn the_zone_verdicts_match_the_engine() {
    if !php_ready() {
        return;
    }
    let render = "function r($d, $t) { return $d === false ? 'false' \
                  : $d->format('P e') . ' ' . intdiv($d->getTimestamp() - $t + 86400 * 400000 + 30, 60); }";
    let out = run(&script(&format!(
        "{render} $t = time(); $a = array_map(fn($f) => r($f(), $t), $p); \
         date_default_timezone_set('Asia/Tokyo'); \
         $b = array_map(fn($f) => r($f(), $t), $p); \
         foreach ($a as $i => $x) {{ echo $x !== $b[$i] ? '1' : '0'; }}"
    )));
    for (row, moved) in ROWS.iter().zip(verdicts(&out)) {
        let reads = row.callee.gate().reads_zone(row.args);
        let label = row.label;
        assert!(reads != Some(false) || !moved, "{label}: moved with the zone, and it is dropped");
        assert!(reads != Some(true) || moved, "{label}: proven and did not move");
        assert_eq!(moved, row.zone_moves, "{label}: expected zone_moves = {}", row.zone_moves);
    }
}

/// Every row's clock verdict against the engine: two evaluations a second apart.
#[test]
fn the_clock_verdicts_match_the_engine() {
    if !php_ready() {
        return;
    }
    let render =
        "function r($d) { return $d === false ? 'false' : $d->format('Y-m-d H:i:s.u e'); }";
    let out = run(&script(&format!(
        "{render} $a = array_map(fn($f) => r($f()), $p); usleep(1100000); \
         $b = array_map(fn($f) => r($f()), $p); \
         foreach ($a as $i => $x) {{ echo $x !== $b[$i] ? '1' : '0'; }}"
    )));
    for (row, moved) in ROWS.iter().zip(verdicts(&out)) {
        let reads = row.callee.gate().reads_clock(row.args);
        let label = row.label;
        assert!(reads != Some(false) || !moved, "{label}: moved with the clock, and it is dropped");
        assert_eq!(moved, row.clock_moves, "{label}: expected clock_moves = {}", row.clock_moves);
    }
}

/// The clock rows a second's sleep cannot reach, under `faketime` at two clocks with the default
/// zone `Europe/London`: the repeated hour of 2024-10-27 resolves by the clock's DST flag where a
/// zone conversion precedes the `!` (the gate keeps the clock), and does not where the reset or
/// the literal fills every field (the gate drops it). Skips without `faketime` on `PATH`, which
/// CI does not install.
#[test]
fn the_faketime_clock_rows() {
    const PROBES: [(&str, &str, bool); 9] = [
        (
            "DateTime::createFromFormat('e !Y-m-d H:i', 'Europe/London 2024-10-27 01:30')",
            "e !Y-m-d H:i",
            true,
        ),
        (
            "DateTime::createFromFormat('e !Y-m-d H:i|', 'Europe/London 2024-10-27 01:30')",
            "e !Y-m-d H:i|",
            true,
        ),
        (
            "DateTime::createFromFormat('|e !Y-m-d H:i', 'Europe/London 2024-10-27 01:30')",
            "|e !Y-m-d H:i",
            true,
        ),
        (
            "DateTime::createFromFormat('|e Y-m-d H:i', 'Europe/London 2024-10-27 01:30')",
            "|e Y-m-d H:i",
            false,
        ),
        (
            "DateTime::createFromFormat('!e Y-m-d H:i', 'Europe/London 2024-10-27 01:30')",
            "!e Y-m-d H:i",
            false,
        ),
        (
            "DateTime::createFromFormat('!Y-m-d H:i e', '2024-10-27 01:30 Europe/London')",
            "!Y-m-d H:i e",
            false,
        ),
        (
            "DateTime::createFromFormat('Y-m-d H:i e|', '2024-10-27 01:30 Europe/London')",
            "Y-m-d H:i e|",
            false,
        ),
        ("DateTime::createFromFormat('!Y-m-d H:i', '2024-10-27 01:30')", "!Y-m-d H:i", false),
        ("new DateTime('2024-10-27 01:30')", "", false),
    ];
    if Command::new("faketime").arg("--version").output().is_err() || !php_ready() {
        eprintln!("SKIP: faketime is not on PATH; the faketime clock rows not run");
        return;
    }
    let probes: Vec<&str> = PROBES.iter().map(|(probe, ..)| *probe).collect();
    let script = format!(
        "date_default_timezone_set('Europe/London'); \
         foreach ([{}] as $d) {{ echo $d->getTimestamp(), ' '; }}",
        probes.join(", ")
    );
    let at = |clock: &str| {
        let out = Command::new("faketime")
            .args(["-f", clock, "php", "-d", "error_reporting=0", "-r", &script])
            .output()
            .expect("run faketime");
        assert!(out.status.success(), "faketime php failed at {clock}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.split_whitespace().map(str::to_owned).collect::<Vec<_>>()
    };
    let (july, january) = (at("@2024-07-01 12:00:00"), at("@2024-01-15 12:00:00"));
    assert_eq!(july.len(), PROBES.len(), "{july:?}");
    let gate = FORMAT.gate();
    for (i, (probe, format, moves)) in PROBES.iter().enumerate() {
        let moved = july[i] != january[i];
        assert_eq!(moved, *moves, "{probe}: {} vs {}", july[i], january[i]);
        if !format.is_empty() {
            let reads = gate.reads_clock(&[Some(Str(format)), Some(Omitted)]);
            assert!(reads != Some(false) || !moved, "{probe}: moved, and the clock is dropped");
        }
    }
}

/// The table is not vacuous: every verdict of both halves appears, and both outcomes.
#[test]
fn the_table_covers_every_verdict_and_outcome() {
    let zones: Vec<Option<bool>> =
        ROWS.iter().map(|r| r.callee.gate().reads_zone(r.args)).collect();
    let clocks: Vec<Option<bool>> =
        ROWS.iter().map(|r| r.callee.gate().reads_clock(r.args)).collect();
    for verdicts in [&zones, &clocks] {
        for verdict in [Some(true), Some(false), None] {
            assert!(verdicts.contains(&verdict), "{verdict:?}");
        }
    }
    assert!(ROWS.iter().any(|r| r.zone_moves) && ROWS.iter().any(|r| !r.zone_moves));
    assert!(ROWS.iter().any(|r| r.clock_moves) && ROWS.iter().any(|r| !r.clock_moves));
}
