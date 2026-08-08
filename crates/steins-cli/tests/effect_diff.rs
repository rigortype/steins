//! End-to-end `steins effect-diff` contracts (issue #69), isolated in private
//! working directories and baselines.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test in this file spawns the binary with `GITHUB_ACTIONS` scrubbed.
/// `check`'s format auto-detection (ADR-0054 §6) reads that variable, so a test
/// run *on* GitHub Actions would otherwise get workflow commands where it
/// asserted text. No test's expected output may depend on the ambient CI
/// environment; detection itself is tested in `tests/format_github.rs`, which
/// sets the variable deliberately.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

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

fn run_in(dir: &Path, args: &[&str]) -> Run {
    let out = steins_cmd().args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// Capture `before`, replace it with `after`, then return the diff run.
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

/// An unresolved interface receiver contributes its envelope to the declared lane.
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
    assert!(!dir.join(".steins-baseline.jsonl").exists(), "effect-diff wrote a diagnostic baseline");
    assert!(dir.join("steins-effects-baseline.json").exists(), "its own sidecar file");
}

#[test]
fn a_new_occurrence_reads_as_the_headline_line() {
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
    // A non-exhaustive summary cannot prove removal; coverage changes separately.
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
    // Declared → proven is one materialization event (ADR-0067 §2.6).
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
