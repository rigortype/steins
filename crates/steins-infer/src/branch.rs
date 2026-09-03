//! Structured branching: `if` / `elseif` / `else` and `match` walks, guard-chain
//! coverage (ADR-0088), per-arm refinement and the no-match path.

use std::collections::HashMap;

use steins_contract::normalize;
use steins_domain::{Certainty, Fact, Val};
use steins_phpdoc::AssertKind;
use steins_syntax::{ArgValue, CallExpr, CmpOp, CondExpr, CondOperand, MatchArmT, Span, Stmt};

use crate::fold::Folder;
use crate::annotate::LineFact;
use crate::asserts::{
    apply_guard_asserts, cond_branch_scoped_invalidations, cond_invalidations,
    guard_assert_kept_lanes,
};
use crate::cond::{eval_cmp, eval_cond, operand_values};
use crate::contract::ProjectIsa;
use crate::descent::escape_and_sweep_calls;
use crate::env::{ContractArm, Descent, Known, Store, Stratum, join_envs, val_of};
use crate::existence::existence_vouch;
use crate::out_params::{check_preg_pattern, seed_out_params};
use crate::predicates::apply_type_narrowing;
use crate::project::Diagnostic;
use crate::refine::{
    Refine, apply_class_narrowing, apply_refinements, collect_guard_calls, collect_guard_calls_any,
    collect_same_expr_call_guards, else_refinements, subtract_contract_lane, then_refinements,
};
use crate::shapes::{ShapeGuard, apply_shape_guard, apply_shape_narrowing, guard_key};
use crate::walk::{Flow, WalkCx, mark_dead, walk_trace};

