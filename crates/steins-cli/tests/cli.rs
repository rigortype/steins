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
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json object");
    // The document is now an object: findings array plus suppression counts.
    assert_eq!(v["suppressed"], 0);
    assert_eq!(v["baselined"], 0);
    let arr = v["findings"].as_array().expect("findings array");
    assert_eq!(arr.len(), 1);
    let d = &arr[0];
    assert_eq!(d["id"], "type.argument-mismatch");
    // ADR-0050 §2: additive per-finding layer field. `type.argument-mismatch` is
    // a proof-layer id.
    assert_eq!(d["layer"], "proof");
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
    // Walking a directory recurses into subdirectories and collects every `.php`
    // file into ONE project (ADR-0009/0015): `render()` is defined in
    // `walk/lib.php` and called (badly) from `walk/sub/main.php`, so the finding
    // only exists because the two files are analyzed together. Exit 1.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/walk");
    let r = run(&["check", dir.to_str().unwrap()]);
    assert_eq!(r.code, 1, "cross-file finding present, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("to render() cannot become int $w"),
        "cross-file finding, got:\n{}",
        r.stdout
    );

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

// ---- annotate (ADR-0020): Rigor-style margin, proven facts only -----------

#[test]
fn annotate_prints_all_fact_kinds_and_exhaustiveness_marker() {
    let r = run(&["annotate", fixture("annotate/annotate.php").to_str().unwrap()]);
    assert_eq!(r.code, 0, "annotate never fails on a readable file, got:\n{}", r.stderr);
    let out = r.stdout;

    // 1. Effects on declaration lines: proven-empty, a proven io write, and the
    //    non-exhaustive `…?` marker for an uncatalogued call.
    assert!(out.contains("function price(): string"), "source reprinted");
    assert!(out.contains("//=> effects: {}"), "proven effect-free body, got:\n{out}");
    assert!(out.contains("//=> effects: {io.fs.write}"), "proven io.fs.write, got:\n{out}");
    assert!(out.contains("//=> effects: {…?}"), "non-exhaustive marker, got:\n{out}");

    // 2. Value facts: a folded builtin, a const-fn return, a plain literal.
    assert!(out.contains(r#"//=> $upper = "XY""#), "folded value, got:\n{out}");
    assert!(out.contains(r#"//=> $named = "abc""#), "const-fn value, got:\n{out}");
    assert!(out.contains("//=> $count = 42"), "literal value, got:\n{out}");

    // 3. Exact-class fact.
    assert!(out.contains("//=> $box: Box (exact)"), "exact class, got:\n{out}");

    // 4. A call line that produced a check diagnostic.
    assert!(out.contains("//=> ✗ type.argument-mismatch"), "finding marker, got:\n{out}");

    // The file is reprinted, never modified: the source lines are all present.
    assert!(out.contains(r#"$upper = strtoupper("xy");"#));
    assert!(out.contains(r#"width("nope");"#));
}

#[test]
fn annotate_no_php_drops_folded_value_keeps_the_rest() {
    let path = fixture("annotate/annotate.php");
    let full = run(&["annotate", path.to_str().unwrap()]);
    assert!(full.stdout.contains(r#"//=> $upper = "XY""#), "folded fact present with PHP");

    let sound = run(&["annotate", "--no-php", path.to_str().unwrap()]);
    assert_eq!(sound.code, 0);
    // The folded fact needs the sidecar — gone under --no-php.
    assert!(!sound.stdout.contains(r#"$upper = "XY""#), "folded fact dropped, got:\n{}", sound.stdout);
    // Everything not requiring folding survives.
    assert!(sound.stdout.contains(r#"//=> $named = "abc""#), "const-fn value stays");
    assert!(sound.stdout.contains("//=> $count = 42"), "literal stays");
    assert!(sound.stdout.contains("//=> $box: Box (exact)"), "exact class stays");
    assert!(sound.stdout.contains("//=> effects: {io.fs.write}"), "effects stay");
    assert!(sound.stdout.contains("//=> ✗ type.argument-mismatch"), "finding stays");
    // The sound-subset posture is surfaced up front.
    assert!(
        sound.stderr.contains("running as sound subset (no PHP sidecar)"),
        "posture notice on stderr, got:\n{}",
        sound.stderr
    );
}

#[test]
fn annotate_errors_politely_on_a_directory() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let r = run(&["annotate", dir.to_str().unwrap()]);
    assert_eq!(r.code, 2, "a directory is a usage error");
    assert!(r.stdout.is_empty(), "no annotation output for a directory");
    assert!(r.stderr.contains("not a directory"), "polite message, got:\n{}", r.stderr);
}

// ---- vendor diagnostics (ADR-0015): off by default -----------------------

#[test]
fn vendor_findings_suppressed_by_default_shown_with_flag() {
    let dir = fixture("vendor_proj");
    let vendor_lib = dir.join("vendor/acme/lib.php").to_string_lossy().into_owned();

    // Default: only the first-party width("abc") finding is reported; the
    // vendor height("xyz") finding is suppressed and summarized. Exit reflects
    // the first-party finding only.
    let def = run(&["check", dir.to_str().unwrap()]);
    assert_eq!(def.code, 1, "first-party finding → exit 1, got:\n{}", def.stdout);
    assert!(def.stdout.contains("to width() cannot become int $w"), "first-party shown, got:\n{}", def.stdout);
    assert!(!def.stdout.contains("to height()"), "vendor finding hidden, got:\n{}", def.stdout);
    assert!(!def.stdout.contains(&vendor_lib), "no vendor path printed, got:\n{}", def.stdout);
    assert!(
        def.stdout.contains("1 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "vendor summary line, got:\n{}",
        def.stdout
    );

    // --vendor-diagnostics: both findings reported, no summary line.
    let show = run(&["check", "--vendor-diagnostics", dir.to_str().unwrap()]);
    assert_eq!(show.code, 1);
    assert!(show.stdout.contains("to width() cannot become int $w"), "first-party shown");
    assert!(show.stdout.contains("to height() cannot become int $h"), "vendor shown, got:\n{}", show.stdout);
    assert!(!show.stdout.contains("in vendor suppressed"), "no summary when shown, got:\n{}", show.stdout);
}

#[test]
fn vendor_suppressed_field_present_in_json() {
    let dir = fixture("vendor_proj");

    // Default JSON: one finding (first-party), vendor_suppressed = 1.
    let def = run(&["check", "--format", "json", dir.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_str(&def.stdout).expect("json object");
    assert_eq!(v["vendor_suppressed"], 1, "got:\n{}", def.stdout);
    let arr = v["findings"].as_array().expect("findings array");
    assert_eq!(arr.len(), 1, "only the first-party finding, got:\n{}", def.stdout);
    assert!(arr[0]["message"].as_str().unwrap().contains("width()"));

    // With the flag: two findings, vendor_suppressed = 0.
    let show = run(&["check", "--vendor-diagnostics", "--format", "json", dir.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_str(&show.stdout).expect("json object");
    assert_eq!(v["vendor_suppressed"], 0, "got:\n{}", show.stdout);
    assert_eq!(v["findings"].as_array().unwrap().len(), 2, "both findings, got:\n{}", show.stdout);
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
