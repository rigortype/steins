use std::collections::HashMap;

use steins_syntax::{Callee, Stmt, StmtKind};

use crate::fold::Folder;
use crate::absence::{check_undefined_function, check_undefined_method};
use crate::arg_check::check_builtin_call_args;
use crate::arity::{check_arity, check_printf_arity};
use crate::declared_receiver::check_phpdoc_undefined_method;
use crate::descent::{
    ThisWriteBack, check_propagated_call, checkable_calls, handle_var_call, try_descend_function,
};
use crate::dump::{emit_asserts, emit_dumps};
use crate::env::{ContractArm, Descent, HeapSummary, Known, ReturnSummary, Store};
use crate::inaccessible::check_inaccessible_method;
use crate::method_call::handle_method_call;
use crate::non_object::{check_call_on_non_object, check_call_on_null};
use crate::out_params::check_preg_pattern;
use crate::project::Diagnostic;
use crate::return_arms::{bindable_args, fn_return_arms_at_call};
use crate::walk::WalkCx;

/// What step 1 of [`walk_trace`] learned from a statement's calls, for the phases
/// after it to read.
///
/// [`walk_trace`]: crate::walk::walk_trace
pub(crate) struct StmtCalls {
    /// The return-fact summary of a `$x = f(...)` / `$x = $o->m(...)` RHS
    /// descent (ADR-0057 T0; ADR-0075 for methods/statics), captured in step 1
    /// and consumed by `apply_assign` in step 2. For an `Assign` statement
    /// `checkable_calls` yields exactly the RHS call. Constructors keep
    /// descending for diagnostics but never fill this slot (ADR-0075 §3).
    pub(crate) summary: Option<ReturnSummary>,
    /// The declared return floor, resolved before `apply_assign` unbinds the
    /// assignment target (self-assign `$o = $o->m(1)` would otherwise drop the
    /// exact receiver first).
    pub(crate) return_arms: Option<Vec<ContractArm>>,
    /// The constructor descent's `$this` snapshot for the `new` this statement
    /// carries (ADR-0057's constructor-summary amendment, C7): captured in step 1,
    /// where the `Callee::Construct` rung already walked the body, and consumed by
    /// the object build — `apply_assign`'s `New` arm in step 2, or the
    /// `return new C()` arm of step 1c's classifier. One walk, one site.
    pub(crate) ctor_heap: Option<HeapSummary>,
    /// The `$this` snapshots this statement's descents came back with (ADR-0057's
    /// 2026-08-17 amendment, D4), applied by step 1a AFTER its sweeps — a list
    /// rather than a slot because an `echo` carries several calls, and because it
    /// is the list that lets that step decline a pair naming one object.
    pub(crate) this_backs: Vec<ThisWriteBack>,
}

