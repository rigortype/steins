//! End-to-end CLI tests: run the real `steins` binary over PHP fixtures.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test spawns the binary with `GITHUB_ACTIONS` scrubbed: `check`'s format
/// auto-detection (ADR-0054 §6) reads it, so a run on CI would otherwise emit workflow
/// commands instead of the asserted text (detection is tested in `tests/format_github.rs`).
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
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
    let out = steins_cmd().args(args).output().expect("run steins");
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
    for name in ["nullable.php", "nullable_strict.php", "silent.php"] {
        let r = run(&["check", fixture(name).to_str().unwrap()]);
        assert_eq!(r.code, 0, "{name} should be clean, got:\n{}", r.stdout);
        assert!(r.stdout.is_empty(), "{name} produced output:\n{}", r.stdout);
    }
}

// parse failure (ADR-0079, issue #180)
#[test]
fn a_file_that_does_not_parse_reports_and_exits_non_zero() {
    // ADR-0079 §2.1 (issue #180): a file `php -l` rejects must fail the run, not
    // silently recover into a clean exit 0 as it used to.
    let r = run(&["check", fixture("broken.php").to_str().unwrap()]);
    assert_eq!(r.code, 1, "a file that does not parse must fail the run:\n{}", r.stdout);
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one finding per broken file, got:\n{}", r.stdout);
    assert!(lines[0].contains("error[syntax.unparsable]"), "{}", lines[0]);
    // Positioned at the FIRST parse error: the missing `)` on line 7 is diagnosed
    // where the parser gives up (line 8), and the message counts the further ones.
    assert!(lines[0].contains("broken.php:8:12:"), "positioned at the first error: {}", lines[0]);
    assert!(lines[0].contains("further parse error"), "{}", lines[0]);
}

