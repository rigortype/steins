//! The inference engine for the first vertical slice.
//!
//! It implements exactly one proof-layer diagnostic (ADR-0002, held to the
//! zero-false-positive bar): [`ID`] = `type.argument-mismatch` (ADR-0022 kebab
//! `family.rule`). A call to a **user-defined function in the same file** that
//! passes a **literal** argument which **provably** raises a runtime `TypeError`
//! under PHP 8.1+ semantics (ADR-0011), honoring the calling file's
//! `declare(strict_types=1)`, is flagged. Everything not provable is silent.
//!
//! The whole thing is a salsa tracked query ([`diagnostics`]) built on
//! `steins-db`'s [`parse`] / [`function_index`] queries (ADR-0009), so it is a
//! memoized fact, not a batch pass.

use std::collections::{HashMap, HashSet};

use steins_db::{Db, SourceFile, function_index, parse};
use steins_sidecar::{FoldArg, FoldResult, FoldValue, Sidecar};
use steins_syntax::{
    ArgValue, Callee, ClassDecl, EffectEnvelope, EffectOrigin, EffectRecv, FunctionDecl, MethodDecl,
    Param, ParamType, Receiver, ScalarType, Scope, ScopeOwner, SourceTree, StaticClass, StmtKind,
    Visibility,
};
use steins_syntax::CallExpr;

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
/// same-file calls propagating a literal is followed at most this many frames
/// deep, after which the descent stops with **no** diagnostic (a cutoff names
/// itself as silence, never a manufactured finding). Direct and indirect
/// recursion is caught earlier by the on-stack binding set; this bound guards
/// against merely long, non-cyclic chains.
pub const MAX_BINDING_DEPTH: usize = 8;

/// The one-line coverage-posture notice (ADR-0004): printed to stderr when a run
/// executes as the sound subset because the PHP sidecar is unavailable.
pub const SOUND_SUBSET_NOTICE: &str =
    "note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted";

// ---------------------------------------------------------------------------
// Folding seam (ADR-0004 / ADR-0024).
//
// Folding — executing a real pure builtin over literal args to learn its value
// — is the one part of the check that may perform IPC and is therefore NOT a
// salsa query (queries must stay deterministic and side-effect-free). The
// engine expresses its need for a fold through this trait; who answers it (a
// real PHP sidecar, a test mock, or nobody) is the caller's choice.
// ---------------------------------------------------------------------------

/// Something that can fold a builtin call to a concrete literal value.
///
/// The engine only calls [`Folder::fold`] after it has already checked the
/// gate: `name` is not a same-file user function, [`steins_catalog::foldable`]
/// is `true`, and every element of `args` is a literal ([`ArgValue::is_literal`]).
/// A `None` return means "widen" (unknown) — always the safe side.
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
///
/// Lifecycle (ADR-0004): the sidecar is spawned only when the first foldable
/// call is actually encountered. If spawning fails (or `--no-php` disabled it),
/// every fold widens and the sound-subset notice is emitted once to stderr.
pub struct SidecarFolder {
    sidecar: Option<Sidecar>,
    memo: HashMap<(String, Vec<ArgValue>), Option<ArgValue>>,
    /// Explicitly disabled (`--no-php`): never spawn, never fold.
    disabled: bool,
    /// A prior spawn attempt failed: stop trying.
    spawn_failed: bool,
    /// The sound-subset notice has already been printed.
    notified: bool,
}

impl SidecarFolder {
    /// Create a folder. `disabled` (the CLI's `--no-php`) makes it a permanent
    /// no-op that never spawns PHP. When disabled by flag the caller is expected
    /// to have already surfaced the coverage posture, so this folder stays quiet.
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
    /// it cannot spawn PHP. Used by callers that do not print the notice up front.
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
            // Defensive: every arg is a literal by the engine's gate, so the
            // count must match; a mismatch means a non-literal slipped in.
            if fargs.len() != args.len() {
                return None;
            }
            match sc.fold(name, &fargs) {
                FoldResult::Value(v) => fold_value_to_arg(&v),
                // Throw / widen both mean "no known literal" for now.
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

/// Convert a folded value back to a literal [`ArgValue`]. Array results have no
/// literal in the IR yet, so they widen.
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

/// The proof-layer diagnostics for one file, as a memoized salsa query.
///
/// This query computes the **sound subset**: it uses [`NoFold`], so it never
/// executes PHP and stays a pure, deterministic salsa fact. Runs that want
/// folding (CLI, gate) call [`check_file`] instead — same salsa inputs
/// ([`parse`], [`function_index`]), but the folding check runs *outside* the
/// query graph.
#[salsa::tracked]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let functions = function_index(db, file);
    check_with(tree, functions, file.path(db), &mut NoFold)
}

/// The folding-aware check for one file, run **outside** salsa (ADR-0004).
///
/// Salsa determinism is preserved by construction: `parse` and `function_index`
/// remain memoized queries, but the folding pass — which may perform IPC — is a
/// plain function taking `&mut dyn Folder`. Pass a [`SidecarFolder`] for the
/// default posture, or [`NoFold`] for the sound subset.
#[must_use]
pub fn check_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let functions = function_index(db, file);
    check_with(tree, functions, file.path(db), folder)
}

/// The pure checking core with no folding — the sound subset. Kept for
/// unit tests and callers that never execute PHP; equivalent to
/// [`check_with`] with [`NoFold`].
#[must_use]
pub fn check(tree: &SourceTree, functions: &[FunctionDecl], path: &str) -> Vec<Diagnostic> {
    check_with(tree, functions, path, &mut NoFold)
}

// ---------------------------------------------------------------------------
// `annotate` facts (ADR-0020): the Rigor-style margin — proven facts only.
// ---------------------------------------------------------------------------

/// One proven fact the `annotate` margin can print against a source line. The
/// honesty rule (ADR-0020): every variant is something the analyzer *proved* —
/// where nothing is known, no fact is produced and the line stays bare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactKind {
    /// The inferred effect set on a function/method **declaration** line — the
    /// same fixpoint the envelope check uses, exposed for every unit. `labels`
    /// are sorted+deduplicated; `exhaustive` false appends the `…?` marker.
    Effects { labels: Vec<String>, exhaustive: bool },
    /// A proven post-statement value on an **assignment** line: `$var`'s rendered
    /// literal (a plain literal, a folded builtin result, or a const-fn return).
    Value { var: String, rendered: String },
    /// A proven exact class on an assignment line (`$x = new Foo()`).
    ExactClass { var: String, class: String },
    /// A **call** line that produced a check diagnostic; carries its registry id.
    Finding { id: &'static str },
}

/// A [`FactKind`] keyed to a 1-based source line (ADR-0020 margin display).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineFact {
    pub line: u32,
    pub kind: FactKind,
}

