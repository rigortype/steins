//! The salsa demand-driven query database (ADR-0009).
//!
//! Every fact about a file is a memoized salsa query from day one — not a batch
//! pipeline. This crate owns the database, the file input, and the *syntax-level*
//! queries ([`parse`], [`function_index`]). Semantic queries (the proof-layer
//! checks) are tracked queries defined in `steins-infer` against the [`Db`] trait
//! here, so the checking logic stays out of the engine crate while remaining a
//! first-class salsa query.

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
