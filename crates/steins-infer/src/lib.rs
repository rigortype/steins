//! The inference engine — now whole-project (cross-file) resolution.
//!
//! It implements the proof-layer diagnostics (ADR-0002, held to the
//! zero-false-positive bar): [`ID`] = `type.argument-mismatch`, plus the
//! effect-envelope checks. A call to a **user-defined function or method
//! resolved anywhere in the project** that passes a **literal** argument which
//! **provably** raises a runtime `TypeError` under PHP 8.1+ semantics
//! (ADR-0011), honoring the calling file's `declare(strict_types=1)`, is
//! flagged. Everything not provable is silent.
//!
//! Name resolution follows PHP semantics conservatively (ADR-0001): fully-
//! qualified / qualified / unqualified names resolve against a project symbol
//! index ([`steins_db::project_index`]) plus the builtin catalog, with
//! `use` imports and the namespace/global fallback applied. Ambiguous symbols
//! (duplicate FQN, builtin-shadowing) are never resolved — silent.
//!
//! The single-file entry points ([`check`], [`check_file`], [`diagnostics`])
//! run over a one-file project, so every same-file soundness guard keeps
//! working unchanged; [`check_project`] / [`annotate_project`] run over many.

pub mod suppress;

pub use suppress::{
    DIAGNOSTIC_IDS, InlineOutcome, SUPPRESS_UNKNOWN_ID, SUPPRESS_UNMATCHED_ID,
    apply_inline_ignores, pattern_is_known,
};

use std::collections::{HashMap, HashSet};

use steins_db::{Db, DeclSite, Project, ProjectIndex, Resolve, SourceFile, parse, project_index};
use steins_sidecar::{FoldArg, FoldResult, FoldValue, Sidecar};
use steins_syntax::CallExpr;
use steins_syntax::{
    ArgValue, ArrayKey, Callee, ClassDecl, EffectEnvelope, EffectOrigin, EffectRecv, FunctionDecl,
    MethodDecl, NameRef, NativeType, NormKey, Param, Receiver, RefKind, ScalarType, Scope,
    ScopeOwner, SourceTree, StaticClass, StmtKind, TypeMember, Visibility, normalize_array,
};

use steins_phpdoc::ast::{ArrayShapeKind, ConstExpr, ShapeKey, StringLit, TypeKind as PKind};
use steins_phpdoc::{TagKind, Type as PType, parse_type, scan_docblock};

/// The registry id for the `type.argument-mismatch` proof-layer check (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

/// The registry id for the `type.return-mismatch` proof-layer check (ADR-0022):
/// a function/method whose return type is a native scalar/union and one of its
/// (trace-visible) `return <literal>;` statements provably raises a `TypeError`.
pub const RETURN_ID: &str = "type.return-mismatch";

/// The registry id for the phpdoc declared-contract param check (ADR-0030 relation
/// #1): a proven value flowing into a parameter with a `@param` phpdoc envelope
/// that it provably does **not** inhabit under contract (set) acceptance — no
/// coercion (a numeric string `"5"` does not satisfy `int` here). Distinct from the
/// runtime relation ([`ID`]); phpdoc types are never enforced at runtime.
pub const PARAM_MISMATCH_ID: &str = "phpdoc.param-mismatch";

/// The registry id for the phpdoc declared-contract return check (ADR-0030): a
/// proven `return <value>;` that provably does not inhabit the `@return` envelope
/// under contract acceptance.
pub const RETURN_MISMATCH_ID: &str = "phpdoc.return-mismatch";

/// The registry id for the effect-envelope check (ADR-0005/0022): a function
/// declared `#[\Steins\Pure]` / `#[\Steins\Effect(...)]` whose inferred effects
/// exceed the declared envelope (ADR-0018 prefix subsumption).
pub const EFFECT_ID: &str = "effect.envelope-exceeded";

/// The registry id for the unknown-effect-label check (ADR-0018/0022): a declared
/// `#[\Steins\Effect(...)]` label that is not in the label registry
/// ([`steins_catalog::is_known_label`]) — a typo or an unregistered private label.
pub const UNKNOWN_LABEL_ID: &str = "effect.unknown-label";

/// The maximum depth of interprocedural argument-binding descent (Feature B).
///
/// ADR-0009 makes inference cutoffs a first-class budget discipline: a chain of
/// calls propagating a literal is followed at most this many frames deep, after
/// which the descent stops with **no** diagnostic (a cutoff names itself as
/// silence, never a manufactured finding). Direct and indirect recursion is
/// caught earlier by the on-stack binding set; this bound guards against merely
/// long, non-cyclic chains.
pub const MAX_BINDING_DEPTH: usize = 8;

/// The one-line coverage-posture notice (ADR-0004): printed to stderr when a run
/// executes as the sound subset because the PHP sidecar is unavailable.
pub const SOUND_SUBSET_NOTICE: &str =
    "note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted";

// ---------------------------------------------------------------------------
// Folding seam (ADR-0004 / ADR-0024). Unchanged from the per-file slice.
// ---------------------------------------------------------------------------

/// Something that can fold a builtin call to a concrete literal value.
pub trait Folder {
    /// Fold `name(args...)` to a literal, or `None` to widen.
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue>;
}

/// The sound-subset folder: never folds anything. This is what the salsa
/// [`diagnostics`] query uses, keeping that query deterministic.
pub struct NoFold;

impl Folder for NoFold {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
}

/// A [`Folder`] backed by a lazily-spawned PHP [`Sidecar`], with a per-run memo
/// so a repeated `(name, args)` never triggers duplicate IPC.
pub struct SidecarFolder {
    sidecar: Option<Sidecar>,
    memo: HashMap<(String, Vec<ArgValue>), Option<ArgValue>>,
    disabled: bool,
    spawn_failed: bool,
    notified: bool,
}

impl SidecarFolder {
    /// Create a folder. `disabled` (the CLI's `--no-php`) makes it a permanent
    /// no-op that never spawns PHP.
    #[must_use]
    pub fn new(disabled: bool) -> Self {
        Self {
            sidecar: None,
            memo: HashMap::new(),
            disabled,
            spawn_failed: false,
            notified: true, // suppress our own notice; only spawn-failure re-arms it.
        }
    }

    /// Create an enabled folder that will emit the sound-subset notice itself if
    /// it cannot spawn PHP.
    #[must_use]
    pub fn enabled() -> Self {
        Self { notified: false, ..Self::new(false) }
    }

    /// Ensure a live sidecar, or record that we cannot have one.
    fn ensure_sidecar(&mut self) -> Option<&mut Sidecar> {
        if self.disabled || self.spawn_failed {
            return None;
        }
        if self.sidecar.is_none() {
            match Sidecar::spawn() {
                Ok(sc) => self.sidecar = Some(sc),
                Err(_) => {
                    self.spawn_failed = true;
                    if !self.notified {
                        eprintln!("{SOUND_SUBSET_NOTICE}");
                        self.notified = true;
                    }
                    return None;
                }
            }
        }
        self.sidecar.as_mut()
    }
}

impl Folder for SidecarFolder {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        let key = (name.to_owned(), args.to_vec());
        if let Some(cached) = self.memo.get(&key) {
            return cached.clone();
        }
        let folded = self.ensure_sidecar().and_then(|sc| {
            let fargs: Vec<FoldArg> = args.iter().filter_map(arg_to_fold).collect();
            if fargs.len() != args.len() {
                return None;
            }
            match sc.fold(name, &fargs) {
                FoldResult::Value(v) => fold_value_to_arg(&v),
                FoldResult::Throw { .. } | FoldResult::Widen { .. } => None,
            }
        });
        self.memo.insert(key, folded.clone());
        folded
    }
}

/// Convert a literal [`ArgValue`] to a [`FoldArg`]; non-literals yield `None`.
fn arg_to_fold(arg: &ArgValue) -> Option<FoldArg> {
    match arg {
        ArgValue::Int(v) => Some(FoldArg::Int(*v)),
        ArgValue::Float(v) => Some(FoldArg::Float(*v)),
        ArgValue::Str(v) => Some(FoldArg::Str(v.clone())),
        ArgValue::Bool(v) => Some(FoldArg::Bool(*v)),
        ArgValue::Null => Some(FoldArg::Null),
        ArgValue::Var(_)
        | ArgValue::Call(..)
        | ArgValue::New(..)
        | ArgValue::Array(_)
        | ArgValue::Other => None,
    }
}

/// Convert a folded value back to a literal [`ArgValue`].
fn fold_value_to_arg(value: &FoldValue) -> Option<ArgValue> {
    Some(match value {
        FoldValue::Int(v) => ArgValue::Int(*v),
        FoldValue::Float(v) => ArgValue::Float(*v),
        FoldValue::Str(v) => ArgValue::Str(v.clone()),
        FoldValue::Bool(v) => ArgValue::Bool(*v),
        FoldValue::Null => ArgValue::Null,
    })
}

/// Whether a diagnostic path lies inside a `vendor/` directory (ADR-0015).
///
/// Vendor code is fully indexed and inferred (shapes/values/effects flow
/// through it), but its diagnostics are off by default: a finding whose path has
/// a `vendor` **directory component** — a top-level `vendor/…` or any nested
/// `…/vendor/…` — is vendor. The match is on whole path components (split on
/// both `/` and `\`), so a sibling like `vendor_proj/` or a file named
/// `vendor.php` is *not* vendor. The trailing filename can never equal `vendor`
/// (it carries a `.php` extension), so a bare component test is exact.
pub fn is_vendor_path(path: &str) -> bool {
    path.split(['/', '\\']).any(|component| component == "vendor")
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
struct Site {
    file: usize,
    index: usize,
}

/// The outcome of resolving an FQN against the in-memory project index.
#[derive(Clone, Copy)]
enum Res {
    Absent,
    Unique(Site),
    Ambiguous,
}

/// The project symbol index in the analysis's own `Site` terms (a file *index*,
/// not a salsa handle). Built either directly from the [`FileUnit`] slice
/// (single-file / test paths) or adapted from the salsa [`ProjectIndex`]
/// (the db-backed [`check_project`] path — so the tracked query is the authority
/// on incrementality, ADR-0009).
#[derive(Default)]
struct Index {
    functions: HashMap<String, Site>,
    ambiguous_functions: HashSet<String>,
    classes: HashMap<String, Site>,
    ambiguous_classes: HashSet<String>,
    fn_by_simple: HashMap<String, Vec<Site>>,
}

impl Index {
    /// Build the index straight from the file units (mirrors the db query).
    fn from_units(units: &[FileUnit]) -> Self {
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
        idx
    }

    /// Adapt the salsa [`ProjectIndex`] to `Site`s, using `pos` to map each
    /// [`SourceFile`] to its position in the (identically ordered) unit slice.
    fn from_db(db_index: &ProjectIndex, pos: &HashMap<SourceFile, usize>) -> Self {
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
        idx
    }

    fn resolve_function(&self, fqn: &str) -> Res {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_functions.contains(&key) {
            Res::Ambiguous
        } else {
            self.functions.get(&key).copied().map_or(Res::Absent, Res::Unique)
        }
    }

    fn resolve_class(&self, fqn: &str) -> Res {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_classes.contains(&key) {
            Res::Ambiguous
        } else {
            self.classes.get(&key).copied().map_or(Res::Absent, Res::Unique)
        }
    }

    fn unique_fn_by_simple(&self, simple: &str) -> Option<Site> {
        match self.fn_by_simple.get(&simple.to_ascii_lowercase()) {
            Some(sites) if sites.len() == 1 => Some(sites[0]),
            _ => None,
        }
    }

    fn has_simple_function(&self, simple: &str) -> bool {
        self.fn_by_simple.contains_key(&simple.to_ascii_lowercase())
    }
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
enum FnResolution {
    /// A user function defined in the project (its declaration site).
    User(Site),
    /// A catalogued builtin — no user body, but folding/effect labels apply.
    Builtin,
    /// Ambiguous or unresolved — skip everything (no check, no fold, no effect
    /// classification). The silent side.
    Unknown,
}

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// The proof-layer diagnostics for one file, as a memoized salsa query (sound
/// subset — [`NoFold`], no PHP). Analyzes the file as a one-file project.
#[salsa::tracked]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, &mut NoFold)
}

/// The folding-aware check for one file (run **outside** salsa; ADR-0004),
/// analyzed as a one-file project.
#[must_use]
pub fn check_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, folder)
}

/// The folding-aware check for a whole **project** (ADR-0009/0015): every file
/// in `project` is analyzed as one unit, so cross-file calls, class chains, and
/// effects resolve. Resolution is driven by the salsa [`project_index`] query.
#[must_use]
pub fn check_project(db: &dyn Db, project: Project, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos);
    check_units(&units, &index, folder)
}

