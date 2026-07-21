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
use steins_syntax::{
    ArgValue, CallExpr, FunctionDecl, ParamType, ScalarType, Scope, SourceTree, StmtKind,
};

/// The registry id for the one check this crate emits (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

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
#[salsa::tracked]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let functions = function_index(db, file);
    check(tree, functions, file.path(db))
}

/// The pure checking core (no salsa) — easy to unit-test and to reuse.
///
/// Two passes feed the one check. The **direct pass** walks every call site with
/// a literal argument (unchanged behavior). The **propagation pass** walks each
/// scope's linear trace and resolves `$var` / constant-function-return arguments
/// to proven values (ADR-0001). The two partition cleanly by argument kind —
/// literals go to the first, `Var`/`Call` arguments to the second — so no call
/// site is reported twice.
#[must_use]
pub fn check(tree: &SourceTree, functions: &[FunctionDecl], path: &str) -> Vec<Diagnostic> {
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

    // --- Propagation pass: resolved `$var` / constant-return arguments. ---
    for scope in tree.scopes() {
        check_scope(&cx, scope, &mut out);
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
fn check_scope(cx: &Cx, scope: &Scope, out: &mut Vec<Diagnostic>) {
    let mut env: HashMap<String, Known> = HashMap::new();

    for stmt in &scope.stmts {
        match &stmt.kind {
            StmtKind::Barrier => env.clear(),
            StmtKind::Return(_) => {}
            StmtKind::Assign { var, value, span } => {
                // A poisoned scope never trusts a variable value.
                match cx.resolve_literal(value, &env, scope.poisoned) {
                    Some(lit) => {
                        let line = cx.tree.position(span.start).line;
                        env.insert(var.clone(), Known { value: lit, line });
                    }
                    None => {
                        env.remove(var);
                    }
                }
            }
            StmtKind::Call(call) => check_propagated_call(cx, scope.poisoned, call, &env, out),
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
            ArgValue::Call(name, args) if args.is_empty() => cx
                .resolve_const_fn(name)
                .map(|(lit, line)| (lit, format!("from {name}(), defined at line {line}"))),
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
    ) -> Option<ArgValue> {
        if poisoned {
            return None;
        }
        match value {
            v if v.is_literal() => Some(v.clone()),
            ArgValue::Var(name) => env.get(name).map(|k| k.value.clone()),
            ArgValue::Call(name, args) if args.is_empty() => {
                self.resolve_const_fn(name).map(|(lit, _line)| lit)
            }
            _ => None,
        }
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
