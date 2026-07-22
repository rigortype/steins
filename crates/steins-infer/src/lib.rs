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
use steins_syntax::Span;
use steins_syntax::{
    ArgValue, ArrayKey, Callee, CatchClause, ClassDecl, CmpOp, CondExpr, CondOperand,
    EffectEnvelope, EffectOrigin, EffectRecv, FunctionDecl, MethodDecl, NameRef, NativeType,
    NormKey, Param, Receiver, RefKind, ScalarType, Scope, ScopeOwner, SourceTree, StaticClass,
    Stmt, StmtKind, ThrowKind, ThrowOrigin, TypeMember, Visibility, normalize_array,
};

use steins_phpdoc::ast::{ArrayShapeKind, ConstExpr, ShapeKey, StringLit, TypeKind as PKind};
use steins_phpdoc::{AssertKind, TagKind, Type as PType, parse_type, scan_docblock};

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

/// The registry id for the branch-sensitive null-dereference proof (ADR-0031
/// stage 1): a method call whose receiver variable is **proven `null`** on the
/// current path (e.g. inside `if ($u === null) { $u->name(); }`) — a guaranteed
/// runtime `Error` ("Call to a member function on null"). Only a *`Singleton(null)`*
/// receiver fires; a `OneOf` that merely *includes* null is `Maybe` → silent.
pub const CALL_ON_NULL_ID: &str = "call.on-null";

/// The registry id for the effect-envelope check (ADR-0005/0022): a function
/// declared `#[\Steins\Pure]` / `#[\Steins\Effect(...)]` whose inferred effects
/// exceed the declared envelope (ADR-0018 prefix subsumption).
pub const EFFECT_ID: &str = "effect.envelope-exceeded";

/// The one unified trinary judgment (ADR-0031), defined in `steins-domain`
/// and re-exported here: condition evaluation in the branch walk, phpdoc
/// contract acceptance (ADR-0030), and the domain's own fact queries all
/// speak the same `Certainty`.
pub use steins_domain::Certainty;

/// The registry id for the unknown-effect-label check (ADR-0018/0022): a declared
/// `#[\Steins\Effect(...)]` label that is not in the label registry
/// ([`steins_catalog::is_known_label`]) — a typo or an unregistered private label.
pub const UNKNOWN_LABEL_ID: &str = "effect.unknown-label";

/// The registry id for the `@throws` envelope check (ADR-0040/0007): a **checked**
/// exception that **provably escapes** (`Yes`) a function/method whose docblock
/// declares `@throws`, and is a subclass of **none** of the declared classes. Only
/// proven escapes report; `Maybe`-escape and unknown-hierarchy stay silent (the
/// consumer-inverted safe side of ADR-0040).
pub const THROW_UNDECLARED_ID: &str = "throw.undeclared";