/// The pure single-file check (sound subset). Kept for unit tests and callers
/// that never execute PHP. `functions` is accepted for signature stability; the
/// tree's own function list is authoritative.
#[must_use]
pub fn check(tree: &SourceTree, functions: &[FunctionDecl], path: &str) -> Vec<Diagnostic> {
    check_with(tree, functions, path, &mut NoFold)
}

/// The folding-aware single-file check core, analyzed as a one-file project.
#[must_use]
pub fn check_with(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<Diagnostic> {
    let _ = functions; // authoritative list comes from `tree.functions()`
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, folder)
}

/// The project checking core: direct + propagation passes over every file's
/// calls and scopes, then the one project-wide effects pass.
fn check_units(units: &[FileUnit], index: &Index, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);

        // --- Direct pass: literal / array / `new` arguments at every function
        // call site (env-free; propagation adds `$var`/folded resolution). Native
        // scalar checks and the phpdoc declared-contract check both run here; a
        // site where the native check fired is skipped by the phpdoc check (no
        // double-report; ADR-0030). ---------------------------------------------
        let empty_env: HashMap<String, Known> = HashMap::new();
        let empty_classes: HashMap<String, String> = HashMap::new();
        for call in cx.tree().calls() {
            let Some(site) = cx.resolve_user_fn(call) else { continue };
            let decl = cx.fn_decl(site);
            let envelopes = parse_envelopes(decl.docblock.as_deref());
            for (i, arg) in call.args.iter().enumerate() {
                let Some(param) = decl.params.get(i) else { break };
                if param.variadic {
                    break;
                }
                if param.by_ref {
                    continue;
                }
                let mut native_fired = false;
                if let Some(ty) = param.ty.as_ref()
                    && arg.value.is_literal()
                    && is_type_error(cx.strict(), ty, &arg.value)
                {
                    out.push(cx.diagnostic(
                        arg.span.start,
                        &arg.value,
                        None,
                        &decl.name,
                        &param.name,
                        ty,
                    ));
                    native_fired = true;
                }
                // The direct pass owns env-free arg kinds (literal / array / `new`);
                // `$var`/`call()` resolution — and their phpdoc check — belong to the
                // propagation pass, so the two never both fire on one arg.
                let env_free =
                    arg.value.is_literal() || matches!(arg.value, ArgValue::Array(_) | ArgValue::New(..));
                if !native_fired
                    && env_free
                    && let Some(env) = &envelopes
                {
                    check_phpdoc_param(
                        &cx,
                        folder,
                        env,
                        param,
                        site.file,
                        decl.span.start,
                        &decl.name,
                        arg.span.start,
                        &arg.value,
                        &empty_env,
                        &empty_classes,
                        false,
                        &mut out,
                    );
                }
            }
        }

        // --- Propagation pass: resolved `$var`/const/folded args + all method /
        // static / constructor call checking and descent. --------------------
        for scope in cx.tree().scopes() {
            analyze_scope(&cx, folder, scope, HashMap::new(), HashMap::new(), None, None, None, &mut out);
        }
    }

    // --- Effects pass (ADR-0005), computed once over the whole project. ------
    out.extend(effect_diagnostics(units, index));

    dedup(&mut out);
    out
}

/// Drop exact-duplicate diagnostics, preserving first-occurrence order.
fn dedup(out: &mut Vec<Diagnostic>) {
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
}

// ---------------------------------------------------------------------------
// `annotate` facts (ADR-0020): the Rigor-style margin — proven facts only.
// ---------------------------------------------------------------------------

/// One proven fact the `annotate` margin can print against a source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactKind {
    Effects { labels: Vec<String>, exhaustive: bool },
    Value { var: String, rendered: String },
    ExactClass { var: String, class: String },
    Finding { id: &'static str },
}

/// A [`FactKind`] keyed to a 1-based source line (ADR-0020 margin display).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineFact {
    pub line: u32,
    pub kind: FactKind,
}

impl LineFact {
    /// The margin body (without the `//=>` prefix or padding).
    #[must_use]
    pub fn body(&self) -> String {
        match &self.kind {
            FactKind::Effects { labels, exhaustive } => {
                let mut parts = labels.clone();
                if !*exhaustive {
                    parts.push("…?".to_owned());
                }
                format!("effects: {{{}}}", parts.join(", "))
            }
            FactKind::Value { var, rendered } => format!("${var} = {rendered}"),
            FactKind::ExactClass { var, class } => format!("${var}: {class} (exact)"),
            FactKind::Finding { id } => format!("✗ {id}"),
        }
    }
}

/// Single-file annotate facts (kept for tests / the no-`--project` CLI path).
#[must_use]
pub fn annotate_facts(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<LineFact> {
    let _ = (functions, classes);
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    annotate_units(&units, &index, 0, folder)
}

/// Salsa-fed single-file annotate.
#[must_use]
pub fn annotate_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<LineFact> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    annotate_units(&units, &index, 0, folder)
}

/// Project-aware annotate (ADR-0020, `--project`): compute the margin facts for
/// `target` while resolving names, classes, and effects against the whole
/// `project`. Returns facts for the target file only.
#[must_use]
pub fn annotate_project(
    db: &dyn Db,
    project: Project,
    target: SourceFile,
    folder: &mut dyn Folder,
) -> Vec<LineFact> {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos);
    let Some(target_idx) = handles.iter().position(|&f| f == target) else {
        return Vec::new();
    };
    annotate_units(&units, &index, target_idx, folder)
}

/// Compute the annotate facts for `target` file within a project view.
fn annotate_units(
    units: &[FileUnit],
    index: &Index,
    target: usize,
    folder: &mut dyn Folder,
) -> Vec<LineFact> {
    let mut facts: Vec<LineFact> = Vec::new();

    // 1. Effects on each declaration line in the target file.
    for s in effect_summary_units(units, index, target) {
        facts.push(LineFact {
            line: s.line,
            kind: FactKind::Effects { labels: s.labels, exhaustive: s.exhaustive },
        });
    }

    // 2. Value / exact-class facts from the propagation walk of the target file.
    let cx = Cx::new(units, index, target);
    let mut sink: Vec<Diagnostic> = Vec::new();
    for scope in cx.tree().scopes() {
        analyze_scope(
            &cx,
            folder,
            scope,
            HashMap::new(),
            HashMap::new(),
            None,
            None,
            Some(&mut facts),
            &mut sink,
        );
    }

    // 3. Findings on the target file (project-wide check, filtered by path).
    let target_path = units[target].path;
    for d in check_units(units, index, folder) {
        if d.path == target_path {
            facts.push(LineFact { line: d.line, kind: FactKind::Finding { id: d.id } });
        }
    }

    facts.sort_by_key(|f| f.line);
    facts
}

// ---------------------------------------------------------------------------
// Effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking, project-wide.
// ---------------------------------------------------------------------------

/// One proven effect a unit carries, with the provenance a transitive `via`
/// message needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectFinding {
    label: String,
    origin: String,
    line: u32,
    /// The path of the file the origin lives in, so a transitive `via` message
    /// can name the other file when the effect arises cross-file.
    path: String,
}

/// A node in the unified project effect call graph — a free function (keyed by
/// FQN) or a class method (keyed by class FQN + method name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Sym {
    Func(String),
    Method(String, String),
}

/// One unit's fixpoint result: its proven effect findings and exhaustiveness.
#[derive(Debug, Clone, Default)]
struct EffectSet {
    findings: HashSet<EffectFinding>,
    exhaustive: bool,
}

/// The unified effect fixpoint for **every** function and method in the whole
/// project, keyed by [`Sym`] (FQN-based, so cross-file edges match).
fn compute_effects(units: &[FileUnit], index: &Index) -> HashMap<Sym, EffectSet> {
    // Each effect unit with the file it lives in and its enclosing class FQN.
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        origins: &'a [EffectOrigin],
    }
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        for f in u.tree.functions() {
            ulist.push(Unit { sym: Sym::Func(f.fqn.clone()), file: fi, class_fqn: None, origins: &f.effect_origins });
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                ulist.push(Unit {
                    sym: Sym::Method(c.fqn.clone(), m.name.clone()),
                    file: fi,
                    class_fqn: Some(c.fqn.clone()),
                    origins: &m.effect_origins,
                });
            }
        }
    }

    let mut direct: HashMap<Sym, HashSet<EffectFinding>> = HashMap::new();
    let mut edges: HashMap<Sym, HashSet<Sym>> = HashMap::new();
    let mut exhaustive: HashMap<Sym, bool> = HashMap::new();
    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        let d = direct.entry(unit.sym.clone()).or_default();
        let e = edges.entry(unit.sym.clone()).or_default();
        let ex = exhaustive.entry(unit.sym.clone()).or_insert(true);
        for origin in unit.origins {
            match origin {
                EffectOrigin::Call { name, span } => match cx.resolve_function(name) {
                    FnResolution::User(site) => {
                        e.insert(Sym::Func(cx.fn_decl(site).fqn.clone()));
                    }
                    FnResolution::Builtin => {
                        for f in builtin_findings(name.simple(), *span, cx.tree(), cx.path()) {
                            d.insert(f);
                        }
                    }
                    // Ambiguous / unresolved: effects unknown → non-exhaustive.
                    FnResolution::Unknown => *ex = false,
                },
                EffectOrigin::Output { keyword, span } => {
                    d.insert(EffectFinding {
                        label: "output".to_owned(),
                        origin: (*keyword).to_owned(),
                        line: cx.tree().position(span.start).line,
                        path: cx.path().to_owned(),
                    });
                }
                EffectOrigin::Exit { keyword, span } => {
                    d.insert(EffectFinding {
                        label: "exit".to_owned(),
                        origin: (*keyword).to_owned(),
                        line: cx.tree().position(span.start).line,
                        path: cx.path().to_owned(),
                    });
                }
                EffectOrigin::MethodCall { receiver, method, .. } => {
                    match resolve_effect_edge(&cx, unit.class_fqn.as_deref(), receiver, method) {
                        Some(callee) => {
                            e.insert(callee);
                        }
                        None => *ex = false,
                    }
                }
                EffectOrigin::Opaque { .. } => *ex = false,
            }
        }
    }

    // Fixpoint: effects(u) = direct(u) ∪ ⋃ effects(callees); exhaustive taints.
    let syms: Vec<Sym> = ulist.iter().map(|u| u.sym.clone()).collect();
    let mut findings: HashMap<Sym, HashSet<EffectFinding>> = direct;
    loop {
        let mut changed = false;
        for sym in &syms {
            let callees: Vec<Sym> = edges.get(sym).into_iter().flatten().cloned().collect();
            let mut incoming: Vec<EffectFinding> = Vec::new();
            let mut callee_taint = false;
            for c in &callees {
                if let Some(ce) = findings.get(c) {
                    incoming.extend(ce.iter().cloned());
                }
                if exhaustive.get(c).copied() == Some(false) {
                    callee_taint = true;
                }
            }
            let set = findings.entry(sym.clone()).or_default();
            for ef in incoming {
                changed |= set.insert(ef);
            }
            if callee_taint && exhaustive.get(sym).copied() != Some(false) {
                exhaustive.insert(sym.clone(), false);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    syms.into_iter()
        .map(|s| {
            let f = findings.remove(&s).unwrap_or_default();
            let ex = exhaustive.get(&s).copied().unwrap_or(true);
            (s, EffectSet { findings: f, exhaustive: ex })
        })
        .collect()
}

/// One line of the `annotate` effect margin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSummary {
    pub symbol: String,
    pub line: u32,
    pub labels: Vec<String>,
    pub exhaustive: bool,
}

/// The proven effect set of every concrete function/method in a single file
/// (ADR-0020 annotate margin). Analyzed as a one-file project.
#[must_use]
pub fn effect_summary(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
) -> Vec<EffectSummary> {
    let _ = (functions, classes);
    let units = [FileUnit { path: "", tree }];
    let index = Index::from_units(&units);
    effect_summary_units(&units, &index, 0)
}

/// The proven effect set of every concrete function/method in the `target` file.
#[must_use]
fn effect_summary_units(units: &[FileUnit], index: &Index, target: usize) -> Vec<EffectSummary> {
    let effects = compute_effects(units, index);
    let tree = units[target].tree;
    let sorted_labels = |sym: &Sym| -> Vec<String> {
        let mut labels: Vec<String> = effects
            .get(sym)
            .into_iter()
            .flat_map(|e| e.findings.iter().map(|f| f.label.clone()))
            .collect();
        labels.sort();
        labels.dedup();
        labels
    };
    let exhaustive = |sym: &Sym| effects.get(sym).is_none_or(|e| e.exhaustive);

    let mut out = Vec::new();
    for f in tree.functions() {
        let sym = Sym::Func(f.fqn.clone());
        out.push(EffectSummary {
            symbol: f.name.clone(),
            line: tree.position(f.span.start).line,
            labels: sorted_labels(&sym),
            exhaustive: exhaustive(&sym),
        });
    }
    for c in tree.classes() {
        for m in &c.methods {
            if m.is_abstract {
                continue;
            }
            let sym = Sym::Method(c.fqn.clone(), m.name.clone());
            out.push(EffectSummary {
                symbol: format!("{}::{}", c.name, m.name),
                line: tree.position(m.span.start).line,
                labels: sorted_labels(&sym),
                exhaustive: exhaustive(&sym),
            });
        }
    }
    out
}

/// Effect-envelope diagnostics for the whole project (proven violations only).
fn effect_diagnostics(units: &[FileUnit], index: &Index) -> Vec<Diagnostic> {
    // Fast path: no envelope anywhere → nothing to check.
    let any_envelope = units.iter().any(|u| {
        u.tree.functions().iter().any(|f| f.effect_envelope.is_some())
            || u.tree.classes().iter().any(|c| c.methods.iter().any(|m| m.effect_envelope.is_some()))
    });
    if !any_envelope {
        return Vec::new();
    }

    let effects = compute_effects(units, index);
    let mut out = Vec::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let Some(env) = &f.effect_envelope else { continue };
            report_unit(&mut out, &cx, None, &f.name, env, &f.effect_origins, &effects);
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                let Some(env) = &m.effect_envelope else { continue };
                let display = format!("{}::{}", c.name, m.name);
                report_unit(
                    &mut out,
                    &cx,
                    Some(&c.fqn),
                    &display,
                    env,
                    &m.effect_origins,
                    &effects,
                );
            }
        }
    }
    out
}

