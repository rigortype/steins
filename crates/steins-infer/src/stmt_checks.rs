//! The statement walk's checks that judge a statement without changing the env:
//! step 1z at the read positions, and step 1b on a `return`'s value.

use std::collections::HashMap;

use steins_domain::Certainty;
use steins_syntax::{ArgValue, Stmt, StmtKind};

use crate::fold::Folder;
use crate::{
    RETURN_MISMATCH_ID, arg_abstract_fact, contract_touches_class, describe_fact,
    is_pure_class_contract, phpdoc_object_guard_blind, rendered_cval,
};
use crate::absence::{check_undefined_class_const, check_undefined_property};
use crate::arg_check::{is_type_error, object_world_guard_blind};
use crate::contract::accepts;
use crate::env::{Descent, Known, Store, Stratum};
use crate::inaccessible::{check_inaccessible_class_const, check_inaccessible_property};
use crate::non_object::check_property_on_non_object;
use crate::offsets::{
    check_coalesce_final_arm, check_destructure_source, check_offset_read, check_shape_read,
};
use crate::project::Diagnostic;
use crate::return_maybe::check_maybe_return_mismatch;
use crate::string_context::check_string_contexts;
use crate::walk::WalkCx;

/// Step 1z of [`walk_trace`]: the checks that fire at the read positions this IR
/// spells, against the env and store the statement is entered with. Plain per-scope
/// pass only, which is the caller's gate: a descent must not re-judge the same site.
///
/// [`walk_trace`]: crate::walk::walk_trace
pub(crate) fn check_read_positions(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let scope = w.scope;
    // 1z. Offset family (ADR-0049 §7 / S3): fire `offset.missing` /
    // `offset.on-unsupported` at the whitelisted read positions only (A7) — a
    // plain assignment-RHS and a return operand whose value is directly an
    // `OffsetRead`. Judged once per site in the plain per-scope pass
    // (`descent.is_none()`), reading the pre-statement env (which already carries
    // this sub-trace's branch refinements — e.g. an `=== []` guard narrowing the
    // container to `Singleton([])`).
    if let StmtKind::Assign { value: ArgValue::OffsetRead { base, key }, span, .. }
    | StmtKind::Return { value: ArgValue::OffsetRead { base, key }, span, .. } = &stmt.kind
    {
        check_offset_read(cx, folder, base, key, env, scope.poisoned, *span, out);
        // The strict leg (ADR-0062 S6 / A-G10) at the SAME whitelisted position:
        // where `check_offset_read` judges a proven whole container, this judges
        // the *declared* shape. The two are disjoint by construction — a
        // `Fact::Shape` is `Asserted`, and `check_offset_read`'s operand gate
        // takes `Verified` facts only — so at most one of them ever fires here.
        check_shape_read(cx, base, key, env, scope.poisoned, *span, out);
    }

    // 1z-bis-a. The destructure source (issue #288), the third whitelisted read
    // position: `[$a, $b] = $m;` / `list($a, $b) = $m;` reads `$m[0]`, `$m[1]`
    // exactly as the assignment-RHS position reads `$m[0]`, and PHP warns per
    // absent key. The targets are writes and stay silent (audit note G7(e)).
    if let StmtKind::Destructure { source, call, reads, span } = &stmt.kind {
        check_destructure_source(
            w, folder, source, call.as_ref(), reads, env, store, *span, out,
        );
    }

    // 1z-bis. The `??` final arm (ADR-0062 S6, issue #51 §2): a coalesce operand
    // is a silence carrier for every arm it protects, but the right-most arm is a
    // plain read — the value whenever everything left fell through. Judged under
    // the accumulated `¬isset` premise ladder S5 built.
    if let StmtKind::Assign { value: value @ ArgValue::Coalesce(..), span, .. }
    | StmtKind::Return { value: value @ ArgValue::Coalesce(..), span, .. } = &stmt.kind
    {
        check_coalesce_final_arm(cx, value, env, scope.poisoned, *span, out);
    }

    // 1z-ter. `property.on-non-object` (ADR-0078, issue #190) at the same
    // whitelisted read positions the offset family uses (A7), against the
    // pre-statement env — so a branch that narrowed the receiver is already in
    // force. Argument/echo/condition positions are outside the whitelist,
    // like `offset.missing`.
    if let StmtKind::Assign { value: ArgValue::PropFetch { var, prop }, span, .. }
    | StmtKind::Return { value: ArgValue::PropFetch { var, prop }, span, .. } = &stmt.kind
    {
        check_property_on_non_object(cx, var, prop, env, scope.poisoned, *span, out);
    }

    // 1z-ter. String context (ADR-0078, issue #193): every value this statement
    // hands to PHP's string conversion, judged against the pre-statement env,
    // the env PHP evaluates the operands in.
    check_string_contexts(w, folder, stmt, env, store, out);

    // 1z-quater. `property.inaccessible` / `class-const.inaccessible` (ADR-0078,
    // issue #185) at the member-access positions this IR spells: the same two
    // whitelisted read positions the offset family uses, plus the property write
    // statement, itself a member access (`Cannot access private property C::$p`
    // is witnessed both directions).
    match &stmt.kind {
        StmtKind::Assign { value: ArgValue::PropFetch { var, prop }, span, .. }
        | StmtKind::Return { value: ArgValue::PropFetch { var, prop }, span, .. } => {
            check_inaccessible_property(w, var, prop, store, false, *span, out);
            // Absence twin (ADR-0078, issue #197), read position only — the
            // write side is `property.dynamic-write`, deferred with its own
            // design. Disjoint by construction from the inaccessible check
            // above (that requires a *declared* property).
            check_undefined_property(w, folder, var, prop, store, *span, out);
        }
        StmtKind::PropAssign { target_var, prop, span, .. } => {
            check_inaccessible_property(w, target_var, prop, store, true, *span, out);
        }
        StmtKind::Assign { value: ArgValue::ClassConst(sc, name), span, .. }
        | StmtKind::Return { value: ArgValue::ClassConst(sc, name), span, .. } => {
            check_inaccessible_class_const(w, sc, name, *span, out);
            check_undefined_class_const(w, folder, sc, name, *span, out);
        }
        _ => {}
    }
}

