//! Project-global dynamic-code obstacle detection (ADR-0046 §2).
//!
//! The reverse call-site sweep proves "every caller is accounted for" from the
//! CST. Two dynamic-code constructs defeat that enumeration invisibly:
//!
//! - `eval('foo(42)')` calls `foo` with **no CST call site** — the string-value
//!   reference scan (exact-name match) cannot see it.
//! - a dynamic or out-of-universe `include`/`require` pulls in code the project
//!   never indexed (a compiled-template cache), which can define or call anything.
//!
//! Either makes "all callers proven" false project-wide, so — while one stands —
//! *every* candidate refuses. The obstacles are recorded ONCE per run (with the
//! full offending-site list) rather than duplicated onto each refusal.
//!
//! ## Vendor presumption (ADR-0046)
//! `eval`/dynamic-include inside a `vendor/` path is composer autoload plumbing,
//! presumed universe-internal — a documented, rebuttable soundness trade. Without
//! it every composer project would refuse everything. Non-vendor sites are
//! obstacles.
//!
//! ## The vouching valve
//! A user may vouch specific sites (`steins.toml`, [`VouchSet`]); a vouched site
//! does not raise its obstacle. Vouching does not silently pass: the vouched sites
//! flow to [`TransformReport::vouched_exemptions`], downgrading the completeness
//! claim to "conditional on N user-vouched dynamic-code exemptions" (ADR-0037: a
//! user assertion is a trust stratum, and the proof says so).

use std::collections::HashSet;

use steins_db::{Db, Project, parse};
use steins_infer::is_vendor_path;
use steins_syntax::{DynamismKind, IncludePath};

use crate::common::{REASON_DYNAMIC_INCLUDE, REASON_EVAL_PRESENT};
use crate::transform::{Obstacle, SiteRef};

/// The set of sites a user vouched as safe in `steins.toml` (`file:line`).
///
/// Matching is path-suffix tolerant: a vouch `legacy.php:42` matches a site whose
/// path ends with `/legacy.php` (or equals it) on the same line, so the config
/// need not repeat the exact on-disk prefix the CLI was invoked with.
#[derive(Debug, Clone, Default)]
pub struct VouchSet {
    entries: Vec<VouchEntry>,
}

#[derive(Debug, Clone)]
struct VouchEntry {
    file: String,
    line: u32,
    /// Set once the entry matches at least one *raised* obstacle site — an
    /// unmatched entry is a no-op the CLI warns about.
    matched: std::cell::Cell<bool>,
}

impl VouchSet {
    /// An empty vouch set (no exemptions) — the default for a run with no config.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a vouch set from raw `file:line` strings. Malformed entries (no `:`,
    /// a non-numeric line) are dropped; the caller may pre-validate for a warning.
    #[must_use]
    pub fn from_entries(entries: impl IntoIterator<Item = (String, u32)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(file, line)| VouchEntry { file, line, matched: std::cell::Cell::new(false) })
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `(path, line)` is vouched. Records the match so unused entries can
    /// be reported afterward.
    fn covers(&self, path: &str, line: u32) -> bool {
        let mut hit = false;
        for e in &self.entries {
            if e.line == line && path_suffix_match(path, &e.file) {
                e.matched.set(true);
                hit = true;
            }
        }
        hit
    }

    /// The `file:line` spellings of vouch entries that matched no raised obstacle
    /// site — vouching an already-benign (or nonexistent) site is a no-op warning.
    #[must_use]
    pub fn unused(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| !e.matched.get())
            .map(|e| format!("{}:{}", e.file, e.line))
            .collect()
    }
}

