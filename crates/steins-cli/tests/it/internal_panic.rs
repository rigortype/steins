//! Per-file fault isolation (issue #895, owner decision D3): a panic in one
//! file's walk is that file's `internal.panic` finding, the run goes on with
//! every other file, and `check` exits `2`.
//!
//! The panic is provoked through the debug-build test hook
//! (`STEINS_TEST_PANIC_ON`, read by the walk): any file whose path ends with
//! its value panics as its walk begins. Release builds never read it, so every
//! test here is ignored there.
//!
//! Each test runs in a private temp dir as its working directory, so the
//! auto-loaded `steins.toml` and baseline are its own, and `--no-php` keeps it
//! hermetic.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const PANIC_ENV: &str = "STEINS_TEST_PANIC_ON";

/// One proof finding per file, from the sound subset alone.
const APP: &str = "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n";
const HELPER: &str = "<?php\nfunction area(int $a): int { return $a; }\narea(null);\n";

/// A throwaway directory under the OS temp dir, removed on drop. Not under the
/// repository: a cached run writes `.steins/` beside the code it analyzes.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("steins-internal-panic-{tag}-{}-{n}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("create workdir");
        Self(dir)
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).expect("write fixture");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run `steins` in `dir`, panicking on files ending in `panic_on` when given.
fn run_in(dir: &Path, args: &[&str], panic_on: Option<&str>) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_steins"));
    // Format auto-detection (ADR-0054 §6) reads it; a developer's backtrace
    // switch would put the standard panic report on stderr.
    cmd.env_remove("GITHUB_ACTIONS").env_remove("RUST_BACKTRACE").env_remove(PANIC_ENV);
    if let Some(suffix) = panic_on {
        cmd.env(PANIC_ENV, suffix);
    }
    let out = cmd.args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn two_file_project(tag: &str) -> TempDir {
    let dir = TempDir::new(tag);
    dir.write("src/app.php", APP);
    dir.write("src/helper.php", HELPER);
    dir
}

/// The panicking file gets exactly one `internal.panic` naming it, the other
/// file keeps its ordinary finding, and the exit is 2 — not 1, though a
/// fail-level finding is displayed too, and not the 101 of an unwound process.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn the_panicking_file_is_named_and_the_run_goes_on() {
    let dir = two_file_project("cold");
    let r = run_in(&dir.0, &["check", "--no-php", "--no-cache", "src"], Some("src/app.php"));
    assert_eq!(
        r.code,
        2,
        "a panic is the tool failing; stdout:\n{}\nstderr:\n{}",
        r.stdout,
        r.stderr
    );
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "one line per file, got:\n{}", r.stdout);
    assert!(lines[0].starts_with("src/app.php:1:1: error[internal.panic]: "), "{}", lines[0]);
    assert!(
        lines[0].contains("STEINS_TEST_PANIC_ON names this file"),
        "the message is carried: {}",
        lines[0]
    );
    assert!(
        !r.stdout.contains("width()"),
        "the panicked file's own findings are withheld, got:\n{}",
        r.stdout
    );
    assert!(
        lines[1].starts_with("src/helper.php:3:6: error[type.argument-mismatch]"),
        "the other file is analyzed as ever: {}",
        lines[1]
    );
    assert!(
        r.stderr.contains("1 file(s) panicked in analysis"),
        "stderr says why it exits 2:\n{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("panicked at"),
        "the default panic report is captured:\n{}",
        r.stderr
    );
}

/// `--format json` carries the finding like any other: its id, layer and level.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn json_carries_it_like_any_finding() {
    let dir = two_file_project("json");
    let r = run_in(
        &dir.0,
        &["check", "--no-php", "--no-cache", "--format", "json", "src"],
        Some("src/app.php"),
    );
    assert_eq!(r.code, 2, "stderr:\n{}", r.stderr);
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("a json document");
    let findings = doc["findings"].as_array().expect("a findings array");
    let ids: Vec<(&str, &str)> = findings
        .iter()
        .map(|f| (f["id"].as_str().unwrap(), f["path"].as_str().unwrap()))
        .collect();
    assert_eq!(
        ids,
        [("internal.panic", "src/app.php"), ("type.argument-mismatch", "src/helper.php")],
        "{}",
        r.stdout
    );
    assert_eq!(findings[0]["layer"], "mechanics");
    assert_eq!(findings[0]["level"], "fail");
    assert_eq!((findings[0]["line"].as_u64(), findings[0]["column"].as_u64()), (Some(1), Some(1)));
}