/// The registry id for the Liskov throw-widening check (ADR-0033/0040 rule 4): an
/// override/implementation whose declared `@throws` names a checked class that is
/// a subclass of none of the parent method's declared `@throws` classes. Fires
/// only when **both** sides declare `@throws`; `Maybe` resolution stays silent.
pub const THROW_LISKOV_ID: &str = "throw.liskov-widened";

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
        | ArgValue::Ternary { .. }
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

        // --- Propagation pass FIRST: it walks every scope and, as a side
        // product, proves dead regions (decided branches, unreachable tails) —
        // the env-free direct pass below must not report inside them
        // (live-path discipline, ADR-0002/0031). Binding descents contribute
        // nothing here: their deadness is per-binding, not universal. ---------
        let mut dead_spans: Vec<Span> = Vec::new();
        for scope in cx.tree().scopes() {
            analyze_scope(
                &cx,
                folder,
                scope,
                HashMap::new(),
                HashMap::new(),
                None,
                None,
                None,
                Some(&mut dead_spans),
                &mut out,
            );
        }

        // --- Direct pass: literal / array / `new` arguments at every function
        // call site (env-free; propagation adds `$var`/folded resolution). Native
        // scalar checks and the phpdoc declared-contract check both run here; a
        // site where the native check fired is skipped by the phpdoc check (no
        // double-report; ADR-0030). Calls in proven-dead regions are skipped. ---
        let empty_env: HashMap<String, Known> = HashMap::new();
        let empty_classes: HashMap<String, String> = HashMap::new();
        for call in cx.tree().calls() {
            if in_dead(&dead_spans, call.span.start) {
                continue;
            }
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

    }

    // --- Effects pass (ADR-0005), computed once over the whole project. ------
    out.extend(effect_diagnostics(units, index));

    // --- Throw system (ADR-0040/0007): `@throws` envelope + Liskov. ----------
    out.extend(throw_diagnostics(units, index));

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
    /// The inferred throw set (ADR-0040): the classes a function/method can raise
    /// that escape it, with a shared `…?` taint marker when non-exhaustive.
    Throws { classes: Vec<String>, exhaustive: bool },
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
            FactKind::Throws { classes, exhaustive } => {
                let mut parts = classes.clone();
                if !*exhaustive {
                    parts.push("…?".to_owned());
                }
                format!("throws: {{{}}}", parts.join(", "))
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

    // 1. Effects (and throws) on each declaration line in the target file.
    for s in effect_summary_units(units, index, target) {
        let throws_present = !s.throws.is_empty() || !s.throws_exhaustive;
        facts.push(LineFact {
            line: s.line,
            kind: FactKind::Effects { labels: s.labels, exhaustive: s.exhaustive },
        });
        // Throws print on the same line, after effects, only when non-empty
        // (or tainted) — one color, one spelling (ADR-0006): throws are their
        // own margin fact, never an effect label.
        if throws_present {
            facts.push(LineFact {
                line: s.line,
                kind: FactKind::Throws { classes: s.throws, exhaustive: s.throws_exhaustive },
            });
        }
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
            None,
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
    /// The inferred escaping throw classes (ADR-0040), sorted; empty when none.
    pub throws: Vec<String>,
    /// Whether the throw set is exhaustive (no dynamic/unresolved taint).
    pub throws_exhaustive: bool,
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
    let throws = compute_throws(units, index);
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
    // Escaping throw classes (Yes or Maybe escape) as compact simple names.
    let throw_classes = |sym: &Sym| -> Vec<String> {
        let mut cs: Vec<String> = throws
            .get(sym)
            .into_iter()
            .flat_map(|t| t.facts.keys().map(|f| last_segment(&f.class).to_owned()))
            .collect();
        cs.sort();
        cs.dedup();
        cs
    };
    let throws_exhaustive = |sym: &Sym| throws.get(sym).is_none_or(|t| t.exhaustive);

    let mut out = Vec::new();
    for f in tree.functions() {
        let sym = Sym::Func(f.fqn.clone());
        out.push(EffectSummary {
            symbol: f.name.clone(),
            line: tree.position(f.span.start).line,
            labels: sorted_labels(&sym),
            exhaustive: exhaustive(&sym),
            throws: throw_classes(&sym),
            throws_exhaustive: throws_exhaustive(&sym),
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
                throws: throw_classes(&sym),
                throws_exhaustive: throws_exhaustive(&sym),
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
// Throw system (ADR-0040 damming / ADR-0007 checked accounting). Runs alongside
// the effect fixpoint over the same resolved call graph: `throws(f) = escaping
// own-throws(f) ∪ ⋃ filter(throws(callee), caller-guards)`, monotone to a
// fixpoint, with a throw-exhaustiveness bit tainted by dynamic/unresolved calls
// and opaque throws (mirroring effects).
// ---------------------------------------------------------------------------

/// One throw fact a unit can raise, with the provenance a `via` message needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThrowFact {
    /// The thrown class, resolved to an FQN in its origin file's context.
    class: String,
    /// Display for the throwing construct (`new RuntimeException`, `intdiv()`).
    origin: String,
    /// The file the origin lives in (for cross-file position/provenance).
    origin_file: usize,
    /// The origin construct's span start in `origin_file`.
    offset: u32,
    line: u32,
    path: String,
}

/// A unit's throw fixpoint result: the set of throws that **escape** it (each
/// with an escape [`Certainty`] — only `Yes`/`Maybe` are stored; `No`/absorbed
/// throws never enter), plus whether the set is exhaustive (ADR-0040).
#[derive(Debug, Clone, Default)]
struct ThrowSet {
    facts: HashMap<ThrowFact, Certainty>,
    exhaustive: bool,
}

/// `sub <: super` through the project inheritance chain **and** the builtin
/// exception table (ADR-0040), as a [`Certainty`]: `Yes` when the chain reaches
/// `super`; `No` when the chain is fully known (terminates at a project root or a
/// builtin root like `Throwable`) without reaching it; `Maybe` when the chain
/// leaves both the project and the builtin table (an unknown external class —
/// the FP-safe middle).
fn throw_subtype(cx: &Cx, sub_fqn: &str, sup_fqn: &str) -> Certainty {
    let sup = sup_fqn.trim_start_matches('\\');
    let mut cur = sub_fqn.trim_start_matches('\\').to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if cur.eq_ignore_ascii_case(sup) {
            return Certainty::Yes;
        }
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Certainty::Maybe; // cycle → give up
        }
        if let Some((file, cd)) = cx.find_class(&cur) {
            match &cd.parent {
                Some(pref) => cur = cx.units[file].tree.resolve_class_fqn(pref),
                None => return Certainty::No, // known project root, no match
            }
        } else if let Some(parent) = steins_catalog::builtin_exception_parent(&cur) {
            cur = parent.to_owned();
        } else if cur.eq_ignore_ascii_case("Throwable") {
            return Certainty::No; // known builtin root, no match
        } else {
            return Certainty::Maybe; // unknown external class — chain incomplete
        }
    }
}

/// Whether a thrown `class` is **checked** for envelope purposes (ADR-0007):
/// `No` (unchecked) for the `Error` / `LogicException` families, `Yes` when
/// provably neither, `Maybe` when the hierarchy is unknown (checked-but-Maybe —
/// envelope-silent, exhaustiveness-tainting).
fn throw_checked(cx: &Cx, class: &str) -> Certainty {
    let unchecked = throw_subtype(cx, class, "Error").or(throw_subtype(cx, class, "LogicException"));
    match unchecked {
        Certainty::Yes => Certainty::No,
        Certainty::No => Certainty::Yes,
        Certainty::Maybe => Certainty::Maybe,
    }
}

/// Whether one catch `clause` absorbs a thrown `sub` class (ADR-0040): `Yes` when
/// a caught member is provably a supertype; `Maybe` when a member might be (chain
/// leaves known territory) or the clause has an unnameable caught member; `No`
/// when no member can catch it.
fn clause_absorbs(cx: &Cx, sub: &str, clause: &CatchClause) -> Certainty {
    let mut r = if clause.has_unresolvable { Certainty::Maybe } else { Certainty::No };
    for cref in &clause.classes {
        let d = cx.class_fqn(cref);
        r = r.or(throw_subtype(cx, sub, &d));
        if r == Certainty::Yes {
            return Certainty::Yes;
        }
    }
    r
}

/// The escape [`Certainty`] of a `Yes`-arriving throw of `sub` past an ordered
/// (innermost-first) guard stack: `No` once a guard provably absorbs it; `Maybe`
/// if a guard might; else `Yes` (ADR-0040 damming, envelope-consumer side).
fn escape_through_guards(cx: &Cx, sub: &str, guards: &[Vec<CatchClause>]) -> Certainty {
    let mut maybe = false;
    for guard in guards {
        let mut absorb = Certainty::No;
        for clause in guard {
            absorb = absorb.or(clause_absorbs(cx, sub, clause));
            if absorb == Certainty::Yes {
                break;
            }
        }
        match absorb {
            Certainty::Yes => return Certainty::No,
            Certainty::Maybe => maybe = true,
            Certainty::No => {}
        }
    }
    if maybe { Certainty::Maybe } else { Certainty::Yes }
}

/// The unified throw fixpoint for every function/method in the project, keyed by
/// [`Sym`] (shared with the effect graph).
fn compute_throws(units: &[FileUnit], index: &Index) -> HashMap<Sym, ThrowSet> {
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        origins: &'a [ThrowOrigin],
    }
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        for f in u.tree.functions() {
            ulist.push(Unit { sym: Sym::Func(f.fqn.clone()), file: fi, class_fqn: None, origins: &f.throw_origins });
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                ulist.push(Unit {
                    sym: Sym::Method(c.fqn.clone(), m.name.clone()),
                    file: fi,
                    class_fqn: Some(c.fqn.clone()),
                    origins: &m.throw_origins,
                });
            }
        }
    }

    type Edge = (Sym, Vec<Vec<CatchClause>>);
    let mut direct: HashMap<Sym, HashMap<ThrowFact, Certainty>> = HashMap::new();
    let mut edges: HashMap<Sym, Vec<Edge>> = HashMap::new();
    let mut ex: HashMap<Sym, bool> = HashMap::new();
    let mut sym_file: HashMap<Sym, usize> = HashMap::new();

    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        sym_file.insert(unit.sym.clone(), unit.file);
        let d = direct.entry(unit.sym.clone()).or_default();
        let e = edges.entry(unit.sym.clone()).or_default();
        let x = ex.entry(unit.sym.clone()).or_insert(true);
        let add_fact = |class: String, origin: String, span: steins_syntax::Span, cert: Certainty, d: &mut HashMap<ThrowFact, Certainty>| {
            if cert == Certainty::No {
                return;
            }
            let line = cx.tree().position(span.start).line;
            let fact = ThrowFact {
                class,
                origin,
                origin_file: unit.file,
                offset: span.start,
                line,
                path: cx.path().to_owned(),
            };
            let slot = d.entry(fact).or_insert(Certainty::No);
            *slot = slot.or(cert);
        };
        for origin in unit.origins {
            match &origin.kind {
                ThrowKind::New(class) => {
                    let d_fqn = cx.class_fqn(class);
                    let esc = escape_through_guards(&cx, &d_fqn, &origin.guards);
                    let display = format!("new {}", last_segment(&d_fqn));
                    add_fact(d_fqn, display, origin.span, esc, d);
                }
                ThrowKind::Rethrow { caught, has_unresolvable } => {
                    for cref in caught {
                        let d_fqn = cx.class_fqn(cref);
                        let esc = escape_through_guards(&cx, &d_fqn, &origin.guards);
                        let display = format!("rethrow {}", last_segment(&d_fqn));
                        add_fact(d_fqn, display, origin.span, esc, d);
                    }
                    if *has_unresolvable {
                        *x = false;
                    }
                }
                ThrowKind::Call(name) => match cx.resolve_function(name) {
                    FnResolution::User(site) => {
                        e.push((Sym::Func(cx.fn_decl(site).fqn.clone()), origin.guards.clone()));
                    }
                    FnResolution::Builtin => {
                        if let Some(classes) = steins_catalog::builtin_throws(name.simple()) {
                            for c in classes {
                                let esc = escape_through_guards(&cx, c, &origin.guards);
                                add_fact((*c).to_owned(), format!("{}()", name.simple()), origin.span, esc, d);
                            }
                        }
                    }
                    FnResolution::Unknown => *x = false,
                },
                ThrowKind::MethodCall { receiver, method } => {
                    match resolve_effect_edge(&cx, unit.class_fqn.as_deref(), receiver, method) {
                        Some(callee) => e.push((callee, origin.guards.clone())),
                        None => *x = false,
                    }
                }
                ThrowKind::Taint => *x = false,
            }
        }
    }

    // Fixpoint: propagate callee throws through each call site's guards.
    let syms: Vec<Sym> = ulist.iter().map(|u| u.sym.clone()).collect();
    let mut facts = direct;
    loop {
        let mut changed = false;
        for sym in &syms {
            let file = sym_file[sym];
            let cx = Cx::new(units, index, file);
            let sym_edges: Vec<Edge> = edges.get(sym).cloned().unwrap_or_default();
            for (callee, guards) in &sym_edges {
                if ex.get(callee).copied() == Some(false) && ex.get(sym).copied() != Some(false) {
                    ex.insert(sym.clone(), false);
                    changed = true;
                }
                let callee_facts: Vec<(ThrowFact, Certainty)> =
                    facts.get(callee).into_iter().flatten().map(|(f, c)| (f.clone(), *c)).collect();
                for (fact, cert) in callee_facts {
                    let esc = escape_through_guards(&cx, &fact.class, guards);
                    let nc = cert.and(esc);
                    if nc == Certainty::No {
                        continue;
                    }
                    let slot = facts.entry(sym.clone()).or_default();
                    match slot.get(&fact).copied() {
                        Some(prev) => {
                            let merged = prev.or(nc);
                            if merged != prev {
                                slot.insert(fact, merged);
                                changed = true;
                            }
                        }
                        None => {
                            slot.insert(fact, nc);
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    syms.into_iter()
        .map(|s| {
            let f = facts.remove(&s).unwrap_or_default();
            let x = ex.get(&s).copied().unwrap_or(true);
            (s, ThrowSet { facts: f, exhaustive: x })
        })
        .collect()
}

/// The last `\`-segment of an FQN (for a compact throw display).
fn last_segment(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

/// The declared `@throws` class FQNs of one docblock, resolved in the file's
/// context at `offset` (ADR-0040 envelope opt-in). Accepts bare class names and
/// unions of them; anything else contributes nothing. Empty ⇒ no envelope.
fn declared_throws(cx: &Cx, offset: u32, docblock: Option<&str>) -> Vec<String> {
    let Some(text) = docblock else { return Vec::new() };
    let mut out = Vec::new();
    for tag in scan_docblock(text) {
        if tag.kind != TagKind::Throws {
            continue;
        }
        let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
        collect_class_names(&ty, &mut |name| {
            let fqn = resolve_class_name(cx, offset, name);
            if !out.contains(&fqn) {
                out.push(fqn);
            }
        });
    }
    out
}

/// Resolve a phpdoc class name to an FQN in the current file at `offset`.
fn resolve_class_name(cx: &Cx, offset: u32, name: &str) -> String {
    let raw = name.trim_start_matches('\\').to_owned();
    let kind = if name.starts_with('\\') {
        RefKind::FullyQualified
    } else if raw.contains('\\') {
        RefKind::Qualified
    } else {
        RefKind::Unqualified
    };
    cx.tree().resolve_class_fqn(&NameRef { raw, kind, offset })
}

/// Visit each plain class-name identifier in a phpdoc type that is a class name
/// or a union of class names; non-class members are ignored (no envelope).
fn collect_class_names(ty: &PType, f: &mut dyn FnMut(&str)) {
    match &ty.kind {
        PKind::Identifier(name) => f(name),
        PKind::Union { types, .. } => {
            for t in types {
                collect_class_names(t, f);
            }
        }
        PKind::Nullable(inner) => collect_class_names(inner, f),
        _ => {}
    }
}

/// The whole-project throw diagnostics: `throw.undeclared` envelope escapes and
/// `throw.liskov-widened` overrides (ADR-0040/0033).
fn throw_diagnostics(units: &[FileUnit], index: &Index) -> Vec<Diagnostic> {
    // Fast path: nothing to check without a `@throws` tag anywhere.
    let any_throws = units.iter().any(|u| {
        let has = |d: Option<&str>| d.is_some_and(|t| t.contains("@throws") || t.contains("throws"));
        u.tree.functions().iter().any(|f| f.docblock.as_deref().is_some_and(|t| t.contains("throws")))
            || u.tree.classes().iter().any(|c| {
                c.methods.iter().any(|m| has(m.docblock.as_deref()))
            })
    });
    if !any_throws {
        return Vec::new();
    }

    let throws = compute_throws(units, index);
    let mut out = Vec::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let declared = declared_throws(&cx, f.span.start, f.docblock.as_deref());
            if declared.is_empty() {
                continue;
            }
            let sym = Sym::Func(f.fqn.clone());
            emit_undeclared(&mut out, &cx, index, units, &sym, &f.name, &declared, &throws);
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                let declared = declared_throws(&cx, m.span.start, m.docblock.as_deref());
                let display = format!("{}::{}", c.name, m.name);
                if !declared.is_empty() {
                    let sym = Sym::Method(c.fqn.clone(), m.name.clone());
                    emit_undeclared(&mut out, &cx, index, units, &sym, &display, &declared, &throws);
                }
                // Liskov: an override/impl whose declared throws widen the parent's.
                emit_liskov(&mut out, &cx, c, m, &declared);
            }
        }
    }
    out
}

/// Emit `throw.undeclared` for each checked, proven-escaping throw of `sym` not
/// covered by its declared `@throws` set.
#[allow(clippy::too_many_arguments)]
fn emit_undeclared(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    index: &Index,
    units: &[FileUnit],
    sym: &Sym,
    display: &str,
    declared: &[String],
    throws: &HashMap<Sym, ThrowSet>,
) {
    let Some(set) = throws.get(sym) else { return };
    let declared_list = declared.iter().map(|d| last_segment(d).to_owned()).collect::<Vec<_>>().join("|");
    let mut facts: Vec<(&ThrowFact, Certainty)> = set.facts.iter().map(|(f, c)| (f, *c)).collect();
    facts.sort_by(|a, b| (a.0.origin_file, a.0.offset, &a.0.class).cmp(&(b.0.origin_file, b.0.offset, &b.0.class)));
    for (fact, cert) in facts {
        if cert != Certainty::Yes {
            continue; // Maybe-escape is silent (ADR-0040)
        }
        if throw_checked(cx, &fact.class) != Certainty::Yes {
            continue; // unchecked or unknown-hierarchy — never counts
        }
        // Covered iff a subclass of some declared class (Yes through chain).
        let covered = declared.iter().any(|d| throw_subtype(cx, &fact.class, d) != Certainty::No);
        if covered {
            continue; // Yes (covered) or Maybe (unproven) → silent
        }
        let ocx = Cx::new(units, index, fact.origin_file);
        let pos = ocx.tree().position(fact.offset);
        let simple = last_segment(&fact.class);
        let msg = format!(
            "{simple} can escape {display}() but is not declared (@throws {declared_list}) — proven escape"
        );
        out.push(Diagnostic {
            id: THROW_UNDECLARED_ID,
            path: fact.path.clone(),
            line: pos.line,
            column: pos.column,
            message: msg,
        });
    }
}

/// Emit `throw.liskov-widened` when a child method's declared `@throws` names a
/// checked class covered by none of the nearest ancestor method's declared
/// `@throws` (both sides must declare; `Maybe` resolution is silent).
fn emit_liskov(out: &mut Vec<Diagnostic>, cx: &Cx, class: &ClassDecl, m: &MethodDecl, child_declared: &[String]) {
    if child_declared.is_empty() {
        return;
    }
    let Some((parent_display, parent_declared)) = nearest_parent_throws(cx, class, &m.name) else {
        return;
    };
    if parent_declared.is_empty() {
        return;
    }
    let parent_list = parent_declared.iter().map(|d| last_segment(d)).collect::<Vec<_>>().join("|");
    for c in child_declared {
        // A child-declared class widens iff it is a subclass of NO parent class.
        let covered = parent_declared.iter().any(|p| throw_subtype(cx, c, p) != Certainty::No);
        if covered {
            continue;
        }
        let pos = cx.tree().position(m.span.start);
        let msg = format!(
            "{} is declared thrown by {}::{}() but {parent_display}::{}() (its abstraction) declares only @throws {parent_list} — Liskov widening",
            last_segment(c), class.name, m.name, m.name
        );
        out.push(Diagnostic {
            id: THROW_LISKOV_ID,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: msg,
        });
    }
}

/// The nearest ancestor class (walking `extends`) that declares a method named
/// `method` with a `@throws` docblock, returning its class name and declared set.
fn nearest_parent_throws(cx: &Cx, class: &ClassDecl, method: &str) -> Option<(String, Vec<String>)> {
    let mut cur = class.parent.as_ref().map(|p| cx.class_fqn(p))?;
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None;
        }
        let (file, cd) = cx.find_class(&cur)?;
        if let Some(pm) = cd.methods.iter().find(|pm| pm.name.eq_ignore_ascii_case(method)) {
            let pcx = Cx::new(cx.units, cx.index, file);
            let declared = declared_throws(&pcx, pm.span.start, pm.docblock.as_deref());
            if !declared.is_empty() {
                return Some((cd.name.clone(), declared));
            }
        }
        cur = cx.units[file].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
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
            ArgValue::Var(name) => env.get(name).and_then(Known::singleton),
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

    /// The parameter list of a scope's owning function or method (same file this
    /// `Cx` points at), or `None` for the top-level script scope. Used by the
    /// native-type parameter seeding (Feature B).
    fn scope_params(&self, scope: &Scope) -> Option<&'a [Param]> {
        match &scope.owner {
            ScopeOwner::TopLevel => None,
            ScopeOwner::Function(name) => {
                let f = self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                Some(&f.params)
            }
            ScopeOwner::Method { class, method } => {
                let cd = self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                Some(&m.params)
            }
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

// ---------------------------------------------------------------------------
// The env now stores the full four-layer `steins_domain::Fact` (ADR-0035) — the
// finished algebra lives in `steins-domain`; this crate only converts to/from
// the trace IR's `ArgValue` at the two seams below and calls the domain's joins,
// membership, and trinary queries. Stage-2 abstract facts (`Refined`/`General`)
// now flow through the env exactly like the finite layers: they resolve no
// *value* (only a `Singleton` does), but they carry knowledge for guard
// refinements (ADR-0031 stage 2), native-type seeding, and contract-on-fact
// acceptance (ADR-0030).
// ---------------------------------------------------------------------------

/// The domain value-fact (four layers), aliased as `Fact` throughout the walk.
use steins_domain::Fact;
use steins_domain::{Base, IntRange, Key as VKey, Refinement, StrPreds, Val};

/// The conversion seam **into** the domain: a literal (or fully-literal array)
/// [`ArgValue`] to a domain [`Val`]. Array keys carry PHP key-normalization in
/// insertion order (reusing [`normalize_array`], matching [`VKey`]). Any
/// non-literal element (or a non-literal `ArgValue`) yields `None` — the fact is
/// dropped (the safe side).
fn val_of(arg: &ArgValue) -> Option<Val> {
    match arg {
        ArgValue::Int(i) => Some(Val::Int(*i)),
        ArgValue::Float(f) => Some(Val::Float(*f)),
        ArgValue::Str(s) => Some(Val::Str(s.clone())),
        ArgValue::Bool(b) => Some(Val::Bool(*b)),
        ArgValue::Null => Some(Val::Null),
        ArgValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (k, v) in normalize_array(items) {
                let key = match k {
                    NormKey::Int(i) => VKey::Int(i),
                    NormKey::Str(s) => VKey::Str(s),
                };
                out.push((key, val_of(&v)?));
            }
            Some(Val::Array(out))
        }
        ArgValue::Var(_)
        | ArgValue::Call(..)
        | ArgValue::New(..)
        | ArgValue::Ternary { .. }
        | ArgValue::Other => None,
    }
}

/// The conversion seam **out of** the domain: a concrete [`Val`] back to the
/// trace IR's [`ArgValue`]. Total (the domain's `Val` is exactly the concrete
/// subset of `ArgValue`), so proven-value consumers (native truth table, folding
/// args, descent binding) keep receiving an `ArgValue` as before.
fn arg_of_val(v: &Val) -> ArgValue {
    match v {
        Val::Int(i) => ArgValue::Int(*i),
        Val::Float(f) => ArgValue::Float(*f),
        Val::Str(s) => ArgValue::Str(s.clone()),
        Val::Bool(b) => ArgValue::Bool(*b),
        Val::Null => ArgValue::Null,
        Val::Array(items) => ArgValue::Array(
            items
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        VKey::Int(i) => ArrayKey::Int(*i),
                        VKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, arg_of_val(v))
                })
                .collect(),
        ),
    }
}