/// Walk a structured `if`/`elseif`/`else` (ADR-0031 stage 1). Evaluates the guard
/// to a [`Certainty`], walks each **live** branch on a cloned env (applying
/// positive refinement), then joins the envs of the branches that fall through.
/// When no live branch falls through, the code after the `if` is unreachable and
/// the whole construct terminates.
///
/// `chain` is `Some` only when this whole `if`/`elseif` construct IS a `match
/// (true)`/`match (false)` guard chain with no `default` (ADR-0088 §5, issue
/// #448) — computed once by [`walk_trace`] before the first call and threaded
/// unchanged through every recursive `walk_if`/`walk_else` pair for the SAME
/// chain, never recomputed. [`walk_else`]'s own terminal case is where it is
/// finally consulted.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_if(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then_trace: &[Stmt],
    elseifs: &[(CondExpr, Vec<Stmt>)],
    else_trace: Option<&[Stmt]>,
    chain: Option<&GuardChainCoverage>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let poisoned = w.scope.poisoned;
    // 1. Evaluate the guard in the pre-branch env (short-circuit env refinement is
    // stage 2 — each condition sees the same entry env).
    let verdict = eval_cond(w, folder, cond, env, store, poisoned);

    // A decided guard proves the skipped side dead — record it so the env-free
    // direct pass never reports inside it (live-path discipline, ADR-0002/0031).
    match verdict {
        Certainty::Yes => {
            for (_, trace) in elseifs {
                mark_dead(w, &[trace.as_slice()]);
            }
            if let Some(trace) = else_trace {
                mark_dead(w, &[trace]);
            }
        }
        Certainty::No => mark_dead(w, &[then_trace]),
        Certainty::Maybe => {}
    }

    // 2. The guard's own effects on *every* resulting path, sequenced at the calls'
    // positions (ADR-0052 §6): a retained guard call escapes its object
    // arguments/receiver and sweeps the escaped objects' mutable props (the method
    // receiver's binding survives, letting `$x !== null && $x->m()` keep a
    // proven-non-null receiver), then by-ref argument invalidation and opaque reads
    // are forgotten. Both apply before the branch clones.
    let guard_calls: Vec<&CallExpr> = collect_guard_calls_any(cond);
    escape_and_sweep_calls(w, &guard_calls, store, &[]);
    // The pattern-refusal check at guard position (ADR-0078 / issue #189):
    // `if (preg_match('/…/', $s))` is the idiom the id is about, and a guard
    // condition is not a `checkable_calls` position. An `elseif` chain recurses
    // into `walk_if` per link, so each condition is judged exactly once.
    if descent.is_none() {
        for call in &guard_calls {
            check_preg_pattern(w, folder, call, env, out);
        }
    }
    // The declared-arm lanes a guard call's own `@phpstan-assert-*` tag names, taken
    // before the conservative read-set drop below and put back after it — same
    // rationale as the statement-position rule (walk_trace step 3 before step 4):
    // the tag is worthless if the call's blanket invalidation erases the lane it
    // narrows.
    //
    // The lift is minimal: arm lane only (value lane and `Member` sets still drop);
    // only for a variable the callee takes by value at the asserted position
    // (ADR-0070's gate); and only when no other call in the condition touches it.
    let kept_lanes = guard_assert_kept_lanes(w, cond, &guard_calls, env, store);
    let invalidated = cond_invalidations(w.cx, cond, env, store, poisoned);
    // The presence guard's key (issue #536): forgotten for the branches this
    // guard decides, and handed back to each of them at its exit, so the
    // forgetting never outlives the construct. The snapshot is taken BEFORE the
    // drop below, which is the only place the pre-branch lanes still exist.
    let branch_scoped = cond_branch_scoped_invalidations(w.cx, cond, &invalidated);
    let restore: Vec<BranchScopedLanes> = branch_scoped
        .iter()
        .map(|v| (v.clone(), env.get(v).cloned(), store.contract.get(v).cloned()))
        .collect();
    for v in invalidated {
        env.remove(&v);
        store.unbind(&v);
    }
    for v in &branch_scoped {
        env.remove(v);
        store.unbind(v);
    }
    for (var, arms) in kept_lanes {
        store.contract.insert(var, arms);
    }

    // 3. Walk the live branches on cloned envs, collecting those that fall through.
    let mut fell: Vec<(HashMap<String, Known>, Store)> = Vec::new();

    // Guard calls carrying `-if-true`/`-if-false` envelopes, collected per branch
    // polarity through the `&&`/`||` structure (ADR-0052 §6, extending N2's
    // top-level-only consumption into nested positions). Each carries whether the
    // call returned `true` on that branch. Specs apply at `Asserted` stratum (§5).
    if verdict != Certainty::No {
        let mut benv = env.clone();
        let mut bclasses = store.clone();
        apply_cond_side(w, folder, cond, true, &mut benv, &mut bclasses);
        if walk_trace(w, folder, then_trace, &mut benv, &mut bclasses, descent, facts, true, out)
            == Flow::FellThrough
        {
            restore_branch_scoped(&restore, &mut benv, &mut bclasses);
            fell.push((benv, bclasses));
        }
    }

    if verdict != Certainty::Yes {
        let mut benv = env.clone();
        let mut bclasses = store.clone();
        apply_cond_side(w, folder, cond, false, &mut benv, &mut bclasses);
        if walk_else(w, folder, elseifs, else_trace, chain, &mut benv, &mut bclasses, descent, facts, out)
            == Flow::FellThrough
        {
            restore_branch_scoped(&restore, &mut benv, &mut bclasses);
            fell.push((benv, bclasses));
        }
    }

    // 4. Merge. No live fall-through → the successor is unreachable.
    if fell.is_empty() {
        return Flow::Terminated;
    }
    let (jenv, jclasses) = join_envs(fell);
    *env = jenv;
    *store = jclasses;
    Flow::FellThrough
}

/// Apply one side of a decided-or-not condition to an env/store pair: everything
/// a branch taken under `then` polarity knows because that condition held.
///
/// Extracted from [`walk_if`]'s two branch blocks, which is where the ordering
/// constraints below were established; a `while` header's body entry ([`walk_while`])
/// takes the same true-side application, for the same reason an `if`'s then-branch
/// does — the condition was evaluated true immediately before the code that follows.
///
/// The four vocabularies run in a fixed order. The DR2 type vocabulary is first
/// because it is the only one that can mint a fact over an unfacted binding, which
/// the scalar refinements must then see — `is_string($v) && $v !== ''` narrows to
/// `non-empty-string` only in this order.
fn apply_cond_side(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    apply_type_narrowing(w.cx, cond, then, env, store);
    let refs = if then {
        then_refinements(cond, w.cx.php_minor)
    } else {
        else_refinements(cond, w.cx.php_minor)
    };
    apply_refinements(&refs, env, store, Stratum::Verified);
    apply_class_narrowing(w, cond, then, store);
    apply_shape_narrowing(w.cx, cond, then, env, store, true);
    // Same-expression call guards (issue #421): a call has no binding to narrow, so
    // this records the guard rather than a fact — only on the side where the "not
    // null"/"not false" reading actually holds, so a sibling branch that never
    // tested the expression does not inherit it through the join (`join_stores`
    // intersects, same as `vouched`). The `false` polarity is the twin reading,
    // `if ($e === null) {} else { <here> }`.
    let mut guards = Vec::new();
    collect_same_expr_call_guards(cond, then, &mut guards);
    for v in guards {
        store.guard_call(v);
    }
    let mut calls = Vec::new();
    collect_guard_calls(cond, then, &mut calls);
    for (call, returns_true) in calls {
        let kind = if returns_true { AssertKind::IfTrue } else { AssertKind::IfFalse };
        apply_guard_asserts(w, call, kind, env, store);
        // Guard-respect leg (ADR-0049 §4): an existence guard vouches its symbol on
        // the side where it holds true — including the negated-guard else-branch,
        // `if (!method_exists(...)) {} else <here>`.
        if returns_true && let Some(v) = existence_vouch(w.cx, store, call) {
            store.vouch(v);
        }
    }
    // The out-parameter seed on both sides, for the same reason: `if
    // (!preg_match($re, $s, $m)) { return; }` reaches its else-branch with the call
    // proven truthy (ADR-0077 §3.1) — the polarity the witness survives under decides.
    seed_out_params(w, folder, cond, then, env, store);
}

