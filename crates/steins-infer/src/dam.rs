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
//! - every `class_alias(...)` naming a **runtime-minted** class (a class-name mint
//!   the reference scan cannot resolve — [`steins_syntax::DynamismKind::ClassAlias`]);
//! - every **non-vendor** file that fails to parse (ADR-0079 §2.2): a recovery point
//!   is a place the world is not enumerated, so it may have swallowed a class or a
//!   function declaration outright;
//! - every `define(...)` naming a **runtime-minted constant** (issue #198 —
//!   [`steins_syntax::DynamismKind::DefineDynamic`]). This is the one kind with a
//!   narrower blast radius: it dams `constant.undefined` only, since `define()`
//!   cannot mint a function or a class ([`DamKind::dams_names`]).
//!
//! The vendor presumption of ADR-0046 §2 carries over verbatim: `eval` /
//! dynamic-include inside a `vendor/` path is composer plumbing, presumed
//! universe-internal. (A `class_alias` whose two names are known at compile time —
//! string literals, or the `X::class` constant, which the *compiler* resolves and
//! which therefore mints nothing at run time (issue #36) — instead contributes an
//! index edge; it is never a dam site.)
//!
//! The existence ids read this fact (`call.undefined-function`, `class.undefined`,
//! `call.undefined-method`'s homonym leg). The vouch valve (ADR-0046) and
//! checker-side region scoping (ADR-0047 §9) are deferred; v1 is whole-universe.

use std::collections::HashSet;

use steins_db::ProjectLayout;
use steins_syntax::{DynamismKind, IncludePath};

use crate::FileUnit;

/// The kind of a dam site (ADR-0049 §2). Mirrors the dynamism taxonomy the
/// existence ids reason about; carried so triage/coverage surfaces can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DamKind {
    /// An `eval(...)` construct.
    Eval,
    /// A non-vendor `include`/`require` with an unproven or out-of-universe path.
    Include,
    /// A `class_alias(...)` naming a runtime-minted class (issue #36: `X::class` is
    /// compile-time and mints an index edge instead).
    ClassAlias,
    // parse failure (ADR-0079, issue #180)
    /// A **non-vendor** file `SourceTree::parse` recovered from (ADR-0079 §2.2).
    /// Unlike the three above, the site is not a *construct* — it is the whole
    /// file, positioned at its first parse error. Its blast radius is also wider:
    /// besides the whole-universe existence dam every kind carries, an
    /// `Unparsable` site makes the class-likes THAT FILE declares member-incomplete
    /// (§2.5, [`DamFacts::file_is_unparsable`]) — `eval` can mint a new name but
    /// cannot reopen a defined class, whereas a mangled class body can have lost
    /// methods.
    Unparsable,
    // end parse failure (ADR-0079, issue #180)
    // global constants (ADR-0078, issue #198)
    /// A `define(...)` naming a **runtime-minted constant** — the constant-side twin
    /// of [`Self::ClassAlias`] (a `define` with a literal name mints a declaration
    /// record instead and is never a dam site).
    ///
    /// The only kind with a **narrower** blast radius than the rest: `define()` can
    /// mint a constant and nothing else, so this site dams `constant.undefined` and
    /// leaves the function/class-existence ids alone. That asymmetry is spelled in
    /// [`DamKind::dams_names`], not open-coded at the consumers.
    DefineDynamic,
    // end global constants (ADR-0078, issue #198)
}

impl DamKind {
    /// Whether the site can mint a **function or class-like name** — the question
    /// [`DamFacts::is_clear`] asks. True for every kind but
    /// [`Self::DefineDynamic`], whose mint is a constant.
    ///
    /// The converse needs no method: every kind here can mint a *constant*
    /// (`eval` and an unproven include obviously; a mangled file may have swallowed
    /// a `const` statement; a computed `class_alias` is the one that arguably
    /// cannot, and is kept in the conservative direction rather than carved out
    /// for a gain nobody asked for), so the constant question is simply "any site
    /// at all" — see [`DamFacts::constants_are_clear`].
    #[must_use]
    pub const fn dams_names(self) -> bool {
        !matches!(self, Self::DefineDynamic)
    }
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
/// A *query answer* recomputed per run (ADR-0048).
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