/// Step 1b of [`walk_trace`]: a `return`'s value against the enclosing function's
/// native return type and its `@return` contract. Nothing for any other statement.
///
/// [`walk_trace`]: crate::walk::walk_trace
pub(crate) fn check_return_value(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
    descent: &mut Option<Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let scope = w.scope;
    if let StmtKind::Return { value, span, .. } = &stmt.kind {
        let mut native_fired = false;
        // A proven scalar (env/fold), or a proven object / class constant
        // (ADR-0043 stage 3). Stratum rides with the resolution (issue #127): an
        // Asserted fold must not launder to Verified via a syntactic re-read.
        let ret_resolved: Option<(ArgValue, Stratum)> = cx
            .resolve_literal_strat_ex(
                value,
                env,
                scope.poisoned,
                folder,
                descent.as_mut(),
                Some(&mut *out),
            )
            .or_else(|| {
                cx.resolve_static_value(value, w.enclosing_class)
                    .map(|v| (v, Stratum::Verified))
            });
        // The native return check is proof-layer (`type.return-mismatch`): a
        // returned value proven only through an `Asserted` fact stays silent
        // (ADR-0052 §5). The phpdoc contract check below accepts `Asserted`.
        if let Some((ret, display)) = w.ret_info
            && let Some((lit, strat)) = ret_resolved.as_ref()
            && *strat == Stratum::Verified
            && is_type_error(cx, ret, lit)
            && !object_world_guard_blind(descent.is_some(), ret, lit)
        {
            out.push(cx.return_diagnostic(span.start, lit, ret, display));
            native_fired = true;
        }
        if !native_fired
            && let Some((pret, display)) = w.ret_phpdoc
        {
            // Proven-value path, then the abstract-fact path (Feature E) — same
            // discipline as `@param`: only a definite `No`.
            let rendered = match cx.resolve_cval(value, env, store, scope.poisoned, folder) {
                // ADR-0043 stage 4: class-touching verdict is guard-blind inside a
                // descent, mirroring `object_world_guard_blind`.
                Some(cv) => (accepts(cx, cx.cur, span.start, pret, &cv) == Certainty::No
                    && !phpdoc_object_guard_blind(descent.is_some(), pret, Some(&cv)))
                .then(|| rendered_cval(&cv)),
                None => arg_abstract_fact(value, env, scope.poisoned).and_then(|fact| {
                    let cty = steins_contract::lower(pret);
                    // The class valve opens for a pure known-class contract against
                    // a definite scalar fact (see `check_phpdoc_param`).
                    let open_class_valve = is_pure_class_contract(cx, cx.cur, span.start, pret)
                        && !phpdoc_object_guard_blind(descent.is_some(), pret, None);
                    ((!contract_touches_class(&cty) || open_class_valve)
                        && steins_contract::admits_fact(&cty, fact) == Certainty::No)
                        .then(|| describe_fact(fact))
                }),
            };
            if let Some(rendered) = rendered {
                let pos = cx.tree().position(span.start);
                out.push(Diagnostic {
                    id: RETURN_MISMATCH_ID,
                    facet: None,
                    fix: None,
                    path: cx.path().to_owned(),
                    line: pos.line,
                    column: pos.column,
                    message: format!(
                        "return value {rendered} violates declared @return {pret} of {display}() — declared contract violation",
                    ),
                });
            }
        }
        // The possibly-grade sibling (ADR-0081 §8's 2026-08-27 amendment, issue
        // #537), placed where its argument twin is: after the native proof had
        // its chance, so a definite No is never shadowed by the weaker claim
        // about the same `return`. It rides beside the phpdoc contract check
        // rather than behind it, since the two judge different declarations.
        if !native_fired
            && let Some((ret, display)) = w.ret_info
        {
            check_maybe_return_mismatch(
                cx,
                ret,
                display,
                value,
                env,
                store,
                scope.poisoned,
                span.start,
                out,
            );
        }
    }
}
