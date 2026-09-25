//! The rungs of `$var = <value>` below the literal rung ([`bind_unfolded`]): an
//! array literal's shape, `::class`, the union fold, the builtin-call ladder, and
//! last a call's return summary or declared arms. Beside them, the element places
//! a call right-hand side produces ([`bind_call_places`]).

use std::collections::HashMap;

use steins_domain::Key as VKey;
use steins_syntax::{ArgValue, CallExpr};

use crate::fold::Folder;
use crate::builtin_returns::{
    BuiltinRung, CATALOG_FLOOR, OptionalRungs, bind_produced_places, builtin_call_rung,
    floor_value_fact, socket_pair_places,
};
use crate::descent::summary_binds;
use crate::env::{
    ContractArm, Known, ReturnSummary, Store, Stratum, array_literal_fact, class_const_class_fact,
    elem_place,
};
use crate::refine::seed_shape_fact;
use crate::return_arms::call_return_arms;
use crate::walk::WalkCx;

use super::AssignLhs;

/// Bind a right-hand side the literal rung could not fold: an array literal's
/// shape, `::class`, the union fold, the builtin-call ladder, and last a call's
/// return summary or declared arms, the first that answers.
#[allow(clippy::too_many_arguments)]
pub(super) fn bind_unfolded(
    w: &WalkCx,
    folder: &mut dyn Folder,
    lhs: &mut AssignLhs,
    value: &ArgValue,
    call: Option<&CallExpr>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    summary: Option<&ReturnSummary>,
    return_arms: Option<&[ContractArm]>,
) {
    let cx = w.cx;
    let (var, line) = (lhs.var, lhs.line);
    match value {
        // An array literal the rung above could not prove whole (issue
        // #327): keys, count, and sealing are known even when an element's
        // value is not, so it seeds a `Fact::Shape` rather than dropping.
        ArgValue::Array(items)
            if let Some((fact, strat)) = array_literal_fact(
                cx,
                folder,
                items,
                env,
                w.scope.poisoned,
                Some(&*store),
            ) =>
        {
            lhs.bind_quiet(env, store, fact, strat);
            bind_handle_elements(cx, var, items, store);
        }
        // The `::class` magic constant (issue #236): `$c = Foo::class`
        // binds its FQN literal, `$c = static::class` the refinement.
        // Verified: PHP's own guarantee, not a declaration's.
        ArgValue::ClassConst(sc, name)
            if let Some(fact) = class_const_class_fact(cx, w.scope, sc, name) =>
        {
            lhs.bind_quiet(env, store, fact, Stratum::Verified);
        }
        // The member-wise union fold (issue #74): a bounded union-of-constants
        // argument is enumerated and every combination answered by the real
        // engine. Binds where a folded literal binds, carrying the input
        // union's own stratum (N2's min), not the engine's `Verified`.
        ArgValue::Call(name, args)
            if let Some((fact, strat, prov)) =
                cx.try_union_fold(name, args, env, w.scope.poisoned, folder) =>
        {
            // A product whose members all agreed composes to a `Singleton`.
            lhs.note_literal(&fact);
            lhs.bind_known(env, store, Known::value_strat(fact, line, Some(prov), strat));
        }
        // The builtin-call ladder (`builtin_call_rung`), every rung of it:
        // the §2.7 resource folds are asked HERE — where the right-hand
        // side IS the call — so no other call of the statement can have
        // moved the handle's state first, and the resource arms because
        // this seam binds the heap resource they come with. A poisoned
        // scope binds nothing and asks nothing.
        ArgValue::Call(name, args)
            if !w.scope.poisoned
                && let Some(rung) = builtin_call_rung(
                    cx,
                    folder,
                    name,
                    args,
                    env,
                    Some(&*store),
                    w.scope.poisoned,
                    OptionalRungs { resource_folds: true, resource_arms: true },
                ) =>
        {
            bind_builtin_rung(w, lhs, rung, env, store);
        }
        // The return summary, then the arm floor (ADR-0057 T0/T1 /
        // ADR-0052 §9). `unbind` first (voids any stale arm lane).
        //
        // The HEAP rung first (T1, §1's rebind): a summary carrying an
        // allocation binds `var` to a **fresh object in this walk's own heap**
        // — a copy, no shared identity, so no callee-side name survives and no
        // aliasing question crosses the boundary. Ordering the two rungs is
        // formality: an object return carries no value fact (ADR-0035), so
        // they are exclusive by construction.
        //
        // Then the value rung: the summary is the value floor above the
        // declared arms (A1): a bindable value fact binds as `var`'s value fact
        // at its joined stratum, sitting where a folded literal would.
        // Otherwise the summary degraded to the floor and the declared arms
        // stand. Since issue #596 a `Fact::Shape` is bindable, so this rung is
        // also the sharp twin of `seed_returned_shape` below: the same lane,
        // the same consumers, a proven shape instead of a declared one — and,
        // crucially, the summary's own stratum instead of that seed's flat
        // `Asserted`. No heap question arises for it: a returned array is a
        // COPY (PHP value semantics), so unlike the heap rung above it needs no
        // fresh `AllocId` and shares no identity with anything the callee kept.
        _ => {
            lhs.clear(env, store);
            if let Some(ReturnSummary { heap: Some(hs), .. }) = summary
                && !w.scope.poisoned
            {
                // The snapshot, verbatim (§1's field-by-field list): class and
                // exactness copied never promoted, props with their strata,
                // readonly bookkeeping transferred (sweep immunity does not
                // stop at a `return`), carries kept, and `escaped` = the
                // summary's escaped-BEFORE-return bit — `false` meaning the
                // caller now holds the sole reference, so the object survives
                // an unrelated unknown call exactly as a local `new` does.
                let id = w.fresh_id();
                store.heap.insert(id, hs.obj.clone());
                store.refs.insert(var.to_owned(), id);
            } else if let Some(ReturnSummary { value: Some(sv), .. }) = summary
                && summary_binds(&sv.fact)
            {
                env.insert(
                    var.to_owned(),
                    Known::value_strat(sv.fact.clone(), line, None, sv.stratum),
                );
            } else if let Some(arms) = return_arms {
                // Prefer arms captured at resolution (before this unbind),
                // so method self-assign keeps the declared floor.
                seed_returned_shape(var, arms, line, env);
                store.contract.insert(var.to_owned(), arms.to_vec());
            } else if let Some(c) = call
                && let Some(arms) = call_return_arms(
                    cx,
                    c,
                    store,
                    w.this_exact,
                    w.enclosing_class,
                    w.scope.poisoned,
                )
            {
                // Fallback: free-function / non-self-assign paths.
                seed_returned_shape(var, &arms, line, env);
                store.contract.insert(var.to_owned(), arms);
            }
        }
    }
}

