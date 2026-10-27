//! The names a branch side may write, read off the trace (issue #1110).
//!
//! A two-sided `if` hands the pre-test value of an untested name back at the
//! join only when no side rewrote it. "Rewrote" cannot be read from the side's
//! env: a by-reference call *forgets* the name, which looks the same as a
//! side that never touched it. This is the syntactic answer instead: every
//! name the side's statements may bind, however the walk models them.
//!
//! The set is a may-write over-approximation. A name in it withholds the
//! hand-back, so a later test of the name is judged on what the sides left and
//! may stay undecided: that can make a finding (a branch the hand-back pruned is
//! reachable again) as well as lose one (a value the hand-back knew is gone).

use std::collections::HashSet;

use steins_syntax::{InvalidatedVar, Stmt, StmtKind};

use crate::by_value::arg_is_by_ref;
use crate::conjunct_writes::cond_rebinds;
use crate::cx::Cx;

/// What a run of statements may write: a set of names, or every name.
#[derive(Debug, Default)]
pub(crate) struct SideWrites {
    names: HashSet<String>,
    everything: bool,
}

impl SideWrites {
    /// Whether the side may write `name`.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.everything || self.names.contains(name)
    }

    fn add_all<'a>(&mut self, names: impl IntoIterator<Item = &'a String>) {
        self.names.extend(names.into_iter().cloned());
    }
}

/// The names `stmts` may write, recursing through every structured sub-trace.
pub(crate) fn trace_writes(cx: &Cx, stmts: &[Stmt], into: &mut SideWrites) {
    for stmt in stmts {
        stmt_writes(cx, stmt, into);
        for sub in stmt.kind.sub_traces() {
            trace_writes(cx, sub, into);
        }
    }
}

/// The names a condition may rebind (its assignments and by-reference calls).
pub(crate) fn cond_writes(cx: &Cx, cond: &steins_syntax::CondExpr, into: &mut SideWrites) {
    into.names.extend(cond_rebinds(cx, cond));
}

fn stmt_writes(cx: &Cx, stmt: &Stmt, into: &mut SideWrites) {
    for inv in &stmt.invalidated {
        if may_write(cx, inv) {
            into.names.insert(inv.name.clone());
        }
    }
    match &stmt.kind {
        StmtKind::Assign { var, .. } => {
            into.names.insert(var.clone());
        }
        StmtKind::OffsetWrite { base, .. }
        | StmtKind::OffsetAppend { base, .. }
        | StmtKind::OffsetUnset { base, .. } => {
            into.names.insert(base.clone());
        }
        // The pattern's targets are not recorded (the walk forgets everything).
        StmtKind::Destructure { .. } => into.everything = true,
        StmtKind::Opaque { writes, poisons, .. } => {
            into.everything |= *poisons;
            into.add_all(writes);
        }
        StmtKind::If { cond, elseifs, .. } => {
            cond_writes(cx, cond, into);
            for (c, _) in elseifs {
                cond_writes(cx, c, into);
            }
        }
        StmtKind::While { cond, writes, poisons, .. }
        | StmtKind::DoWhile { cond, writes, poisons, .. } => {
            cond_writes(cx, cond, into);
            into.everything |= *poisons;
            into.add_all(writes);
        }
        StmtKind::For { cond, writes, carried, poisons, .. } => {
            cond_writes(cx, cond, into);
            into.everything |= *poisons;
            into.add_all(writes.iter().chain(carried));
        }
        StmtKind::Foreach { subject, key_var, value_var, by_ref, writes, poisons, .. } => {
            into.everything |= *poisons;
            into.add_all(writes.iter().chain(key_var).chain(value_var));
            if *by_ref {
                into.add_all(subject);
            }
        }
        StmtKind::Try {
            body_writes, catch_writes, writes, poisons, ..
        } => {
            into.everything |= *poisons;
            into.add_all(writes.iter().chain(body_writes).chain(catch_writes));
        }
        _ => {}
    }
}

/// Whether an invalidated name may be rewritten, not just forgotten: opaque
/// evidence, or one provable site that is a by-reference parameter. The same
/// predicate [`cond_rebinds`] applies to a condition's call sites.
fn may_write(cx: &Cx, inv: &InvalidatedVar) -> bool {
    inv.opaque || inv.sites.iter().any(|(callee, position)| arg_is_by_ref(cx, callee, *position))
}
