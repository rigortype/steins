//! The project view: the files analyzed together and their symbol index —
//! [`FileUnit`], [`Index`], the magic-member obstacles (ADR-0046), the `Diagnostic` /
//! `Fix` records every pass emits, and the vendor-path test.

use std::collections::{BTreeMap, HashMap, HashSet};

use steins_db::{
    DeclSite, MergedTables, PackageShard, ProjectIndex, Resolve, ShardSite, SourceFile,
    fallback_package_key, merge_shards,
};
use steins_syntax::{NameRef, RefKind, SourceTree};

use crate::absence::magic_obstacles_in_reach;
use crate::cx::Cx;
use crate::suppress::Facet;

/// Whether a diagnostic path lies inside a `vendor/` directory (ADR-0015).
///
/// **The floor, not the rule.** This is the directory-name guess: a path with a
/// `vendor` component — a top-level `vendor/…` or any nested `…/vendor/…` — is
/// vendor. It is right for the common Composer install and wrong for a tree that
/// vendors elsewhere, so the answer a run actually uses comes from
/// [`ProjectLayout::is_vendor`], which reads the project's own `composer.json`
/// and falls back to this. Kept public for callers that have no project in hand.
///
/// Vendor code is fully indexed and inferred (shapes/values/effects flow through
/// it) either way; only its diagnostics are off by default.
///
/// [`ProjectLayout::is_vendor`]: steins_db::ProjectLayout::is_vendor
pub fn is_vendor_path(path: &str) -> bool {
    steins_db::fallback_is_vendor(path)
}

/// A proof-layer finding. Kept deliberately flat so the CLI can render text or
/// JSON without knowing anything about the analysis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub id: &'static str,
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
    /// The registry-declared facet this finding carries (ADR-0050 §4), or `None`
    /// for ids that declare no facet. v1: `Some(Facet::Origin(_))` on
    /// `throw.undeclared` only, computed at emit time from walk-local data (the
    /// measurement note's same-file-plus-own-origin rule). Additive — the
    /// `--format json` output shows it as an extra key only when present, and the
    /// value never participates in a check's inference behavior. It *does* take
    /// part in equality/hash, but harmlessly: two findings that were previously
    /// equal share an origin file+offset and so compute the same facet.
    pub facet: Option<Facet>,
    /// The fix-it this finding carries as a first-class payload (ADR-0010), or
    /// `None` for the many findings whose remedy is a judgment call. v1: only
    /// the explicit dump pair (`debug.type` / `debug.phpdoc-type`) ships one —
    /// delete the whole dump expression-statement, the remedy ADR-0053 itself
    /// names. Additive like `facet`: the `--format json` output shows it as an
    /// extra key only when present, `check --fix` applies it, and the value
    /// never participates in a check's inference behavior. Equality/hash
    /// participation is likewise harmless — equal findings compute equal fixes.
    pub fix: Option<Fix>,
}

/// The mechanical remedy a diagnostic carries (ADR-0010): byte-span edits
/// that, applied together, resolve the finding. The edit shape mirrors
/// steins-edit's `Edit` (path + `[start, end)` byte span + replacement), so
/// the CLI can pour a run's fixes into one atomic `EditPlan` — and a JSON
/// consumer can apply them with the same splice — without translation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fix {
    /// A short human label of the remedy ("remove the dump statement").
    pub title: &'static str,
    /// The non-overlapping edits, in file order.
    pub edits: Vec<FixEdit>,
}

/// One byte-span replacement of a [`Fix`]: splice `replacement` over the
/// `[start, end)` byte range of `path`'s current contents. Deletion is an
/// empty replacement; insertion is a zero-width span.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FixEdit {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub replacement: String,
}

// ---------------------------------------------------------------------------
// The project view: the files analyzed together and their symbol index.
// ---------------------------------------------------------------------------

/// One file in the analyzed project: its diagnostic path and its lowered tree.
/// The tree owns everything else the analysis needs (functions, classes,
/// scopes, positions, namespace contexts).
#[derive(Clone, Copy)]
pub struct FileUnit<'a> {
    pub path: &'a str,
    pub tree: &'a SourceTree,
}