/// Walk a structured `while` body (ADR-0027 amendment, issue #649).
///
/// `benv`/`bstore` are the body's **entry** pair, built by the caller from what the
/// loop provably cannot change (`loop_entry_forget`), and this consumes them: a
/// loop body contributes **findings**, never facts. The body's exit env is
/// discarded, so the code after the loop sees exactly what the construct's own
/// sets left standing, and carrying the negated condition out of a break-free loop
/// stays the separate question it is (issue #651).
///
/// The entry env needs no fixpoint. Nothing in it is specific to one iteration —
/// every name the loop can rebind is forgotten in it and the mutable state of every
/// object it still names has been swept — and PHP evaluates the header before
/// **every** entry to the body, the first included, so the true-side application is
/// exactly as sound here as it is on an `if`'s then-branch. A body whose last
/// statement reassigns the subject the header narrowed — the parent-pointer
/// traversal that motivated the slice — is therefore no obstacle: the next
/// iteration's entry re-derives the fact from the header.
///
/// A header the walk decides is false runs its body zero times, so the body is not
/// walked. That reading is taken in the entry env for the same reason the narrowing
/// is: the env holds at every evaluation of the header, so a `No` there is a `No`
/// at all of them. The region is not marked dead: the env-free direct pass reports
/// there today, and withdrawing those findings is a separate judgment from adding
/// these.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_while_body(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    body: &[Stmt],
    mut benv: HashMap<String, Known>,
    mut bstore: Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) {
    if eval_cond(w, folder, cond, &benv, &bstore, w.scope.poisoned) == Certainty::No {
        return;
    }
    apply_cond_side(w, folder, cond, true, &mut benv, &mut bstore);
    // The body's own `Flow` is discarded: a body that terminates on every path
    // terminates an ITERATION, and a `while` whose condition is not decided may run
    // none at all, so the successor stays reachable either way.
    let _ = walk_trace(w, folder, body, &mut benv, &mut bstore, descent, facts, true, out);
}

/// One name's pre-branch value and declared-arm lanes, held across the branches
/// a guard decides so [`restore_branch_scoped`] can hand them back.
type BranchScopedLanes = (String, Option<Known>, Option<Vec<ContractArm>>);

/// Put back the lanes a branch-scoped guard forgetting took (issue #536), at
/// the exit of a branch that falls through to the join.
///
/// Skipped for a name the branch **rebound itself**: whatever it holds now came
/// from an assignment, and the pre-branch lanes describe a value that is gone.
/// A branch that never falls through is never restored, which is the point —
/// `if (array_key_exists($k, $a)) { return; }` leaves nothing to join from that
/// side, and the surviving side carries the key it always had.
///
/// The two lanes are the two the forgetting cost: the value binding and the
/// declared arms. Deliberately not the heap/`Member` lanes — a key tested by
/// `array_key_exists` is a scalar, and [`guard_assert_kept_lanes`] scopes its
/// own put-back the same way.
fn restore_branch_scoped(
    restore: &[BranchScopedLanes],
    benv: &mut HashMap<String, Known>,
    bstore: &mut Store,
) {
    for (var, known, arms) in restore {
        if benv.contains_key(var) || bstore.contract.contains_key(var) {
            continue;
        }
        if let Some(k) = known {
            benv.insert(var.clone(), k.clone());
        }
        if let Some(a) = arms {
            bstore.contract.insert(var.clone(), a.clone());
        }
    }
}