/// Whether two paths name the same file up to a leading-directory prefix.
fn path_suffix_match(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// The result of scanning a project for dynamic-code obstacles.
#[derive(Debug, Clone, Default)]
pub struct DynamismObstacles {
    /// Obstacles still standing (each reason with ≥1 unvouched site).
    pub obstacles: Vec<Obstacle>,
    /// Sites the user vouched away (the completeness-claim downgrade).
    pub vouched_exemptions: Vec<SiteRef>,
}

impl DynamismObstacles {
    /// The reason every candidate should refuse with while an obstacle stands, or
    /// `None` when the project is clear. `eval-present` takes precedence over
    /// `dynamic-include-present` when both are present (the stronger havoc).
    #[must_use]
    pub fn blocking_reason(&self) -> Option<(&'static str, String)> {
        let eval = self.obstacles.iter().find(|o| o.reason == REASON_EVAL_PRESENT);
        let inc = self.obstacles.iter().find(|o| o.reason == REASON_DYNAMIC_INCLUDE);
        if let Some(o) = eval {
            return Some((
                REASON_EVAL_PRESENT,
                format!(
                    "the project contains {} `eval(...)` site(s); \"all callers proven\" is unknowable (see obstacles)",
                    o.sites.len()
                ),
            ));
        }
        if let Some(o) = inc {
            return Some((
                REASON_DYNAMIC_INCLUDE,
                format!(
                    "the project contains {} dynamic/out-of-universe include/require site(s); \"all callers proven\" is unknowable (see obstacles)",
                    o.sites.len()
                ),
            ));
        }
        None
    }
}

/// Scan `project` for the ADR-0046 §2 dynamic-code obstacles, applying the vendor
/// presumption and the vouching valve. A proven-literal include resolving inside
/// the analyzed universe is enumeration-benign (no obstacle).
#[must_use]
pub fn detect(db: &dyn Db, project: Project, vouches: &VouchSet) -> DynamismObstacles {
    // The analyzed universe: every project + vendor file the salsa Project knows,
    // normalized for path comparison.
    let universe: HashSet<String> =
        project.files(db).iter().map(|f| normalize_path(f.path(db))).collect();

    let mut eval_sites: Vec<SiteRef> = Vec::new();
    let mut include_sites: Vec<SiteRef> = Vec::new();
    let mut vouched: Vec<SiteRef> = Vec::new();

    for &file in project.files(db).iter() {
        let path = file.path(db);
        // Vendor presumption: eval/dynamic-include in vendor/ is autoload plumbing.
        if is_vendor_path(path) {
            continue;
        }
        let tree = parse(db, file);
        for site in tree.dynamism_sites() {
            let pos = tree.position(site.span.start);
            match &site.kind {
                DynamismKind::Eval => {
                    let sref = SiteRef::new(path, pos.line, pos.column, "eval(...)");
                    if vouches.covers(path, pos.line) {
                        vouched.push(sref);
                    } else {
                        eval_sites.push(sref);
                    }
                }
                DynamismKind::Include(ip) => {
                    // A proven literal resolving inside the universe is benign — its
                    // file's calls are already enumerated (call-site enumeration is
                    // per-file, ADR-0046 §2). Everything else is an obstacle.
                    if include_is_benign(ip, path, &universe) {
                        continue;
                    }
                    let label = format!("include/require {}", render_include_path(ip));
                    let sref = SiteRef::new(path, pos.line, pos.column, label);
                    if vouches.covers(path, pos.line) {
                        vouched.push(sref);
                    } else {
                        include_sites.push(sref);
                    }
                }
                // Non-literal `class_alias` (ADR-0049 §2): a dam site for the
                // checker-side finding-breadth family, but the transform-side
                // obstacle scan deliberately ignores it in S1 to stay byte-identical
                // (no `class_alias` was tracked here before). Damming it here is a
                // deferred transform behavior change, not part of the S1 groundwork.
                DynamismKind::ClassAlias => {}
            }
        }
    }

    let mut obstacles = Vec::new();
    if !eval_sites.is_empty() {
        obstacles.push(Obstacle::new(
            REASON_EVAL_PRESENT,
            "a non-vendor project file contains `eval(...)`, which can call any function with no visible call site (ADR-0046 §2)",
            eval_sites,
        ));
    }
    if !include_sites.is_empty() {
        obstacles.push(Obstacle::new(
            REASON_DYNAMIC_INCLUDE,
            "a non-vendor `include`/`require` uses an unproven or out-of-universe path, which can pull in code that defines or calls anything (ADR-0046 §2)",
            include_sites,
        ));
    }

    DynamismObstacles { obstacles, vouched_exemptions: vouched }
}

/// Whether a proven-literal include path resolves to a file inside the analyzed
/// universe (project + vendor). An `Unproven` path, or a literal that resolves
/// outside the universe (a compiled-template cache), is *not* benign.
fn include_is_benign(ip: &IncludePath, from: &str, universe: &HashSet<String>) -> bool {
    let target = match ip {
        IncludePath::Unproven => return false,
        IncludePath::Literal(p) => resolve_from(from, p),
        // `__DIR__ . '<suffix>'` — the suffix is directory-relative to the file.
        IncludePath::DirRelative(suffix) => {
            let rel = suffix.strip_prefix('/').unwrap_or(suffix);
            normalize_path(&join(dir_of(from), rel))
        }
    };
    universe.contains(&target)
}

/// Resolve a literal include path against the including file's directory (a
/// relative path) or as-is (an absolute path), normalized for comparison.
fn resolve_from(from: &str, p: &str) -> String {
    if is_absolute(p) {
        normalize_path(p)
    } else {
        normalize_path(&join(dir_of(from), p))
    }
}

fn render_include_path(ip: &IncludePath) -> String {
    match ip {
        IncludePath::Literal(p) => format!("'{p}' (out of universe)"),
        IncludePath::DirRelative(s) => format!("__DIR__ . '{s}' (out of universe)"),
        IncludePath::Unproven => "(dynamic path)".to_owned(),
    }
}

// ---- Path helpers (POSIX-style, `/`-separated) -----------------------------

fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with('\\')
}

