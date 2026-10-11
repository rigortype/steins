//! The statement walk's checks that judge a statement without changing the env:
//! step 1z at the read positions, and step 1b on a `return`'s value.
//!
//! # Member and offset reads (ADR-0104, issues #936 and #949)
//!
//! Every offset, property and class-constant read the statement records
//! ([`Stmt::read_positions`]) is judged at its own span (decision D1), against the
//! statement's entry env, through the carrier the lowering classified it with. The
//! families split by what the answer depends on:
//!
//! * **offset** (`offset.missing`, `offset.on-unsupported`, `offset.undeclared`,
//!   `offset.maybe-missing`) and **property value** (`property.undefined`,
//!   `property.maybe-undefined`, `property.on-non-object`) read the env or the
//!   receiver's binding: value-dependent;
//! * **property access** (`property.inaccessible`) reads the receiver's exact
//!   allocation, which no test can change, and PHP routes a by-reference fetch through
//!   the same `__get` lookup as a read (witnessed PHP 8.5: `byref($c->priv)` throws
//!   `Cannot access private property`, and with only `__get` declared it calls it);
//! * **class constant** (`class-const.undefined`, `class-const.inaccessible`) reads no
//!   env at all: env-independent.
//!
//! | Carrier | offset | property value | property access | class constant |
//! | --- | --- | --- | --- | --- |
//! | `Plain` | judge: warns | judge: warns | judge: Error | judge: Error |
//! | `Argument`, by-value parameter of a resolved function | judge: warns | judge: warns | judge: Error | judge: Error |
//! | `Argument`, by-reference parameter | decline: the key is created, silent | decline: the property is created, silent | judge: Error, fetched through `__get` like a read | judge: Error |
//! | `Argument` of a method, constructor or unresolved callee, or `position: None` | decline: the parameter is unknowable (D5) | decline (D5) | judge | judge |
//! | `Conditional` (ternary arm, right of `&&` `\|\|` `??`, past `?->`) | decline: the test may establish the key (D3) | decline (D3) | judge (D3) | judge (D3) |
//! | `IssetLike` | not recorded: silent | not recorded: silent | not recorded: silent | judge: Error (`A::K ?? 1` throws) |
//! | `Suppressed` (`@`) | decline: warns, silent at the default floor (D2) | decline (D2) | judge: an Error is not suppressible | judge: Error |
//! | `CompoundRead` (`++`, `--`, `.=`) | judge `offset.missing` and the strict leg: warns; not `offset.on-unsupported`, since a null base is created and a scalar one throws | decline: on null it is the Error `Attempt to assign property`, and a written name is a write obstacle anyway | decline: the write half's `__set` route is not modelled | not a target |
//!
//! Two rules sit across the table. A position inside an extent the guard sweep proved
//! dead (a decided `defined()` or `function_exists()` test, [`mark_dead_operand`]) is
//! never judged. And a read of a variable (offset, property value and property access)
//! declines when the same statement may rebind that variable, or its key variable,
//! before the read runs: a by-reference or unresolved call anywhere in the statement,
//! or an embedded assignment (`f($a) . $a['k']` with `f(&$a)`, `($a['k'] = 1) +
//! $a['k']`). Only a name every call provably takes by value keeps the entry env's
//! binding ([`by_value_survivors`]). In the top-level frame, whose locals are the
//! globals, any call that may run userland can rebind every name (`global $a;`), so
//! such a statement judges none of these reads.
//!
//! The right-most arm of a `??` keeps its own premise ladder
//! ([`check_coalesce_final_arm`]), and a destructuring source its own key rule
//! ([`check_destructure_source`]); a finding one of them already raised at the same
//! place is not raised twice.
//!
//! [`Stmt::read_positions`]: steins_syntax::Stmt::read_positions
//! [`mark_dead_operand`]: crate::walk::mark_dead_operand
//! [`by_value_survivors`]: crate::by_value::by_value_survivors

use std::collections::{HashMap, HashSet};

use steins_domain::Certainty;
use steins_syntax::{ArgValue, Carrier, Key, Place, ReadKind, ReadPosition, Stmt, StmtKind};

use crate::fold::Folder;
use crate::{
    RETURN_MISMATCH_ID, arg_abstract_fact, contract_touches_class, describe_fact,
    is_pure_class_contract, phpdoc_object_guard_blind, rendered_cval,
};
use crate::absence::{check_undefined_class_const, check_undefined_property};
use crate::arg_check::{is_type_error, object_world_guard_blind};
use crate::value_tops::resolve_native_value;
use crate::by_value::{arg_is_by_value, by_value_survivors};
use crate::contract::accepts;
use crate::cx::Cx;
use crate::env::{Descent, Known, Store, Stratum};
use crate::inaccessible::{check_inaccessible_class_const, check_inaccessible_property};
use crate::non_object::check_property_on_non_object;
use crate::offsets::{
    check_coalesce_final_arm, check_destructure_source, check_offset_read, check_shape_read,
};
use crate::project::Diagnostic;
use crate::return_maybe::check_maybe_return_mismatch;
use crate::string_context::check_string_contexts;
use crate::walk::{WalkCx, in_dead};

