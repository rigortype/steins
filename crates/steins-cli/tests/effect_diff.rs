//! End-to-end tests for `steins effect-diff` (issue #69): the effect baseline
//! captures per-function summaries, and a later run reports what a change did to
//! them.
//!
//! Each test runs the real binary in a private temp directory, so the default
//! `steins-effects-baseline.json` and the relative entry paths are isolated.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// A fresh, unique working directory under the system temp dir.
fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-effectdiff-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run `steins <args>` with CWD set to `dir`.
fn run_in(dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// Capture a baseline over `a.php`, then rewrite the file and diff against it.
/// Returns the diff run.
fn capture_then(dir: &Path, before: &str, after: &str, args: &[&str]) -> Run {
    write(dir, "a.php", before);
    let cap = run_in(dir, &["effect-diff", "--set-baseline", "a.php"]);
    assert_eq!(cap.code, 0, "capture failed: {}{}", cap.stdout, cap.stderr);
    write(dir, "a.php", after);
    let mut argv = vec!["effect-diff"];
    argv.extend_from_slice(args);
    argv.push("a.php");
    run_in(dir, &argv)
}

/// The DI shape ADR-0067 exists for: an interface envelope reaches the caller's
/// *declared* lane through a receiver no concrete class resolves.
const REPO: &str = concat!(
    "interface Repo {\n",
    "    #[\\Steins\\Effect('io.db')]\n",
    "    public function find(int $id): string;\n",
    "}\n",
);

#[test]
fn capture_then_identical_run_reports_nothing() {
    let dir = workdir("identical");
    let src = "<?php\nfunction report(): int { printf(\"x\"); return 1; }\n";
    let r = capture_then(&dir, src, src, &[]);
    assert_eq!(r.code, 0, "informational surface always exits 0");
    assert!(r.stdout.is_empty(), "no events, no footer, got:\n{}", r.stdout);
    // The diagnostic channel is untouched: its own file is never created here.
    assert!(!dir.join(".steins-baseline.jsonl").exists(), "effect-diff wrote a diagnostic baseline");
    assert!(dir.join("steins-effects-baseline.json").exists(), "its own sidecar file");
}

#[test]
fn a_new_occurrence_reads_as_the_headline_line() {
    // Issue #69's promise, literally: the refactor added `io.db` to
    // `Checkout::confirm`, and that sentence is what the line says.
    let dir = workdir("added");
    let r = capture_then(
        &dir,
        "<?php\nfinal class Checkout {\n    public function confirm(): int { return 1; }\n}\n",
        "<?php\nfinal class Checkout {\n    public function confirm(): int { (new PDO('x'))->query('y'); return 1; }\n}\n",
        &[],
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "a.php Checkout::confirm: + io.db\n", "stderr:\n{}", r.stderr);
}

#[test]
fn a_removal_is_confident_when_the_current_summary_is_exhaustive() {
    let dir = workdir("removed");
    let r = capture_then(
        &dir,
        "<?php\nfunction report(): int { printf(\"x\"); return 1; }\n",
        "<?php\nfunction report(): int { return 1; }\n",
        &[],
    );
    assert_eq!(r.stdout, "a.php report: - output\n");
}

#[test]
fn a_non_exhaustive_current_summary_hedges_the_removal() {
    // "These effects, and possibly more" cannot prove an absence — so the same
    // candidate arrives hedged, and the exhaustiveness change is its own line.
    let dir = workdir("hedged");
    let r = capture_then(
        &dir,
        "<?php\nfunction report(): int { printf(\"x\"); return 1; }\n",
        "<?php\nfunction report(): int { unknown_helper(); return 1; }\n",
        &[],
    );
    assert_eq!(
        r.stdout,
        "a.php report: ? - output (possibly removed; current summary non-exhaustive)\n\
         a.php report: coverage narrowed (exhaustive → non-exhaustive)\n"
    );
    assert!(!r.stdout.contains("report: - output"), "no confident removal, got:\n{}", r.stdout);
}

#[test]
fn a_declared_bound_becoming_proven_is_one_materialization() {
    // ADR-0067 §2.6: declared → proven is a materialization, not a removal plus an
    // addition. Exactly one line, and it names the lane it crossed.
    let dir = workdir("materialized");
    let r = capture_then(
        &dir,
        &format!(
            "<?php\n{REPO}final class Checkout {{\n    public function confirm(Repo $r): string {{ return $r->find(1); }}\n}}\n"
        ),
        "<?php\nfinal class Checkout {\n    public function confirm(): string { return (new PDO('x'))->query('y'); }\n}\n",
        &[],
    );
    assert_eq!(
        r.stdout,
        "a.php Checkout::confirm: ≤→ io.db (declared bound now proven)\n",
        "stderr:\n{}",
        r.stderr
    );
}

#[test]
fn a_declared_bound_appearing_is_labeled_as_declared() {
    let dir = workdir("declared-added");
    let r = capture_then(
        &dir,
        &format!(
            "<?php\n{REPO}final class Checkout {{\n    public function confirm(Repo $r): string {{ return 'x'; }}\n}}\n"
        ),
        &format!(
            "<?php\n{REPO}final class Checkout {{\n    public function confirm(Repo $r): string {{ return $r->find(1); }}\n}}\n"
        ),
        &[],
    );
    assert_eq!(r.stdout, "a.php Checkout::confirm: + ≤io.db (declared)\n", "stderr:\n{}", r.stderr);
}

#[test]
fn an_exhaustiveness_transition_is_never_folded_into_a_label_event() {
    let dir = workdir("coverage");
    let narrowed = capture_then(
        &dir,
        "<?php\nfunction report(): int { return 1; }\n",
        "<?php\nfunction report(): int { unknown_helper(); return 1; }\n",
        &[],
    );
    assert_eq!(
        narrowed.stdout,
        "a.php report: coverage narrowed (exhaustive → non-exhaustive)\n"
    );

    let dir = workdir("coverage-back");
    let completed = capture_then(
        &dir,
        "<?php\nfunction report(): int { unknown_helper(); return 1; }\n",
        "<?php\nfunction report(): int { return 1; }\n",
        &[],
    );
    assert_eq!(
        completed.stdout,
        "a.php report: coverage completed (non-exhaustive → exhaustive)\n"
    );
}

#[test]
fn a_renamed_function_produces_no_removal_noise() {
    // The acceptance criterion: the effects did not go anywhere, the name did. Only
    // the footer counts it.
    let dir = workdir("renamed");
    let r = capture_then(
        &dir,
        "<?php\nfunction report(): int { printf(\"x\"); return 1; }\n",
        "<?php\nfunction summarize(): int { printf(\"x\"); return 1; }\n",
        &[],
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "1 functions not in baseline, 1 no longer present\n");
}

#[test]
fn json_shape_is_pinned() {
    let dir = workdir("json");
    let r = capture_then(
        &dir,
        "<?php\nfunction report(): int { return 1; }\nfunction kept(): int { return 2; }\n",
        "<?php\nfunction report(): int { printf(\"x\"); return 1; }\nfunction added(): int { return 3; }\n",
        &["--format", "json"],
    );
    assert_eq!(r.code, 0);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json object");
    assert_eq!(v["compared"], 1, "only `report` is on both sides");
    assert_eq!(v["not_in_baseline"], 1);
    assert_eq!(v["no_longer_present"], 1);
    let events = v["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1, "renamed functions are never itemized: {events:?}");
    assert_eq!(events[0]["file"], "a.php");
    assert_eq!(events[0]["symbol"], "report");
    assert_eq!(events[0]["category"], "proven-added");
    assert_eq!(events[0]["label"], "output");
}

#[test]
fn a_coverage_event_carries_a_null_label_in_json() {
    let dir = workdir("json-null");
    let r = capture_then(
        &dir,
        "<?php\nfunction report(): int { return 1; }\n",
        "<?php\nfunction report(): int { unknown_helper(); return 1; }\n",
        &["--format", "json"],
    );
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json object");
    assert_eq!(v["events"][0]["category"], "coverage-narrowed");
    assert!(v["events"][0]["label"].is_null(), "no label on an exhaustiveness event");
}

#[test]
fn an_explicit_baseline_path_is_honored_and_a_missing_one_is_a_usage_error() {
    let dir = workdir("path");
    write(&dir, "a.php", "<?php\nfunction report(): int { return 1; }\n");
    let missing = run_in(&dir, &["effect-diff", "a.php"]);
    assert_eq!(missing.code, 2, "no baseline to compare against → usage error");
    assert!(missing.stderr.contains("--set-baseline"), "stderr:\n{}", missing.stderr);

    let cap = run_in(&dir, &["effect-diff", "--set-baseline", "--baseline", "effects.json", "a.php"]);
    assert_eq!(cap.code, 0);
    assert!(dir.join("effects.json").exists(), "the named file, not the default");
    assert!(!dir.join("steins-effects-baseline.json").exists());

    write(&dir, "a.php", "<?php\nfunction report(): int { printf(\"x\"); return 1; }\n");
    let r = run_in(&dir, &["effect-diff", "--baseline", "effects.json", "a.php"]);
    assert_eq!(r.stdout, "a.php report: + output\n");
}
