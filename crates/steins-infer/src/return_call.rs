//! `return f(args);` judged as `$v = f(args); return $v;` is (ADR-0043's 2026-10-11
//! amendment on #950).
//!
//! Before it, the return check read a returned call through
//! `resolve_literal_strat_ex` only, whose `Call` arm answers a nullary constant
//! function and a builtin fold: "a bare project call is not resolved here". So
//! `function g(): int { return s(1); }` with `s(): string` was silent, while
//! `return s0()` and `$x = s(1); return $x;` both reported. The summary the call's
//! descent came back with (step 1 of the walk) and the declared return arms never
//! reached the return seam.
//!
//! # The rule
//!
//! The returned call is bound to a reserved slot of a copy of the env and store by
//! the very function an assignment runs ([`apply_assign`]), fed the statement's own
//! [`StmtCalls`], and the return seam then judges `Var(slot)` against the copy.
//! There is no second summary application to drift from the first: whatever the
//! assignment binds, the return judges, and whatever it leaves unbound, the return
//! leaves unjudged.
//!
//! ```text
//! returned value              binding                              judged as
//! --------------------------- ------------------------------------ ---------------------
//! project function call       the call's return summary, else      `$v = f(args)`
//!                             its declared return arms
//! method / static call        the same, through the receiver's     `$v = $o->m(args)`
//!                             resolution                           `$v = A::m(args)`
//! builtin call                the fold, the union fold, then the   `$v = strlen($s)`
//!                             builtin ladder
//! anything else               no binding (the value is read as     unchanged
//!                             before)
//! ```
//!
//! The copy is made only where a return type is declared, natively or in the
//! docblock, and only in the plain per-scope pass: a descent rebinds the callee's
//! parameters, not its return, and carries no return type to judge.
//!
//! [`apply_assign`]: crate::assign::apply_assign

use std::collections::HashMap;

use steins_syntax::{ArgValue, Stmt, StmtKind};

use crate::assign::apply_assign;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::stmt_calls::StmtCalls;
use crate::walk::WalkCx;

/// The reserved name the returned call is bound to. A NUL never appears in a PHP
/// variable name the lowering produces, so it cannot collide with a real binding.
const RETURN_SLOT: &str = "\0return";

/// A returned call, bound as an assignment would bind it.
pub(crate) struct ReturnedCall {
    /// The env after the virtual assignment.
    pub(crate) env: HashMap<String, Known>,
    /// The store after the virtual assignment.
    pub(crate) store: Store,
    /// The value to judge in place of the call: `Var(slot)`.
    pub(crate) slot: ArgValue,
}

/// Bind the call a `return` carries, or `None` where the returned value is not a
/// project, method, static or builtin call, or nothing declares a return type.
pub(crate) fn bind_returned_call(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    calls: &StmtCalls,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<ReturnedCall> {
    let StmtKind::Return { value, call: Some(call), span } = &stmt.kind else { return None };
    if !matches!(value, ArgValue::Call(..) | ArgValue::MethodCall { .. })
        || (w.ret_info.is_none() && w.ret_phpdoc.is_none())
        || w.scope.poisoned
    {
        return None;
    }
    let (mut env, mut store) = (env.clone(), store.clone());
    // The findings under the call's own arguments were emitted by step 1; the
    // virtual binding's are the same ones again, so they go nowhere.
    let mut scratch = Vec::new();
    apply_assign(
        w,
        folder,
        RETURN_SLOT,
        value,
        Some(call),
        span.start,
        &mut env,
        &mut store,
        &mut None,
        calls.summary.as_ref(),
        calls.ctor_heap.as_ref(),
        calls.return_arms.as_deref(),
        &mut scratch,
    );
    Some(ReturnedCall { env, store, slot: ArgValue::Var(RETURN_SLOT.to_owned()) })
}
