//! Private-corpus injection point (ADR-0013 §4): optional local projects that
//! serve as additional FP gates under the same discipline as the pinned corpus,
//! but whose definitions live *outside* the repo so non-public codebases never
//! land in git.
//!
//! Config file `corpus.local.toml` at the repo root (gitignored), format:
//!
//! ```toml
//! [[project]]
//! name = "monorepo"
//! path = "/absolute/path"
//! exclude = ["cache/**", "assets-origin/**"]  # optional
//!
//! # optional: collect only these subdirectories instead of the whole tree.
//! # Absent (default) walks everything. Exists for a project whose repo also
//! # ships a deliberately-invalid fixture tree, outside the gate's zero-FP bar.
//! paths = ["src"]
//!
//! # optional: checkout revision the seeded baseline was measured at. Lets a
//! # tripwire tell "the analyzer regressed" from "the corpus moved" (see
//! # `gate::classify_revision`). Lives here, gitignored, since a private sha
//! # must never enter this repository.
//! revision = "<the sha the baseline was seeded at>"
//!
//! # optional partition declaration (ADR-0047 §7); shape-validated and IGNORED
//! # this slice (Slice A) — Slice E consumes it for scoped measurement.
//! [project.partitions]
//! observers = ["tests/**"]
//! [project.partitions.sets]
//! svc-a = ["svc-a/**"]
//! batch = ["batch/**"]
//! ```
//!
//! Local projects are **unmanaged** (live working trees — no sync, no lock,
//! nothing this repo can check out) and consumed only by `fp-gate`. `freq`
//! ignores them entirely so the committed frequency report stays private-free.
//!
//! `revision` does not *pin* the tree the way `corpus.lock.toml` pins a public
//! package — nothing here moves the checkout. It records what state a
//! measurement was taken at, so the gate can tell a regression (count moved
//! under a constant corpus) from drift (corpus moved).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::corpus::repo_root;

/// One private project injected into the gate.
#[derive(Debug, Clone, Deserialize)]
pub struct LocalProject {
    /// Display name for the summary table (marked `(local)`).
    pub name: String,
    /// Absolute path to the project's working tree.
    pub path: String,
    /// Glob patterns (see [`glob_match`]) pruning subtrees/files from the walk.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Optional: subdirectories of [`Self::path`] to collect instead of the whole
    /// tree. **Absent means "everything"**. Coarser than [`Self::exclude`] on
    /// purpose: `exclude` prunes noise from an in-scope corpus, `paths` says
    /// which part *is* the corpus — needed when a project ships a
    /// deliberately-invalid fixture tree beside its real source (same
    /// presumption as ADR-0079 §2.3's parser fixtures; otherwise violates the
    /// fp-gate's zero-FP-on-working-code bar).
    ///
    /// Entries are project-relative directory names joined onto `path`; a
    /// missing one contributes nothing. `exclude` globs stay relative to `path`,
    /// not the subtree.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Optional: checkout revision the gate's seeded baseline was measured at.
    /// **Absent is legal** — the gate then says the baseline is unpinned. PRIVATE
    /// data: print to the operator's terminal, never write into a tracked file.
    #[serde(default)]
    pub revision: Option<String>,
    /// Optional per-project partition declaration (ADR-0047 §7), mirroring
    /// `steins.toml [transform.partitions]`. Shape-validated only this slice
    /// (Slice A) — NOT consumed until Slice E; until then fp-gate stays
    /// one-universe-per-package.
    #[allow(dead_code)] // shape-validated passthrough only (Slice E)
    #[serde(default)]
    pub partitions: Option<PartitionsSpec>,
}

