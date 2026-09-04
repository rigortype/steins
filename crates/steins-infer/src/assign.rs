//! Assignment into the env: `$var = <value>` (extracted from the walk), the
//! returned-shape seed, `??` coalesce facts and their projections, and the value
//! fact of an argument.

use std::collections::HashMap;

use steins_domain::{CoverFlavor, Fact, ShapeFact, Val, Key as VKey};
use steins_syntax::{ArgValue, CallExpr, Span, ValueOp};

use crate::fold::Folder;
use crate::annotate::{FactKind, LineFact};
use crate::builtin_returns::{
    CATALOG_FLOOR, builtin_call_return_fact, builtin_resource_arms, builtin_return_floor,
    floor_value_fact, shape_builtin_return_fact,
};
use crate::cond::{
    coalesce_lhs_proven_present, eval_binary_fact, eval_cast_fact, eval_concat_fact,
    eval_isset_fact, eval_logical_fact, eval_not_fact, eval_spaceship_fact, eval_ternary_fact,
};
use crate::descent::summary_binds;
use crate::env::{
    ContractArm, HeapSummary, Known, ReturnSummary, Store, Stratum, array_literal_fact,
    class_const_class_fact, render_val, singleton_fact,
};
use crate::heap::{build_closure_val, build_new_object};
use crate::offsets::{ShapeRead, offset_key_of, offset_operand_fact, shape_read, shape_read_at};
use crate::project::Diagnostic;
use crate::refine::{clear_null, seed_shape_fact};
use crate::return_arms::call_return_arms;
use crate::walk::{WalkCx, mark_dead_span, value_stratum};

