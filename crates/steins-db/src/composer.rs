//! Reading the project's own `composer.json` — the IO boundary that produces a
//! [`ProjectLayout`] (ADR-0015).
//!
//! Everything here runs once, before any salsa input is set. The value it
//! produces is pure and is carried as a [`crate::Project`] input, so a replay
//! with the same inputs gives the same answer without re-reading the filesystem
//! (ADR-0048's canonical entry state).
//!
//! # What is read
//!
//! - `config.vendor-dir` — where Composer installs dependencies. Default
//!   `vendor`. This is the field that makes a tree vendoring into `3rdparty/`
//!   legible.
//! - `autoload` and `autoload-dev` — the `psr-4`, `psr-0` and `classmap`
//!   directories. These are the project's own code, and they are what stops a
//!   first-party `src/vendor/` from being disowned by the directory-name floor.
//!
//! `autoload.files` is deliberately not read: it names individual files, and
//! promoting their parent directory to a first-party root would claim more than
//! the manifest says.
//!
//! # What is walked
//!
//! Upward from each analyzed path to its nearest ancestor manifest — that is the
//! project root when someone runs `steins check src/`. Then downward through the
//! analyzed paths, so a monorepo's subproject manifests each govern their own
//! subtree.
//!
//! The downward walk does **not** descend into a directory named `vendor`, into a
//! vendor root already declared by an enclosing manifest, or into `.git` /
//! `node_modules`. A dependency's own `composer.json` is not a governing root,
//! and there are thousands of them.
//!
//! A manifest that cannot be read or parsed is skipped, not fatal: the paths it
//! would have governed fall through to the floor, which is where they were
//! before this existed.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::layout::{GoverningRoot, ProjectLayout, normalize};

/// Directory names the downward walk never descends into. `vendor` is the floor's
/// own spelling; the other two are large and never carry a governing manifest.
const NEVER_DESCEND: &[&str] = &["vendor", ".git", "node_modules"];

/// Discover the layout governing `paths`, resolving relative paths against `cwd`.
///
/// Returns [`ProjectLayout::fallback`] when no manifest is found — the behavior
/// that predates this module, and an honest answer for a tree that is not a
/// Composer project at all.
#[must_use]
pub fn discover(paths: &[PathBuf], cwd: &Path) -> ProjectLayout {
    let seeds = seed_dirs(paths, cwd);
    let mut roots: Vec<GoverningRoot> = Vec::new();

    // Upward: the nearest ancestor manifest of each analyzed path. This is what
    // finds the project root when only a subdirectory was named.
    for seed in &seeds {
        for ancestor in seed.ancestors() {
            let manifest = ancestor.join("composer.json");
            if manifest.is_file() {
                push_root(&mut roots, &manifest, ancestor);
                break;
            }
        }
    }

    // Downward: subproject manifests, parents before children so an enclosing
    // root's vendor declaration can prune the walk beneath it.
    let mut queue: VecDeque<PathBuf> = seeds.iter().cloned().collect();
    // A set, not a list: the walk visits every directory in the analyzed tree, and
    // a linear membership test makes that quadratic on a monorepo.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    while let Some(dir) = queue.pop_front() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let manifest = dir.join("composer.json");
        if manifest.is_file() {
            push_root(&mut roots, &manifest, &dir);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let child = entry.path();
            if !child.is_dir() || is_pruned(&child, &roots) {
                continue;
            }
            queue.push_back(child);
        }
    }

    if roots.is_empty() {
        return ProjectLayout::fallback();
    }
    ProjectLayout::new(cwd.to_path_buf(), roots)
}

/// The directories the walk starts from: each analyzed path, absolutized and
/// normalized, with a file path standing in for its parent directory.
fn seed_dirs(paths: &[PathBuf], cwd: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        let abs = if p.is_absolute() { normalize(p) } else { normalize(&cwd.join(p)) };
        let dir = if abs.is_dir() { abs } else { abs.parent().map_or(abs.clone(), Path::to_path_buf) };
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

/// Whether the downward walk should skip `dir`: a never-descend name, or a
/// vendor root some already-discovered manifest declared.
fn is_pruned(dir: &Path, roots: &[GoverningRoot]) -> bool {
    if dir.file_name().is_some_and(|n| NEVER_DESCEND.iter().any(|s| n == *s)) {
        return true;
    }
    roots.iter().flat_map(GoverningRoot::vendor_roots).any(|v| dir.starts_with(v))
}

/// Parse `manifest` and append the root it declares, unless `dir` already has one
/// (the upward and downward walks overlap at the project root).
fn push_root(roots: &mut Vec<GoverningRoot>, manifest: &Path, dir: &Path) {
    if roots.iter().any(|r| r.dir() == dir) {
        return;
    }
    if let Some(root) = read_root(manifest, dir) {
        roots.push(root);
    }
}

/// Read one `composer.json` into a [`GoverningRoot`]. `None` when the file cannot
/// be read or is not a JSON object — the paths it would govern then fall through
/// to the floor.
fn read_root(manifest: &Path, dir: &Path) -> Option<GoverningRoot> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    if !json.is_object() {
        return None;
    }

    let vendor_dir = json.get("config").and_then(|c| c.get("vendor-dir")).and_then(Value::as_str).unwrap_or("vendor");
    let vendor = vec![resolve(dir, vendor_dir)];

    let mut first_party: Vec<PathBuf> = Vec::new();
    for section in ["autoload", "autoload-dev"] {
        let Some(block) = json.get(section) else { continue };
        for key in ["psr-4", "psr-0"] {
            let Some(map) = block.get(key).and_then(Value::as_object) else { continue };
            for value in map.values() {
                collect_dirs(value, dir, &mut first_party);
            }
        }
        if let Some(list) = block.get("classmap") {
            collect_dirs(list, dir, &mut first_party);
        }
    }
    first_party.sort();
    first_party.dedup();

    Some(GoverningRoot::new(manifest.to_path_buf(), dir.to_path_buf(), vendor, first_party))
}

