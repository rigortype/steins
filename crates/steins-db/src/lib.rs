//! The salsa demand-driven query database (ADR-0009).
//!
//! Every fact about a file is a memoized salsa query, not a batch
//! pipeline. This crate owns the database, the file input, and the *syntax-level*
//! queries ([`parse`], [`function_index`]). Semantic queries (the proof-layer
//! checks) are tracked queries defined in `steins-infer` against the [`Db`] trait
//! here, so the checking logic stays out of the engine crate while remaining a
//! first-class salsa query.

use std::collections::{BTreeMap, HashMap, HashSet};

use salsa::Storage;
use steins_syntax::{FunctionDecl, SourceTree};

pub mod composer;
pub mod effects;
pub mod layout;
pub mod partition;
pub mod plugins;
pub mod shard;

pub use effects::EffectsPolicy;
pub use layout::{GoverningRoot, PhpTarget, PhpTargetSource, ProjectLayout, fallback_is_vendor};
pub use partition::PackagePartition;
pub use plugins::PluginFacts;
pub use shard::{
    MagicObstacle, MergedTables, PackageShard, ShardSite, class_magic_obstacles,
    fallback_package_key, merge_shards,
};

/// The database trait analysis queries are written against. Downstream crates
/// (e.g. `steins-infer`) define tracked queries taking `&dyn Db`.
#[salsa::db]
pub trait Db: salsa::Database {}

/// A source file input: its path (for diagnostics) and full text. Mutating the
/// text via [`salsa::Setter`] creates a new revision and invalidates only the
/// queries that depended on it.
#[salsa::input]
pub struct SourceFile {
    #[returns(deref)]
    pub path: String,
    #[returns(deref)]
    pub text: String,
}

/// Parse a file into the owned, Mago-free [`SourceTree`] (ADR-0003). Memoized:
/// re-parsing only happens when the file text changes.
#[salsa::tracked]
pub fn parse(db: &dyn Db, file: SourceFile) -> SourceTree {
    SourceTree::parse(file.text(db))
}

/// The per-file index of user-defined function declarations. A separate query
/// so a call-site check can depend on the index without re-triggering on
/// unrelated body edits.
#[salsa::tracked]
pub fn function_index(db: &dyn Db, file: SourceFile) -> Vec<FunctionDecl> {
    parse(db, file).functions().to_vec()
}

// The project: a set of source files analyzed together as one salsa DB.

/// A whole-project input: the set of `.php` [`SourceFile`]s analyzed together
/// (ADR-0009/0015). Cross-file resolution ([`project_index`]) and the
/// project-wide inference in `steins-infer` are computed against this. Setting
/// the file list creates a new revision; the monolithic [`project_index`] then
/// re-runs (see its granularity note).
///
/// [`ProjectLayout`], [`PluginFacts`] and [`EffectsPolicy`] all ride along as
/// further inputs for the same reason: each decides part of what is reported
/// (vendor classification/ADR-0015, plugin-registered labels/ADR-0068, the
/// purity oracle/ADR-0084 §1), so each is *project* state, not ambient state,
/// and a replay must reach the same verdict from the same inputs (ADR-0048).
/// Each is resolved once at the boundary: layout by [`composer::discover`]
/// (falling back to [`ProjectLayout::fallback`] with no manifest), plugins by
/// [`plugins::PluginFacts::discover`] (empty via [`PluginFacts::none`] with no
/// plugin), effects from `steins.toml`'s `[effects]` table (`#[default]` empty,
/// the pre-ADR-0084 world, so [`Project::builder`] is only needed where a
/// policy is actually in hand).
#[salsa::input]
pub struct Project {
    #[returns(deref)]
    pub files: Vec<SourceFile>,
    #[returns(ref)]
    pub layout: ProjectLayout,
    #[returns(ref)]
    pub plugins: PluginFacts,
    #[default]
    #[returns(ref)]
    pub effects: EffectsPolicy,
}

/// Where a declaration lives: the owning file and its index in that file's
/// `functions()` / `classes()` list. The consumer re-derives the decl (and its
/// spans/scopes) via [`parse`] on `file` — memoized, so this is cheap.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DeclSite {
    pub file: SourceFile,
    pub index: usize,
}

/// The outcome of resolving an FQN against the project index.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Resolve {
    /// No such FQN is defined in the project.
    Absent,
    /// Exactly one definition — the resolvable case.
    Unique(DeclSite),
    /// Two or more files define this FQN (polyfills / conditional decls). PHP
    /// would fatal on a real double-definition and we can't know which body
    /// runs, so an ambiguous FQN is **never** resolved.
    Ambiguous,
}

