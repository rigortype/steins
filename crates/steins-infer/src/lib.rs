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

use std::collections::HashMap;

use steins_db::{Db, SourceFile, function_index, parse};
use steins_sidecar::{FoldArg, FoldResult, FoldValue, Sidecar};
use steins_syntax::{
    ArgValue, CallExpr, FunctionDecl, ParamType, ScalarType, Scope, SourceTree, StmtKind,
};

/// The registry id for the one check this crate emits (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

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
        check_scope(&cx, folder, scope, &mut out);
    }

    out
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
}

/// Walk one scope's trace, tracking known local values, and check every call.
fn check_scope(cx: &Cx, folder: &mut dyn Folder, scope: &Scope, out: &mut Vec<Diagnostic>) {
    let mut env: HashMap<String, Known> = HashMap::new();

    for stmt in &scope.stmts {
        match &stmt.kind {
            StmtKind::Barrier => env.clear(),
            StmtKind::Return(_) => {}
            StmtKind::Assign { var, value, span } => {
                // A poisoned scope never trusts a variable value.
                match cx.resolve_literal(value, &env, scope.poisoned, folder) {
                    Some(lit) => {
                        let line = cx.tree.position(span.start).line;
                        env.insert(var.clone(), Known { value: lit, line });
                    }
                    None => {
                        env.remove(var);
                    }
                }
            }
            StmtKind::Call(call) => {
                check_propagated_call(cx, folder, scope.poisoned, call, &env, out);
            }
        }

        // After the statement, any variable handed to a call is untrustworthy
        // (a by-ref parameter could have mutated it).
        for v in &stmt.invalidated {
            env.remove(v);
        }
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
            ArgValue::Var(name) if !poisoned => env
                .get(name)
                .map(|k| (k.value.clone(), format!("from ${name}, assigned at line {}", k.line))),
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
        let StmtKind::Return(value) = &stmt.kind else { return None };
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