#[test]
fn json_format_smoke() {
    let r = run(&["check", "--format", "json", fixture("demo.php").to_str().unwrap()]);
    assert_eq!(r.code, 1);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json object");
    assert_eq!(v["suppressed"], 0);
    assert_eq!(v["baselined"], 0);
    let arr = v["findings"].as_array().expect("findings array");
    assert_eq!(arr.len(), 1);
    let d = &arr[0];
    assert_eq!(d["id"], "type.argument-mismatch");
    // ADR-0050 §2: additive per-finding `layer` field; this id is proof-layer.
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
    // Walking a directory recurses into subdirectories, collecting every `.php` file
    // into ONE project (ADR-0009/0015): the finding only exists because `walk/lib.php`
    // and `walk/sub/main.php` are analyzed together.
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

// PHP-sidecar folding (real `php`, end to end): if `php` were absent, folded
// findings would simply be omitted (sound subset) and these tests would fail loudly.

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
    let full = run(&["check", path.to_str().unwrap()]);
    assert_eq!(full.code, 1);
    assert_eq!(full.stdout.lines().count(), 2, "both findings, got:\n{}", full.stdout);
    assert!(full.stdout.contains("folded from strtolower(\"XYZ\")"));

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
fn array_literals_fold_through_the_untouched_allowlist() {
    // Issue #39's acceptance criterion, end to end: `count`, `in_array` and
    // `implode` fold once the argument gate accepts an array literal.
    let path = fixture("fold_array.php");
    let r = run(&["annotate", path.to_str().unwrap()]);
    assert_eq!(r.code, 0, "annotate never fails on a readable file, got:\n{}", r.stderr);
    let out = r.stdout;

    // The three parked entries, folded on the project's own PHP.
    assert!(out.contains("//=> $n = 3"), "count folded, got:\n{out}");
    assert!(out.contains(r#"//=> $joined = "a,b""#), "implode folded, got:\n{out}");
    assert!(out.contains("//=> $member = true"), "in_array folded, got:\n{out}");
    // Nesting is represented: the outer count is 2, not a widen.
    assert!(out.contains("//=> $nested = 2"), "nested literal folded, got:\n{out}");
    // PHP's own key semantics, because PHP is what builds the array.
    assert!(out.contains("//=> $dup = 1"), "duplicate key is one entry, got:\n{out}");
    assert!(out.contains(r#"//=> $mixed = "a,b,c""#), "mixed keys, got:\n{out}");
    // A non-spread element is always exactly one entry regardless of its value, so
    // `count([1, $x])` is known statically even though $x is unproven (issue #327);
    // the FOLD still declines since its own argument gate is untouched.
    assert!(out.contains("//=> $unfolded = 2"), "the count is known, got:\n{out}");
    // The widening pin lives where it's truly unknowable: a spread contributes as
    // many entries as its subject has.
    assert!(out.contains("$widened = count([1, ...$x]);"), "source reprinted, got:\n{out}");
    assert!(!out.contains("//=> $widened"), "a spread must widen, got:\n{out}");
    // The `$dup` literal's duplicate key is ALSO a genuine `array.duplicate-key`
    // finding (ADR-0078, issue #187), on the same margin line as the fold fact.
    assert!(out.contains("✗ array.duplicate-key"), "the finding margin, got:\n{out}");

    // Under `--no-php` every FOLDED fact goes (ADR-0004: sound subset never invents
    // an unexecuted value) but `array.duplicate-key` needs no PHP (issue #187) and stays.
    let sound = run(&["annotate", "--no-php", path.to_str().unwrap()]);
    assert_eq!(sound.code, 0);
    for needle in [
        "//=> $n = 3",
        r#"//=> $joined = "a,b""#,
        "//=> $member = true",
        "//=> $nested = 2",
        "//=> $dup = 1",
        r#"//=> $mixed = "a,b,c""#,
        // The shape rung goes too: its answer needs the engine's reflected envelope
        // (ADR-0061 §2), and there is no engine here.
        "//=> $unfolded = 2",
    ] {
        assert!(!sound.stdout.contains(needle), "no folded facts without PHP ({needle}), got:\n{}", sound.stdout);
    }
    assert!(
        sound.stdout.contains("//=> ✗ array.duplicate-key"),
        "the duplicate-key finding is PHP-independent, got:\n{}",
        sound.stdout
    );
}

// ---- annotate (ADR-0020): Rigor-style margin, proven facts only -----------

#[test]
fn annotate_prints_all_fact_kinds_and_exhaustiveness_marker() {
    let r = run(&["annotate", fixture("annotate/annotate.php").to_str().unwrap()]);
    assert_eq!(r.code, 0, "annotate never fails on a readable file, got:\n{}", r.stderr);
    let out = r.stdout;

    // 1. Effects: proven-empty, a proven io write, non-exhaustive `…?` marker.
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
    assert!(!sound.stdout.contains(r#"$upper = "XY""#), "folded fact dropped, got:\n{}", sound.stdout);
    assert!(sound.stdout.contains(r#"//=> $named = "abc""#), "const-fn value stays");
    assert!(sound.stdout.contains("//=> $count = 42"), "literal stays");
    assert!(sound.stdout.contains("//=> $box: Box (exact)"), "exact class stays");
    assert!(sound.stdout.contains("//=> effects: {io.fs.write}"), "effects stay");
    assert!(sound.stdout.contains("//=> ✗ type.argument-mismatch"), "finding stays");
    assert!(
        sound.stderr.contains("running as sound subset (no PHP sidecar)"),
        "posture notice on stderr, got:\n{}",
        sound.stderr
    );
}

// ---- annotate --format json (issue #65): machine-readable effect summaries -

#[test]
fn annotate_json_shape_pins_colored_pure_and_tainted_functions() {
    let path = fixture("annotate/annotate.php");
    let r = run(&["annotate", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(r.code, 0, "annotate never fails on a readable file, got:\n{}", r.stderr);
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json object");
    let functions = doc["functions"].as_array().expect("functions array");

    let by_name = |name: &str| -> &serde_json::Value {
        functions.iter().find(|f| f["name"] == name).unwrap_or_else(|| panic!("no `{name}` entry"))
    };

    // A catalogued-pure function: proven-empty effects, exhaustive, name+line.
    let price = by_name("price");
    assert_eq!(price["effects"], serde_json::json!([]));
    assert_eq!(price["declared"], serde_json::json!([]));
    assert_eq!(price["exhaustive"], serde_json::json!(true));
    assert_eq!(price["line"], serde_json::json!(8), "declaration line, got:\n{doc}");

    // A colored function (calls file_put_contents): one proven label, exhaustive.
    let writer = by_name("writer");
    assert_eq!(writer["effects"], serde_json::json!(["io.fs.write"]));
    assert_eq!(writer["declared"], serde_json::json!([]));
    assert_eq!(writer["exhaustive"], serde_json::json!(true));

    // An exhaustiveness-tainted function: the uncatalogued/dynamic call widens to no
    // proven label but flips the exhaustiveness bit (contrast with `price` above).
    let mystery = by_name("mystery");
    assert_eq!(mystery["effects"], serde_json::json!([]));
    assert_eq!(mystery["declared"], serde_json::json!([]));
    assert_eq!(mystery["exhaustive"], serde_json::json!(false));

    // The declared lane (ADR-0067): a call through an interface-typed parameter proves
    // nothing but bounds it in its own `declared` array (never leaking into `effects`),
    // discharging the taint so `exhaustive` stays true.
    let stamp = by_name("stamp");
    assert_eq!(stamp["effects"], serde_json::json!([]));
    assert_eq!(stamp["declared"], serde_json::json!(["nondet.time"]));
    assert_eq!(stamp["exhaustive"], serde_json::json!(true));
}

#[test]
fn annotate_json_is_opt_in_default_format_stays_the_text_margin() {
    let path = fixture("annotate/annotate.php");
    let r = run(&["annotate", path.to_str().unwrap()]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("//=> effects: {io.fs.write}"), "text margin unchanged, got:\n{}", r.stdout);
    // The same declared bound the JSON reports, in the margin's own spelling.
    assert!(
        r.stdout.contains("//=> effects: {≤nondet.time}"),
        "declared lane in the margin, got:\n{}",
        r.stdout
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&r.stdout).is_err(),
        "default output is the text margin, not JSON, got:\n{}",
        r.stdout
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

    // Default: only the first-party finding is reported; the vendor finding is
    // suppressed and summarized, and exit reflects the first-party finding only.
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

    let show = run(&["check", "--vendor-diagnostics", dir.to_str().unwrap()]);
    assert_eq!(show.code, 1);
    assert!(show.stdout.contains("to width() cannot become int $w"), "first-party shown");
    assert!(show.stdout.contains("to height() cannot become int $h"), "vendor shown, got:\n{}", show.stdout);
    assert!(!show.stdout.contains("in vendor suppressed"), "no summary when shown, got:\n{}", show.stdout);
}

#[test]
fn vendor_suppressed_field_present_in_json() {
    let dir = fixture("vendor_proj");

    let def = run(&["check", "--format", "json", dir.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_str(&def.stdout).expect("json object");
    assert_eq!(v["vendor_suppressed"], 1, "got:\n{}", def.stdout);
    let arr = v["findings"].as_array().expect("findings array");
    assert_eq!(arr.len(), 1, "only the first-party finding, got:\n{}", def.stdout);
    assert!(arr[0]["message"].as_str().unwrap().contains("width()"));

    let show = run(&["check", "--vendor-diagnostics", "--format", "json", dir.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_str(&show.stdout).expect("json object");
    assert_eq!(v["vendor_suppressed"], 0, "got:\n{}", show.stdout);
    assert_eq!(v["findings"].as_array().unwrap().len(), 2, "both findings, got:\n{}", show.stdout);
}

// ---- composer-aware vendor-dir resolution (issue #181) --------------------

#[test]
fn a_composer_declared_vendor_dir_is_suppressed_by_default_shown_with_flag() {
    // `composer.json` declares `config.vendor-dir: "3rdparty"` (not literal `vendor`),
    // proving suppression reads Composer's config rather than guessing a dir name.
    let dir = fixture("composer_vendor_dir_proj");
    let vendor_lib = dir.join("3rdparty/acme/lib.php").to_string_lossy().into_owned();

    let def = run(&["check", dir.to_str().unwrap()]);
    assert_eq!(def.code, 1, "first-party finding → exit 1, got:\n{}", def.stdout);
    assert!(def.stdout.contains("to width() cannot become int $w"), "first-party shown, got:\n{}", def.stdout);
    assert!(!def.stdout.contains("to height()"), "3rdparty finding hidden, got:\n{}", def.stdout);
    assert!(!def.stdout.contains(&vendor_lib), "no vendor path printed, got:\n{}", def.stdout);
    assert!(
        def.stdout.contains("1 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "vendor summary line, got:\n{}",
        def.stdout
    );

    let show = run(&["check", "--vendor-diagnostics", dir.to_str().unwrap()]);
    assert_eq!(show.code, 1);
    assert!(show.stdout.contains("to width() cannot become int $w"), "first-party shown");
    assert!(show.stdout.contains("to height() cannot become int $h"), "3rdparty shown, got:\n{}", show.stdout);
    assert!(!show.stdout.contains("in vendor suppressed"), "no summary when shown, got:\n{}", show.stdout);
}

#[test]
fn a_broken_file_under_a_composer_declared_vendor_dir_does_not_dam_the_project() {
    // `3rdparty/pkg/broken.php` fails to parse. If the ADR-0079 dam read a literal
    // `vendor` instead of the same resolved path `check` uses, it would mistreat this as
    // first-party and silence `src/main.php`'s fatal project-wide (§2.2) — the declared
    // `3rdparty` dir carries the ADR-0046 §2 presumption too.
    let dir = fixture("composer_vendor_dir_dam_proj");

    let def = run(&["check", dir.to_str().unwrap()]);
    assert_eq!(def.code, 1, "the existence-family fatal must still fire, got:\n{}", def.stdout);
    assert!(
        def.stdout.contains("call to undefined function tyop()"),
        "not dammed by the vendor break, got:\n{}",
        def.stdout
    );
    assert!(
        !def.stdout.contains("syntax.unparsable"),
        "the broken file's own finding is vendor-suppressed by default, got:\n{}",
        def.stdout
    );
    assert!(
        def.stdout.contains("1 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "got:\n{}",
        def.stdout
    );

    let show = run(&["check", "--vendor-diagnostics", dir.to_str().unwrap()]);
    assert!(
        show.stdout.contains("call to undefined function tyop()"),
        "still not dammed, got:\n{}",
        show.stdout
    );
    assert!(
        show.stdout.contains("error[syntax.unparsable]"),
        "the broken file's own finding rides the ordinary vendor filter, got:\n{}",
        show.stdout
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

/// ADR-0050 §7 amendment: an explicitly-passed path naming nothing is a usage error
/// (exit 2), not an empty clean report — closing a regression where CI stayed green
/// after a directory rename.
#[test]
fn nonexistent_path_is_a_usage_error() {
    let r = run(&["check", "/definitely-not-a-real-path-9x8"]);
    assert_eq!(r.code, 2, "nonexistent path is exit 2, got stdout:\n{}", r.stdout);
    assert!(
        r.stderr.contains("path does not exist: /definitely-not-a-real-path-9x8"),
        "the missing path is named, got:\n{}",
        r.stderr
    );
    assert!(r.stdout.is_empty(), "no report emitted, got:\n{}", r.stdout);

    // §7 amendment point 2: --format json emits NO document — a consumer must
    // never see a well-formed empty findings set for a path that names nothing.
    let j = run(&["check", "--format", "json", "/definitely-not-a-real-path-9x8"]);
    assert_eq!(j.code, 2);
    assert!(j.stdout.is_empty(), "json run emitted a document:\n{}", j.stdout);

    // Every command that walks a path set shares the contract.
    let t = run(&["transform", "phpdoc-to-native", "/definitely-not-a-real-path-9x8"]);
    assert_eq!(t.code, 2, "transform too, got stdout:\n{}", t.stdout);
    assert!(t.stderr.contains("path does not exist"), "got:\n{}", t.stderr);

    let e = run(&["effect-diff", "/definitely-not-a-real-path-9x8"]);
    assert_eq!(e.code, 2, "effect-diff too, got stdout:\n{}", e.stdout);
    assert!(e.stderr.contains("path does not exist"), "got:\n{}", e.stderr);
}

/// Every missing path is named in one message, so a multi-path invocation reports all
/// of its typos at once rather than one per re-run; a real path alongside them does
/// not rescue the run.
#[test]
fn all_missing_paths_are_named_at_once() {
    let real = fixture("silent.php");
    let r = run(&["check", "/no-such-a-1", real.to_str().unwrap(), "/no-such-b-2"]);
    assert_eq!(r.code, 2, "one bad path fails the run, got stdout:\n{}", r.stdout);
    assert!(r.stderr.contains("/no-such-a-1"), "got:\n{}", r.stderr);
    assert!(r.stderr.contains("/no-such-b-2"), "second typo also named, got:\n{}", r.stderr);
}

/// §7 amendment point 3: existence is the discriminator, emptiness is not. A directory
/// that exists and holds no `.php` files is a real location the run had nothing to say
/// about — still exit 0, still an empty report.
#[test]
fn existing_but_empty_dir_stays_clean() {
    let dir = std::env::temp_dir().join("steins-empty-dir-test");
    std::fs::create_dir_all(&dir).expect("create empty dir");

    let r = run(&["check", dir.to_str().unwrap()]);
    assert_eq!(r.code, 0, "empty dir is a no-op, got stderr:\n{}", r.stderr);
    assert!(r.stdout.is_empty(), "got:\n{}", r.stdout);

    let j = run(&["check", "--format", "json", dir.to_str().unwrap()]);
    assert_eq!(j.code, 0);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).expect("json object");
    assert_eq!(v["findings"].as_array().expect("findings array").len(), 0);

    std::fs::remove_dir(&dir).ok();
}