/// Render a domain [`Val`] for a message/margin **byte-for-byte** identically to
/// the existing [`ArgValue::render`] (float `5.0` form, `['a', 'b']` arrays,
/// double-quoted scalars) — it simply routes through the shared renderer, so a
/// `Singleton` fact renders exactly as its `ArgValue` always did.
fn render_val(v: &Val) -> String {
    arg_of_val(v).render()
}

/// A domain `Singleton` fact from a literal/array [`ArgValue`], or `None` when
/// the value is not representable (a non-literal) — the fact is then dropped.
fn singleton_fact(arg: &ArgValue) -> Option<Fact> {
    val_of(arg).map(Fact::Singleton)
}

/// A proven local fact plus where it was established (for provenance).
#[derive(Clone)]
struct Known {
    fact: Fact,
    line: u32,
    bound: Option<String>,
}

impl Known {
    /// The single proven value, when the fact is a `Singleton` (converted back to
    /// the trace IR's [`ArgValue`]); `None` for every abstract or multi-valued
    /// layer — those resolve no proven value.
    fn singleton(&self) -> Option<ArgValue> {
        match &self.fact {
            Fact::Singleton(v) => Some(arg_of_val(v)),
            _ => None,
        }
    }
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
    dead_out: Option<&mut Vec<Span>>,
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

    // Native-type parameter seeding (Feature B): seed each parameter with the fact
    // its native type guarantees at runtime. This is sound in BOTH strict and
    // coercive modes: the engine coerces or throws at entry, so inside the body an
    // `int $x` param IS an int (post-coercion) — the seed is a *runtime-enforced*
    // fact, not a guess. A descent already binds actual values for the params the
    // caller supplied; we only seed params still absent from the env (unbound
    // params may still carry their native-type fact — sound).
    if let Some(params) = cx.scope_params(scope) {
        for p in params {
            if env.contains_key(&p.name) || classes_env.contains_key(&p.name) {
                continue;
            }
            if let Some(fact) = seed_fact(p) {
                env.insert(p.name.clone(), Known { fact, line: 0, bound: Some("native parameter type".to_owned()) });
            }
        }
    }

    let w = WalkCx {
        cx,
        scope,
        enclosing_class,
        this_exact: this_exact.as_deref(),
        ret_info: &ret_info,
        ret_phpdoc: &ret_phpdoc,
        dead: std::cell::RefCell::new(Vec::new()),
    };
    walk_trace(&w, folder, &scope.stmts, &mut env, &mut classes_env, &mut descent, &mut facts, out);
    if let Some(sink) = dead_out {
        sink.extend(w.dead.into_inner());
    }
}

/// Whether a walked (sub-)trace runs off its end (its successor is reachable) or
/// terminates (`return`/`throw`/`exit`, or an `if` where no branch falls through).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    FellThrough,
    Terminated,
}

/// The immutable context shared across a scope's recursive branch walk (ADR-0031).
struct WalkCx<'a, 'w> {
    cx: &'w Cx<'a>,
    scope: &'w Scope,
    enclosing_class: Option<&'w str>,
    this_exact: Option<&'w str>,
    ret_info: &'w Option<(&'a NativeType, String)>,
    ret_phpdoc: &'w Option<(PType, String)>,
    /// Proven-dead statement spans discovered during this walk (skipped decided
    /// branches, unreachable tails). Only the PLAIN per-scope walk's regions are
    /// universal truths — a binding descent's dead branches are dead *for that
    /// binding only*, so descents discard theirs (`dead_out: None`).
    dead: std::cell::RefCell<Vec<Span>>,
}

/// Record every top-level statement span of the given traces as proven dead.
/// Nested constructs' calls lie within their statement's span, so containment
/// filtering over these covers them. (Skipped `elseif` *conditions* are not
/// yet marked — a literal-arg call inside one is vanishingly rare; TODO.)
fn mark_dead(w: &WalkCx, traces: &[&[Stmt]]) {
    let mut dead = w.dead.borrow_mut();
    for trace in traces {
        for stmt in *trace {
            dead.push(stmt.span);
        }
    }
}

/// Whether a byte position falls inside any proven-dead region.
fn in_dead(dead: &[Span], pos: u32) -> bool {
    dead.iter().any(|s| s.start <= pos && pos < s.end)
}

/// Walk an ordered statement (sub-)trace against a mutable env, threading the same
/// findings sink, descent, and facts. Returns whether the trace falls through.
/// Statements after a terminator are unreachable and are **not** walked (ADR-0031
/// closes ADR-0027's dead-fallthrough gap).
#[allow(clippy::too_many_arguments)]
fn walk_trace(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmts: &[Stmt],
    env: &mut HashMap<String, Known>,
    classes_env: &mut HashMap<String, String>,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let cx = w.cx;
    let scope = w.scope;
    for (stmt_idx, stmt) in stmts.iter().enumerate() {
        // 1. Check + descend every statically-named call this statement carries.
        for call in checkable_calls(&stmt.kind) {
            match &call.receiver {
                Callee::Function(_) => {
                    check_propagated_call(cx, folder, scope.poisoned, call, env, classes_env, out);
                    try_descend_function(cx, folder, call, env, scope.poisoned, descent.as_mut(), out);
                }
                Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
                    // Branch-sensitive null-dereference proof (ADR-0031): a `$v->m()`
                    // whose receiver is proven `Singleton(null)` on this path.
                    check_call_on_null(w, call, env, out);
                    handle_method_call(
                        cx,
                        folder,
                        scope,
                        call,
                        env,
                        classes_env,
                        w.this_exact,
                        w.enclosing_class,
                        descent.as_mut(),
                        out,
                    );
                }
                Callee::Dynamic => {}
            }
        }

        // 1b. Return-type check (native + phpdoc contract); see the original notes.
        if let StmtKind::Return { value, span, .. } = &stmt.kind {
            let mut native_fired = false;
            if let Some((ret, display)) = w.ret_info
                && let Some(lit) = cx.resolve_literal(value, env, scope.poisoned, folder)
                && is_type_error(cx.strict(), ret, &lit)
            {
                out.push(cx.return_diagnostic(span.start, &lit, ret, display));
                native_fired = true;
            }
            if !native_fired
                && let Some((pret, display)) = w.ret_phpdoc
            {
                // Proven-value path, then the abstract-fact path (Feature E) —
                // same discipline as the `@param` check: only a definite `No`.
                let rendered = match cx.resolve_cval(value, env, classes_env, scope.poisoned, folder) {
                    Some(cv) => (accepts(cx, cx.cur, span.start, pret, &cv) == Certainty::No)
                        .then(|| rendered_cval(&cv)),
                    None => arg_abstract_fact(value, env, scope.poisoned).and_then(|fact| {
                        let cty = steins_contract::lower(pret);
                        (!contract_touches_class(&cty)
                            && steins_contract::admits_fact(&cty, fact) == Certainty::No)
                            .then(|| describe_fact(fact))
                    }),
                };
                if let Some(rendered) = rendered {
                    let pos = cx.tree().position(span.start);
                    out.push(Diagnostic {
                        id: RETURN_MISMATCH_ID,
                        path: cx.path().to_owned(),
                        line: pos.line,
                        column: pos.column,
                        message: format!(
                            "return value {rendered} violates declared @return {pret} of {display}() — declared contract violation",
                        ),
                    });
                }
            }
        }

        // 2. Apply the statement's own effect on the environment + compute its flow.
        let flow = match &stmt.kind {
            StmtKind::Barrier => {
                env.clear();
                classes_env.clear();
                Flow::FellThrough
            }
            // `echo` assigns nothing on its own; anything it *can* mutate (embedded
            // assignment / by-ref call) is in `invalidated` (step 3). Reading a
            // variable in an echo no longer forgets it (ADR-0031 precision payoff).
            StmtKind::Echo(_) => Flow::FellThrough,
            // A still-`Opaque` construct (loop / switch / try) forgets what it may
            // write AND what it branches on (reads) — unchanged from ADR-0027,
            // since the trace does not model its control flow.
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
                Flow::FellThrough
            }
            StmtKind::Call(_) => Flow::FellThrough,
            // Terminators: the trace stops; the remainder is unreachable.
            StmtKind::Return { .. } | StmtKind::Throw { .. } | StmtKind::Exit { .. } => {
                for v in &stmt.invalidated {
                    env.remove(v);
                    classes_env.remove(v);
                }
                return Flow::Terminated;
            }
            StmtKind::Assign { var, value, span, .. } => {
                apply_assign(w, folder, var, value, span.start, env, classes_env, facts);
                Flow::FellThrough
            }
            StmtKind::If { cond, then_trace, elseifs, else_trace } => walk_if(
                w, folder, cond, then_trace, elseifs, else_trace.as_deref(), env, classes_env,
                descent, facts, out,
            ),
        };

        // 3. Apply `@phpstan-assert` (Always) narrowings from every call in this
        // statement (Feature D), collecting the vars they establish. This runs
        // BEFORE the by-ref invalidation below so the replace-if-weaker decision
        // sees a proven `Singleton`/`OneOf` (kept over a weaker asserted fact); the
        // asserted vars are then protected from the conservative forget, since the
        // assertion helper's contract is a *stronger* statement than "the call may
        // have mutated this by reference".
        let mut asserted: HashSet<String> = HashSet::new();
        for call in checkable_calls(&stmt.kind) {
            apply_stmt_asserts(
                cx, scope, call, env, classes_env, w.this_exact, w.enclosing_class, &mut asserted,
            );
        }

        // 4. After the statement, invalidate any variable handed to a call — except
        // one an assertion just narrowed (its post-call fact is known).
        for v in &stmt.invalidated {
            if asserted.contains(v) {
                continue;
            }
            env.remove(v);
            classes_env.remove(v);
        }

        if flow == Flow::Terminated {
            // The rest of this trace is proven unreachable (ADR-0031).
            mark_dead(w, &[&stmts[stmt_idx + 1..]]);
            return Flow::Terminated;
        }
    }
    Flow::FellThrough
}