/// Bind the element places a call right-hand side produces (ADR-0098 §2.2):
/// `$pair = stream_socket_pair(…)`.
pub(super) fn bind_call_places(
    w: &WalkCx,
    folder: &mut dyn Folder,
    var: &str,
    value: &ArgValue,
    store: &mut Store,
) {
    // The poison leg (ADR-0046) is the twin of `produced_places`', and like it
    // it is defence in depth: measured by mutation, removing it changes no
    // finding, because `resource_call_effects` refuses a poisoned scope before
    // any closing call is recorded. `a_poisoned_scope_convicts_through_no_place`
    // pins the posture rather than this line.
    if let ArgValue::Call(name, _) = value
        && !w.scope.poisoned
        && let Some(places) = socket_pair_places(w.cx, folder, name)
    {
        bind_produced_places(w, var, places, store);
    }
}

/// Bind `$var = name(…)` from the builtin-call ladder's answer
/// ([`builtin_call_rung`]): the assignment seam's sink, one arm per rung.
fn bind_builtin_rung(
    w: &WalkCx,
    lhs: &mut AssignLhs,
    rung: BuiltinRung,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    match rung {
        // `gettype`, `get_debug_type`, `get_resource_type`, `get_resource_id`
        // over a proven handle (ADR-0097 §2.7).
        BuiltinRung::ResourceFold(fact, strat) => lhs.bind(env, store, fact, strat),
        // The type rung above the envelope (ADR-0061 §1): reads the call's
        // argument facts — ADR-0062 §4's `count`/`array_is_list` shape transfers.
        // Enters at the argument's own stratum, not `Verified`.
        BuiltinRung::Shape(fact, strat) => lhs.bind_quiet(env, store, fact, strat),
        // The reflected return envelope: `Verified`, a native declaration
        // (ADR-0056 R1, §2).
        BuiltinRung::Envelope(fact) => lhs.bind_quiet(env, store, fact, Stratum::Verified),
        // The resource rung (ADR-0056 §8). The gate confirms this engine still
        // declares nothing for the name (`fopen` declares nothing: PHP has no
        // `resource` return-type syntax).
        //
        // Arm lane only, `Verified` — no `Val` is a resource (ADR-0035/0038), so
        // `env` is cleared rather than left stale. The `false` arm is an ordinary
        // literal arm, subtracted by ordinary guard machinery.
        BuiltinRung::ResourceArms(arms, res) => {
            lhs.clear(env, store);
            store.contract.insert(lhs.var.to_owned(), arms);
            // The identity and the state (ADR-0097 §2.3): a fresh heap resource,
            // `Open`, bound exactly as `new` binds an object — so `$b = $h`
            // shares it and `fclose($b)` closes it for both.
            store.bind_resource(lhs.var, w.fresh_id(), res);
        }
        // The declared-return floor (ADR-0069). Enters `Asserted` — a catalog
        // declaration, not a runtime answer — carried down every derivation step.
        //
        // Both carriers are seeded, as `@param` entry seeding does: the arm lane
        // holds the declaration itself, and the value lane holds the one fact the
        // arms denote where they denote one. A multi-arm row lives in the arm lane
        // alone.
        BuiltinRung::Floor(arms) => {
            match floor_value_fact(&arms) {
                Some(fact) => lhs.bind_known(
                    env,
                    store,
                    Known::value_strat(
                        fact,
                        lhs.line,
                        Some(CATALOG_FLOOR.to_owned()),
                        Stratum::Asserted,
                    ),
                ),
                None => lhs.clear(env, store),
            }
            store.contract.insert(lhs.var.to_owned(), arms);
        }
    }
}

