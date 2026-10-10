//! The S6e witness of ADR-0101 §3.16 against the engine: the residue cell's readers and writers
//! (issue #1000).
//!
//! Each row runs one call in two states of the cell it touches (`bcscale(0)` and `bcscale(3)`, two
//! `include_path`s, two `error_reporting` levels, two `max_execution_time`s) and reports two
//! things: the call's return value, which a read makes differ between the states, and whether the
//! entry the row names changed across the call, which a write makes. The row's expectation is what
//! the catalog's gate decides for the call's literal shape, so a change to either side fails here.
//!
//! The bcmath rows are the `$scale` table of `setting_reads/ini.rs`: an omitted or `null` scale
//! reads, a literal, a string, a boolean does not. `bcceil` reads nothing (no gate, no row).
//!
//! Like the other oracles, the test skips loudly without `php` (or without the `bcmath` extension)
//! unless `CI` is set, where a missing `php` fails.

use std::process::{Command, Stdio};

use steins_catalog::{GateArg, effect_labels, setting_read_gate};

use super::locale_oracle::oracle_unavailable;

/// One call, the two states it runs in, the entry it reads back, and what php shows.
struct Row {
    name: &'static str,
    call: &'static str,
    /// The statements that put the cell in the first state and in the second.
    states: (&'static str, &'static str),
    /// The expression that reads the entry the call touches.
    entry: &'static str,
    reads: bool,
    writes: bool,
    /// The literal shape of the deciding argument, as the gate sees it; `None` for a row no gate
    /// decides.
    shape: Option<GateArg<'static>>,
}

const BC: &str = "ini_get('bcmath.scale')";
const BC_STATES: (&str, &str) = ("bcscale(0);", "bcscale(3);");
const ER: &str = "ini_get('error_reporting')";
const ER_STATES: (&str, &str) = ("error_reporting(E_ALL & ~E_NOTICE);", "error_reporting(E_WARNING);");
const INC: &str = "ini_get('include_path')";
const INC_STATES: (&str, &str) = ("set_include_path('/s6e-a');", "set_include_path('/s6e-b');");
const MET: &str = "ini_get('max_execution_time')";
const MET_STATES: (&str, &str) = ("ini_set('max_execution_time', '1');", "ini_set('max_execution_time', '2');");

