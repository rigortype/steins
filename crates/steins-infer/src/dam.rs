//! The checker-side runtime-definition dam (ADR-0049 §2, ADR-0046 applied
//! checker-side).
//!
//! Function- and class-*existence* absence claims are unsound while the universe
//! contains dynamic code that can mint names the reference scan never sees. This
//! module aggregates the **whole-universe** dam fact: every dam site across the
//! lowered project. It is a *query answer* (ADR-0048) — recomputed per run from the
//! lowered universe, with no entry state, no ordering dependence, and no
//! cross-scope coupling. Method-*absence* claims need no dam (PHP cannot reopen a
//! defined class — the immunity asymmetry of ADR-0049 §2), so this fact gates only
//! the existence ids.
//!
//! ## The dam set
//! - every `eval(...)` (code as data — ADR-0046 §2 universe havoc);
//! - every **non-vendor** `include`/`require` whose path is not provably
//!   in-universe: `Unproven`, or a bare-relative / `./`-prefixed literal (A5, as
//!   amended — runtime resolves those against `include_path` → the script dir →
//!   CWD, so directory-relative belief is unsound), or an absolute / `__DIR__`-
//!   anchored literal that resolves *outside* the analyzed universe;
//! - every **non-literal** `class_alias(...)` (a runtime class-name mint —
//!   [`steins_syntax::DynamismKind::ClassAlias`]).
//!
//! The vendor presumption of ADR-0046 §2 carries over verbatim: `eval` /
//! dynamic-include inside a `vendor/` path is composer plumbing, presumed
//! universe-internal. (A literal `class_alias` instead contributes an index edge —
//! it is never a dam site.)
//!
//! **S1 groundwork: this fact is carried and tested, but consumed by nothing.**
//! The existence ids that read it (`call.undefined-function`, `class.undefined`,
//! `call.undefined-method`'s homonym leg) land in later stages. The vouch valve
//! (ADR-0046) and checker-side region scoping (ADR-0047 §9) are deferred with the
//! consuming stages; v1 is whole-universe.

use std::collections::HashSet;

use steins_syntax::{DynamismKind, IncludePath};

use crate::FileUnit;
use crate::is_vendor_path;

/// The kind of a dam site (ADR-0049 §2). Mirrors the dynamism taxonomy the
/// existence ids reason about; carried so triage/coverage surfaces can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DamKind {
    /// An `eval(...)` construct.
    Eval,
    /// A non-vendor `include`/`require` with an unproven or out-of-universe path.
    Include,
    /// A non-literal `class_alias(...)` — a runtime class-name mint.
    ClassAlias,
}

/// One dam site: where a runtime-definition construct stands (ADR-0049 §2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DamSite {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub kind: DamKind,
}

/// The whole-universe dam fact for one run (ADR-0049 §2): every dam site, or none.
/// A *query answer* recomputed per run (ADR-0048); consumed by nothing in S1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DamFacts {
    sites: Vec<DamSite>,
}

impl DamFacts {
    /// The dam sites standing this run (order mirrors the input universe; the fact
    /// itself is the *set*, so consumers must not depend on order — ADR-0048).
    #[must_use]
    pub fn sites(&self) -> &[DamSite] {
        &self.sites
    }

    /// Whether the universe is **dam-clear**: no runtime-definition site stands, so
    /// existence-absence claims are undammed (subject to the per-id ladder legs).
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.sites.is_empty()
    }

    /// The number of dam sites (the report/doctor posture's "N dammed sites").
    #[must_use]
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// Whether there are no dam sites (alias of [`Self::is_clear`], for the
    /// `is_empty`/`len` clippy pairing).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }
}

/// Compute the whole-universe dam fact from the lowered `units` (ADR-0049 §2).
/// A query answer — pure over the universe, no ordering dependence (ADR-0048).
#[must_use]
pub fn dam_facts(units: &[FileUnit]) -> DamFacts {
    // The analyzed universe: every project + vendor file, path-normalized for
    // include resolution (a proven include is benign only if it lands here).
    let universe: HashSet<String> = units.iter().map(|u| normalize_path(u.path)).collect();

    let mut sites = Vec::new();
    for u in units {
        let tree = u.tree;
        let vendor = is_vendor_path(u.path);
        for site in tree.dynamism_sites() {
            let pos = tree.position(site.span.start);
            let kind = match &site.kind {
                // Vendor presumption (ADR-0046 §2): eval/dynamic-include in vendor/
                // is autoload plumbing, presumed universe-internal.
                DynamismKind::Eval if vendor => continue,
                DynamismKind::Eval => DamKind::Eval,
                DynamismKind::Include(_) if vendor => continue,
                DynamismKind::Include(ip) => {
                    if include_is_benign(ip, u.path, &universe) {
                        continue;
                    }
                    DamKind::Include
                }
                // A non-literal `class_alias` is a runtime name mint. The vendor
                // presumption does not extend to it: unlike autoload include/eval,
                // an aliasing call mints a *project-visible* class name regardless of
                // where it sits, so it dams even in vendor.
                DynamismKind::ClassAlias => DamKind::ClassAlias,
            };
            sites.push(DamSite { path: u.path.to_owned(), line: pos.line, column: pos.column, kind });
        }
    }
    DamFacts { sites }
}

