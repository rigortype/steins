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
    ArgValue, CallExpr, FunctionDecl, ParamType, ScalarType, Scope, SourceTree, StmtKind,
};

/// The registry id for the one check this crate emits (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

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
        ArgValue::Var(_) | ArgValue::Call(..) | ArgValue::Other => None,
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
    let cx = Cx { tree, functions, path, strict: tree.has_strict_types() };
    let mut out = Vec::new();

    // --- Direct pass: literal arguments at every call site. --------------
    for call in tree.calls() {
        let Some(decl) = resolve_callee(functions, call) else { continue };
        for (i, arg) in call.args.iter().enumerate() {
            let Some(ty) = param_scalar_type(decl, i) else {
                if arg_binds_to_variadic(decl, i) {
                    break;
                }
                continue;
            };
            // Only literals here; `Var`/`Call` are the propagation pass's job.
            if !arg.value.is_literal() {
                continue;
            }
            if is_type_error(cx.strict, ty, &arg.value) {
                out.push(cx.diagnostic(arg.span.start, &arg.value, None, decl, i, ty));
            }
        }
    }

    // --- Propagation pass: resolved `$var` / constant-return / folded args. ---
    for scope in tree.scopes() {
        analyze_scope(&cx, folder, scope, HashMap::new(), None, &mut out);
    }

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