/// Emit the diagnostics for one declared-envelope unit (ADR-0005/0018).
fn report_unit(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    class_fqn: Option<&str>,
    display: &str,
    envelope: &EffectEnvelope,
    origins: &[EffectOrigin],
    effects: &HashMap<Sym, EffectSet>,
) {
    // 1. Unknown declared labels (one diagnostic each, at the attribute span).
    for label in &envelope.labels {
        if steins_catalog::is_known_label(label) {
            continue;
        }
        let suggestion = steins_catalog::nearest_label(label)
            .map(|s| format!(" — did you mean '{s}'?"))
            .unwrap_or_default();
        let msg = format!(
            "unknown effect label '{label}' in #[\\Steins\\Effect] on {display}(){suggestion}"
        );
        let pos = cx.tree().position(envelope.span.start);
        out.push(Diagnostic {
            id: UNKNOWN_LABEL_ID,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: msg,
        });
    }

    // 2. Envelope-exceeded violations.
    let labels = &envelope.labels;
    for origin in origins {
        match origin {
            EffectOrigin::Call { name, span } => match cx.resolve_function(name) {
                FnResolution::User(site) => {
                    let callee = Sym::Func(cx.fn_decl(site).fqn.clone());
                    emit_transitive(out, cx, &callee, effects, span.start, display, labels);
                }
                FnResolution::Builtin => {
                    for f in builtin_findings(name.simple(), *span, cx.tree(), cx.path()) {
                        if exceeds(labels, &f.label) {
                            let prefix = format!("{}() has effect {}", name.simple(), f.label);
                            out.push(exceeded_diag(cx, span.start, &prefix, display, labels, &f.label));
                        }
                    }
                }
                FnResolution::Unknown => {}
            },
            EffectOrigin::Output { keyword, span } if exceeds(labels, "output") => {
                let prefix = format!("{keyword} has effect output");
                out.push(exceeded_diag(cx, span.start, &prefix, display, labels, "output"));
            }
            EffectOrigin::Exit { keyword, span } if exceeds(labels, "exit") => {
                let prefix = format!("{keyword} has effect exit");
                out.push(exceeded_diag(cx, span.start, &prefix, display, labels, "exit"));
            }
            EffectOrigin::MethodCall { receiver, method, span } => {
                if let Some(callee) = resolve_effect_edge(cx, class_fqn, receiver, method) {
                    emit_transitive(out, cx, &callee, effects, span.start, display, labels);
                }
            }
            EffectOrigin::Output { .. } | EffectOrigin::Exit { .. } => {}
            EffectOrigin::Opaque { .. } => {}
        }
    }
}

/// Emit each proven effect of `callee` not subsumed by the envelope as a
/// transitive violation, naming the ultimate origin.
fn emit_transitive(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    callee: &Sym,
    effects: &HashMap<Sym, EffectSet>,
    offset: u32,
    display: &str,
    labels: &[String],
) {
    let callee_display = cx.sym_display(callee);
    let mut fs: Vec<&EffectFinding> =
        effects.get(callee).map(|e| &e.findings).into_iter().flatten().collect();
    fs.sort_by(|a, b| (a.line, &a.label, &a.origin).cmp(&(b.line, &b.label, &b.origin)));
    for ef in fs {
        if !exceeds(labels, &ef.label) {
            continue;
        }
        // Name the file when the ultimate origin arises in a different file than
        // the declared-envelope unit being reported (cross-file provenance).
        let loc = if ef.path == cx.path() {
            format!("line {}", ef.line)
        } else {
            format!("{} line {}", ef.path, ef.line)
        };
        let prefix =
            format!("{callee_display}() has effect {} (via {} at {loc})", ef.label, ef.origin);
        out.push(exceeded_diag(cx, offset, &prefix, display, labels, &ef.label));
    }
}

/// Whether an inferred `effect_label` **exceeds** the declared `labels`.
fn exceeds(labels: &[String], effect_label: &str) -> bool {
    !labels.iter().any(|l| steins_catalog::subsumes(l, effect_label))
}

/// Build an `effect.envelope-exceeded` diagnostic.
fn exceeded_diag(
    cx: &Cx,
    offset: u32,
    prefix: &str,
    display: &str,
    labels: &[String],
    exceeding_label: &str,
) -> Diagnostic {
    let clause = if labels.is_empty() {
        "#[\\Steins\\Pure]".to_owned()
    } else {
        let quoted: Vec<String> = labels.iter().map(|l| format!("'{l}'")).collect();
        format!(
            "#[\\Steins\\Effect({})] — {exceeding_label} exceeds the envelope",
            quoted.join(", ")
        )
    };
    let msg = format!("{prefix}, but {display}() is declared {clause}");
    let pos = cx.tree().position(offset);
    Diagnostic { id: EFFECT_ID, path: cx.path().to_owned(), line: pos.line, column: pos.column, message: msg }
}

/// The proven effect findings a builtin `name` carries (empty for pure or
/// uncatalogued builtins).
fn builtin_findings(
    name: &str,
    span: steins_syntax::Span,
    tree: &SourceTree,
    path: &str,
) -> Vec<EffectFinding> {
    match steins_catalog::effect_labels(name) {
        Some(labels) => {
            let line = tree.position(span.start).line;
            labels
                .iter()
                .map(|&label| EffectFinding {
                    label: label.to_owned(),
                    origin: name.to_owned(),
                    line,
                    path: path.to_owned(),
                })
                .collect()
        }
        None => Vec::new(),
    }
}

/// Resolve a method-call effect origin to the unit it edges to (project-wide).
fn resolve_effect_edge(
    cx: &Cx,
    enclosing: Option<&str>,
    receiver: &EffectRecv,
    method: &str,
) -> Option<Sym> {
    let (start, exact) = match receiver {
        EffectRecv::This | EffectRecv::SelfKw => (enclosing?.to_owned(), false),
        EffectRecv::Parent => (cx.parent_fqn(enclosing?)?, true),
        EffectRecv::ClassName(name) => (cx.class_fqn(name), true),
    };
    let Resolution::Found(r) = resolve_in_chain(cx, &start, method) else { return None };
    if r.method.visibility == Visibility::Private
        && !enclosing.is_some_and(|e| e.eq_ignore_ascii_case(&r.declaring_class.fqn))
    {
        return None;
    }
    if !exact {
        let declaring_final = r.declaring_class.is_final;
        if !(r.method.is_final || r.method.visibility == Visibility::Private || declaring_final) {
            return None;
        }
    }
    Some(Sym::Method(r.declaring_class.fqn.clone(), r.method.name.clone()))
}

// ---------------------------------------------------------------------------
// The project-aware analysis context.
// ---------------------------------------------------------------------------

/// Read-only analysis context: the whole project view plus the file currently
/// being analyzed. Cheap to copy (all borrows); descent rebuilds it at the
/// callee's file via [`Cx::at`].
#[derive(Clone, Copy)]
struct Cx<'a> {
    units: &'a [FileUnit<'a>],
    index: &'a Index,
    cur: usize,
}