/// Walk the `else` side of an `if`: the `elseif` chain desugars to a nested
/// `if`/`else`; the terminal `else` (if any) is a plain sub-trace; an absent
/// `else` falls through unchanged (the negated-guard path) — UNLESS `chain`
/// names this construct as a desugared `match (true)`/`match (false)` guard
/// chain (ADR-0088 §5, issue #448), in which case the fall-through is exactly
/// the no-`default` `\UnhandledMatchError` path and asks the same coverage
/// question [`walk_match`] asks for a by-value `match`, off the SAME
/// accumulated `store` the ordinary `elseif` recursion already built.
#[allow(clippy::too_many_arguments)]
fn walk_else(
    w: &WalkCx,
    folder: &mut dyn Folder,
    elseifs: &[(CondExpr, Vec<Stmt>)],
    else_trace: Option<&[Stmt]>,
    chain: Option<&GuardChainCoverage>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    match elseifs.split_first() {
        Some(((cond, trace), rest)) => {
            walk_if(w, folder, cond, trace, rest, else_trace, chain, env, store, descent, facts, out)
        }
        None => match else_trace {
            Some(stmts) => walk_trace(w, folder, stmts, env, store, descent, facts, true, out),
            None => {
                if let Some(gc) = chain
                    && store.contract_narrowed(&gc.subject)
                    && !store.contract_emptied(&gc.subject)
                {
                    w.uncovered_matches.borrow_mut().push(gc.span);
                }
                Flow::FellThrough
            }
        },
    }
}

/// The desugared `match (true)`/`match (false)` guard-chain coverage question
/// (ADR-0088 §5, issue #448), computed once by [`walk_trace`] and threaded
/// unchanged through one `if`/`elseif` chain's recursive walk: `span` is the
/// original `match`'s own span — the same one [`ThrowKind::New`]'s synthetic
/// `UnhandledMatchError` fact for the same construct already carries — and
/// `subject` is the one variable every arm condition in the chain addresses
/// ([`guard_chain_subject`]).
pub(crate) struct GuardChainCoverage {
    pub(crate) span: Span,
    pub(crate) subject: String,
}

/// The chain-wide subject of a desugared `match (true)`/`match (false)` guard
/// chain (issue #448): the single variable every arm condition — the leading
/// `cond` plus every `elseifs` link — mentions, when there is exactly one such
/// variable across the whole chain. `None` when the chain mentions zero
/// variables (nothing to check) or more than one distinct variable, declining
/// rather than guessing: whether a MULTI-variable chain is exhaustive is a
/// joint-domain question one variable's [`Store::contract`] lane cannot answer,
/// and reading a lane no guard in THIS chain actually narrowed as evidence is
/// exactly the false-positive shape this run's hazard note warns about.
pub(crate) fn guard_chain_subject(cond: &CondExpr, elseifs: &[(CondExpr, Vec<Stmt>)]) -> Option<String> {
    let mut vars = Vec::new();
    collect_cond_vars(cond, &mut vars);
    for (c, _) in elseifs {
        collect_cond_vars(c, &mut vars);
    }
    vars.sort();
    vars.dedup();
    match vars.split_first() {
        Some((only, [])) => Some(only.clone()),
        _ => None,
    }
}

/// Every bare variable name a condition mentions, anywhere — not narrowing
/// specifically, just *mentioned*, which is the conservative (over-inclusive)
/// reading [`guard_chain_subject`] needs: a condition that touches a second
/// variable in any way at all is reason enough to decline picking one.
fn collect_cond_vars(cond: &CondExpr, out: &mut Vec<String>) {
    match cond {
        CondExpr::Cmp { lhs, rhs, .. } => {
            push_cond_operand_var(lhs, out);
            push_cond_operand_var(rhs, out);
        }
        CondExpr::Truthy(op) => push_cond_operand_var(op, out),
        CondExpr::Instanceof { operand, .. } => push_cond_operand_var(operand, out),
        CondExpr::Not(c) => collect_cond_vars(c, out),
        CondExpr::And(a, b) | CondExpr::Or(a, b) => {
            collect_cond_vars(a, out);
            collect_cond_vars(b, out);
        }
        // `InstanceofDyn`'s `reads` is carried for exactly this consumer (issue
        // #571): subject selection sees the same mention set the `Opaque`
        // lowering recorded, while the invalidation path no longer sees one.
        CondExpr::Call { reads, .. }
        | CondExpr::Opaque { reads }
        | CondExpr::InstanceofDyn { reads, .. } => out.extend(reads.iter().cloned()),
        CondExpr::Isset { var, .. } | CondExpr::IssetVar { var } => out.push(var.clone()),
    }
}