/// Apply a plain `$var = <value>;` assignment to the env (extracted from the walk).
/// `return_arms` is the declared return floor resolved at the call site **before**
/// this assignment may unbind its own target (self-assign `$o = $o->m(1)`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_assign(
    w: &WalkCx,
    folder: &mut dyn Folder,
    var: &str,
    value: &ArgValue,
    call: Option<&CallExpr>,
    span_start: u32,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    facts: &mut Option<&mut Vec<LineFact>>,
    summary: Option<&ReturnSummary>,
    // The constructor descent's `$this` snapshot for a `new` right-hand side
    // (ADR-0057 C4): the fresh allocation IS that snapshot, the allocation having had
    // no alias before the constructor ran.
    ctor_heap: Option<&HeapSummary>,
    return_arms: Option<&[ContractArm]>,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let line = cx.tree().position(span_start).line;

    // A ternary rvalue `$x = $c ? A : B` (ADR-0031): the walk evaluates the guard
    // and resolves to the chosen arm, or (undecided) a `OneOf` of both when
    // literal, else unknown.
    if let ArgValue::Ternary { cond, then_val, then_span, else_val, else_span } = value {
        match eval_ternary_fact(
            w,
            folder,
            cond,
            then_val,
            else_val,
            (*then_span, *else_span),
            env,
            store,
        ) {
            Some(fact) => {
                if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
                    });
                }
                // Derivation clause: result stratum is `min` over the arms (either
                // could be the taken one under a `Maybe` verdict).
                let strat = value_stratum(then_val, env, Some(&*store)).min(value_stratum(else_val, env, Some(&*store)));
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                store.unbind(var);
            }
            None => {
                env.remove(var);
                store.unbind(var);
            }
        }
        return;
    }

    // A comparison rvalue `$b = $x > 3;` (issue #260): the operator's fact, by the
    // same evaluator the dump surface reads, so the two can never disagree. Total
    // for a comparison — the binding is `bool` at worst, never dropped. A
    // `ValueOp::BitOr` (issue #615) is deliberately not matched: it has no such
    // floor, so it falls through and binds nothing, exactly as it did when a `|`
    // lowered to `ArgValue::Other`.
    if let ArgValue::Binary { op: ValueOp::Cmp(op), lhs, rhs } = value {
        let (fact, strat) =
            eval_binary_fact(cx, folder, *op, lhs, rhs, env, Some(&*store), w.scope.poisoned);
        if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
            facts.push(LineFact {
                line,
                kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
            });
        }
        env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
        store.unbind(var);
        return;
    }

    // A `<=>` rvalue `$n = $a <=> $b;` (issue #625): the operator's fact, by the
    // same evaluator the dump surface reads. Total one layer up from the
    // comparison — `int<-1, 1>` at worst, never dropped.
    if let ArgValue::Binary { op: ValueOp::Spaceship, lhs, rhs } = value {
        let (fact, strat) =
            eval_spaceship_fact(cx, folder, lhs, rhs, env, Some(&*store), w.scope.poisoned);
        if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
            facts.push(LineFact {
                line,
                kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
            });
        }
        env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
        store.unbind(var);
        return;
    }

    // A logical rvalue `$b = $x && $y;` and its negation `$b = !$x;` (issue
    // #625): the operator's fact, by the same evaluator the dump surface reads.
    // Total — PHP has no operator overloading for these — so the binding is
    // `bool` at worst and never dropped. A decided `&&`/`||` also records its
    // unevaluated right operand dead here (ADR-0052 §6).
    if let ArgValue::Logical { op, lhs, rhs, rhs_span } = value {
        let (fact, strat) = eval_logical_fact(
            w,
            folder,
            *op,
            lhs,
            rhs,
            *rhs_span,
            env,
            Some(&*store),
            w.scope.poisoned,
        );
        if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
            facts.push(LineFact {
                line,
                kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
            });
        }
        env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
        store.unbind(var);
        return;
    }
    if let ArgValue::Not(inner) = value {
        let (fact, strat) =
            eval_not_fact(w, folder, inner, env, Some(&*store), w.scope.poisoned);
        if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
            facts.push(LineFact {
                line,
                kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
            });
        }
        env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
        store.unbind(var);
        return;
    }

    // A cast rvalue `$n = (int) $x;` (issue #626): the grid's fact, by the same
    // evaluator the dump surface reads — the assignment and the dump of the same
    // expression can never disagree. Total, so the binding is the target's base
    // at worst and never dropped.
    if let ArgValue::Cast { target, operand } = value {
        let (fact, strat) =
            eval_cast_fact(w, folder, *target, operand, env, Some(&*store), w.scope.poisoned);
        if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
            facts.push(LineFact {
                line,
                kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
            });
        }
        env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
        store.unbind(var);
        return;
    }

    // A concatenation rvalue `$s = $a . $b;` (issue #627): the operator's fact,
    // by the same evaluator the dump surface reads — the assignment and the dump
    // of the same expression can never disagree. Total, so the binding is
    // `string` at worst and never dropped.
    if let ArgValue::Concat(lhs, rhs) = value {
        let (fact, strat) =
            eval_concat_fact(w, folder, lhs, rhs, env, Some(&*store), w.scope.poisoned);
        if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
            facts.push(LineFact {
                line,
                kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
            });
        }
        env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
        store.unbind(var);
        return;
    }

    // An `isset(…)` rvalue `$b = isset($a['k']);` (issue #579): the construct's
    // fact, by the same evaluator the dump surface reads — the assignment and the
    // dump of the same expression can never disagree. Total, so the binding is
    // `bool` at worst and never dropped.
    if let ArgValue::Isset(ops) = value {
        let (fact, strat) = eval_isset_fact(cx, ops, env, w.scope.poisoned);
        if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
            facts.push(LineFact {
                line,
                kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
            });
        }
        env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
        store.unbind(var);
        return;
    }

    // A closure value (ADR-0033): record a `ClosureVal` with its by-value capture
    // snapshot from the current (definition-site) env. A poisoned scope drops it.
    if let ArgValue::Closure(cref) = value {
        env.remove(var);
        store.unbind(var);
        if w.scope.poisoned {
            return;
        }
        // A closure that captures an object escapes it (ADR-0036): the closure holds
        // the object handle, so an unknown call may reach and mutate it.
        if let steins_syntax::ClosureRef::Anonymous { captures, .. } = cref {
            for name in captures {
                store.mark_escaped(name);
            }
        }
        if let Some(cv) = build_closure_val(cx, cref, line, env) {
            env.insert(var.to_owned(), Known::closure(cv, line));
        }
        return;
    }

    // The declared-arm lane travels across a plain copy `$c = $o` (ADR-0052 §9,
    // issue #196 piece 2): a copy binds the same value, so declared possibilities
    // carry over at the same stratum. Read before the match (a self-assign `$a =
    // $a` would otherwise unbind `var` and lose the source's own lane); written
    // after, since every match arm for a `Var` rvalue drops `var`'s lane without
    // replacing it.
    let copied_arms: Option<Vec<ContractArm>> = match value {
        ArgValue::Var(src) if !w.scope.poisoned => store.contract.get(src).cloned(),
        _ => None,
    };

    match value {
        // `$x = new Foo(args)` (ADR-0036): a fresh allocation, class from resolution,
        // props populated from promoted ctor params + literal defaults.
        ArgValue::New(class_ref, args, named) => {
            env.remove(var);
            store.unbind(var);
            if !w.scope.poisoned {
                let class = cx.class_fqn(class_ref);
                let id = build_new_object(w, folder, &class, args, named, env, store, ctor_heap);
                store.refs.insert(var.to_owned(), id);
                if let Some(facts) = facts.as_deref_mut() {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::ExactClass { var: var.to_owned(), class },
                    });
                }
            }
        }
        // `$b = $a` where `$a` holds an object (ADR-0036 aliasing): copy the ObjRef
        // (shared id), so a later write through either alias is visible via both.
        ArgValue::Var(src) if !w.scope.poisoned && store.is_bound(src) => {
            env.remove(var);
            let id = store.id_of(src).expect("bound var has an id");
            store.refs.insert(var.to_owned(), id);
        }
        // `clone $a` (ADR-0036 adversarial #1): a NEW id with a COPY of the source
        // object's props (PHP shallow clone) — post-clone writes stay isolated.
        ArgValue::Clone(src) if !w.scope.poisoned && store.is_bound(src) => {
            // Read the source id before unbinding `var`. For a self-clone
            // `$a = clone $a`, `var == src`, so unbinding first would drop `src`'s
            // binding too. PHP evaluates the rvalue before assigning, so the
            // pre-assignment id is the correct one to capture.
            let src_id = store.id_of(src).expect("bound var has an id");
            env.remove(var);
            store.unbind(var);
            if let Some(src_obj) = store.heap.get(&src_id) {
                let mut copy = src_obj.clone();
                copy.escaped = false; // a fresh, local clone has not escaped
                let id = w.fresh_id();
                store.heap.insert(id, copy);
                store.refs.insert(var.to_owned(), id);
            }
        }
        // `$x = $o->p` (ADR-0036): a property read flows the prop's fact into `$x`,
        // carrying the prop's stratum (derivation clause — heap reads).
        ArgValue::PropFetch { var: recv, prop } if !w.scope.poisoned => {
            env.remove(var);
            store.unbind(var);
            if let Some(fact) = store.prop_fact(recv, prop).cloned() {
                let strat = store.prop_stratum(recv, prop);
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
            }
        }
        // `$x = $base[k]` where `$base` carries an abstract shape (ADR-0062 §4's
        // read row, S3): a constant-key read takes the declared field's value slot.
        // A key with no fact behind it binds nothing — never value∪null (A-G9:
        // missing-ness is the strict leg's finding, never type pollution). The
        // whitelisted `offset.missing` judgment for this statement already ran
        // independently (step 1z).
        ArgValue::OffsetRead { base, key } => {
            // Resolve against the PRE-assignment env: PHP evaluates the rvalue
            // first, so a self-read `$a = $a['k']` still reads the old `$a`.
            let read = shape_read_at(base, key, env, w.scope.poisoned, cx.php_minor)
                .and_then(|(read, strat)| Some((read.into_fact()?, strat)));
            env.remove(var);
            store.unbind(var);
            if let Some((fact, strat)) = read {
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
            }
        }
        // `$x = $a ?? $b` (ADR-0052 §6): `clear_null(fact($a)) join fact($b)`. A
        // fact only when BOTH operands are visible facts, so `??` never manufactures
        // certainty for a value it cannot spell. The join widens, so it can only
        // lose precision — the FP-safe side.
        ArgValue::Coalesce(a, b, rhs_span) => {
            // `??` gates its right operand like a ternary gates an arm: an arm
            // proven set-and-non-null means PHP never evaluates what follows it.
            // The evaluator owns that record now (issue #630) — it needs the same
            // predicate to pick the value, and one predicate cannot disagree with
            // itself. Stratum is the evaluator's own `min` over the spine's arms.
            match eval_coalesce_fact(w, folder, a, b, *rhs_span, env, Some(&*store)) {
                Some((fact, strat)) => {
                    env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                    store.unbind(var);
                }
                None => {
                    env.remove(var);
                    store.unbind(var);
                }
            }
        }
        _ => match cx
            .resolve_literal_strat_ex(
                value,
                env,
                w.scope.poisoned,
                folder,
                None,
                Some(&mut *out),
            )
            .and_then(|(lit, strat)| singleton_fact(&lit, cx.php_minor).map(|f| (lit, f, strat)))
        {
            Some((lit, fact, strat)) => {
                if let Some(facts) = facts.as_deref_mut() {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::Value { var: var.to_owned(), rendered: lit.render() },
                    });
                }
                // Derivation clause: folds and array composition resolve through
                // `resolve_literal`, consuming env facts and nested project-call
                // summary strata (issue #127) — stamp that min. Nested descents for
                // fold args emit through `out` so findings under `strtoupper(g(1))`
                // aren't discarded.
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                store.unbind(var);
            }
            // `$x = is_int($y)` and kin: the fold could not reach it, so seed the
            // uniquely-resolved builtin's reflected return envelope (ADR-0056 R1).
            // Enters at `Verified` — a native declaration (§2).
            None => match value {
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
                    env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                    store.unbind(var);
                }
                // The `::class` magic constant (issue #236): `$c = Foo::class`
                // binds its FQN literal, `$c = static::class` the refinement.
                // Verified: PHP's own guarantee, not a declaration's.
                ArgValue::ClassConst(sc, name)
                    if let Some(fact) = class_const_class_fact(cx, w.scope, sc, name) =>
                {
                    env.insert(var.to_owned(), Known::value(fact, line, None));
                    store.unbind(var);
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
                    if let (Fact::Singleton(v), Some(facts)) = (&fact, facts.as_deref_mut()) {
                        facts.push(LineFact {
                            line,
                            kind: FactKind::Value {
                                var: var.to_owned(),
                                rendered: render_val(v),
                            },
                        });
                    }
                    env.insert(var.to_owned(), Known::value_strat(fact, line, Some(prov), strat));
                    store.unbind(var);
                }
                // The type rung above the envelope (ADR-0061 §1): reads the call's
                // argument facts — ADR-0062 §4's `count`/`array_is_list` shape
                // transfers. Enters at the argument's own stratum, not `Verified`.
                ArgValue::Call(name, args)
                    if let Some((fact, strat)) = shape_builtin_return_fact(
                        cx,
                        folder,
                        name,
                        args,
                        env,
                        Some(&*store),
                        w.scope.poisoned,
                    ) =>
                {
                    env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                    store.unbind(var);
                }
                ArgValue::Call(name, _)
                    if !w.scope.poisoned
                        && let Some(fact) = builtin_call_return_fact(cx, folder, name) =>
                {
                    env.insert(var.to_owned(), Known::value(fact, line, None));
                    store.unbind(var);
                }
                // The resource rung (ADR-0056 §8), below the reflected envelope and
                // above the declared floor: fires only where the envelope
                // structurally cannot (PHP has no `resource` return-type syntax, so
                // `fopen` declares nothing). The gate confirms this engine still
                // declares nothing for the name.
                //
                // Arm lane only, `Verified` — no `Val` is a resource (ADR-0035/0038),
                // so `env` is cleared rather than left stale. The `false` arm is an
                // ordinary literal arm, subtracted by ordinary guard machinery.
                ArgValue::Call(name, _)
                    if !w.scope.poisoned
                        && let Some(arms) = builtin_resource_arms(cx, folder, name) =>
                {
                    store.unbind(var);
                    env.remove(var);
                    store.contract.insert(var.to_owned(), arms);
                }
                // The declared-return floor (ADR-0069): reached only where the
                // engine said nothing about this name. Enters `Asserted` — a catalog
                // declaration, not a runtime answer — carried down every derivation
                // step.
                //
                // Both carriers are seeded, as `@param` entry seeding does: the arm
                // lane holds the declaration itself, and the value lane holds the
                // one fact the arms denote where they denote one. A multi-arm row
                // lives in the arm lane alone.
                ArgValue::Call(name, _)
                    if !w.scope.poisoned && let Some(arms) = builtin_return_floor(cx, name) =>
                {
                    store.unbind(var);
                    match floor_value_fact(&arms) {
                        Some(fact) => {
                            env.insert(
                                var.to_owned(),
                                Known::value_strat(
                                    fact,
                                    line,
                                    Some(CATALOG_FLOOR.to_owned()),
                                    Stratum::Asserted,
                                ),
                            );
                        }
                        None => {
                            env.remove(var);
                        }
                    }
                    store.contract.insert(var.to_owned(), arms);
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
                    env.remove(var);
                    store.unbind(var);
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
            },
        },
    }

    if let Some(arms) = copied_arms {
        store.contract.insert(var.to_owned(), arms);
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

/// The fact of a `??` chain (ADR-0052 §6, extended by ADR-0062 A-G11 / S5).
///
/// The join law is unchanged: `clear_null(fact($a)) join fact($b)`, folded
/// right-to-left over the whole spine (`??` is right-associative, so `$a ?? $b ??
/// $c` is one chain, not two nested pairs). An operand the domain cannot spell
/// yields `None` for the whole expression; every arm but the last contributes only
/// its non-null part.
///
/// What S5 adds is the premise ladder (A-G11). A `??` arm is reached only when
/// every arm to its left failed `isset`, so a pure depth-1 projection arm
/// `$x['k']` contributes the premise `¬isset($x['k'])` to everything after it.
/// [`ShapeFact::cover_proves`] consumes those: a KeyCover from
/// `isset($x['a']) || isset($x['b'])` plus `¬isset($x['a'])` proves `$x['b']`
/// present, turning an otherwise undischarged optional read into a value.
///
/// Any other arm form invalidates the ladder (A-G11's conservatism): a call may
/// write through a reference and make an earlier `¬isset` stale, so a non-projection
/// arm contributes no premise and drops every accumulated one — why
/// `$x['a'] ?? f() ?? $x['b']` discharges nothing.
///
/// # A non-projection arm settles the chain too (issue #630)
///
/// `settled` ends the spine at an arm PHP proves is the value, because nothing to
/// its right is evaluated. It used to be computed **only** inside the projection
/// branch, and the other branch hardcoded `false` — so a left arm that is a literal
/// or a proven-non-null variable never settled and the join added an arm PHP never
/// reaches: `'foo' ?? null` answered `'foo'|null`, `$scalar = 3; $scalar ?? 4`
/// answered `3|4`. The predicate that branch wants is
/// [`coalesce_lhs_proven_present`], which already stood next door in the assignment
/// seam deciding the same question for deadness alone.
///
/// **This evaluator now owns the `mark_dead_span` record**, the choice issue #625's
/// leg 1 made for [`eval_ternary_fact`]: one predicate decides the value and the
/// deadness together, so no seam can answer `??` with a fact while disagreeing about
/// which arms PHP ran. The assignment seam's separate call is gone. `rest` is the
/// source extent to the right of each arm, threaded by
/// [`flatten_coalesce_spans`].
///
/// The projection branch's `settled` deliberately does **not** mark. Its presence
/// claim comes from a shape fact, which is `Asserted` — and reachability stays
/// proof-only, the same line `coalesce_lhs_proven_present`'s own third refusal
/// draws. A shape may say a key is `Required` on a docblock's word; that is enough
/// to pick a value, never enough to prove code unreached.
///
/// [`eval_ternary_fact`]: crate::cond::eval_ternary_fact
/// [`coalesce_lhs_proven_present`]: crate::cond::coalesce_lhs_proven_present
pub(crate) fn eval_coalesce_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    a: &ArgValue,
    b: &ArgValue,
    rhs_span: Span,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<(Fact, Stratum)> {
    let (poisoned, php_minor) = (w.scope.poisoned, w.cx.php_minor);
    let mut arms: Vec<(&ArgValue, Option<Span>)> = Vec::new();
    flatten_coalesce_spans(a, Some(rhs_span), &mut arms);
    flatten_coalesce_spans(b, None, &mut arms);
    let last = arms.len() - 1;

    let mut premises: Vec<(String, VKey)> = Vec::new();
    // `Option<(Option<Fact>, Stratum)>`: the outer `None` is "the domain cannot
    // spell this arm" and ends the whole expression; the INNER `None` is "PHP
    // proves this arm falls through", which contributes no value and still
    // contributes its stratum.
    let mut parts: Vec<(Option<Fact>, Stratum)> = Vec::with_capacity(arms.len());
    for (i, (arm, rest)) in arms.iter().enumerate() {
        let projection = coalesce_projection(arm, env, poisoned, php_minor);
        let (part, settled) = match &projection {
            Some((var, key)) => {
                let (f, s, settled) = coalesce_arm_fact(var, key, env, &premises, i == last)?;
                (Some((f, s)), settled)
            }
            None => {
                // Marked before the fact is demanded, so an arm proven present that
                // the domain still cannot spell records the deadness it proves —
                // exactly what the assignment seam did with its own call.
                let settled = store
                    .is_some_and(|s| coalesce_lhs_proven_present(w, folder, arm, env, s));
                if settled && let Some(span) = rest {
                    mark_dead_span(w, *span);
                }
                (
                    arg_value_fact(w, folder, arm, env)
                        .map(|f| (Some(f), value_stratum(arm, env, store))),
                    settled,
                )
            }
        };
        parts.push(part?);
        // An arm proven present and non-null is the value: `??` never evaluates
        // anything to its right, so the chain ends here.
        if settled {
            break;
        }
        match projection {
            Some(p) => premises.push(p),
            None => premises.clear(),
        }
    }

    // Derivation clause (ADR-0052 §5): result is no stronger than the weakest arm.
    // The last arm is the value whenever everything left of it fell through, so it
    // is the one arm that must carry a fact.
    let (last_fact, mut stratum) = parts.pop()?;
    let mut acc = last_fact?;
    while let Some((fact, s)) = parts.pop() {
        stratum = stratum.min(s);
        // A provably-absent arm (no fact) and a provably-null one (nothing survives
        // `clear_null`) are the same law: PHP falls through both, so neither
        // contributes a value — and both still contribute their stratum.
        if let Some(nonnull) = fact.as_ref().and_then(clear_null) {
            acc = nonnull.join(&acc)?;
        }
    }
    Some((acc, stratum))
}

/// Flatten a `??` spine into its arms, left to right. Both sides are walked: `??`
/// is right-associative, so the nesting is normally on the right, but explicit
/// parentheses (`($a ?? $b) ?? $c`) nest left and mean the same chain.
pub(crate) fn flatten_coalesce<'a>(v: &'a ArgValue, out: &mut Vec<&'a ArgValue>) {
    match v {
        ArgValue::Coalesce(a, b, _) => {
            flatten_coalesce(a, out);
            flatten_coalesce(b, out);
        }
        _ => out.push(v),
    }
}

