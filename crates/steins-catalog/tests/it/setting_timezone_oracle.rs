//! The S6b-1 witness table of ADR-0101 §3.14 against the engine: each time-family function the
//! catalog colours as a timezone reader moves when the timezone cell does, each one it leaves
//! alone does not, and each clock verdict of a call is checked against the engine's clock
//! (issue #1000).
//!
//! The table has two halves, because the row is two labels with two causes.
//!
//! * **The zone half.** A row is a PHP expression that returns a string and a `setup` that writes
//!   the cell: the expression runs twice, in a process whose `date.timezone` is `UTC` and again
//!   after the setup (`date_default_timezone_set('Asia/Tokyo')`, or the `ini_set` the catalog
//!   colours as the same write), and the outputs are compared. The catalog's verdict is read both
//!   ways, as the other oracles do: a probe that moved is never a call the row drops
//!   (**soundness**), and a call the row proves moved, unless php-src reads the zone and the
//!   probe's value does not depend on it (`strtotime('… UTC')`, `strtotime('@0')`: **stable**).
//! * **The clock half.** A row is a PHP expression evaluated twice in one process, 1.1 seconds
//!   apart: a call that reads the clock gives two answers, a call handed its timestamp gives one.
//!   The verdict is [`ClockGate::reads`]; a call it cannot place keeps the label, and `mktime`,
//!   which has no gate, keeps it at every arity (`Kept`).
//!
//! Every function the table probes exists on both PHP minors the CI matrix carries (8.4 and
//! 8.5); `strftime` is deprecated there and `error_reporting` is `0`. Like the other oracles, the
//! test skips loudly without `php` unless `CI` is set, where a missing `php` fails.

use std::process::{Command, Stdio};

use steins_catalog::{GateArg, SettingCell, clock_gate, effect_labels, ini_cell};

use super::locale_oracle::oracle_unavailable;

const ZONE: &str = "global.read.setting.timezone";
const ZONE_WRITE: &str = "global.write.setting.timezone";
const CLOCK: &str = "nondet.time";