fn push_cond_operand_var(op: &CondOperand, out: &mut Vec<String>) {
    if let CondOperand::Var(v) = op {
        out.push(v.clone());
    }
}

/// Walk a structured statement-position `match`/`switch` (ADR-0031 Part B).
///
/// Per arm, the "taken" certainty is computed left to right with first-match
/// semantics: `taken(k) = Yes` iff arm `k` matches and every earlier arm provably
/// does not; `No` iff arm `k` provably does not match; `Maybe` otherwise — this
/// ordering rule stops a later `Yes` arm from being walked as sole-live while an
/// earlier arm is only `Maybe`. `No` arms are recorded dead; every other arm is
/// walked on a cloned env with the subject refined to the arm's literal set (a
/// `match` binds `Singleton`/`OneOf`; a `switch` binds nothing since its loose
/// `==` truth set is multi-valued).
///
/// The "no arm matched" outcome: with a `default` arm it runs that body (unless
/// dead); without one, a `switch` falls through unchanged while a `match` raises
/// `\UnhandledMatchError` (a terminator). Either way the no-match path carries the
/// arms' conditions **subtracted** from the subject ([`subtract_no_match_path`],
/// issue #439) — the negated-guard path an `elseif` chain has always modelled. The
/// successor env is the join of every branch that falls through; if none does, the
/// construct terminates.
///
/// A default-less `match` (not `switch`) additionally asks ADR-0088 §5's question
/// (issue #433): does the subtraction above prove the subject's Verified domain is
/// NOT exhausted? When it does — on the plain per-scope walk only, per
/// [`WalkCx::uncovered_matches`]'s own doc — `match_span` (this construct's own
/// span, the same one [`ThrowKind::New`]'s synthetic `UnhandledMatchError` origin
/// carries) is recorded there, for the throw system to read back and decide
/// whether the contribution is real.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_match(
    w: &WalkCx,
    folder: &mut dyn Folder,
    subject: &CondOperand,
    arms: &[MatchArmT],
    default: Option<&[Stmt]>,
    loose: bool,
    match_span: Span,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let poisoned = w.scope.poisoned;
    let op = if loose { CmpOp::Loose } else { CmpOp::Identical };
    let subj_vals = operand_values(subject, env, poisoned);

    // 1. Per-arm first-match "taken" certainty (left to right). `earlier_all_no`
    // tracks whether every earlier arm provably does NOT match; `decided_done`
    // records a decided match — every later arm and the default become unreachable.
    let mut takens: Vec<Certainty> = Vec::with_capacity(arms.len());
    let mut earlier_all_no = true;
    let mut decided_done = false;
    for arm in arms {
        if decided_done {
            takens.push(Certainty::No); // a prior sure match makes this unreachable
            continue;
        }
        let cond_k =
            eval_arm_cond(op, subj_vals.as_deref(), &arm.conditions, env, poisoned, w.cx.php_minor);
        let taken = match cond_k {
            Certainty::No => Certainty::No,
            Certainty::Yes if earlier_all_no => {
                decided_done = true;
                Certainty::Yes
            }
            _ => Certainty::Maybe,
        };
        if cond_k != Certainty::No {
            earlier_all_no = false;
        }
        takens.push(taken);
    }
    // The default / no-match path: `No` once a decided arm consumed the value;
    // `Yes` when every arm provably fails to match; else `Maybe`.
    let no_match_taken = if decided_done {
        Certainty::No
    } else if earlier_all_no {
        Certainty::Yes
    } else {
        Certainty::Maybe
    };

    // 2. Walk each live arm on a cloned env; record `No` arms dead.
    let mut fell: Vec<(HashMap<String, Known>, Store)> = Vec::new();
    for (arm, taken) in arms.iter().zip(&takens) {
        if *taken == Certainty::No {
            mark_dead(w, &[arm.trace.as_slice()]);
            continue;
        }
        let mut benv = env.clone();
        let mut bclasses = store.clone();
        refine_match_arm(subject, &arm.conditions, loose, &mut benv, w.cx.php_minor);
        refine_match_arm_enum_case(w, subject, &arm.conditions, loose, &mut bclasses);
        // Tag-based discrimination (ADR-0062 A-G4): a `match`/`switch` on a
        // constant-key projection subtracts the base's array arms by the field's
        // `admits` verdict, minting the collapsed shape into the arm's env. The
        // `default` arm refines nothing (a residue A-G4 does not model in v1).
        if let CondOperand::Offset { var, key } = subject
            && let Some(k) = guard_key(key, w.cx.php_minor)
            && let Some(tags) = arm_tag_literals(&arm.conditions)
        {
            apply_shape_guard(
                w.cx,
                &ShapeGuard::Tag { var: var.clone(), key: k, tags, loose },
                &mut benv,
                &mut bclasses,
                true,
            );
        }
        if walk_trace(w, folder, &arm.trace, &mut benv, &mut bclasses, descent, facts, true, out)
            == Flow::FellThrough
        {
            fell.push((benv, bclasses));
        }
    }

    // 3. The "no arm matched" outcome.
    match default {
        Some(dtrace) => {
            if no_match_taken == Certainty::No {
                mark_dead(w, &[dtrace]);
            } else {
                let mut benv = env.clone();
                let mut bclasses = store.clone();
                subtract_no_match_path(w, subject, arms, loose, &mut benv, &mut bclasses);
                if walk_trace(w, folder, dtrace, &mut benv, &mut bclasses, descent, facts, true, out)
                    == Flow::FellThrough
                {
                    fell.push((benv, bclasses));
                }
            }
        }
        None => {
            // A default-less `switch` falls through to after itself on no match —
            // carrying the same subtraction the `default` body would have carried;
            // a default-less `match` throws `\UnhandledMatchError`, a terminator
            // that joins nothing (and so has no path to refine). Both outcomes need
            // the identical subtraction, so it runs once regardless of `loose`.
            if no_match_taken != Certainty::No {
                let mut benv = env.clone();
                let mut bclasses = store.clone();
                subtract_no_match_path(w, subject, arms, loose, &mut benv, &mut bclasses);
                if loose {
                    fell.push((benv, bclasses));
                } else {
                    // ADR-0088 §5 (issue #433): does the subtraction just run prove
                    // the arms do NOT exhaust the subject's Verified domain? Read
                    // exactly the way #428's sentinel already reads the same
                    // question in the other direction (`check_never_sentinel`):
                    // [`Store::contract_narrowed`] is the chain-level evidence bit
                    // that separates a residue a real subtraction produced from a
                    // lane the arm conditions simply could not touch (a `switch`'s
                    // own over-approximate residue included — `loose` is handled
                    // above and never reaches here), and
                    // [`Store::contract_emptied`] is `true` only for a lane that
                    // was all-`Verified` AND emptied, never for one merely absent.
                    // A narrowed-but-non-empty residue is the missing-a-case
                    // shape; an un-narrowed or absent lane is ignorance, and stays
                    // silent (ADR-0002) exactly as the sentinel declines it.
                    //
                    // Plain per-scope walk only: a descent's `bclasses` reflects
                    // one caller's hypothetical bindings, not a fact this
                    // construct's own declaration earns (mirrors `dead`'s same
                    // restriction, and `Store::contract` is never even seeded on a
                    // descent — see `analyze_scope`'s own seeding gate).
                    if descent.is_none()
                        && let CondOperand::Var(name) = subject
                        && bclasses.contract_narrowed(name)
                        && !bclasses.contract_emptied(name)
                    {
                        w.uncovered_matches.borrow_mut().push(match_span);
                    }
                    // No `fell.push`: the construct still terminates here exactly
                    // as it always has — the coverage verdict decides whether the
                    // throw is REPORTABLE, never whether it happens.
                }
            }
        }
    }

    // 4. Merge. No live fall-through → the successor is unreachable.
    if fell.is_empty() {
        return Flow::Terminated;
    }
    let (jenv, jclasses) = join_envs(fell);
    *env = jenv;
    *store = jclasses;
    Flow::FellThrough
}