/// [`flatten_coalesce`] carrying, per arm, the source extent of everything to its
/// **right** in the chain — the region `mark_dead_span` records when that arm
/// settles (issue #630). `rest` is what lies to the right of `v` itself.
///
/// `ArgValue::Coalesce(a, b, rhs_span)` spans `b` with `rhs_span`, so on the
/// ordinary right-associative nesting each arm gets exactly the extent the
/// assignment seam used to mark for the head arm, and the arms after it get their
/// own tighter ones. A left-nested chain `($x ?? $y) ?? $z` gives `$x` the extent of
/// `$y` alone: `$z` is dead too, but recording a sub-extent only *under*-suppresses,
/// which is the FP-safe side and the only side deadness may err on.
fn flatten_coalesce_spans<'a>(
    v: &'a ArgValue,
    rest: Option<Span>,
    out: &mut Vec<(&'a ArgValue, Option<Span>)>,
) {
    match v {
        ArgValue::Coalesce(a, b, rhs_span) => {
            flatten_coalesce_spans(a, Some(*rhs_span), out);
            flatten_coalesce_spans(b, rest, out);
        }
        _ => out.push((v, rest)),
    }
}

/// Is this `??` arm a pure depth-1 projection `$x[k]` with a resolvable constant
/// key (A-G11's premise carrier)? Key resolution is the offset family's own
/// ([`offset_operand_fact`] + [`offset_key_of`]), so a premise and a cover can
/// never disagree.
///
/// Depth is exactly one and the base is exactly a binding: `$x['a']['b']` and
/// `$this->x['a']` are not premise carriers and invalidate the ladder rather than
/// extending it.
pub(crate) fn coalesce_projection(
    arm: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    php_minor: Option<(u16, u16)>,
) -> Option<(String, VKey)> {
    let ArgValue::OffsetRead { base, key } = arm else { return None };
    let ArgValue::Var(name) = base.as_ref() else { return None };
    let Some(Fact::Singleton(key_val)) = offset_operand_fact(key, env, poisoned, php_minor) else {
        return None;
    };
    Some((name.clone(), offset_key_of(&key_val)?))
}

