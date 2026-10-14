//! Version-dependent regions: the branches of an `if` whose condition reads the
//! engine's version and that the declared target range does not decide.
//!
//! A branch such as `if (PHP_VERSION_ID < 80000) { mktime(); }` runs on an older PHP
//! only. The analysing PHP (8.5) reflects the signatures of the NEWER engine, so a
//! verdict about a call inside the branch is about a PHP the branch never runs on.
//! The families that judge a call from the analysing PHP's reflection (internal
//! arity, engine methods, callbacks) ask [`in_version_dependent_region`] and decline.
//!
//! A condition reads the version when it mentions `PHP_VERSION_ID`,
//! `PHP_MAJOR_VERSION`, `PHP_MINOR_VERSION`, `PHP_RELEASE_VERSION` or `PHP_VERSION`,
//! or calls `version_compare`. The one comparison the target range can decide, the
//! `PHP_VERSION_ID` against an int literal ([`eval_version_id_cmp`]), is not a region
//! when it is decided: the arm that cannot run on any target is a region (the walk
//! skips its statements but still visits the closures written in it), the other is live
//! on every target. Every other read is undecided by construction (the walk does not fold it).
//! Every arm of an `if`/`elseif`/`else` chain with one such condition is a region,
//! since each arm's liveness depends on every earlier condition.
//!
//! Ternaries and the right operand of `&&`/`||` are not regions yet.

use steins_domain::Certainty;
use steins_syntax::{ArgValue, CallExpr, CondExpr, CondOperand, Span, Stmt, StmtKind};

use crate::cond::eval_version_id_cmp;
use crate::cx::Cx;

const VERSION_CONSTS: [&str; 5] = [
    "PHP_VERSION_ID",
    "PHP_MAJOR_VERSION",
    "PHP_MINOR_VERSION",
    "PHP_RELEASE_VERSION",
    "PHP_VERSION",
];

fn is_version_const(raw: &str) -> bool {
    VERSION_CONSTS.contains(&raw.trim_start_matches('\\'))
}

fn call_reads_version(call: &CallExpr) -> bool {
    call.callee.as_deref().is_some_and(|c| c.eq_ignore_ascii_case("version_compare"))
        || call
            .args
            .iter()
            .any(|a| matches!(&a.value, ArgValue::GlobalConst(r) if is_version_const(&r.raw)))
}

fn operand_reads_version(op: &CondOperand) -> bool {
    match op {
        CondOperand::Const(r) => is_version_const(&r.raw),
        CondOperand::Other { call: Some(call), .. } => call_reads_version(call),
        _ => false,
    }
}

/// Whether `cond` reads the version in a way the target range leaves undecided.
fn cond_undecided_version(cx: &Cx, cond: &CondExpr) -> bool {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => {
            if !(operand_reads_version(lhs) || operand_reads_version(rhs)) {
                return false;
            }
            // A decided `PHP_VERSION_ID <op> literal` is not a region.
            !matches!(
                eval_version_id_cmp(cx, *op, lhs, rhs),
                Some(Certainty::Yes | Certainty::No)
            )
        }
        CondExpr::Truthy(op) => operand_reads_version(op),
        CondExpr::Not(c) => cond_undecided_version(cx, c),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            cond_undecided_version(cx, a) || cond_undecided_version(cx, b)
        }
        CondExpr::Call { call, .. } => call_reads_version(call),
        _ => false,
    }
}

/// The source extent a trace covers, or `None` for an empty one.
fn trace_span(trace: &[Stmt]) -> Option<Span> {
    Some(Span { start: trace.first()?.span.start, end: trace.last()?.span.end })
}

/// The verdict the target range gives a plain `PHP_VERSION_ID <op> literal` condition.
fn decided(cx: &Cx, cond: &CondExpr) -> Option<Certainty> {
    let CondExpr::Cmp { op, lhs, rhs } = cond else { return None };
    eval_version_id_cmp(cx, *op, lhs, rhs).filter(|v| *v != Certainty::Maybe)
}

fn collect(cx: &Cx, stmts: &[Stmt], regions: &mut Vec<Span>) {
    for stmt in stmts {
        if let StmtKind::If { cond, then_trace, elseifs, else_trace } = &stmt.kind {
            let rest = || {
                elseifs.iter().map(|(_, t)| t.as_slice()).chain(else_trace.as_deref())
            };
            match decided(cx, cond) {
                // The range fixes the condition: only the arm that cannot run on any
                // target is a region (its closures are scopes the walk still visits).
                Some(Certainty::Yes) => regions.extend(rest().filter_map(trace_span)),
                Some(_) => regions.extend(trace_span(then_trace)),
                None if cond_undecided_version(cx, cond)
                    || elseifs.iter().any(|(c, _)| cond_undecided_version(cx, c)) =>
                {
                    regions.extend(stmt.kind.sub_traces().into_iter().filter_map(trace_span));
                }
                None => {}
            }
        }
        for sub in stmt.kind.sub_traces() {
            collect(cx, sub, regions);
        }
    }
}

/// Whether the byte `offset` of the current file lies in a branch whose liveness
/// depends on the PHP version the target range does not fix. Computed on demand: the
/// callers ask only when about to report.
pub(crate) fn in_version_dependent_region(cx: &Cx, offset: u32) -> bool {
    let mut regions = Vec::new();
    for scope in cx.tree().scopes() {
        collect(cx, &scope.stmts, &mut regions);
    }
    regions.iter().any(|r| r.start <= offset && offset < r.end)
}