impl<'a> Cx<'a> {
    fn new(units: &'a [FileUnit<'a>], index: &'a Index, cur: usize) -> Self {
        Self { units, index, cur }
    }

    /// A context pointing at a different file (for cross-file descent).
    fn at(&self, file: usize) -> Cx<'a> {
        Cx { units: self.units, index: self.index, cur: file }
    }

    fn tree(&self) -> &'a SourceTree {
        self.units[self.cur].tree
    }
    fn path(&self) -> &'a str {
        self.units[self.cur].path
    }
    fn strict(&self) -> bool {
        self.tree().has_strict_types()
    }

    fn fn_decl(&self, site: Site) -> &'a FunctionDecl {
        &self.units[site.file].tree.functions()[site.index]
    }
    fn class_decl(&self, site: Site) -> (usize, &'a ClassDecl) {
        (site.file, &self.units[site.file].tree.classes()[site.index])
    }

    /// Resolve a class reference (in the current file's context) to its FQN.
    fn class_fqn(&self, r: &NameRef) -> String {
        self.tree().resolve_class_fqn(r)
    }

    /// Find a class by FQN (case-insensitive), returning its file and decl.
    fn find_class(&self, fqn: &str) -> Option<(usize, &'a ClassDecl)> {
        match self.index.resolve_class(fqn) {
            Res::Unique(site) => Some(self.class_decl(site)),
            _ => None,
        }
    }

    /// The FQN of `class_fqn`'s parent, resolved in the parent's own file ctx.
    fn parent_fqn(&self, class_fqn: &str) -> Option<String> {
        let (file, cd) = self.find_class(class_fqn)?;
        let pref = cd.parent.as_ref()?;
        Some(self.units[file].tree.resolve_class_fqn(pref))
    }

    /// Resolve a **function** call reference per PHP name resolution (ADR-0001).
    fn resolve_function(&self, r: &NameRef) -> FnResolution {
        let catalog_knows = |n: &str| steins_catalog::effect_labels(n).is_some();
        match r.kind {
            RefKind::FullyQualified => {
                let fqn = r.raw.to_ascii_lowercase();
                match self.index.resolve_function(&fqn) {
                    Res::Unique(site) => FnResolution::User(site),
                    Res::Ambiguous => FnResolution::Unknown,
                    Res::Absent => {
                        // `\strlen` — a single-segment global name may be a builtin.
                        if !r.raw.contains('\\') && catalog_knows(&r.raw) {
                            FnResolution::Builtin
                        } else {
                            FnResolution::Unknown
                        }
                    }
                }
            }
            RefKind::Qualified => {
                // First segment via class/namespace imports, else current ns.
                let ctx = self.tree().ctx_at(r.offset);
                let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
                let first = &r.raw[..first_len];
                let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                    format!("{t}{}", &r.raw[first_len..])
                } else if ctx.namespace.is_empty() {
                    r.raw.clone()
                } else {
                    format!("{}\\{}", ctx.namespace, r.raw)
                };
                match self.index.resolve_function(&fqn) {
                    Res::Unique(site) => FnResolution::User(site),
                    _ => FnResolution::Unknown,
                }
            }
            RefKind::Unqualified => {
                let ctx = self.tree().ctx_at(r.offset);
                let name = r.raw.to_ascii_lowercase();
                // `use function` import wins outright.
                if let Some(t) = ctx.fn_imports.get(&name) {
                    return match self.index.resolve_function(&t.to_ascii_lowercase()) {
                        Res::Unique(site) => FnResolution::User(site),
                        _ => FnResolution::Unknown,
                    };
                }
                let is_builtin = catalog_knows(&name);
                // PHP tries NS\name first (when in a namespace).
                if !ctx.namespace.is_empty() {
                    let ns_fqn = format!("{}\\{}", ctx.namespace, name);
                    match self.index.resolve_function(&ns_fqn) {
                        Res::Unique(site) => return FnResolution::User(site),
                        Res::Ambiguous => return FnResolution::Unknown,
                        Res::Absent => {}
                    }
                }
                // Global fallback (also the whole story in the global namespace).
                match self.index.resolve_function(&name) {
                    Res::Ambiguous => FnResolution::Unknown,
                    Res::Unique(site) => {
                        // A user global that shadows a builtin name is ambiguous.
                        if is_builtin { FnResolution::Unknown } else { FnResolution::User(site) }
                    }
                    Res::Absent => {
                        if is_builtin { FnResolution::Builtin } else { FnResolution::Unknown }
                    }
                }
            }
        }
    }

    /// The site of a **user** function this call resolves to (positional-only),
    /// or `None` for builtins / unknown / dynamic / named-arg calls.
    fn resolve_user_fn(&self, call: &CallExpr) -> Option<Site> {
        if !call.positional_only {
            return None;
        }
        let r = call.callee_ref.as_ref()?;
        match self.resolve_function(r) {
            FnResolution::User(site) => Some(site),
            _ => None,
        }
    }

    /// The unique body scope of the user function at `site`, plus its file.
    fn fn_scope(&self, site: Site) -> Option<(usize, &'a Scope)> {
        let name = &self.fn_decl(site).name;
        let tree = self.units[site.file].tree;
        let mut it = tree.scopes().iter().filter(|s| s.function_name.as_deref() == Some(name));
        let scope = it.next()?;
        if it.next().is_some() { None } else { Some((site.file, scope)) }
    }

    /// The unique method body scope for `class_fqn::method` in `file`.
    fn method_scope(&self, file: usize, class_fqn: &str, method: &str) -> Option<&'a Scope> {
        let tree = self.units[file].tree;
        let mut it = tree.scopes().iter().filter(|s| {
            matches!(&s.owner, ScopeOwner::Method { class: c, method: m }
                if c.eq_ignore_ascii_case(class_fqn) && m.eq_ignore_ascii_case(method))
        });
        let scope = it.next()?;
        if it.next().is_some() { None } else { Some(scope) }
    }

    /// A display name for an effect [`Sym`] (`f`, `Foo::bar`), using the resolved
    /// declaration's written case where available.
    fn sym_display(&self, sym: &Sym) -> String {
        match sym {
            Sym::Func(fqn) => match self.index.resolve_function(fqn) {
                Res::Unique(site) => self.fn_decl(site).name.clone(),
                _ => fqn.clone(),
            },
            Sym::Method(cfqn, m) => match self.find_class(cfqn) {
                Some((_, cd)) => format!("{}::{}", cd.name, m),
                None => format!("{cfqn}::{m}"),
            },
        }
    }

    /// Resolve an [`ArgValue`] to a concrete literal, if provable.
    fn resolve_literal(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<ArgValue> {
        if poisoned {
            return None;
        }
        match value {
            v if v.is_literal() => Some(v.clone()),
            ArgValue::Var(name) => env.get(name).map(|k| k.value.clone()),
            ArgValue::Call(name, args) => {
                if args.is_empty()
                    && let Some((lit, _line)) = self.resolve_const_fn(name)
                {
                    return Some(lit);
                }
                self.try_fold(name, args, folder).map(|(lit, _prov)| lit)
            }
            // An array is proven iff every element value is proven (keys are fixed
            // at lowering). Folding is never applied to arrays (ADR-0001).
            ArgValue::Array(items) => {
                let mut resolved = Vec::with_capacity(items.len());
                for (k, v) in items {
                    let rv = self.resolve_literal(v, env, poisoned, folder)?;
                    resolved.push((k.clone(), rv));
                }
                Some(ArgValue::Array(resolved))
            }
            _ => None,
        }
    }

    /// Try to fold an allowlisted builtin call over literal arguments.
    fn try_fold(
        &self,
        name: &str,
        args: &[ArgValue],
        folder: &mut dyn Folder,
    ) -> Option<(ArgValue, String)> {
        // Any project user function sharing this simple name shadows the builtin
        // (or makes it ambiguous) — do not fold. Conservative, never an FP.
        if self.index.has_simple_function(name) {
            return None;
        }
        if !steins_catalog::foldable(name) {
            return None;
        }
        if !args.iter().all(ArgValue::is_literal) {
            return None;
        }
        let folded = folder.fold(name, args)?;
        Some((folded, format!("folded from {}", render_call(name, args))))
    }

    /// Resolve a zero-argument constant function anywhere in the project by its
    /// simple name: unique definition, no params, body exactly `return <lit>`,
    /// scope not poisoned. Returns the literal and the definition line.
    fn resolve_const_fn(&self, name: &str) -> Option<(ArgValue, u32)> {
        let site = self.index.unique_fn_by_simple(name)?;
        let decl = self.fn_decl(site);
        if !decl.params.is_empty() {
            return None;
        }
        let tree = self.units[site.file].tree;
        let mut scopes = tree.scopes().iter().filter(|s| s.function_name.as_deref() == Some(&decl.name));
        let scope = scopes.next()?;
        if scopes.next().is_some() || scope.poisoned {
            return None;
        }
        let [stmt] = scope.stmts.as_slice() else { return None };
        let StmtKind::Return { value, .. } = &stmt.kind else { return None };
        if !value.is_literal() {
            return None;
        }
        Some((value.clone(), tree.position(decl.span.start).line))
    }

    /// Build a `type.argument-mismatch` diagnostic (path/line from the current
    /// file — where the call textually is).
    fn diagnostic(
        &self,
        offset: u32,
        value: &ArgValue,
        provenance: Option<&str>,
        callee: &str,
        param_name: &str,
        ty: &NativeType,
    ) -> Diagnostic {
        let pos = self.tree().position(offset);
        let mode = if self.strict() { "strict" } else { "coercive" };
        let message = match provenance {
            Some(p) => format!(
                "argument {} ({}) to {}() cannot become {} ${} — proven TypeError ({} mode)",
                value.render(), p, callee, ty.render(), param_name, mode,
            ),
            None => format!(
                "argument {} to {}() cannot become {} ${} — proven TypeError ({} mode)",
                value.render(), callee, ty.render(), param_name, mode,
            ),
        };
        Diagnostic { id: ID, path: self.path().to_owned(), line: pos.line, column: pos.column, message }
    }

    /// Build a `type.return-mismatch` diagnostic. `display` is the owning
    /// function/method name (`f`, `Foo::bar`); `mode` is governed by the owning
    /// file's `declare(strict_types=1)` — the file this `Cx` points at.
    fn return_diagnostic(
        &self,
        offset: u32,
        value: &ArgValue,
        ret: &NativeType,
        display: &str,
    ) -> Diagnostic {
        let pos = self.tree().position(offset);
        let mode = if self.strict() { "strict" } else { "coercive" };
        let message = format!(
            "return {} cannot become {} (return type of {}()) — proven TypeError ({} mode)",
            value.render(),
            ret.render(),
            display,
            mode,
        );
        Diagnostic {
            id: RETURN_ID,
            path: self.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message,
        }
    }

    /// The native return type and display name of a scope's owning function or
    /// method (the same file this `Cx` points at), or `None` for the top-level
    /// script scope or an owner with no native scalar/union return type.
    fn scope_return(&self, scope: &Scope) -> Option<(&'a NativeType, String)> {
        match &scope.owner {
            ScopeOwner::TopLevel => None,
            ScopeOwner::Function(name) => {
                let f =
                    self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                f.ret.as_ref().map(|r| (r, f.name.clone()))
            }
            ScopeOwner::Method { class, method } => {
                // `owner.class` is the case-preserved FQN; `ClassDecl.fqn` is
                // lowercase-normalized — compare case-insensitively.
                let cd =
                    self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                m.ret.as_ref().map(|r| (r, format!("{}::{}", cd.name, m.name)))
            }
        }
    }

    /// The `@return` phpdoc envelope and display name of a scope's owning function
    /// or method (same file this `Cx` points at), or `None` when there is no
    /// docblock `@return` (or the scope is top-level).
    fn scope_return_phpdoc(&self, scope: &Scope) -> Option<(PType, String)> {
        match &scope.owner {
            ScopeOwner::TopLevel => None,
            ScopeOwner::Function(name) => {
                let f =
                    self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                let ret = parse_envelopes(f.docblock.as_deref())?.ret?;
                Some((ret, f.name.clone()))
            }
            ScopeOwner::Method { class, method } => {
                let cd =
                    self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                let ret = parse_envelopes(m.docblock.as_deref())?.ret?;
                Some((ret, format!("{}::{}", cd.name, m.name)))
            }
        }
    }
}

/// A proven local value plus where it was established (for provenance).
struct Known {
    value: ArgValue,
    line: u32,
    bound: Option<String>,
}

/// A binding-descent key: the callee (by FQN-ish key) plus its bound params.
type BindingKey = (String, Vec<(String, ArgValue)>);

/// The state threaded down an interprocedural binding descent (Feature B).
struct Descent<'a> {
    provenance: &'a str,
    depth: usize,
    stack: &'a mut Vec<BindingKey>,
    memo: &'a mut HashSet<BindingKey>,
}

/// Walk one scope's trace with a given initial environment.
#[allow(clippy::too_many_arguments)]
fn analyze_scope(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    mut env: HashMap<String, Known>,
    mut classes_env: HashMap<String, String>,
    this_exact: Option<String>,
    mut descent: Option<Descent<'_>>,
    mut facts: Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) {
    let enclosing_class = scope_class(scope);

    // The owning function/method's native return type, resolved once. Return-type
    // checking runs only in the plain per-scope pass (`descent.is_none()`), never
    // under an interprocedural binding descent: a descent rebinds the *callee's*
    // parameters, not its return, so re-checking there would only duplicate the
    // per-scope finding (and the file's own strict mode already governs it). The
    // returned value is resolved against the *file's own* `strict` via `cx`.
    let ret_info: Option<(&NativeType, String)> =
        if descent.is_none() { cx.scope_return(scope) } else { None };
    // The owning declaration's `@return` phpdoc envelope, resolved the same way
    // (plain per-scope pass only). Checked under contract acceptance, skipped where
    // the native return check already fired (no double-report).
    let ret_phpdoc: Option<(PType, String)> =
        if descent.is_none() { cx.scope_return_phpdoc(scope) } else { None };

    for stmt in &scope.stmts {
        // 1. Check + descend every statically-named call this statement carries.
        for call in checkable_calls(&stmt.kind) {
            match &call.receiver {
                Callee::Function(_) => {
                    check_propagated_call(cx, folder, scope.poisoned, call, &env, &classes_env, out);
                    try_descend_function(cx, folder, call, &env, scope.poisoned, descent.as_mut(), out);
                }
                Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
                    handle_method_call(
                        cx,
                        folder,
                        scope,
                        call,
                        &env,
                        &classes_env,
                        this_exact.as_deref(),
                        enclosing_class,
                        descent.as_mut(),
                        out,
                    );
                }
                Callee::Dynamic => {}
            }
        }

        // 1b. Return-type check: a trace-visible `return <value>;` whose value
        // resolves to a proven value (direct literal incl. arrays, env-known var,
        // folded call, const-fn, `New` exact-class) that provably fails the owner's
        // native return type (runtime relation) or its `@return` phpdoc envelope
        // (contract relation). Returns nested inside control flow live in `Opaque`
        // and never surface here — an accepted limitation (only top-of-trace
        // returns are checked).
        if let StmtKind::Return { value, span, .. } = &stmt.kind {
            let mut native_fired = false;
            if let Some((ret, display)) = &ret_info
                && let Some(lit) = cx.resolve_literal(value, &env, scope.poisoned, folder)
                && is_type_error(cx.strict(), ret, &lit)
            {
                out.push(cx.return_diagnostic(span.start, &lit, ret, display));
                native_fired = true;
            }
            if !native_fired
                && let Some((pret, display)) = &ret_phpdoc
                && let Some(cv) = cx.resolve_cval(value, &env, &classes_env, scope.poisoned, folder)
                && accepts(cx, cx.cur, span.start, pret, &cv) == Tri::No
            {
                let pos = cx.tree().position(span.start);
                out.push(Diagnostic {
                    id: RETURN_MISMATCH_ID,
                    path: cx.path().to_owned(),
                    line: pos.line,
                    column: pos.column,
                    message: format!(
                        "return value {} violates declared @return {pret} of {display}() — declared contract violation",
                        rendered_cval(&cv),
                    ),
                });
            }
        }

        // 2. Apply the statement's own effect on the known-value environment.
        match &stmt.kind {
            StmtKind::Barrier | StmtKind::Echo(_) => {
                env.clear();
                classes_env.clear();
            }
            // A control-flow construct forgets both what it may write AND what it
            // branches on (reads): a guard that early-returns on a variable's
            // value excludes that value from the fall-through path, so keeping the
            // binding would assert an unreachable path (soundness — see the
            // `StmtKind::Opaque` docs). Both sets drop from the literal env and the
            // exact-class env (an `instanceof`/`is null` guard filters class facts
            // the same way it filters scalar facts).
            StmtKind::Opaque { writes, reads, poisons } => {
                if *poisons {
                    env.clear();
                    classes_env.clear();
                } else {
                    for v in writes.iter().chain(reads) {
                        env.remove(v);
                        classes_env.remove(v);
                    }
                }
            }
            StmtKind::Return { .. } | StmtKind::Call(_) => {}
            StmtKind::Assign { var, value, span, .. } => {
                let line = cx.tree().position(span.start).line;
                match value {
                    ArgValue::New(class_ref, _) => {
                        env.remove(var);
                        if scope.poisoned {
                            classes_env.remove(var);
                        } else {
                            let class = cx.class_fqn(class_ref);
                            classes_env.insert(var.clone(), class.clone());
                            if let Some(facts) = facts.as_deref_mut() {
                                facts.push(LineFact {
                                    line,
                                    kind: FactKind::ExactClass { var: var.clone(), class },
                                });
                            }
                        }
                    }
                    _ => match cx.resolve_literal(value, &env, scope.poisoned, folder) {
                        Some(lit) => {
                            if let Some(facts) = facts.as_deref_mut() {
                                facts.push(LineFact {
                                    line,
                                    kind: FactKind::Value { var: var.clone(), rendered: lit.render() },
                                });
                            }
                            env.insert(var.clone(), Known { value: lit, line, bound: None });
                            classes_env.remove(var);
                        }
                        None => {
                            env.remove(var);
                            classes_env.remove(var);
                        }
                    },
                }
            }
        }

        // 3. After the statement, invalidate any variable handed to a call.
        for v in &stmt.invalidated {
            env.remove(v);
            classes_env.remove(v);
        }
    }
}