/// Apply a plain `$var = <value>;` assignment to the env (extracted from the walk).
#[allow(clippy::too_many_arguments)]
fn apply_assign(
    w: &WalkCx,
    folder: &mut dyn Folder,
    var: &str,
    value: &ArgValue,
    span_start: u32,
    env: &mut HashMap<String, Known>,
    classes_env: &mut HashMap<String, String>,
    facts: &mut Option<&mut Vec<LineFact>>,
) {
    let cx = w.cx;
    let line = cx.tree().position(span_start).line;

    // A ternary rvalue `$x = $c ? A : B` is a conditional value (ADR-0031): the
    // walk evaluates the guard and resolves to the chosen arm, or (undecided) a
    // `OneOf` of the two arms when both are literal, else unknown.
    if let ArgValue::Ternary { cond, then_val, else_val } = value {
        match eval_ternary_fact(w, folder, cond, then_val, else_val, env, classes_env) {
            Some(fact) => {
                if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
                    });
                }
                env.insert(var.to_owned(), Known { fact, line, bound: None });
                classes_env.remove(var);
            }
            None => {
                env.remove(var);
                classes_env.remove(var);
            }
        }
        return;
    }

    match value {
        ArgValue::New(class_ref, _) => {
            env.remove(var);
            if w.scope.poisoned {
                classes_env.remove(var);
            } else {
                let class = cx.class_fqn(class_ref);
                classes_env.insert(var.to_owned(), class.clone());
                if let Some(facts) = facts.as_deref_mut() {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::ExactClass { var: var.to_owned(), class },
                    });
                }
            }
        }
        _ => match cx.resolve_literal(value, env, w.scope.poisoned, folder).and_then(|lit| {
            singleton_fact(&lit).map(|f| (lit, f))
        }) {
            Some((lit, fact)) => {
                if let Some(facts) = facts.as_deref_mut() {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::Value { var: var.to_owned(), rendered: lit.render() },
                    });
                }
                env.insert(var.to_owned(), Known { fact, line, bound: None });
                classes_env.remove(var);
            }
            None => {
                env.remove(var);
                classes_env.remove(var);
            }
        },
    }
}

/// Walk a structured `if`/`elseif`/`else` (ADR-0031 stage 1). Evaluates the guard
/// to a [`Certainty`], walks each **live** branch on a cloned env (applying
/// positive refinement), then joins the envs of the branches that fall through.
/// When no live branch falls through, the code after the `if` is unreachable and
/// the whole construct terminates.
#[allow(clippy::too_many_arguments)]
fn walk_if(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then_trace: &[Stmt],
    elseifs: &[(CondExpr, Vec<Stmt>)],
    else_trace: Option<&[Stmt]>,
    env: &mut HashMap<String, Known>,
    classes_env: &mut HashMap<String, String>,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let poisoned = w.scope.poisoned;
    // 1. Evaluate the guard in the pre-branch env (short-circuit env refinement is
    // stage 2 — each condition sees the same entry env).
    let verdict = eval_cond(w, cond, env, classes_env, poisoned);

    // A decided guard proves the skipped side dead — record it so the env-free
    // direct pass never reports inside it (live-path discipline, ADR-0002/0031).
    match verdict {
        Certainty::Yes => {
            for (_, trace) in elseifs {
                mark_dead(w, &[trace.as_slice()]);
            }
            if let Some(trace) = else_trace {
                mark_dead(w, &[trace]);
            }
        }
        Certainty::No => mark_dead(w, &[then_trace]),
        Certainty::Maybe => {}
    }

    // 2. Variables the guard itself may mutate (opaque condition reads — e.g. a
    // by-ref call `if (parse($x))`) are forgotten on *every* resulting path.
    for v in cond_invalidations(cond) {
        env.remove(&v);
        classes_env.remove(&v);
    }

    // 3. Walk the live branches on cloned envs, collecting those that fall through.
    let mut fell: Vec<(HashMap<String, Known>, HashMap<String, String>)> = Vec::new();

    if verdict != Certainty::No {
        let mut benv = env.clone();
        let mut bclasses = classes_env.clone();
        apply_refinements(&then_refinements(cond), &mut benv, &mut bclasses);
        if walk_trace(w, folder, then_trace, &mut benv, &mut bclasses, descent, facts, out)
            == Flow::FellThrough
        {
            fell.push((benv, bclasses));
        }
    }

    if verdict != Certainty::Yes {
        let mut benv = env.clone();
        let mut bclasses = classes_env.clone();
        apply_refinements(&else_refinements(cond), &mut benv, &mut bclasses);
        if walk_else(w, folder, elseifs, else_trace, &mut benv, &mut bclasses, descent, facts, out)
            == Flow::FellThrough
        {
            fell.push((benv, bclasses));
        }
    }

    // 4. Merge. No live fall-through → the successor is unreachable.
    if fell.is_empty() {
        return Flow::Terminated;
    }
    let (jenv, jclasses) = join_envs(fell);
    *env = jenv;
    *classes_env = jclasses;
    Flow::FellThrough
}

/// Walk the `else` side of an `if`: the `elseif` chain desugars to a nested
/// `if`/`else`; the terminal `else` (if any) is a plain sub-trace; an absent
/// `else` falls through unchanged (the negated-guard path).
#[allow(clippy::too_many_arguments)]
fn walk_else(
    w: &WalkCx,
    folder: &mut dyn Folder,
    elseifs: &[(CondExpr, Vec<Stmt>)],
    else_trace: Option<&[Stmt]>,
    env: &mut HashMap<String, Known>,
    classes_env: &mut HashMap<String, String>,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    match elseifs.split_first() {
        Some(((cond, trace), rest)) => {
            walk_if(w, folder, cond, trace, rest, else_trace, env, classes_env, descent, facts, out)
        }
        None => match else_trace {
            Some(stmts) => walk_trace(w, folder, stmts, env, classes_env, descent, facts, out),
            None => Flow::FellThrough,
        },
    }
}

/// The branch-sensitive null-dereference proof (ADR-0031, `call.on-null`): a
/// non-null-safe `$v->m(...)` whose receiver `$v` is proven `Singleton(null)` on
/// the current path is a guaranteed runtime `Error`. A `OneOf` that merely
/// includes null stays `Maybe` (silent), and `?->` never fires.
fn check_call_on_null(
    w: &WalkCx,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    if w.scope.poisoned {
        return;
    }
    let Callee::Method { receiver: Receiver::Var(v), method, nullsafe: false } = &call.receiver
    else {
        return;
    };
    let Some(k) = env.get(v) else { return };
    if !matches!(&k.fact, Fact::Singleton(Val::Null)) {
        return;
    }
    let pos = w.cx.tree().position(call.span.start);
    out.push(Diagnostic {
        id: CALL_ON_NULL_ID,
        path: w.cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "method call ${v}->{method}() — ${v} is proven null on this path — proven Error (Call to a member function on null)"
        ),
    });
}

// ---------------------------------------------------------------------------
// Condition evaluation → `Certainty` (ADR-0031 stage 1).
// ---------------------------------------------------------------------------

/// Evaluate a lowered [`CondExpr`] against the env to a unified [`Certainty`].
fn eval_cond(
    w: &WalkCx,
    cond: &CondExpr,
    env: &HashMap<String, Known>,
    classes_env: &HashMap<String, String>,
    poisoned: bool,
) -> Certainty {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => {
            match (operand_values(lhs, env, poisoned), operand_values(rhs, env, poisoned)) {
                (Some(lv), Some(rv)) => eval_cmp(*op, &lv, &rv),
                _ => Certainty::Maybe,
            }
        }
        CondExpr::Truthy(op) => match operand_values(op, env, poisoned) {
            Some(vs) => all_agree(vs.iter().map(php_truthy)),
            None => Certainty::Maybe,
        },
        CondExpr::Instanceof { operand, class_ref } => {
            eval_instanceof(w, operand, class_ref, classes_env, poisoned)
        }
        CondExpr::Not(c) => eval_cond(w, c, env, classes_env, poisoned).not(),
        CondExpr::And(a, b) => eval_cond(w, a, env, classes_env, poisoned)
            .and(eval_cond(w, b, env, classes_env, poisoned)),
        CondExpr::Or(a, b) => eval_cond(w, a, env, classes_env, poisoned)
            .or(eval_cond(w, b, env, classes_env, poisoned)),
        CondExpr::Opaque { .. } => Certainty::Maybe,
    }
}

/// Evaluate a ternary rvalue to an env [`Fact`] (ADR-0031): a decided guard picks
/// the chosen arm's proven value; an undecided guard yields a `OneOf` of the two
/// arms when both resolve to literals, else `None` (unknown → the var is dropped).
fn eval_ternary_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then_val: &ArgValue,
    else_val: &ArgValue,
    env: &HashMap<String, Known>,
    classes_env: &HashMap<String, String>,
) -> Option<Fact> {
    let poisoned = w.scope.poisoned;
    let mut resolve = |v: &ArgValue| w.cx.resolve_literal(v, env, poisoned, folder);
    match eval_cond(w, cond, env, classes_env, poisoned) {
        Certainty::Yes => resolve(then_val).and_then(|a| singleton_fact(&a)),
        Certainty::No => resolve(else_val).and_then(|a| singleton_fact(&a)),
        Certainty::Maybe => {
            // Undecided guard: the value is one of the two arms. `Fact::from_vals`
            // gives the canonical finite form (a `Singleton` when the arms are
            // equal, else a `OneOf`), or `None` (dropped) when an arm is not
            // representable.
            let t = val_of(&resolve(then_val)?)?;
            let e = val_of(&resolve(else_val)?)?;
            Fact::from_vals(vec![t, e])
        }
    }
}