/// The certainty that a `match`/`switch` arm is the one taken *by value* — i.e.
/// the subject equals ANY of the arm's conditions (`===` for match, loose `==`
/// for switch). An unknown subject or condition contributes `Maybe`; the OR folds
/// the per-condition verdicts (any `Yes` → `Yes`, all `No` → `No`, else `Maybe`).
fn eval_arm_cond(
    op: CmpOp,
    subj_vals: Option<&[ArgValue]>,
    conditions: &[CondOperand],
    env: &HashMap<String, Known>,
    poisoned: bool,
    php_minor: Option<(u16, u16)>,
) -> Certainty {
    let Some(subj) = subj_vals else { return Certainty::Maybe };
    let mut acc = Certainty::No;
    for c in conditions {
        let cert = match operand_values(c, env, poisoned) {
            Some(cv) => eval_cmp(op, subj, &cv, php_minor),
            None => Certainty::Maybe,
        };
        acc = acc.or(cert);
        if acc == Certainty::Yes {
            return Certainty::Yes;
        }
    }
    acc
}

/// Refine the subject variable inside a matched arm's cloned env. A `match`
/// (strict `===`) whose subject is a bare variable and whose conditions are all
/// literals binds the subject to that exact finite set (`Singleton`/`OneOf`). A
/// `switch` (loose `==`) binds nothing: its truth set is multi-valued (`case 0`
/// matches `0`, `"0"`, `false`, `0.0`, …), so no single `Fact` is sound.
fn arm_tag_literals(conditions: &[CondOperand]) -> Option<Vec<ArgValue>> {
    if conditions.is_empty() {
        return None;
    }
    conditions
        .iter()
        .map(|c| match c {
            CondOperand::Literal(v) => Some(v.clone()),
            _ => None,
        })
        .collect()
}

