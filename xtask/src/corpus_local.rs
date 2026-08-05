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
//! # optional:
//! exclude = ["cache/**", "assets-origin/**"]
//!
//! # optional: the checkout revision the gate's seeded baseline was measured at.
//! # Recording it is what lets a tripwire distinguish "the analyzer regressed"
//! # from "the corpus moved" (see `gate::classify_revision`). It lives HERE, in
//! # the gitignored file, precisely because a private repo's commit sha must
//! # never enter this repository.
//! revision = "<the sha the baseline was seeded at>"
//!
//! # optional per-project partition declaration (ADR-0047 §7); shape-validated
//! # and IGNORED this slice (Slice A) — Slice E consumes it for the scoped
//! # measurement:
//! [project.partitions]
//! observers = ["tests/**"]
//! [project.partitions.sets]
//! svc-a = ["svc-a/**"]
//! batch = ["batch/**"]
//! ```
//!
//! Local projects are **unmanaged** (they are live working trees — no sync, no
//! lock, nothing this repo can check out) and are consumed only by `fp-gate`.
//! `freq` ignores them entirely so the committed frequency report never contains
//! private-code measurements.
//!
//! The optional `revision` above does not *pin* such a tree the way
//! `corpus.lock.toml` pins a public package — nothing here moves the checkout. It
//! records which state a measurement was taken at, so the gate can say whether a
//! count moved under a constant corpus (a regression) or under a moving one
//! (drift). That distinction previously cost a session's archaeology.

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
    /// Optional: the checkout revision the gate's seeded baseline for this project
    /// was measured at. **Absent is legal** and stays legal — an entry without it
    /// behaves exactly as before, except the gate now says out loud that the
    /// baseline is unpinned. The value is PRIVATE data: it may be printed to the
    /// operator's terminal, never written into a tracked file.
    #[serde(default)]
    pub revision: Option<String>,
    /// Optional per-project partition declaration (ADR-0047 §7), mapped onto the
    /// same shape as `steins.toml [transform.partitions]`. **Parsed and validated
    /// for shape only this slice (ADR-0047 Slice A) — it is NOT consumed by the
    /// gate.** Slice E wires the measurement passthrough that reads it; until then
    /// the fp-gate remains one-universe-per-package (ADR-0047 §7).
    // Deliberately unread this slice — shape-validated passthrough only (Slice E).
    #[allow(dead_code)]
    #[serde(default)]
    pub partitions: Option<PartitionsSpec>,
}

/// The `[project.partitions]` shape on a `corpus.local.toml` entry (ADR-0047 §7):
/// observer globs and a `[project.partitions.sets]` name→glob-list table. This
/// mirrors `steins.toml [transform.partitions]` but is only shape-validated here;
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

/// The revision a local project's working tree is actually sitting on, read by
/// asking git itself (`git -C <path> rev-parse HEAD`).
///
/// Everything that can go wrong degrades to `None` — "unknown" — and nothing here
/// can fail the run: a path that is not a git checkout (git exits nonzero), a
/// missing `git` binary or a spawn failure (`output()` is an `Err`), a detached
/// empty repo with no commit yet, or output that is not a plain hex object name.
/// git's stderr is captured rather than inherited, so a non-checkout does not
/// spray noise into the gate report.
///
/// The returned sha is private data (it names a commit in a private repository):
/// print it to the operator, never persist it into a tracked file.
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
/// A recorded revision matching the measured one does NOT establish that the files
/// just measured are that revision — and on a private corpus, which is somebody's
/// working checkout, a dirty tree is the normal state rather than an edge case.
/// Without this, the one message whose job is to say "stop looking at the corpus,
/// this is a regression" would be issued against a tree that is not exactly the
/// recorded commit.
///
/// Asked as `git -C <path> status --porcelain`, and **any** non-empty output means
/// dirty. Untracked content counts exactly as much as a modification: the gate
/// walks the FILESYSTEM, not the index, so an untracked `.php` file is measured
/// like any other. No attempt is made to be clever about which porcelain status
/// codes matter.
///
/// Degrades exactly as [`checkout_revision`] does — spawn failure, missing `git`,
/// non-zero exit all yield `None` ("unknown"), never a panic and never a changed
/// verdict.
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

/// Collect every `.php` file under `root`, skipping `.git` and any path matched
/// by an `exclude` glob. Directory subtrees whose whole contents are excluded
/// (patterns of the form `<prefix>/**` or `**`) are pruned without descent.
pub fn collect_php_files(root: &Path, excludes: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, root, excludes, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, excludes: &[String], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        // Project-relative, forward-slashed path for glob matching.
        let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if dir_excluded(&rel, excludes) {
                continue;
            }
            walk(root, &path, excludes, out);
        } else if path.extension().is_some_and(|e| e == "php")
            && !excludes.iter().any(|g| glob_match(g, &rel))
        {
            out.push(path);
        }
    }
}

/// Whether a directory subtree can be pruned wholesale: a pattern `<prefix>/**`
/// (or the bare `**`) excludes everything beneath `<prefix>`, so if this dir *is*
/// that prefix we skip it without walking in. Non-`**` patterns that happen to
/// match the dir path itself also prune.
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
        // Missing `exclude` defaults to empty.
        assert_eq!(cfg.projects[1].name, "plugin");
        assert!(cfg.projects[1].exclude.is_empty());
    }

    #[test]
    fn parses_projects_with_and_without_revision() {
        // Synthetic shas only: a real private-corpus revision must never appear in
        // a tracked file, fixtures included.
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
        // Absent is legal and must stay legal — an unrevisioned entry is not an error.
        assert!(cfg.projects[1].revision.is_none());
    }

    #[test]
    fn parses_optional_partitions_passthrough_shape() {
        // ADR-0047 Slice A: the `[project.partitions]` table is shape-validated and
        // carried on the entry, but not consumed by the gate yet (Slice E).
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
        // The degradation contract: anything unreadable is `None` — never a panic,
        // never a failed run. A path that does not exist is the cheapest instance.
        let path = std::env::temp_dir().join("steins-xtask-test-no-such-checkout");
        assert!(checkout_revision(&path).is_none());
    }

    #[test]
    fn dirtiness_of_a_non_git_path_is_unknown_not_assumed_clean() {
        // Same degradation contract as `checkout_revision`, and the direction
        // matters: an unreadable tree must not read as `Some(false)`, which would
        // let the gate assert a regression on a tree it never inspected.
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
