//! The salsa demand-driven query database (ADR-0009).
//!
//! Every fact about a file is a memoized salsa query from day one — not a batch
//! pipeline. This crate owns the database, the file input, and the *syntax-level*
//! queries ([`parse`], [`function_index`]). Semantic queries (the proof-layer
//! checks) are tracked queries defined in `steins-infer` against the [`Db`] trait
//! here, so the checking logic stays out of the engine crate while remaining a
//! first-class salsa query.

use std::collections::{HashMap, HashSet};

use salsa::Storage;
use steins_syntax::{FunctionDecl, SourceTree};

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

// ---------------------------------------------------------------------------
// The project: a set of source files analyzed together as one salsa DB.
// ---------------------------------------------------------------------------

/// A whole-project input: the set of `.php` [`SourceFile`]s analyzed together
/// (ADR-0009/0015). Cross-file resolution ([`project_index`]) and the
/// project-wide inference in `steins-infer` are computed against this.
///
/// Setting the file list creates a new revision; the monolithic
/// [`project_index`] then re-runs (see its granularity note).
#[salsa::input]
pub struct Project {
    #[returns(deref)]
    pub files: Vec<SourceFile>,
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
    /// would fatal on a real double-definition, and we cannot know which body
    /// runs, so an ambiguous FQN is **never** resolved (silent).
    Ambiguous,
}

/// The whole-project symbol index (ADR-0009). FQN keys are lowercase-normalized
/// (PHP function/class/namespace names are case-insensitive).
///
/// **Granularity (ADR-0009):** this is one monolithic tracked query, so *any*
/// file edit invalidates it and every analysis downstream of it. That is
/// acceptable for the batch CLI; the recorded plan is per-symbol salsa interning
/// once the LSP lands, so an edit to one file re-indexes only its symbols.
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
    /// Lowercased simple function name → every definition site. Used for the
    /// simple-name checks (constant-function resolution and fold shadowing) where
    /// only the last segment is available at the use site.
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

    /// Read access to the unambiguous function map (fqn → site), for consumers
    /// that rebuild their own view keyed on file position.
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

/// Build the whole-project symbol index by parsing every file and folding its
/// declarations in. Duplicate FQNs are demoted to the ambiguous set (and dropped
/// from the resolvable map), so an ambiguous symbol is never resolved.
#[salsa::tracked]
pub fn project_index(db: &dyn Db, project: Project) -> ProjectIndex {
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
    idx
}

/// Insert `fqn → site`, demoting to ambiguity on any collision. `fqn` is already
/// lowercase-normalized by the syntax layer.
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
        // A second definition of the same FQN: mark ambiguous, keep it unresolved.
        ambiguous.insert(fqn.to_owned());
    } else {
        map.insert(fqn.to_owned(), site);
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
