//! The `try`/`catch` structure and `throw` statements (ADR-0040 damming): the
//! guard stack and catch-variable scope the walk threads down, and the site a
//! `throw` is.

use mago_span::HasSpan;
use mago_syntax::cst::{Expression, Node, Throw, TryCatchClause, Variable};

use super::{SiteScope, scan_sites};
use crate::ast::{SiteKind, SiteOrigin, ThrownKind};
use crate::lower_decl::lower_catch_clause;
use crate::lower_expr::instantiation_class;
use crate::lower_stmt::{collect_assign_writes, collect_call_vars};
use crate::{bytes_to_string, children, strip_dollar, to_span};

/// Walk a `try` statement. Its own guard wraps the try block only: the `catch`
/// and `finally` blocks are outside it, though inside any outer `try`, and a
/// catch clause's variable enters the rethrow scope for its own body.
pub(super) fn try_node(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let Node::Try(t) = node else { return };
    let mut guards = sx.guards.to_vec();
    guards.push(t.catch_clauses.iter().map(lower_catch_clause).collect());
    let inside = SiteScope { guards: &guards, ..*sx };
    for child in children(node) {
        match &child {
            Node::Block(_) => scan_sites(&child, &inside, out),
            Node::TryCatchClause(c) => catch_clause(c, sx, out),
            // The `try` keyword, and `finally`, which this try's catches never absorb.
            _ => scan_sites(&child, sx, out),
        }
    }
}

/// Walk one `catch` clause, its body under the clause's variable when that
/// variable can still be read as the caught exception.
fn catch_clause(c: &TryCatchClause<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let clause = lower_catch_clause(c);
    let mut scope = sx.catch_scope.to_vec();
    if let Some(var) = &clause.var {
        // Rethrow precision is only sound while `$e` still holds the caught
        // exception. If the clause body writes the variable — by assignment
        // or by handing it to any call (a by-ref signature could rebind it)
        // — a later `throw $e` may throw something else, so the variable
        // must NOT enter the rethrow scope (its throws degrade to unresolved).
        // Counterexample this fixed: `catch (RuntimeException $e) { $e =
        // new JsonException(); throw $e; }` under `@throws JsonException`
        // falsely reported RuntimeException.
        let mut written = Vec::new();
        for s in c.block.statements.iter() {
            collect_assign_writes(&Node::Statement(s), &mut written);
            collect_call_vars(&Node::Statement(s), &mut written);
        }
        if !written.contains(var) {
            scope.push((var.clone(), clause.classes.clone(), clause.has_unresolvable));
        }
    }
    let inside = SiteScope { catch_scope: &scope, ..*sx };
    for child in children(&Node::TryCatchClause(c)) {
        match &child {
            Node::Block(_) => scan_sites(&child, &inside, out),
            _ => scan_sites(&child, sx, out),
        }
    }
}

/// A `throw <expr>`: the thrown class, a rethrow of an enclosing catch's
/// variable, or an expression the scan cannot classify. The expression is
/// walked by the caller — a call inside it is a site of its own.
pub(super) fn throw_site(t: &Throw<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let thrown = match t.exception.unparenthesized() {
        Expression::Instantiation(inst) => match instantiation_class(inst) {
            Some(class) => ThrownKind::New(class),
            None => ThrownKind::Unresolved, // `throw new $c()` — dynamic class
        },
        Expression::Variable(Variable::Direct(dv)) => {
            let name = strip_dollar(bytes_to_string(dv.name));
            match sx.catch_scope.iter().rev().find(|(v, _, _)| *v == name) {
                Some((_, caught, unresolvable)) => {
                    ThrownKind::Rethrow { caught: caught.clone(), has_unresolvable: *unresolvable }
                }
                None => ThrownKind::Unresolved, // throwing a non-catch variable
            }
        }
        _ => ThrownKind::Unresolved,
    };
    out.push(sx.site(to_span(t.span()), SiteKind::Throw(thrown)));
}
