//! The S6a witness table of ADR-0101 §3.15 against the engine: each float renderer the catalog
//! gates moves when the entry it reads moves, each value it shows to hold no float does not, and
//! the functions and operators it leaves alone are checked in the same pass (issue #1000).
//!
//! A row is a PHP statement list that echoes its result. Every row runs three times in one
//! process each: under the bare ini, after `ini_set('precision', '3')`, and after
//! `ini_set('serialize_precision', '3')`, and the three outputs are compared. The row says which
//! entry the probe must move (`precision`, `serialize_precision` or neither, **exactly**: a row
//! that moved the other entry too would be a reader the table has wrong), and the catalog's
//! verdict is read both ways, as the other oracles do: a call the gate proves (`Some(true)`) must
//! have moved an entry, a call it drops (`Some(false)`) must have moved neither, and a call it
//! leaves undecided is checked for nothing but the movement the row records. Both entries are the
//! one cell (`global.read.setting.precision`, registered by S3), so `ini_set` of either name is
//! its write.
//!
//! The probe value is `1234.5678`, which prints differently at `3` digits (`1.23E+3`) under both
//! entries. Every function the table probes exists on both PHP minors the CI matrix carries (8.4
//! and 8.5), and the two agreed on every row. Like the other oracles, the test skips loudly
//! without `php` unless `CI` is set, where a missing `php` fails.

use std::process::{Command, Stdio};

use steins_catalog::{
    GateArg, RenderDepth, Rendered, SettingCell, effect_labels, ini_cell, precision_gate,
};

use super::locale_oracle::oracle_unavailable;

const PRECISION: &str = "global.read.setting.precision";

/// What the catalog says of the call a row probes.
enum Verdict {
    /// The gate of `name`, handed what the call shows of its `arity` positional arguments.
    Gate {
        name: &'static str,
        arity: usize,
        values: &'static [Option<Rendered>],
        extras: &'static [Option<GateArg<'static>>],
    },
    /// A function with no gate whose row carries no precision read: `number_format`, `round`.
    NoGate(&'static str),
    /// An operator site: no call, no label (D4). The row records that the entry does move.
    Operator,
}

/// Which entry the probe is expected to move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Moves {
    Neither,
    Precision,
    Serialize,
}

struct Row {
    label: &'static str,
    probe: &'static str,
    verdict: Verdict,
    moves: Moves,
}

use GateArg::{Int, Omitted, Str};
use Moves::{Neither, Precision, Serialize};
use Rendered::{Float, NoFloat};

const fn gate(
    label: &'static str,
    probe: &'static str,
    (name, arity): (&'static str, usize),
    (values, extras): (
        &'static [Option<Rendered>],
        &'static [Option<GateArg<'static>>],
    ),
    moves: Moves,
) -> Row {
    Row { label, probe, verdict: Verdict::Gate { name, arity, values, extras }, moves }
}