/// Whether a proven include path resolves inside the analyzed universe (ADR-0049
/// A5, amended). **Only** absolute literals and `__DIR__`-anchored concatenations
/// can be benign, and only when they resolve to an indexed file. A bare-relative or
/// `./`-prefixed literal (both `IncludePath::Literal` without a leading `/`) is
/// never benign — runtime resolves it against `include_path` → the script dir →
/// CWD, so a same-named in-universe neighbor cannot prove the universe closed.
fn include_is_benign(ip: &IncludePath, from: &str, universe: &HashSet<String>) -> bool {
    match ip {
        IncludePath::Unproven => false,
        IncludePath::Literal(p) => {
            // `./x` is `Literal("./x")` — not absolute, so it stays unproven (A5:
            // `./` anchors to CWD, not the including file's directory).
            is_absolute(p) && universe.contains(&normalize_path(p))
        }
        IncludePath::DirRelative(suffix) => {
            let rel = suffix.strip_prefix('/').unwrap_or(suffix);
            universe.contains(&normalize_path(&join(dir_of(from), rel)))
        }
    }
}

// ---- Path helpers (POSIX-style, `/`-separated) -----------------------------
//
// Deliberately duplicated from the transform-side obstacle scanner: A5 says the
// checker dam and the transform oracle share one *corrected* judgment, but the
// transform side keeps its (under-damming) rule byte-identical in S1, so the
// checker owns the corrected copy here rather than reaching across the crate.

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
/// `..` against the preceding component, preserve a leading `/` for absolute paths.
/// Purely lexical — the universe is a known set, so no filesystem access.
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
    use steins_syntax::SourceTree;

    /// Build owned trees, then borrow them into units (the trees must outlive the
    /// units, so the caller holds them).
    fn tree(src: &str) -> SourceTree {
        SourceTree::parse(src)
    }

    #[test]
    fn a_clean_universe_is_dam_clear() {
        let t = tree("<?php\nfunction f(int $x): int { return $x; }\nclass C {}\nf(1);\n");
        let units = [FileUnit { path: "src/a.php", tree: &t }];
        let facts = dam_facts(&units);
        assert!(facts.is_clear(), "clean universe: {:?}", facts.sites());
        assert_eq!(facts.len(), 0);
    }

    #[test]
    fn each_dam_site_kind_is_collected() {
        // eval; a bare-relative include (A5: unproven); a non-literal class_alias.
        let t = tree(
            "<?php\neval('x();');\ninclude 'inc/util.php';\nclass_alias($a, 'B');\n",
        );
        let units = [FileUnit { path: "src/boot.php", tree: &t }];
        let facts = dam_facts(&units);
        let kinds: HashSet<DamKind> = facts.sites().iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&DamKind::Eval), "{:?}", facts.sites());
        assert!(kinds.contains(&DamKind::Include), "{:?}", facts.sites());
        assert!(kinds.contains(&DamKind::ClassAlias), "{:?}", facts.sites());
        assert_eq!(facts.len(), 3);
        assert!(!facts.is_clear());
    }

    #[test]
    fn dot_slash_literal_is_unproven_and_dams() {
        // A5: `./config.php` resolves against CWD, not the including dir → dam.
        let t = tree("<?php\ninclude './config.php';\n");
        let units = [FileUnit { path: "src/a.php", tree: &t }];
        assert_eq!(dam_facts(&units).len(), 1);
    }

    #[test]
    fn dir_relative_and_absolute_in_universe_do_not_dam() {
        // `__DIR__ . '/util.php'` from src/a.php resolves to src/util.php (indexed);
        // an absolute literal pointing at an indexed file is likewise benign.
        let t = tree("<?php\nrequire __DIR__ . '/util.php';\nrequire '/proj/lib.php';\n");
        let util = tree("<?php\n");
        let lib = tree("<?php\n");
        let units = [
            FileUnit { path: "src/a.php", tree: &t },
            FileUnit { path: "src/util.php", tree: &util },
            FileUnit { path: "/proj/lib.php", tree: &lib },
        ];
        let facts = dam_facts(&units);
        assert!(facts.is_clear(), "{:?}", facts.sites());
    }

    #[test]
    fn vendor_eval_and_include_do_not_dam() {
        let t = tree("<?php\neval('x();');\ninclude $dynamic;\n");
        let units = [FileUnit { path: "vendor/pkg/autoload.php", tree: &t }];
        assert!(dam_facts(&units).is_clear());
    }

    #[test]
    fn literal_class_alias_is_not_a_dam_site() {
        // A literal class_alias is an index edge, never a dam site.
        let t = tree("<?php\nclass_alias('A', 'B');\n");
        let units = [FileUnit { path: "src/a.php", tree: &t }];
        assert!(dam_facts(&units).is_clear());
    }
}
