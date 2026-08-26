//! The experimental generation gate reaches a tree that is not under the
//! working directory (issue #506, ADR-0092 §5).
//!
//! The sealed capture keys every file by `strip_prefix(capture_root)`, so
//! passing the process working directory made `steins check /some/tree` from
//! anywhere else fail capture on its first file and drop the whole lifecycle
//! to the cold path with a note. These tests drive the real binary from a
//! working directory that is *not* an ancestor of the analyzed tree — the
//! ordinary CI shape — and pin what the fix buys: the gate engages, a second
//! run is warm with zero reparses, and the findings are byte-identical to the
//! same invocation with the gate off (a cache that changed meaning would be
//! the only real bug here).
//!
//! `--no-php` throughout, so no sidecar is involved and the tests are
//! hermetic: the gate's temperature does not depend on the engine, and both
//! sides of every comparison run under the same posture anyway.

use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Fixture and plumbing.
// ---------------------------------------------------------------------------

/// One finding per file, from the sound subset alone (no engine needed).
const APP: &str = "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n";
const HELPER: &str = "<?php\nfunction area(int $a): int { return $a; }\narea(null);\n";

/// A throwaway directory under the OS temp dir, cleaned on drop. Deliberately
/// not under the repository: a gated run writes `.steins/` beside the code.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-capture-root-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The two-directory, manifest-less fixture: `src/app.php`, `tests/helper.php`.
fn write_fixture(root: &Path) {
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("src/app.php", APP);
    write("tests/helper.php", HELPER);
}

struct Run {
    stdout: String,
    stderr: String,
}

/// `steins check --no-php <paths>` from `cwd`, with the gate on or off.
/// `GITHUB_ACTIONS` is scrubbed like every other CLI test (ADR-0054 §6
/// auto-detection would otherwise change the rendering under CI).
fn check(cwd: &Path, paths: &[&Path], gated: bool) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_steins"));
    cmd.env_remove("GITHUB_ACTIONS").env_remove("STEINS_EXPERIMENTAL_GENERATIONS");
    if gated {
        cmd.env("STEINS_EXPERIMENTAL_GENERATIONS", "1");
    }
    cmd.current_dir(cwd).arg("check").arg("--no-php");
    for path in paths {
        cmd.arg(path);
    }
    let out = cmd.output().expect("run steins");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The orchestrator's own ledger line, the one the oracles read.
fn ledger(run: &Run) -> String {
    run.stderr
        .lines()
        .find(|l| l.starts_with("steins: experimental generations: ") && l.contains(" run, "))
        .unwrap_or_else(|| panic!("no generation ledger on stderr:\n{}", run.stderr))
        .to_owned()
}

fn assert_engaged(run: &Run) {
    assert!(
        !run.stderr.contains("experimental generations unavailable"),
        "the gate degraded instead of engaging:\n{}",
        run.stderr
    );
}

// ---------------------------------------------------------------------------
// The oracles.
// ---------------------------------------------------------------------------

/// Issue #506 proper: an absolute target outside the working directory builds
/// cold, publishes, and warm-rebuilds with zero reparses — and neither run
/// moves a finding.
#[test]
fn an_absolute_out_of_cwd_target_runs_cold_then_warm() {
    let tree = TempDir::new("abs-tree");
    let elsewhere = TempDir::new("abs-cwd");
    write_fixture(&tree.dir);
    let paths = [tree.dir.as_path()];

    let ungated = check(&elsewhere.dir, &paths, false);
    assert!(!ungated.stdout.is_empty(), "the fixture must produce findings");

    let cold = check(&elsewhere.dir, &paths, true);
    assert_engaged(&cold);
    assert!(ledger(&cold).contains("cold run"), "{}", ledger(&cold));
    assert!(
        ledger(&cold).contains("0 file(s) loaded from artifacts, 2 parsed"),
        "{}",
        ledger(&cold)
    );
    assert!(!cold.stderr.contains("(unpublished)"), "the cold build publishes:\n{}", cold.stderr);
    assert_eq!(cold.stdout, ungated.stdout, "the gate must not move a finding");

    let warm = check(&elsewhere.dir, &paths, true);
    assert_engaged(&warm);
    assert!(ledger(&warm).contains("warm run"), "{}", ledger(&warm));
    assert!(
        ledger(&warm).contains("2 file(s) loaded from artifacts, 0 parsed"),
        "the warm run must reparse nothing: {}",
        ledger(&warm)
    );
    assert_eq!(warm.stdout, ungated.stdout, "warm findings are the cold findings");
}

/// Two path arguments capture against their shared parent — the same root the
/// single-argument invocation derives, so the store they share is warm.
#[test]
fn a_two_path_invocation_captures_against_the_shared_parent() {
    let tree = TempDir::new("two-path-tree");
    let elsewhere = TempDir::new("two-path-cwd");
    write_fixture(&tree.dir);
    let (src, tests) = (tree.dir.join("src"), tree.dir.join("tests"));
    let paths = [src.as_path(), tests.as_path()];

    let ungated = check(&elsewhere.dir, &paths, false);
    assert!(!ungated.stdout.is_empty(), "the fixture must produce findings");

    let cold = check(&elsewhere.dir, &paths, true);
    assert_engaged(&cold);
    assert!(ledger(&cold).contains("cold run"), "{}", ledger(&cold));
    assert_eq!(cold.stdout, ungated.stdout, "the gate must not move a finding");

    let warm = check(&elsewhere.dir, &paths, true);
    assert_engaged(&warm);
    assert!(
        ledger(&warm).contains("2 file(s) loaded from artifacts, 0 parsed"),
        "the warm run must reparse nothing: {}",
        ledger(&warm)
    );
    assert_eq!(warm.stdout, ungated.stdout, "warm findings are the cold findings");

    // The whole tree is one root package however it was spelled, so the store
    // the single-argument run would have written is the one this run reuses.
    let single = check(&elsewhere.dir, &[tree.dir.as_path()], true);
    assert!(
        ledger(&single).contains("2 file(s) loaded from artifacts, 0 parsed"),
        "a differently spelled invocation of the same tree stays warm: {}",
        ledger(&single)
    );
}

/// The manifest-less store follows the code, not the caller: `.steins/` lands
/// in the analyzed tree, and the working directory is left untouched.
#[test]
fn a_manifest_less_store_lands_beside_the_code_not_the_caller() {
    let tree = TempDir::new("store-tree");
    let elsewhere = TempDir::new("store-cwd");
    write_fixture(&tree.dir);

    let run = check(&elsewhere.dir, &[tree.dir.as_path()], true);
    assert_engaged(&run);
    assert!(
        tree.dir.join(".steins/gen/CURRENT").is_file(),
        "the store belongs to the tree it caches:\n{}",
        run.stderr
    );
    assert!(
        !elsewhere.dir.join(".steins").exists(),
        "nothing is written beside the caller:\n{}",
        run.stderr
    );
}