/// The `[project.partitions]` shape on a `corpus.local.toml` entry (ADR-0047 §7):
/// observer globs plus a `[project.partitions.sets]` name→glob-list table.
/// Mirrors `steins.toml [transform.partitions]`; shape-validated only here —
/// Slice E builds the region map from it.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)] // fields consumed by Slice E; shape-validated only for now.
pub struct PartitionsSpec {
    /// Observer path-sets (tests, dev-scripts; ADR-0047 §1).
    #[serde(default)]
    pub observers: Vec<String>,
    /// Partition name → glob list.
    #[serde(default)]
    pub sets: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct LocalConfig {
    #[serde(default, rename = "project")]
    projects: Vec<LocalProject>,
}

/// Path to the (optional, gitignored) local-corpus config.
pub fn config_path() -> PathBuf {
    repo_root().join("corpus.local.toml")
}

/// Read `corpus.local.toml`. A missing file is not an error — it yields an empty
/// list (the committed repo has no local projects, so the gate behaves exactly
/// as before). A malformed file *is* an error, surfaced to the caller.
pub fn read_local() -> Result<Vec<LocalProject>, String> {
    read_local_at(&config_path())
}

fn read_local_at(path: &Path) -> Result<Vec<LocalProject>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let cfg: LocalConfig = toml::from_str(&text)
                .map_err(|e| format!("{} is malformed: {e}", path.display()))?;
            Ok(cfg.projects)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// The revision a local project's working tree is actually sitting on
/// (`git -C <path> rev-parse HEAD`).
///
/// Everything that can go wrong degrades to `None` — non-checkout, missing `git`
/// binary, spawn failure, empty repo, non-hex output — nothing fails the run.
/// stderr is captured, not inherited, so a non-checkout stays quiet.
///
/// The returned sha is PRIVATE data: print to the operator, never persist into a
/// tracked file.
pub fn checkout_revision(path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rev = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!rev.is_empty() && rev.chars().all(|c| c.is_ascii_hexdigit())).then_some(rev)
}

/// Whether a local project's working tree carries anything on top of the revision
/// [`checkout_revision`] reports: `Some(true)` dirty, `Some(false)` clean, `None`
/// unknown.
///
/// A matching recorded revision does not prove the measured files ARE that
/// revision — a dirty tree is normal on a private working checkout, not an edge
/// case. Without this check, the "this is a regression, stop looking at the
/// corpus" message could fire against an uncommitted tree.
///
/// Asked as `git -C <path> status --porcelain`; any non-empty output is dirty.
/// Untracked content counts as much as a modification (the gate walks the
/// filesystem, not the index). Degrades like [`checkout_revision`] — spawn
/// failure, missing `git`, non-zero exit all yield `None`, never a panic.
pub fn checkout_is_dirty(path: &Path) -> Option<bool> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!out.stdout.iter().all(u8::is_ascii_whitespace))
}

/// Collect every `.php` file under `root`, skipping any path matched by an
/// `exclude` glob. Subtrees wholly excluded (`<prefix>/**` or `**`) are pruned
/// without descent.
///
/// `subdirs` ([`LocalProject::paths`]) restricts the walk when non-empty; an
/// **empty list walks the whole tree** (the pre-existing behaviour). The walk
/// keeps `root` as glob origin either way, so `exclude` means the same thing
/// under both settings.
///
/// The walk itself is [`steins_db::walk`] — the same one the `steins` binary
/// does (issue #524) — with this function supplying only the exclude filter.
/// The boundary stays `root` even when `subdirs` narrows the walk: a link from
/// one listed subdirectory to another part of the same project has not left the
/// project, and only a link out of the project counts as leaving.
pub fn collect_php_files_in(root: &Path, subdirs: &[String], excludes: &[String]) -> Vec<PathBuf> {
    // Project-relative, forward-slashed path — what an `exclude` glob matches.
    let rel = |path: &Path| {
        path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
    };
    let walk = steins_db::walk::Walk::new()
        .confined_to(root)
        .pruning(|dir: &Path| dir_excluded(&rel(dir), excludes))
        .keeping(|file: &Path| !excludes.iter().any(|g| glob_match(g, &rel(file))));

    let roots: Vec<PathBuf> = if subdirs.is_empty() {
        vec![root.to_path_buf()]
    } else {
        subdirs.iter().map(|sub| root.join(sub)).filter(|dir| dir.is_dir()).collect()
    };
    walk.run(&roots).files
}

/// Whether a directory subtree can be pruned wholesale: `<prefix>/**` (or bare
/// `**`) excludes everything beneath `<prefix>`, so a dir matching that prefix
/// is skipped without descent. A non-`**` pattern matching the dir itself also
/// prunes.
fn dir_excluded(rel: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|g| {
        if g == "**" {
            true
        } else if let Some(prefix) = g.strip_suffix("/**") {
            glob_match(prefix, rel)
        } else {
            glob_match(g, rel)
        }
    })
}

