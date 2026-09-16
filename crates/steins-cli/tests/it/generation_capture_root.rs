//! The generation cache as `steins check` ships it (ADR-0092 §5, ADR-0020
//! amendment / issue #525): on by default, `--no-cache` to opt out, and silent
//! either way.
//!
//! Two properties are pinned here, and only one of them is about caching.
//!
//! **It must not move a finding.** Every oracle below compares the cached run's
//! stdout against the same invocation's `--no-cache` stdout. A cache that
//! changed meaning would be the only real bug in this file.
//!
//! **It must not say anything.** Since the lifecycle became the default, a
//! normal run's stderr has to be the stderr of a run that never had a store:
//! the disposition of a cache is cost, and cost is not news. That is why these
//! tests read the store on disk rather than a stderr ledger — the ledger is
//! gone on purpose, and `steins doctor`'s store section is where the
//! disposition went.
//!
//! The reach-outside-the-cwd case is issue #506's: the sealed capture keys
//! every file by `strip_prefix(capture_root)`, so passing the process working
//! directory made `steins check /some/tree` from anywhere else fail capture on
//! its first file and drop the whole lifecycle. These tests drive the real
//! binary from a working directory that is *not* an ancestor of the analyzed
//! tree — the ordinary CI shape.
//!
//! `--no-php` throughout, so no sidecar is involved and the tests are
//! hermetic: the cache's temperature does not depend on the engine, and both
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
/// not under the repository: a cached run writes `.steins/` beside the code.
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
        // A read-only fixture has to be made writable again or the cleanup
        // silently leaks the directory.
        restore_write(&self.dir);
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

/// `steins check --no-php [--no-cache] <paths>` from `cwd`. `GITHUB_ACTIONS`
/// is scrubbed like every other CLI test (ADR-0054 §6 auto-detection would
/// otherwise change the rendering under CI).
fn check(cwd: &Path, paths: &[&Path], cached: bool) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_steins"));
    cmd.env_remove("GITHUB_ACTIONS");
    cmd.current_dir(cwd).arg("check").arg("--no-php");
    if !cached {
        cmd.arg("--no-cache");
    }
    for path in paths {
        cmd.arg(path);
    }
    let out = cmd.output().expect("run steins");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The generation `CURRENT` names, or `None` when nothing was published.
fn current_generation(tree: &Path) -> Option<String> {
    std::fs::read_to_string(tree.join(".steins/gen/CURRENT"))
        .ok()
        .map(|s| s.trim().to_owned())
}

/// How many published generations the store holds — one, on a tree whose
/// sources never moved, however many times it was checked.
fn generation_count(tree: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(tree.join(".steins/gen")) else { return 0 };
    entries
        .flatten()
        .filter(|e| {
            e.file_type().is_ok_and(|t| t.is_dir())
                && e.file_name().to_str().is_some_and(|n| n.len() == 64)
        })
        .count()
}

/// Drop every write bit under `dir` (and on `dir` itself), deepest first.
#[cfg(unix)]
fn drop_write(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                drop_write(&path);
            } else if let Ok(meta) = std::fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() & !0o222);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }
    }
    if let Ok(meta) = std::fs::metadata(dir) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() & !0o222);
        let _ = std::fs::set_permissions(dir, perms);
    }
}

/// Put the owner's write bit back, shallowest first, so the fixture can be
/// removed again.
#[cfg(unix)]
fn restore_write(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(dir) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o200);
        let _ = std::fs::set_permissions(dir, perms);
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                restore_write(&path);
            } else if let Ok(meta) = std::fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o200);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }
    }
}

#[cfg(not(unix))]
fn restore_write(_dir: &Path) {}

// ---------------------------------------------------------------------------
// The oracles.
// ---------------------------------------------------------------------------