/// The whole-project symbol index (ADR-0009). FQN keys are lowercase-normalized
/// (PHP function/class/namespace names are case-insensitive).
///
/// **Granularity:** one monolithic tracked query, so *any* file edit
/// invalidates it and everything downstream — acceptable for the batch CLI.
/// ADR-0009 recorded per-symbol salsa interning as the LSP plan; ADR-0092 §3
/// supersedes it — the index shards per *package* (see [`shard`]), with every
/// global table merged per generation, and this query already delegates to
/// that builder.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ProjectIndex {
    /// Unambiguous function FQN → definition site.
    functions: HashMap<String, DeclSite>,
    /// Unambiguous class FQN → definition site.
    classes: HashMap<String, DeclSite>,
    /// Function FQNs defined in more than one file (ambiguous → never resolved).
    ambiguous_functions: HashSet<String>,
    /// Class FQNs defined in more than one file (ambiguous → never resolved).
    ambiguous_classes: HashSet<String>,
    /// Lowercased simple function name → every definition site. Used where only
    /// the last segment is available at the use site (constant-function
    /// resolution, fold shadowing).
    fn_by_simple: HashMap<String, Vec<DeclSite>>,
}

impl ProjectIndex {
    /// Resolve a function FQN (case-insensitive).
    #[must_use]
    pub fn resolve_function(&self, fqn: &str) -> Resolve {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_functions.contains(&key) {
            Resolve::Ambiguous
        } else {
            self.functions.get(&key).copied().map_or(Resolve::Absent, Resolve::Unique)
        }
    }

    /// Resolve a class FQN (case-insensitive).
    #[must_use]
    pub fn resolve_class(&self, fqn: &str) -> Resolve {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_classes.contains(&key) {
            Resolve::Ambiguous
        } else {
            self.classes.get(&key).copied().map_or(Resolve::Absent, Resolve::Unique)
        }
    }

    /// The unique definition site of a function by its simple (last-segment)
    /// name, or `None` if absent or defined in more than one place. `simple` is
    /// matched case-insensitively.
    #[must_use]
    pub fn unique_by_simple(&self, simple: &str) -> Option<DeclSite> {
        match self.fn_by_simple.get(&simple.to_ascii_lowercase()) {
            Some(sites) if sites.len() == 1 => Some(sites[0]),
            _ => None,
        }
    }

    /// Whether the project defines any user function with this simple name
    /// (case-insensitive) — the fold-shadowing guard.
    #[must_use]
    pub fn has_simple_function(&self, simple: &str) -> bool {
        self.fn_by_simple.contains_key(&simple.to_ascii_lowercase())
    }

    /// Read access to the unambiguous function map (fqn → site).
    #[must_use]
    pub fn functions(&self) -> &HashMap<String, DeclSite> {
        &self.functions
    }

    /// Read access to the unambiguous class map (fqn → site).
    #[must_use]
    pub fn classes(&self) -> &HashMap<String, DeclSite> {
        &self.classes
    }

    /// The set of ambiguous (multiply-defined) function FQNs.
    #[must_use]
    pub fn ambiguous_functions(&self) -> &HashSet<String> {
        &self.ambiguous_functions
    }

    /// The set of ambiguous (multiply-defined) class FQNs.
    #[must_use]
    pub fn ambiguous_classes(&self) -> &HashSet<String> {
        &self.ambiguous_classes
    }

    /// Read access to the simple-name → sites map.
    #[must_use]
    pub fn fn_by_simple(&self) -> &HashMap<String, Vec<DeclSite>> {
        &self.fn_by_simple
    }
}

/// Build the whole-project symbol index via the package-shard builder
/// (ADR-0092 §3, issue #486): group the files into per-package shards, merge
/// every global table from them, and keep the symbol half. Duplicate FQNs are
/// demoted to the ambiguous set (and dropped from the resolvable map), so an
/// ambiguous symbol is never resolved — the merge's multiset arithmetic, which
/// the differential-oracle test pins against the pre-shard construction.
///
/// The grouping is [`fallback_package_key`] — a path heuristic, because no
/// `composer.lock` rides on [`Project`]; the merge is partition-invariant, so
/// the grouping decides shard boundaries and never the result.
#[salsa::tracked]
pub fn project_index(db: &dyn Db, project: Project) -> ProjectIndex {
    let files = project.files(db);
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (slot, file) in files.iter().enumerate() {
        groups.entry(shard::fallback_package_key(file.path(db))).or_default().push(slot);
    }
    let mut shards: Vec<PackageShard> = Vec::with_capacity(groups.len());
    for slots in groups.into_values() {
        let mut s = PackageShard::default();
        for slot in slots {
            let file = files[slot];
            let tree = parse(db, file);
            s.add_file(slot, file.path(db), tree);
        }
        shards.push(s);
    }
    ProjectIndex::from_merged(merge_shards(&shards), files)
}

