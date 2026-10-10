//! `try`/`catch`/`finally` as a sub-trace (the ADR-0027 `try` amendment, issues
//! #943 and #905): the [`StmtKind::Try`] lowering, the construct's terminality
//! ([`try_end`]), and the throw-free statement whitelist both of them and the
//! presence pass share.
//!
//! One rule decides where control leaves the construct, and every reader applies
//! it — this module to [`BodyEnd`], the walker to its `Flow`, the presence pass to
//! its own flow. The table it comes from is on [`StmtKind::Try`]:
//!
//! * the **live arms** are the body and, unless the body provably cannot throw
//!   ([`try_body_cannot_throw`]), every `catch`;
//! * the construct's successor is reached exactly when `finally` (if any) can fall
//!   through **and** some live arm can;
//! * so it terminates when `finally` terminates, or when every live arm does. A
//!   `finally` that falls through does not rescue a terminating arm: the pending
//!   `return`, `throw`, `break` or `continue` proceeds after it.

use mago_syntax::cst::{
    ArrayElement, Expression, Literal, Node, Statement, Try, UnaryPrefixOperator, Variable,
};

use crate::ast::{BodyEnd, CatchArm, Stmt, StmtKind};
use crate::lower_decl::lower_catch_clause;
use crate::lower_expr::opaque_sets;
use crate::lower_presence::subtree_has_goto;
use crate::lower_stmt::{
    block_end, collect_assign_writes, collect_call_vars, collect_read_vars, lower_trace,
};

/// Lower a `try` to [`StmtKind::Try`]. The construct's own sets come from
/// [`opaque_sets`] over the whole statement, unchanged, so its successor is an
/// `Opaque`'s to the byte; what the variant adds is the three parts as sub-traces
/// and the two sets a handler's entry forgets.
pub(crate) fn lower_try(s: &Statement<'_>, t: &Try<'_>) -> Stmt {
    let (writes, reads, poisons, may_return) = opaque_sets(&Node::Statement(s));
    let body_nodes: Vec<Node<'_, '_>> = t.block.statements.iter().map(Node::Statement).collect();
    let (body_writes, body_reads) = list_sets(&body_nodes);
    let catch_nodes: Vec<Node<'_, '_>> =
        t.catch_clauses.iter().map(Node::TryCatchClause).collect();
    let (catch_writes, catch_reads) = list_sets(&catch_nodes);
    let catches = t
        .catch_clauses
        .iter()
        .map(|c| CatchArm {
            clause: lower_catch_clause(c),
            trace: lower_trace(c.block.statements.as_slice()),
        })
        .collect();
    let kind = StmtKind::Try {
        body: lower_trace(t.block.statements.as_slice()),
        catches,
        finally: t.finally_clause.as_ref().map(|f| lower_trace(f.block.statements.as_slice())),
        catches_live: !try_body_cannot_throw(t.block.statements.as_slice()),
        has_goto: subtree_has_goto(&Node::Statement(s)),
        body_writes,
        body_reads,
        catch_writes,
        catch_reads,
        writes,
        reads,
        poisons,
        may_return,
    };
    Stmt::lowered(kind, Vec::new())
}

/// The write and read sets of a list of sibling nodes, with the meaning
/// [`opaque_sets`] gives one node's: the writes are every assignment target and
/// every name handed to a call, the reads every other name mentioned.
fn list_sets(nodes: &[Node<'_, '_>]) -> (Vec<String>, Vec<String>) {
    let mut writes = Vec::new();
    for n in nodes {
        collect_call_vars(n, &mut writes);
        collect_assign_writes(n, &mut writes);
    }
    let mut reads = Vec::new();
    for n in nodes {
        collect_read_vars(n, &writes, &mut reads);
    }
    (writes, reads)
}

/// A `try`'s terminality, by the module's rule over [`block_end`]'s answers.
///
/// * A `goto` or a label anywhere in the construct is [`BodyEnd::Unknown`]: its
///   edges are the one kind this rule does not bound.
/// * The live arms join as an `if`'s do ([`BodyEnd::join_arms`]).
/// * A `finally` that terminates terminates the construct; one that falls
///   through leaves the join standing; an undecided one keeps a terminating join
///   (both of its outcomes terminate) and turns any other into `Unknown`.
///
/// `break` and `continue` count as terminating here as they do in every list
/// [`block_end`] reads: they leave the list, and the statement after the `try` in
/// it is not reached. A `finally` cannot hold one that leaves it — PHP rejects
/// that at compile time.
pub(crate) fn try_end(s: &Statement<'_>, t: &Try<'_>) -> BodyEnd {
    if subtree_has_goto(&Node::Statement(s)) {
        return BodyEnd::Unknown;
    }
    let mut arms = vec![block_end(t.block.statements.as_slice())];
    if !try_body_cannot_throw(t.block.statements.as_slice()) {
        arms.extend(t.catch_clauses.iter().map(|c| block_end(c.block.statements.as_slice())));
    }
    let joined = BodyEnd::join_arms(arms);
    let Some(f) = t.finally_clause.as_ref() else { return joined };
    match block_end(f.block.statements.as_slice()) {
        BodyEnd::Terminates => BodyEnd::Terminates,
        BodyEnd::FallsThrough => joined,
        BodyEnd::Unknown if joined == BodyEnd::Terminates => BodyEnd::Terminates,
        BodyEnd::Unknown => BodyEnd::Unknown,
    }
}

