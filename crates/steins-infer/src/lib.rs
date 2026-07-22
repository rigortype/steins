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

use std::collections::{HashMap, HashSet};

use steins_db::{Db, DeclSite, Project, ProjectIndex, Resolve, SourceFile, parse, project_index};
use steins_sidecar::{FoldArg, FoldResult, FoldValue, Sidecar};
use steins_syntax::CallExpr;
use steins_syntax::{
    ArgValue, Callee, ClassDecl, EffectEnvelope, EffectOrigin, EffectRecv, FunctionDecl, MethodDecl,
    NameRef, Param, ParamType, Receiver, RefKind, ScalarType, Scope, ScopeOwner, SourceTree,
    StaticClass, StmtKind, Visibility,
};

/// The registry id for the `type.argument-mismatch` proof-layer check (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

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
        ArgValue::Var(_) | ArgValue::Call(..) | ArgValue::New(..) | ArgValue::Other => None,
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

        // --- Direct pass: literal arguments at every function call site. ------
        for call in cx.tree().calls() {
            let Some(site) = cx.resolve_user_fn(call) else { continue };
            let decl = cx.fn_decl(site);
            for (i, arg) in call.args.iter().enumerate() {
                let Some(ty) = param_scalar_type(&decl.params, i) else {
                    if arg_binds_to_variadic(&decl.params, i) {
                        break;
                    }
                    continue;
                };
                if !arg.value.is_literal() {
                    continue;
                }
                if is_type_error(cx.strict(), ty, &arg.value) {
                    out.push(cx.diagnostic(
                        arg.span.start,
                        &arg.value,
                        None,
                        &decl.name,
                        &decl.params[i].name,
                        ty,
                    ));
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
        ty: ParamType,
    ) -> Diagnostic {
        let pos = self.tree().position(offset);
        let mode = if self.strict() { "strict" } else { "coercive" };
        let message = match provenance {
            Some(p) => format!(
                "argument {} ({}) to {}() cannot become {} ${} — proven TypeError ({} mode)",
                value.render(), p, callee, ty.scalar.keyword(), param_name, mode,
            ),
            None => format!(
                "argument {} to {}() cannot become {} ${} — proven TypeError ({} mode)",
                value.render(), callee, ty.scalar.keyword(), param_name, mode,
            ),
        };
        Diagnostic { id: ID, path: self.path().to_owned(), line: pos.line, column: pos.column, message }
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

    for stmt in &scope.stmts {
        // 1. Check + descend every statically-named call this statement carries.
        for call in checkable_calls(&stmt.kind) {
            match &call.receiver {
                Callee::Function(_) => {
                    check_propagated_call(cx, folder, scope.poisoned, call, &env, out);
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

        // 2. Apply the statement's own effect on the known-value environment.
        match &stmt.kind {
            StmtKind::Barrier | StmtKind::Echo(_) => {
                env.clear();
                classes_env.clear();
            }
            StmtKind::Opaque { writes, poisons } => {
                if *poisons {
                    env.clear();
                    classes_env.clear();
                } else {
                    for w in writes {
                        env.remove(w);
                        classes_env.remove(w);
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

/// Check a function call whose arguments may be propagated values (`Var`/`Call`).
fn check_propagated_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(site) = cx.resolve_user_fn(call) else { return };
    let decl = cx.fn_decl(site);

    for (i, arg) in call.args.iter().enumerate() {
        let Some(ty) = param_scalar_type(&decl.params, i) else {
            if arg_binds_to_variadic(&decl.params, i) {
                break;
            }
            continue;
        };

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
                        .map(|(lit, line)| (lit, format!("from {name}(), defined at line {line}")))
                        .or_else(|| cx.try_fold(name, args, folder))
                } else {
                    cx.try_fold(name, args, folder)
                }
            }
            _ => None,
        };
        let Some((value, provenance)) = resolved else { continue };

        if is_type_error(cx.strict(), ty, &value) {
            out.push(cx.diagnostic(
                arg.span.start,
                &value,
                Some(&provenance),
                &decl.name,
                &decl.params[i].name,
                ty,
            ));
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
        let Some(ty) = param.ty else {
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
    check_method_args(cx, folder, target.method, &callee_name, call, env, scope.poisoned, out);

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

/// Check the arguments of a resolved method/constructor call at its call site.
#[allow(clippy::too_many_arguments)]
fn check_method_args(
    cx: &Cx,
    folder: &mut dyn Folder,
    method: &MethodDecl,
    callee_name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    for (i, arg) in call.args.iter().enumerate() {
        let Some(ty) = param_scalar_type(&method.params, i) else {
            if arg_binds_to_variadic(&method.params, i) {
                break;
            }
            continue;
        };

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
                        .map(|(lit, line)| (lit, Some(format!("from {name}(), defined at line {line}"))))
                        .or_else(|| cx.try_fold(name, args, folder).map(|(l, p)| (l, Some(p))))
                } else {
                    cx.try_fold(name, args, folder).map(|(l, p)| (l, Some(p)))
                }
            }
            _ => None,
        };
        let Some((value, prov)) = resolved else { continue };
        if !value.is_literal() {
            continue;
        }
        if is_type_error(cx.strict(), ty, &value) {
            out.push(cx.diagnostic(
                arg.span.start,
                &value,
                prov.as_deref(),
                callee_name,
                &method.params[i].name,
                ty,
            ));
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

/// The simple scalar type of parameter `i`, or `None` when the argument should
/// be skipped (past the last declared param, variadic, by-ref, or untyped).
fn param_scalar_type(params: &[Param], i: usize) -> Option<ParamType> {
    let param = params.get(i)?;
    if param.variadic || param.by_ref {
        return None;
    }
    param.ty
}

/// Whether argument `i` binds to a variadic parameter.
fn arg_binds_to_variadic(params: &[Param], i: usize) -> bool {
    params.get(i).is_some_and(|p| p.variadic)
}

/// The truth table: does passing `arg` to a parameter of type `ty` provably
/// raise a `TypeError` under PHP 8.1+ (given `strict`)?
fn is_type_error(strict: bool, ty: ParamType, arg: &ArgValue) -> bool {
    if matches!(arg, ArgValue::Null) {
        return !ty.nullable;
    }

    if strict {
        match ty.scalar {
            ScalarType::Int => !matches!(arg, ArgValue::Int(_)),
            ScalarType::Float => !matches!(arg, ArgValue::Int(_) | ArgValue::Float(_)),
            ScalarType::String => !matches!(arg, ArgValue::Str(_)),
            ScalarType::Bool => !matches!(arg, ArgValue::Bool(_)),
        }
    } else {
        match ty.scalar {
            ScalarType::Int | ScalarType::Float => match arg {
                ArgValue::Str(s) => !php_is_numeric(s),
                _ => false,
            },
            ScalarType::String | ScalarType::Bool => false,
        }
    }
}

/// The value a parameter of type `ty` holds when `value` is passed under
/// `strict`, or `None` when the pass fatals at entry or the coercion is
/// uncertain (silence is safe — ADR-0002).
fn coerce_into_param(strict: bool, ty: ParamType, value: &ArgValue) -> Option<ArgValue> {
    if is_type_error(strict, ty, value) {
        return None;
    }
    if matches!(value, ArgValue::Null) {
        return Some(ArgValue::Null);
    }
    Some(match (ty.scalar, value) {
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
