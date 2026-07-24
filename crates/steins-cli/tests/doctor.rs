//! End-to-end tests for `steins doctor` (ADR-0054 Part II, C3 minimal scope): the
//! four sections (Runtime, Config + active surface, Envelopes, Baseline) and the
//! exit semantics (§10 — environment degrades at 0, configuration contradicts at 1,
//! usage at 2).
//!
//! Each test runs the real `steins` binary in a private temp dir (its own CWD) so
//! the auto-loaded `steins.toml` and `.steins-baseline.jsonl` are isolated. Most use
//! `--no-php` for determinism (no dependency on a `php` on PATH); the Runtime section
//! still renders (the sound-subset posture) and every run stays exit-neutral there.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-doctor-{}-{tag}-{n}", std::process::id()));
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

/// Two functions and one method, each carrying a written `@throws` (3 envelopes),
/// plus one function with no `@throws` (not counted).
const THREE_THROWS: &str = "<?php\n\
    /** @throws \\RuntimeException */\n\
    function a(): void { throw new \\RuntimeException(); }\n\
    /** @throws \\LogicException */\n\
    function b(): void {}\n\
    class C {\n\
    /** @throws \\JsonException */\n\
    public function m(): void {}\n\
    public function n(): void {}\n\
    }\n";

// ------------------------------------------------------- all sections render ---

#[test]
fn doctor_renders_all_four_sections_exit_zero() {
    let dir = workdir("sections");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "healthy/degraded world → exit 0; stdout:\n{}", r.stdout);
    for section in ["Runtime", "Config + active surface", "Envelopes", "Baseline"] {
        assert!(r.stdout.contains(section), "missing `{section}` section; stdout:\n{}", r.stdout);
    }
}

#[test]
fn doctor_default_path_is_dot() {
    // No path argument defaults to `.`.
    let dir = workdir("defaultpath");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("Envelopes"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_without_no_php_still_renders_runtime_and_exits_zero() {
    // With or without a `php` on PATH the Runtime section renders and the run is
    // exit-neutral: a missing sidecar is the sound subset, never a failure (§10).
    let dir = workdir("runtime");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "."]);
    assert_eq!(r.code, 0, "environment facts report at 0; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("Runtime"), "stdout:\n{}", r.stdout);
    // Either a live version line or the sound-subset posture — one must appear.
    assert!(
        r.stdout.contains("PHP version:") || r.stdout.contains("sound subset"),
        "runtime posture must render; stdout:\n{}",
        r.stdout
    );
}

// -------------------------------------------------------- active surface line ---

#[test]
fn doctor_reflects_configured_profile() {
    let dir = workdir("profile");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[check]\nprofile = \"contracts\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("active profile: `contracts`"),
        "active profile must reflect [check] profile; stdout:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("[check] profile"), "provenance named; stdout:\n{}", r.stdout);
}

#[test]
fn doctor_default_profile_provenance() {
    let dir = workdir("default-prof");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("active profile: `default`") && r.stdout.contains("built-in default"),
        "stdout:\n{}",
        r.stdout
    );
}

// -------------------------------------------------------------- envelope scan ---

#[test]
fn doctor_counts_written_throws_envelopes() {
    let dir = workdir("envcount");
    write(&dir, "a.php", THREE_THROWS);
    // Default profile does not check throw.undeclared → the G1-demote notice fires.
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("3 written throw envelope"),
        "expected 3 envelopes counted; stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("does not check them") && r.stdout.contains("contracts"),
        "the G1-demote notice must name the checking profile; stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_envelope_notice_flips_under_contracts() {
    let dir = workdir("envcontracts");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[check]\nprofile = \"contracts\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("3 declaration(s) carry a written @throws")
            && r.stdout.contains("checks them"),
        "under contracts the envelopes are checked; stdout:\n{}",
        r.stdout
    );
}

// ------------------------------------------------------------------- baseline ---

#[test]
fn doctor_reports_no_baseline_when_absent() {
    let dir = workdir("nobaseline");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(r.stdout.contains("Baseline\n  none"), "stdout:\n{}", r.stdout);
    assert_eq!(r.code, 0);
}

#[test]
fn doctor_reports_baseline_capture_surface_and_dormant() {
    let dir = workdir("baseline");
    // A proof finding (width("abc")) plus a direct throw; capture under CONTRACTS so
    // the baseline holds both the proof and the throw entry.
    let mixed = "<?php\n\
        function width(int $w): int { return $w; }\n\
        width(\"abc\");\n\
        /** @throws \\JsonException */\n\
        function f(): void { throw new \\RangeException(); }\n";
    write(&dir, "a.php", mixed);
    let r = run_in(&dir, &["check", "--no-php", "--profile", "contracts", "--set-baseline", "a.php"]);
    assert_eq!(r.code, 0, "set-baseline exits 0; stderr:\n{}", r.stderr);

    // Doctor under the DEFAULT surface: the throw entry's id is outside it → dormant.
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "dormant entries are not a failure; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("capture surface: profile `contracts`"), "stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("active surface: profile `default`"), "stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("dormant entr"), "the out-of-surface throw entry is dormant; stdout:\n{}", r.stdout);
}

#[test]
fn doctor_accepts_explicit_baseline_flag() {
    let dir = workdir("baseline-flag");
    write(&dir, "a.php", "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n");
    let r = run_in(&dir, &["check", "--no-php", "--set-baseline", "--baseline", "custom.jsonl", "a.php"]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let r = run_in(&dir, &["doctor", "--no-php", "--baseline", "custom.jsonl", "."]);
    assert!(r.stdout.contains("custom.jsonl"), "explicit baseline reported; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("capture surface: profile `default`"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_unparseable_baseline_is_a_contradiction() {
    let dir = workdir("bad-baseline");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, ".steins-baseline.jsonl", "not json at all\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unparseable baseline → config contradiction exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("UNPARSEABLE"), "stdout:\n{}", r.stdout);
}

// ------------------------------------------------- config contradiction exits ---

#[test]
fn doctor_malformed_toml_exits_one() {
    // ADR-0054 §10: for DOCTOR a config contradiction is exit 1 (check's is exit 2).
    let dir = workdir("bad-toml");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "garbage = = [[[\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "malformed steins.toml → doctor exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("PARSE ERROR"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_unknown_profile_exits_one() {
    let dir = workdir("unknown-prof");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[check]\nprofile = \"nope\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unknown profile → doctor config contradiction exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("profile resolution: ERROR"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_unknown_runtime_key_exits_one() {
    // The unknown-[runtime]-key parse failure is exit 2 for check, but a config
    // contradiction (exit 1) for doctor (§10).
    let dir = workdir("bad-runtime");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[runtime]\nzend-asertions = \"enabled\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unknown [runtime] key → doctor exit 1; stdout:\n{}", r.stdout);
}

// ----------------------------------------------------------------- usage errors ---

#[test]
fn doctor_rejects_extra_paths() {
    let dir = workdir("usage");
    let r = run_in(&dir, &["doctor", "a", "b"]);
    assert_eq!(r.code, 2, "too many paths → usage error exit 2; stderr:\n{}", r.stderr);
}

#[test]
fn doctor_rejects_unknown_flag() {
    let dir = workdir("badflag");
    let r = run_in(&dir, &["doctor", "--nope"]);
    assert_eq!(r.code, 2, "unknown flag → usage error exit 2; stderr:\n{}", r.stderr);
}