fn refine_match_arm(
    subject: &CondOperand,
    conditions: &[CondOperand],
    loose: bool,
    env: &mut HashMap<String, Known>,
    php_minor: Option<(u16, u16)>,
) {
    if loose {
        return;
    }
    let CondOperand::Var(name) = subject else { return };
    let mut vals = Vec::with_capacity(conditions.len());
    for c in conditions {
        match c {
            CondOperand::Literal(v) => match val_of(v, php_minor) {
                Some(val) => vals.push(val),
                None => return,
            },
            _ => return,
        }
    }
    if let Some(fact) = Fact::from_vals(vals) {
        let line = env.get(name).map_or(0, |k| k.line);
        env.insert(name.clone(), Known::value(fact, line, Some("matched arm".to_owned())));
    }
}

/// Refine the subject's enum-case identity inside a matched arm (issue #433):
/// the arm-lane twin of [`refine_match_arm`], for the one identity
/// [`refine_match_arm`] cannot carry — an enum case is an object and has no
/// [`Val`], exactly the reason [`subtract_no_match_path`] gives for reading the
/// arm lane there too. Reuses [`apply_class_narrowing`]'s own positive-polarity
/// `Subtrahend::EnumCase` subtraction (the `$s === Suit::Hearts` guard's own
/// mechanism), so a matched arm and a taken guard branch narrow the same way
/// through the same call.
///
/// Only a **single-condition** arm narrows: `case Hearts, Spades => …` is a
/// disjunction ("is Hearts OR Spades") no single subtraction call spells, so a
/// multi-condition arm is left unrefined — the conservative direction every
/// unrepresentable shape takes (ADR-0002), matching [`refine_match_arm`]'s own
/// per-operand `return` on the first condition it cannot carry.
fn refine_match_arm_enum_case(
    w: &WalkCx,
    subject: &CondOperand,
    conditions: &[CondOperand],
    loose: bool,
    store: &mut Store,
) {
    if loose {
        return;
    }
    let CondOperand::Var(name) = subject else { return };
    let [CondOperand::ClassConst(sc, case)] = conditions else { return };
    let Some(enum_fqn) = w.cx.resolve_enum_case(sc, case, w.enclosing_class) else { return };
    let oracle = ProjectIsa { cx: w.cx, demote_catalog: w.cx.a11_demote_catalog() };
    subtract_contract_lane(
        store,
        name,
        &normalize::Subtrahend::EnumCase { enum_fqn, case: case.clone(), polarity: true },
        &oracle,
    );
}