/// The class FQN that lexically owns a method scope; `None` for function/top.
fn scope_class(scope: &Scope) -> Option<&str> {
    match &scope.owner {
        ScopeOwner::Method { class, .. } => Some(class),
        ScopeOwner::TopLevel | ScopeOwner::Function(_) => None,
    }
}

/// The statically-named calls a statement carries.
fn checkable_calls(kind: &StmtKind) -> Vec<&CallExpr> {
    match kind {
        StmtKind::Call(c) => vec![c],
        StmtKind::Return { call: Some(c), .. } | StmtKind::Assign { call: Some(c), .. } => vec![c],
        StmtKind::Echo(cs) => cs.iter().collect(),
        _ => Vec::new(),
    }
}

/// Check a function call whose arguments may be propagated values (`Var`/`Call`/
/// array). Runs the native runtime check and the phpdoc declared-contract check;
/// a site where the native check fired is skipped by the phpdoc check.
fn check_propagated_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    classes_env: &HashMap<String, String>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(site) = cx.resolve_user_fn(call) else { return };
    let decl = cx.fn_decl(site);
    let envelopes = parse_envelopes(decl.docblock.as_deref());

    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = decl.params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }

        let mut native_fired = false;
        if let Some(ty) = param.ty.as_ref() {
            let resolved: Option<(ArgValue, String)> = match &arg.value {
                ArgValue::Var(name) if !poisoned => env.get(name).map(|k| {
                    let prov = match &k.bound {
                        Some(b) => format!("from ${name}, {b}"),
                        None => format!("from ${name}, assigned at line {}", k.line),
                    };
                    (k.value.clone(), prov)
                }),
                ArgValue::Call(name, args) => {
                    if args.is_empty() {
                        cx.resolve_const_fn(name)
                            .map(|(lit, line)| {
                                (lit, format!("from {name}(), defined at line {line}"))
                            })
                            .or_else(|| cx.try_fold(name, args, folder))
                    } else {
                        cx.try_fold(name, args, folder)
                    }
                }
                _ => None,
            };
            if let Some((value, provenance)) = resolved
                && is_type_error(cx.strict(), ty, &value)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &value,
                    Some(&provenance),
                    &decl.name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
        }

        // Only the propagation-carrier arg kinds (`$var`/`call()`) are the
        // propagation pass's to phpdoc-check; literal/array/`new` args are owned
        // by the direct pass (no double-report across the two passes).
        if !native_fired
            && matches!(arg.value, ArgValue::Var(_) | ArgValue::Call(..))
            && let Some(env_e) = &envelopes
        {
            check_phpdoc_param(
                cx,
                folder,
                env_e,
                param,
                site.file,
                decl.span.start,
                &decl.name,
                arg.span.start,
                &arg.value,
                env,
                classes_env,
                poisoned,
                out,
            );
        }
    }
}

/// Attempt an interprocedural binding descent into a same-project function.
fn try_descend_function(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(site) = cx.resolve_user_fn(call) else { return };
    let decl = cx.fn_decl(site);
    let Some((callee_file, callee_scope)) = cx.fn_scope(site) else { return };
    descend(
        cx,
        folder,
        &decl.params,
        callee_file,
        callee_scope,
        &decl.fqn,
        &decl.name,
        None,
        call,
        env,
        poisoned,
        descent,
        out,
    );
}

/// Interprocedural argument-binding descent into a resolved callee body.
#[allow(clippy::too_many_arguments)]
fn descend(
    cx: &Cx,
    folder: &mut dyn Folder,
    params: &[Param],
    callee_file: usize,
    callee_scope: &Scope,
    key_name: &str,
    display_name: &str,
    body_this_exact: Option<String>,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    if callee_scope.poisoned {
        return;
    }

    // Resolve each positional argument to a literal and try to bind it (using
    // the *caller's* env, strict mode, and folding).
    let mut bound: Vec<(String, ArgValue)> = Vec::new();
    let mut render_args: Vec<ArgValue> = Vec::new();
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = params.get(i) else { break };
        if param.variadic {
            break;
        }
        let Some(value) = cx.resolve_literal(&arg.value, env, poisoned, folder) else {
            continue;
        };
        render_args.push(value.clone());
        if param.by_ref {
            return;
        }
        let Some(ty) = param.ty.as_ref() else {
            bound.push((param.name.clone(), value));
            continue;
        };
        match coerce_into_param(cx.strict(), ty, &value) {
            Some(coerced) => bound.push((param.name.clone(), coerced)),
            None => return,
        }
    }

    if bound.is_empty() {
        return;
    }

    let mut key_binding = bound.clone();
    key_binding.sort_by(|a, b| a.0.cmp(&b.0));
    let key: BindingKey = (key_name.to_owned(), key_binding);

    // Provenance names the *first* binding site; a nested descent inherits it.
    // When the call crosses files, the site names the caller's file.
    let cross = cx.cur != callee_file;
    let new_provenance;
    let (provenance, next_depth): (&str, usize) = match &descent {
        Some(d) => (d.provenance, d.depth + 1),
        None => {
            let line = cx.tree().position(call.span.start).line;
            let render = render_call(display_name, &render_args);
            new_provenance = if cross {
                format!("bound at {render} call at {} line {line}", cx.path())
            } else {
                format!("bound at {render} call on line {line}")
            };
            (&new_provenance, 1)
        }
    };

    if next_depth > MAX_BINDING_DEPTH {
        return;
    }

    let bound_env: HashMap<String, Known> = bound
        .into_iter()
        .map(|(name, value)| (name, Known { value, line: 0, bound: Some(provenance.to_owned()) }))
        .collect();

    let child_cx = cx.at(callee_file);
    match descent {
        Some(d) => {
            if d.stack.contains(&key) || d.memo.contains(&key) {
                return;
            }
            d.stack.push(key.clone());
            let child = Descent { provenance, depth: next_depth, stack: d.stack, memo: d.memo };
            analyze_scope(
                &child_cx,
                folder,
                callee_scope,
                bound_env,
                HashMap::new(),
                body_this_exact,
                Some(child),
                None,
                out,
            );
            d.stack.pop();
            d.memo.insert(key);
        }
        None => {
            let mut stack: Vec<BindingKey> = vec![key.clone()];
            let mut memo: HashSet<BindingKey> = HashSet::new();
            let child = Descent { provenance, depth: next_depth, stack: &mut stack, memo: &mut memo };
            analyze_scope(
                &child_cx,
                folder,
                callee_scope,
                bound_env,
                HashMap::new(),
                body_this_exact,
                Some(child),
                None,
                out,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Class-world method resolution (ADR-0001 sound dispatch), project-wide.
// ---------------------------------------------------------------------------

/// A method resolved through a project inheritance chain.
struct ResolvedMethod<'a> {
    method: &'a MethodDecl,
    declaring_class: &'a ClassDecl,
    class_file: usize,
}

/// The outcome of walking a class's inheritance chain for a method.
enum Resolution<'a> {
    Found(ResolvedMethod<'a>),
    NotFoundChainComplete,
    Unknown,
}

/// Walk `start_fqn`'s project inheritance chain for a concrete `method`.
fn resolve_in_chain<'a>(cx: &Cx<'a>, start_fqn: &str, method: &str) -> Resolution<'a> {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Resolution::Unknown;
        }
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return Resolution::Unknown; // chain leaves the project
        };
        if cd.uses_traits {
            return Resolution::Unknown;
        }
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
            return if m.is_abstract {
                Resolution::Unknown
            } else {
                Resolution::Found(ResolvedMethod { method: m, declaring_class: cd, class_file: cfile })
            };
        }
        match &cd.parent {
            None => return Resolution::NotFoundChainComplete,
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
}

/// A resolved call target.
struct CallTarget<'a> {
    method: &'a MethodDecl,
    declaring_class: &'a ClassDecl,
    class_file: usize,
    this_exact: Option<String>,
}

/// Resolve a method/static/constructor `receiver` to a project target.
fn resolve_call_target<'a>(
    cx: &Cx<'a>,
    receiver: &Callee,
    classes_env: &HashMap<String, String>,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<CallTarget<'a>> {
    match receiver {
        Callee::Construct { class } => {
            let fqn = cx.class_fqn(class);
            resolve_exact(cx, &fqn, "__construct", enclosing_class, Some(fqn.clone()))
        }
        Callee::Method { receiver: Receiver::New(class), method } => {
            let fqn = cx.class_fqn(class);
            resolve_exact(cx, &fqn, method, enclosing_class, Some(fqn.clone()))
        }
        Callee::Method { receiver: Receiver::Var(v), method } => {
            if poisoned {
                return None;
            }
            let class = classes_env.get(v)?;
            resolve_exact(cx, class, method, enclosing_class, Some(class.clone()))
        }
        Callee::Method { receiver: Receiver::This, method } => {
            let enclosing = enclosing_class?;
            match this_exact {
                Some(exact) => resolve_exact(cx, exact, method, enclosing_class, Some(exact.to_owned())),
                None => resolve_guarded(cx, enclosing, method, enclosing_class),
            }
        }
        Callee::Static { class: StaticClass::SelfKw, method } => {
            let enclosing = enclosing_class?;
            resolve_guarded(cx, enclosing, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Parent, method } => {
            let parent = cx.parent_fqn(enclosing_class?)?;
            resolve_static_named(cx, &parent, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Named(name), method } => {
            let fqn = cx.class_fqn(name);
            resolve_static_named(cx, &fqn, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Static, .. } => None,
        Callee::Function(_) | Callee::Dynamic => None,
    }
}

/// Resolve an exact-receiver instance/constructor call (no override guard).
fn resolve_exact<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
    this_exact: Option<String>,
) -> Option<CallTarget<'a>> {
    match resolve_in_chain(cx, class, method) {
        Resolution::Found(r) if !private_blocked(&r, enclosing_class) => Some(CallTarget {
            method: r.method,
            declaring_class: r.declaring_class,
            class_file: r.class_file,
            this_exact,
        }),
        _ => None,
    }
}

/// Resolve a `$this->`/`self::` call under the override guard.
fn resolve_guarded<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    let declaring_final = r.declaring_class.is_final;
    let final_or_private =
        r.method.is_final || r.method.visibility == Visibility::Private || declaring_final;
    if !final_or_private {
        return None;
    }
    Some(CallTarget {
        method: r.method,
        declaring_class: r.declaring_class,
        class_file: r.class_file,
        this_exact: None,
    })
}

/// Resolve an explicit `Foo::m()` / `parent::m()` static call (exact).
fn resolve_static_named<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    if !r.method.is_static && enclosing_class.is_none() {
        return None;
    }
    Some(CallTarget {
        method: r.method,
        declaring_class: r.declaring_class,
        class_file: r.class_file,
        this_exact: None,
    })
}

/// Whether a resolved `private` method is invisible at the call site.
fn private_blocked(r: &ResolvedMethod, enclosing_class: Option<&str>) -> bool {
    r.method.visibility == Visibility::Private
        && !enclosing_class.is_some_and(|e| e.eq_ignore_ascii_case(&r.declaring_class.fqn))
}

