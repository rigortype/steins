//! The S6c witness of ADR-0101 §3.12 against the engine: `getenv` reads the process environment
//! block at every arity, `putenv` writes it, and `$_ENV` is a startup copy that no call reaches
//! (issue #1000).
//!
//! The locale oracle asserts movement under a witness locale, which says nothing about the
//! environment block; this one asserts values. Each row runs one probe in `php` and compares its
//! output with what the catalog's rows say: `getenv` in every arity observes a `putenv` made
//! before it, `putenv` without `=` removes the entry, and a write to `$_ENV` never reaches
//! `getenv`, nor does `putenv` reach `$_ENV`.
//!
//! Like the other oracles, the test skips loudly without `php` unless `CI` is set, where a missing
//! `php` fails.

use std::process::{Command, Stdio};

use steins_catalog::{effect_labels, setting_read_gate};

use super::locale_oracle::oracle_unavailable;

const READ: &str = "global.read.setting.env";
const WRITE: &str = "global.write.setting.env";

/// One probe: the name of the call it exercises and the output line it must print.
struct Row {
    label: &'static str,
    expected: &'static str,
}

/// The probe script. `$k` is a name no other code uses; each `echo` is one row, in order.
const SCRIPT: &str = r#"
    $k = 'STEINS_S6C_PROBE';
    putenv("$k=one");
    echo "arity1\t", var_export(getenv($k), true), "\n";
    putenv("$k=two");
    echo "arity1\t", var_export(getenv($k), true), "\n";
    echo "arity0\t", var_export(getenv()[$k] ?? 'absent', true), "\n";
    echo "local\t", var_export(getenv($k, true), true), "\n";
    echo "put\t", var_export(putenv($k), true), "\t", var_export(getenv($k), true), "\n";
    $_ENV[$k] = 'nine';
    echo "env\t", var_export(getenv($k), true), "\t", var_export($_ENV[$k], true), "\n";
    putenv("$k=three");
    echo "snapshot\t", var_export(isset($_ENV[$k]) && $_ENV[$k] === 'three', true), "\n";
"#;

/// The rows, in the order the script prints them; each expected line is what php 8.x prints (the
/// `var_export` spelling of each value).
const ROWS: &[Row] = &[
    Row { label: "arity1", expected: "arity1\t'one'" },
    Row { label: "arity1", expected: "arity1\t'two'" },
    Row { label: "arity0", expected: "arity0\t'two'" },
    Row { label: "local", expected: "local\t'two'" },
    Row { label: "put", expected: "put\ttrue\tfalse" },
    Row { label: "env", expected: "env\tfalse\t'nine'" },
    Row { label: "snapshot", expected: "snapshot\tfalse" },
];

fn run_php(script: &str) -> Option<String> {
    if Command::new("php").arg("--version").output().is_err() {
        oracle_unavailable("php is not on PATH");
        return None;
    }
    let out = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", script])
        .stdout(Stdio::piped())
        .output()
        .expect("run php");
    assert!(out.status.success(), "php failed");
    Some(String::from_utf8(out.stdout).expect("utf8"))
}

/// The catalog's side of the table: `getenv` carries the read at every arity and nothing gates
/// it; `putenv` carries the write and nothing else; neither is a locale read.
#[test]
fn the_catalog_colours_getenv_as_the_env_read_and_putenv_as_its_write() {
    assert_eq!(effect_labels("getenv"), Some(&[READ][..]));
    assert_eq!(effect_labels("putenv"), Some(&[WRITE][..]));
    assert!(setting_read_gate("getenv").is_none(), "getenv is argument-blind");
    assert!(setting_read_gate("putenv").is_none(), "putenv is argument-blind");
}

/// Every probe prints the line the catalog's rows predict: `getenv` observes each write made
/// before it in every arity, `putenv` writes the entry it names, and `$_ENV` stays apart from the
/// environment block in both directions.
#[test]
fn getenv_observes_putenv_and_never_the_superglobal() {
    let Some(text) = run_php(SCRIPT) else { return };
    let observed: Vec<&str> = text.lines().collect();
    assert_eq!(observed.len(), ROWS.len(), "{text}");
    for (row, got) in ROWS.iter().zip(observed) {
        assert_eq!(got, row.expected, "probe {}", row.label);
    }
}
