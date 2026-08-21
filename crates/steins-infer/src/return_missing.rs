//! `type.return-missing` / `type.return-maybe-missing` (ADR-0078 / issue #199): a
//! non-void declared return whose body provably falls through — or may, on some
//! path — without returning, discriminated by [`body_has_terminator`] and the
//! never-returning callee set.

use std::collections::HashSet;

use steins_syntax::{
    CallExpr, Callee, OpaqueConstruct, RetHintKind, Scope, ScopeOwner, Stmt, StmtKind, body_end,
    body_has_terminator,
};

use crate::cx::Cx;
use crate::descent::checkable_calls;
use crate::project::{Diagnostic, FileUnit};
use crate::{TYPE_RETURN_MAYBE_MISSING_ID, TYPE_RETURN_MISSING_ID};

// return missing (ADR-0078, issue #199)

/// Every function-like body in this file, judged for the return-missing pair —
/// [`TYPE_RETURN_MISSING_ID`] and its `maybe-` sibling
/// [`TYPE_RETURN_MAYBE_MISSING_ID`]. Both premises are shared; one predicate,
/// [`body_has_terminator`], routes the finding between them.
///
/// Env-free and dead-region-free by design: the premise is a declaration plus
/// the ADR-0078 reachability foundation's structural verdict, neither of which a
/// walked value can change — the same posture `check_declaration_fatals` and
/// `docblock_hygiene` take.
///
/// `never_returning` is the set of callee names the run proved never return; see
/// [`never_returning_names`].
pub(crate) fn check_return_missing(cx: &Cx, never_returning: &HashSet<String>, out: &mut Vec<Diagnostic>) {
    for scope in cx.tree().scopes() {
        // Premise 1a: a written, non-void, non-never return hint. The RAW hint, not
        // the lowered `NativeType`: `: array` / `: mixed` / `: self` lower to `None`
        // and fatal exactly as `: int` does. `RetHintKind::Mixed` is spelled out with
        // `Other` on purpose — its summary exemption (issue #364) is about what the
        // envelope ADMITS, and admitting everything still admits nothing at all.
        let Some(hint) = scope.ret_hint else { continue };
        if matches!(hint.kind, RetHintKind::Void | RetHintKind::Never) {
            continue;
        }
        // Premise 1b: not a generator — a body with `yield` returns a `Generator`
        // object from the call, describing that object rather than a body exit
        // (ADR-0057 §5), so falling off the end is legal.
        if scope.is_generator {
            continue;
        }
        // Premise 2: the body PROVABLY falls through ([`BodyEnd`]'s asymmetry):
        // `Unknown` (a `try`/`catch`, a `goto`, an unstructurable `switch`) reads as
        // terminating, i.e. silence.
        if !body_end(&scope.stmts).provably_falls_through() {
            continue;
        }
        // Never-returning-callee refinement: `function g(): never { exit(1); }`
        // makes `function f(): int { g(); }` run clean (witnessed 8.5.9). Scope-wide
        // rather than path-precise — the safe, larger-silence direction.
        if scope_calls_never_returning(scope, never_returning) {
            continue;
        }
        // `include`/`require`/`eval` bring in code that can `exit` the whole script,
        // invisibly to this judgment. Only these two are vetoed — the rest of the
        // ADR-0001 give-up list defeats *value* tracking, which this premise never
        // consults.
        if scope
            .opaque
            .iter()
            .any(|s| matches!(s.construct, OpaqueConstruct::Include | OpaqueConstruct::Eval))
        {
            continue;
        }

        // The definite/possibly discriminator (ADR-0078 §1.3): a body that exits
        // nowhere fatals on every execution (the definite id); one that exits
        // somewhere but not on every path fatals only along the uncovered edge (the
        // `maybe-` sibling). Disjoint by construction: one predicate decides.
        let conditional = body_has_terminator(&scope.stmts);

        let declared = cx.tree().text_at(hint.span).unwrap_or("the declared type").trim();
        let (subject, php_subject) = return_missing_subject(cx, scope);
        let pos = cx.tree().position(hint.span.start);
        let message = if conditional {
            format!(
                "{subject} declares a return type of {declared} and returns on some paths, but \
                 one path falls through to the end — PHP fatals there with \"{php_subject}(): \
                 Return value must be of type {declared}, none returned\""
            )
        } else {
            format!(
                "{subject} declares a return type of {declared} but its body falls through \
                 to the end — PHP fatals there with \"{php_subject}(): Return value must be of \
                 type {declared}, none returned\""
            )
        };
        out.push(Diagnostic {
            id: if conditional { TYPE_RETURN_MAYBE_MISSING_ID } else { TYPE_RETURN_MISSING_ID },
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message,
            facet: None,
            fix: None,
        });
    }
}