/// The candidate values of a condition operand: the fact's value set for a known
/// variable, the literal itself, else `None` (unknown → the caller yields `Maybe`).
fn operand_values(
    op: &CondOperand,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<Vec<ArgValue>> {
    match op {
        CondOperand::Literal(v) => Some(vec![v.clone()]),
        // Only the finite layers (`Singleton`/`OneOf`) offer concrete candidate
        // values for a comparison; an abstract fact has none → `None` → `Maybe`
        // (the sound side). Condition evaluation over `finite_members()`.
        CondOperand::Var(name) if !poisoned => {
            env.get(name).and_then(|k| k.fact.finite_members().map(|vs| vs.iter().map(arg_of_val).collect()))
        }
        _ => None,
    }
}

/// Evaluate a comparison over two candidate value sets (ADR-0031 OneOf rule: all
/// member pairs agree → that verdict; any disagreement or undecidable pair → Maybe).
fn eval_cmp(op: CmpOp, lhs: &[ArgValue], rhs: &[ArgValue]) -> Certainty {
    let mut acc: Option<bool> = None;
    for l in lhs {
        for r in rhs {
            let b = match op {
                CmpOp::Identical => php_identical(l, r),
                CmpOp::NotIdentical => php_identical(l, r).map(|x| !x),
                CmpOp::Loose => php_loose_eq(l, r),
                CmpOp::NotLoose => php_loose_eq(l, r).map(|x| !x),
                // Ordering: decide only for concrete numeric operands (PHP numeric
                // ordering); any other pairing is undecidable here → `Maybe`. The
                // refinement machinery consumes these guards regardless of verdict.
                CmpOp::Lt => php_num_order(l, r).map(|o| o == std::cmp::Ordering::Less),
                CmpOp::Le => php_num_order(l, r).map(|o| o != std::cmp::Ordering::Greater),
                CmpOp::Gt => php_num_order(l, r).map(|o| o == std::cmp::Ordering::Greater),
                CmpOp::Ge => php_num_order(l, r).map(|o| o != std::cmp::Ordering::Less),
            };
            match b {
                None => return Certainty::Maybe,
                Some(v) => match acc {
                    None => acc = Some(v),
                    Some(prev) if prev != v => return Certainty::Maybe,
                    _ => {}
                },
            }
        }
    }
    Certainty::from_opt(acc)
}

/// PHP numeric ordering of two concrete operands, decided only when **both** are
/// `int`/`float` (comparing as f64); any other pairing (strings, bools, null,
/// arrays) is `None` — undecidable here, so the guard verdict is `Maybe` (sound).
fn php_num_order(a: &ArgValue, b: &ArgValue) -> Option<std::cmp::Ordering> {
    let num = |v: &ArgValue| match v {
        #[allow(clippy::cast_precision_loss)]
        ArgValue::Int(i) => Some(*i as f64),
        ArgValue::Float(f) => Some(*f),
        _ => None,
    };
    let (x, y) = (num(a)?, num(b)?);
    x.partial_cmp(&y)
}

/// Fold a sequence of per-member truth verdicts (`None` = undecidable) into one
/// [`Certainty`]: all-agree → that pole, else `Maybe`.
fn all_agree(iter: impl Iterator<Item = Option<bool>>) -> Certainty {
    let mut acc: Option<bool> = None;
    for b in iter {
        match b {
            None => return Certainty::Maybe,
            Some(v) => match acc {
                None => acc = Some(v),
                Some(prev) if prev != v => return Certainty::Maybe,
                _ => {}
            },
        }
    }
    Certainty::from_opt(acc)
}

/// `operand instanceof Class`: `Yes` only when the operand's proven exact class
/// is-a the target through the project chain; a non-object literal is `No`;
/// everything else (unknown class, chain leaving the project) is `Maybe`.
fn eval_instanceof(
    w: &WalkCx,
    operand: &CondOperand,
    class_ref: &NameRef,
    classes_env: &HashMap<String, String>,
    poisoned: bool,
) -> Certainty {
    match operand {
        CondOperand::Var(name) if !poisoned => match classes_env.get(name) {
            Some(obj_fqn) => {
                let target = w.cx.class_fqn(class_ref);
                // `object_is_a` returns false both for "provably not" and for a
                // chain that leaves the project, so only a positive is a definite
                // verdict; a negative stays `Maybe` (the FP-safe side).
                if w.cx.object_is_a(obj_fqn, &target) {
                    Certainty::Yes
                } else {
                    Certainty::Maybe
                }
            }
            None => Certainty::Maybe,
        },
        // A concrete non-object literal (`null`, `5`, `"x"`, …) is never an
        // instance of a class.
        CondOperand::Literal(v) if v.is_literal() => Certainty::No,
        _ => Certainty::Maybe,
    }
}

// ---------------------------------------------------------------------------
// Guard refinement (ADR-0031 stage 1 → stage 2 negative facts). A guard narrows
// a variable's fact on the branch where it holds. Stage 1's positive `$x === v`
// binds a Singleton; stage 2 adds the *negative* facts (ADR-0031): `!== null`
// clears nullability, `!== v` removes a member (or, for `!== ''`, adds
// NON_EMPTY), ordering guards intersect an int interval, and truthiness adds
// NON_FALSY / clears null. Instanceof binds nothing — membership is not
// exactness (a subclass instance would make an exact-class fact WRONG).
//
// A refinement that would empty a fact (e.g. an int-range intersection with no
// overlap, reachable only across an `&&` of contradictory guards) drops the
// var's fact rather than signalling branch-death: the decided-guard verdict
// already prunes truly-dead branches up front, so dropping-to-no-fact is the
// sound, simpler fallback here (documented choice).
// ---------------------------------------------------------------------------

/// The fact a parameter's **native** type guarantees at runtime (Feature B), or
/// `None` when nothing representable is seeded this slice. Only a **single scalar**
/// type (optionally nullable) seeds — a `General{base, nullable}` fact; unions and
/// bool-literal members (`string|false`) have no clean single-`Fact` form and are
/// skipped (documented). By-ref params are never seeded (the caller may hold
/// anything and the var can be rebound); variadic params are skipped.
fn seed_fact(p: &Param) -> Option<Fact> {
    if p.by_ref || p.variadic {
        return None;
    }
    let ty = p.ty.as_ref()?;
    let [TypeMember::Scalar(scalar)] = ty.members.as_slice() else { return None };
    let base = match scalar {
        ScalarType::Int => Base::Int,
        ScalarType::Float => Base::Float,
        ScalarType::String => Base::String,
        ScalarType::Bool => Base::Bool,
    };
    // A `= null` default makes even a non-`?T` param implicitly nullable.
    let nullable = ty.nullable || p.has_null_default;
    Some(Fact::General { base, nullable })
}

/// One narrowing a guard establishes for a variable on a given branch.
enum Refine {
    /// `$x === v` (then) — narrow to exactly this value.
    Exact(String, Val),
    /// `$x !== null` (then) / `$x === null` (else) — drop nullability: clear the
    /// abstract `nullable` flag, or remove the `null` member of a finite fact.
    NotNull(String),
    /// `$x !== v` (non-null, then) — remove `v` from a finite fact; for a
    /// String-based abstract fact and `v == ""`, add `NON_EMPTY` instead.
    Exclude(String, Val),
    /// `$x > k` &c. — intersect an Int-based abstract fact with this interval.
    IntRange(String, IntRange),
    /// `if ($x)` (then) — truthiness: clear nullability and, for a String-based
    /// fact, add `NON_FALSY` (a truthy string is neither `""` nor `"0"`).
    Truthy(String),
}

/// The refinements that hold when `cond` is TRUE (the then-branch).
fn then_refinements(cond: &CondExpr) -> Vec<Refine> {
    let mut out = Vec::new();
    collect_refine(cond, true, &mut out);
    out
}

/// The refinements that hold when `cond` is FALSE (the else-branch).
fn else_refinements(cond: &CondExpr) -> Vec<Refine> {
    let mut out = Vec::new();
    collect_refine(cond, false, &mut out);
    out
}

/// Collect the refinements a condition implies on the given polarity (`then` =
/// true-path, `!then` = false-path). Negation flips polarity; `&&` distributes on
/// the true-path, `||` on the false-path (De Morgan).
fn collect_refine(cond: &CondExpr, then: bool, out: &mut Vec<Refine>) {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => collect_cmp_refine(*op, lhs, rhs, then, out),
        CondExpr::Truthy(op) => {
            // Only the true-path of a bare truthiness test refines (the false-path
            // — "falsy" — is not cleanly representable: `""`, `"0"`, `0`, null …).
            if then && let CondOperand::Var(v) = op {
                out.push(Refine::Truthy(v.clone()));
            }
        }
        CondExpr::Not(c) => collect_refine(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_refine(a, true, out);
            collect_refine(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_refine(a, false, out);
            collect_refine(b, false, out);
        }
        _ => {}
    }
}

/// Refinements from a comparison guard on a given polarity.
fn collect_cmp_refine(op: CmpOp, lhs: &CondOperand, rhs: &CondOperand, then: bool, out: &mut Vec<Refine>) {
    // Identity/equality guards over a (var, literal) pair.
    if let Some((v, val)) = var_literal(lhs, rhs) {
        // The *effective* operator on this branch: `===`/`!==` flip under `!then`.
        let identical = match (op, then) {
            (CmpOp::Identical, true) | (CmpOp::NotIdentical, false) => Some(true),
            (CmpOp::NotIdentical, true) | (CmpOp::Identical, false) => Some(false),
            _ => None,
        };
        if let Some(identical) = identical
            && let Some(vv) = val_of(&val)
        {
            match (identical, &vv) {
                (true, _) => out.push(Refine::Exact(v, vv)),
                (false, Val::Null) => out.push(Refine::NotNull(v)),
                (false, _) => out.push(Refine::Exclude(v, vv)),
            }
            return;
        }
    }
    // Ordering guards over a (var, int-literal) pair → an interval intersection.
    if let Some((v, k, var_on_left)) = var_int_literal(lhs, rhs) {
        // Normalize so the operator reads `var <op> k`.
        let eff_op = if var_on_left { op } else { flip_ordering(op) };
        // On the false-path the guard is negated.
        let branch_op = if then { eff_op } else { negate_ordering(eff_op) };
        if let Some(range) = ordering_range(branch_op, k) {
            out.push(Refine::IntRange(v, range));
        }
    }
}

/// The `($var, literal)` of a comparison whose two operands are exactly one bare
/// variable and one literal (in either order).
fn var_literal(lhs: &CondOperand, rhs: &CondOperand) -> Option<(String, ArgValue)> {
    match (lhs, rhs) {
        (CondOperand::Var(v), CondOperand::Literal(val))
        | (CondOperand::Literal(val), CondOperand::Var(v)) => Some((v.clone(), val.clone())),
        _ => None,
    }
}

/// The `($var, int_literal, var_on_left)` of a comparison with one bare variable
/// and one **int** literal (ordering refinement only applies to int bounds).
fn var_int_literal(lhs: &CondOperand, rhs: &CondOperand) -> Option<(String, i64, bool)> {
    match (lhs, rhs) {
        (CondOperand::Var(v), CondOperand::Literal(ArgValue::Int(i))) => Some((v.clone(), *i, true)),
        (CondOperand::Literal(ArgValue::Int(i)), CondOperand::Var(v)) => Some((v.clone(), *i, false)),
        _ => None,
    }
}

/// Mirror an ordering operator (used when the variable is the right operand).
fn flip_ordering(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        other => other,
    }
}

/// The logical negation of an ordering operator (for the false-path).
fn negate_ordering(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Ge,
        CmpOp::Le => CmpOp::Gt,
        CmpOp::Gt => CmpOp::Le,
        CmpOp::Ge => CmpOp::Lt,
        other => other,
    }
}

/// The int interval `var <op> k` denotes (`> 0` → positive, `>= 0` → non-negative,
/// `<`/`<=` symmetric). `None` when the interval is empty (a saturating bound
/// overflow) — the caller adds no refinement.
fn ordering_range(op: CmpOp, k: i64) -> Option<IntRange> {
    match op {
        CmpOp::Gt => IntRange::new(k.checked_add(1)?, i64::MAX),
        CmpOp::Ge => IntRange::new(k, i64::MAX),
        CmpOp::Lt => IntRange::new(i64::MIN, k.checked_sub(1)?),
        CmpOp::Le => IntRange::new(i64::MIN, k),
        _ => None,
    }
}

/// Apply a branch's refinements to its cloned env (clearing any stale exact-class
/// fact for a positively-narrowed variable).
fn apply_refinements(
    refs: &[Refine],
    env: &mut HashMap<String, Known>,
    classes_env: &mut HashMap<String, String>,
) {
    for r in refs {
        match r {
            Refine::Exact(var, val) => {
                env.insert(
                    var.clone(),
                    Known {
                        fact: Fact::Singleton(val.clone()),
                        line: 0,
                        bound: Some("proven on this branch".to_owned()),
                    },
                );
                classes_env.remove(var);
            }
            Refine::NotNull(var) => refine_fact(env, var, clear_null),
            Refine::Exclude(var, val) => {
                refine_fact(env, var, |f| exclude_member(f, val));
            }
            Refine::IntRange(var, range) => refine_fact(env, var, |f| intersect_int(f, *range)),
            Refine::Truthy(var) => refine_fact(env, var, truthy_narrow),
        }
    }
}