/// Whether no statement of a `try` body can throw, so no `catch` of it can ever be
/// entered (D2 of the walker coverage run, issue #1033).
///
/// Every statement must be [`stmt_cannot_throw`]'s, or a `return` whose value's
/// type the spelling decides — `return 1;`, `return [];`, a bare `return;`. Such a
/// `return` throws only when the declared return type rejects that type, which is
/// a proven `TypeError` the return-type check reports on the statement itself;
/// reading it as throw-free can only drop a `catch (TypeError)` arm of code that
/// check already convicts. A `return $x;` is not admitted: what `$x` holds is not
/// something this syntactic reading can vouch for.
pub(crate) fn try_body_cannot_throw(stmts: &[Statement<'_>]) -> bool {
    stmts.iter().all(|s| match s {
        Statement::Return(r) => r.value.is_none_or(|v| {
            !matches!(v.unparenthesized(), Expression::Variable(_)) && expr_cannot_throw(v)
        }),
        _ => stmt_cannot_throw(s),
    })
}

/// Whether a statement **provably cannot throw**, over a whitelist narrow enough
/// that no PHP semantics argument is needed to read it.
///
/// Almost every PHP construct can raise something: a call, a property fetch, a
/// division, a concatenation with an object, an undefined constant. So this answers
/// `true` only for a plain `=` assignment from a literal, an array of literals or
/// another local — the prologue idiom (`$count = 0;`, `$out = [];`, `$x = $y;`) and
/// nothing beyond it. Answering `false` costs precision and never correctness: it
/// puts the statement back on the "may have thrown before this" side, which is the
/// conservative reading the presence pass applies to the whole block anyway.
pub(crate) fn stmt_cannot_throw(s: &Statement<'_>) -> bool {
    match s {
        Statement::Noop(_) => true,
        Statement::Expression(es) => match es.expression.unparenthesized() {
            Expression::Assignment(a) => {
                a.operator.is_assign()
                    && matches!(a.lhs.unparenthesized(), Expression::Variable(Variable::Direct(_)))
                    && expr_cannot_throw(a.rhs)
            }
            _ => false,
        },
        _ => false,
    }
}

/// The value half of [`stmt_cannot_throw`]: a literal, another local, an array
/// literal of such values, a `!` over one, or a sign over a number literal.
///
/// The sign is admitted over a number literal only: witnessed on 8.5.11, `-[]`,
/// `-$a` with `$a = []` and `-"abc"` each throw a `TypeError`
/// ("Unsupported operand types").
fn expr_cannot_throw(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(_) | Expression::Variable(Variable::Direct(_)) => true,
        Expression::Array(a) => a.elements.iter().all(element_cannot_throw),
        Expression::LegacyArray(a) => a.elements.iter().all(element_cannot_throw),
        Expression::UnaryPrefix(up) => match up.operator {
            UnaryPrefixOperator::Not(_) => expr_cannot_throw(up.operand),
            UnaryPrefixOperator::Negation(_) | UnaryPrefixOperator::Plus(_) => matches!(
                up.operand.unparenthesized(),
                Expression::Literal(Literal::Integer(_) | Literal::Float(_))
            ),
            _ => false,
        },
        _ => false,
    }
}

/// One array-literal element of [`expr_cannot_throw`]. A key must be a literal: an
/// array or object held by a local throws as a key ("Illegal offset type").
fn element_cannot_throw(element: &ArrayElement<'_>) -> bool {
    match element {
        ArrayElement::KeyValue(kv) => {
            matches!(kv.key.unparenthesized(), Expression::Literal(_)) && expr_cannot_throw(kv.value)
        }
        ArrayElement::Value(v) => expr_cannot_throw(v.value),
        ArrayElement::Missing(_) => true,
        ArrayElement::Variadic(_) => false,
    }
}
