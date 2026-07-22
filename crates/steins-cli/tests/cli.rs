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
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
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

// ---- PHP-sidecar folding (end-to-end, real `php`) -------------------------
//
// These execute the actual sidecar. `php` is present in this environment; if it
// were not, the folded findings would simply be omitted (sound subset) and the
// asserted-flagged tests would fail loudly — which is the correct signal.

#[test]
fn fold_argument_position_flagged_with_provenance() {
    let r = run(&["check", fixture("fold_arg.php").to_str().unwrap()]);
    assert_eq!(r.code, 1, "folded finding present, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("argument \"abc\" (folded from strtolower(\"ABC\")) to width()"),
        "expected folded provenance, got:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("(coercive mode)"));
}

#[test]
fn fold_assignment_rhs_flagged() {
    let r = run(&["check", fixture("fold_assign.php").to_str().unwrap()]);
    assert_eq!(r.code, 1, "got:\n{}", r.stdout);
    assert!(r.stdout.contains("argument \"XY\""), "got:\n{}", r.stdout);
    assert!(r.stdout.contains("from $w, assigned at line"), "got:\n{}", r.stdout);
}

#[test]
fn fold_nonliteral_inner_arg_is_silent() {
    let r = run(&["check", fixture("fold_nonliteral.php").to_str().unwrap()]);
    assert_eq!(r.code, 0, "non-literal inner arg must not fold, got:\n{}", r.stdout);
    assert!(r.stdout.is_empty());
}

#[test]
fn no_php_omits_folded_but_keeps_direct_and_notes_posture() {
    let path = fixture("fold_mixed.php");
    // Default posture: both the direct `width("abc")` and the folded
    // `width(strtolower("XYZ"))` fire.
    let full = run(&["check", path.to_str().unwrap()]);
    assert_eq!(full.code, 1);
    assert_eq!(full.stdout.lines().count(), 2, "both findings, got:\n{}", full.stdout);
    assert!(full.stdout.contains("folded from strtolower(\"XYZ\")"));

    // `--no-php`: the folded finding is omitted, the direct literal stays, and
    // the sound-subset notice is printed to stderr.
    let sound = run(&["check", "--no-php", path.to_str().unwrap()]);
    assert_eq!(sound.code, 1, "direct finding still fires");
    assert_eq!(sound.stdout.lines().count(), 1, "only the direct finding, got:\n{}", sound.stdout);
    assert!(sound.stdout.contains("argument \"abc\""));
    assert!(!sound.stdout.contains("folded from"), "no folded finding under --no-php");
    assert!(
        sound.stderr.contains("running as sound subset (no PHP sidecar)"),
        "sound-subset notice on stderr, got:\n{}",
        sound.stderr
    );
}

#[test]
fn fold_strval_flagged_in_strict_silent_in_coercive() {
    let strict = run(&["check", fixture("fold_strval_strict.php").to_str().unwrap()]);
    assert_eq!(strict.code, 1, "strval(5)->\"5\" into int is a strict TypeError");
    assert!(strict.stdout.contains("(folded from strval(5))"), "got:\n{}", strict.stdout);
    assert!(strict.stdout.contains("(strict mode)"));

    let coercive = run(&["check", fixture("fold_strval_coercive.php").to_str().unwrap()]);
    assert_eq!(coercive.code, 0, "\"5\" coerces to int in coercive mode, got:\n{}", coercive.stdout);
    assert!(coercive.stdout.is_empty());
}