impl LineFact {
    /// The margin body (without the `//=>` prefix or padding), e.g.
    /// `effects: {io.fs.write, …?}`, `$w = "XY"`, `$x: Foo (exact)`,
    /// `✗ type.argument-mismatch`.
    #[must_use]
    pub fn body(&self) -> String {
        match &self.kind {
            FactKind::Effects { labels, exhaustive } => {
                let mut parts = labels.clone();
                if !*exhaustive {
                    // Silence names itself: "at least these; not proven complete."
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

/// Compute every proven margin fact for one file (ADR-0020 `annotate`), reusing
/// the existing analysis machinery — no new inference:
///
/// * **effects** on each declaration line come from [`effect_summary`] (the same
///   fixpoint as the envelope check, now with the exhaustiveness bit);
/// * **value / exact-class** facts come from re-running the propagation walk
///   ([`analyze_scope`]) with a fact collector attached, so the recorded value
///   is exactly the one the checker would trust at that point (folding included,
///   and correctly absent under a `NoFold`/`--no-php` folder);
/// * **finding** facts are the [`check_with`] diagnostics, keyed by line.
///
/// Facts are returned in line order (stable within a line).
#[must_use]
pub fn annotate_facts(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<LineFact> {
    let mut facts: Vec<LineFact> = Vec::new();

    // 1. Effects on every function/method declaration line.
    for s in effect_summary(tree, functions, classes) {
        facts.push(LineFact {
            line: s.line,
            kind: FactKind::Effects { labels: s.labels, exhaustive: s.exhaustive },
        });
    }

    // 2. Value / exact-class facts: the propagation walk with a collector. Each
    // top-level scope is walked once with an empty env (no binding assumptions);
    // descent bodies pass no collector, so only an assign's own line is recorded.
    let cx = Cx { tree, functions, classes: tree.classes(), path, strict: tree.has_strict_types() };
    let mut sink: Vec<Diagnostic> = Vec::new(); // diagnostics come from `check_with` below
    for scope in tree.scopes() {
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

    // 3. Findings: one marker per check diagnostic, at its line.
    for d in check_with(tree, functions, path, folder) {
        facts.push(LineFact { line: d.line, kind: FactKind::Finding { id: d.id } });
    }

    // Line order, stable within a line (effects, then values, then findings).
    facts.sort_by_key(|f| f.line);
    facts
}

/// Salsa-fed convenience over [`annotate_facts`], mirroring [`check_file`]: parse
/// + index are memoized queries; the fact walk runs outside the query graph.
#[must_use]
pub fn annotate_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<LineFact> {
    let tree = parse(db, file);
    let functions = function_index(db, file);
    annotate_facts(tree, functions, tree.classes(), file.path(db), folder)
}

/// The checking core (no salsa) — easy to unit-test and to reuse.
///
/// Two passes feed the one check. The **direct pass** walks every call site with
/// a literal argument (unchanged behavior). The **propagation pass** walks each
/// scope's linear trace and resolves `$var` / constant-function-return / folded
/// builtin-call arguments to proven values (ADR-0001). The two partition cleanly
/// by argument kind — literals go to the first, `Var`/`Call` arguments to the
/// second — so no call site is reported twice. `folder` answers fold requests
/// raised by `Call` arguments to allowlisted builtins.
#[must_use]
pub fn check_with(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<Diagnostic> {
    let cx = Cx { tree, functions, classes: tree.classes(), path, strict: tree.has_strict_types() };
    let mut out = Vec::new();

    // --- Direct pass: literal arguments at every function call site. ------
    for call in tree.calls() {
        let Some(decl) = resolve_callee(functions, call) else { continue };
        for (i, arg) in call.args.iter().enumerate() {
            let Some(ty) = param_scalar_type(&decl.params, i) else {
                if arg_binds_to_variadic(&decl.params, i) {
                    break;
                }
                continue;
            };
            // Only literals here; `Var`/`Call` are the propagation pass's job.
            if !arg.value.is_literal() {
                continue;
            }
            if is_type_error(cx.strict, ty, &arg.value) {
                out.push(cx.diagnostic(arg.span.start, &arg.value, None, &decl.name, &decl.params[i].name, ty));
            }
        }
    }

    // --- Propagation pass: resolved `$var` / constant-return / folded args,
    // plus all method / static / constructor call checking + descent. --------
    for scope in tree.scopes() {
        analyze_scope(&cx, folder, scope, HashMap::new(), HashMap::new(), None, None, None, &mut out);
    }

    // --- Effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking. Needs no
    // folder/sidecar — it reads only the catalog and CST-derived effect origins.
    out.extend(effect_diagnostics(tree, functions, tree.classes(), path));

    // Global dedup (Feature B): the same finding can be reached both by a scope's
    // empty-env walk and by a binding descent into that scope, or by a diamond of
    // binding paths. Identical `(id, path, line, column, message)` tuples collapse
    // to one; findings that differ only in binding provenance stay distinct.
    dedup(&mut out);
    out
}

/// Drop exact-duplicate diagnostics, preserving first-occurrence order.
fn dedup(out: &mut Vec<Diagnostic>) {
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
}

// ---------------------------------------------------------------------------
// Effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking, proven only.
// ---------------------------------------------------------------------------

/// One proven effect a function carries, with the provenance a transitive `via`
/// message needs. Two effects are the same iff their `(label, origin, line)`
/// agree, so the fixpoint deduplicates naturally.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectFinding {
    /// The effect label (ADR-0018 dot-path), e.g. `nondet.random`, `io.fs.write`.
    label: String,
    /// The ultimate origin's spelling: a builtin name, or `echo`/`print`/
    /// `exit`/`die`. Preserved verbatim as effects propagate up call edges, so a
    /// transitive finding names where the effect truly arises.
    origin: String,
    /// 1-based line of the ultimate origin (in whichever body defines it).
    line: u32,
}

/// Effect-envelope diagnostics for one file (ADR-0005), **proven violations
/// only**. A function declared `#[\Steins\Pure]` must have no effects; each
/// proven origin in (or transitively reachable from) its body is one finding.
///
/// Silent by construction — the deferred "cannot-verify" maybe-diagnostic of the
/// design (ADR-0005): uncatalogued builtins ([`steins_catalog::effect_labels`]
/// `None`), dynamic and method calls (never recorded as an [`EffectOrigin`]),
/// `throw` (permitted by `Pure`, ADR-0006), and closures nested in the body
/// (separate scopes, deferred this slice). `exit`/`die` **are** caught
/// structurally (ADR-0019 rule 4).
///
/// The same-file call graph is closed with a monotone fixpoint (effects of a
/// function = its own proven origins ∪ the effects of its same-file callees), so
/// direct and mutual recursion converge without looping.
/// A node in the unified effect call graph — a free function or a class method.
/// Names are the canonical declaration spellings, so an edge built from a resolved
/// callee matches the unit built for that callee.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Sym {
    Func(String),
    Method(String, String),
}

impl Sym {
    /// The diagnostic spelling of the symbol (`f` or `Foo::bar`).
    fn display(&self) -> String {
        match self {
            Sym::Func(n) => n.clone(),
            Sym::Method(c, m) => format!("{c}::{m}"),
        }
    }
}

/// One unit's fixpoint result: its proven effect findings and whether that set
/// is **exhaustive** (ADR-0005 certainty discipline). Exhaustive means every
/// call in (and transitively reachable from) the body resolved and was
/// classified — so the empty set really is *no effects*, not *no effects we
/// could see*. A single uncatalogued builtin, dynamic call, or unresolved method
/// anywhere in the reachable graph makes it non-exhaustive (the annotate `…?`).
#[derive(Debug, Clone, Default)]
struct EffectSet {
    findings: HashSet<EffectFinding>,
    exhaustive: bool,
}

/// The unified effect fixpoint for **every** function and method in the file —
/// computed regardless of any declared envelope, because `annotate` exposes the
/// effect set on every declaration line (not just `Pure`/`Effect` ones) and the
/// envelope check propagates a callee's effects to annotated callers either way.
///
/// Alongside the proven effect findings this carries the **exhaustiveness bit**:
/// a unit's own set is non-exhaustive the moment it contains an origin the
/// analyzer cannot classify — an uncatalogued builtin ([`steins_catalog::effect_labels`]
/// `None`), a structurally-dynamic call ([`EffectOrigin::Opaque`]), or a method
/// call whose same-file target cannot be resolved ([`resolve_effect_edge`] `None`).
/// Non-exhaustiveness then **taints callers** through the same call edges the
/// findings flow along, in the same monotone fixpoint (both quantities only ever
/// grow / flip toward "more/unknown", so chaotic iteration converges).
fn compute_effects(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
) -> HashMap<Sym, EffectSet> {
    // Every effect unit (function + method), with the class that encloses its
    // origins (for resolving `$this`/`self`/`parent` method-call edges).
    let mut units: Vec<(Sym, Option<&str>, &[EffectOrigin])> = Vec::new();
    for f in functions {
        units.push((Sym::Func(f.name.clone()), None, &f.effect_origins));
    }
    for c in classes {
        for m in &c.methods {
            units.push((
                Sym::Method(c.name.clone(), m.name.clone()),
                Some(c.name.as_str()),
                &m.effect_origins,
            ));
        }
    }

    // Per unit: its own proven origins (`direct`), its resolved same-file callee
    // edges (`edges`), and whether its *own* origins are all classifiable
    // (`own_exhaustive`; callee taint is folded in by the fixpoint below).
    let mut direct: HashMap<Sym, HashSet<EffectFinding>> = HashMap::new();
    let mut edges: HashMap<Sym, HashSet<Sym>> = HashMap::new();
    let mut exhaustive: HashMap<Sym, bool> = HashMap::new();
    for (sym, class, origins) in &units {
        let d = direct.entry(sym.clone()).or_default();
        let e = edges.entry(sym.clone()).or_default();
        let ex = exhaustive.entry(sym.clone()).or_insert(true);
        for origin in *origins {
            match origin {
                EffectOrigin::Call { name, span } => {
                    if let Some(canon) = user_fn_canon(functions, name) {
                        e.insert(Sym::Func(canon)); // same-file user function edge
                    } else {
                        // A catalogued builtin (colored or pure) is classified; an
                        // uncatalogued one cannot be proven effect-free.
                        if steins_catalog::effect_labels(name).is_none() {
                            *ex = false;
                        }
                        for f in builtin_findings(name, *span, tree) {
                            d.insert(f);
                        }
                    }
                }
                EffectOrigin::Output { keyword, span } => {
                    d.insert(EffectFinding {
                        label: "output".to_owned(),
                        origin: (*keyword).to_owned(),
                        line: tree.position(span.start).line,
                    });
                }
                EffectOrigin::Exit { keyword, span } => {
                    d.insert(EffectFinding {
                        label: "exit".to_owned(),
                        origin: (*keyword).to_owned(),
                        line: tree.position(span.start).line,
                    });
                }
                EffectOrigin::MethodCall { receiver, method, .. } => {
                    match resolve_effect_edge(classes, *class, receiver, method) {
                        Some(callee) => {
                            e.insert(callee);
                        }
                        // A method call we cannot resolve to a same-file target:
                        // its effects are unknown → non-exhaustive.
                        None => *ex = false,
                    }
                }
                // A structurally-dynamic call names its own uncertainty.
                EffectOrigin::Opaque { .. } => *ex = false,
            }
        }
    }

    // Fixpoint: effects(u) = direct(u) ∪ ⋃_{c ∈ edges(u)} effects(c), and
    // exhaustive(u) stays true only while every callee is exhaustive too. Both
    // are monotone over finite domains, so chaotic iteration converges.
    let syms: Vec<Sym> = units.iter().map(|(s, ..)| s.clone()).collect();
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

/// One line of the `annotate` effect margin: a function/method declaration with
/// its proven effect labels (sorted, deduplicated) and the exhaustiveness bit.
/// Public so the CLI (and infer's own tests) can read the fixpoint result for
/// *all* units, not only enveloped ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSummary {
    /// The declaration spelling: `f` for a function, `Foo::bar` for a method.
    pub symbol: String,
    /// 1-based line of the declaration (the name identifier).
    pub line: u32,
    /// Proven effect labels, sorted and deduplicated (ADR-0018 dot-paths).
    pub labels: Vec<String>,
    /// Whether the set is exhaustive (see [`EffectSet`]). `false` earns the `…?`
    /// marker: "at least these effects; not proven complete."
    pub exhaustive: bool,
}

/// The proven effect set of every concrete function and method in the file
/// (ADR-0005), for the `annotate` margin. Abstract methods (no body) are omitted
/// — there is nothing to prove about a declaration whose implementation lives in
/// an override. Reuses [`compute_effects`], so it agrees with the envelope check
/// by construction.
#[must_use]
pub fn effect_summary(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
) -> Vec<EffectSummary> {
    let effects = compute_effects(tree, functions, classes);
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
    for f in functions {
        let sym = Sym::Func(f.name.clone());
        out.push(EffectSummary {
            symbol: f.name.clone(),
            line: tree.position(f.span.start).line,
            labels: sorted_labels(&sym),
            exhaustive: exhaustive(&sym),
        });
    }
    for c in classes {
        for m in &c.methods {
            if m.is_abstract {
                continue; // no body — nothing proven
            }
            let sym = Sym::Method(c.name.clone(), m.name.clone());
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

fn effect_diagnostics(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
    path: &str,
) -> Vec<Diagnostic> {
    // Fast path: with no envelope anywhere in the file there is nothing to check.
    let any_envelope = functions.iter().any(|f| f.effect_envelope.is_some())
        || classes.iter().any(|c| c.methods.iter().any(|m| m.effect_envelope.is_some()));
    if !any_envelope {
        return Vec::new();
    }

    // The unified effect fixpoint for every function + method in the file (its
    // proven effect set and its exhaustiveness bit). Shared with `effect_summary`
    // (the annotate margin); here only `.findings` is read.
    let effects = compute_effects(tree, functions, classes);

    // Report: one declared envelope at a time, each proven origin in source order.
    let mut out = Vec::new();
    for f in functions {
        let Some(env) = &f.effect_envelope else { continue };
        report_unit(&mut out, tree, path, functions, classes, None, &f.name, env, &f.effect_origins, &effects);
    }
    for c in classes {
        for m in &c.methods {
            let Some(env) = &m.effect_envelope else { continue };
            let display = format!("{}::{}", c.name, m.name);
            report_unit(&mut out, tree, path, functions, classes, Some(&c.name), &display, env, &m.effect_origins, &effects);
        }
    }
    out
}

/// Emit the diagnostics for one declared-envelope unit (ADR-0005/0018):
///
/// 1. **`effect.unknown-label`** — one per declared label not in the registry,
///    reported at the attribute span (typos, unregistered private labels).
/// 2. **`effect.envelope-exceeded`** — walking the unit's own origins in source
///    order, each proven effect *not* subsumed by any declared label is a
///    violation (direct builtins/output/exit reported at the origin; same-file
///    function/method edges reported transitively with the ultimate origin named).
///    The empty envelope (`Pure`) is exceeded by every effect, reproducing the
///    pre-generalization behavior and message shape exactly.
#[allow(clippy::too_many_arguments)]
fn report_unit(
    out: &mut Vec<Diagnostic>,
    tree: &SourceTree,
    path: &str,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
    class: Option<&str>,
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
        let pos = tree.position(envelope.span.start);
        out.push(Diagnostic {
            id: UNKNOWN_LABEL_ID,
            path: path.to_owned(),
            line: pos.line,
            column: pos.column,
            message: msg,
        });
    }

    // 2. Envelope-exceeded violations (each effect not subsumed by the envelope).
    let labels = &envelope.labels;
    for origin in origins {
        match origin {
            EffectOrigin::Call { name, span } => {
                if let Some(canon) = user_fn_canon(functions, name) {
                    emit_transitive(out, tree, path, &Sym::Func(canon), effects, span.start, display, labels);
                } else {
                    for f in builtin_findings(name, *span, tree) {
                        if exceeds(labels, &f.label) {
                            let prefix = format!("{name}() has effect {}", f.label);
                            out.push(exceeded_diag(tree, path, span.start, &prefix, display, labels, &f.label));
                        }
                    }
                }
            }
            EffectOrigin::Output { keyword, span } if exceeds(labels, "output") => {
                let prefix = format!("{keyword} has effect output");
                out.push(exceeded_diag(tree, path, span.start, &prefix, display, labels, "output"));
            }
            EffectOrigin::Exit { keyword, span } if exceeds(labels, "exit") => {
                let prefix = format!("{keyword} has effect exit");
                out.push(exceeded_diag(tree, path, span.start, &prefix, display, labels, "exit"));
            }
            EffectOrigin::MethodCall { receiver, method, span } => {
                if let Some(callee) = resolve_effect_edge(classes, class, receiver, method) {
                    emit_transitive(out, tree, path, &callee, effects, span.start, display, labels);
                }
            }
            // Output / Exit subsumed by the envelope → silent.
            EffectOrigin::Output { .. } | EffectOrigin::Exit { .. } => {}
            // An unprovable call carries no proven effect — silent for the
            // envelope check (it only feeds the exhaustiveness bit).
            EffectOrigin::Opaque { .. } => {}
        }
    }
}

/// Emit each proven effect of `callee` *not subsumed by the envelope* as a
/// transitive violation, naming the ultimate origin.
#[allow(clippy::too_many_arguments)]
fn emit_transitive(
    out: &mut Vec<Diagnostic>,
    tree: &SourceTree,
    path: &str,
    callee: &Sym,
    effects: &HashMap<Sym, EffectSet>,
    offset: u32,
    display: &str,
    labels: &[String],
) {
    let callee_display = callee.display();
    let mut fs: Vec<&EffectFinding> =
        effects.get(callee).map(|e| &e.findings).into_iter().flatten().collect();
    fs.sort_by(|a, b| (a.line, &a.label, &a.origin).cmp(&(b.line, &b.label, &b.origin)));
    for ef in fs {
        if !exceeds(labels, &ef.label) {
            continue;
        }
        let prefix = format!(
            "{callee_display}() has effect {} (via {} at line {})",
            ef.label, ef.origin, ef.line
        );
        out.push(exceeded_diag(tree, path, offset, &prefix, display, labels, &ef.label));
    }
}

/// Whether an inferred `effect_label` **exceeds** the declared `labels`: a
/// violation iff no declared label subsumes it (ADR-0018). The empty envelope
/// (`Pure`) subsumes nothing, so every effect exceeds it.
fn exceeds(labels: &[String], effect_label: &str) -> bool {
    !labels.iter().any(|l| steins_catalog::subsumes(l, effect_label))
}

/// Build an `effect.envelope-exceeded` diagnostic. `prefix` names the effect and
/// its source (`rand() has effect nondet.random`); the tail names the declared
/// envelope — `#[\Steins\Pure]` for the empty set (the unchanged legacy shape),
/// else `#[\Steins\Effect('io')] — <label> exceeds the envelope`.
fn exceeded_diag(
    tree: &SourceTree,
    path: &str,
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
    effect_diag(tree, path, offset, msg)
}

/// The canonical declaration name of a same-file user function matching `name`
/// (case-insensitive; PHP function names are), or `None` for builtins/unknowns.
fn user_fn_canon(functions: &[FunctionDecl], name: &str) -> Option<String> {
    functions.iter().find(|f| f.name.eq_ignore_ascii_case(name)).map(|f| f.name.clone())
}

/// The proven effect findings a builtin `name` carries (empty for pure or
/// uncatalogued builtins — the silent side of proven-only checking).
fn builtin_findings(name: &str, span: steins_syntax::Span, tree: &SourceTree) -> Vec<EffectFinding> {
    match steins_catalog::effect_labels(name) {
        Some(labels) => {
            let line = tree.position(span.start).line;
            labels
                .iter()
                .map(|&label| EffectFinding {
                    label: label.to_owned(),
                    origin: name.to_owned(),
                    line,
                })
                .collect()
        }
        None => Vec::new(),
    }
}

/// Resolve a method-call effect origin to the unit it edges to, under the same
/// sound dispatch rules the type check uses (no flow environment, so only
/// `$this`/`self`/`parent`/`Foo::`/`new Foo()->` receivers resolve). `$this` and
/// `self::` require the final/private override guard; `parent::`/`Foo::` are
/// exact.
fn resolve_effect_edge(
    classes: &[ClassDecl],
    enclosing: Option<&str>,
    receiver: &EffectRecv,
    method: &str,
) -> Option<Sym> {
    let (start, exact) = match receiver {
        EffectRecv::This | EffectRecv::SelfKw => (enclosing?.to_owned(), false),
        EffectRecv::Parent => (find_class(classes, enclosing?)?.parent.clone()?, true),
        EffectRecv::ClassName(name) => (name.clone(), true),
    };
    let Resolution::Found(r) = resolve_in_chain(classes, &start, method) else { return None };
    // A private method is callable only from within its declaring class.
    if r.method.visibility == Visibility::Private
        && !enclosing.is_some_and(|e| e.eq_ignore_ascii_case(r.declaring_class))
    {
        return None;
    }
    if !exact {
        let declaring_final = find_class(classes, r.declaring_class).is_some_and(|c| c.is_final);
        if !(r.method.is_final || r.method.visibility == Visibility::Private || declaring_final) {
            return None; // a non-final public method may be overridden elsewhere
        }
    }
    Some(Sym::Method(r.declaring_class.to_owned(), r.method.name.clone()))
}

/// Build an `effect.envelope-exceeded` diagnostic at `offset` with `message`.
fn effect_diag(tree: &SourceTree, path: &str, offset: u32, message: String) -> Diagnostic {
    let pos = tree.position(offset);
    Diagnostic { id: EFFECT_ID, path: path.to_owned(), line: pos.line, column: pos.column, message }
}

/// Read-only analysis context threaded through the propagation pass.
struct Cx<'a> {
    tree: &'a SourceTree,
    functions: &'a [FunctionDecl],
    classes: &'a [ClassDecl],
    path: &'a str,
    strict: bool,
}

/// A proven local value plus where it was established (for provenance).
struct Known {
    value: ArgValue,
    /// 1-based line of the assignment that established the value.
    line: u32,
    /// When the value came from an interprocedural argument binding (Feature B),
    /// the provenance tail naming the outer binding call site
    /// (`bound at outer("abc") call on line N`). `None` for an ordinary
    /// same-scope assignment, whose provenance is derived from `line` instead.
    bound: Option<String>,
}

/// A binding-descent key: the callee name plus its bound parameters (sorted by
/// name), identifying a `(function, binding)` frame for recursion detection and
/// memoization (Feature B).
type BindingKey = (String, Vec<(String, ArgValue)>);

/// The state threaded down an interprocedural binding descent (Feature B).
struct Descent<'a> {
    /// The provenance tail naming the **first** (outermost) binding call site,
    /// e.g. `bound at outer("abc") call on line 9`. Fixed for the whole descent
    /// so every finding, however deep, names the site that started the chain.
    provenance: &'a str,
    /// Current descent depth (the first binding is depth 1).
    depth: usize,
    /// `(function, binding)` frames currently on the descent stack — a revisit
    /// is direct/indirect recursion and stops the descent.
    stack: &'a mut Vec<BindingKey>,
    /// `(function, binding)` frames already fully analyzed in this descent —
    /// collapses diamonds without re-walking.
    memo: &'a mut HashSet<BindingKey>,
}

/// Walk one scope's trace with a given initial environment, tracking known local
/// values, checking every call, and attempting interprocedural binding descent.
///
/// `env` is empty for a scope's own top-level walk and pre-loaded with bound
/// parameters for a binding descent; `descent` is `None` at the top level and
/// `Some` inside a descent (carrying the budget/recursion/provenance state).
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
    // The class that lexically encloses this scope (a method body), for resolving
    // `$this->`, `self::`, and `parent::` calls; `None` for functions/top-level.
    let enclosing_class = scope_class(scope);

    for stmt in &scope.stmts {
        // 1. Check + descend every statically-named call this statement carries,
        // against the env as it stands *before* the statement's own effect.
        for call in checkable_calls(&stmt.kind) {
            match &call.receiver {
                // Function calls keep the exact function-world behavior.
                Callee::Function(_) => {
                    check_propagated_call(cx, folder, scope.poisoned, call, &env, out);
                    try_descend_function(cx, folder, call, &env, scope.poisoned, descent.as_mut(), out);
                }
                // Method / static / constructor calls: the class-world path.
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
            // `echo` assigns nothing, but stays conservative like the former
            // Barrier: clear afterward (the calls were already checked in step 1).
            StmtKind::Barrier | StmtKind::Echo(_) => {
                env.clear();
                classes_env.clear();
            }
            // ADR-0027 ratchet: forget only the construct's write set (unless it
            // poisons, in which case it behaves exactly like a Barrier). A write
            // to `$x` is a reassignment, so it drops `$x`'s exact-class fact too.
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
                let line = cx.tree.position(span.start).line;
                match value {
                    // `$x = new Foo(...)` — record `$x`'s exact class and drop any
                    // stale scalar fact. The fact holds only until `$x` is handed to
                    // a call (a by-ref parameter can rebind it; see step 3). A
                    // poisoned scope trusts nothing.
                    ArgValue::New(class, _) => {
                        env.remove(var);
                        if scope.poisoned {
                            classes_env.remove(var);
                        } else {
                            classes_env.insert(var.clone(), class.clone());
                            if let Some(facts) = facts.as_deref_mut() {
                                facts.push(LineFact {
                                    line,
                                    kind: FactKind::ExactClass {
                                        var: var.clone(),
                                        class: class.clone(),
                                    },
                                });
                            }
                        }
                    }
                    _ => match cx.resolve_literal(value, &env, scope.poisoned, folder) {
                        Some(lit) => {
                            if let Some(facts) = facts.as_deref_mut() {
                                facts.push(LineFact {
                                    line,
                                    kind: FactKind::Value {
                                        var: var.clone(),
                                        rendered: lit.render(),
                                    },
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

        // 3. After the statement, any variable handed to a call is untrustworthy
        // — a by-ref parameter can *rebind* it to a different object
        // (`f(&$x) { $x = new Bar(); }`), which changes the variable's class, not
        // just its scalar value. So invalidation drops the exact-class fact too,
        // exactly as it drops a scalar literal fact. (The receiver of `$x->m()` is
        // *not* listed here — a method call cannot rebind the caller's variable —
        // so a method call on `$x` keeps `$x`'s class fact; and a direct
        // `(new Foo())->m()` has no variable to rebind, so that path stays exact.)
        for v in &stmt.invalidated {
            env.remove(v);
            classes_env.remove(v);
        }
    }
}

/// The class that lexically owns a method scope, for resolving `$this`/`self`/
/// `parent` inside it; `None` for function and top-level scopes.
fn scope_class(scope: &Scope) -> Option<&str> {
    match &scope.owner {
        ScopeOwner::Method { class, .. } => Some(class),
        ScopeOwner::TopLevel | ScopeOwner::Function(_) => None,
    }
}

/// The statically-named calls a statement carries that must be checked and
/// descended against the env at the statement's start: the statement-level call,
/// a `return f(...)` / `$x = f(...)` right-hand call, or each `echo f(...)`
/// operand. Calls nested inside control-flow bodies are deliberately excluded —
/// they run under a different (post-assignment) env and stay `Opaque`.
fn checkable_calls(kind: &StmtKind) -> Vec<&CallExpr> {
    match kind {
        StmtKind::Call(c) => vec![c],
        StmtKind::Return { call: Some(c), .. } | StmtKind::Assign { call: Some(c), .. } => vec![c],
        StmtKind::Echo(cs) => cs.iter().collect(),
        _ => Vec::new(),
    }
}

/// Check a call whose arguments may be propagated values (`Var` / `Call`).
fn check_propagated_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(decl) = resolve_callee(cx.functions, call) else { return };

    for (i, arg) in call.args.iter().enumerate() {
        let Some(ty) = param_scalar_type(&decl.params, i) else {
            if arg_binds_to_variadic(&decl.params, i) {
                break;
            }
            continue;
        };

        // Only `Var` and `Call` arguments — literals belong to the direct pass.
        let resolved: Option<(ArgValue, String)> = match &arg.value {
            ArgValue::Var(name) if !poisoned => env.get(name).map(|k| {
                // A bound parameter names the outer binding site; a plain local
                // assignment names its own line.
                let prov = match &k.bound {
                    Some(b) => format!("from ${name}, {b}"),
                    None => format!("from ${name}, assigned at line {}", k.line),
                };
                (k.value.clone(), prov)
            }),
            ArgValue::Call(name, args) => {
                // A zero-arg same-file constant function wins; otherwise try to
                // fold an allowlisted builtin over literal args.
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

        if is_type_error(cx.strict, ty, &value) {
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

/// Attempt an interprocedural argument-binding descent into a same-file
/// **function** (Feature B). Thin wrapper over [`descend`].
fn try_descend_function(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(decl) = resolve_callee(cx.functions, call) else { return };
    let Some(callee_scope) = cx.unique_scope(&decl.name) else { return };
    descend(
        cx,
        folder,
        &decl.params,
        callee_scope,
        &decl.name,
        &decl.name,
        None,
        call,
        env,
        poisoned,
        descent,
        out,
    );
}

/// Interprocedural argument-binding descent into a resolved callee body (Feature
/// B), shared by the function and method paths. When one or more positional
/// arguments of `call` resolve to literals, `callee_scope` is re-analyzed with
/// those `params` bound to their post-coercion values; any proven
/// `type.argument-mismatch` inside is reported at the inner call site with a
/// provenance chain naming the outermost binding site. Zero-FP rules (entry
/// coercion, by-ref skip, depth/recursion budget) are enforced here.
///
/// `key_name` uniquely identifies the callee for recursion/memoization
/// (`"func"` or `"Class::method"`); `display_name` is the provenance render base
/// (`"width"`, `"Foo::m"`, `"new Foo"`); `body_this_exact` is the exact `$this`
/// class the callee body runs with (`Some` iff dispatched on an exact receiver),
/// so nested `$this->…` inside resolves under the exact-class rule.
#[allow(clippy::too_many_arguments)]
fn descend(
    cx: &Cx,
    folder: &mut dyn Folder,
    params: &[Param],
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

    // Resolve each positional argument to a literal and try to bind it.
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
        // A by-ref parameter in a bound position: skip the whole binding.
        if param.by_ref {
            return;
        }
        let Some(ty) = param.ty else {
            bound.push((param.name.clone(), value));
            continue;
        };
        match coerce_into_param(cx.strict, ty, &value) {
            Some(coerced) => bound.push((param.name.clone(), coerced)),
            None => return, // entry TypeError (reported at outer site) or unsure.
        }
    }

    if bound.is_empty() {
        return;
    }

    // Canonical `(callee, binding)` key for recursion detection / memoization.
    let mut key_binding = bound.clone();
    key_binding.sort_by(|a, b| a.0.cmp(&b.0));
    let key: BindingKey = (key_name.to_owned(), key_binding);

    // Provenance names the *first* binding site; a nested descent inherits it.
    let new_provenance;
    let (provenance, next_depth): (&str, usize) = match &descent {
        Some(d) => (d.provenance, d.depth + 1),
        None => {
            let line = cx.tree.position(call.span.start).line;
            new_provenance = format!(
                "bound at {} call on line {}",
                render_call(display_name, &render_args),
                line
            );
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

    match descent {
        Some(d) => {
            if d.stack.contains(&key) || d.memo.contains(&key) {
                return;
            }
            d.stack.push(key.clone());
            let child = Descent { provenance, depth: next_depth, stack: d.stack, memo: d.memo };
            analyze_scope(
                cx,
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
            let child =
                Descent { provenance, depth: next_depth, stack: &mut stack, memo: &mut memo };
            analyze_scope(
                cx,
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
// Class-world method resolution (ADR-0001 sound dispatch).
// ---------------------------------------------------------------------------

/// A method resolved through a same-file inheritance chain.
struct ResolvedMethod<'a> {
    method: &'a MethodDecl,
    /// The class in the chain that declares the resolved method.
    declaring_class: &'a str,
}

/// The outcome of walking a class's same-file inheritance chain for a method.
enum Resolution<'a> {
    /// A concrete, in-file method body was found.
    Found(ResolvedMethod<'a>),
    /// The whole chain is in-file and ends at a root with no such method (for a
    /// constructor this means PHP's default constructor — no args checked).
    NotFoundChainComplete,
    /// The chain left the file, hit a trait-using class, or hit an abstract
    /// method whose concrete body lives in an out-of-file subclass — give up.
    Unknown,
}

/// Walk `start_class`'s same-file inheritance chain looking for a concrete
/// `method`, most-derived first. Gives up (`Unknown`) the instant the chain
/// leaves the file or reaches a trait-using class — the FP-safe boundary.
fn resolve_in_chain<'a>(
    classes: &'a [ClassDecl],
    start_class: &str,
    method: &str,
) -> Resolution<'a> {
    let mut cur = start_class.to_owned();
    // Guard against pathological cyclic `extends` (illegal PHP, but be safe).
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Resolution::Unknown;
        }
        let Some(cd) = find_class(classes, &cur) else {
            return Resolution::Unknown; // chain leaves the file
        };
        if cd.uses_traits {
            return Resolution::Unknown; // trait bodies live elsewhere
        }
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
            return if m.is_abstract {
                // A concrete override must be in an out-of-file subclass.
                Resolution::Unknown
            } else {
                Resolution::Found(ResolvedMethod { method: m, declaring_class: &cd.name })
            };
        }
        match &cd.parent {
            None => return Resolution::NotFoundChainComplete,
            Some(p) => cur = p.clone(),
        }
    }
}

/// The first same-file class named `name` (case-insensitive).
fn find_class<'a>(classes: &'a [ClassDecl], name: &str) -> Option<&'a ClassDecl> {
    classes.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// A resolved call target: the method to check/descend, plus the exact `$this`
/// class its body runs with (`Some` iff dispatched on an exact receiver).
struct CallTarget<'a> {
    method: &'a MethodDecl,
    declaring_class: &'a str,
    this_exact: Option<String>,
}

/// Resolve a method/static/constructor `receiver` to a same-file target under
/// the sound dispatch rules, or `None` (silent) when dispatch is uncertain, the
/// chain leaves the file, or the call would be a PHP fatal we do not report.
fn resolve_call_target<'a>(
    cx: &'a Cx,
    receiver: &Callee,
    classes_env: &HashMap<String, String>,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<CallTarget<'a>> {
    match receiver {
        // `new Foo(...)` constructs exactly Foo → exact resolution of __construct.
        Callee::Construct { class } => {
            resolve_exact(cx, class, "__construct", enclosing_class, Some(class.clone()))
        }
        // `(new Foo())->m()` — exact receiver.
        Callee::Method { receiver: Receiver::New(class), method } => {
            resolve_exact(cx, class, method, enclosing_class, Some(class.clone()))
        }
        // `$x->m()` — exact only when the env knows `$x`'s class.
        Callee::Method { receiver: Receiver::Var(v), method } => {
            if poisoned {
                return None;
            }
            let class = classes_env.get(v)?;
            resolve_exact(cx, class, method, enclosing_class, Some(class.clone()))
        }
        // `$this->m()` — exact when `$this` is known exactly (descent), else the
        // final/private guard (a subclass override could receive the call).
        Callee::Method { receiver: Receiver::This, method } => {
            let enclosing = enclosing_class?;
            match this_exact {
                Some(exact) => {
                    resolve_exact(cx, exact, method, enclosing_class, Some(exact.to_owned()))
                }
                None => resolve_guarded(cx, enclosing, method, enclosing_class),
            }
        }
        // `self::m()` — conservative final/private guard (early-bound in PHP, but
        // the guard is only ever stricter, so it stays sound).
        Callee::Static { class: StaticClass::SelfKw, method } => {
            let enclosing = enclosing_class?;
            resolve_guarded(cx, enclosing, method, enclosing_class)
        }
        // `parent::m()` — the parent chain, exact (parent is fixed at compile time).
        Callee::Static { class: StaticClass::Parent, method } => {
            let parent = find_class(cx.classes, enclosing_class?)?.parent.as_deref()?;
            resolve_static_named(cx, parent, method, enclosing_class)
        }
        // `Foo::m()` — explicit class, exact (a subclass override only affects
        // `static::`, never `Foo::`).
        Callee::Static { class: StaticClass::Named(name), method } => {
            resolve_static_named(cx, name, method, enclosing_class)
        }
        // `static::m()` — late static binding, always unknown.
        Callee::Static { class: StaticClass::Static, .. } => None,
        Callee::Function(_) | Callee::Dynamic => None,
    }
}

/// Resolve an exact-receiver instance call (`new Foo()->m()`, `$x->m()` with a
/// known class, exact `$this->m()`): any override-uncertainty is absent, so no
/// final guard — only the private-visibility skip applies.
fn resolve_exact<'a>(
    cx: &'a Cx,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
    this_exact: Option<String>,
) -> Option<CallTarget<'a>> {
    match resolve_in_chain(cx.classes, class, method) {
        Resolution::Found(r) if !private_blocked(&r, enclosing_class) => {
            Some(CallTarget { method: r.method, declaring_class: r.declaring_class, this_exact })
        }
        _ => None,
    }
}

/// Resolve a `$this->`/`self::` call under the override guard: resolvable only
/// when the found method is `private`, `final`, or its declaring class is
/// `final` (else a subclass override elsewhere could receive the call). The body
/// runs with an *open* `$this` (a subclass instance), so `this_exact` is `None`.
fn resolve_guarded<'a>(
    cx: &'a Cx,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx.classes, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    let declaring_final = find_class(cx.classes, r.declaring_class).is_some_and(|c| c.is_final);
    let final_or_private =
        r.method.is_final || r.method.visibility == Visibility::Private || declaring_final;
    if !final_or_private {
        return None; // a non-final public method may be overridden elsewhere.
    }
    Some(CallTarget { method: r.method, declaring_class: r.declaring_class, this_exact: None })
}

/// Resolve an explicit `Foo::m()` / `parent::m()` static call (exact). Guards
/// against the PHP-fatal case of calling a non-static instance method statically
/// from *outside* any class context (where the fatal would mask our finding).
fn resolve_static_named<'a>(
    cx: &'a Cx,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx.classes, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    // A non-static method called statically from outside a class is a fatal.
    if !r.method.is_static && enclosing_class.is_none() {
        return None;
    }
    Some(CallTarget { method: r.method, declaring_class: r.declaring_class, this_exact: None })
}

/// Whether a resolved `private` method is invisible at the call site (a private
/// method is callable only from within its declaring class). Non-private
/// methods are never blocked here.
fn private_blocked(r: &ResolvedMethod, enclosing_class: Option<&str>) -> bool {
    r.method.visibility == Visibility::Private
        && !enclosing_class.is_some_and(|e| e.eq_ignore_ascii_case(r.declaring_class))
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
    // Named/spread arguments make positional mapping unreliable — skip.
    if !call.positional_only {
        return;
    }
    let Some(target) = resolve_call_target(
        cx,
        &call.receiver,
        classes_env,
        this_exact,
        enclosing_class,
        scope.poisoned,
    ) else {
        return;
    };

    let callee_name = diag_callee_name(&call.receiver, target.declaring_class, &target.method.name);

    // 1. Check the arguments passed *at this call site* (literals + resolved
    // `$var`/const-fn/folded values) against the resolved method's params.
    check_method_args(cx, folder, target.method, &callee_name, call, env, scope.poisoned, out);

    // 2. Descend into the method body with the params bound and the exact `$this`
    // class (when known) so nested `$this->…` resolves.
    let Some(callee_scope) = cx.method_scope(target.declaring_class, &target.method.name) else {
        return;
    };
    let display = display_of_call(&call.receiver, target.declaring_class, &target.method.name);
    descend(
        cx,
        folder,
        &target.method.params,
        callee_scope,
        &format!("{}::{}", target.declaring_class, target.method.name),
        &display,
        target.this_exact,
        call,
        env,
        scope.poisoned,
        descent,
        out,
    );
}

/// The callee spelling for a `type.argument-mismatch` message: `Class::method`
/// (constructors render as `Class::__construct`), where `Class` is the class
/// that actually declares the resolved method.
fn diag_callee_name(_receiver: &Callee, declaring_class: &str, method: &str) -> String {
    format!("{declaring_class}::{method}")
}

/// The provenance render base for a bound method/constructor call: `new Foo` for
/// a constructor (so `render_call` yields `new Foo("abc")`), else `Class::method`.
fn display_of_call(receiver: &Callee, declaring_class: &str, method: &str) -> String {
    match receiver {
        Callee::Construct { class } => format!("new {class}"),
        _ => format!("{declaring_class}::{method}"),
    }
}

/// Check the arguments of a resolved method/constructor call at its call site.
/// Unlike the function direct pass, this covers **literal** arguments too (no
/// separate direct pass reaches method calls). Non-literal resolved values
/// (notably `new` receivers) never flow into a scalar type check.
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
        let Some((value, prov)) = resolved else { continue };
        // Only concrete scalar literals are ever checked (a `new` receiver etc.
        // resolves to a non-literal and is skipped — object→scalar is out of scope).
        if !value.is_literal() {
            continue;
        }
        if is_type_error(cx.strict, ty, &value) {
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

impl Cx<'_> {
    /// Resolve an [`ArgValue`] to a concrete literal, if this slice can prove
    /// one: a bare literal, a currently-known variable, or a zero-argument call
    /// to a same-file constant function. `poisoned` disables variable resolution
    /// entirely (nothing is ever known in a poisoned scope).
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

    /// Try to fold an allowlisted builtin call over literal arguments, returning
    /// the folded literal and its provenance string (`folded from f("x")`).
    ///
    /// The gate (ADR-0004 / ADR-0008): the callee must NOT be a same-file user
    /// function, [`steins_catalog::foldable`] must permit it, and every argument
    /// must already be a literal the IR carries. Only then is the `folder` asked.
    fn try_fold(
        &self,
        name: &str,
        args: &[ArgValue],
        folder: &mut dyn Folder,
    ) -> Option<(ArgValue, String)> {
        // A same-file user function is never folded via the sidecar (the const
        // function path handles the zero-arg case; anything else is unknown).
        if self.functions.iter().any(|f| f.name == name) {
            return None;
        }
        if !steins_catalog::foldable(name) {
            return None;
        }
        // Inner arguments must be literals directly — we do not resolve nested
        // variables here (keeps the gate simple and the fold self-contained).
        if !args.iter().all(ArgValue::is_literal) {
            return None;
        }
        let folded = folder.fold(name, args)?;
        Some((folded, format!("folded from {}", render_call(name, args))))
    }

    /// The unique body scope of the same-file user function `name`, or `None`
    /// when there is no such scope or more than one (ambiguous → give up).
    fn unique_scope(&self, name: &str) -> Option<&'_ Scope> {
        let mut it =
            self.tree.scopes().iter().filter(|s| s.function_name.as_deref() == Some(name));
        let scope = it.next()?;
        if it.next().is_some() { None } else { Some(scope) }
    }

    /// Resolve a zero-argument same-file constant function: its body must be
    /// exactly `[Return(literal)]`, it must be unambiguous, take no parameters,
    /// and its scope must not be poisoned. Returns the literal and the function's
    /// definition line.
    fn resolve_const_fn(&self, name: &str) -> Option<(ArgValue, u32)> {
        // Unique declaration, zero parameters.
        let mut decls = self.functions.iter().filter(|f| f.name == name);
        let decl = decls.next()?;
        if decls.next().is_some() || !decl.params.is_empty() {
            return None;
        }
        // Unique scope for this function.
        let mut scopes =
            self.tree.scopes().iter().filter(|s| s.function_name.as_deref() == Some(name));
        let scope = scopes.next()?;
        if scopes.next().is_some() || scope.poisoned {
            return None;
        }
        // Body is exactly one `return <literal>;`.
        let [stmt] = scope.stmts.as_slice() else { return None };
        let StmtKind::Return { value, .. } = &stmt.kind else { return None };
        if !value.is_literal() {
            return None;
        }
        Some((value.clone(), self.tree.position(decl.span.start).line))
    }

    /// The unique method body scope for `class::method` (case-insensitive), or
    /// `None` if absent or ambiguous.
    fn method_scope(&self, class: &str, method: &str) -> Option<&'_ Scope> {
        let mut it = self.tree.scopes().iter().filter(|s| {
            matches!(&s.owner, ScopeOwner::Method { class: c, method: m }
                if c.eq_ignore_ascii_case(class) && m.eq_ignore_ascii_case(method))
        });
        let scope = it.next()?;
        if it.next().is_some() { None } else { Some(scope) }
    }

    /// Build a `type.argument-mismatch` diagnostic for a call to `callee`'s
    /// parameter `param_name`. With `provenance`, the message names the value's
    /// origin hop; without, it is the direct-literal message (byte-for-byte
    /// identical to the pre-propagation output).
    #[allow(clippy::too_many_arguments)]
    fn diagnostic(
        &self,
        offset: u32,
        value: &ArgValue,
        provenance: Option<&str>,
        callee: &str,
        param_name: &str,
        ty: ParamType,
    ) -> Diagnostic {
        let pos = self.tree.position(offset);
        let mode = if self.strict { "strict" } else { "coercive" };
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
        Diagnostic {
            id: ID,
            path: self.path.to_owned(),
            line: pos.line,
            column: pos.column,
            message,
        }
    }
}

/// Render a call with its literal arguments for a folding provenance string,
/// e.g. `strtolower("ABC")` or `str_repeat("ab", 3)`.
fn render_call(name: &str, args: &[ArgValue]) -> String {
    let inner: Vec<String> = args.iter().map(ArgValue::render).collect();
    format!("{name}({})", inner.join(", "))
}

/// Resolve a call's callee to the *unique* same-file user function, honoring the
/// positional-only requirement. Ambiguity or a dynamic callee → `None`.
fn resolve_callee<'a>(functions: &'a [FunctionDecl], call: &CallExpr) -> Option<&'a FunctionDecl> {
    if !call.positional_only {
        return None;
    }
    let callee = call.callee.as_deref()?;
    let mut matches = functions.iter().filter(|f| f.name == callee);
    let decl = matches.next()?;
    if matches.next().is_some() {
        return None; // redeclaration; not our call to make.
    }
    Some(decl)
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

/// Whether argument `i` binds to a variadic parameter (so it, and every later
/// argument, must be skipped).
fn arg_binds_to_variadic(params: &[Param], i: usize) -> bool {
    params.get(i).is_some_and(|p| p.variadic)
}

/// The truth table: does passing `arg` to a parameter of type `ty` provably
/// raise a `TypeError` under PHP 8.1+ (given `strict` = `declare(strict_types=1)`)?
///
/// The bar is *provable breakage*. When unsure, return `false` (silent).
fn is_type_error(strict: bool, ty: ParamType, arg: &ArgValue) -> bool {
    // `null` is special and mode-independent: it satisfies a nullable param and
    // otherwise always errors — userland functions never coerce `null`.
    if matches!(arg, ArgValue::Null) {
        return !ty.nullable;
    }

    if strict {
        // Strict mode: no scalar coercion, with the single exception of int→float.
        match ty.scalar {
            ScalarType::Int => !matches!(arg, ArgValue::Int(_)),
            ScalarType::Float => !matches!(arg, ArgValue::Int(_) | ArgValue::Float(_)),
            ScalarType::String => !matches!(arg, ArgValue::Str(_)),
            ScalarType::Bool => !matches!(arg, ArgValue::Bool(_)),
        }
    } else {
        // Coercive mode: the only literal TypeErrors are non-numeric strings into
        // a numeric (int|float) parameter. Everything else coerces silently.
        match ty.scalar {
            ScalarType::Int | ScalarType::Float => match arg {
                ArgValue::Str(s) => !php_is_numeric(s),
                _ => false,
            },
            ScalarType::String | ScalarType::Bool => false,
        }
    }
}

/// The value a parameter of type `ty` actually holds when `value` is passed to
/// it under `strict`, or `None` when the pass would fatal at entry (a TypeError
/// already reported at the outer site by the direct/propagation check) **or**
/// when the coercion is one this slice is not certain about (silence is safe —
/// ADR-0002 zero-FP).
///
/// This is Feature B's descend-value computation: only when a bound literal
/// *passes* the callee's entry check do we analyze its body, and we do so with
/// the post-coercion value (`"5"` into an int parameter becomes int `5` in
/// coercive mode; under strict it would have fataled, so we never reach here).
///
/// The table is deliberately partial. Value precision only ever affects a
/// downstream finding through the numeric-string-into-numeric rule, so the one
/// risk is producing a string whose numericness we get wrong. We therefore emit
/// only strings we can render exactly (`int`/`bool`→`string`) and decline
/// `float`→`string` (PHP's rendering depends on the `precision` ini) by widening
/// to `None`. `int`/`float`/`bool` descend values never trigger a downstream
/// coercive TypeError, so their exact magnitude is immaterial.
fn coerce_into_param(strict: bool, ty: ParamType, value: &ArgValue) -> Option<ArgValue> {
    // Entry check first: a value that fatals never reaches the callee's body.
    if is_type_error(strict, ty, value) {
        return None;
    }
    // `null` into a nullable parameter stays `null` (non-nullable already
    // rejected by the entry check above).
    if matches!(value, ArgValue::Null) {
        return Some(ArgValue::Null);
    }
    Some(match (ty.scalar, value) {
        // Identity: the value already matches the target scalar.
        (ScalarType::Int, ArgValue::Int(_))
        | (ScalarType::Float, ArgValue::Float(_))
        | (ScalarType::String, ArgValue::Str(_))
        | (ScalarType::Bool, ArgValue::Bool(_)) => value.clone(),

        // int -> float widening (permitted in both modes).
        (ScalarType::Float, ArgValue::Int(i)) => ArgValue::Float(*i as f64),

        // The rest are coercive-only (strict would have fataled at entry):
        // numeric string -> int / float.
        (ScalarType::Int, ArgValue::Str(s)) => ArgValue::Int(php_str_to_int(s)?),
        (ScalarType::Float, ArgValue::Str(s)) => ArgValue::Float(php_str_to_float(s)?),
        // float / bool -> int.
        (ScalarType::Int, ArgValue::Float(f)) => ArgValue::Int(php_float_to_int(*f)?),
        (ScalarType::Int, ArgValue::Bool(b)) => ArgValue::Int(i64::from(*b)),
        // bool -> float.
        (ScalarType::Float, ArgValue::Bool(b)) => ArgValue::Float(if *b { 1.0 } else { 0.0 }),
        // -> bool (well-defined truthiness).
        (ScalarType::Bool, ArgValue::Int(i)) => ArgValue::Bool(*i != 0),
        (ScalarType::Bool, ArgValue::Float(f)) => ArgValue::Bool(*f != 0.0),
        (ScalarType::Bool, ArgValue::Str(s)) => ArgValue::Bool(!(s.is_empty() || s == "0")),
        // int / bool -> string (rendered exactly).
        (ScalarType::String, ArgValue::Int(i)) => ArgValue::Str(i.to_string()),
        (ScalarType::String, ArgValue::Bool(b)) => {
            ArgValue::Str(if *b { "1".to_owned() } else { String::new() })
        }

        // Anything else — notably float -> string — is uncertain: widen.
        _ => return None,
    })
}

/// Whitespace PHP trims before interpreting a numeric string (matches
/// [`php_is_numeric`]).
fn php_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'))
}

/// Convert a PHP *numeric string* (already validated by [`php_is_numeric`]) to
/// the int it coerces to: integer form parses directly, float form truncates
/// toward zero. `None` only on the unreachable non-numeric path.
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

/// Truncate a float toward zero to an int (PHP scalar coercion). Non-finite
/// floats have no well-defined int and widen to `None`.
fn php_float_to_int(f: f64) -> Option<i64> {
    f.is_finite().then(|| f.trunc() as i64)
}

/// PHP 8 `is_numeric` semantics: optional leading/trailing whitespace, optional
/// sign, decimal integer or float with optional exponent. Hex, `inf`, and `nan`
/// are *not* numeric strings.
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