/// Check + descend one method / static / constructor call.
#[allow(clippy::too_many_arguments)]
fn handle_method_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    classes_env: &HashMap<String, String>,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    if !call.positional_only {
        return;
    }
    let Some(target) =
        resolve_call_target(cx, &call.receiver, classes_env, this_exact, enclosing_class, scope.poisoned)
    else {
        return;
    };

    let callee_name = format!("{}::{}", target.declaring_class.name, target.method.name);
    check_method_args(
        cx,
        folder,
        target.method,
        target.class_file,
        &callee_name,
        call,
        env,
        classes_env,
        scope.poisoned,
        out,
    );

    let Some(callee_scope) =
        cx.method_scope(target.class_file, &target.declaring_class.fqn, &target.method.name)
    else {
        return;
    };
    let display = display_of_call(&call.receiver, &target.declaring_class.name, &target.method.name);
    descend(
        cx,
        folder,
        &target.method.params,
        target.class_file,
        callee_scope,
        &format!("{}::{}", target.declaring_class.fqn, target.method.name),
        &display,
        target.this_exact,
        call,
        env,
        scope.poisoned,
        descent,
        out,
    );
}

/// The provenance render base for a bound method/constructor call.
fn display_of_call(receiver: &Callee, declaring_class: &str, method: &str) -> String {
    match receiver {
        Callee::Construct { class } => format!("new {}", class.simple()),
        _ => format!("{declaring_class}::{method}"),
    }
}

/// Check the arguments of a resolved method/constructor call at its call site
/// (native runtime check plus the phpdoc declared-contract check; no double-report).
/// `class_file` locates the callee method's docblock context for class-name
/// resolution.
#[allow(clippy::too_many_arguments)]
fn check_method_args(
    cx: &Cx,
    folder: &mut dyn Folder,
    method: &MethodDecl,
    class_file: usize,
    callee_name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    classes_env: &HashMap<String, String>,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    let envelopes = parse_envelopes(method.docblock.as_deref());
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = method.params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }

        let mut native_fired = false;
        if let Some(ty) = param.ty.as_ref() {
            let resolved: Option<(ArgValue, Option<String>)> = match &arg.value {
                v if v.is_literal() => Some((v.clone(), None)),
                ArgValue::Var(name) if !poisoned => env.get(name).map(|k| {
                    let prov = match &k.bound {
                        Some(b) => format!("from ${name}, {b}"),
                        None => format!("from ${name}, assigned at line {}", k.line),
                    };
                    (k.value.clone(), Some(prov))
                }),
                ArgValue::Call(name, args) => {
                    if args.is_empty() {
                        cx.resolve_const_fn(name)
                            .map(|(lit, line)| {
                                (lit, Some(format!("from {name}(), defined at line {line}")))
                            })
                            .or_else(|| cx.try_fold(name, args, folder).map(|(l, p)| (l, Some(p))))
                    } else {
                        cx.try_fold(name, args, folder).map(|(l, p)| (l, Some(p)))
                    }
                }
                _ => None,
            };
            if let Some((value, prov)) = resolved
                && value.is_literal()
                && is_type_error(cx.strict(), ty, &value)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &value,
                    prov.as_deref(),
                    callee_name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
        }

        if !native_fired
            && let Some(env_e) = &envelopes
        {
            check_phpdoc_param(
                cx,
                folder,
                env_e,
                param,
                class_file,
                method.span.start,
                callee_name,
                arg.span.start,
                &arg.value,
                env,
                classes_env,
                poisoned,
                out,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Value / type helpers (unchanged from the per-file slice).
// ---------------------------------------------------------------------------

/// Render a call with its literal arguments for a folding provenance string.
fn render_call(name: &str, args: &[ArgValue]) -> String {
    let inner: Vec<String> = args.iter().map(ArgValue::render).collect();
    format!("{name}({})", inner.join(", "))
}

/// The generalized truth table: does passing (or returning) a **literal** `arg`
/// where a native scalar/union type `ty` is required provably raise a
/// `TypeError` under PHP 8.1+ (honoring `strict`)?
///
/// The table was settled **empirically against PHP 8.5.8** (the analyzer's floor
/// is 8.1, ADR-0011; these union-coercion rules have been stable since 8.0). The
/// reproduction snippets and outputs (union members, both modes):
///
/// ```text
/// COERCIVE (error iff the value coerces to NO member):
///   1.5   -> int|string  => OK  (becomes int 1; the string sink also accepts)
///   1.5   -> string|bool => OK  (becomes '1.5')
///   true  -> int|string  => OK  (becomes int 1)
///   "abc" -> int|float    => TypeError   (non-numeric string, no string sink)
///   "abc" -> int|false    => TypeError   (false-literal accepts only `false`)
///   "5"   -> int|float    => OK  (numeric string coerces)
///   false -> string|false => OK  (matches the `false` literal member exactly)
///   true  -> string|false => OK  (becomes '1' via the string member)
///   null  -> int|string   => TypeError   (non-nullable)
///   0/""/true -> false     => TypeError   (no coercion into a bool-literal)
/// STRICT (value must match SOME member; only int->float widening is implicit):
///   1.5   -> int|string  => TypeError   (float, no float member)
///   true  -> int|string  => TypeError   (bool, no bool/bool-literal member)
///   5     -> int|float    => OK  (int member; also OK via int->float widening)
///   false -> string|false => OK  (matches the `false` literal member)
///   true  -> string|false => TypeError   (`false` literal ≠ `true`; no bool)
///   5     -> string|false => TypeError   (int, no int member)
/// ```
///
/// Uncertain cells resolve to "not an error" (silence is always safe; ADR-0002).
fn is_type_error(strict: bool, ty: &NativeType, arg: &ArgValue) -> bool {
    match arg {
        // `null` is accepted iff the type is nullable (`?T` or a `null` member).
        ArgValue::Null => !ty.nullable,
        // A concrete non-null literal: an error iff no member accepts it.
        ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Str(_) | ArgValue::Bool(_) => {
            if strict {
                !ty.members.iter().any(|&m| member_accepts_strict(m, arg))
            } else {
                !ty.members.iter().any(|&m| member_accepts_coercive(m, arg))
            }
        }
        // An array is never a native scalar/union finding (arrays only ever fail
        // the phpdoc contract relation, checked separately).
        ArgValue::Array(_) => false,
        // Non-literal (`Var`/`Call`/`New`/`Other`): not provable → never an error.
        ArgValue::Var(_) | ArgValue::Call(..) | ArgValue::New(..) | ArgValue::Other => false,
    }
}

/// Strict mode: does a single union `member` accept the non-null literal `arg`
/// *exactly* (the only implicit conversion PHP allows in strict mode is
/// int→float, so a `float` member also accepts an `int` arg)?
fn member_accepts_strict(m: TypeMember, arg: &ArgValue) -> bool {
    match m {
        TypeMember::Scalar(ScalarType::Int) => matches!(arg, ArgValue::Int(_)),
        TypeMember::Scalar(ScalarType::Float) => matches!(arg, ArgValue::Int(_) | ArgValue::Float(_)),
        TypeMember::Scalar(ScalarType::String) => matches!(arg, ArgValue::Str(_)),
        TypeMember::Scalar(ScalarType::Bool) => matches!(arg, ArgValue::Bool(_)),
        TypeMember::BoolLiteral(b) => matches!(arg, ArgValue::Bool(v) if *v == b),
    }
}

/// Coercive mode: could the non-null literal `arg` be coerced into this single
/// union `member`? `string`/`bool` are universal sinks for scalars; numeric
/// members accept int/float/bool and numeric strings only; a bool-literal member
/// accepts **only** the exact matching bool value (no coercion into it).
fn member_accepts_coercive(m: TypeMember, arg: &ArgValue) -> bool {
    match m {
        // Any scalar coerces to `string` or to `bool`.
        TypeMember::Scalar(ScalarType::String) | TypeMember::Scalar(ScalarType::Bool) => true,
        // Numeric members accept numbers, bools, and numeric strings; a
        // non-numeric string is the only scalar that fails.
        TypeMember::Scalar(ScalarType::Int) | TypeMember::Scalar(ScalarType::Float) => match arg {
            ArgValue::Str(s) => php_is_numeric(s),
            ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Bool(_) => true,
            _ => false,
        },
        // No value coerces *into* a bool-literal; only the exact bool matches.
        TypeMember::BoolLiteral(b) => matches!(arg, ArgValue::Bool(v) if *v == b),
    }
}

/// The value a parameter of type `ty` holds when `value` is passed under
/// `strict`, or `None` when the pass fatals at entry or the coercion is
/// uncertain (silence is safe — ADR-0002).
///
/// For a single-scalar type the exact per-scalar coercion is reproduced (keeping
/// interprocedural binding precise). For a **union** the value is bound only when
/// it already matches a member's own type exactly — Steins does not guess which
/// member PHP would coerce a mismatched value into, so it stops the descent
/// (silent) rather than risk an unsound bound value.
fn coerce_into_param(strict: bool, ty: &NativeType, value: &ArgValue) -> Option<ArgValue> {
    if is_type_error(strict, ty, value) {
        return None;
    }
    if matches!(value, ArgValue::Null) {
        return Some(ArgValue::Null);
    }
    if let [TypeMember::Scalar(scalar)] = ty.members.as_slice() {
        return coerce_scalar(*scalar, value);
    }
    // Union: bind only on an exact-type member match; otherwise silence.
    if ty.members.iter().any(|&m| member_matches_exact(m, value)) {
        return Some(value.clone());
    }
    None
}

/// Whether a union `member` matches the *runtime type* of the non-null literal
/// `value` exactly (no coercion) — used to decide when a union binding is safe.
fn member_matches_exact(m: TypeMember, value: &ArgValue) -> bool {
    match (m, value) {
        (TypeMember::Scalar(ScalarType::Int), ArgValue::Int(_))
        | (TypeMember::Scalar(ScalarType::Float), ArgValue::Float(_))
        | (TypeMember::Scalar(ScalarType::String), ArgValue::Str(_))
        | (TypeMember::Scalar(ScalarType::Bool), ArgValue::Bool(_)) => true,
        (TypeMember::BoolLiteral(b), ArgValue::Bool(v)) => *v == b,
        _ => false,
    }
}

/// The value a single-scalar parameter holds after coercion (the per-scalar
/// PHP 8 coercion table), or `None` when the conversion is uncertain.
fn coerce_scalar(scalar: ScalarType, value: &ArgValue) -> Option<ArgValue> {
    Some(match (scalar, value) {
        (ScalarType::Int, ArgValue::Int(_))
        | (ScalarType::Float, ArgValue::Float(_))
        | (ScalarType::String, ArgValue::Str(_))
        | (ScalarType::Bool, ArgValue::Bool(_)) => value.clone(),

        (ScalarType::Float, ArgValue::Int(i)) => ArgValue::Float(*i as f64),

        (ScalarType::Int, ArgValue::Str(s)) => ArgValue::Int(php_str_to_int(s)?),
        (ScalarType::Float, ArgValue::Str(s)) => ArgValue::Float(php_str_to_float(s)?),
        (ScalarType::Int, ArgValue::Float(f)) => ArgValue::Int(php_float_to_int(*f)?),
        (ScalarType::Int, ArgValue::Bool(b)) => ArgValue::Int(i64::from(*b)),
        (ScalarType::Float, ArgValue::Bool(b)) => ArgValue::Float(if *b { 1.0 } else { 0.0 }),
        (ScalarType::Bool, ArgValue::Int(i)) => ArgValue::Bool(*i != 0),
        (ScalarType::Bool, ArgValue::Float(f)) => ArgValue::Bool(*f != 0.0),
        (ScalarType::Bool, ArgValue::Str(s)) => ArgValue::Bool(!(s.is_empty() || s == "0")),
        (ScalarType::String, ArgValue::Int(i)) => ArgValue::Str(i.to_string()),
        (ScalarType::String, ArgValue::Bool(b)) => {
            ArgValue::Str(if *b { "1".to_owned() } else { String::new() })
        }

        _ => return None,
    })
}

/// Whitespace PHP trims before interpreting a numeric string.
fn php_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'))
}

/// Convert a PHP numeric string to the int it coerces to.
fn php_str_to_int(s: &str) -> Option<i64> {
    let t = php_trim(s);
    if let Ok(i) = t.parse::<i64>() {
        return Some(i);
    }
    php_float_to_int(t.parse::<f64>().ok()?)
}

/// Convert a PHP numeric string to the float it coerces to.
fn php_str_to_float(s: &str) -> Option<f64> {
    php_trim(s).parse::<f64>().ok()
}

/// Truncate a float toward zero to an int (PHP scalar coercion).
fn php_float_to_int(f: f64) -> Option<i64> {
    f.is_finite().then(|| f.trunc() as i64)
}

/// PHP 8 `is_numeric` semantics.
fn php_is_numeric(s: &str) -> bool {
    let s = s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
    let bytes = s.as_bytes();
    let mut i = 0;

    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }

    let mut saw_digit = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        saw_digit = true;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return false;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let mut saw_exp = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_exp = true;
        }
        if !saw_exp {
            return false;
        }
    }

    i == bytes.len()
}