/// Step 1 of [`walk_trace`]: check and descend every statically-named call `stmt`
/// carries, against the env and store the statement is entered with.
///
/// [`walk_trace`]: crate::walk::walk_trace
pub(crate) fn check_stmt_calls(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
    descent: &mut Option<Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> StmtCalls {
    let cx = w.cx;
    let scope = w.scope;
    let mut stmt_summary: Option<ReturnSummary> = None;
    let mut stmt_return_arms: Option<Vec<ContractArm>> = None;
    let mut stmt_ctor_heap: Option<HeapSummary> = None;
    let mut stmt_this_backs: Vec<ThisWriteBack> = Vec::new();
    for call in checkable_calls(&stmt.kind) {
        match &call.receiver {
            Callee::Function(_) => {
                // Where this call's findings begin: the discarded-call
                // judgment below reads what the checkers between here and
                // there concluded about the same call (ADR-0096 §3).
                let before = out.len();
                check_propagated_call(
                    cx,
                    folder,
                    scope.poisoned,
                    descent.is_some(),
                    call,
                    env,
                    store,
                    w.this_exact,
                    w.enclosing_class,
                    out,
                );
                // The builtin arm of the same judgment (ADR-0056 §9): the check
                // above returns early for a callee it cannot resolve to a
                // project function, and this one answers exactly there, off the
                // engine's own reflected parameter list.
                check_builtin_call_args(
                    cx,
                    folder,
                    scope.poisoned,
                    descent.is_some(),
                    call,
                    env,
                    store,
                    w.this_exact,
                    w.enclosing_class,
                    out,
                );
                // Userland function arity (ADR-0049 §6 / S5): judged once in the
                // plain per-scope pass, like the checks below.
                if descent.is_none() {
                    check_arity(cx, folder, call, store, scope.poisoned, out);
                    // Printf-family arity (ADR-0078, issue #188): a folded literal
                    // format string demanding more placeholders than proven.
                    check_printf_arity(cx, folder, call, env, scope.poisoned, out);
                    // Existence flagship (ADR-0049 §3 / S4): a call to a
                    // provably-undefined function behind a clear dam. The branch
                    // store carries the FP-15 `function_exists` vouch.
                    check_undefined_function(cx, folder, call, store, out);
                    // Pattern-refusal check (ADR-0078 / issue #189): a `preg_*`
                    // call whose proven-literal pattern the project's own PCRE
                    // refuses to compile.
                    check_preg_pattern(w, folder, call, env, out);
                    // Dump surface (ADR-0053 D3/D4): a recognized
                    // `PHPStan\dumpType`-family or `var_dump` call emits its fact
                    // rendering here. A statement-position call also hands its
                    // statement span down, so its finding can carry the
                    // statement-deletion fix payload (ADR-0010, issue #114).
                    let removal =
                        matches!(stmt.kind, StmtKind::Call(_)).then_some(stmt.span);
                    // Discarded calls (ADR-0096, issue #320): the statement
                    // IS the call, so its result is unused by construction,
                    // and the oracle answers what the callee's summary
                    // proves. Same statement span the dump family's deletion
                    // fix uses — the thing a reader would delete.
                    // `value_position` is the one thing `StmtKind::Call`
                    // cannot say for itself: a `match` arm's body lowers to
                    // one and its result is the construct's value.
                    if let Some(span) = removal
                        && !stmt.value_position
                    {
                        crate::no_effect::check_no_effect(cx, span, call, before, out);
                    }
                    emit_dumps(w, folder, call, env, store, removal, out);
                    // Oracle idea B (harness-only): when the assertType sink is
                    // installed, record this call's (expected, rendering) pair —
                    // a no-op in every normal check (sink absent).
                    emit_asserts(w, folder, call, env, store);
                }
                stmt_summary = try_descend_function(
                    cx, folder, call, env, store, scope.poisoned, descent.as_mut(), out,
                );
                // The declared floor, resolved with this call's own arguments in
                // hand (issue #363): a function-level `@template T` bound from
                // an argument's carry lets the callee's `@return T` name a type
                // here. Read at the same point the receiver twin is — before the
                // statement's escape/sweep pass — so the carry the read wants is
                // still the one the call was made against.
                let bindable = bindable_args(call);
                stmt_return_arms = cx.resolve_user_fn_any(call).and_then(|site| {
                    fn_return_arms_at_call(
                        cx,
                        folder,
                        site,
                        &bindable,
                        env,
                        store,
                        scope.poisoned,
                    )
                });
            }
            Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
                // Branch-sensitive null-dereference proof (ADR-0031): a `$v->m()`
                // whose receiver is proven `Singleton(null)` on this path.
                check_call_on_null(w, call, env, store, out);
                // Sibling on the same receiver fact (ADR-0078, issue #190): a
                // `$v->m()` whose receiver is proven a non-null non-object — same
                // fatal, different id, disjoint from the null case by construction.
                check_call_on_non_object(w, call, env, store, out);
                // Absence flagship (ADR-0049 §4 / S2): fire only in the plain
                // per-scope pass — a descent must not re-judge the same site.
                if descent.is_none() {
                    // Absence flagship's positive twin (ADR-0078, issue #185): the
                    // method IS there, hidden by declared visibility.
                    check_inaccessible_method(w, call, store, out);
                    check_undefined_method(cx, folder, call, store, scope.poisoned, out);
                    // Declared-receiver lane (ADR-0049 §8 / S6): a method absent on
                    // a phpdoc-declared receiver narrowed by branch analysis.
                    // Disjoint from S2 by construction — S2 fires on class_exact
                    // receivers, S6 only on non-exact ones with a narrowed arm lane.
                    check_phpdoc_undefined_method(cx, folder, call, store, scope.poisoned, out);
                    // Method / constructor / static arity (ADR-0049 §6 / S5), under
                    // a proven-exact receiver only (the declared-receiver variant
                    // is unsound — see `resolve_arity_method`).
                    check_arity(cx, folder, call, store, scope.poisoned, out);
                }
                let outcome = handle_method_call(
                    cx,
                    folder,
                    scope,
                    call,
                    env,
                    store,
                    w.this_exact,
                    w.enclosing_class,
                    descent.as_mut(),
                    out,
                );
                // ADR-0075: a resolved method/static summary rebinds on the same
                // rungs as a function's. A constructor keeps its exactness lane
                // (ADR-0036) and takes the other channel: its `$this` snapshot,
                // which the object build binds later in this statement (ADR-0057
                // C7).
                if matches!(call.receiver, Callee::Construct { .. }) {
                    stmt_ctor_heap = outcome.ctor_heap;
                } else {
                    stmt_summary = outcome.summary;
                    stmt_return_arms = outcome.return_arms;
                }
                // …and, for a call that ran with a `$this` seeded from a caller
                // object, the snapshot step 1a copies back (D4).
                stmt_this_backs.extend(outcome.this_back);
            }
            // `$fn(...)` — resolve the callee variable against the env: a proven
            // closure value descends into its scope (ADR-0033), a proven string
            // resolves as a function name.
            Callee::DynamicVar(name) => {
                // Issue #128: a `$fn(...)` on a proven closure rebinds its
                // return summary on the same rungs as free functions / methods.
                let outcome = handle_var_call(
                    cx, folder, scope, name, call, env, store, descent.as_mut(), out,
                );
                stmt_summary = outcome.summary;
                if stmt_return_arms.is_none() {
                    stmt_return_arms = outcome.return_arms;
                }
            }
            Callee::Dynamic => {}
        }
    }
    StmtCalls {
        summary: stmt_summary,
        return_arms: stmt_return_arms,
        ctor_heap: stmt_ctor_heap,
        this_backs: stmt_this_backs,
    }
}