/// Step 1z of [`walk_trace`]: the checks that fire at the read positions this IR
/// spells, against the env and store the statement is entered with. Plain per-scope
/// pass only, which is the caller's gate: a descent must not re-judge the same site.
/// The caller sweeps the statement's guard regions first, so a dead extent is known.
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
    let first = out.len();

    // 1z-bis-a. The destructure source (issue #288): `[$a, $b] = $m;` /
    // `list($a, $b) = $m;` reads `$m[0]`, `$m[1]`, and PHP warns per absent key.
    // The targets are writes and stay silent (audit note G7(e)).
    if let StmtKind::Destructure { source, call, reads, span } = &stmt.kind {
        check_destructure_source(
            w, folder, source, call.as_ref(), reads, env, store, *span, out,
        );
    }

    // 1z-bis. The `??` final arm (ADR-0062 S6, issue #51 §2): a coalesce operand
    // is a silence carrier for every arm it protects, but the right-most arm is a
    // plain read — the value whenever everything left fell through. Judged under
    // the accumulated `¬isset` premise ladder S5 built. Its read position is
    // `Conditional`, which the offset family declines below.
    if let StmtKind::Assign { value: value @ ArgValue::Coalesce(..), span, .. }
    | StmtKind::Return { value: value @ ArgValue::Coalesce(..), span, .. } = &stmt.kind
    {
        check_coalesce_final_arm(cx, value, env, scope.poisoned, *span, out);
    }

    // 1z-ter. String context (ADR-0078, issue #193): every value this statement
    // hands to PHP's string conversion, judged against the pre-statement env,
    // the env PHP evaluates the operands in.
    check_string_contexts(w, folder, stmt, env, store, out);

    // 1z-quater. `property.inaccessible` on the property write statement, itself a
    // member access (`Cannot access private property C::$p` is witnessed both
    // directions). The read side is a read position like any other, below.
    if let StmtKind::PropAssign { target_var, prop, span, .. } = &stmt.kind {
        check_inaccessible_property(w, target_var, prop, store, true, *span, out);
    }

    // 1z. The offset, property and class-constant reads, at every position the
    // statement writes them (the module doc's table).
    let mut found = Vec::new();
    check_member_reads(w, folder, stmt, env, store, &mut found);
    for d in found {
        let seen = out[first..]
            .iter()
            .any(|o| o.id == d.id && o.line == d.line && o.column == d.column);
        if !seen {
            out.push(d);
        }
    }
}

/// The family classes of the module doc's table, as far as a carrier treats them
/// differently.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Offset,
    PropertyValue,
    PropertyAccess,
    ClassConst,
}

/// The table's verdict for one family at one carrier: `None` declines, `Some(compound)`
/// judges, `compound` saying the read is the read half of a compound write.
fn carrier_verdict(
    cx: &Cx,
    positions: &[ReadPosition],
    carrier: Carrier,
    family: Family,
) -> Option<bool> {
    let judged = match (family, carrier) {
        (Family::Offset, Carrier::CompoundRead) => return Some(true),
        (Family::ClassConst | Family::PropertyAccess, Carrier::CompoundRead) => false,
        (Family::ClassConst, _) | (_, Carrier::Plain) => true,
        (Family::PropertyAccess, Carrier::Argument { .. }) => true,
        (_, Carrier::Argument { call, position }) => {
            argument_by_value(cx, positions, call, position)
        }
        (Family::PropertyAccess, Carrier::Conditional | Carrier::Suppressed) => true,
        _ => false,
    };
    judged.then_some(false)
}

/// Whether an argument binds to a parameter that is provably by value: a function
/// call the file records at the call position's span, resolved, with a by-value
/// parameter at a known position. A method, a constructor, an unresolved callee and
/// an unknown position (a named argument, one at or after a spread) answer `false`.
fn argument_by_value(
    cx: &Cx,
    positions: &[ReadPosition],
    call: Option<u32>,
    position: Option<u32>,
) -> bool {
    let (Some(call), Some(position)) = (call, position) else { return false };
    let Some(at) = positions.get(call as usize).filter(|p| matches!(p.kind, ReadKind::Call))
    else {
        return false;
    };
    // The file's calls are collected in source order; a miss is a decline.
    let calls = cx.tree().calls();
    let from = calls.partition_point(|c| c.span.start < at.span.start);
    calls[from..]
        .iter()
        .take_while(|c| c.span.start == at.span.start)
        .find(|c| c.span == at.span)
        .and_then(|c| c.callee_ref.as_ref())
        .is_some_and(|callee| arg_is_by_value(cx, callee, position))
}

/// The variables an offset read's place and key name: the bindings the read depends on.
fn place_vars<'p>(place: &'p Place, key: &'p Key, names: &mut Vec<&'p str>) {
    if let Key::Var(k) = key {
        names.push(k);
    }
    match place {
        Place::Var(v) => names.push(v),
        Place::Index(inner) => place_vars(&inner.0, &inner.1, names),
        Place::Other => {}
    }
}