// ---------------------------------------------------------------------------
// PHPDoc declared-contract acceptance (ADR-0029/0030 relation #1).
//
// A separate acceptance relation from the runtime one above: **pure set
// semantics, NO coercion** (a numeric string `"5"` does NOT satisfy `int`). The
// judgment is trinary — `Yes`/`No`/`Maybe` — and only a definite `No` (proven
// non-membership) is ever reported; `Maybe` is silent (the zero-FP side).
// ---------------------------------------------------------------------------

/// The trinary contract-acceptance judgment (the Certainty discipline, ADR-0030).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tri {
    Yes,
    No,
    Maybe,
}

/// Intersection-style combine: `No` dominates, then `Maybe`, else `Yes`. Used
/// when *every* sub-obligation must hold (element/key membership, shape items).
fn combine(a: Tri, b: Tri) -> Tri {
    match (a, b) {
        (Tri::No, _) | (_, Tri::No) => Tri::No,
        (Tri::Maybe, _) | (_, Tri::Maybe) => Tri::Maybe,
        _ => Tri::Yes,
    }
}

/// A proven value in contract terms: a scalar literal, an array of proven values
/// (normalized keys), or an object of an exact class (a `New` fact).
enum CVal {
    Scalar(ArgValue),
    Array(Vec<(NormKey, CVal)>),
    Object(String),
}

/// The `@param`/`@return` phpdoc envelopes parsed off one declaration's docblock.
struct Envelopes {
    /// Parameter name (no `$`) → declared phpdoc type.
    params: Vec<(String, PType)>,
    ret: Option<PType>,
}

impl Envelopes {
    fn param(&self, name: &str) -> Option<&PType> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }
}

/// Parse the `@param`/`@return` envelopes from a raw docblock, or `None` when the
/// declaration carries no docblock or no envelope-bearing tag. A tag whose type
/// fails to parse (or carries an `Unsupported` node) contributes no envelope; the
/// other tags are unaffected (ADR-0029). `@var`/`@throws` are out of scope.
fn parse_envelopes(docblock: Option<&str>) -> Option<Envelopes> {
    let text = docblock?;
    // A `@phpstan-`/`@psalm-` prefixed tag overrides the plain one for the same
    // target (PHPStan precedence; ADR-0029). Track whether each recorded envelope
    // came from a prefixed tag so a later prefixed tag wins but a plain one never
    // displaces a prefixed one.
    let mut params: Vec<(String, PType)> = Vec::new();
    let mut param_prefixed: HashSet<String> = HashSet::new();
    let mut ret: Option<PType> = None;
    let mut ret_prefixed = false;
    for tag in scan_docblock(text) {
        match tag.kind {
            TagKind::Param => {
                let Some(var) = &tag.var_name else { continue };
                let name = var.trim_start_matches('$').to_owned();
                let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
                match params.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => {
                        // Replace only if we are not downgrading precedence.
                        if tag.prefixed || !param_prefixed.contains(&name) {
                            slot.1 = ty;
                        }
                    }
                    None => params.push((name.clone(), ty)),
                }
                if tag.prefixed {
                    param_prefixed.insert(name);
                }
            }
            TagKind::Return => {
                let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
                if tag.prefixed || (ret.is_none() && !ret_prefixed) {
                    ret = Some(ty);
                    ret_prefixed = tag.prefixed;
                }
            }
            TagKind::Var | TagKind::Throws => {}
        }
    }
    (!params.is_empty() || ret.is_some()).then_some(Envelopes { params, ret })
}

/// Parse one tag's type text into a phpdoc [`PType`], or `None` on a parse error
/// or an `Unsupported` node (no envelope — silence is safe).
fn parse_tag_type(text: &str) -> Option<PType> {
    let parsed = parse_type(text).ok()?;
    (!kind_has_unsupported(&parsed.ty.kind)).then_some(parsed.ty)
}

/// Whether a phpdoc type subtree contains an `Unsupported` node anywhere.
fn kind_has_unsupported(kind: &PKind) -> bool {
    match kind {
        PKind::Unsupported(_) => true,
        PKind::Nullable(t) | PKind::Array(t) => kind_has_unsupported(&t.kind),
        PKind::Union { types, .. } | PKind::Intersection(types) => {
            types.iter().any(|t| kind_has_unsupported(&t.kind))
        }
        PKind::Generic { args, .. } => args.iter().any(|a| kind_has_unsupported(&a.ty.kind)),
        PKind::OffsetAccess { base, offset } => {
            kind_has_unsupported(&base.kind) || kind_has_unsupported(&offset.kind)
        }
        PKind::ArrayShape(s) => s.items.iter().any(|i| kind_has_unsupported(&i.value.kind)),
        PKind::ObjectShape(items) => items.iter().any(|i| kind_has_unsupported(&i.value.kind)),
        _ => false,
    }
}

impl<'a> Cx<'a> {
    /// Resolve a call/return value to a proven [`CVal`] (scalars, arrays of proven
    /// values, or a `New` exact-class object), or `None` when not provable.
    fn resolve_cval(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        classes_env: &HashMap<String, String>,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<CVal> {
        match value {
            v if v.is_literal() => Some(CVal::Scalar(v.clone())),
            ArgValue::New(class_ref, _) if !poisoned => Some(CVal::Object(self.class_fqn(class_ref))),
            ArgValue::Array(items) => {
                let normalized = normalize_array(items);
                let mut out = Vec::with_capacity(normalized.len());
                for (k, v) in normalized {
                    out.push((k, self.resolve_cval(&v, env, classes_env, poisoned, folder)?));
                }
                Some(CVal::Array(out))
            }
            ArgValue::Var(name) if !poisoned => {
                if let Some(k) = env.get(name) {
                    self.resolve_cval(&k.value, env, classes_env, poisoned, folder)
                } else {
                    classes_env.get(name).map(|c| CVal::Object(c.clone()))
                }
            }
            ArgValue::Call(..) => {
                self.resolve_literal(value, env, poisoned, folder).map(CVal::Scalar)
            }
            _ => None,
        }
    }

    /// Resolve a phpdoc class name to its FQN in the callee file `cfile`'s context
    /// (offset `coff` picks the namespace/use scope where the docblock was written).
    fn resolve_pclass(&self, cfile: usize, coff: u32, name: &str) -> String {
        let raw = name.trim_start_matches('\\').to_owned();
        let kind = if name.starts_with('\\') {
            RefKind::FullyQualified
        } else if raw.contains('\\') {
            RefKind::Qualified
        } else {
            RefKind::Unqualified
        };
        self.units[cfile].tree.resolve_class_fqn(&NameRef { raw, kind, offset: coff })
    }

    /// Whether `obj_fqn`'s project inheritance chain reaches `target_fqn` (FQN
    /// equality or subclass; case-insensitive). An exact-name match succeeds even
    /// when the class is not in the project index; subclassing needs the chain.
    fn object_is_a(&self, obj_fqn: &str, target_fqn: &str) -> bool {
        let mut cur = obj_fqn.to_owned();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if cur.eq_ignore_ascii_case(target_fqn) {
                return true;
            }
            if !seen.insert(cur.to_ascii_lowercase()) {
                return false;
            }
            let Some((file, cd)) = self.find_class(&cur) else { return false };
            match &cd.parent {
                Some(pref) => cur = self.units[file].tree.resolve_class_fqn(pref),
                None => return false,
            }
        }
    }
}

/// Contract acceptance (ADR-0030): does the proven value `v` inhabit the phpdoc
/// type `ty`? Class names in `ty` resolve in the callee file `cfile` at `coff`.
fn accepts(cx: &Cx, cfile: usize, coff: u32, ty: &PType, v: &CVal) -> Tri {
    match &ty.kind {
        PKind::Identifier(name) => accepts_identifier(cx, cfile, coff, name, v),
        PKind::This => Tri::Maybe, // `$this` — silent this slice
        PKind::Nullable(inner) => match v {
            CVal::Scalar(ArgValue::Null) => Tri::Yes,
            _ => accepts(cx, cfile, coff, inner, v),
        },
        // Union: `Yes` if any member accepts, `No` only if all definitely reject.
        PKind::Union { types, .. } => {
            let (mut any_yes, mut any_maybe) = (false, false);
            for t in types {
                match accepts(cx, cfile, coff, t, v) {
                    Tri::Yes => any_yes = true,
                    Tri::Maybe => any_maybe = true,
                    Tri::No => {}
                }
            }
            if any_yes {
                Tri::Yes
            } else if any_maybe {
                Tri::Maybe
            } else {
                Tri::No
            }
        }
        PKind::Intersection(_) => Tri::Maybe, // class intersections — silent
        // `T[]` — an array (any keys) whose values inhabit `T`.
        PKind::Array(inner) => match v {
            CVal::Array(entries) => {
                let mut r = Tri::Yes;
                for (_, cv) in entries {
                    r = combine(r, accepts(cx, cfile, coff, inner, cv));
                    if r == Tri::No {
                        return Tri::No;
                    }
                }
                r
            }
            _ => Tri::No,
        },
        PKind::Generic { base, args } => accepts_generic(cx, cfile, coff, base, args, v),
        PKind::ArrayShape(shape) => accepts_shape(cx, cfile, coff, shape, v),
        PKind::Const(c) => accepts_const(c, v),
        // Callables, offset-access, conditionals, object-shapes → silent.
        PKind::Callable(_) | PKind::OffsetAccess { .. } | PKind::Conditional(_)
        | PKind::ObjectShape(_) | PKind::Unsupported(_) => Tri::Maybe,
    }
}

/// Acceptance for a bare identifier type: the scalar keyword table, the string/int
/// predicate refinements, `mixed`/`scalar`/`array-key`/`object`, or a class name.
fn accepts_identifier(cx: &Cx, cfile: usize, coff: u32, name: &str, v: &CVal) -> Tri {
    let scalar = match v {
        CVal::Scalar(s) => Some(s),
        _ => None,
    };
    let yes_no = |b: bool| if b { Tri::Yes } else { Tri::No };
    match name.to_ascii_lowercase().as_str() {
        "mixed" => Tri::Yes,
        "int" => yes_no(matches!(scalar, Some(ArgValue::Int(_)))),
        // `int` is accepted by `float` (PHPStan core semantics).
        "float" => yes_no(matches!(scalar, Some(ArgValue::Float(_) | ArgValue::Int(_)))),
        "string" => yes_no(matches!(scalar, Some(ArgValue::Str(_)))),
        "bool" => yes_no(matches!(scalar, Some(ArgValue::Bool(_)))),
        "true" => yes_no(matches!(scalar, Some(ArgValue::Bool(true)))),
        "false" => yes_no(matches!(scalar, Some(ArgValue::Bool(false)))),
        "null" => yes_no(matches!(scalar, Some(ArgValue::Null))),
        "scalar" => yes_no(matches!(
            scalar,
            Some(ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Str(_) | ArgValue::Bool(_))
        )),
        "array-key" => yes_no(matches!(scalar, Some(ArgValue::Int(_) | ArgValue::Str(_)))),
        "positive-int" => match scalar {
            Some(ArgValue::Int(i)) => yes_no(*i > 0),
            _ => Tri::No,
        },
        "negative-int" => match scalar {
            Some(ArgValue::Int(i)) => yes_no(*i < 0),
            _ => Tri::No,
        },
        "non-negative-int" => match scalar {
            Some(ArgValue::Int(i)) => yes_no(*i >= 0),
            _ => Tri::No,
        },
        "numeric-string" => match scalar {
            Some(ArgValue::Str(s)) => yes_no(php_is_numeric(s)),
            _ => Tri::No,
        },
        "non-empty-string" => match scalar {
            Some(ArgValue::Str(s)) => yes_no(!s.is_empty()),
            _ => Tri::No,
        },
        "non-falsy-string" | "truthy-string" => match scalar {
            Some(ArgValue::Str(s)) => yes_no(!s.is_empty() && s != "0"),
            _ => Tri::No,
        },
        "array" => yes_no(matches!(v, CVal::Array(_))),
        "object" => yes_no(matches!(v, CVal::Object(_))),
        // `iterable`: a proven array satisfies it; an object might be Traversable
        // (unprovable) → silent; a scalar is silent too (not in the checked set).
        "iterable" => match v {
            CVal::Array(_) => Tri::Yes,
            _ => Tri::Maybe,
        },
        // Types we deliberately keep silent this slice (class-string, self/static,
        // callable-string, void/never, …).
        "class-string" | "self" | "static" | "parent" | "void" | "never" | "callable-string"
        | "interface-string" | "trait-string" | "enum-string" | "literal-string"
        | "callable" | "closure" | "resource" | "empty" | "value-of" | "key-of" => Tri::Maybe,
        // A class-name type: only `New`-exact facts are checked (match or subclass);
        // any non-object value, or an unresolved class, stays silent.
        _ => match v {
            CVal::Object(obj) => {
                let target = cx.resolve_pclass(cfile, coff, name);
                if cx.object_is_a(obj, &target) { Tri::Yes } else { Tri::Maybe }
            }
            _ => Tri::Maybe,
        },
    }
}