/// Append the directories named by an autoload value — a string, or an array of
/// them — resolved against `dir`.
fn collect_dirs(value: &Value, dir: &Path, out: &mut Vec<PathBuf>) {
    match value {
        Value::String(s) => out.push(resolve(dir, s)),
        Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    out.push(resolve(dir, s));
                }
            }
        }
        _ => {}
    }
}

/// Resolve a manifest-relative path. An empty or `.` value means the manifest's
/// own directory, which Composer spells `""` or `"./"`.
fn resolve(dir: &Path, rel: &str) -> PathBuf {
    let trimmed = rel.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return normalize(dir);
    }
    normalize(&dir.join(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree under the process's temp dir, removed on drop.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("steins-composer-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // The temp dir itself may be a symlink (macOS `/tmp`); the layout
            // compares lexically, so anchor on the same spelling it will see.
            Self(dir.canonicalize().unwrap())
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn dir(&self, rel: &str) {
            std::fs::create_dir_all(self.0.join(rel)).unwrap();
        }

        fn path(&self, rel: &str) -> String {
            self.0.join(rel).to_string_lossy().into_owned()
        }

        fn layout(&self) -> ProjectLayout {
            discover(std::slice::from_ref(&self.0), &self.0)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_custom_vendor_dir_is_read() {
        let t = Tree::new("custom-vendor");
        t.write("composer.json", r#"{"config":{"vendor-dir":"3rdparty"},"autoload":{"psr-4":{"NC\\":"lib/"}}}"#);
        t.dir("3rdparty/pkg");
        t.dir("lib");
        let l = t.layout();
        assert!(!l.is_fallback());
        assert!(l.is_vendor(&t.path("3rdparty/pkg/Lib.php")));
        assert!(!l.is_vendor(&t.path("lib/App.php")));
    }

    #[test]
    fn an_autoload_root_protects_a_first_party_vendor_directory() {
        let t = Tree::new("first-party-vendor");
        t.write("composer.json", r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#);
        t.dir("src/vendor");
        t.dir("vendor/pkg");
        let l = t.layout();
        assert!(!l.is_vendor(&t.path("src/vendor/Money.php")));
        assert!(l.is_vendor(&t.path("vendor/pkg/Lib.php")));
    }

    #[test]
    fn a_subproject_manifest_governs_its_own_subtree() {
        let t = Tree::new("monorepo");
        t.write("composer.json", r#"{"autoload":{"psr-4":{"Root\\":"src/"}}}"#);
        t.write("sub/composer.json", r#"{"config":{"vendor-dir":"deps"},"autoload":{"psr-4":{"Sub\\":"lib/"}}}"#);
        t.dir("sub/deps/pkg");
        t.dir("sub/lib");
        let l = t.layout();
        assert_eq!(l.roots().len(), 2);
        assert!(l.is_vendor(&t.path("sub/deps/pkg/Lib.php")));
        assert!(!l.is_vendor(&t.path("sub/lib/App.php")));
    }

    #[test]
    fn dependency_manifests_are_not_governing_roots() {
        let t = Tree::new("prune-vendor");
        t.write("composer.json", "{}");
        t.write("vendor/acme/lib/composer.json", r#"{"autoload":{"psr-4":{"Acme\\":"src/"}}}"#);
        let l = t.layout();
        assert_eq!(l.roots().len(), 1, "only the project's own manifest governs");
        assert!(l.is_vendor(&t.path("vendor/acme/lib/src/Lib.php")));
    }

    #[test]
    fn a_manifest_above_the_analyzed_path_is_found() {
        let t = Tree::new("upward");
        t.write("composer.json", r#"{"config":{"vendor-dir":"3rdparty"},"autoload":{"psr-4":{"App\\":"src/"}}}"#);
        t.dir("src/Sub");
        t.dir("3rdparty");
        let l = discover(&[t.0.join("src")], &t.0);
        assert!(!l.is_fallback());
        assert!(l.is_vendor(&t.path("3rdparty/pkg/Lib.php")));
    }

    #[test]
    fn an_unparseable_manifest_falls_back() {
        let t = Tree::new("broken");
        t.write("composer.json", "{ this is not json");
        t.dir("vendor/pkg");
        let l = t.layout();
        assert!(l.is_fallback());
        assert!(l.is_vendor(&t.path("vendor/pkg/Lib.php")), "the floor still answers");
    }

    #[test]
    fn no_manifest_at_all_is_the_fallback_layout() {
        let t = Tree::new("no-manifest");
        t.dir("src");
        assert!(t.layout().is_fallback());
    }
}