const NO_FLAGS: &[Option<GateArg<'static>>] = &[Some(Omitted), Some(Omitted)];

/// `$f` is `1234.5678`, `$o` an object holding it in a property.
#[rustfmt::skip]
const ROWS: &[Row] = &[
    // The `precision` ini: the conversions that follow `(string) $f`.
    gate("Q1 strval of a float", "echo strval($f);", ("strval", 1), (&[Some(Float)], &[]), Precision),
    gate("Q2 strval of an int", "echo strval(42);", ("strval", 1), (&[Some(NoFloat)], &[]), Neither),
    gate("Q2b strval of an array", "echo strval([$f]);", ("strval", 1), (&[Some(NoFloat)], &[]), Neither),
    Row { label: "Q3 the string cast", probe: "echo (string) $f;", verdict: Verdict::Operator, moves: Precision },
    Row { label: "Q3b interpolation", probe: "echo \"x{$f}\";", verdict: Verdict::Operator, moves: Precision },
    Row { label: "Q3c concatenation", probe: "echo 'x' . $f;", verdict: Verdict::Operator, moves: Precision },
    Row { label: "Q3d echo", probe: "echo $f;", verdict: Verdict::Operator, moves: Precision },
    gate("Q4 implode with a float element", "echo implode(',', [1, $f]);", ("implode", 2), (&[Some(NoFloat), Some(Float)], &[]), Precision),
    gate("Q4b implode of one array", "echo implode([1, $f]);", ("implode", 1), (&[Some(Float)], &[]), Precision),
    gate("Q4c implode with a float separator", "echo implode($f, ['a', 'b']);", ("implode", 2), (&[Some(Float), Some(NoFloat)], &[]), Precision),
    gate("Q4d join is implode", "echo join(',', [$f]);", ("join", 2), (&[Some(NoFloat), Some(Float)], &[]), Precision),
    // `implode` renders the elements one level: an array element is `"Array"`.
    gate("Q4e implode does not walk into an element", "echo implode(',', [1, [$f]]);", ("implode", 2), (&[Some(NoFloat), Some(NoFloat)], &[]), Neither),
    gate("Q5 implode of ints and strings", "echo implode(',', [1, 'a']);", ("implode", 2), (&[Some(NoFloat), Some(NoFloat)], &[]), Neither),
    gate("Q6 print_r of a float", "echo print_r($f, true);", ("print_r", 2), (&[Some(Float)], &[]), Precision),
    gate("Q7 print_r walks", "echo print_r([[$f]], true);", ("print_r", 2), (&[Some(Float)], &[]), Precision),
    gate("Q7b print_r of an object", "echo print_r($o, true);", ("print_r", 2), (&[None], &[]), Precision),
    gate("Q7c print_r of ints and strings", "echo print_r([1, 'a'], true);", ("print_r", 2), (&[Some(NoFloat)], &[]), Neither),
    gate("Q8 settype to string", "$x = $f; settype($x, 'string'); echo $x;", ("settype", 2), (&[Some(Float)], &[Some(Str("string"))]), Precision),
    gate("Q8b settype to STRING", "$x = $f; settype($x, 'STRING'); echo $x;", ("settype", 2), (&[Some(Float)], &[Some(Str("STRING"))]), Precision),
    gate("Q8c settype to int", "$x = $f; settype($x, 'int'); echo $x;", ("settype", 2), (&[Some(Float)], &[Some(Str("int"))]), Neither),
    gate("Q8d settype to float", "$x = $f; settype($x, 'float'); echo gettype($x);", ("settype", 2), (&[Some(Float)], &[Some(Str("float"))]), Neither),
    // Certified by S4: neither entry is read.
    Row { label: "Q10 number_format", probe: "echo number_format($f, 2);", verdict: Verdict::NoGate("number_format"), moves: Neither },
    Row { label: "Q11 round", probe: "echo round($f, 2) === 1234.57 ? 'y' : 'n';", verdict: Verdict::NoGate("round"), moves: Neither },
    Row { label: "Q12 intval", probe: "echo intval($f);", verdict: Verdict::NoGate("intval"), moves: Neither },
    // The `serialize_precision` ini: the renderers that must round-trip.
    gate("Q14 var_export of a float", "echo var_export($f, true);", ("var_export", 2), (&[Some(Float)], &[]), Serialize),
    gate("Q15 var_export walks", "echo var_export([[$f]], true);", ("var_export", 2), (&[Some(Float)], &[]), Serialize),
    gate("Q15b var_export of ints and strings", "echo var_export([1, 'a'], true);", ("var_export", 2), (&[Some(NoFloat)], &[]), Neither),
    gate("Q16 json_encode of a float", "echo json_encode($f);", ("json_encode", 1), (&[Some(Float)], NO_FLAGS), Serialize),
    gate("Q17 json_encode walks", "echo json_encode(['a' => [$f]]);", ("json_encode", 1), (&[Some(Float)], NO_FLAGS), Serialize),
    gate("Q18 json_encode of an int", "echo json_encode(1);", ("json_encode", 1), (&[Some(NoFloat)], NO_FLAGS), Neither),
    gate("Q18b json_encode of scalars", "echo json_encode([1, 'a', null, true]);", ("json_encode", 1), (&[Some(NoFloat)], NO_FLAGS), Neither),
    gate("Q18c json_encode with a flag that changes nothing", "echo json_encode(['a' => 1], JSON_PRETTY_PRINT);", ("json_encode", 2), (&[Some(NoFloat)], &[Some(Int(128)), Some(Omitted)]), Neither),
    // `JSON_NUMERIC_CHECK` makes a float of a numeric string.
    gate("Q18d json_encode numeric check of a string", "echo json_encode(['1234.5678'], JSON_NUMERIC_CHECK);", ("json_encode", 2), (&[Some(NoFloat)], &[Some(Int(32)), Some(Omitted)]), Serialize),
    gate("Q18e json_encode numeric check of a float", "echo json_encode([$f], JSON_NUMERIC_CHECK);", ("json_encode", 2), (&[Some(Float)], &[Some(Int(32)), Some(Omitted)]), Serialize),
    // A depth too small for the value ends the walk before it reaches the float.
    gate("Q18f json_encode past its depth", "echo var_export(json_encode([[$f]], 0, 1), true);", ("json_encode", 3), (&[Some(Float)], &[Some(Int(0)), Some(Int(1))]), Neither),
    gate("Q18g json_encode with a depth it fits", "echo json_encode([[$f]], 0, 5);", ("json_encode", 3), (&[Some(Float)], &[Some(Int(0)), Some(Int(5))]), Serialize),
    // A non-finite float is cut by the entry (`-INF` is `-IN` at 3 digits) for six renderers.
    gate("Q18h strval of -INF", "echo strval(-INF);", ("strval", 1), (&[Some(Float)], &[]), Precision),
    gate("Q18i implode of -INF", "echo implode(',', [-INF]);", ("implode", 2), (&[Some(NoFloat), Some(Float)], &[]), Precision),
    gate("Q18j print_r of -INF", "echo print_r(-INF, true);", ("print_r", 2), (&[Some(Float)], &[]), Precision),
    gate("Q18k settype of -INF", "$x = -INF; settype($x, 'string'); echo $x;", ("settype", 2), (&[Some(Float)], &[Some(Str("string"))]), Precision),
    gate("Q18l var_export of -INF", "echo var_export(-INF, true);", ("var_export", 2), (&[Some(Float)], &[]), Serialize),
    gate("Q18m serialize of -INF", "echo serialize(-INF);", ("serialize", 1), (&[Some(Float)], &[]), Serialize),
    // Written whole or refused: no read.
    gate("Q18n json_encode of -INF", "echo var_export(json_encode(-INF), true);", ("json_encode", 1), (&[Some(NoFloat)], NO_FLAGS), Neither),
    gate("Q18o var_dump of -INF", "var_dump(-INF);", ("var_dump", 1), (&[Some(NoFloat)], &[]), Neither),
    gate("Q18p debug_zval_dump of -INF", "debug_zval_dump(-INF);", ("debug_zval_dump", 1), (&[Some(NoFloat)], &[]), Neither),
    gate("Q19 serialize of a float", "echo serialize($f);", ("serialize", 1), (&[Some(Float)], &[]), Serialize),
    gate("Q20 serialize of an object", "echo serialize($o);", ("serialize", 1), (&[None], &[]), Serialize),
    gate("Q20b serialize of ints and strings", "echo serialize([1, 'a']);", ("serialize", 1), (&[Some(NoFloat)], &[]), Neither),
    gate("Q21 var_dump of a float", "var_dump($f);", ("var_dump", 1), (&[Some(Float)], &[]), Serialize),
    gate("Q21b var_dump of a later argument", "var_dump(1, $f);", ("var_dump", 2), (&[Some(NoFloat), Some(Float)], &[]), Serialize),
    gate("Q21c var_dump walks", "var_dump([[$f]]);", ("var_dump", 1), (&[Some(Float)], &[]), Serialize),
    gate("Q21d var_dump of ints and strings", "var_dump(1, 'a');", ("var_dump", 2), (&[Some(NoFloat), Some(NoFloat)], &[]), Neither),
    gate("Q21e debug_zval_dump of a float", "debug_zval_dump($f);", ("debug_zval_dump", 1), (&[Some(Float)], &[]), Serialize),
    gate("Q21f debug_zval_dump walks", "debug_zval_dump([$f]);", ("debug_zval_dump", 1), (&[Some(Float)], &[]), Serialize),
    gate("Q21g debug_zval_dump of an int", "debug_zval_dump(1);", ("debug_zval_dump", 1), (&[Some(NoFloat)], &[]), Neither),
];

impl Row {
    /// Whether the catalog proves the call reads an entry (`Some(true)`), proves it does not
    /// (`Some(false)`), or leaves it to a value the call does not show (`None`).
    fn reads(&self) -> Option<bool> {
        match &self.verdict {
            Verdict::Gate { name, arity, values, extras } => {
                let gate = precision_gate(name).unwrap_or_else(|| panic!("{name} has no gate"));
                assert!(
                    effect_labels(name).is_some_and(|l| l.contains(&PRECISION)),
                    "{name} has no precision read"
                );
                assert_eq!(gate.values(*arity).len(), values.len(), "{}", self.label);
                assert_eq!(gate.extras().len(), extras.len(), "{}", self.label);
                gate.reads(values, extras)
            }
            Verdict::NoGate(name) => {
                assert!(precision_gate(name).is_none(), "{name} is gated");
                assert!(
                    effect_labels(name).is_none_or(|l| !l.contains(&PRECISION)),
                    "{name} carries the precision read"
                );
                Some(false)
            }
            Verdict::Operator => None,
        }
    }
}

fn php_ready() -> bool {
    if Command::new("php").arg("--version").output().is_err() {
        oracle_unavailable("php is not on PATH");
        return false;
    }
    true
}

/// Every row's output under `setup`, in row order.
fn run(setup: &str) -> Vec<String> {
    let probes: Vec<String> = ROWS
        .iter()
        .map(|row| {
            format!("function () use ($f, $o) {{ ob_start(); {} return ob_get_clean(); }}", row.probe)
        })
        .collect();
    let script = format!(
        "$f = 1234.5678; $o = new stdClass; $o->p = 1234.5678; {setup} $rows = [{}]; \
         $out = []; foreach ($rows as $row) {{ $out[] = $row(); }} echo implode(\"\\x1e\", $out);",
        probes.join(", ")
    );
    let out = Command::new("php")
        .args(["-d", "error_reporting=0", "-d", "display_errors=0", "-r", &script])
        .stdout(Stdio::piped())
        .output()
        .expect("run php");
    assert!(out.status.success(), "php failed");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let outputs: Vec<String> = text.split('\x1e').map(str::to_owned).collect();
    assert_eq!(outputs.len(), ROWS.len(), "one output per row");
    outputs
}

/// Every row against the engine, in both directions.
#[test]
fn the_precision_verdicts_match_the_engine() {
    if !php_ready() {
        return;
    }
    assert_eq!(ini_cell("precision"), Some(SettingCell::Precision));
    assert_eq!(ini_cell("serialize_precision"), Some(SettingCell::Precision));
    let bare = run("");
    let precision = run("ini_set('precision', '3');");
    let serialize = run("ini_set('serialize_precision', '3');");
    for (index, row) in ROWS.iter().enumerate() {
        let moved = match (bare[index] != precision[index], bare[index] != serialize[index]) {
            (false, false) => Neither,
            (true, false) => Precision,
            (false, true) => Serialize,
            (true, true) => panic!("{}: moved both entries", row.label),
        };
        assert_eq!(moved, row.moves, "{}: {:?}", row.label, bare[index]);
        let label = row.label;
        match row.reads() {
            Some(false) => assert_eq!(moved, Neither, "{label}: moved, and the read is dropped"),
            Some(true) => assert_ne!(moved, Neither, "{label}: proven and did not move"),
            None => {}
        }
    }
}

/// The table is not vacuous: every verdict, both entries and every kind of row appear.
#[test]
fn the_table_covers_every_verdict_and_entry() {
    let reads: Vec<Option<bool>> = ROWS.iter().map(Row::reads).collect();
    assert!(reads.contains(&Some(true)) && reads.contains(&Some(false)) && reads.contains(&None));
    for moves in [Neither, Precision, Serialize] {
        assert!(ROWS.iter().any(|r| r.moves == moves), "{moves:?}");
    }
    assert!(ROWS.iter().any(|r| matches!(r.verdict, Verdict::NoGate(_))));
    assert!(ROWS.iter().any(|r| matches!(r.verdict, Verdict::Operator)));
    for name in [
        "strval", "settype", "implode", "join", "print_r", "var_export", "json_encode", "serialize",
        "var_dump", "debug_zval_dump",
    ] {
        let probed = |row: &&Row| matches!(&row.verdict, Verdict::Gate { name: n, .. } if *n == name);
        let proven = ROWS.iter().filter(probed).any(|r| r.reads() == Some(true));
        assert!(proven, "{name}: no proven row");
    }
    // A walked name is probed past one level, and `implode` shows that it does not walk.
    assert!(precision_gate("implode").is_some_and(|g| g.values(2)[1].1 == RenderDepth::Elements));
}
