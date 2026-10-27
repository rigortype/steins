//! The per-statement value-condition pre-step (issue #1098): a `&&`/`||` or a ternary in
//! value position is a condition, and the operand or arm a decided one skips is dead
//! **before** the statement's calls are judged.
//!
//! The assignment seam, a returning exit and the dump surface evaluate these through
//! [`eval_cond`] too, but only at step 2 of the statement walk, after step 1 has judged
//! the positions the dead extent should have silenced. [`sweep_guard_regions`] runs
//! early and is environment-free by design. This is the environment-reading half: the
//! conditions are judged against the statement's entry env and store, read-only, and the
//! extents they prove unevaluated are recorded. The seam later evaluating the same
//! condition a second time is safe and marks the same spans.
//!
//! A condition that reads a name the statement may rebind elsewhere is left to step 2: the
//! entry env would be stale for it (`g(h($m), $m !== null && f())`).
//!
//! [`sweep_guard_regions`]: crate::guard_sweep::sweep_guard_regions

use std::collections::HashMap;

use steins_domain::Certainty;
use steins_syntax::{
    ArgValue, CallExpr, Callee, CondExpr, CondOperand, LogicalOp, Receiver, Stmt, StmtKind,
};

use crate::by_value::by_value_survivors;
use crate::cond::eval_cond;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::oneof_guard::collect_reads;
use crate::walk::{WalkCx, mark_dead_span};

/// One value-position condition and the extents its verdict can prove dead.
struct ValueCond<'a> {
    cond: &'a CondExpr,
    /// The ternary arms `(then, else)`; `None` for a connective, whose operand the
    /// condition itself marks.
    arms: Option<(steins_syntax::Span, steins_syntax::Span)>,
}

/// Judge every value-position condition of `stmt` against the entry state and record
/// what the decided ones skip. Plain per-scope walk only (the caller's gate).
pub(crate) fn sweep_value_conds(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
) {
    let mut conds = Vec::new();
    collect_stmt(&stmt.kind, &mut conds);
    if conds.is_empty() {
        return;
    }
    // A name handed to a call survives it only where ADR-0070's gate proves the call
    // by value; any other name the statement hands on may be rebound before the
    // condition runs, and the entry env is stale for it.
    let (kept, object_kept) =
        by_value_survivors(w.cx, w.scope.poisoned, &stmt.invalidated, env, store);
    for vc in conds {
        let mut reads = Vec::new();
        collect_reads(vc.cond, &mut reads);
        // A handle the call kept keeps its identity, not its state: a condition that reads
        // only names, literals, constants and calls cannot see the difference.
        let state_free = reads_no_object_state(vc.cond);
        let stale = |r: &String| {
            stmt.invalidated.iter().any(|i| i.name == *r)
                && !(kept.contains(r.as_str()) && (state_free || !object_kept.contains(r.as_str())))
        };
        if reads.iter().any(stale) {
            continue;
        }
        let verdict = eval_cond(w, folder, vc.cond, env, store, w.scope.poisoned);
        if let Some((then_span, else_span)) = vc.arms {
            match verdict {
                Certainty::Yes => mark_dead_span(w, else_span),
                Certainty::No => mark_dead_span(w, then_span),
                Certainty::Maybe => {}
            }
        }
    }
}

fn collect_stmt<'a>(kind: &'a StmtKind, out: &mut Vec<ValueCond<'a>>) {
    match kind {
        StmtKind::Assign { value, call, .. } | StmtKind::Return { value, call, .. } => {
            collect_value(value, out);
            if let Some(c) = call {
                collect_call(c, out);
            }
        }
        StmtKind::PropAssign { value, value_call, .. } => {
            collect_value(value, out);
            if let Some(c) = value_call {
                collect_call(c, out);
            }
        }
        StmtKind::Call(c) => collect_call(c, out),
        StmtKind::Echo(calls) => calls.iter().for_each(|c| collect_call(c, out)),
        _ => {}
    }
}

fn collect_call<'a>(call: &'a CallExpr, out: &mut Vec<ValueCond<'a>>) {
    collect_callee(&call.receiver, out);
    for a in &call.args {
        collect_value(&a.value, out);
    }
    for n in &call.named_args {
        collect_value(&n.value, out);
    }
}

fn collect_callee<'a>(callee: &'a Callee, out: &mut Vec<ValueCond<'a>>) {
    if let Callee::Method { receiver: Receiver::New { args, named, .. }, .. } = callee {
        args.iter().for_each(|a| collect_value(a, out));
        named.iter().for_each(|n| collect_value(&n.value, out));
    }
}

fn collect_value<'a>(value: &'a ArgValue, out: &mut Vec<ValueCond<'a>>) {
    match value {
        ArgValue::Logical { op, lhs, rhs, cond, .. } => {
            if *op != LogicalOp::Xor {
                out.push(ValueCond { cond, arms: None });
            }
            collect_value(lhs, out);
            collect_value(rhs, out);
        }
        ArgValue::Ternary { cond, then_val, then_span, else_val, else_span } => {
            out.push(ValueCond { cond, arms: Some((*then_span, *else_span)) });
            collect_value(then_val, out);
            collect_value(else_val, out);
        }
        ArgValue::Coalesce(a, b, _)
        | ArgValue::Concat(a, b)
        | ArgValue::Binary { lhs: a, rhs: b, .. }
        | ArgValue::OffsetRead { base: a, key: b } => {
            collect_value(a, out);
            collect_value(b, out);
        }
        ArgValue::Not(v) | ArgValue::Cast { operand: v, .. } => collect_value(v, out),
        ArgValue::Array(items) => items.iter().for_each(|(_, v)| collect_value(v, out)),
        ArgValue::Call(_, args) => args.iter().for_each(|a| collect_value(a, out)),
        ArgValue::New(_, args, named) => {
            args.iter().for_each(|a| collect_value(a, out));
            named.iter().for_each(|n| collect_value(&n.value, out));
        }
        ArgValue::MethodCall { callee, args, named } => {
            collect_callee(callee, out);
            args.iter().for_each(|a| collect_value(a, out));
            named.iter().for_each(|n| collect_value(&n.value, out));
        }
        _ => {}
    }
}

/// Whether `cond` is built only from atoms that read a variable's binding and never the
/// state of an object it holds: no property or offset operand, no unmodelled expression.
fn reads_no_object_state(cond: &CondExpr) -> bool {
    let operand = |op: &CondOperand| {
        matches!(
            op,
            CondOperand::Var(_)
                | CondOperand::Literal(_)
                | CondOperand::Const(_)
                | CondOperand::ClassConst(..)
        )
    };
    match cond {
        CondExpr::Cmp { lhs, rhs, .. } => operand(lhs) && operand(rhs),
        CondExpr::Truthy(op) | CondExpr::Instanceof { operand: op, .. } => operand(op),
        CondExpr::Not(c) => reads_no_object_state(c),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            reads_no_object_state(a) && reads_no_object_state(b)
        }
        CondExpr::Call { .. } | CondExpr::IssetVar { .. } => true,
        CondExpr::Isset { .. } | CondExpr::InstanceofDyn { .. } | CondExpr::Opaque { .. } => false,
    }
}
