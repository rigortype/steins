//! The project view: the files analyzed together and their symbol index —
//! [`FileUnit`], [`Index`], the magic-member obstacles (ADR-0046), the `Diagnostic` /
//! `Fix` records every pass emits, and the vendor-path test.

use std::collections::{HashMap, HashSet};

use steins_db::{DeclSite, ProjectIndex, Resolve, SourceFile};
use steins_phpdoc::{MagicTagKind, scan_magic_member_tags};
use steins_syntax::{ClassDecl, NameRef, RefKind, SourceTree};

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

/// One recorded **silence obstacle** a class-like's docblock declares (ADR-0049
/// A14, issue #195): a `@method` / `@property*` / `@mixin` / `@phpstan-type` tag
/// says members exist somewhere the index cannot enumerate, so every absence
/// proof over that class-like is silent.
///
/// The shape is normative: a record is `(class-like, obstacle kind, subject)` —
/// **never** a class-level "has magic somewhere" boolean. A plugin pack that
/// declares what the magic actually provides (ADR-0039) must be able to discharge
/// the obstacle member by member and re-enable the absence proof for the
/// undeclared remainder, which a boolean forecloses. The discharge channel itself
/// is not built here; only the record granularity that lets it be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicObstacle {
    /// The declaring class-like's own FQN, lowercase-normalized
    /// ([`ClassDecl::fqn`]) — the class-like half of the A14 triple.
    pub class: String,
    /// Which tag recorded it.
    pub kind: MagicTagKind,
    /// The tag's subject as written: the method name, the property name (no `$`),
    /// the mixin target reference, the alias name. Empty when the tag's tail gave
    /// none — presence alone still obstructs.
    pub subject: String,
    /// For [`MagicTagKind::Mixin`] only: [`Self::subject`] resolved to a project
    /// FQN against the declaring file's namespace and `use` imports, so the reach
    /// walk can follow it. A target that resolves to nothing is not a finding and
    /// not an error — the `@mixin` record itself already obstructs.
    pub mixin_target: Option<String>,
}

/// The project symbol index in the analysis's own `Site` terms (a file *index*,
/// not a salsa handle). Built either directly from the [`FileUnit`] slice
/// (single-file / test paths) or adapted from the salsa [`ProjectIndex`]
/// (the db-backed [`check_project`] path — so the tracked query is the authority
/// on incrementality, ADR-0009).
///
/// [`check_project`]: crate::check_project
#[derive(Default)]
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
    /// Scanned off the [`FileUnit`] slice rather than carried on the salsa
    /// [`ProjectIndex`], the same route [`Self::magic_obstacles`] takes: the
    /// lowering (which salsa memoizes) already holds the records, and a set with no
    /// site identity needs none of the project index's collision machinery.
    constants: HashSet<String>,
    // end global constants (ADR-0078, issue #198)
    /// Every unit's diagnostic path → its index in the [`FileUnit`] slice — the
    /// per-run derivation of a units index from the stable file identity (issue
    /// #497). A value type that survives a walk names a file by path; a consumer
    /// that needs a `Cx` (or unit order) looks the index up here rather than
    /// embedding one.
    files: HashMap<String, usize>,
}

// member absence (ADR-0078, issue #197)
/// Fold every file's property-write inventory into one project-wide obstacle set
/// (ADR-0078, issue #197). A whole-universe query in the ADR-0048 sense:
/// recomputed per run from the unit slice, with no ordering dependence.
fn scan_property_writes(units: &[FileUnit]) -> (HashSet<String>, bool) {
    let mut names: HashSet<String> = HashSet::new();
    let mut dynamic = false;
    for u in units {
        names.extend(u.tree.property_write_names().iter().cloned());
        dynamic |= u.tree.writes_computed_property_name();
    }
    (names, dynamic)
}
// end member absence (ADR-0078, issue #197)

/// Map every unit's diagnostic path to its position in the slice (issue #497) —
/// the [`Index::file_index_of`] table, built once per run from the same slice
/// every other per-run query reads.
fn scan_file_paths(units: &[FileUnit]) -> HashMap<String, usize> {
    units.iter().enumerate().map(|(i, u)| (u.path.to_owned(), i)).collect()
}

impl Index {
    /// Build the index straight from the file units (mirrors the db query).
    pub(crate) fn from_units(units: &[FileUnit]) -> Self {
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
        // Literal `class_alias` edges (ADR-0049 §2 / A2iii) fold in after every
        // textual decl, mirroring the db-backed `project_index`. Targets resolve
        // against the textual snapshot (order-independent, ADR-0048); collisions
        // (alias vs textual, or two aliases for one name) demote to `Ambiguous`.
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
        idx.magic_obstacles = scan_magic_obstacles(units);
        // member absence (ADR-0078, issue #197)
        idx.property_writes = scan_property_writes(units);
        // end member absence (ADR-0078, issue #197)
        idx.constants = scan_global_constants(units);
        idx.files = scan_file_paths(units);
        idx
    }