/// The probe table. The reads and writes are what php shows; the shapes are what the gate takes.
const ROWS: &[Row] = &[
    Row { name: "bcadd", call: "bcadd('1', '2')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcadd", call: "bcadd('1', '2', null)", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Null) },
    Row { name: "bcadd", call: "bcadd('1', '2', 2)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(2)) },
    Row { name: "bcadd", call: "bcadd('1', '2', 0)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(0)) },
    Row { name: "bcadd", call: "bcadd('1', '2', '2')", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Str("2")) },
    Row { name: "bcadd", call: "bcadd('1', '2', true)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::NotText) },
    Row { name: "bcsub", call: "bcsub('1', '0.5')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcsub", call: "bcsub('1', '0.5', 1)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(1)) },
    Row { name: "bcmul", call: "bcmul('1.5', '2')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcdiv", call: "bcdiv('1', '3')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcdiv", call: "bcdiv('1', '3', 4)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(4)) },
    Row { name: "bcmod", call: "bcmod('10', '3')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcmod", call: "bcmod('10', '3', 1)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(1)) },
    Row { name: "bcpow", call: "bcpow('2', '-1')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcpow", call: "bcpow('2', '-1', 3)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(3)) },
    Row { name: "bcsqrt", call: "bcsqrt('2')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcsqrt", call: "bcsqrt('2', null)", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Null) },
    Row { name: "bcsqrt", call: "bcsqrt('2', 5)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(5)) },
    Row { name: "bccomp", call: "bccomp('1.001', '1')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bccomp", call: "bccomp('1.001', '1', 2)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(2)) },
    Row { name: "bcdivmod", call: "bcdivmod('10.5', '3')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcdivmod", call: "bcdivmod('10.5', '3', 1)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(1)) },
    Row { name: "bcpowmod", call: "bcpowmod('4', '13', '497')", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcpowmod", call: "bcpowmod('4', '13', '497', 0)", states: BC_STATES, entry: BC, reads: false, writes: false, shape: Some(GateArg::Int(0)) },
    Row { name: "bcscale", call: "bcscale()", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "bcscale", call: "bcscale(null)", states: BC_STATES, entry: BC, reads: true, writes: false, shape: Some(GateArg::Null) },
    Row { name: "bcscale", call: "bcscale(5)", states: BC_STATES, entry: BC, reads: true, writes: true, shape: Some(GateArg::Int(5)) },
    Row { name: "error_reporting", call: "error_reporting()", states: ER_STATES, entry: ER, reads: true, writes: false, shape: Some(GateArg::Omitted) },
    Row { name: "error_reporting", call: "error_reporting(null)", states: ER_STATES, entry: ER, reads: true, writes: false, shape: Some(GateArg::Null) },
    Row { name: "error_reporting", call: "error_reporting(2)", states: ER_STATES, entry: ER, reads: true, writes: true, shape: Some(GateArg::Int(2)) },
    Row { name: "get_include_path", call: "get_include_path()", states: INC_STATES, entry: INC, reads: true, writes: false, shape: None },
    Row { name: "set_include_path", call: "set_include_path('/s6e-c')", states: INC_STATES, entry: INC, reads: true, writes: true, shape: None },
    Row { name: "set_time_limit", call: "set_time_limit(9)", states: MET_STATES, entry: MET, reads: false, writes: true, shape: None },
];

/// Whether the `bcmath` extension is loaded in the `php` on PATH (`None`: no `php` at all).
fn bcmath_loaded() -> Option<bool> {
    let out = Command::new("php")
        .args(["-r", "echo extension_loaded('bcmath') ? 'y' : 'n';"])
        .stdout(Stdio::piped())
        .output()
        .ok()?;
    Some(out.stdout == b"y")
}

/// The probe of one row: the call's value in each state (as `var_export`), and whether the entry
/// changed across the call in each state.
fn probe(row: &Row) -> String {
    format!(
        "{a}\n$b1 = {e}; $r1 = {c}; $a1 = {e};\n{b}\n$b2 = {e}; $r2 = {c}; $a2 = {e};\n\
         echo var_export($r1, true), \"\\t\", var_export($r2, true), \"\\t\",\n\
         ($b1 === $a1 && $b2 === $a2) ? 'same' : 'changed', \"\\n\";\n",
        a = row.states.0,
        b = row.states.1,
        e = row.entry,
        c = row.call,
    )
}

fn run_php(script: &str) -> Option<String> {
    let out = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", script])
        .stdout(Stdio::piped())
        .output()
        .expect("run php");
    assert!(out.status.success(), "php failed on {script}");
    Some(String::from_utf8(out.stdout).expect("utf8"))
}

/// Each call's observed reads and writes, as php 8.5 shows them, match the gate's verdict for its
/// literal shape, and the names the gate covers are exactly the ones the table probes.
#[test]
fn the_residue_readers_and_writers_match_php() {
    match bcmath_loaded() {
        None => return oracle_unavailable("php is not on PATH"),
        Some(false) => return oracle_unavailable("php has no bcmath extension"),
        Some(true) => {}
    }
    for row in ROWS {
        let Some(text) = run_php(&probe(row)) else { return };
        let mut fields = text.trim_end().split('\t');
        let (r1, r2, changed) = (fields.next(), fields.next(), fields.next());
        let read = r1 != r2;
        let write = changed == Some("changed");
        assert_eq!(read, row.reads, "{}: read observed as {text:?}", row.call);
        assert_eq!(write, row.writes, "{}: write observed as {text:?}", row.call);
        let Some(shape) = row.shape else { continue };
        let gate = setting_read_gate(row.name).unwrap_or_else(|| panic!("{} has no gate", row.name));
        assert_eq!(gate.reads(&[Some(shape)]), Some(row.reads), "{}: gate read", row.call);
        // Only the old-value readers' write is a shape-decided write; a bcmath gate writes nothing.
        if matches!(row.name, "bcscale" | "error_reporting") {
            assert_eq!(gate.writes(&[Some(shape)]), row.writes, "{}: gate write", row.call);
        }
    }
}

/// The catalog colours each probed name with the cell's labels, and `bcceil` (which reads nothing)
/// has no bcmath read.
#[test]
fn the_catalog_rows_name_the_cell_the_probes_touch() {
    let ini_read = "global.read.setting.ini";
    let ini_write = "global.write.setting.ini";
    for name in ["bcadd", "bcscale", "error_reporting", "get_include_path", "set_include_path"] {
        assert!(effect_labels(name).is_some_and(|l| l.contains(&ini_read)), "{name}");
    }
    assert!(effect_labels("set_time_limit").is_some_and(|l| l == [ini_write]));
    assert!(!effect_labels("bcceil").is_some_and(|l| l.contains(&ini_read)), "bcceil reads nothing");
}