/// Subtract every arm's conditions from the subject on the **no-match path**
/// (issue #439): the `default` body, and the fall-through of a `default`-less
/// `switch`. Reaching it means each arm was tried and each failed, so the path
/// carries the conjunction of the negated conditions — exactly the negated-guard
/// path an `elseif` chain has modelled since ADR-0031, and the one path
/// `walk_match` refined with nothing.
///
/// Because it is a **conjunction** of negations, each condition is subtracted on
/// its own: an arm mixing a subtractable literal with an unrepresentable operand
/// still contributes the literal. That is the mirror image of [`refine_match_arm`],
/// where the arm's conditions are a *disjunction* and one unrepresentable operand
/// therefore voids the whole arm's positive refinement.
///
/// Both ADR-0052 carriers are subtracted, through the machinery the guard path
/// already uses — no parallel mechanism:
///
/// * the **value lane**, via [`Refine::NotNull`] / [`Refine::Exclude`] at the
///   `Verified` stratum (a runtime comparison decided this path);
/// * the **arm lane**, via [`normalize::Subtrahend::Null`] /
///   [`normalize::Subtrahend::Value`], plus [`normalize::Subtrahend::EnumCase`] for
///   a `Enum::Case` arm condition — the one subtrahend the value lane cannot carry,
///   since an enum case is an object and has no [`Val`] (ADR-0052's 2026-08-18
///   note).
///
/// A condition whose subtraction is inexpressible subtracts nothing and leaves the
/// lane as wide as it arrived — the conservative direction every other guard takes
/// (ADR-0002).
///
/// # The residue is evidence only when EVERY condition landed
///
/// ADR-0088 §4's proven-narrowing rule reads [`Store::narrowed`] before it treats a
/// non-empty residue as reachability, because an untouched lane and a narrowed one
/// look alike. A `match` is a whole chain of subtractions at once, and the mark is
/// one bit per variable, so a chain where *some* conditions landed and others did
/// not would set it and hand a consumer a residue that is ignorance about the arms
/// it could not model. `match ($b) { null => …, true => …, false => … }` over a
/// `?bool` is the measured shape: the `null` arm dies, neither bool literal covers
/// the general `bool` arm, and the residue reads `bool` on a chain that is in fact
/// exhaustive. So the mark survives this construct only when every condition's
/// subtraction demonstrably landed — one that did not voids the evidence for the
/// whole no-match path, exactly as one unrepresentable arm condition voids the
/// construct's structuring. The narrowing itself is kept either way; it is only the
/// *claim* that is withheld.
///
/// # `switch` subtracts the same set, and its residue is never evidence
///
/// A `switch` compares loosely, so its no-match path proves `$s != c`, which
/// **implies** `$s !== c` (identity implies loose equality, so the failure of the
/// loose test carries the failure of the strict one). Subtracting the exact literal
/// is therefore sound for `switch` too — the same one-directional reading
/// [`collect_cmp_refine`] already applies to the failing branch of `$x == null`
/// (issue #391).
///
/// What does not carry over is the *converse*: `case 0` also consumes `"0"`,
/// `false` and `0.0`, and the loose-equal set of a literal is infinite, so it has
/// no finite subtrahend spelling. A `switch`'s modelled residue is therefore only
/// an **over-approximation** of what actually reaches the no-match path, where a
/// `match`'s is exact. That asymmetry decides what may be read off it: an *empty*
/// residue still proves emptiness (an over-approximation that is empty leaves
/// nothing underneath), but a *non-empty* one proves nothing, because the values it
/// still holds may be precisely the ones a loose comparison already consumed. So a
/// `switch` subtraction leaves the mark unset unconditionally. Silence is bought; a
/// finding is not.
///
/// [`collect_cmp_refine`]: crate::refine::collect_cmp_refine
fn subtract_no_match_path(
    w: &WalkCx,
    subject: &CondOperand,
    arms: &[MatchArmT],
    loose: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let CondOperand::Var(name) = subject else { return };
    let oracle = ProjectIsa { cx: w.cx, demote_catalog: w.cx.a11_demote_catalog() };
    // The mark an earlier guard on this path already set is this construct's to
    // keep, never to earn: only what happens below is judged.
    let was_narrowed = store.contract_narrowed(name);
    let mut every_condition_landed = true;
    for cond in arms.iter().flat_map(|a| a.conditions.iter()) {
        let landed = match cond {
            CondOperand::Literal(lit) => match val_of(lit, w.cx.php_minor) {
                Some(val) => {
                    let (refine, sub) = if matches!(val, Val::Null) {
                        (Refine::NotNull(name.clone()), normalize::Subtrahend::Null)
                    } else {
                        (
                            Refine::Exclude(name.clone(), val.clone()),
                            normalize::Subtrahend::Value(val),
                        )
                    };
                    apply_refinements(&[refine], env, store, Stratum::Verified);
                    subtract_contract_lane(store, name, &sub, &oracle)
                }
                None => false,
            },
            // An enum-case arm. The absence discipline decides whether the case
            // resolves at all; one that does not subtracts nothing, so no chain can
            // claim an exhaustion over a case set the declaration never proved.
            CondOperand::ClassConst(sc, case) => {
                match w.cx.resolve_enum_case(sc, case, w.enclosing_class) {
                    Some(enum_fqn) => subtract_contract_lane(
                        store,
                        name,
                        &normalize::Subtrahend::EnumCase {
                            enum_fqn,
                            case: case.clone(),
                            polarity: false,
                        },
                        &oracle,
                    ),
                    None => false,
                }
            }
            _ => false,
        };
        every_condition_landed &= landed;
    }
    if (loose || !every_condition_landed) && !was_narrowed {
        store.narrowed.remove(name);
    }
}
