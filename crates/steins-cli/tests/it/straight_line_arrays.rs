//! Time budgets for straight-line array building at the binary's own entry
//! point (issue #884, the root cause of #658).
//!
//! Each `$a[] = v` and `$a['k'] = v` on a witnessed shape rebuilt the whole
//! shape, and the rebuild was quadratic in its width, so N straight-line
//! appends to one variable cost O(N^3): 3,000 of them took 10 s in a release
//! build, and a generated file with tens of thousands never finished. Two
//! fixes bound the cost: the lookups and the order check inside a rebuild are
//! no longer quadratic, and a shape wider than `SHAPE_WIDTH_LIMIT` (A-G6) is
//! summarized by its tail whoever built it, so past 256 keys an append no
//! longer rebuilds anything wide.
//!
//! A cubic regression does not fail by a few percent; at these sizes it fails
//! by orders of magnitude. The budgets are therefore generous: they were set
//! from a debug-profile measurement (CI runs debug tests), where the three
//! fixtures take 0.2 s, 1.1 s and 0.3 s, with a margin of well over ten times
//! for a slow runner.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("steins-straight-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

/// `function f()` building `$a` with `n` statements of the form `line(i)`.
fn builder(n: usize, line: impl Fn(usize) -> String) -> String {
    let mut src = String::from("<?php\nfunction f(): int {\n$a = [];\n");
    for i in 0..n {
        src.push_str(&line(i));
        src.push('\n');
    }
    src.push_str("return count($a);\n}\n");
    src
}

/// Run `steins check` over `src` and fail unless it ends in a normal exit
/// inside `budget`.
fn check_within(tag: &str, src: &str, budget: Duration) -> Duration {
    let dir = workdir(tag);
    let file: &Path = &dir.join("t.php");
    std::fs::write(file, src).expect("write fixture");

    let started = Instant::now();
    let out = Command::new(bin())
        .env_remove("GITHUB_ACTIONS")
        .args(["check", "--no-php", "--no-cache"])
        .arg(file)
        .output()
        .expect("run steins");
    let took = started.elapsed();
    eprintln!("{tag}: {took:?}");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "{tag}: steins check ended abnormally ({:?}):\n{stderr}",
        out.status
    );
    assert!(took < budget, "{tag}: took {took:?}, over the {budget:?} budget");
    let _ = std::fs::remove_dir_all(&dir);
    took
}

#[test]
fn three_thousand_straight_line_appends_stay_inside_the_budget() {
    let src = builder(3_000, |i| format!("$a[] = {i};"));
    check_within("append3000", &src, Duration::from_secs(20));
}

#[test]
fn eight_thousand_straight_line_appends_stay_inside_the_budget() {
    let src = builder(8_000, |i| format!("$a[] = {i};"));
    check_within("append8000", &src, Duration::from_secs(30));
}

#[test]
fn two_thousand_straight_line_offset_writes_stay_inside_the_budget() {
    let src = builder(2_000, |i| format!("$a['k{i}'] = {i};"));
    check_within("write2000", &src, Duration::from_secs(20));
}