/// A declaration's position within the project view: the file's index in the
/// [`FileUnit`] slice, and the declaration's index in that file's list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Site {
    pub(crate) file: usize,
    pub(crate) index: usize,
}

/// The outcome of resolving an FQN against the in-memory project index.
#[derive(Clone, Copy)]
pub(crate) enum Res {
    Absent,
    Unique(Site),
    Ambiguous,
}

/// The A14 silence-obstacle record (ADR-0049 A14, issue #195). Defined beside
/// the package-shard builder since issue #486 — the obstacle table is one of
/// the per-package tables the generation merge recomputes — and re-exported
/// here unchanged, where its consumers live.
pub use steins_db::MagicObstacle;

/// The project symbol index in the analysis's own `Site` terms (a file *index*,
/// not a salsa handle). Built either directly from the [`FileUnit`] slice
/// (single-file / test paths) or adapted from the salsa [`ProjectIndex`]
/// (the db-backed [`check_project`] path — so the tracked query is the authority
/// on incrementality, ADR-0009). Both routes stand on the package-shard builder
/// (ADR-0092 §3, issue #486): [`Index::from_units`] partitions the units into
/// [`PackageShard`]s and merges every global table from them, and the salsa
/// query delegates to the same builder on its side — one implementation under
/// what used to be two.
///
/// [`check_project`]: crate::check_project
#[derive(Default, PartialEq)]
pub(crate) struct Index {
    functions: HashMap<String, Site>,
    ambiguous_functions: HashSet<String>,
    classes: HashMap<String, Site>,
    ambiguous_classes: HashSet<String>,
    fn_by_simple: HashMap<String, Vec<Site>>,
    /// The A14 obstacle records, keyed by the **declaration's own** lowercase FQN
    /// ([`ClassDecl::fqn`]) rather than by whatever name a lookup arrived on — a
    /// `class_alias` edge must never make a tag-carrying class read as tag-free.
    /// Empty for a project that spells none of the tags, which is the cheap path
    /// every consumer checks first.
    magic_obstacles: HashMap<String, Vec<MagicObstacle>>,
    // member absence (ADR-0078, issue #197)
    /// Every property name **written** anywhere in the project, and whether any
    /// write went through a computed name — the dynamic-property obstacle for
    /// [`PROPERTY_UNDEFINED_ID`]. See `SourceTree::property_write_names` for why
    /// the obstacle is keyed by name rather than by receiver.
    ///
    /// [`PROPERTY_UNDEFINED_ID`]: crate::PROPERTY_UNDEFINED_ID
    property_writes: (HashSet<String>, bool),
    // end member absence (ADR-0078, issue #197)
    // global constants (ADR-0078, issue #198)
    /// Every global constant the universe declares, keyed by
    /// [`steins_syntax::normalize_const_fqn`] (namespace lowercased, final segment
    /// case-preserved).
    ///
    /// A **set**, not a `Site` map, and that is the whole design: this table exists
    /// for one absence proof, which asks "does anything define this name?" and
    /// nothing else. There is therefore no `Unique`/`Ambiguous` distinction to
    /// draw — two `define('X', …)` calls are a redefinition *warning* at run time
    /// and the first one wins, so the name is still defined either way, and a
    /// duplicate `const X` is a load-time fatal that is the declaration-fatal
    /// family's business, not this id's. Presence can only silence an absence
    /// claim, never raise one.
    ///
    /// Scanned off the [`FileUnit`] slice (via the shard merge) rather than
    /// carried on the salsa [`ProjectIndex`], the same route
    /// [`Self::magic_obstacles`] takes: the lowering (which salsa memoizes)
    /// already holds the records, and a set with no site identity needs none of
    /// the project index's collision machinery.
    constants: HashSet<String>,
    // end global constants (ADR-0078, issue #198)
    /// Every unit's diagnostic path → its index in the [`FileUnit`] slice — the
    /// per-run derivation of a units index from the stable file identity (issue
    /// #497). A value type that survives a walk names a file by path; a consumer
    /// that needs a `Cx` (or unit order) looks the index up here rather than
    /// embedding one.
    files: HashMap<String, usize>,
}