/// What the catalog says of the call a zone row probes.
#[derive(Clone, Copy)]
enum Verdict {
    /// The row of this function carries the timezone read.
    Reads(&'static str),
    /// The row of this function does not carry the timezone read.
    NoRead(&'static str),
    /// The setup's call is a write of the cell: the name it spells.
    Writes(&'static str),
    /// The ini name the setup's `ini_set` spells is the cell's.
    IniName(&'static str),
}

/// What the probe is expected to do between the bare run and the run after the setup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Moves {
    Yes,
    No,
    /// php-src reads the zone and the probe's value does not depend on it: the catalog proves the
    /// read and the probe stays still.
    Stable,
}

struct ZoneRow {
    label: &'static str,
    setup: &'static str,
    probe: &'static str,
    verdict: Verdict,
    moves: Moves,
}

const fn zone(
    label: &'static str,
    probe: &'static str,
    verdict: Verdict,
    moves: Moves,
) -> ZoneRow {
    ZoneRow { label, setup: TOKYO, probe, verdict, moves }
}

use Moves::{No, Stable, Yes};
use Verdict::{IniName, NoRead, Reads, Writes};

/// The write every zone reader follows.
const TOKYO: &str = "date_default_timezone_set('Asia/Tokyo');";

const ZONE_ROWS: &[ZoneRow] = &[
    zone("T1 date with a timestamp", "date('Y-m-d H:i:s', 0)", Reads("date"), Yes),
    zone("T1b date without one", "date('P')", Reads("date"), Yes),
    zone("T2 gmdate", "gmdate('Y-m-d H:i:s', 0)", NoRead("gmdate"), No),
    zone("T2b gmdate without a timestamp", "gmdate('P')", NoRead("gmdate"), No),
    zone("T3 mktime", "mktime(0, 0, 0, 1, 1, 2020)", Reads("mktime"), Yes),
    zone("T4 gmmktime", "gmmktime(0, 0, 0, 1, 1, 2020)", NoRead("gmmktime"), No),
    zone("T5 strtotime", "strtotime('2020-01-01 00:00:00')", Reads("strtotime"), Yes),
    zone("T5b strtotime with a base", "strtotime('tomorrow', 0)", Reads("strtotime"), Yes),
    zone(
        "T6 strtotime naming UTC",
        "strtotime('2020-01-01 00:00:00 UTC')",
        Reads("strtotime"),
        Stable,
    ),
    zone("T6b strtotime of an epoch", "strtotime('@0')", Reads("strtotime"), Stable),
    zone("T7 idate", "idate('H', 0)", Reads("idate"), Yes),
    zone("T8 getdate", "json_encode(getdate(0))", Reads("getdate"), Yes),
    zone("T9 localtime", "json_encode(localtime(0, true))", Reads("localtime"), Yes),
    zone("T10 checkdate", "var_export(checkdate(2, 29, 2020), true)", NoRead("checkdate"), No),
    zone(
        "T11 date_default_timezone_get",
        "date_default_timezone_get()",
        Reads("date_default_timezone_get"),
        Yes,
    ),
    zone("T12 strftime", "strftime('%H', 0)", Reads("strftime"), Yes),
    zone("T12b gmstrftime", "gmstrftime('%H', 0)", NoRead("gmstrftime"), No),
    ZoneRow {
        label: "T13 ini_set of date.timezone is the same write",
        setup: "ini_set('date.timezone', 'Asia/Tokyo');",
        probe: "date('Y-m-d H:i:s', 0)",
        verdict: IniName("date.timezone"),
        moves: Yes,
    },
    ZoneRow {
        label: "T14 date_default_timezone_set is the write",
        setup: TOKYO,
        probe: "date_default_timezone_get() . date('H', 0)",
        verdict: Writes("date_default_timezone_set"),
        moves: Yes,
    },
];

/// Whether the catalog proves a read (`Some(true)`) or proves none (`Some(false)`) for a zone
/// row's verdict. A write verdict is checked on its own and answers `Some(true)`.
fn catalog_reads(verdict: Verdict) -> Option<bool> {
    let carries = |name: &str, label: &str| effect_labels(name).is_some_and(|l| l.contains(&label));
    match verdict {
        Reads(name) => {
            assert!(carries(name, ZONE), "{name} has no timezone read");
            Some(true)
        }
        NoRead(name) => {
            assert!(!carries(name, ZONE), "{name} carries the timezone read");
            Some(false)
        }
        Writes(name) => {
            assert!(carries(name, ZONE_WRITE), "{name}");
            Some(true)
        }
        IniName(name) => {
            assert_eq!(ini_cell(name), Some(SettingCell::Timezone), "{name}");
            Some(true)
        }
    }
}

/// The bare ini the table runs under: the zone is `UTC` and no `php.ini` can move a row.
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

fn php_ready() -> bool {
    if Command::new("php").arg("--version").output().is_err() {
        oracle_unavailable("php is not on PATH");
        return false;
    }
    true
}

/// Every zone row against the engine, in both directions.
#[test]
fn the_timezone_verdicts_match_the_engine() {
    if !php_ready() {
        return;
    }
    for row in ZONE_ROWS {
        let bare = run(&format!("echo {};", row.probe));
        let after = run(&format!("{} echo {};", row.setup, row.probe));
        let moved = bare != after;
        let reads = catalog_reads(row.verdict);
        let label = row.label;
        assert!(reads != Some(false) || !moved, "{label}: moved ({bare:?} -> {after:?}), dropped");
        assert!(
            reads != Some(true) || moved || row.moves == Stable,
            "{label}: proven and did not move ({bare:?})"
        );
        match row.moves {
            Yes => assert!(moved, "{label}: expected to move, stayed {bare:?}"),
            No | Stable => {
                assert!(!moved, "{label}: expected to stay, moved {bare:?} -> {after:?}");
            }
        }
    }
}

/// What the catalog says of a clock row.
#[derive(Clone, Copy)]
enum Clock {
    /// The function's clock gate, handed the arguments the call shows.
    Gate(&'static str, &'static [Option<GateArg<'static>>]),
    /// A time-family function with no gate: the row keeps the clock at every arity.
    Kept(&'static str),
}

struct ClockRow {
    label: &'static str,
    probe: &'static str,
    clock: Clock,
    /// Whether the two evaluations, a second apart, differ.
    moves: bool,
}

const fn clock(
    label: &'static str,
    probe: &'static str,
    clock: Clock,
    moves: bool,
) -> ClockRow {
    ClockRow { label, probe, clock, moves }
}

use Clock::{Gate, Kept};
use GateArg::{Int, Null, Omitted};

/// `$n` is a `null` the script holds: a call handed it is one the catalog cannot place.
const CLOCK_ROWS: &[ClockRow] = &[
    clock("C1 date omitted", "date('s')", Gate("date", &[Some(Omitted)]), true),
    clock("C1b date null", "date('s', null)", Gate("date", &[Some(Null)]), true),
    clock("C1c date supplied", "date('s', 0)", Gate("date", &[Some(Int(0))]), false),
    clock("C1d date from a variable", "date('s', $n)", Gate("date", &[None]), true),
    clock("C2 gmdate omitted", "gmdate('s')", Gate("gmdate", &[Some(Omitted)]), true),
    clock("C2b gmdate supplied", "gmdate('s', 0)", Gate("gmdate", &[Some(Int(0))]), false),
    clock("C3 idate omitted", "idate('s')", Gate("idate", &[Some(Omitted)]), true),
    clock("C3b idate supplied", "idate('s', 0)", Gate("idate", &[Some(Int(0))]), false),
    clock(
        "C4 strtotime without a base",
        "strtotime('+1 day')",
        Gate("strtotime", &[Some(Omitted)]),
        true,
    ),
    clock(
        "C4b strtotime null base",
        "strtotime('+1 day', null)",
        Gate("strtotime", &[Some(Null)]),
        true,
    ),
    clock(
        "C4c strtotime with a base",
        "strtotime('+1 day', 0)",
        Gate("strtotime", &[Some(Int(0))]),
        false,
    ),
    clock(
        "C4d strtotime of a full date",
        "strtotime('2020-01-01 00:00:00', 0)",
        Gate("strtotime", &[Some(Int(0))]),
        false,
    ),
    clock("C5 getdate omitted", "json_encode(getdate())", Gate("getdate", &[Some(Omitted)]), true),
    clock("C5b getdate null", "json_encode(getdate(null))", Gate("getdate", &[Some(Null)]), true),
    clock(
        "C5c getdate supplied",
        "json_encode(getdate(0))",
        Gate("getdate", &[Some(Int(0))]),
        false,
    ),
    clock(
        "C6 localtime omitted",
        "json_encode(localtime())",
        Gate("localtime", &[Some(Omitted)]),
        true,
    ),
    clock(
        "C6b localtime supplied",
        "json_encode(localtime(0))",
        Gate("localtime", &[Some(Int(0))]),
        false,
    ),
    clock("C7 strftime omitted", "strftime('%S')", Gate("strftime", &[Some(Omitted)]), true),
    clock("C7b strftime supplied", "strftime('%S', 0)", Gate("strftime", &[Some(Int(0))]), false),
    clock("C7c gmstrftime omitted", "gmstrftime('%S')", Gate("gmstrftime", &[Some(Omitted)]), true),
    clock(
        "C7d gmstrftime supplied",
        "gmstrftime('%S', 0)",
        Gate("gmstrftime", &[Some(Int(0))]),
        false,
    ),
    clock(
        "C8 gmmktime with the seconds left out",
        "gmmktime(1)",
        Gate(
            "gmmktime",
            &[
                Some(Int(1)),
                Some(Omitted),
                Some(Omitted),
                Some(Omitted),
                Some(Omitted),
                Some(Omitted),
            ],
        ),
        true,
    ),
    clock(
        "C8b gmmktime with a null second",
        "gmmktime(1, 2, null, 4, 5, 2020)",
        Gate(
            "gmmktime",
            &[
                Some(Int(1)),
                Some(Int(2)),
                Some(Null),
                Some(Int(4)),
                Some(Int(5)),
                Some(Int(2020)),
            ],
        ),
        true,
    ),
    clock(
        "C8c gmmktime with all six",
        "gmmktime(1, 2, 3, 4, 5, 2020)",
        Gate(
            "gmmktime",
            &[
                Some(Int(1)),
                Some(Int(2)),
                Some(Int(3)),
                Some(Int(4)),
                Some(Int(5)),
                Some(Int(2020)),
            ],
        ),
        false,
    ),
    clock("C9 mktime with the seconds left out", "mktime(1)", Kept("mktime"), true),
    clock("C9b mktime with all six", "mktime(1, 2, 3, 4, 5, 2020)", Kept("mktime"), false),
];

/// Whether the catalog proves the clock read (`Some(true)`), proves none (`Some(false)`), or
/// leaves the call undecided (`None`, the label kept).
fn catalog_clock(row: &ClockRow) -> Option<bool> {
    match row.clock {
        Gate(name, args) => {
            let gate = clock_gate(name).unwrap_or_else(|| panic!("{name} has no clock gate"));
            assert!(effect_labels(name).is_some_and(|l| l.contains(&CLOCK)), "{name}");
            assert_eq!(gate.positions().len(), args.len(), "{name}");
            gate.reads(args)
        }
        Kept(name) => {
            assert!(clock_gate(name).is_none(), "{name} is gated");
            assert!(effect_labels(name).is_some_and(|l| l.contains(&CLOCK)), "{name}");
            Some(true)
        }
    }
}

/// Every clock row against the engine: one process evaluates every probe, sleeps, and evaluates
/// them again; a probe whose two answers differ read the clock.
#[test]
fn the_clock_verdicts_match_the_engine() {
    if !php_ready() {
        return;
    }
    let probes: Vec<String> =
        CLOCK_ROWS.iter().map(|row| format!("fn() => json_encode({})", row.probe)).collect();
    let script = format!(
        "$n = null; $p = [{}]; \
         $a = array_map(fn($f) => $f(), $p); usleep(1100000); \
         $b = array_map(fn($f) => $f(), $p); \
         foreach ($a as $i => $x) {{ echo $x !== $b[$i] ? '1' : '0'; }}",
        probes.join(", ")
    );
    let out = run(&script);
    assert_eq!(out.len(), CLOCK_ROWS.len(), "one verdict per row: {out:?}");
    for (row, moved) in CLOCK_ROWS.iter().zip(out.chars().map(|c| c == '1')) {
        let reads = catalog_clock(row);
        let label = row.label;
        assert!(reads != Some(false) || !moved, "{label}: moved, and the clock read is dropped");
        assert!(
            reads != Some(true) || moved || matches!(row.clock, Kept(_)),
            "{label}: proven and did not move"
        );
        assert_eq!(moved, row.moves, "{label}: expected moves = {}", row.moves);
    }
}

/// The tables are not vacuous: both verdicts, both outcomes and every kind of row appear.
#[test]
fn the_tables_cover_every_verdict_and_outcome() {
    let reads: Vec<Option<bool>> = ZONE_ROWS.iter().map(|r| catalog_reads(r.verdict)).collect();
    assert!(reads.contains(&Some(true)) && reads.contains(&Some(false)));
    assert!(ZONE_ROWS.iter().any(|r| matches!(r.verdict, Writes(_))));
    assert!(ZONE_ROWS.iter().any(|r| matches!(r.verdict, IniName(_))));
    assert!(ZONE_ROWS.iter().any(|r| r.moves == Stable));
    let clocks: Vec<Option<bool>> = CLOCK_ROWS.iter().map(catalog_clock).collect();
    assert!(clocks.contains(&Some(true)) && clocks.contains(&Some(false)));
    assert!(clocks.contains(&None), "a call the gate cannot place is in the table");
    assert!(CLOCK_ROWS.iter().any(|r| matches!(r.clock, Kept(_))));
    assert!(CLOCK_ROWS.iter().any(|r| r.moves) && CLOCK_ROWS.iter().any(|r| !r.moves));
}