/// No channel a user configures reaches it: not a profile's `disable` or
/// `warn`, not an inline ignore (which, sitting in the panicked file, is not
/// judged either way), not the vendor filter. A baseline cannot hold
/// it, and a run that reports one refuses to write a baseline at all.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn no_configured_channel_reaches_it() {
    let dir = TempDir::new("channels");
    dir.write(
        "steins.toml",
        "[check]\nprofile = \"quiet\"\n\n[profile.quiet]\nextends = \"default\"\n\
         disable = [\"internal.panic\", \"internal.*\"]\nwarn = [\"internal.*\"]\n",
    );
    // The ignore sits on line 1, where the finding is reported.
    let ignored = APP.replacen("<?php\n", "<?php // @steins-ignore internal.panic\n", 1);
    dir.write("src/app.php", &ignored);
    dir.write("vendor/acme/lib/Lib.php", HELPER);

    let plain = run_in(&dir.0, &["check", "--no-php", "--no-cache", "src", "vendor"], None);
    assert!(
        plain.stdout.contains("1 findings in vendor suppressed"),
        "the library is vendor code, filtered as such, got:\n{}",
        plain.stdout
    );
    let r = run_in(&dir.0, &["check", "--no-php", "--no-cache", "src", "vendor"], Some("Lib.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(
        r.stdout.contains("vendor/acme/lib/Lib.php:1:1: error[internal.panic]"),
        "the vendor filter passes it through, at fail level, got:\n{}",
        r.stdout
    );

    let r = run_in(&dir.0, &["check", "--no-php", "--no-cache", "src"], Some("src/app.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(
        r.stdout.contains("src/app.php:1:1: error[internal.panic]"),
        "neither the profile nor the inline ignore reaches it, got:\n{}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("suppress.unmatched"),
        "a panicked file's ignores are not judged, got:\n{}",
        r.stdout
    );

    let set = ["check", "--no-php", "--no-cache", "--set-baseline", "src"];
    let r = run_in(&dir.0, &set, Some("src/app.php"));
    assert_eq!(r.code, 2, "stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("not writing the baseline"), "stderr:\n{}", r.stderr);
    assert!(!dir.0.join(".steins-baseline.jsonl").exists(), "no baseline is written");
}

/// The cache never keeps a panic: a run that reports one publishes nothing,
/// so `CURRENT` stays what it was, and the next run without the panic walks the
/// file again and reports its real finding instead of replaying the panic.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn a_run_that_panicked_publishes_no_generation() {
    let dir = two_file_project("cache");
    let current = dir.0.join(".steins/gen/CURRENT");

    // Cold store: nothing was published before, and nothing is now.
    let r = run_in(&dir.0, &["check", "--no-php", "src"], Some("src/app.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(!current.exists(), "a panicked cold run publishes no generation");

    let clean = run_in(&dir.0, &["check", "--no-php", "src"], None);
    assert_eq!(clean.code, 1, "stdout:\n{}\nstderr:\n{}", clean.stdout, clean.stderr);
    assert!(!clean.stdout.contains("internal.panic"), "nothing replays it, got:\n{}", clean.stdout);
    let real = "src/app.php:3:7: error[type.argument-mismatch]";
    assert!(clean.stdout.contains(real), "{}", clean.stdout);
    let published = std::fs::read(&current).expect("the clean run published");

    // Warm store: the edited file walks, panics, and `CURRENT` is untouched.
    dir.write("src/app.php", &APP.replace("width(\"abc\");", "width(\"abc\");\nwidth(\"xyz\");"));
    let r = run_in(&dir.0, &["check", "--no-php", "src"], Some("src/app.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("src/app.php:1:1: error[internal.panic]"), "{}", r.stdout);
    let after = std::fs::read(&current).unwrap();
    assert_eq!(after, published, "a panicked warm run leaves CURRENT as it was");

    let again = run_in(&dir.0, &["check", "--no-php", "src"], None);
    assert_eq!(again.code, 1, "stdout:\n{}\nstderr:\n{}", again.stdout, again.stderr);
    let real = "src/app.php:4:7: error[type.argument-mismatch]";
    assert!(again.stdout.contains(real), "{}", again.stdout);
    assert!(!again.stdout.contains("internal.panic"), "{}", again.stdout);
}

/// A panicked file's valid ignores and baseline entries are not rot: they
/// matched nothing because the file's walk produced nothing, so neither
/// `suppress.unmatched` nor a stale-entry count may tell the user to delete
/// them.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn a_panicked_files_ignores_and_baseline_entries_are_not_rot() {
    let dir = TempDir::new("rot");
    let ignored = "<?php\nfunction width(int $w): int { return $w; }\n\
                   width(\"abc\"); // @steins-ignore type.argument-mismatch\nwidth(\"x\");\n";
    dir.write("src/app.php", ignored);
    dir.write("src/helper.php", HELPER);
    let set = run_in(&dir.0, &["check", "--no-php", "--no-cache", "--set-baseline", "src"], None);
    assert_eq!(set.code, 0, "stderr:\n{}", set.stderr);

    let r = run_in(&dir.0, &["check", "--no-php", "--no-cache", "src"], Some("src/app.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("src/app.php:1:1: error[internal.panic]"), "{}", r.stdout);
    assert!(!r.stdout.contains("suppress.unmatched"), "the ignore is not rot:\n{}", r.stdout);
    assert!(!r.stdout.contains("stale"), "its baseline entry is not stale:\n{}", r.stdout);
    assert!(
        r.stdout.contains("1 findings in baseline"),
        "helper's entry still matches:\n{}",
        r.stdout
    );
}

/// `check --fix` writes nothing from a panicked run: the fixes are refused by
/// name, and the run exits 2.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn check_fix_writes_nothing_from_a_panicked_run() {
    let dir = TempDir::new("fix");
    dir.write("src/app.php", APP);
    let dump = "<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n";
    dir.write("src/dump.php", dump);
    let fix = ["check", "--no-php", "--no-cache", "--fix", "src"];

    let r = run_in(&dir.0, &fix, Some("src/app.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("fix refused (analysis-panicked)"), "stderr:\n{}", r.stderr);
    let after = std::fs::read_to_string(dir.0.join("src/dump.php")).unwrap();
    assert_eq!(after, dump, "nothing is written");

    // The control: without the panic, the same fix is written.
    let fixed = run_in(&dir.0, &fix, None);
    assert!(fixed.stderr.contains("steins: fixed"), "stderr:\n{}", fixed.stderr);
    assert_ne!(std::fs::read_to_string(dir.0.join("src/dump.php")).unwrap(), dump);
}

/// `transform --apply` writes nothing when a file panics in the post-check's
/// analysis: the post-check fails by name, and the run exits 2.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn transform_writes_nothing_on_a_panicked_post_check() {
    let dir = TempDir::new("transform");
    let lib = "<?php\n/**\n * @param int $w\n * @return int\n */\n\
               function pad($w)\n{\n    return $w;\n}\n";
    dir.write("src/lib.php", lib);
    dir.write("src/caller.php", "<?php\npad(42);\n");
    let apply = ["transform", "phpdoc-to-native", "--apply", "src"];

    let r = run_in(&dir.0, &apply, Some("src/caller.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("Post-check FAILED — 1 file(s) panicked"), "{}", r.stdout);
    assert!(!r.stdout.contains("Post-check OK"), "{}", r.stdout);
    assert!(r.stderr.contains("nothing was written"), "stderr:\n{}", r.stderr);
    assert_eq!(std::fs::read_to_string(dir.0.join("src/lib.php")).unwrap(), lib);

    // The control: without the panic, the same plan is written.
    let applied = run_in(&dir.0, &apply, None);
    assert_eq!(applied.code, 0, "stdout:\n{}\nstderr:\n{}", applied.stdout, applied.stderr);
    assert_ne!(std::fs::read_to_string(dir.0.join("src/lib.php")).unwrap(), lib);
}

/// `annotate`'s findings come from the isolated walk: a panic there leaves the
/// margin's markers missing, so it exits 2 with the same notice as `check`.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the panic test hook is debug-only")]
fn annotate_exits_2_when_its_margin_panicked() {
    let dir = TempDir::new("annotate");
    dir.write("src/app.php", APP);
    let r = run_in(&dir.0, &["annotate", "--no-php", "src/app.php"], Some("src/app.php"));
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("✗ internal.panic"), "{}", r.stdout);
    assert!(r.stderr.contains("1 file(s) panicked in analysis"), "stderr:\n{}", r.stderr);

    let clean = run_in(&dir.0, &["annotate", "--no-php", "src/app.php"], None);
    assert_eq!(clean.code, 0, "stderr:\n{}", clean.stderr);
}