/// Partition the units by [`fallback_package_key`], build one [`PackageShard`]
/// per package, and recompute every global table from the shards (ADR-0092 §3,
/// issue #486). The merge is partition-invariant — the differential-oracle
/// tests pin that any grouping of the same units merges identically — so the
/// path heuristic decides shard boundaries and never a table's contents.
fn merged_tables(units: &[FileUnit]) -> MergedTables {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (slot, u) in units.iter().enumerate() {
        groups.entry(fallback_package_key(u.path)).or_default().push(slot);
    }
    let mut shards: Vec<PackageShard> = Vec::with_capacity(groups.len());
    for slots in groups.into_values() {
        let mut s = PackageShard::default();
        for slot in slots {
            s.add_file(slot, units[slot].path, units[slot].tree);
        }
        shards.push(s);
    }
    merge_shards(&shards)
}

impl Index {
    /// Build the index straight from the file units, via the package-shard
    /// builder (issue #486) — the same one implementation the salsa
    /// `project_index` delegates to on its side.
    pub(crate) fn from_units(units: &[FileUnit]) -> Self {
        Self::from_merged(merged_tables(units))
    }

    /// Every table of a merge, with universe slots read as unit-slice indices
    /// — on this path they are the same thing. `pub(crate)` for the generation
    /// orchestrator (issue #489), whose warm path merges loaded-or-rebuilt
    /// shards itself and hands the result here; the merge is
    /// partition-invariant, so this is the same index either grouping builds.
    pub(crate) fn from_merged(m: MergedTables) -> Self {
        let site = |s: ShardSite| Site { file: s.file, index: s.index };
        Index {
            functions: m.functions.into_iter().map(|(fqn, s)| (fqn, site(s))).collect(),
            ambiguous_functions: m.ambiguous_functions,
            classes: m.classes.into_iter().map(|(fqn, s)| (fqn, site(s))).collect(),
            ambiguous_classes: m.ambiguous_classes,
            fn_by_simple: m
                .fn_by_simple
                .into_iter()
                .map(|(simple, sites)| (simple, sites.into_iter().map(site).collect()))
                .collect(),
            magic_obstacles: m.magic_obstacles,
            property_writes: m.property_writes,
            constants: m.constants,
            files: m.files,
        }
    }

    /// Adapt the salsa [`ProjectIndex`] to `Site`s, using `pos` to map each
    /// [`SourceFile`] to its position in the (identically ordered) unit slice.
    /// The obstacle/write/constant/file tables come from the same shard merge
    /// as [`Index::from_units`]'s: they are facts of the lowered trees (which
    /// salsa already memoizes), not symbol-table facts the project index
    /// carries.
    pub(crate) fn from_db(
        db_index: &ProjectIndex,
        pos: &HashMap<SourceFile, usize>,
        units: &[FileUnit],
    ) -> Self {
        let site = |ds: &DeclSite| Site { file: pos[&ds.file], index: ds.index };
        let mut idx = Index::default();
        for (fqn, ds) in db_index.functions() {
            idx.functions.insert(fqn.clone(), site(ds));
        }
        for (fqn, ds) in db_index.classes() {
            idx.classes.insert(fqn.clone(), site(ds));
        }
        idx.ambiguous_functions = db_index.ambiguous_functions().clone();
        idx.ambiguous_classes = db_index.ambiguous_classes().clone();
        for (simple, sites) in db_index.fn_by_simple() {
            idx.fn_by_simple.insert(simple.clone(), sites.iter().map(site).collect());
        }
        let m = merged_tables(units);
        idx.magic_obstacles = m.magic_obstacles;
        idx.property_writes = m.property_writes;
        idx.constants = m.constants;
        idx.files = m.files;
        idx
    }

    /// The per-run units index of the file whose diagnostic path is `path`, or
    /// `None` when no unit of this run has that path (issue #497). This is where
    /// a stable file identity carried by a value type turns back into the index
    /// a [`Cx`] or the unit-order sorts need.
    ///
    /// [`Cx`]: crate::cx::Cx
    pub(crate) fn file_index_of(&self, path: &str) -> Option<usize> {
        self.files.get(path).copied()
    }