    /// Adapt the salsa [`ProjectIndex`] to `Site`s, using `pos` to map each
    /// [`SourceFile`] to its position in the (identically ordered) unit slice.
    /// The A14 obstacle records are scanned off the same unit slice: they are a
    /// docblock fact of the lowered tree (which salsa already memoizes), not a
    /// symbol-table fact the project index carries.
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
        idx.magic_obstacles = scan_magic_obstacles(units);
        // member absence (ADR-0078, issue #197)
        idx.property_writes = scan_property_writes(units);
        // end member absence (ADR-0078, issue #197)
        idx.constants = scan_global_constants(units);
        idx.files = scan_file_paths(units);
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

/// Scan the universe for every global constant declaration (ADR-0078, issue #198)
/// — `const FOO = …;` and `define('FOO', …)` alike, already normalized to the
/// matching key by the lowering.
///
/// Vendor files are **included**, deliberately: a constant a package declares is
/// as real as one the project declares, and the vendor presumption of ADR-0046 §2
/// is about unproven *dynamism*, not about ignoring plain declarations.
fn scan_global_constants(units: &[FileUnit]) -> HashSet<String> {
    let mut out = HashSet::new();
    for u in units {
        for decl in u.tree.global_const_decls() {
            out.insert(decl.fqn.clone());
        }
    }
    out
}

/// Scan every class-like docblock in the project for magic-member tags, keyed by
/// the declaring class-like's own lowercase FQN (ADR-0049 A14, issue #195).
///
/// A `@mixin` subject is resolved here, once, against its declaring file's
/// namespace and `use` imports — the same resolution a written `extends` gets,
/// because a docblock class reference obeys the same PHP name-resolution rules.
fn scan_magic_obstacles(units: &[FileUnit]) -> HashMap<String, Vec<MagicObstacle>> {
    let mut out: HashMap<String, Vec<MagicObstacle>> = HashMap::new();
    let mut buf: Vec<MagicObstacle> = Vec::new();
    for u in units {
        for cd in u.tree.classes() {
            class_magic_obstacles(u, cd, &mut buf);
            if !buf.is_empty() {
                out.entry(cd.fqn.to_ascii_lowercase()).or_default().append(&mut buf);
            }
        }
    }
    out
}

/// Append one class-like's own magic-member records to `out` (nothing appended
/// when it carries none).
fn class_magic_obstacles(u: &FileUnit, cd: &ClassDecl, out: &mut Vec<MagicObstacle>) {
    let Some(doc) = cd.docblock.as_deref() else { return };
    // Cheap reject: most class docblocks are prose, and the scan below is the
    // only place that pays.
    if !doc.contains('@') {
        return;
    }
    for tag in scan_magic_member_tags(doc) {
        let mixin_target = (tag.kind.is_mixin() && !tag.subject.is_empty())
            .then(|| u.tree.resolve_class_fqn(&docblock_class_ref(&tag.subject, cd.span.start)));
        out.push(MagicObstacle {
            class: cd.fqn.clone(),
            kind: tag.kind,
            subject: tag.subject,
            mixin_target,
        });
    }
}

/// Turn a class reference **written in a docblock** into a [`NameRef`] resolvable
/// against the declaring file's namespace context. `offset` is the declaration's
/// own position, so the context is the one that governs its `extends` clause.
fn docblock_class_ref(raw: &str, offset: u32) -> NameRef {
    if let Some(rest) = raw.strip_prefix('\\') {
        return NameRef { raw: rest.to_owned(), kind: RefKind::FullyQualified, offset };
    }
    // The `namespace\Foo` relative form (ADR-0049 A8) resolves against the
    // enclosing namespace with no imports applied; `raw` drops the prefix.
    // `get` (not a slice) because a class reference may be non-ASCII, and byte 10
    // is then not guaranteed to be a char boundary.
    if raw.get(..10).is_some_and(|p| p.eq_ignore_ascii_case("namespace\\")) {
        return NameRef { raw: raw[10..].to_owned(), kind: RefKind::Relative, offset };
    }
    let kind = if raw.contains('\\') { RefKind::Qualified } else { RefKind::Unqualified };
    NameRef { raw: raw.to_owned(), kind, offset }
}

/// Every magic-member obstacle record the project declares (ADR-0049 A14), in
/// file then source order — the seam a posture/`doctor` surface aggregates
/// ("N absence claims silenced by `@method` tags on M classes") and the one a
/// plugin discharge channel (ADR-0039) will subtract from. The ladders read the
/// same records through the per-class index built from this scan; nothing in this
/// slice reports them.
#[must_use]
pub fn magic_obstacles(units: &[FileUnit<'_>]) -> Vec<MagicObstacle> {
    let mut recs: Vec<MagicObstacle> = Vec::new();
    for u in units {
        for cd in u.tree.classes() {
            class_magic_obstacles(u, cd, &mut recs);
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

/// Insert `fqn → site`, demoting to ambiguity on any collision.
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