/// Transform the fact of `var` in place with `f` (a `None` result drops the fact —
/// the conservative empty-fact fallback); a no-op when `var` has no fact.
fn refine_fact(
    env: &mut HashMap<String, Known>,
    var: &str,
    f: impl FnOnce(&Fact) -> Option<Fact>,
) {
    let Some(k) = env.get(var) else { return };
    match f(&k.fact) {
        Some(nf) => {
            let (line, bound) = (k.line, k.bound.clone());
            env.insert(var.to_owned(), Known { fact: nf, line, bound });
        }
        None => {
            env.remove(var);
        }
    }
}

/// Clear nullability: an abstract fact loses its `nullable` flag; a finite fact
/// loses its `null` member. `None` only if that empties a finite fact.
fn clear_null(f: &Fact) -> Option<Fact> {
    match f {
        Fact::Refined { base, refinement, nullable: true } => {
            Some(Fact::refined(*base, *refinement, false))
        }
        Fact::General { base, nullable: true } => Some(Fact::General { base: *base, nullable: false }),
        Fact::Singleton(_) | Fact::OneOf(_) => exclude_member(f, &Val::Null),
        // Already non-nullable abstract fact — unchanged.
        other => Some(other.clone()),
    }
}

/// Remove `val` from a finite fact; for a String-based abstract fact excluding
/// `""`, add `NON_EMPTY` (the `!== ''` refinement). Otherwise unchanged.
fn exclude_member(f: &Fact, val: &Val) -> Option<Fact> {
    match f.finite_members() {
        Some(members) => {
            let kept: Vec<Val> = members.iter().filter(|m| *m != val).cloned().collect();
            // Empty → drop the fact (conservative fallback; a truly-dead branch is
            // already pruned by the decided-guard verdict).
            Fact::from_vals(kept)
        }
        None => match (f, val) {
            (Fact::Refined { base: Base::String, .. } | Fact::General { base: Base::String, .. }, Val::Str(s))
                if s.is_empty() =>
            {
                Some(add_str_preds(f, StrPreds::NON_EMPTY))
            }
            _ => Some(f.clone()),
        },
    }
}

/// Intersect an Int-based abstract fact with `range`; a finite/other fact is left
/// unchanged. `None` when the intersection is empty.
fn intersect_int(f: &Fact, range: IntRange) -> Option<Fact> {
    match f {
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(have), nullable } => {
            let r = have.intersect(range)?;
            Some(Fact::refined(Base::Int, Refinement::Int(r), *nullable))
        }
        Fact::General { base: Base::Int, nullable } => {
            Some(Fact::refined(Base::Int, Refinement::Int(range), *nullable))
        }
        other => Some(other.clone()),
    }
}

/// Truthiness narrowing on the true-path: clear nullability (null is falsy) and,
/// for a String-based fact, add `NON_FALSY`. Int-based facts gain nothing usable
/// (nonzero is not an interval — skipped, documented). Never empties.
fn truthy_narrow(f: &Fact) -> Option<Fact> {
    let f = clear_null(f)?;
    Some(match &f {
        Fact::Refined { base: Base::String, .. } | Fact::General { base: Base::String, .. } => {
            add_str_preds(&f, StrPreds::NON_FALSY)
        }
        other => other.clone(),
    })
}