/// Issue #506 proper: an absolute target outside the working directory builds
/// cold, publishes, and stays warm on the next run — and neither run moves a
/// finding or says a word.
#[test]
fn an_absolute_out_of_cwd_target_runs_cold_then_warm() {
    let tree = TempDir::new("abs-tree");
    let elsewhere = TempDir::new("abs-cwd");
    write_fixture(&tree.dir);
    let paths = [tree.dir.as_path()];

    let uncached = check(&elsewhere.dir, &paths, false);
    assert!(!uncached.stdout.is_empty(), "the fixture must produce findings");
    assert!(
        !tree.dir.join(".steins").exists(),
        "--no-cache must not build a store:\n{}",
        uncached.stderr
    );

    let cold = check(&elsewhere.dir, &paths, true);
    let published = current_generation(&tree.dir).expect("the cold run publishes a generation");
    assert_eq!(cold.stdout, uncached.stdout, "the cache must not move a finding");
    assert_eq!(cold.stderr, uncached.stderr, "the cache must not add a word to stderr");

    let warm = check(&elsewhere.dir, &paths, true);
    assert_eq!(warm.stdout, uncached.stdout, "warm findings are the uncached findings");
    assert_eq!(warm.stderr, uncached.stderr, "a warm run is as quiet as a cold one");
    // Nothing moved, so there is nothing to republish: same generation, and no
    // second one beside it. A run that had reparsed would have fingerprinted a
    // new identity and published under it.
    assert_eq!(
        current_generation(&tree.dir).as_deref(),
        Some(published.as_str()),
        "an unchanged tree keeps its generation"
    );
    assert_eq!(generation_count(&tree.dir), 1, "an unchanged tree publishes nothing new");
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

    let uncached = check(&elsewhere.dir, &paths, false);
    assert!(!uncached.stdout.is_empty(), "the fixture must produce findings");

    let cold = check(&elsewhere.dir, &paths, true);
    let published = current_generation(&tree.dir).expect("the cold run publishes a generation");
    assert_eq!(cold.stdout, uncached.stdout, "the cache must not move a finding");

    let warm = check(&elsewhere.dir, &paths, true);
    assert_eq!(warm.stdout, uncached.stdout, "warm findings are the uncached findings");
    assert_eq!(generation_count(&tree.dir), 1, "an unchanged tree publishes nothing new");

    // The whole tree is one root package however it was spelled, so the store
    // the single-argument run would have written is the one this run reuses.
    let single = check(&elsewhere.dir, &[tree.dir.as_path()], true);
    assert!(!single.stdout.is_empty(), "the differently spelled run still reports");
    assert_eq!(
        current_generation(&tree.dir).as_deref(),
        Some(published.as_str()),
        "a differently spelled invocation of the same tree stays warm"
    );
    assert_eq!(generation_count(&tree.dir), 1, "…and publishes nothing new either");
}

/// The manifest-less store follows the code, not the caller: `.steins/` lands
/// in the analyzed tree, and the working directory is left untouched.
#[test]
fn a_manifest_less_store_lands_beside_the_code_not_the_caller() {
    let tree = TempDir::new("store-tree");
    let elsewhere = TempDir::new("store-cwd");
    write_fixture(&tree.dir);

    let run = check(&elsewhere.dir, &[tree.dir.as_path()], true);
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

/// A cache the tool wrote unasked must not become a commit the user did not
/// ask for (issue #525): creating the store drops a `.gitignore` holding `*`,
/// the way Cargo does for `target/`.
#[test]
fn creating_the_store_writes_a_gitignore() {
    let tree = TempDir::new("gitignore-tree");
    let elsewhere = TempDir::new("gitignore-cwd");
    write_fixture(&tree.dir);

    check(&elsewhere.dir, &[tree.dir.as_path()], true);
    let ignore = tree.dir.join(".steins/.gitignore");
    assert_eq!(
        std::fs::read_to_string(&ignore).expect("the store writes its own .gitignore"),
        "*\n"
    );

    // A user who edited it keeps their edit: the file is written once, at
    // creation, never re-asserted.
    std::fs::write(&ignore, "# mine\n").unwrap();
    check(&elsewhere.dir, &[tree.dir.as_path()], true);
    assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "# mine\n");
}