    /// Whether the universe is **dam-clear** for *name* existence: no site that can
    /// mint a function or class-like name stands, so those absence claims are
    /// undammed (subject to the per-id ladder legs).
    ///
    /// A [`DamKind::DefineDynamic`] site does not count here (issue #198): a
    /// computed `define()` mints a constant, and reading it as a universe-wide name
    /// dam would silence `call.undefined-function` and `class.undefined` over
    /// something that cannot touch either. The constant ladder asks
    /// [`Self::constants_are_clear`] instead.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        !self.sites.iter().any(|s| s.kind.dams_names())
    }

    // global constants (ADR-0078, issue #198)
    /// Whether the universe is dam-clear for **constant** existence: *any* site at
    /// all closes this valve, since every dam kind can mint a constant name.
    /// Strictly stronger than [`Self::is_clear`].
    #[must_use]
    pub fn constants_are_clear(&self) -> bool {
        self.sites.is_empty()
    }
    // end global constants (ADR-0078, issue #198)

    /// The number of dam sites (the report/doctor posture's "N dammed sites").
    #[must_use]
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// Whether there are no dam sites at all (the `len() == 0` twin clippy pairs
    /// with [`Self::len`]; identical to [`Self::constants_are_clear`], and *not* to
    /// [`Self::is_clear`], which filters by kind).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    // parse failure (ADR-0079, issue #180)
    /// Whether `path` is a standing [`DamKind::Unparsable`] site — the
    /// **member-incompleteness** question of ADR-0079 §2.5, which the
    /// method-absence ladders ask of every class-like they walk through.
    ///
    /// Deliberately narrower than [`Self::is_clear`]: name existence is dammed
    /// universe-wide by *any* site, but member enumeration is only unprovable for
    /// the class-likes the broken file itself declares. And deliberately built on
    /// the site list rather than on `parse_errors()` directly, so the ADR-0046 §2
    /// vendor presumption applies to both legs at once — a broken vendor file is
    /// not a site, so it neither dams nor makes its classes member-incomplete.
    #[must_use]
    pub fn file_is_unparsable(&self, path: &str) -> bool {
        self.sites.iter().any(|s| s.kind == DamKind::Unparsable && s.path == path)
    }
    // end parse failure (ADR-0079, issue #180)
}