fn dir_of(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(i) => &path[..i],
        None => "",
    }
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_owned()
    } else {
        format!("{dir}/{rel}")
    }
}

/// Normalize a `/`-separated path: fold `\` to `/`, drop `.` components, resolve
/// `..` against the preceding component, and preserve a leading `/` for absolute
/// paths. Purely lexical (no filesystem access) — the universe is a known set.
fn normalize_path(path: &str) -> String {
    let absolute = is_absolute(path);
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split(['/', '\\']) {
        match comp {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_resolves_dot_and_dotdot() {
        assert_eq!(normalize_path("a/./b/../c.php"), "a/c.php");
        assert_eq!(normalize_path("/x/y/../z.php"), "/x/z.php");
        assert_eq!(normalize_path("src\\a.php"), "src/a.php");
    }

    #[test]
    fn dir_relative_resolves_against_including_dir() {
        let universe: HashSet<String> = ["src/inc/util.php".to_owned()].into_iter().collect();
        let ip = IncludePath::DirRelative("/inc/util.php".to_owned());
        assert!(include_is_benign(&ip, "src/app.php", &universe));
        // A literal relative path resolves the same way.
        let ip2 = IncludePath::Literal("inc/util.php".to_owned());
        assert!(include_is_benign(&ip2, "src/app.php", &universe));
    }

    #[test]
    fn out_of_universe_and_unproven_are_not_benign() {
        let universe: HashSet<String> = ["src/a.php".to_owned()].into_iter().collect();
        assert!(!include_is_benign(
            &IncludePath::Literal("cache/tpl_123.php".to_owned()),
            "src/a.php",
            &universe
        ));
        assert!(!include_is_benign(&IncludePath::Unproven, "src/a.php", &universe));
    }

    #[test]
    fn vouch_matches_by_suffix_and_line() {
        let v = VouchSet::from_entries([("legacy.php".to_owned(), 42)]);
        assert!(v.covers("/proj/src/legacy.php", 42));
        assert!(!v.covers("/proj/src/legacy.php", 43));
        assert!(v.unused().is_empty());
        let unused = VouchSet::from_entries([("nope.php".to_owned(), 1)]);
        let _ = unused.covers("other.php", 2);
        assert_eq!(unused.unused(), vec!["nope.php:1".to_owned()]);
    }
}