impl ProjectIndex {
    /// The symbol half of a merge, with universe slots mapped back to the
    /// project's [`SourceFile`]s (`files` is the slot order the shards were
    /// built over). The merged obstacle/write/constant tables are dropped
    /// here: they are `steins_infer::project::Index`'s tables, scanned off the
    /// same shards on that side.
    fn from_merged(m: MergedTables, files: &[SourceFile]) -> Self {
        let site = |s: ShardSite| DeclSite { file: files[s.file], index: s.index };
        Self {
            functions: m.functions.into_iter().map(|(fqn, s)| (fqn, site(s))).collect(),
            classes: m.classes.into_iter().map(|(fqn, s)| (fqn, site(s))).collect(),
            ambiguous_functions: m.ambiguous_functions,
            ambiguous_classes: m.ambiguous_classes,
            fn_by_simple: m
                .fn_by_simple
                .into_iter()
                .map(|(simple, sites)| (simple, sites.into_iter().map(site).collect()))
                .collect(),
        }
    }
}

/// The concrete database used by the CLI and tests.
#[salsa::db]
#[derive(Clone, Default)]
pub struct SteinsDatabase {
    storage: Storage<Self>,
}

#[salsa::db]
impl salsa::Database for SteinsDatabase {}

#[salsa::db]
impl Db for SteinsDatabase {}

/// The differential oracle for the shard delegation (issue #486): the frozen
/// pre-shard construction of [`ProjectIndex`], kept test-only, and the
/// assertion that the partition → shards → merge path underneath
/// [`project_index`] reproduces it exactly. Inside the crate because both the
/// reference and the equality need the private fields.
#[cfg(test)]
mod shard_oracle {
    use super::*;

    /// The pre-#486 `project_index` body, frozen verbatim: a single streaming
    /// pass with duplicate demotion, then the literal `class_alias` fold
    /// against the textual snapshot (ADR-0049 §2).
    fn project_index_reference(db: &dyn Db, project: Project) -> ProjectIndex {
        fn insert_unique(
            map: &mut HashMap<String, DeclSite>,
            ambiguous: &mut HashSet<String>,
            fqn: &str,
            site: DeclSite,
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
        let mut idx = ProjectIndex::default();
        for &file in project.files(db) {
            let tree = parse(db, file);
            for (i, f) in tree.functions().iter().enumerate() {
                let site = DeclSite { file, index: i };
                idx.fn_by_simple.entry(f.name.to_ascii_lowercase()).or_default().push(site);
                insert_unique(&mut idx.functions, &mut idx.ambiguous_functions, &f.fqn, site);
            }
            for (i, c) in tree.classes().iter().enumerate() {
                let site = DeclSite { file, index: i };
                insert_unique(&mut idx.classes, &mut idx.ambiguous_classes, &c.fqn, site);
            }
        }
        let mut resolved: Vec<(String, DeclSite)> = Vec::new();
        for &file in project.files(db) {
            let tree = parse(db, file);
            for edge in tree.class_alias_edges() {
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
        idx
    }

    /// A fixture wide enough to make every merge leg observable: root and
    /// vendor paths (multiple shards under [`fallback_package_key`]),
    /// cross-shard function and class duplicates, a within-shard duplicate,
    /// simple-name collisions, and alias edges whose targets and collisions
    /// cross shard boundaries.
    fn fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "src/app.php",
                "<?php\nnamespace App;\nfunction run() {}\nfunction helper() {}\nclass Kernel {}\nclass_alias('lib\\\\a\\\\widget', 'app\\\\widget');\n",
            ),
            (
                "vendor/lib/a/src/widget.php",
                "<?php\nnamespace Lib\\A;\nfunction helper() {}\nclass Widget {}\nclass Dup {}\n",
            ),
            (
                "vendor/lib/b/src/dup.php",
                "<?php\nnamespace Lib\\A;\nclass Dup {}\nfunction helper() {}\n",
            ),
            (
                "vendor/lib/b/src/more.php",
                "<?php\nclass Local {}\nclass Local {}\nclass_alias('app\\\\kernel', 'shim');\nclass_alias('lib\\\\a\\\\dup', 'never');\n",
            ),
            ("vendor/autoload.php", "<?php\nfunction stray_helper() {}\n"),
        ]
    }

    #[test]
    fn project_index_matches_the_frozen_reference() {
        let db = SteinsDatabase::default();
        let inputs: Vec<SourceFile> = fixture()
            .iter()
            .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
            .collect();
        let project = Project::new(&db, inputs, ProjectLayout::fallback(), PluginFacts::none());
        let via_shards = project_index(&db, project);
        let reference = project_index_reference(&db, project);
        assert!(*via_shards == reference, "shard merge diverged from the frozen construction");
        // The fixture is only honest if the cross-shard machinery fired.
        assert!(via_shards.ambiguous_functions.contains("lib\\a\\helper"));
        assert!(via_shards.ambiguous_classes.contains("lib\\a\\dup"));
        assert!(via_shards.ambiguous_classes.contains("local"), "within-shard duplicate survives");
        assert!(via_shards.classes.contains_key("app\\widget"), "cross-shard alias minted");
        assert!(via_shards.classes.contains_key("shim"), "vendor alias of a root target minted");
        assert!(!via_shards.classes.contains_key("never"), "ambiguous target mints no edge");
        assert_eq!(via_shards.fn_by_simple["helper"].len(), 3);
    }
}