/// The fact a projection arm `$var[key]` contributes to its `??` chain, the
/// stratum it inherits from the base's shape fact (derivation clause: never
/// stronger than the base — always `Asserted`), and whether the arm settles the
/// chain (proven to be the value, so `??` never evaluates further).
///
/// A non-final arm is used only when `isset` holds of it, so presence needn't be
/// proved — missing means fall-through, yielding its declared slot
/// ([`ShapeRead::taken_fact`]). The final arm is the value whenever everything to
/// its left fell through, so it must be *proved* present: a `Required` field
/// proves it outright; an optional one needs A-G11's cover discharge, with
/// `absent` the accumulated `¬isset` ladder over this base.
///
/// # The base may be an order-witnessed VALUE, not only an abstract shape
///
/// A fully literal array binds `Fact::Singleton(Val::Array)`, never `Fact::Shape`,
/// so `$array = [1, 2, 3]; $array['string'] ?? 0` used to decline on the base test
/// alone — while `isset($array['string'])` on the very same binding answered
/// `false`, because the `isset` lane reads a literal array directly. The base is
/// **lifted** here for the same reason and by the same call the offset-write barrier
/// uses ([`ShapeFact::lift`], issue #327): a witnessed value is strictly more
/// precise than the shape it lifts to, so reading it through the shape law can only
/// lose precision, never invent it.
///
/// # A provably-absent arm falls through; it does not silence the chain
///
/// `DeclaredAbsent` is a *proof* — a `Sealed` shape's non-field, or a field the
/// declaration marks `Absent`. PHP's `??` skips such an arm and evaluates the next
/// one, so the arm contributes **no fact** and the chain goes on. Returning `None`
/// for it, which is what `taken_fact()` alone did, conflated "this arm is proven
/// not to be the value" with "the domain cannot spell this arm" and let the first
/// kill the whole expression. That is the same law the join loop already applies
/// one level up to a provably-null arm.
///
/// The absent arm still contributes its **stratum**: the absence rests on the
/// base's fact, so an `Asserted` shape's word about a missing key cannot buy a
/// `Verified` answer.
fn coalesce_arm_fact(
    var: &str,
    key: &VKey,
    env: &HashMap<String, Known>,
    premises: &[(String, VKey)],
    final_arm: bool,
) -> Option<(Option<Fact>, Stratum, bool)> {
    let known = env.get(var)?;
    let lifted;
    let shape: &ShapeFact = match &known.fact {
        Some(Fact::Shape { shape, nullable: false }) => shape.as_ref(),
        Some(Fact::Singleton(Val::Array(entries))) => {
            lifted = ShapeFact::lift(entries);
            &lifted
        }
        _ => return None,
    };
    let read = shape_read(shape, key);
    // Present and non-null is exactly PHP's `isset` — same test in both positions,
    // harmless on the last arm and a short-circuit before it.
    let settled = matches!(&read, ShapeRead::Present(Some(f)) if f.is_null().is_no());
    if !final_arm {
        // Proven absent: PHP falls through, so no fact and no silence.
        if matches!(read, ShapeRead::DeclaredAbsent) {
            return Some((None, known.stratum, false));
        }
        return read.taken_fact().map(|f| (Some(f), known.stratum, settled));
    }
    if let ShapeRead::Present(Some(f)) = read {
        return Some((Some(f), known.stratum, settled));
    }
    let absent: Vec<VKey> =
        premises.iter().filter(|(v, _)| v == var).map(|(_, k)| k.clone()).collect();
    let flavor = cover_discharges(shape, key, &absent)?;
    let slot = shape.field(key).and_then(|(_, _, s)| s.as_deref().cloned())?;
    match flavor {
        // "at least one covered key is present AND non-null": this one carries it.
        CoverFlavor::Isset => clear_null(&slot).map(|f| (Some(f), known.stratum, true)),
        // "at least one covered key EXISTS", value possibly null (A-G11's table).
        CoverFlavor::KeyExists => Some((Some(slot), known.stratum, false)),
    }
}

