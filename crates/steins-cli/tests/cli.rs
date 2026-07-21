//! End-to-end CLI tests: run the real `steins` binary over PHP fixtures.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

struct Run {
    code: i32,
    stdout: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).output().expect("run steins");
    Run { code: out.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&out.stdout).into_owned() }
}

#[test]
fn coercive_fixture_flags_abc_and_null_only() {
    let r = run(&["check", fixture("coercive.php").to_str().unwrap()]);
    assert_eq!(r.code, 1, "findings present → exit 1");
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "exactly abc and null flagged, got:\n{}", r.stdout);
    assert!(r.stdout.contains("argument \"abc\" to width() cannot become int $w"));
    assert!(r.stdout.contains("argument null to width() cannot become int $w"));
    assert!(r.stdout.contains("(coercive mode)"));
    assert!(!r.stdout.contains("width(\"5\")"));
}

#[test]
fn strict_fixture_flags_string_and_float_to_int() {
    let r = run(&["check", fixture("strict.php").to_str().unwrap()]);
    assert_eq!(r.code, 1);
    let lines: Vec<&str> = r.stdout.lines().collect();
    // width("5") and width(5.0) flagged; width(5) and area(5) silent.
    assert_eq!(lines.len(), 2, "got:\n{}", r.stdout);
    assert!(r.stdout.contains("(strict mode)"));
    assert!(r.stdout.contains("cannot become int $w"));
}

#[test]
fn clean_fixtures_exit_zero() {
    for name in ["nullable.php", "nullable_strict.php", "silent.php", "broken.php"] {
        let r = run(&["check", fixture(name).to_str().unwrap()]);
        assert_eq!(r.code, 0, "{name} should be clean, got:\n{}", r.stdout);
        assert!(r.stdout.is_empty(), "{name} produced output:\n{}", r.stdout);
    }
}

#[test]
fn json_format_smoke() {
    let r = run(&["check", "--format", "json", fixture("demo.php").to_str().unwrap()]);
    assert_eq!(r.code, 1);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json array");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    let d = &arr[0];
    assert_eq!(d["id"], "type.argument-mismatch");
    assert_eq!(d["line"], 7);
    assert_eq!(d["column"], 7);
    assert_eq!(d["path"].as_str().unwrap(), fixture("demo.php").to_string_lossy());
    assert_eq!(
        d["message"],
        "argument \"abc\" to width() cannot become int $w — proven TypeError (coercive mode)"
    );
}

#[test]
fn directory_walk_and_unknown_command() {
    // Walking the whole fixtures dir finds all findings; exit 1.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let r = run(&["check", dir.to_str().unwrap()]);
    assert_eq!(r.code, 1);
    assert!(!r.stdout.is_empty());

    let bad = run(&["frobnicate"]);
    assert_eq!(bad.code, 2, "unknown command → exit 2");
}