/// The lowered operand a place stands for, so the family judges it through the same
/// functions an `ArgValue` operand goes through. `None` for a form the family cannot
/// resolve anyway.
fn place_arg(place: &Place) -> Option<ArgValue> {
    match place {
        Place::Var(v) => Some(ArgValue::Var(v.clone())),
        Place::Index(inner) => Some(ArgValue::OffsetRead {
            base: Box::new(place_arg(&inner.0)?),
            key: Box::new(key_arg(&inner.1)?),
        }),
        Place::Other => None,
    }
}

/// [`place_arg`] for a key.
fn key_arg(key: &Key) -> Option<ArgValue> {
    match key {
        Key::Int(i) => Some(ArgValue::Int(*i)),
        Key::Str(s) => Some(ArgValue::Str(s.clone())),
        Key::Var(v) => Some(ArgValue::Var(v.clone())),
        Key::Other => None,
    }
}

/// The names the statement may rebind before one of its reads runs.
enum Rebindable<'s> {
    /// Every name it hands to a call or writes in an embedded assignment, less the
    /// ones every call provably takes by value ([`by_value_survivors`]).
    Names(HashSet<&'s str>),
    /// Every name: in the top-level frame, whose locals are the globals, a userland
    /// body the statement runs can rebind any of them (`global $a;`) before a later
    /// operand reads it ([`top_level_rebind_risk`]).
    ///
    /// [`top_level_rebind_risk`]: crate::rebind::top_level_rebind_risk
    All,
}

impl Rebindable<'_> {
    fn contains(&self, name: &str) -> bool {
        match self {
            Rebindable::Names(names) => names.contains(name),
            Rebindable::All => true,
        }
    }
}

fn rebindable<'s>(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &'s Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Rebindable<'s> {
    if crate::rebind::top_level_rebind_risk(w.cx, folder, &stmt.runs) {
        return Rebindable::All;
    }
    if stmt.invalidated.is_empty() {
        return Rebindable::Names(HashSet::new());
    }
    let (kept, _) = by_value_survivors(w.cx, w.scope.poisoned, &stmt.invalidated, env, store);
    let names = stmt.invalidated.iter().map(|v| v.name.as_str()).filter(|n| !kept.contains(n));
    Rebindable::Names(names.collect())
}

/// Step 1z's member and offset half: each offset, property and class-constant read
/// position of `stmt`, judged by the module doc's table, at its own span.
fn check_member_reads(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let positions = stmt.read_positions();
    if positions.is_empty() {
        return;
    }
    let cx = w.cx;
    let poisoned = w.scope.poisoned;
    let rebound = rebindable(w, folder, stmt, env, store);
    let verdict = |carrier, family| carrier_verdict(cx, positions, carrier, family);
    let mut names = Vec::new();
    for pos in positions {
        if in_dead(&w.dead.borrow(), pos.span.start) {
            continue;
        }
        let span = pos.span;
        match &pos.kind {
            ReadKind::Offset(read) => {
                let (place, key) = &**read;
                names.clear();
                place_vars(place, key, &mut names);
                if names.iter().any(|n| rebound.contains(n)) {
                    continue;
                }
                let Some(compound) = verdict(pos.carrier, Family::Offset) else { continue };
                let (Some(base), Some(key)) = (place_arg(place), key_arg(key)) else {
                    continue;
                };
                check_offset_read(cx, folder, &base, &key, env, poisoned, compound, span, out);
                // The strict leg (ADR-0062 S6 / A-G10) at the same position: where
                // `check_offset_read` judges a proven whole container, this judges the
                // *declared* shape. The two are disjoint by construction — a
                // `Fact::Shape` is `Asserted`, and `check_offset_read`'s operand gate
                // takes `Verified` facts only — so at most one of them fires here.
                check_shape_read(cx, &base, &key, env, poisoned, span, out);
            }
            ReadKind::Prop(read) => {
                let (var, prop) = &**read;
                if rebound.contains(var.as_str()) {
                    continue;
                }
                if verdict(pos.carrier, Family::PropertyAccess).is_some() {
                    check_inaccessible_property(w, var, prop, store, false, span, out);
                }
                if verdict(pos.carrier, Family::PropertyValue).is_some() {
                    // Absence twin (ADR-0078, issue #197), disjoint by construction
                    // from the inaccessible check above (that requires a *declared*
                    // property).
                    check_undefined_property(w, folder, var, prop, store, span, out);
                    check_property_on_non_object(cx, var, prop, env, poisoned, span, out);
                }
            }
            ReadKind::ClassConst(read) => {
                let (sc, name) = &**read;
                if verdict(pos.carrier, Family::ClassConst).is_some() {
                    check_inaccessible_class_const(w, sc, name, span, out);
                    check_undefined_class_const(w, folder, sc, name, span, out);
                }
            }
            ReadKind::Call | ReadKind::CallRef => {}
        }
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
                resolve_native_value(cx, value, w.enclosing_class)
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
