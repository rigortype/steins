//! Issue #885: `steins check --progress` (or `STEINS_PROGRESS=1`) says where a
//! run is while it runs, on stderr, and says nothing otherwise.
//!
//! Every test runs `--no-php`: the progress channel is a property of the
//! pipeline, not of the sidecar, and a test that needs `php` on `PATH` proves
//! nothing more about it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const PHASES_COLD: [&str; 8] =
    ["discover", "parse", "universe", "purity oracle", "walk", "report", "suppress", "output"];

const PHASES_STORE: [&str; 11] = [
    "discover",
    "capture",
    "parse",
    "fold engine",
    "universe",
    "purity oracle",
    "walk",
    "report",
    "persist",
    "suppress",
    "output",
];

const FIXTURE: &str = "<?php declare(strict_types = 1);
namespace App;
function f(int $x): int { return $x; }
function g(): int { return f(\"a\"); }
";

struct Run {
    stdout: String,
    stderr: String,
}

/// A fresh project: a Composer manifest and `files` PHP files under `src/`.
fn project(tag: &str, files: usize) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-progress-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("create project");
    std::fs::write(
        dir.join("composer.json"),
        r#"{"name": "fixture/progress", "autoload": {"psr-4": {"App\\": "src/"}}}"#,
    )
    .expect("write manifest");
    for i in 0..files {
        let body = FIXTURE.replace("namespace App;", &format!("namespace App\\N{i};"));
        std::fs::write(dir.join(format!("src/F{i:03}.php")), body).expect("write fixture");
    }
    dir
}

/// `steins check --no-php <args> src` in `dir`, with the progress environment
/// set to exactly `env` and nothing else of it inherited.
fn check(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_steins"));
    cmd.env_remove("GITHUB_ACTIONS")
        .env_remove("STEINS_PROGRESS")
        .env_remove("STEINS_PROGRESS_SLOW_MS")
        .envs(env.iter().copied())
        .args(["check", "--no-php"])
        .args(args)
        .arg("src")
        .current_dir(dir);
    let out = cmd.output().expect("run steins");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn progress_lines(stderr: &str) -> Vec<&str> {
    stderr.lines().filter(|l| l.starts_with("steins: progress: ")).collect()
}

/// The phase names the progress lines carry, in order.
fn phases(stderr: &str) -> Vec<&str> {
    progress_lines(stderr)
        .into_iter()
        .filter_map(|l| l.strip_prefix("steins: progress: "))
        .filter(|l| !l.starts_with("slow file: "))
        .filter_map(|l| l.split_once(": ").map(|(name, _)| name))
        .collect()
}

#[test]
fn the_cold_path_says_each_phase_in_order() {
    let dir = project("cold", 1);
    let run = check(&dir, &["--no-cache", "--progress"], &[]);
    assert_eq!(phases(&run.stderr), PHASES_COLD, "stderr was:\n{}", run.stderr);
    assert!(run.stderr.contains("(elapsed "), "every phase line carries the elapsed time");
}

#[test]
fn the_store_path_says_each_phase_in_order() {
    let dir = project("store", 1);
    let run = check(&dir, &["--progress"], &[]);
    assert_eq!(phases(&run.stderr), PHASES_STORE, "stderr was:\n{}", run.stderr);
    assert!(dir.join(".steins").exists(), "the run went through the store");
}

#[test]
fn the_environment_turns_the_channel_on() {
    let dir = project("env", 1);
    let run = check(&dir, &["--no-cache"], &[("STEINS_PROGRESS", "1")]);
    assert_eq!(phases(&run.stderr), PHASES_COLD);
    let off = check(&dir, &["--no-cache"], &[("STEINS_PROGRESS", "0")]);
    assert!(progress_lines(&off.stderr).is_empty(), "`0` is off:\n{}", off.stderr);
}

#[test]
fn a_file_over_the_threshold_is_named_by_its_diagnostic_path() {
    for args in [&["--no-cache", "--progress"][..], &["--progress"][..]] {
        let dir = project("slow", 1);
        let run = check(&dir, args, &[("STEINS_PROGRESS_SLOW_MS", "0")]);
        let named: Vec<&str> = progress_lines(&run.stderr)
            .into_iter()
            .filter(|l| l.starts_with("steins: progress: slow file: "))
            .collect();
        assert_eq!(named.len(), 1, "{args:?}: stderr was:\n{}", run.stderr);
        assert!(
            named[0].starts_with("steins: progress: slow file: src/F000.php walked in "),
            "{args:?}: {}",
            named[0]
        );
    }
}

#[test]
fn a_file_under_the_threshold_is_not_named() {
    let dir = project("quick", 3);
    let run = check(&dir, &["--no-cache", "--progress"], &[("STEINS_PROGRESS_SLOW_MS", "3600000")]);
    assert!(!run.stderr.contains("slow file"), "stderr was:\n{}", run.stderr);
    assert_eq!(phases(&run.stderr), PHASES_COLD);
}

/// The fan-out hands lines to the sink from several threads: each file is
/// named exactly once, and no line is cut by another's.
#[test]
fn the_parallel_fleet_names_every_file_in_whole_lines() {
    const FILES: usize = 48;
    let dir = project("fleet", FILES);
    let run = check(
        &dir,
        &["--progress"],
        &[("STEINS_PROGRESS_SLOW_MS", "0"), ("STEINS_WALK_WORKERS", "4")],
    );
    for line in run.stderr.lines().filter(|l| l.contains("progress")) {
        assert!(line.starts_with("steins: progress: "), "a line was cut or merged: {line:?}");
        assert_eq!(line.matches("steins: ").count(), 1, "two lines merged: {line:?}");
    }
    for i in 0..FILES {
        let wanted = format!("steins: progress: slow file: src/F{i:03}.php walked in ");
        let n = run.stderr.lines().filter(|l| l.starts_with(&wanted) && l.ends_with(" ms")).count();
        assert_eq!(n, 1, "src/F{i:03}.php named {n} times:\n{}", run.stderr);
    }
}

/// The channel is cost reporting: findings, exit and every other byte of both
/// streams are what they were without it.
#[test]
fn the_channel_changes_no_other_byte() {
    for (tag, args) in [("plain", &["--no-cache"][..]), ("store", &[][..]), ("json", &["--format", "json"][..])] {
        let dir = project(tag, 2);
        let off = check(&dir, args, &[]);
        let mut with = args.to_vec();
        with.push("--progress");
        let on = check(&dir, &with, &[("STEINS_PROGRESS_SLOW_MS", "0")]);
        assert!(off.stdout.contains("type.argument-mismatch"), "the fixture has findings");
        assert_eq!(on.stdout, off.stdout, "{tag}: stdout");
        let quiet: String = on
            .stderr
            .lines()
            .filter(|l| !l.starts_with("steins: progress: "))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(quiet, off.stderr, "{tag}: stderr minus the progress lines");
        assert!(!progress_lines(&on.stderr).is_empty(), "{tag}: the channel was on");
        assert!(progress_lines(&off.stderr).is_empty(), "{tag}: the channel was off");
    }
}