/// A tiny, deliberately-minimal glob matcher for `exclude` patterns. Anchored at
/// both ends (the whole relative path must match the whole pattern). Supports:
///
/// - `*`  — any run of characters **except** the path separator `/`.
/// - `**` — any run of characters **including** `/` (so it spans directories).
///
/// No `?`, character classes, or brace expansion. Patterns and paths use `/`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    matches_from(pattern.as_bytes(), path.as_bytes())
}

fn matches_from(mut pat: &[u8], mut text: &[u8]) -> bool {
    loop {
        match pat.first() {
            None => return text.is_empty(),
            Some(b'*') if pat.get(1) == Some(&b'*') => {
                // `**` (optionally followed by `/`): match the remainder at every
                // suffix of `text`, crossing `/` freely.
                let rest = if pat.get(2) == Some(&b'/') { &pat[3..] } else { &pat[2..] };
                if rest.is_empty() {
                    return true; // trailing `**` matches the rest of the path.
                }
                let mut i = 0;
                loop {
                    if matches_from(rest, &text[i..]) {
                        return true;
                    }
                    if i >= text.len() {
                        return false;
                    }
                    i += 1;
                }
            }
            Some(b'*') => {
                // Single `*`: match a run of non-`/` characters.
                let rest = &pat[1..];
                let mut i = 0;
                loop {
                    if matches_from(rest, &text[i..]) {
                        return true;
                    }
                    if i >= text.len() || text[i] == b'/' {
                        return false;
                    }
                    i += 1;
                }
            }
            Some(&c) => {
                if text.first() == Some(&c) {
                    pat = &pat[1..];
                    text = &text[1..];
                } else {
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_is_empty_not_an_error() {
        // Must not depend on whether the developer's working tree has a real
        // corpus.local.toml (it is gitignored and often present locally).
        let path = std::env::temp_dir().join("steins-xtask-test-no-such-config.toml");
        assert!(read_local_at(&path).expect("missing file is ok").is_empty());
    }

    #[test]
    fn parses_projects_with_and_without_exclude() {
        let cfg: LocalConfig = toml::from_str(
            r#"
            [[project]]
            name = "monorepo"
            path = "/abs/mono"
            exclude = ["cache/**", "assets-origin/**"]

            [[project]]
            name = "plugin"
            path = "/abs/plugin"
            "#,
        )
        .expect("parses");
        assert_eq!(cfg.projects.len(), 2);
        assert_eq!(cfg.projects[0].name, "monorepo");
        assert_eq!(cfg.projects[0].path, "/abs/mono");
        assert_eq!(cfg.projects[0].exclude, vec!["cache/**", "assets-origin/**"]);
        assert_eq!(cfg.projects[1].name, "plugin");
        assert!(cfg.projects[1].exclude.is_empty());
    }

    #[test]
    fn parses_projects_with_and_without_paths() {
        let cfg: LocalConfig = toml::from_str(
            r#"
            [[project]]
            name = "scoped"
            path = "/abs/scoped"
            paths = ["src"]

            [[project]]
            name = "whole"
            path = "/abs/whole"
            "#,
        )
        .expect("parses");
        assert_eq!(cfg.projects[0].paths, vec!["src"]);
        assert!(cfg.projects[1].paths.is_empty()); // absent = whole tree
    }

    #[test]
    fn empty_paths_walks_the_whole_tree_and_a_scope_restricts_it() {
        let root = std::env::temp_dir().join("steins-xtask-test-paths-scope");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/deep")).expect("mkdir src");
        std::fs::create_dir_all(root.join("tests/data")).expect("mkdir tests");
        std::fs::write(root.join("src/a.php"), "<?php").expect("write");
        std::fs::write(root.join("src/deep/b.php"), "<?php").expect("write");
        std::fs::write(root.join("tests/data/broken.php"), "<?php").expect("write");

        let whole = collect_php_files_in(&root, &[], &[]);
        assert_eq!(whole.len(), 3, "no scope walks everything: {whole:?}");

        let scoped = collect_php_files_in(&root, &["src".to_owned()], &[]);
        assert_eq!(scoped.len(), 2, "the scope drops the fixture tree: {scoped:?}");
        assert!(scoped.iter().all(|p| p.starts_with(root.join("src"))));

        // A directory that does not exist contributes nothing, not a failure.
        let missing = collect_php_files_in(&root, &["nope".to_owned()], &[]);
        assert!(missing.is_empty());

        // `exclude` globs stay relative to the ROOT, not to the scoped subtree.
        let both = collect_php_files_in(&root, &["src".to_owned()], &["src/deep/**".to_owned()]);
        assert_eq!(both.len(), 1, "root-relative exclude still applies: {both:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_projects_with_and_without_revision() {
        // Synthetic sha only: a real private-corpus revision must never appear
        // in a tracked file, fixtures included.
        let cfg: LocalConfig = toml::from_str(
            r#"
            [[project]]
            name = "pinned"
            path = "/abs/pinned"
            revision = "0123456789abcdef0123456789abcdef01234567"

            [[project]]
            name = "unpinned"
            path = "/abs/unpinned"
            "#,
        )
        .expect("parses");
        assert_eq!(
            cfg.projects[0].revision.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(cfg.projects[1].revision.is_none()); // absent is legal
    }

    #[test]
    fn parses_optional_partitions_passthrough_shape() {
        // ADR-0047 Slice A: shape-validated and carried, not consumed until Slice E.
        let cfg: LocalConfig = toml::from_str(
            r#"
            [[project]]
            name = "monorepo"
            path = "/abs/mono"

            [project.partitions]
            observers = ["tests/**", "dev-script/**"]

            [project.partitions.sets]
            svc-a = ["svc-a/**"]
            batch = ["batch/**"]
            "#,
        )
        .expect("partitions passthrough parses");
        let p = cfg.projects[0].partitions.as_ref().expect("partitions present");
        assert_eq!(p.observers, vec!["tests/**", "dev-script/**"]);
        assert_eq!(p.sets.len(), 2);
        assert_eq!(p.sets["svc-a"], vec!["svc-a/**"]);
        assert_eq!(p.sets["batch"], vec!["batch/**"]);
    }

    #[test]
    fn partitions_default_to_none_when_absent() {
        let cfg: LocalConfig = toml::from_str(
            r#"
            [[project]]
            name = "plain"
            path = "/abs/plain"
            "#,
        )
        .expect("parses");
        assert!(cfg.projects[0].partitions.is_none());
    }

    #[test]
    fn empty_config_yields_no_projects() {
        let cfg: LocalConfig = toml::from_str("").expect("empty parses");
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn checkout_revision_of_a_non_git_path_is_unknown_not_a_failure() {
        // Degradation contract: unreadable is `None`, never a panic.
        let path = std::env::temp_dir().join("steins-xtask-test-no-such-checkout");
        assert!(checkout_revision(&path).is_none());
    }

    #[test]
    fn dirtiness_of_a_non_git_path_is_unknown_not_assumed_clean() {
        // Direction matters: unreadable must not read as `Some(false)`, or the
        // gate could assert a regression on a tree it never inspected.
        let path = std::env::temp_dir().join("steins-xtask-test-no-such-checkout");
        assert_eq!(checkout_is_dirty(&path), None);
    }

    #[test]
    fn glob_star_stays_within_a_segment() {
        assert!(glob_match("*.php", "foo.php"));
        assert!(!glob_match("*.php", "sub/foo.php")); // `*` does not cross `/`
        assert!(glob_match("src/*.php", "src/a.php"));
        assert!(!glob_match("src/*.php", "src/deep/a.php"));
    }

    #[test]
    fn glob_double_star_crosses_segments() {
        assert!(glob_match("cache/**", "cache/foo.php"));
        assert!(glob_match("cache/**", "cache/deep/nested/foo.php"));
        assert!(!glob_match("cache/**", "other/foo.php"));
        assert!(!glob_match("cache/**", "cache.php")); // needs the `/`
        assert!(glob_match("**/generated.php", "a/b/generated.php"));
        assert!(glob_match("**/generated.php", "generated.php"));
        assert!(glob_match("**", "anything/at/all.php"));
    }

    #[test]
    fn dir_pruning_matches_prefix_of_double_star() {
        assert!(dir_excluded("cache", &["cache/**".to_owned()]));
        assert!(dir_excluded("assets-origin", &["assets-origin/**".to_owned()]));
        assert!(!dir_excluded("src", &["cache/**".to_owned()]));
        assert!(dir_excluded("anything", &["**".to_owned()]));
    }
}