/// The S5 discharge, asked as a presence question (A-G11's table): does a
/// recorded KeyCover plus the accumulated `¬isset(absent)` ladder prove `key`
/// present? `Some(flavor)` when it does, `None` otherwise.
///
/// Split out of [`coalesce_arm_fact`] so the value lane and S6's finding lane
/// consult one predicate: a discharged key with an unrepresentable value slot
/// yields no fact but is still proven present, so the finding lane must stay
/// silent — folding the two would emit a false positive on an unspellable slot.
pub(crate) fn cover_discharges(shape: &ShapeFact, key: &VKey, absent: &[VKey]) -> Option<CoverFlavor> {
    match shape.cover_proves(key, absent)? {
        CoverFlavor::Isset => Some(CoverFlavor::Isset),
        // A present-*null* earlier key satisfies a KeyExists claim while `??` still
        // falls through it, so the discharge is sound only when every premise key's
        // declared value is provably non-nullable ("fell through" == "absent").
        CoverFlavor::KeyExists => absent
            .iter()
            .all(|k| {
                shape
                    .field(k)
                    .and_then(|(_, _, s)| s.as_deref())
                    .is_some_and(|f| f.is_null().is_no())
            })
            .then_some(CoverFlavor::KeyExists),
    }
}

/// The value-domain fact of an rvalue operand for the `??` join: a bare variable's
/// env fact, or a literal/foldable value's `Singleton`. Non-representable operands
/// (calls, offsets → `Other`, objects) yield `None`.
fn arg_value_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    arg: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<Fact> {
    match arg {
        ArgValue::Var(name) if !w.scope.poisoned => env.get(name)?.fact.clone(),
        _ => {
            let lit = w.cx.resolve_literal(arg, env, w.scope.poisoned, folder)?;
            singleton_fact(&lit, w.cx.php_minor)
        }
    }
}