    // global constants (ADR-0078, issue #198)
    /// Whether **anything in the universe** declares the global constant `key`
    /// (already normalized by [`steins_syntax::normalize_const_fqn`]).
    pub(crate) fn declares_constant(&self, key: &str) -> bool {
        self.constants.contains(key)
    }
    // end global constants (ADR-0078, issue #198)

    /// The A14 records a single class-like **declares itself** (no chain walk).
    /// Empty for every class-like that spells none of the tags.
    pub(crate) fn magic_obstacles_of(&self, decl_fqn: &str) -> &[MagicObstacle] {
        if self.magic_obstacles.is_empty() {
            return &[];
        }
        self.magic_obstacles.get(&decl_fqn.to_ascii_lowercase()).map_or(&[], Vec::as_slice)
    }

    /// Whether the project spells any magic-member tag at all — the one-branch
    /// early-out that keeps a tag-free project paying nothing for this leg.
    pub(crate) fn has_magic_obstacles(&self) -> bool {
        !self.magic_obstacles.is_empty()
    }

    // member absence (ADR-0078, issue #197)
    /// Whether a property named `prop` could have been created dynamically
    /// somewhere in the project before this read (ADR-0078, issue #197): the
    /// project writes that exact name anywhere, or it writes some property under
    /// a computed name — which could be any name at all.
    pub(crate) fn property_write_obstacle(&self, prop: &str) -> bool {
        let (names, dynamic) = &self.property_writes;
        *dynamic || names.contains(prop)
    }
    // end member absence (ADR-0078, issue #197)

    pub(crate) fn resolve_function(&self, fqn: &str) -> Res {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_functions.contains(&key) {
            Res::Ambiguous
        } else {
            self.functions.get(&key).copied().map_or(Res::Absent, Res::Unique)
        }
    }

    pub(crate) fn resolve_class(&self, fqn: &str) -> Res {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_classes.contains(&key) {
            Res::Ambiguous
        } else {
            self.classes.get(&key).copied().map_or(Res::Absent, Res::Unique)
        }
    }

    pub(crate) fn unique_fn_by_simple(&self, simple: &str) -> Option<Site> {
        match self.fn_by_simple.get(&simple.to_ascii_lowercase()) {
            Some(sites) if sites.len() == 1 => Some(sites[0]),
            _ => None,
        }
    }

    pub(crate) fn has_simple_function(&self, simple: &str) -> bool {
        self.fn_by_simple.contains_key(&simple.to_ascii_lowercase())
    }
}