/// A read-only project degrades to cold **quietly** (issue #525): the store
/// cannot be created, so there is no cache — and a cache that cannot exist is
/// a cost, not a diagnosis. Same findings, same stderr, same exit, nothing
/// written.
#[cfg(unix)]
#[test]
fn a_read_only_project_degrades_quietly_to_cold() {
    let tree = TempDir::new("readonly-tree");
    let elsewhere = TempDir::new("readonly-cwd");
    write_fixture(&tree.dir);

    let uncached = check(&elsewhere.dir, &[tree.dir.as_path()], false);
    assert!(!uncached.stdout.is_empty(), "the fixture must produce findings");

    drop_write(&tree.dir);
    let cached = check(&elsewhere.dir, &[tree.dir.as_path()], true);
    assert_eq!(cached.stdout, uncached.stdout, "an unwritable tree still reports its findings");
    assert_eq!(
        cached.stderr, uncached.stderr,
        "a store that cannot be created is not news; stderr was:\n{}",
        cached.stderr
    );
    assert!(!tree.dir.join(".steins").exists(), "nothing was written into a read-only tree");
}

/// An edit republishes: `CURRENT` moves to a new generation, the one it
/// replaced is swept (issue #529 — the store is bounded at one), and the
/// findings follow the source rather than the cache.
#[test]
fn an_edit_publishes_a_new_generation_and_new_findings() {
    let tree = TempDir::new("edit-tree");
    let elsewhere = TempDir::new("edit-cwd");
    write_fixture(&tree.dir);

    check(&elsewhere.dir, &[tree.dir.as_path()], true);
    let first = current_generation(&tree.dir).expect("the cold run publishes");

    // The call that was wrong becomes right: its finding must disappear.
    std::fs::write(
        tree.dir.join("src/app.php"),
        "<?php\nfunction width(int $w): int { return $w; }\nwidth(5);\n",
    )
    .unwrap();

    let after = check(&elsewhere.dir, &[tree.dir.as_path()], true);
    let uncached = check(&elsewhere.dir, &[tree.dir.as_path()], false);
    assert_eq!(after.stdout, uncached.stdout, "the warm run reports the edited truth");
    assert!(
        !after.stdout.contains("src/app.php"),
        "the fixed call must stop being reported:\n{}",
        after.stdout
    );
    assert_ne!(
        current_generation(&tree.dir).as_deref(),
        Some(first.as_str()),
        "an edit publishes a new generation"
    );
    assert_eq!(
        generation_count(&tree.dir),
        1,
        "…and the generation it replaced goes with it (issue #529)"
    );
}

/// The growth table of issue #529, through the binary: five edits to one file
/// used to leave five generations and 26 MB. The store must stay at exactly
/// one generation however long the editing goes on.
#[test]
fn repeated_edits_do_not_grow_the_store() {
    let tree = TempDir::new("growth-tree");
    let elsewhere = TempDir::new("growth-cwd");
    write_fixture(&tree.dir);

    check(&elsewhere.dir, &[tree.dir.as_path()], true);
    let mut previous = current_generation(&tree.dir).expect("the cold run publishes");
    for edit in 0..5 {
        std::fs::write(
            tree.dir.join("src/app.php"),
            format!(
                "<?php\n// edit {edit}\nfunction width(int $w): int {{ return $w; }}\nwidth(\"abc\");\n"
            ),
        )
        .unwrap();
        let run = check(&elsewhere.dir, &[tree.dir.as_path()], true);
        assert!(!run.stdout.is_empty(), "the fixture still reports after edit {edit}");
        let now = current_generation(&tree.dir).expect("each edit publishes");
        assert_ne!(now, previous, "edit {edit} must publish a new generation");
        assert_eq!(generation_count(&tree.dir), 1, "edit {edit} left a generation behind");
        previous = now;
    }
}