/// Compute the whole-universe dam fact from the lowered `units` (ADR-0049 §2).
/// A query answer — pure over the universe, no ordering dependence (ADR-0048).
///
/// `layout` decides which files get ADR-0046 §2's vendor presumption. It is a
/// project input rather than a path guess precisely because the presumption is a
/// documented soundness trade: extending it to first-party code would silence
/// real obstacles.
#[must_use]
pub fn dam_facts(units: &[FileUnit], layout: &ProjectLayout) -> DamFacts {
    // The analyzed universe: every project + vendor file, path-normalized for
    // include resolution (a proven include is benign only if it lands here).
    let universe: HashSet<String> = units.iter().map(|u| normalize_path(u.path)).collect();

    let mut sites = Vec::new();
    for u in units {
        let tree = u.tree;
        let vendor = layout.is_vendor(u.path);
        // parse failure (ADR-0079, issue #180)
        // One site per broken file, at the first error — recovery cascades make
        // every position after the first unreliable, so the count of further errors
        // is the emitter's business and the position is the first one's. The vendor
        // presumption of ADR-0046 §2 carries over verbatim (§2.3): parser test
        // suites ship deliberately broken PHP, so a `vendor/` file is not a site.
        //
        // Deferred with design (ADR-0079 §3): a *position-aware* refinement would
        // keep the absence family alive when the recovery region is provably inside
        // a statement body, since a body cannot have swallowed a top-level
        // declaration. It is not built here — it needs the syntax-tree contract to
        // expose the recovery REGIONS (the spans recovery skipped), which the
        // backend does not surface. When it lands, this site gains a region and the
        // consumers below consult it; nothing else changes shape. A naive
        // implementation would be wrong: conditional class declarations inside
        // bodies are legal PHP, so the body-local judgment must still check the
        // region for declaration keywords.
        if !vendor && let Some(first) = tree.parse_errors().first() {
            let pos = tree.position(first.span.start);
            sites.push(DamSite {
                path: u.path.to_owned(),
                line: pos.line,
                column: pos.column,
                kind: DamKind::Unparsable,
            });
        }
        // end parse failure (ADR-0079, issue #180)
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
                // A `class_alias` whose name is not known at compile time is a runtime
                // name mint. The vendor presumption does not extend to it: unlike
                // autoload include/eval, an aliasing call mints a *project-visible*
                // class name regardless of where it sits, so it dams even in vendor.
                DynamismKind::ClassAlias => DamKind::ClassAlias,
                // A computed `define` mints a project-visible CONSTANT name wherever
                // it sits, so — exactly like `class_alias` and for the same reason —
                // the vendor presumption does not extend to it.
                DynamismKind::DefineDynamic => DamKind::DefineDynamic,
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
        let facts = dam_facts(&units, &ProjectLayout::fallback());
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
        let facts = dam_facts(&units, &ProjectLayout::fallback());
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
        assert_eq!(dam_facts(&units, &ProjectLayout::fallback()).len(), 1);
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
        let facts = dam_facts(&units, &ProjectLayout::fallback());
        assert!(facts.is_clear(), "{:?}", facts.sites());
    }

    #[test]
    fn vendor_eval_and_include_do_not_dam() {
        let t = tree("<?php\neval('x();');\ninclude $dynamic;\n");
        let units = [FileUnit { path: "vendor/pkg/autoload.php", tree: &t }];
        assert!(dam_facts(&units, &ProjectLayout::fallback()).is_clear());
    }

    #[test]
    fn literal_class_alias_is_not_a_dam_site() {
        // A literal class_alias is an index edge, never a dam site.
        let t = tree("<?php\nclass_alias('A', 'B');\n");
        let units = [FileUnit { path: "src/a.php", tree: &t }];
        assert!(dam_facts(&units, &ProjectLayout::fallback()).is_clear());
    }

    #[test]
    fn class_const_class_alias_is_not_a_dam_site() {
        // Issue #36: `X::class` is resolved by the compiler, so this mints an index
        // edge like the two-literal form. `is_clear()` is universe-wide, so this one
        // site standing would have silenced the existence family for every file.
        let t = tree("<?php\nclass Thing {}\nclass_alias(Thing::class, 'Legacy_Thing');\n");
        let units = [FileUnit { path: "vendor/pkg/Thing.php", tree: &t }];
        assert!(dam_facts(&units, &ProjectLayout::fallback()).is_clear());
    }

    // parse failure (ADR-0079, issue #180)

    /// A file with exactly one parse error (`$this->s->` with no member name), whose
    /// recovery nevertheless keeps the class declaration.
    const BROKEN: &str = "<?php\nclass Q { public function f(): void { if ($this->s->) {} } }\n";

    #[test]
    fn an_unparsable_non_vendor_file_is_one_site_at_its_first_error() {
        let t = tree(BROKEN);
        let units = [FileUnit { path: "src/q.php", tree: &t }];
        let facts = dam_facts(&units, &ProjectLayout::fallback());
        assert_eq!(facts.len(), 1, "{:?}", facts.sites());
        let site = &facts.sites()[0];
        assert_eq!(site.kind, DamKind::Unparsable);
        assert_eq!(site.path, "src/q.php");
        assert_eq!((site.line, site.column), (2, 53));
        assert!(!facts.is_clear());
    }

    #[test]
    fn a_file_with_many_errors_is_still_exactly_one_site() {
        // The site is the FILE, not the error: recovery cascades make every position
        // after the first unreliable, so counting them here would count noise.
        let t = tree("<?php\nfunction a( int $x {\n}\nfunction b( int $y {\n}\n");
        assert!(t.parse_errors().len() > 1, "fixture must cascade: {:?}", t.parse_errors());
        let units = [FileUnit { path: "src/fns.php", tree: &t }];
        assert_eq!(dam_facts(&units, &ProjectLayout::fallback()).len(), 1);
    }

    #[test]
    fn an_unparsable_vendor_file_is_not_a_site() {
        // ADR-0079 §2.3, carrying ADR-0046 §2 over verbatim: parser test suites ship
        // deliberately broken PHP, so a `vendor/` break is presumed plumbing.
        let t = tree(BROKEN);
        let units = [FileUnit { path: "vendor/pkg/tests/broken.php", tree: &t }];
        let facts = dam_facts(&units, &ProjectLayout::fallback());
        assert!(facts.is_clear(), "{:?}", facts.sites());
        assert!(!facts.file_is_unparsable("vendor/pkg/tests/broken.php"));
    }

    #[test]
    fn member_incompleteness_is_per_file_while_the_dam_is_universe_wide() {
        // The two questions the site list answers are deliberately different reaches
        // (§2.5): `is_clear` is false for EVERY file once one break stands, but
        // `file_is_unparsable` is true only of the broken one — otherwise the member
        // leg would be a second whole-universe dam.
        let broken = tree(BROKEN);
        let sound = tree("<?php\nclass R { public function g(): void {} }\n");
        let units = [
            FileUnit { path: "src/q.php", tree: &broken },
            FileUnit { path: "src/r.php", tree: &sound },
        ];
        let facts = dam_facts(&units, &ProjectLayout::fallback());
        assert!(!facts.is_clear());
        assert!(facts.file_is_unparsable("src/q.php"));
        assert!(!facts.file_is_unparsable("src/r.php"));
    }

    // end parse failure (ADR-0079, issue #180)

    #[test]
    fn one_runtime_name_class_alias_dams_the_whole_universe() {
        // The blast radius the fix is about: the fact is a universe-wide boolean, so
        // a single runtime-minted name in ONE file dams every other file's existence
        // claims. That remains true — the fix narrows what counts, not the reach.
        let clean = tree("<?php\nclass Thing {}\nclass_alias(Thing::class, 'Legacy');\n");
        let dirty = tree("<?php\nclass_alias($computed, 'Other');\n");
        let units = [
            FileUnit { path: "src/a.php", tree: &clean },
            FileUnit { path: "src/b.php", tree: &dirty },
        ];
        let facts = dam_facts(&units, &ProjectLayout::fallback());
        assert_eq!(facts.len(), 1, "{:?}", facts.sites());
        assert!(!facts.is_clear());
    }

    // global constants (ADR-0078, issue #198)
    #[test]
    fn a_runtime_name_define_dams_constants_only() {
        // The one kind with a narrower blast radius. `define()` can mint a constant
        // and nothing else, so reading it as a universe-wide NAME dam would silence
        // `call.undefined-function` and `class.undefined` over something that cannot
        // touch either.
        let t = tree("<?php\ndefine('KNOWN', 1);\ndefine($computed, 2);\n");
        let units = [FileUnit { path: "src/a.php", tree: &t }];
        let facts = dam_facts(&units, &ProjectLayout::fallback());
        assert_eq!(facts.len(), 1, "the literal define is not a site: {:?}", facts.sites());
        assert_eq!(facts.sites()[0].kind, DamKind::DefineDynamic);
        assert!(facts.is_clear(), "function/class existence is undammed by a computed define");
        assert!(!facts.constants_are_clear(), "constant existence is dammed");
    }

    #[test]
    fn a_runtime_name_define_in_vendor_still_dams() {
        // The `class_alias` argument, verbatim: an aliasing or defining call mints a
        // project-visible name regardless of where it sits, so the ADR-0046 §2 vendor
        // presumption does not extend to it.
        let t = tree("<?php\ndefine($computed, 1);\n");
        let units = [FileUnit { path: "vendor/pkg/boot.php", tree: &t }];
        let facts = dam_facts(&units, &ProjectLayout::fallback());
        assert!(!facts.constants_are_clear(), "{:?}", facts.sites());
    }

    #[test]
    fn every_ordinary_kind_dams_constants_too() {
        // The converse of the asymmetry above: `eval` and an unproven include can
        // mint a constant just as easily as a function, so both valves close.
        for src in ["<?php\neval($code);\n", "<?php\ninclude 'config.php';\n"] {
            let t = tree(src);
            let units = [FileUnit { path: "src/a.php", tree: &t }];
            let facts = dam_facts(&units, &ProjectLayout::fallback());
            assert!(!facts.is_clear(), "{src}");
            assert!(!facts.constants_are_clear(), "{src}");
        }
    }
    // end global constants (ADR-0078, issue #198)
}