/// Every magic-member obstacle record the project declares (ADR-0049 A14), in
/// file then source order — the seam a posture/`doctor` surface aggregates
/// ("N absence claims silenced by `@method` tags on M classes") and the one a
/// plugin discharge channel (ADR-0039) will subtract from. The ladders read the
/// same records through the per-class index built from the same scan (the
/// shard builder's, since issue #486); nothing in this slice reports them.
#[must_use]
pub fn magic_obstacles(units: &[FileUnit<'_>]) -> Vec<MagicObstacle> {
    let mut recs: Vec<MagicObstacle> = Vec::new();
    for u in units {
        for cd in u.tree.classes() {
            steins_db::class_magic_obstacles(u.tree, cd, &mut recs);
        }
    }
    recs
}

/// The A14 records in one class-like's **resolved reach** — its own, its parents',
/// its interfaces', and those of its `@mixin` targets followed transitively. This
/// is the exact question the absence ladders ask (non-empty ⇒ not enumerable ⇒
/// silence); public so the reach, not merely the per-declaration scan, is
/// observable to a future posture surface and to tests.
#[must_use]
pub fn magic_obstacles_reaching(units: &[FileUnit<'_>], class_fqn: &str) -> Vec<MagicObstacle> {
    if units.is_empty() {
        return Vec::new();
    }
    let index = Index::from_units(units);
    magic_obstacles_in_reach(&Cx::new(units, &index, 0), class_fqn)
}

/// Whether a function-call reference resolves to a **user** function defined in
/// the project (as opposed to a builtin, or an unresolved/ambiguous name),
/// applying PHP name resolution against the salsa [`ProjectIndex`]. Public so
/// tooling (`xtask freq`) can exclude userland cross-file calls from the
/// builtin-frequency ranking. A name is "userland" here if the project uniquely
/// defines it at any candidate FQN the reference could denote — the
/// builtin-shadow nuance the checker applies is irrelevant to this question.
#[must_use]
pub fn resolves_to_user_function(index: &ProjectIndex, tree: &SourceTree, r: &NameRef) -> bool {
    let unique = |fqn: &str| matches!(index.resolve_function(fqn), Resolve::Unique(_));
    match r.kind {
        RefKind::FullyQualified => unique(&r.raw.to_ascii_lowercase()),
        RefKind::Qualified => {
            let ctx = tree.ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            unique(&fqn)
        }
        RefKind::Unqualified => {
            let ctx = tree.ctx_at(r.offset);
            let name = r.raw.to_ascii_lowercase();
            if let Some(t) = ctx.fn_imports.get(&name) {
                return unique(&t.to_ascii_lowercase());
            }
            if !ctx.namespace.is_empty() && unique(&format!("{}\\{}", ctx.namespace, name)) {
                return true;
            }
            unique(&name)
        }
        // ADR-0049 A8: `namespace\name` — the enclosing-namespace candidate only.
        RefKind::Relative => {
            let ctx = tree.ctx_at(r.offset);
            let fqn = if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            unique(&fqn.to_ascii_lowercase())
        }
    }
}

/// How an unqualified/qualified/FQ **function** call resolves (ADR-0001).
pub(crate) enum FnResolution {
    /// A user function defined in the project (its declaration site).
    User(Site),
    /// A catalogued builtin — no user body, but folding/effect labels apply.
    /// Carries the **resolved catalog name** (lowercase, single segment) —
    /// never the call's own spelling. An unaliased `use function trim;` and a
    /// bare `trim(...)` both carry `"trim"`; an aliased `use function trim as
    /// t;` also carries `"trim"` even though the call site spells it `t`
    /// (issue #279) — every catalog-keyed consumer (the ADR-0070
    /// argument-survival gate, the effects pass, the throws pass, and the
    /// effects/throws higher-order dispatch sites that ask
    /// [`steins_catalog::invocation_shape`]) must key the catalog by this
    /// name, not by [`steins_syntax::NameRef::simple`] or `.raw`, or an
    /// aliased import's calls silently fall back to "unknown name" and lose
    /// everything the catalog would otherwise state.
    ///
    /// **Decision, not an oversight**: diagnostic *display* text (the
    /// `t() has effect …` message body, throw provenance origins, …)
    /// deliberately keeps using [`steins_syntax::NameRef::simple`] — the
    /// call's own written spelling — even where the catalog answer above came
    /// from this canonical name instead. A reader looking at `t()` in their
    /// source wants the diagnostic to say `t()`, not the builtin it happens to
    /// alias; only the catalog *key* needed fixing, never what gets printed.
    Builtin(String),
    /// Ambiguous or unresolved — skip everything (no check, no fold, no effect
    /// classification). The silent side.
    Unknown,
}

/// The differential oracle for the shard delegation (issue #486): the frozen
/// pre-shard construction of [`Index`], kept test-only, and the assertions
/// that partition → shards → merge reproduces it exactly — under the
/// production grouping, under the real Composer partition, and under the
/// finest partition there is (one file per shard). Inside the module because
/// both the reference and the equality need the private fields.
#[cfg(test)]
mod shard_oracle {
    use steins_db::{GoverningRoot, PackagePartition, ProjectLayout};

    use super::*;

    /// The pre-#486 `Index::from_units` body, frozen verbatim: a single
    /// streaming pass with duplicate demotion, the literal `class_alias` fold
    /// against the textual snapshot, and the direct whole-slice scans.
    fn from_units_reference(units: &[FileUnit]) -> Index {
        fn insert_unique(
            map: &mut HashMap<String, Site>,
            ambiguous: &mut HashSet<String>,
            fqn: &str,
            site: Site,
        ) {
            if ambiguous.contains(fqn) {
                return;
            }
            if map.remove(fqn).is_some() {
                ambiguous.insert(fqn.to_owned());
            } else {
                map.insert(fqn.to_owned(), site);
            }
        }
        let mut idx = Index::default();
        for (fi, u) in units.iter().enumerate() {
            for (i, f) in u.tree.functions().iter().enumerate() {
                let site = Site { file: fi, index: i };
                idx.fn_by_simple.entry(f.name.to_ascii_lowercase()).or_default().push(site);
                insert_unique(&mut idx.functions, &mut idx.ambiguous_functions, &f.fqn, site);
            }
            for (i, c) in u.tree.classes().iter().enumerate() {
                let site = Site { file: fi, index: i };
                insert_unique(&mut idx.classes, &mut idx.ambiguous_classes, &c.fqn, site);
            }
        }
        let mut resolved: Vec<(String, Site)> = Vec::new();
        for u in units {
            for edge in u.tree.class_alias_edges() {
                if idx.ambiguous_classes.contains(&edge.target_fqn) {
                    continue;
                }
                if let Some(&target) = idx.classes.get(&edge.target_fqn) {
                    resolved.push((edge.alias_fqn.clone(), target));
                }
            }
        }
        for (alias_fqn, target) in resolved {
            insert_unique(&mut idx.classes, &mut idx.ambiguous_classes, &alias_fqn, target);
        }
        let mut buf: Vec<MagicObstacle> = Vec::new();
        for u in units {
            for cd in u.tree.classes() {
                steins_db::class_magic_obstacles(u.tree, cd, &mut buf);
                if !buf.is_empty() {
                    idx.magic_obstacles.entry(cd.fqn.to_ascii_lowercase()).or_default().append(&mut buf);
                }
            }
            idx.property_writes.0.extend(u.tree.property_write_names().iter().cloned());
            idx.property_writes.1 |= u.tree.writes_computed_property_name();
            for decl in u.tree.global_const_decls() {
                idx.constants.insert(decl.fqn.clone());
            }
        }
        idx.files = units.iter().enumerate().map(|(i, u)| (u.path.to_owned(), i)).collect();
        idx
    }

    /// A fixture wide enough to make every merged table observable across
    /// shard boundaries: root and vendor paths, cross- and within-shard
    /// duplicate FQNs, cross-shard alias edges (minted and demoted), magic
    /// tags on two classes sharing one lowercase FQN in different shards,
    /// property writes (a computed one in vendor only), and constants.
    fn fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "src/app.php",
                "<?php\nnamespace App;\nfunction run() {}\nfunction helper() {}\n/** @method int magic() */\nclass Kernel {}\nclass_alias('lib\\\\a\\\\widget', 'app\\\\widget');\nconst LIMIT = 3;\n$k->written = 1;\n",
            ),
            (
                "vendor/lib/a/src/widget.php",
                "<?php\nnamespace Lib\\A;\nfunction helper() {}\n/** @property string $p */\nclass Widget {}\nclass Dup {}\nclass Same {}\n",
            ),
            (
                "vendor/lib/b/src/dup.php",
                "<?php\nnamespace Lib\\A;\nclass Dup {}\nfunction helper() {}\n/** @mixin Widget */\nclass Same {}\ndefine('FLAG', true);\n$o->{$name} = 5;\n",
            ),
            (
                "vendor/lib/b/src/more.php",
                "<?php\nclass Local {}\nclass Local {}\nclass_alias('app\\\\kernel', 'shim');\nclass_alias('lib\\\\a\\\\dup', 'never');\n",
            ),
            ("vendor/autoload.php", "<?php\nfunction stray_helper() {}\n"),
        ]
    }

    fn parse_all(sources: &[(&'static str, &'static str)]) -> Vec<(&'static str, SourceTree)> {
        sources.iter().map(|&(p, s)| (p, SourceTree::parse(s))).collect()
    }

    fn units_of<'a>(parsed: &'a [(&'static str, SourceTree)]) -> Vec<FileUnit<'a>> {
        parsed.iter().map(|(p, t)| FileUnit { path: p, tree: t }).collect()
    }

    /// The oracle under the production grouping: `from_units` (which now
    /// delegates through the shard builder) reproduces the frozen
    /// construction on every table.
    #[test]
    fn from_units_matches_the_frozen_reference() {
        let parsed = parse_all(&fixture());
        let units = units_of(&parsed);
        let via_shards = Index::from_units(&units);
        let reference = from_units_reference(&units);
        assert!(via_shards == reference, "shard merge diverged from the frozen construction");
        // The fixture is only honest if the cross-shard machinery fired.
        assert!(via_shards.ambiguous_functions.contains("lib\\a\\helper"));
        assert!(via_shards.ambiguous_classes.contains("lib\\a\\dup"));
        assert!(via_shards.classes.contains_key("app\\widget"), "cross-shard alias minted");
        assert!(!via_shards.classes.contains_key("never"), "ambiguous target mints no edge");
        assert_eq!(via_shards.fn_by_simple["helper"].len(), 3);
        assert_eq!(
            via_shards.magic_obstacles["lib\\a\\same"].len(),
            1,
            "the tag-carrying Same, not its tag-free twin"
        );
        assert!(via_shards.property_writes.1 && via_shards.property_writes.0.contains("written"));
        assert!(via_shards.declares_constant("app\\LIMIT") && via_shards.declares_constant("FLAG"));
        assert_eq!(via_shards.file_index_of("vendor/autoload.php"), Some(4));
    }

    /// The oracle under the real Composer partition (the classification the
    /// generation build will use): grouping the same units by
    /// [`PackagePartition::package_of`] — a *different* partition from the
    /// production heuristic (`lib/b`'s files land in stray here, since only
    /// `lib/a` is locked) — merges to the identical index.
    #[test]
    fn composer_partition_shards_merge_like_the_reference() {
        let dir = std::path::PathBuf::from("/proj");
        let root = GoverningRoot::new(
            dir.join("composer.json"),
            dir.clone(),
            vec![dir.join("vendor")],
            vec![dir.join("src")],
        );
        let layout = ProjectLayout::new(dir, vec![root]);
        let lock = r#"{"packages": [
            {"name": "lib/a", "require": {"php": ">=8.1"}},
            {"name": "local/pkg", "dist": {"type": "path"}}
        ]}"#;
        let partition = PackagePartition::from_lock(&layout, Some(lock));

        let parsed = parse_all(&fixture());
        let units = units_of(&parsed);
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (slot, u) in units.iter().enumerate() {
            groups.entry(partition.package_of(u.path).as_str().to_owned()).or_default().push(slot);
        }
        assert!(groups.len() > 1, "the partition must actually split the fixture");
        assert!(groups.contains_key("lib/a"), "the locked package claims its files");
        let mut shards: Vec<PackageShard> = Vec::new();
        for slots in groups.into_values() {
            let mut s = PackageShard::default();
            for slot in slots {
                s.add_file(slot, units[slot].path, units[slot].tree);
            }
            shards.push(s);
        }
        let via_partition = Index::from_merged(merge_shards(&shards));
        assert!(via_partition == from_units_reference(&units), "partition → shards → merge diverged");
        assert!(via_partition == Index::from_units(&units), "two groupings, one merge");
    }

    /// The finest partition there is — one file per shard — merges to the
    /// identical index: the strongest statement of partition invariance.
    #[test]
    fn a_per_file_partition_merges_identically() {
        let parsed = parse_all(&fixture());
        let units = units_of(&parsed);
        let shards: Vec<PackageShard> = units
            .iter()
            .enumerate()
            .map(|(slot, u)| {
                let mut s = PackageShard::default();
                s.add_file(slot, u.path, u.tree);
                s
            })
            .collect();
        let merged = Index::from_merged(merge_shards(&shards));
        assert!(merged == from_units_reference(&units));
    }
}