/// Seed the value lane of `$var = <call>;` with the callee's **declared return
/// shape** (issue #288), the return-lane mirror of the parameter seeding the entry
/// pass does with [`seed_shape_fact`].
///
/// The arm lane alone carried the array vocabulary across a return, and the arm lane
/// is not what the abstract array stratum reads: every shape consumer (S3's read
/// row, S4's guards, S6's strict leg) asks the VALUE lane for a [`Fact::Shape`]. So a
/// `@return array<string, int>` reached the caller as a declared arm nobody could
/// project a key out of, while the same declaration on a `@param` seeded a shape and
/// every one of those consumers worked — the asymmetry issue #288 measured.
///
/// The seed is the same fact, from the same ONE lowering, at the same **`Asserted`**
/// stratum the parameter seed enters at (A-G9's corollary: shape-derived facts never
/// feed proof-layer findings), and it is written only into a value lane this
/// assignment has already cleared — it can never overwrite a more precise fact,
/// because every rung above this one returned before reaching here.
///
/// [`Fact::Shape`]: steins_domain::Fact::Shape
fn seed_returned_shape(
    var: &str,
    arms: &[ContractArm],
    line: u32,
    env: &mut HashMap<String, Known>,
) {
    let Some(fact) = seed_shape_fact(arms) else { return };
    env.insert(
        var.to_owned(),
        Known::value_strat(fact, line, Some("declared array shape".to_owned()), Stratum::Asserted),
    );
}

/// Bind the element **places** of an array literal whose elements are handles
/// (ADR-0098 §2.2): `$bag = [$h]` makes `bag[0]` name the very allocation `$h`
/// holds, so `fclose($bag[0])` closes it for both — PHP's handle semantics, one
/// level out.
///
/// The id is shared and the arm lane is copied, because both are what the
/// element *is*: the heap answers its state, the lane answers that it is a
/// resource at all (ADR-0056 §8.6's lock, which every resource consumer reads).
///
/// Declines whole rather than in part. A literal holding a non-literal key is
/// declined by [`normalize_array`] itself — an unknown key may be an integer and
/// would shift every following auto-index, so no position in it is nameable.
///
/// [`normalize_array`]: steins_syntax::normalize_array
fn bind_handle_elements(
    cx: &crate::cx::Cx,
    var: &str,
    items: &[(steins_syntax::ArrayKey, ArgValue)],
    store: &mut Store,
) {
    let Some(normalized) = steins_syntax::normalize_array(items, cx.php_minor) else { return };
    for (key, value) in normalized {
        let ArgValue::Var(source) = value else { continue };
        let Some(id) = store.id_of(&source).filter(|id| store.resources.contains_key(id)) else {
            continue;
        };
        let key = match key {
            steins_syntax::NormKey::Int(i) => VKey::Int(i),
            steins_syntax::NormKey::Str(s) => VKey::Str(s),
        };
        let place = elem_place(var, &key);
        store.bind_place(place.clone(), id);
        if let Some(arms) = store.contract.get(&source).cloned() {
            store.contract.insert(place, arms);
        }
    }
}