/// How a `type.return-missing` message names its function-like: the readable
/// subject for the sentence, and the subject PHP's own `TypeError` prints.
///
/// The two differ only for a closure, whose runtime name is a synthesized
/// `{closure:file:line}` no source text can be quoted for.
fn return_missing_subject(cx: &Cx, scope: &Scope) -> (String, String) {
    match &scope.owner {
        ScopeOwner::Function(name) => (format!("function {name}"), name.clone()),
        ScopeOwner::Method { class, method } => {
            let qualified = format!("{class}::{method}");
            (format!("method {qualified}"), qualified)
        }
        ScopeOwner::Closure { def_offset } => {
            let line = cx.tree().position(*def_offset).line;
            (format!("the closure on line {line}"), "{closure}".to_owned())
        }
        // The top-level scope carries no return hint, so it never reaches here.
        ScopeOwner::TopLevel => ("the script".to_owned(), "{main}".to_owned()),
    }
}

/// The simple names of every function and method in the run that declares
/// `: never` — the veto set for the never-returning-callee obstacle.
///
/// Read off `Scope::ret_hint`, covering exactly the bodies the analysis lowered.
/// Names are lowercased (PHP names are case-insensitive) and are **simple**
/// names, not resolved targets: a scope is vetoed when it calls something
/// spelled like a never-returning callee. Deliberate over-silence — resolving
/// each call's real target would need the receiver's exact class, and the cost
/// of guessing wrong is a false `TypeError` accusation.
pub(crate) fn never_returning_names(units: &[FileUnit]) -> HashSet<String> {
    let mut names = HashSet::new();
    for unit in units {
        for scope in unit.tree.scopes() {
            if scope.ret_hint.is_none_or(|h| h.kind != RetHintKind::Never) {
                continue;
            }
            match &scope.owner {
                ScopeOwner::Function(name) => names.insert(name.to_ascii_lowercase()),
                ScopeOwner::Method { method, .. } => names.insert(method.to_ascii_lowercase()),
                ScopeOwner::TopLevel | ScopeOwner::Closure { .. } => continue,
            };
        }
    }
    names
}

/// Whether `scope` calls anything named in the never-returning veto set.
///
/// Two sources, together covering every call the lowering records: the trace's
/// statement-position calls (descending the structured `if`/`match` sub-traces,
/// which are where the fall-through path's own statements live) and
/// `Scope::method_calls`, the comprehensive method-call enumeration.
fn scope_calls_never_returning(scope: &Scope, never_returning: &HashSet<String>) -> bool {
    if never_returning.is_empty() {
        return false;
    }
    let named = |call: &CallExpr| {
        callee_simple_name(call).is_some_and(|n| never_returning.contains(&n.to_ascii_lowercase()))
    };
    scope.method_calls.iter().any(named) || trace_calls_any(&scope.stmts, &named)
}

/// `true` when any statement-position call in `stmts` (or in a structured
/// sub-trace beneath it) satisfies `pred`.
fn trace_calls_any(stmts: &[Stmt], pred: &impl Fn(&CallExpr) -> bool) -> bool {
    stmts.iter().any(|stmt| {
        if checkable_calls(&stmt.kind).into_iter().any(pred) {
            return true;
        }
        match &stmt.kind {
            StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                trace_calls_any(then_trace, pred)
                    || elseifs.iter().any(|(_, t)| trace_calls_any(t, pred))
                    || else_trace.as_deref().is_some_and(|t| trace_calls_any(t, pred))
            }
            StmtKind::Match { arms, default, .. } => {
                arms.iter().any(|a| trace_calls_any(&a.trace, pred))
                    || default.as_deref().is_some_and(|t| trace_calls_any(t, pred))
            }
            _ => false,
        }
    })
}

/// The simple callee name of a call, for name-keyed matching: the function name,
/// or the method name of an instance/static call. A constructor, a `$fn()` and an
/// unrepresentable callee have no name to match on.
fn callee_simple_name(call: &CallExpr) -> Option<&str> {
    match &call.receiver {
        Callee::Function(name) => Some(name),
        Callee::Method { method, .. } | Callee::Static { method, .. } => Some(method),
        Callee::Construct { .. } | Callee::DynamicVar(_) | Callee::Dynamic => None,
    }
}

// end return missing (ADR-0078, issue #199)
