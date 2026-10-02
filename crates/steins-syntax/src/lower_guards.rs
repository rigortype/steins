//! The guard regions of a statement (issue #928): every ternary and every short-circuit
//! expression in the expressions the statement itself evaluates, each with the extents
//! PHP leaves unevaluated when its test is decided.
//!
//! A [`GuardRegion`] is read off the CST, not off the lowered [`StmtKind`], because the
//! trace IR spells a statement's expressions only as far as its kinds go: `echo` keeps
//! its calls and nothing else, a bare `a && b;` is a `Barrier`, and a ternary nested in
//! a call argument is an [`ArgValue`] only where that argument is lowered. A sweep over
//! the IR would therefore have a different reach in each position; this one has the
//! same reach everywhere, which is the point of issue #928 (the same guard, discharged
//! the same way in every position).

use mago_span::HasSpan;
use mago_syntax::cst::{BinaryOperator, Node, Statement, UnaryPrefixOperator};

use crate::ast::{CondExpr, CondOperand, EXISTENCE_PREDICATES, GuardRegion};
use crate::lower_expr::{lower_binary_cond, lower_cond};
use crate::{children, to_span};

/// The [`GuardRegion`]s one statement's **own** expressions carry.
///
/// The same four statement kinds, and for the same reason, as `string_context_sites`: an
/// expression statement, `return`, and the two `echo` forms are the positions where the
/// walk's entry state is the state PHP evaluates the expression in. A branch condition, a
/// loop header or a `match` subject is judged by the construct that owns it, and a nested
/// statement collects its own.
pub(crate) fn guard_regions_of(s: &Statement<'_>) -> Vec<GuardRegion> {
    let mut out = Vec::new();
    match s {
        Statement::Expression(es) => scan_guard_regions(&Node::Expression(es.expression), &mut out),
        Statement::Return(r) => {
            if let Some(e) = r.value {
                scan_guard_regions(&Node::Expression(e), &mut out);
            }
        }
        Statement::Echo(e) => {
            for v in e.values.iter() {
                scan_guard_regions(&Node::Expression(v), &mut out);
            }
        }
        Statement::EchoTag(e) => {
            for v in e.values.iter() {
                scan_guard_regions(&Node::Expression(v), &mut out);
            }
        }
        _ => {}
    }
    out
}

/// Collect the guard regions inside one expression subtree. A closure, an arrow
/// function and a nested declaration are their own scopes, judged separately, and are
/// not descended.
pub(crate) fn scan_guard_regions(node: &Node<'_, '_>, out: &mut Vec<GuardRegion>) {
    scan(node, false, out);
}

/// `covered` is true while `node` is an operand [`lower_cond`] folds into a logical
/// expression already recorded: that one region's evaluation reaches every `&&`/`||`
/// in it, so a second region for an inner one would only lower the same prefix again.
/// It passes through the wrappers that stand between an operand and its parent (an
/// expression node, parentheses, `!`) and stops at anything else — a call's argument is
/// not folded into the condition.
fn scan(node: &Node<'_, '_>, covered: bool, out: &mut Vec<GuardRegion>) {
    let mut operand_of_logical = false;
    match node {
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_) => return,
        // `c ? a : b` evaluates exactly one arm, and `c ?: b` evaluates `b` only when
        // `c` is falsy.
        // The test is one operand of the region, so a connective in it is covered by it.
        Node::Conditional(c) => {
            let cond = lower_cond(c.condition);
            if could_decide_without_env(&cond) {
                out.push(GuardRegion {
                    cond,
                    dead_if_true: Some(to_span(c.r#else.span())),
                    dead_if_false: c.then.map(|t| to_span(t.span())),
                });
            }
            scan(&Node::Expression(c.condition), true, out);
            if let Some(then) = c.then {
                scan(&Node::Expression(then), false, out);
            }
            scan(&Node::Expression(c.r#else), false, out);
            return;
        }
        // `a && b` evaluates `b` only when `a` is truthy, `a || b` only when it is not;
        // the whole expression is lowered once, and evaluating it records the right
        // operand of every connective in it that its left operand decides.
        // `xor` evaluates both.
        Node::Binary(b) if is_short_circuit(&b.operator) => {
            if !covered {
                let cond = lower_binary_cond(b);
                if could_decide_without_env(&cond) {
                    out.push(GuardRegion { cond, dead_if_true: None, dead_if_false: None });
                }
            }
            operand_of_logical = true;
        }
        Node::Expression(_) | Node::Parenthesized(_) => operand_of_logical = covered,
        Node::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::Not(_)) => {
            operand_of_logical = covered;
        }
        _ => {}
    }
    for child in children(node) {
        scan(&child, operand_of_logical, out);
    }
}

fn is_short_circuit(op: &BinaryOperator<'_>) -> bool {
    matches!(
        op,
        BinaryOperator::And(_)
            | BinaryOperator::LowAnd(_)
            | BinaryOperator::Or(_)
            | BinaryOperator::LowOr(_)
    )
}

/// Whether `name` spells a function the engine folds as an existence guard
/// ([`EXISTENCE_PREDICATES`]), the one kind of call that can decide an environment-free
/// test. The callee name is the simple one; whether it denotes the global builtin is the
/// analyzer's question, asked at evaluation.
fn is_guard_predicate(name: &str) -> bool {
    let name = name.trim_start_matches('\\');
    EXISTENCE_PREDICATES.iter().any(|p| name.eq_ignore_ascii_case(p))
}

/// Whether evaluating `cond` against an **empty** environment can answer anything but
/// `Maybe`: an existence-predicate call, or a comparison, a truth test or an
/// `instanceof` whose operands are all literals, constants or class constants.
///
/// A region is judged against no environment at all, so only a test that names no
/// variable can be decided there; one that cannot is not worth carrying in the trace.
fn could_decide_without_env(cond: &CondExpr) -> bool {
    let free = |op: &CondOperand| {
        matches!(op, CondOperand::Literal(_) | CondOperand::Const(_) | CondOperand::ClassConst(..))
    };
    match cond {
        CondExpr::Call { call, .. } => call.callee.as_deref().is_some_and(is_guard_predicate),
        CondExpr::Cmp { lhs, rhs, .. } => free(lhs) && free(rhs),
        CondExpr::Truthy(op) | CondExpr::Instanceof { operand: op, .. } => free(op),
        CondExpr::Not(c) => could_decide_without_env(c),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            could_decide_without_env(a) || could_decide_without_env(b)
        }
        CondExpr::Isset { .. }
        | CondExpr::IssetVar { .. }
        | CondExpr::InstanceofDyn { .. }
        | CondExpr::Opaque { .. } => false,
    }
}