/// Add string predicates to a String-based abstract fact (union-closed); a
/// non-string or finite fact is returned unchanged.
fn add_str_preds(f: &Fact, preds: StrPreds) -> Fact {
    match f {
        Fact::Refined { base: Base::String, refinement: Refinement::Str(have), nullable } => {
            Fact::refined(Base::String, Refinement::Str(have.union(preds)), *nullable)
        }
        Fact::General { base: Base::String, nullable } => {
            Fact::refined(Base::String, Refinement::Str(preds), *nullable)
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// `@phpstan-assert` application (ADR-0030, Feature D). After a call to an
// assertion helper, the asserted type narrows the CALLER's env for the variable
// passed at the asserted position. `Always` asserts apply on the fall-through
// (statement position); `-if-true`/`-if-false` apply only in guard position.
// ---------------------------------------------------------------------------

/// Convert a lowered contract type to the domain [`Fact`] an assertion of it
/// establishes (conservative): `Base` → General, `IntIn` → Refined, `StrWith` →
/// Refined, `Null` → `Singleton(null)`, a nullable union (`X|null`) → `X`'s fact
/// with `nullable = true`; anything else → `None` (no application).
fn assert_fact_of(cty: &steins_contract::ContractTy) -> Option<Fact> {
    use steins_contract::ContractTy as C;
    match cty {
        C::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        C::IntIn(r) => Some(Fact::refined(Base::Int, Refinement::Int(*r), false)),
        C::StrWith(p) => Some(Fact::refined(Base::String, Refinement::Str(*p), false)),
        C::Null => Some(Fact::Singleton(Val::Null)),
        C::Union(members) => {
            let has_null = members.iter().any(|m| matches!(m, C::Null));
            let non_null: Vec<&C> = members.iter().filter(|m| !matches!(m, C::Null)).collect();
            // `X|null` (exactly one representable non-null member) → X, nullable.
            if has_null && non_null.len() == 1 {
                return Some(with_nullable(assert_fact_of(non_null[0])?));
            }
            None
        }
        _ => None,
    }
}

/// Set the `nullable` flag on an abstract fact (a `Singleton`/`OneOf` is left
/// unchanged — a nullable-union member never lowers to a finite fact here).
fn with_nullable(f: Fact) -> Fact {
    match f {
        Fact::General { base, .. } => Fact::General { base, nullable: true },
        Fact::Refined { base, refinement, .. } => Fact::refined(base, refinement, true),
        other => other,
    }
}

/// Apply the `Always` assertions of every statically-resolved call in a statement
/// to the caller's env (Feature D). `-if-true`/`-if-false` asserts are **not**
/// applied here — they hold only conditionally on the boolean result, so they
/// belong to guard position (see the deferral note in the module tests).
#[allow(clippy::too_many_arguments)]
fn apply_stmt_asserts(
    cx: &Cx,
    scope: &Scope,
    call: &CallExpr,
    env: &mut HashMap<String, Known>,
    classes_env: &mut HashMap<String, String>,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    asserted: &mut HashSet<String>,
) {
    if scope.poisoned || !call.positional_only {
        return;
    }
    let (params, docblock): (&[Param], Option<&str>) = match &call.receiver {
        Callee::Function(_) => {
            let Some(site) = cx.resolve_user_fn(call) else { return };
            let decl = cx.fn_decl(site);
            (&decl.params, decl.docblock.as_deref())
        }
        Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
            let Some(target) = resolve_call_target(
                cx, &call.receiver, classes_env, this_exact, enclosing_class, scope.poisoned,
            ) else {
                return;
            };
            (&target.method.params, target.method.docblock.as_deref())
        }
        Callee::Dynamic => return,
    };
    let Some(envelopes) = parse_envelopes(docblock) else { return };
    for spec in &envelopes.asserts {
        if spec.kind != AssertKind::Always {
            continue;
        }
        let Some(pos) = params.iter().position(|p| p.name == spec.param) else { continue };
        let Some(arg) = call.args.get(pos) else { continue };
        let ArgValue::Var(v) = &arg.value else { continue };
        if apply_assert_to_var(env, classes_env, v, spec) {
            asserted.insert(v.clone());
        }
    }
}

/// Apply one assertion spec to a caller variable (replace-if-weaker): a stronger
/// finite fact (`Singleton`/`OneOf`) is kept; otherwise the asserted fact replaces
/// it. A negated `!null` clears nullability; other negated forms are not
/// representable as a positive fact and are skipped (documented).
///
/// Returns whether the variable now carries an established fact (so the caller
/// protects it from the by-ref invalidation) — `true` when a fact was set or a
/// stronger finite fact was deliberately kept, `false` when nothing applied.
fn apply_assert_to_var(
    env: &mut HashMap<String, Known>,
    classes_env: &mut HashMap<String, String>,
    var: &str,
    spec: &AssertSpec,
) -> bool {
    let cty = steins_contract::lower(&spec.ty);
    if spec.negated {
        // Only `!null` is representable as a positive narrowing (clear nullable);
        // other negated forms establish nothing.
        if matches!(cty, steins_contract::ContractTy::Null) && env.contains_key(var) {
            refine_fact(env, var, clear_null);
            return true;
        }
        return false;
    }
    let Some(fact) = assert_fact_of(&cty) else { return false };
    // Never override a stronger finite fact with a weaker asserted one; keep it
    // (and still protect it from invalidation — the by-value assert did not mutate
    // it). Assertion helpers are conventionally by-value; a by-ref helper that
    // rebinds is the documented edge where keeping the singleton is the simple,
    // task-specified choice.
    if env.get(var).is_some_and(|k| k.fact.finite_members().is_some()) {
        return true;
    }
    env.insert(var.to_owned(), Known { fact, line: 0, bound: Some("asserted".to_owned()) });
    classes_env.remove(var);
    true
}

/// Every bare variable an opaque sub-condition reads (for the guard-mutation
/// invalidation: an opaque condition may mutate its operands by reference).
fn cond_invalidations(cond: &CondExpr) -> Vec<String> {
    let mut out = Vec::new();
    collect_cond_opaque_reads(cond, &mut out);
    out
}

fn collect_cond_opaque_reads(cond: &CondExpr, out: &mut Vec<String>) {
    match cond {
        CondExpr::Opaque { reads } => {
            for r in reads {
                if !out.contains(r) {
                    out.push(r.clone());
                }
            }
        }
        CondExpr::Not(c) => collect_cond_opaque_reads(c, out),
        CondExpr::And(a, b) | CondExpr::Or(a, b) => {
            collect_cond_opaque_reads(a, out);
            collect_cond_opaque_reads(b, out);
        }
        _ => {}
    }
}

/// Join the fall-through envs of several live branches (ADR-0031/0035): a scalar
/// fact survives only when present in *every* branch, folded through [`Fact::join`]
/// (equal → Singleton; differing → OneOf; overflow → dropped). An exact-class fact
/// survives only when every branch agrees on the class.
fn join_envs(
    branches: Vec<(HashMap<String, Known>, HashMap<String, String>)>,
) -> (HashMap<String, Known>, HashMap<String, String>) {
    let mut it = branches.into_iter();
    let (first_env, first_classes) = it.next().expect("join_envs called with no branches");
    let rest: Vec<(HashMap<String, Known>, HashMap<String, String>)> = it.collect();
    if rest.is_empty() {
        return (first_env, first_classes);
    }

    let mut env: HashMap<String, Known> = HashMap::new();
    for (name, k0) in &first_env {
        let mut fact = k0.fact.clone();
        let mut ok = true;
        for (be, _) in &rest {
            match be.get(name) {
                Some(k) => match fact.join(&k.fact) {
                    Some(joined) => fact = joined,
                    None => {
                        ok = false;
                        break;
                    }
                },
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            env.insert(name.clone(), Known { fact, line: k0.line, bound: k0.bound.clone() });
        }
    }

    let mut classes: HashMap<String, String> = HashMap::new();
    for (name, c0) in &first_classes {
        if rest.iter().all(|(_, bc)| bc.get(name) == Some(c0)) {
            classes.insert(name.clone(), c0.clone());
        }
    }
    (env, classes)
}

// ---------------------------------------------------------------------------
// PHP scalar comparison semantics (ADR-0031). `===`/truthiness are exact; `==`
// is settled EMPIRICALLY against PHP 8.5.8 (see [`php_loose_eq`]). Undecidable
// cells return `None` → the caller yields `Maybe` (silence, the sound side).
// ---------------------------------------------------------------------------

/// PHP truthiness of a proven value. Note `"0"` and `""` are the only falsy
/// non-empty/-empty strings (`"0.0"` and `"00"` are **truthy**), `0`/`0.0`/`[]`
/// are falsy. `None` for a non-concrete value.
fn php_truthy(v: &ArgValue) -> Option<bool> {
    match v {
        ArgValue::Null => Some(false),
        ArgValue::Bool(b) => Some(*b),
        ArgValue::Int(i) => Some(*i != 0),
        ArgValue::Float(f) => Some(*f != 0.0),
        ArgValue::Str(s) => Some(!(s.is_empty() || s == "0")),
        ArgValue::Array(items) => Some(!items.is_empty()),
        _ => None,
    }
}

/// Strict identity `===`: same runtime type AND equal value. Different concrete
/// runtime types are a definite non-identity; a non-concrete operand is `None`.
fn php_identical(a: &ArgValue, b: &ArgValue) -> Option<bool> {
    use ArgValue::{Array, Bool, Float, Int, Null, Str};
    match (a, b) {
        (Int(x), Int(y)) => Some(x == y),
        (Float(x), Float(y)) => Some(x == y),
        (Str(x), Str(y)) => Some(x == y),
        (Bool(x), Bool(y)) => Some(x == y),
        (Null, Null) => Some(true),
        (Array(_), Array(_)) => php_array_identical(a, b),
        _ if is_concrete(a) && is_concrete(b) => Some(false),
        _ => None,
    }
}

/// Deep `===` of two array literals: same length, same key order, element-wise
/// identical. A non-concrete element makes the result `None`.
fn php_array_identical(a: &ArgValue, b: &ArgValue) -> Option<bool> {
    let (ArgValue::Array(ai), ArgValue::Array(bi)) = (a, b) else { return None };
    let na = normalize_array(ai);
    let nb = normalize_array(bi);
    if na.len() != nb.len() {
        return Some(false);
    }
    for ((ka, va), (kb, vb)) in na.iter().zip(nb.iter()) {
        if ka != kb {
            return Some(false);
        }
        match php_identical(va, vb) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => return None,
        }
    }
    Some(true)
}

/// Whether a value is a fully-known concrete value (a scalar literal or an array).
fn is_concrete(v: &ArgValue) -> bool {
    v.is_literal() || matches!(v, ArgValue::Array(_))
}

/// Loose equality `==`, settled **empirically against PHP 8.5.8** (`php -r`, the
/// full cross-product of `null`/`false`/`true`/`0`/`0.0`/`""`/`"0"`/`"abc"`/`"5"`/`[]`
/// recorded). The measured table (`T` = equal):
///
/// ```text
///           null false true  0   0.0   ""   "0"  "abc" "5"   []
///   null     T    T    F    T    T     T    F    F     F     T
///   false    T    T    F    T    T     T    T    F     F     T
///   true     F    F    T    F    F     F    F    T     T     F
///   0        T    T    F    T    T     F    T    F     F     F
///   0.0      T    T    F    T    T     F    T    F     F     F
///   ""       T    T    F    F    F     T    F    F     F     F
///   "0"      F    T    F    T    T     F    T    F     F     F
///   "abc"    F    F    T    F    F     F    F    T     F     F
///   "5"      F    F    T    F    F     F    F    F     T     F
///   []       T    T    F    F    F     F    F    F     F     T
/// ```
///
/// The rules reproduced (stable since PHP 8.0): a `bool` operand casts BOTH sides
/// to bool; `null` compares to the other side's zero/empty (except bool, handled
/// by the bool rule); `int`/`float` vs a numeric string compares numerically,
/// vs a non-numeric string compares the number's string form; two strings compare
/// numerically iff both are numeric strings, else byte-wise; an array is unequal
/// to any scalar (non-null, non-bool). Cells not covered (a `float` vs a
/// non-numeric string; non-trivial arrays) return `None` → `Maybe`.
fn php_loose_eq(a: &ArgValue, b: &ArgValue) -> Option<bool> {
    use ArgValue::{Array, Bool, Float, Int, Null, Str};
    // A `bool` on either side casts both operands to bool (subsumes null==bool).
    if matches!(a, Bool(_)) || matches!(b, Bool(_)) {
        return Some(php_truthy(a)? == php_truthy(b)?);
    }
    match (a, b) {
        (Null, Null) => Some(true),
        (Null, Int(i)) | (Int(i), Null) => Some(*i == 0),
        (Null, Float(f)) | (Float(f), Null) => Some(*f == 0.0),
        (Null, Str(s)) | (Str(s), Null) => Some(s.is_empty()),
        (Null, Array(items)) | (Array(items), Null) => Some(items.is_empty()),
        (Null, _) | (_, Null) => None,

        (Int(x), Int(y)) => Some(x == y),
        (Int(x), Float(y)) | (Float(y), Int(x)) => Some((*x as f64) == *y),
        (Float(x), Float(y)) => Some(x == y),

        (Int(i), Str(s)) | (Str(s), Int(i)) => Some(php_int_str_eq(*i, s)),
        (Float(f), Str(s)) | (Str(s), Float(f)) => php_float_str_eq(*f, s),
        (Str(x), Str(y)) => Some(php_str_eq(x, y)),

        (Array(x), Array(y)) => php_array_loose_eq(x, y),
        // An array is never loosely equal to a (non-null, non-bool) scalar.
        (Array(_), Int(_) | Float(_) | Str(_)) | (Int(_) | Float(_) | Str(_), Array(_)) => {
            Some(false)
        }
        _ => None,
    }
}

/// `int == string`: numeric string → numeric compare; else compare the int's
/// decimal form to the string (PHP 8 semantics).
fn php_int_str_eq(i: i64, s: &str) -> bool {
    if php_is_numeric(s) {
        php_str_to_float(s).is_some_and(|f| (i as f64) == f)
    } else {
        i.to_string() == s
    }
}

/// `float == string`: numeric string → numeric compare; a non-numeric string is
/// undecidable here (float→string formatting is precision-sensitive) → `None`.
fn php_float_str_eq(f: f64, s: &str) -> Option<bool> {
    if php_is_numeric(s) {
        Some(php_str_to_float(s).is_some_and(|g| f == g))
    } else {
        None
    }
}

/// `string == string`: both numeric strings → numeric compare; else byte compare.
fn php_str_eq(x: &str, y: &str) -> bool {
    if php_is_numeric(x) && php_is_numeric(y) {
        match (php_str_to_float(x), php_str_to_float(y)) {
            (Some(a), Some(b)) => a == b,
            _ => x == y,
        }
    } else {
        x == y
    }
}

/// `array == array`: same key set with loosely-equal values (order-independent).
/// An undecidable element value makes the whole comparison `None`.
fn php_array_loose_eq(x: &[(ArrayKey, ArgValue)], y: &[(ArrayKey, ArgValue)]) -> Option<bool> {
    let nx = normalize_array(x);
    let ny = normalize_array(y);
    if nx.len() != ny.len() {
        return Some(false);
    }
    for (k, va) in &nx {
        let Some((_, vb)) = ny.iter().find(|(k2, _)| k2 == k) else {
            return Some(false);
        };
        match php_loose_eq(va, vb) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => return None,
        }
    }
    Some(true)
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
                ArgValue::Var(name) if !poisoned => env.get(name).and_then(|k| {
                    let v = k.singleton()?;
                    let prov = match &k.bound {
                        Some(b) => format!("from ${name}, {b}"),
                        None => format!("from ${name}, assigned at line {}", k.line),
                    };
                    Some((v, prov))
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

    // Bound params are always resolved literals/arrays, so `singleton_fact`
    // succeeds; a value that somehow fails conversion is simply left unbound
    // (the callee param stays unknown — sound).
    let bound_env: HashMap<String, Known> = bound
        .into_iter()
        .filter_map(|(name, value)| {
            singleton_fact(&value)
                .map(|fact| (name, Known { fact, line: 0, bound: Some(provenance.to_owned()) }))
        })
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
        Callee::Method { receiver: Receiver::New(class), method, .. } => {
            let fqn = cx.class_fqn(class);
            resolve_exact(cx, &fqn, method, enclosing_class, Some(fqn.clone()))
        }
        Callee::Method { receiver: Receiver::Var(v), method, .. } => {
            if poisoned {
                return None;
            }
            let class = classes_env.get(v)?;
            resolve_exact(cx, class, method, enclosing_class, Some(class.clone()))
        }
        Callee::Method { receiver: Receiver::This, method, .. } => {
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
                ArgValue::Var(name) if !poisoned => env.get(name).and_then(|k| {
                    let v = k.singleton()?;
                    let prov = match &k.bound {
                        Some(b) => format!("from ${name}, {b}"),
                        None => format!("from ${name}, assigned at line {}", k.line),
                    };
                    Some((v, Some(prov)))
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
        // Non-literal (`Var`/`Call`/`New`/`Ternary`/`Other`): not provable → never
        // an error (a `Ternary` is resolved to a concrete arm before this point).
        ArgValue::Var(_)
        | ArgValue::Call(..)
        | ArgValue::New(..)
        | ArgValue::Ternary { .. }
        | ArgValue::Other => false,
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

/// Intersection-style combine: `No` dominates, then `Maybe`, else `Yes`. Used
/// when *every* sub-obligation must hold (element/key membership, shape items).
/// This is exactly [`Certainty::and`], kept as a free function for the existing
/// call sites.
fn combine(a: Certainty, b: Certainty) -> Certainty {
    a.and(b)
}

/// A convenience alias inside this module: the phpdoc contract acceptance code
/// (ADR-0030) was written against a local `Tri`; it now shares the one project-wide
/// [`Certainty`] type (ADR-0031 — one trinary, never parallel ones).
use Certainty as Tri;

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
    /// Parameter names (no `$`) that an assertion tag (`@phpstan-assert` &c.)
    /// targets on this same declaration — the function is an **assertion helper**
    /// for them (see [`check_phpdoc_param`]). Property/`$this->…` assertion targets
    /// are excluded (they say nothing about a call-site argument).
    assert_params: HashSet<String>,
    /// The full assertion specs on this declaration (Feature D): the asserted type
    /// applied to the caller's env after a call (`Always`), or in guard position
    /// (`IfTrue`/`IfFalse`). Property/`$this` targets are excluded (as above).
    asserts: Vec<AssertSpec>,
}

/// One `@phpstan-assert[-if-true|-if-false] [!]<type> $param` spec (Feature D).
struct AssertSpec {
    /// Target parameter name (no `$`).
    param: String,
    /// The asserted phpdoc type.
    ty: PType,
    /// Unconditional / conditional-on-true / conditional-on-false.
    kind: AssertKind,
    /// The negated form (`@phpstan-assert !T $x`): asserts NOT `T`.
    negated: bool,
}

impl Envelopes {
    fn param(&self, name: &str) -> Option<&PType> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    /// Whether `name` is an assertion target on this declaration, in which case its
    /// `@param` states a **post**-condition and checking arguments against it is a
    /// category error (see [`check_phpdoc_param`]).
    fn is_assert_target(&self, name: &str) -> bool {
        self.assert_params.contains(name)
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
    let mut assert_params: HashSet<String> = HashSet::new();
    let mut asserts: Vec<AssertSpec> = Vec::new();
    for tag in scan_docblock(text) {
        // An assertion tag targeting a parameter marks it an assert-helper param
        // (its `@param` is a post-condition; ADR-0030). Property targets are inert.
        // All three kinds (Always/IfTrue/IfFalse) and the negated form exempt alike:
        // whatever the type or condition, the parameter is not being *constrained*
        // on entry, so a call-site argument cannot violate it. The spec is also
        // recorded (with its parsed type) for post-call application (Feature D).
        if let TagKind::Assert { kind: akind, negated } = tag.kind
            && !tag.assert_property_target
            && let Some(var) = &tag.var_name
        {
            let name = var.trim_start_matches('$').to_owned();
            assert_params.insert(name.clone());
            if let Some(ty) = parse_tag_type(&tag.type_text) {
                asserts.push(AssertSpec { param: name, ty, kind: akind, negated });
            }
            continue;
        }
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
            // Assertion tags are consumed above (collected into `assert_params`);
            // they never contribute a `@param`/`@return` envelope.
            TagKind::Assert { .. } => {}
        }
    }
    // Return an envelope set whenever there is anything to check *or* any assertion
    // to remember: an assert-only docblock still carries the exemption fact, so a
    // sibling `@param` (added later, or resolved in another pass) sees it.
    (!params.is_empty() || ret.is_some() || !assert_params.is_empty())
        .then_some(Envelopes { params, ret, assert_params, asserts })
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
                    // A `OneOf` fact is not one proven value → not a `CVal`.
                    let v = k.singleton()?;
                    self.resolve_cval(&v, env, classes_env, poisoned, folder)
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
///
/// # Assertion-helper exemption (ADR-0030)
///
/// A function/method whose docblock carries an assertion tag (`@phpstan-assert`
/// and its `-if-true`/`-if-false`/negated variants) targeting parameter `$x` is an
/// **assertion helper for `$x`**: its `@param` for `$x` states a *post*-condition
/// that the helper establishes about `$x`, not a precondition its callers must
/// satisfy. Checking a call-site argument against it is therefore semantically
/// wrong — the whole point of such a helper is to be called with a *wider* value
/// (e.g. `mixed`/`string|int`) and to narrow it. So we skip `phpdoc.param-mismatch`
/// for that parameter. This holds for all three assert kinds and the negated form.
///
/// Scope of the exemption, deliberately narrow:
/// - **Other parameters** of the same function are still checked (the tag exempts
///   only its own target).
/// - **`@return`** checking is unaffected (a different relation entirely).
/// - **Native** runtime checks are unaffected: a native type hint is a real
///   runtime gate regardless of any docblock assertion, so it still fires (and,
///   firing first, already suppresses this phpdoc check at that site).
///
/// This slice does **not** implement the *positive* refinement effect — applying
/// the asserted type to the caller's environment after the call. That is a
/// branch-analysis capability and lands with the structured trace tree / value
/// domain (ADR-0031, ADR-0035); here we only suppress the incorrect precondition
/// reading.
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
    // Assertion-helper exemption (see the doc comment above): this parameter's
    // `@param` is a post-condition, so a call-site argument cannot violate it.
    if envelopes.is_assert_target(&param.name) {
        return;
    }
    let Some(ty) = envelopes.param(&param.name) else { return };
    let param_name = &param.name;
    let rendered = match cx.resolve_cval(value, env, classes_env, poisoned, folder) {
        Some(cv) => {
            // A parameter that is nullable by its native type, or implicitly nullable
            // via a `= null` default, accepts `null` regardless of a non-nullable
            // `@param` spelling — PHP/PHPStan honor this, so reporting it would be a
            // false positive.
            if matches!(cv, CVal::Scalar(ArgValue::Null))
                && (param.has_null_default || param.ty.as_ref().is_some_and(|t| t.nullable))
            {
                return;
            }
            if accepts(cx, cfile, coff, ty, &cv) != Tri::No {
                return;
            }
            rendered_cval(&cv)
        }
        // Abstract-fact path (Feature E, ADR-0030/0035): an argument that resolves
        // to an abstract fact (not a proven value — e.g. a native-seeded param or a
        // guard-refined var) is judged by the domain's **set** acceptance via
        // `steins_contract::admits_fact`. Only a definite `No` (every value the fact
        // admits is rejected) reports; `Maybe` is silent.
        None => {
            let Some(fact) = arg_abstract_fact(value, env, poisoned) else { return };
            let cty = steins_contract::lower(ty);
            // A class-shaped contract stays silent against a scalar fact: a bare
            // identifier may be a template / type-alias, and infer's proven-value
            // `accepts` already treats a scalar vs any class name as `Maybe`. Keeping
            // the two paths consistent preserves the zero-FP posture.
            if contract_touches_class(&cty) {
                return;
            }
            if steins_contract::admits_fact(&cty, fact) != Certainty::No {
                return;
            }
            describe_fact(fact)
        }
    };
    let pos = cx.tree().position(arg_offset);
    let message = format!(
        "argument {rendered} to {callee}() violates declared @param {ty} ${param_name} — declared contract violation",
    );
    out.push(Diagnostic {
        id: PARAM_MISMATCH_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
    });
}

/// The abstract fact an argument resolves to: a bare `$var` whose env fact is an
/// abstract layer (no finite members). Finite/proven values go through
/// `resolve_cval` instead, so this is the disjoint "abstract" arm of Feature E.
fn arg_abstract_fact<'e>(
    value: &ArgValue,
    env: &'e HashMap<String, Known>,
    poisoned: bool,
) -> Option<&'e Fact> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    let f = &env.get(name)?.fact;
    f.finite_members().is_none().then_some(f)
}

/// Whether a lowered contract type contains a class-name node — a bare identifier
/// that may actually be a template or a type-alias. The abstract-fact check stays
/// silent on these (see [`check_phpdoc_param`]).
fn contract_touches_class(ty: &steins_contract::ContractTy) -> bool {
    use steins_contract::ContractTy as C;
    match ty {
        C::Class(_) => true,
        C::Union(m) | C::Inter(m) => m.iter().any(contract_touches_class),
        C::ListOf { elem, .. } => contract_touches_class(elem),
        C::MapOf { key, val, .. } | C::IterableOf { key, val } => {
            contract_touches_class(key) || contract_touches_class(val)
        }
        C::Shape { fields, unsealed, .. } => {
            fields.iter().any(|f| contract_touches_class(&f.ty))
                || unsealed.as_ref().is_some_and(|(k, v)| {
                    k.as_ref().is_some_and(|k| contract_touches_class(k))
                        || contract_touches_class(v)
                })
        }
        _ => false,
    }
}

/// A short, phpdoc-flavored description of an abstract fact for a diagnostic
/// message (`a value of type int`, `a non-empty-string value`, `an int|null
/// value`). Finite facts never reach here (they render as concrete values).
fn describe_fact(f: &Fact) -> String {
    let base_kw = |b: Base| match b {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
        Base::Bool => "bool",
    };
    let (name, nullable) = match f {
        Fact::General { base, nullable } => (base_kw(*base).to_owned(), *nullable),
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable } => {
            let n = if *r == IntRange::POSITIVE {
                "positive-int".to_owned()
            } else if *r == IntRange::NEGATIVE {
                "negative-int".to_owned()
            } else if *r == IntRange::NON_NEGATIVE {
                "non-negative-int".to_owned()
            } else {
                format!("int<{}, {}>", r.lo(), r.hi())
            };
            (n, *nullable)
        }
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable } => {
            let n = if p.contains_all(StrPreds::NON_FALSY) {
                "non-falsy-string"
            } else if p.contains_all(StrPreds::NUMERIC) {
                "numeric-string"
            } else if p.contains_all(StrPreds::NON_EMPTY) {
                "non-empty-string"
            } else {
                "string"
            };
            (n.to_owned(), *nullable)
        }
        Fact::Refined { base, nullable, .. } => (base_kw(*base).to_owned(), *nullable),
        // Finite facts do not reach here.
        Fact::Singleton(_) | Fact::OneOf(_) => ("value".to_owned(), false),
    };
    if nullable {
        format!("a value of type {name}|null")
    } else {
        format!("a value of type {name}")
    }
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

#[cfg(test)]
mod domain_tests {
    //! Unit tests for the ADR-0031/0035 domain skeleton: the unified [`Certainty`]
    //! algebra, [`Fact`] joins (agree / OneOf / cap overflow), and the empirically
    //! settled PHP comparison primitives.
    use super::*;
    use steins_syntax::ArgValue;

    fn sing(v: ArgValue) -> Fact {
        singleton_fact(&v).expect("literal converts")
    }

    #[test]
    fn certainty_algebra() {
        use Certainty::{Maybe, No, Yes};
        // not swaps the poles, fixes Maybe.
        assert_eq!(Yes.not(), No);
        assert_eq!(No.not(), Yes);
        assert_eq!(Maybe.not(), Maybe);
        // and: No dominates, then Maybe.
        assert_eq!(Yes.and(Yes), Yes);
        assert_eq!(Yes.and(No), No);
        assert_eq!(Yes.and(Maybe), Maybe);
        assert_eq!(No.and(Maybe), No);
        // or: Yes dominates, then Maybe.
        assert_eq!(No.or(No), No);
        assert_eq!(No.or(Yes), Yes);
        assert_eq!(No.or(Maybe), Maybe);
        assert_eq!(Yes.or(Maybe), Yes);
    }

    #[test]
    fn fact_join_agree_keeps_singleton() {
        // The env now stores `steins_domain::Fact`; joins go through the domain
        // algebra. Equal singletons stay a Singleton and resolve to the value.
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(5))).unwrap();
        assert!(matches!(j, Fact::Singleton(Val::Int(5))));
        let k = Known { fact: j, line: 0, bound: None };
        assert_eq!(k.singleton(), Some(ArgValue::Int(5)));
    }

    #[test]
    fn fact_join_differ_forms_oneof_and_dedups() {
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j, Fact::OneOf(vs) if vs.len() == 2));
        // A OneOf never resolves to a single proven value.
        assert_eq!(Known { fact: j.clone(), line: 0, bound: None }.singleton(), None);
        // Re-joining an already-present value dedups.
        let j2 = j.join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j2, Fact::OneOf(vs) if vs.len() == 2));
    }

    #[test]
    fn fact_join_overflow_widens_to_refined() {
        // Beyond the OneOf cap the domain widens to a *computed* Refined summary
        // (an int interval), rather than dropping — abstract facts now flow
        // through the env (ADR-0035 stage 2). The widened fact resolves no value.
        let full = Fact::from_vals((0..steins_domain::CAP as i64).map(Val::Int).collect()).unwrap();
        assert!(matches!(full, Fact::OneOf(_)));
        let widened = full.join(&sing(ArgValue::Int(999))).unwrap();
        assert!(matches!(widened, Fact::Refined { base: Base::Int, .. }));
        assert_eq!(Known { fact: widened, line: 0, bound: None }.singleton(), None);
    }

    #[test]
    fn loose_eq_measured_cells_php_8_5_8() {
        use ArgValue::{Bool, Int, Null, Str};
        let s = |x: &str| Str(x.to_owned());
        // A representative slice of the recorded PHP 8.5.8 table.
        assert_eq!(php_loose_eq(&Null, &Null), Some(true));
        assert_eq!(php_loose_eq(&Null, &Int(0)), Some(true));
        assert_eq!(php_loose_eq(&Null, &s("")), Some(true));
        assert_eq!(php_loose_eq(&Null, &s("0")), Some(false)); // the PHP 8 trap
        assert_eq!(php_loose_eq(&Null, &Bool(false)), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("0")), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("abc")), Some(false));
        assert_eq!(php_loose_eq(&Bool(true), &s("abc")), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s("abc")), Some(false)); // PHP 8, not PHP 7
        assert_eq!(php_loose_eq(&Int(0), &s("0")), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s("")), Some(false));
        assert_eq!(php_loose_eq(&s("0"), &s("")), Some(false));
        assert_eq!(php_loose_eq(&s("5"), &s("5")), Some(true));
        assert_eq!(php_loose_eq(&s("5"), &Int(5)), Some(true));
    }

    #[test]
    fn truthiness_edge_cells() {
        use ArgValue::{Array, Float, Int, Null, Str};
        assert_eq!(php_truthy(&Str("0".to_owned())), Some(false)); // "0" is falsy
        assert_eq!(php_truthy(&Str("0.0".to_owned())), Some(true)); // but "0.0" is truthy
        assert_eq!(php_truthy(&Str(String::new())), Some(false));
        assert_eq!(php_truthy(&Int(0)), Some(false));
        assert_eq!(php_truthy(&Float(0.0)), Some(false));
        assert_eq!(php_truthy(&Null), Some(false));
        assert_eq!(php_truthy(&Array(vec![])), Some(false)); // [] is falsy
    }

    #[test]
    fn identical_is_type_strict() {
        use ArgValue::{Float, Int};
        assert_eq!(php_identical(&Int(5), &Int(5)), Some(true));
        assert_eq!(php_identical(&Int(5), &Float(5.0)), Some(false)); // 5 === 5.0 is false
    }
}
