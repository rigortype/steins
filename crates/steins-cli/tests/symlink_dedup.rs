//! Regression tests for issue #179: `collect_php_files` deduped the file list by
//! path STRING, so a tree reachable via a directory symlink under two spellings
//! (`mirror/src -> ../src`) was ingested twice. Declaration-dependent findings
//! vanished (the absence-family existence guard, ADR-0049, read the duplicated
//! hierarchy as non-enumerable); flow-derived findings survived but doubled.
//!
//! Fix: `dedup_canonical` in `crates/steins-cli/src/main.rs` dedups by
//! [`std::path::Path::canonicalize`], keeping whichever spelling came first
//! (argument order, then walk order).

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Spawns with `GITHUB_ACTIONS` scrubbed so `check`'s CI auto-detection
/// (ADR-0054 §6) doesn't emit workflow commands where a test expects text.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

/// A fresh, unique working directory under the system temp dir.
fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-symlinkdedup-{}-{tag}-{n}", std::process::id()));
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
    let out = steins_cmd().args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).expect("create parent");
    std::fs::write(&p, contents).expect("write fixture");
    p
}

/// `src/a.php`: ArgumentCountError (ctor wants 3 args, called with 1) — the issue's
/// repro. DECLARATION-dependent: needs the class-like enumerated once; #179 dropped it.
const ARITY_SRC: &str = "<?php
class Mapper {
    public function __construct(private int $a, private int $b, private int $c) {}
}
new Mapper(1);
";

/// `src/a.php`: proven-null receiver in `=== null` guard — `call.on-null` (ADR-0031).
/// FLOW-derived not declaration-dependent: #179 let it survive but reported it twice.
const FLOW_SRC: &str = "<?php
class U { public function name(): string { return \"x\"; } }
function f($u): void {
    if ($u === null) { $u->name(); }
}
";

/// `<dir>/mirror/src -> ../src`: `src` and `mirror/src` name the same tree.
fn setup_mirrored_tree(dir: &Path, contents: &str) {
    write(dir, "src/a.php", contents);
    std::fs::create_dir_all(dir.join("mirror")).expect("create mirror dir");
    symlink("../src", dir.join("mirror/src")).expect("create mirror/src symlink");
}

// (1) declaration-dependent finding: same single finding with/without the mirror argument

#[test]
fn arity_finding_survives_and_stays_singular_through_a_symlinked_mirror() {
    let dir = workdir("arity");
    setup_mirrored_tree(&dir, ARITY_SRC);

    let direct = run_in(&dir, &["check", "src"]);
    assert_eq!(
        direct.code, 1,
        "direct: arity finding present, got:\n{}\nstderr:\n{}",
        direct.stdout, direct.stderr
    );
    let direct_lines: Vec<&str> = direct.stdout.lines().collect();
    assert_eq!(direct_lines.len(), 1, "exactly one finding, got:\n{}", direct.stdout);
    assert!(direct.stdout.contains("call.too-few-arguments"), "got:\n{}", direct.stdout);

    // Pre-#179: vanished silently — exit 0, empty stdout.
    let mirrored = run_in(&dir, &["check", "src", "mirror"]);
    assert_eq!(
        mirrored.code, 1,
        "mirrored: the arity finding must survive the symlinked duplicate, got exit {} stdout:\n{}\nstderr:\n{}",
        mirrored.code, mirrored.stdout, mirrored.stderr
    );
    let mirrored_lines: Vec<&str> = mirrored.stdout.lines().collect();
    assert_eq!(
        mirrored_lines.len(),
        1,
        "exactly one finding, not one per spelling, got:\n{}",
        mirrored.stdout
    );
    assert!(mirrored.stdout.contains("call.too-few-arguments"), "got:\n{}", mirrored.stdout);
}

// (2) flow-derived finding: reported once, not twice

#[test]
fn flow_derived_finding_reachable_through_two_paths_reports_once() {
    let dir = workdir("flow");
    setup_mirrored_tree(&dir, FLOW_SRC);

    let direct = run_in(&dir, &["check", "src"]);
    assert_eq!(direct.code, 1, "direct: call.on-null present, got:\n{}", direct.stdout);
    assert_eq!(direct.stdout.lines().count(), 1, "one finding, got:\n{}", direct.stdout);

    // Pre-#179: survived (flow-derived) but reported TWICE, once per spelling.
    let mirrored = run_in(&dir, &["check", "src", "mirror"]);
    assert_eq!(mirrored.code, 1, "mirrored: call.on-null present, got:\n{}", mirrored.stdout);
    let lines: Vec<&str> = mirrored.stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one call.on-null, not one per spelling (src/a.php AND mirror/src/a.php), got:\n{}",
        mirrored.stdout
    );
    assert!(mirrored.stdout.contains("call.on-null"), "got:\n{}", mirrored.stdout);
}

// (3) the reported path is stable: first argument order wins

#[test]
fn reported_path_is_the_first_spelling_in_argument_order() {
    let dir = workdir("stable");
    setup_mirrored_tree(&dir, ARITY_SRC);

    let src_first = run_in(&dir, &["check", "src", "mirror"]);
    assert_eq!(src_first.code, 1, "got:\n{}", src_first.stdout);
    assert!(
        src_first.stdout.starts_with("src/a.php:"),
        "`src` named first → reported path is `src/a.php`, got:\n{}",
        src_first.stdout
    );
    assert!(!src_first.stdout.contains("mirror/src/a.php"), "got:\n{}", src_first.stdout);

    // Argument order decides, not sort/canonical order — same either way given.
    let mirror_first = run_in(&dir, &["check", "mirror", "src"]);
    assert_eq!(mirror_first.code, 1, "got:\n{}", mirror_first.stdout);
    assert!(
        mirror_first.stdout.starts_with("mirror/src/a.php:"),
        "`mirror` named first → reported path is `mirror/src/a.php`, got:\n{}",
        mirror_first.stdout
    );
    assert!(!mirror_first.stdout.contains("\nsrc/a.php"), "got:\n{}", mirror_first.stdout);
}

// Directory symlink cycle: the walker must terminate

#[test]
fn directory_symlink_cycle_does_not_hang_or_crash() {
    let dir = workdir("cycle");
    write(&dir, "src/a.php", ARITY_SRC);
    // `src/self -> .`: infinite symlink cycle without a walker cycle guard (#179).
    symlink(".", dir.join("src/self")).expect("create self-referential symlink");

    let r = run_in(&dir, &["check", "src"]);
    assert_eq!(r.code, 1, "the real finding still surfaces, got:\n{}", r.stdout);
    assert_eq!(r.stdout.lines().count(), 1, "one finding, not one per cycle traversal, got:\n{}", r.stdout);
}