/// Read-only analysis context threaded through the propagation pass.
struct Cx<'a> {
    tree: &'a SourceTree,
    functions: &'a [FunctionDecl],
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
fn analyze_scope(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    mut env: HashMap<String, Known>,
    mut descent: Option<Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    for stmt in &scope.stmts {
        // 1. Check + descend every statically-named call this statement carries,
        // against the env as it stands *before* the statement's own effect. This
        // uniformly covers statement-level calls, `return f(...)`, `$x = f(...)`,
        // and `echo f(...)` — all evaluate under the entry env in straight line.
        for call in checkable_calls(&stmt.kind) {
            check_propagated_call(cx, folder, scope.poisoned, call, &env, out);
            try_descend(cx, folder, call, &env, scope.poisoned, descent.as_mut(), out);
        }

        // 2. Apply the statement's own effect on the known-value environment.
        match &stmt.kind {
            // `echo` assigns nothing, but stays conservative like the former
            // Barrier: clear afterward (the calls were already checked in step 1).
            StmtKind::Barrier | StmtKind::Echo(_) => env.clear(),
            // ADR-0027 ratchet: forget only the construct's write set (unless it
            // poisons, in which case it behaves exactly like a Barrier).
            StmtKind::Opaque { writes, poisons } => {
                if *poisons {
                    env.clear();
                } else {
                    for w in writes {
                        env.remove(w);
                    }
                }
            }
            StmtKind::Return { .. } | StmtKind::Call(_) => {}
            StmtKind::Assign { var, value, span, .. } => {
                // A poisoned scope never trusts a variable value.
                match cx.resolve_literal(value, &env, scope.poisoned, folder) {
                    Some(lit) => {
                        let line = cx.tree.position(span.start).line;
                        env.insert(var.clone(), Known { value: lit, line, bound: None });
                    }
                    None => {
                        env.remove(var);
                    }
                }
            }
        }

        // 3. After the statement, any variable handed to a call is untrustworthy
        // (a by-ref parameter could have mutated it).
        for v in &stmt.invalidated {
            env.remove(v);
        }
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
        let Some(ty) = param_scalar_type(decl, i) else {
            if arg_binds_to_variadic(decl, i) {
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
            out.push(cx.diagnostic(arg.span.start, &value, Some(&provenance), decl, i, ty));
        }
    }
}

/// Attempt an interprocedural argument-binding descent for one call (Feature B).
///
/// When `call` targets a same-file, non-poisoned user function and one or more
/// positional arguments resolve to literals, the callee's body is re-analyzed
/// with those parameters bound to their (post-coercion) values. Any proven
/// `type.argument-mismatch` inside is reported at the inner call site with a
/// provenance chain naming the outermost binding site. Zero-FP rules from the
/// slice's spec are enforced here (see inline notes).
fn try_descend(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(decl) = resolve_callee(cx.functions, call) else { return };
    // The callee must have a unique, non-poisoned body scope to analyze.
    let Some(callee_scope) = cx.unique_scope(&decl.name) else { return };
    if callee_scope.poisoned {
        return;
    }

    // Resolve each positional argument to a literal and try to bind it.
    let mut bound: Vec<(String, ArgValue)> = Vec::new();
    let mut render_args: Vec<ArgValue> = Vec::new();
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = decl.params.get(i) else { break };
        // Variadic parameter: it and everything after stays unbound.
        if param.variadic {
            break;
        }
        // Resolve the argument value (literal / known var / const-fn / fold).
        let Some(value) = cx.resolve_literal(&arg.value, env, poisoned, folder) else {
            // Unknown argument (or a default with no argument): leave unbound.
            continue;
        };
        render_args.push(value.clone());
        // A by-ref parameter in a bound position: skip the whole binding — its
        // in-callee value is not determined by the caller's literal.
        if param.by_ref {
            return;
        }
        // The callee's declared type acts first. If the literal already violates
        // it, the real call fatals at entry and the existing direct/propagation
        // check reports at the outer site — do not descend. Otherwise bind the
        // post-coercion value (what the parameter actually holds inside).
        let Some(ty) = param.ty else {
            // Untyped parameter: it holds the value unchanged.
            bound.push((param.name.clone(), value));
            continue;
        };
        match coerce_into_param(cx.strict, ty, &value) {
            Some(coerced) => bound.push((param.name.clone(), coerced)),
            None => return, // entry TypeError (reported at outer site) or unsure.
        }
    }

    if bound.is_empty() {
        return; // nothing known to propagate.
    }

    // Canonical `(callee, binding)` key for recursion detection / memoization.
    let mut key_binding = bound.clone();
    key_binding.sort_by(|a, b| a.0.cmp(&b.0));
    let key: BindingKey = (decl.name.clone(), key_binding);

    // Provenance names the *first* binding site; a nested descent inherits it.
    let new_provenance;
    let (provenance, next_depth): (&str, usize) = match &descent {
        Some(d) => (d.provenance, d.depth + 1),
        None => {
            let line = cx.tree.position(call.span.start).line;
            new_provenance =
                format!("bound at {} call on line {}", render_call(&decl.name, &render_args), line);
            (&new_provenance, 1)
        }
    };

    // Budget (ADR-0009): stop past the cap with no diagnostic.
    if next_depth > MAX_BINDING_DEPTH {
        return;
    }

    // Build the bound environment; parameters carry the outer-site provenance.
    let bound_env: HashMap<String, Known> = bound
        .into_iter()
        .map(|(name, value)| {
            (name, Known { value, line: 0, bound: Some(provenance.to_owned()) })
        })
        .collect();

    // Recursion / memo bookkeeping, then descend with a fresh or inherited frame.
    match descent {
        Some(d) => {
            if d.stack.contains(&key) || d.memo.contains(&key) {
                return;
            }
            d.stack.push(key.clone());
            let child = Descent {
                provenance,
                depth: next_depth,
                stack: d.stack,
                memo: d.memo,
            };
            analyze_scope(cx, folder, callee_scope, bound_env, Some(child), out);
            d.stack.pop();
            d.memo.insert(key);
        }
        None => {
            // First binding from a top-level scope walk: fresh recursion state.
            let mut stack: Vec<BindingKey> = vec![key.clone()];
            let mut memo: HashSet<BindingKey> = HashSet::new();
            let child =
                Descent { provenance, depth: next_depth, stack: &mut stack, memo: &mut memo };
            analyze_scope(cx, folder, callee_scope, bound_env, Some(child), out);
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

    /// Build a `type.argument-mismatch` diagnostic for argument `i` of `decl`.
    /// With `provenance`, the message names the value's origin hop; without, it
    /// is the direct-literal message (byte-for-byte identical to the
    /// pre-propagation output).
    fn diagnostic(
        &self,
        offset: u32,
        value: &ArgValue,
        provenance: Option<&str>,
        decl: &FunctionDecl,
        i: usize,
        ty: ParamType,
    ) -> Diagnostic {
        let pos = self.tree.position(offset);
        let mode = if self.strict { "strict" } else { "coercive" };
        let callee = &decl.name;
        let param_name = &decl.params[i].name;
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
fn param_scalar_type(decl: &FunctionDecl, i: usize) -> Option<ParamType> {
    let param = decl.params.get(i)?;
    if param.variadic || param.by_ref {
        return None;
    }
    param.ty
}

/// Whether argument `i` binds to a variadic parameter (so it, and every later
/// argument, must be skipped).
fn arg_binds_to_variadic(decl: &FunctionDecl, i: usize) -> bool {
    decl.params.get(i).is_some_and(|p| p.variadic)
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