/// Acceptance for a literal constant type (`'foo'`, `123`, `1.5`, `true`, …) by
/// value equality; a const-fetch (`Foo::BAR`) is unresolved → silent.
fn accepts_const(c: &ConstExpr, v: &CVal) -> Tri {
    let scalar = match v {
        CVal::Scalar(s) => s,
        _ => return Tri::No,
    };
    let yes_no = |b: bool| if b { Tri::Yes } else { Tri::No };
    match c {
        ConstExpr::Int(s) => match (s.parse::<i64>().ok(), scalar) {
            (Some(n), ArgValue::Int(i)) => yes_no(*i == n),
            _ => Tri::No,
        },
        ConstExpr::Float(s) => match (s.parse::<f64>().ok(), scalar) {
            (Some(n), ArgValue::Float(f)) => yes_no(*f == n),
            _ => Tri::No,
        },
        ConstExpr::Str(lit) => match scalar {
            ArgValue::Str(s) => yes_no(s == string_lit_value(lit)),
            _ => Tri::No,
        },
        ConstExpr::True => yes_no(matches!(scalar, ArgValue::Bool(true))),
        ConstExpr::False => yes_no(matches!(scalar, ArgValue::Bool(false))),
        ConstExpr::Null => yes_no(matches!(scalar, ArgValue::Null)),
        ConstExpr::Fetch { .. } => Tri::Maybe,
    }
}

fn string_lit_value(lit: &StringLit) -> &str {
    match lit {
        StringLit::Single(s) | StringLit::Double(s) => s,
    }
}

/// Acceptance for a generic type: `array<…>`/`list<…>`/`non-empty-*<…>` (per the
/// phpstan#14939 list semantics), simple `int<lo, hi>` ranges; everything else
/// (`Collection<…>`, `iterable<…>`, template generics) is silent.
fn accepts_generic(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    base: &str,
    args: &[steins_phpdoc::ast::GenericArg],
    v: &CVal,
) -> Tri {
    let base_lc = base.to_ascii_lowercase();
    match base_lc.as_str() {
        "array" | "non-empty-array" | "list" | "non-empty-list" => {
            let CVal::Array(entries) = v else { return Tri::No };
            let non_empty = base_lc.starts_with("non-empty");
            let require_list = base_lc.ends_with("list");
            // list<V> / non-empty-list<V>: 1 arg (value). array<V>: 1 arg (value);
            // array<K, V>: 2 args (key, value).
            let (key_ty, val_ty) = match (require_list, args) {
                (_, [v1]) => (None, &v1.ty),
                (false, [k, v2]) => (Some(&k.ty), &v2.ty),
                (true, [_, v2]) => (None, &v2.ty), // list<int, V> is unusual; ignore key
                _ => return Tri::Maybe,
            };
            check_arraylike(cx, cfile, coff, entries, key_ty, val_ty, require_list, non_empty)
        }
        // `int<lo, hi>` with simple integer/`min`/`max` bounds.
        "int" => match (args, v) {
            ([lo, hi], CVal::Scalar(ArgValue::Int(i))) => {
                match (int_bound(&lo.ty, i64::MIN), int_bound(&hi.ty, i64::MAX)) {
                    (Some(lo), Some(hi)) => {
                        if *i >= lo && *i <= hi { Tri::Yes } else { Tri::No }
                    }
                    _ => Tri::Maybe,
                }
            }
            ([_, _], CVal::Scalar(_)) => Tri::No, // non-int can't inhabit an int range
            _ => Tri::Maybe,
        },
        _ => Tri::Maybe,
    }
}

/// The bound of a simple `int<…>` argument: an integer literal, or `min`/`max`
/// keywords (mapped to `default`). Anything else → `None` (not a simple bound).
fn int_bound(ty: &PType, default: i64) -> Option<i64> {
    match &ty.kind {
        PKind::Const(ConstExpr::Int(s)) => s.parse::<i64>().ok(),
        PKind::Identifier(name) if name.eq_ignore_ascii_case("min") || name.eq_ignore_ascii_case("max") => {
            Some(default)
        }
        _ => None,
    }
}

/// Membership for an `array`/`list` generic (per phpstan#14939): a value is a list
/// iff its normalized keys are exactly `0..n-1` in order; element (and, for
/// `array<K, V>`, key) membership is checked recursively; an uncertain element
/// makes the whole check `Maybe` (silent).
#[allow(clippy::too_many_arguments)]
fn check_arraylike(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    entries: &[(NormKey, CVal)],
    key_ty: Option<&PType>,
    val_ty: &PType,
    require_list: bool,
    non_empty: bool,
) -> Tri {
    if non_empty && entries.is_empty() {
        return Tri::No;
    }
    if require_list && !is_list_shaped(entries) {
        return Tri::No;
    }
    let mut r = Tri::Yes;
    for (k, cv) in entries {
        if let Some(kt) = key_ty {
            r = combine(r, accepts(cx, cfile, coff, kt, &normkey_cval(k)));
            if r == Tri::No {
                return Tri::No;
            }
        }
        r = combine(r, accepts(cx, cfile, coff, val_ty, cv));
        if r == Tri::No {
            return Tri::No;
        }
    }
    r
}

/// Whether normalized `entries` form a list: keys exactly `0, 1, …, n-1` in order.
fn is_list_shaped(entries: &[(NormKey, CVal)]) -> bool {
    entries
        .iter()
        .enumerate()
        .all(|(i, (k, _))| matches!(k, NormKey::Int(n) if *n == i as i64))
}

/// A normalized key as a scalar [`CVal`] (for key membership).
fn normkey_cval(k: &NormKey) -> CVal {
    match k {
        NormKey::Int(i) => CVal::Scalar(ArgValue::Int(*i)),
        NormKey::Str(s) => CVal::Scalar(ArgValue::Str(s.clone())),
    }
}

/// Membership for an array-shape / list-shape (per phpstan#14939): `array{…}` is an
/// order-agnostic required-key map (optional `?` keys may be absent; sealed unless
/// `…`); `list{…}` is positional. A missing required key, a definite element-type
/// violation, or an extra key in a sealed shape → `No`. An unresolvable shape key
/// (a const-fetch) makes the whole check `Maybe`.
fn accepts_shape(cx: &Cx, cfile: usize, coff: u32, shape: &steins_phpdoc::ast::ArrayShape, v: &CVal) -> Tri {
    let CVal::Array(entries) = v else { return Tri::No };
    let non_empty =
        matches!(shape.kind, ArrayShapeKind::NonEmptyArray | ArrayShapeKind::NonEmptyList);
    if non_empty && entries.is_empty() {
        return Tri::No;
    }
    let require_list = matches!(shape.kind, ArrayShapeKind::List | ArrayShapeKind::NonEmptyList);
    if require_list && !is_list_shaped(entries) {
        return Tri::No;
    }

    // Assign each shape item its normalized key (positional next-int for keyless
    // items, explicit keys otherwise). An unresolvable key → the whole shape maybe.
    let mut expected: Vec<(NormKey, &PType, bool)> = Vec::with_capacity(shape.items.len());
    let mut next_auto: i64 = 0;
    for item in &shape.items {
        let key = match &item.key {
            None => {
                let k = NormKey::Int(next_auto);
                next_auto += 1;
                k
            }
            Some(sk) => {
                let Some(k) = shape_key_norm(sk) else { return Tri::Maybe };
                if let NormKey::Int(i) = k
                    && i >= next_auto
                {
                    next_auto = i + 1;
                }
                k
            }
        };
        expected.push((key, &item.value, item.optional));
    }

    let mut r = Tri::Yes;
    let mut used: HashSet<NormKey> = HashSet::new();
    for (k, ety, optional) in &expected {
        used.insert(k.clone());
        match entries.iter().find(|(ek, _)| ek == k) {
            Some((_, cv)) => {
                r = combine(r, accepts(cx, cfile, coff, ety, cv));
                if r == Tri::No {
                    return Tri::No;
                }
            }
            None => {
                if !optional {
                    return Tri::No; // missing required key
                }
            }
        }
    }

    // Extra keys: a sealed shape rejects them (PHPStan parity); an unsealed `…<V>`
    // checks their values against the tail type.
    if shape.sealed {
        if entries.iter().any(|(k, _)| !used.contains(k)) {
            return Tri::No;
        }
    } else if let Some(u) = &shape.unsealed {
        for (k, cv) in entries {
            if !used.contains(k) {
                r = combine(r, accepts(cx, cfile, coff, &u.value, cv));
                if r == Tri::No {
                    return Tri::No;
                }
            }
        }
    }
    r
}

/// The normalized runtime key a phpdoc shape key denotes, or `None` for an
/// unresolvable const-fetch key. Bareword and string keys fold integer-like
/// spellings to `Int` (PHP key normalization).
fn shape_key_norm(k: &ShapeKey) -> Option<NormKey> {
    match k {
        ShapeKey::Int(s) => s.parse::<i64>().ok().map(NormKey::Int),
        ShapeKey::Str(lit) => Some(norm_str_key(string_lit_value(lit))),
        ShapeKey::Ident(s) => Some(norm_str_key(s)),
        ShapeKey::ConstFetch { .. } => None,
    }
}

/// Fold an integer-like string key to `Int`, else keep it a `Str` key.
fn norm_str_key(s: &str) -> NormKey {
    match s.parse::<i64>() {
        Ok(i) if i.to_string() == s => NormKey::Int(i),
        _ => NormKey::Str(s.to_owned()),
    }
}

/// The phpdoc contract-acceptance check for one argument at a call site. Runs only
/// when the native check did **not** fire at this site (no double-report). Reports
/// `phpdoc.param-mismatch` iff the proven value provably does not inhabit the
/// `@param` type. `cfile`/`coff` locate the callee's docblock context (class-name
/// resolution). Returns nothing for `Maybe`/`Yes`.
#[allow(clippy::too_many_arguments)]
fn check_phpdoc_param(
    cx: &Cx,
    folder: &mut dyn Folder,
    envelopes: &Envelopes,
    param: &Param,
    cfile: usize,
    coff: u32,
    callee: &str,
    arg_offset: u32,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    classes_env: &HashMap<String, String>,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    let Some(ty) = envelopes.param(&param.name) else { return };
    let Some(cv) = cx.resolve_cval(value, env, classes_env, poisoned, folder) else { return };
    // A parameter that is nullable by its native type, or implicitly nullable via
    // a `= null` default, accepts `null` regardless of a non-nullable `@param`
    // spelling — PHP/PHPStan honor this, so reporting it would be a false positive.
    if matches!(cv, CVal::Scalar(ArgValue::Null))
        && (param.has_null_default || param.ty.as_ref().is_some_and(|t| t.nullable))
    {
        return;
    }
    if accepts(cx, cfile, coff, ty, &cv) != Tri::No {
        return;
    }
    let param_name = &param.name;
    let pos = cx.tree().position(arg_offset);
    let message = format!(
        "argument {} to {callee}() violates declared @param {ty} ${param_name} — declared contract violation",
        rendered_cval(&cv),
    );
    out.push(Diagnostic {
        id: PARAM_MISMATCH_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
    });
}

/// Render a proven [`CVal`] for a diagnostic message (delegates arrays/scalars to
/// [`ArgValue::render`]; objects show `new Class()`).
fn rendered_cval(v: &CVal) -> String {
    match v {
        CVal::Scalar(s) => s.render(),
        CVal::Object(class) => format!("new {}()", class.rsplit('\\').next().unwrap_or(class)),
        CVal::Array(entries) => {
            // Rebuild an `ArgValue::Array` with explicit keys so the shared compact
            // renderer applies (it re-normalizes; explicit keys round-trip).
            let items: Vec<(ArrayKey, ArgValue)> = entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect();
            ArgValue::Array(items).render()
        }
    }
}

/// A best-effort [`ArgValue`] reconstruction of a [`CVal`], for rendering only.
fn cval_to_argvalue(v: &CVal) -> ArgValue {
    match v {
        CVal::Scalar(s) => s.clone(),
        CVal::Object(_) => ArgValue::Other,
        CVal::Array(entries) => ArgValue::Array(
            entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect(),
        ),
    }
}
