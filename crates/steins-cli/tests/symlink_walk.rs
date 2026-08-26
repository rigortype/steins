//! Issue #524: the walk must not leave the tree it was pointed at, and what it
//! refuses must be visible.
//!
//! The fixture is the shape that invalidated the ADR-0092 measurements — a
//! directory symlink out of the tree, one back into it (`corpus/corpus ->
//! corpus`), and a file symlink — and the assertion is the one a human makes
//! looking at the tree: *this many files*. `doctor` is where the CLI states
//! that number, and is also where the skipped links are reported; the harness
//! half of this pair is `xtask/src/corpus.rs::tests`, over the same shape.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// A fresh working directory holding the fixture below:
///
/// ```text
/// <dir>/outside/away.php            code the run was never asked about
/// <dir>/tree/pkg/a.php
/// <dir>/tree/pkg/sub/b.php
/// <dir>/tree/pkg/link.php -> pkg/a.php    file symlink, same file twice
/// <dir>/tree/out          -> <dir>/outside    dir symlink, leaves the tree
/// <dir>/tree/tree         -> <dir>/tree       dir symlink, re-enters it
/// ```
///
/// Two files is the human answer: `pkg/a.php` and `pkg/sub/b.php`.
fn fixture(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("steins-symlinkwalk-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("outside")).expect("create outside");
    std::fs::create_dir_all(dir.join("tree/pkg/sub")).expect("create tree");
    // Each file carries the same proven-null receiver finding, so a doubled
    // universe shows up as doubled findings rather than as nothing at all.
    std::fs::write(dir.join("outside/away.php"), src("Away")).expect("write away");
    std::fs::write(dir.join("tree/pkg/a.php"), src("A")).expect("write a");
    std::fs::write(dir.join("tree/pkg/sub/b.php"), src("B")).expect("write b");
    symlink(dir.join("tree/pkg/a.php"), dir.join("tree/pkg/link.php")).expect("file link");
    symlink(dir.join("outside"), dir.join("tree/out")).expect("escaping link");
    symlink(dir.join("tree"), dir.join("tree/tree")).expect("re-entering link");
    dir
}

/// A `call.on-null` (ADR-0031) in a uniquely-named class, so findings can be
/// attributed to the file they came from.
fn src(tag: &str) -> String {
    format!(
        "<?php
class U{tag} {{ public function name(): string {{ return \"x\"; }} }}
function f{tag}($u): void {{
    if ($u === null) {{ $u->name(); }}
}}
"
    )
}

fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run steins");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The count `doctor` reports is the count a human would give — not twice it,
/// and not the far tree's files added to it.
#[test]
fn doctor_counts_the_files_a_human_would_count() {
    let dir = fixture("count");
    let (code, stdout, stderr) = run(&dir, &["doctor", "--no-php", "tree"]);
    assert_eq!(code, 0, "doctor exits 0 on a clean posture, got {code}\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("2 file(s)"),
        "the tree holds two real .php files; got:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The skipped links are named, with the count and the reason — a silently
/// skipped path is how this class of bug hid for a whole ADR series.
#[test]
fn doctor_reports_the_links_it_refused() {
    let dir = fixture("report");
    let (_, stdout, _) = run(&dir, &["doctor", "--no-php", "tree"]);
    assert!(
        stdout.contains("2 path(s) skipped as symlinks (1 leaving the analyzed tree, 1 re-entering it)"),
        "got:\n{stdout}"
    );
    assert!(stdout.contains("tree/out — leaves the analyzed tree"), "got:\n{stdout}");
    assert!(
        stdout.contains("tree/tree — re-enters a directory already walked"),
        "got:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tree with no symlink at all says nothing about symlinks: the line is
/// posture, and posture that is always printed stops being read.
#[test]
fn doctor_is_silent_when_nothing_was_skipped() {
    let dir = fixture("silent");
    std::fs::remove_file(dir.join("tree/out")).expect("drop the escaping link");
    std::fs::remove_file(dir.join("tree/tree")).expect("drop the re-entering link");
    let (_, stdout, _) = run(&dir, &["doctor", "--no-php", "tree"]);
    assert!(!stdout.contains("skipped as symlinks"), "got:\n{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `check` analyzes each real file once and never the far tree: two findings,
/// one per file, and none of them from `outside/`.
#[test]
fn check_analyzes_each_file_once_and_leaves_the_far_tree_alone() {
    let dir = fixture("check");
    let (code, stdout, stderr) = run(&dir, &["check", "--no-php", "tree"]);
    assert_eq!(code, 1, "findings present, got {code}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    let lines: Vec<&str> = stdout.lines().filter(|l| l.contains("call.on-null")).collect();
    assert_eq!(lines.len(), 2, "one finding per real file, got:\n{stdout}");
    assert!(!stdout.contains("away.php"), "the far tree is not analyzed, got:\n{stdout}");
    assert!(!stdout.contains("tree/tree"), "the re-entry is not analyzed, got:\n{stdout}");
    assert!(!stdout.contains("link.php"), "the file link resolves to a.php, got:\n{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Naming the far tree analyzes it: the boundary is what the user asked for,
/// not a rule about where files may live.
#[test]
fn naming_the_far_tree_analyzes_it() {
    let dir = fixture("named");
    let (code, stdout, _) = run(&dir, &["check", "--no-php", "tree", "outside"]);
    assert_eq!(code, 1, "got:\n{stdout}");
    let lines: Vec<&str> = stdout.lines().filter(|l| l.contains("call.on-null")).collect();
    assert_eq!(lines.len(), 3, "three real files once each, got:\n{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}
