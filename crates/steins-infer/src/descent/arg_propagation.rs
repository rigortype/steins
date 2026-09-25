//! The argument check of a resolved project function call whose arguments carry
//! propagated values — a `$var`, a call, a method call ([`check_propagated_call`])
//! — and the one resolver it shares with the builtin arm, [`propagated_arg_value`].

use std::collections::HashMap;

use steins_domain::Fact;
use steins_syntax::{ArgValue, CallExpr};

use crate::fold::Folder;
use crate::arg_check::{
    check_maybe_argument_mismatch, implicit_null_accepted, is_type_error, object_world_guard_blind,
};
use crate::builtin_returns::store_holds_resource;
use crate::cx::Cx;
use crate::env::{Known, Store, Stratum, arg_of_val};
use crate::generics::{check_named_phpdoc_params, check_phpdoc_param};
use crate::heap::simple_class;
use crate::project::Diagnostic;
use crate::walk::WalkCx;

use super::{project_call_summary, project_method_summary};

/// The **propagated value** an argument position carries: the resolved
/// [`ArgValue`], a provenance phrase for the message, and the trust stratum that
/// arrived with the resolution (issue #127 review).
///
/// The stratum is the resolution's own and never a re-read of the syntactic call
/// tree through `value_stratum`, which would launder an `Asserted` fold
/// (`strtoupper(g(...))`) into `Verified` — the proof gate consumes it directly
/// (ADR-0052 §5).
///
/// One resolver, two consumers: the project arm of [`check_propagated_call`] and
/// the builtin arm beside it (ADR-0056 §9.2). A builtin argument is resolved by
/// exactly the code a project argument is, which is what makes the two judgments
/// the same judgment rather than two that agree today.
#[allow(clippy::too_many_arguments)]
pub(crate) fn propagated_arg_value(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
    in_descent: bool,
    span_start: u32,
    out: &mut Vec<Diagnostic>,
) -> Option<(ArgValue, String, Stratum)> {
    match value {
        ArgValue::Var(name) if !poisoned => env.get(name).and_then(|k| {
            let v = k.singleton()?;
            let prov = match &k.bound {
                Some(b) => format!("from ${name}, {b}"),
                None => format!("from ${name}, assigned at line {}", k.line),
            };
            Some((v, prov, k.stratum))
        }),
        ArgValue::Call(name, args) => {
            let direct = if args.is_empty() {
                cx.resolve_const_fn(name)
                    .map(|(lit, line)| {
                        (
                            lit,
                            format!("from {name}(), defined at line {line}"),
                            Stratum::Verified,
                        )
                    })
                    .or_else(|| {
                        cx.try_fold_emit(name, args, env, poisoned, folder, out)
                    })
            } else {
                cx.try_fold_emit(name, args, env, poisoned, folder, out)
            };
            // A nested project call (issue #60): its Singleton return summary
            // is the argument's proven value — `takesInt(g(1))` sees what `g`
            // provably returns, the same crossing `$x = g(1); takesInt($x)`
            // always had. Plain per-scope pass only: a fresh descent tree
            // started from inside a live descent would evade the on-stack
            // recursion guard (mutual recursion through an argument position
            // would loop), and the plain pass walks every scope anyway, so
            // the descent-pass decline loses no site. `Verified`-only: the
            // native proof below consumes an all-Verified premise (ADR-0052
            // §5), and an Asserted summary must not launder into it. Findings
            // go to the real `out` so a binding-specific proof under `g(1)`
            // is not discarded (issue #127 review); dedup absorbs any
            // binding-independent copy already emitted by the plain walk.
            direct.or_else(|| {
                if in_descent {
                    return None;
                }
                let summary = project_call_summary(
                    cx, folder, name, args, env, store, poisoned, span_start, None, out,
                )?;
                let sv = summary.value?;
                if sv.stratum != Stratum::Verified {
                    return None;
                }
                let Fact::Singleton(v) = &sv.fact else { return None };
                Some((arg_of_val(v), format!("returned from {name}()"), sv.stratum))
            })
        }
        // A nested method / static call (issue #386): its `Singleton` summary
        // is the argument's proven value, on the same terms the function arm
        // above states — plain per-scope pass only, `Verified` only, findings
        // to the real `out`. The provenance names the call as it was written,
        // since a method has no bare name to print.
        ArgValue::MethodCall { callee, args, named } if !in_descent => {
            let summary = project_method_summary(
                cx,
                folder,
                callee,
                args,
                named,
                env,
                store,
                this_exact,
                enclosing_class,
                poisoned,
                span_start,
                None,
                out,
            );
            summary.and_then(|s| {
                let sv = s.value?;
                if sv.stratum != Stratum::Verified {
                    return None;
                }
                let Fact::Singleton(v) = &sv.fact else { return None };
                Some((
                    arg_of_val(v),
                    format!("returned from {}", value.render()),
                    sv.stratum,
                ))
            })
        }
        // A property read `$o->p` (ADR-0036): a `Singleton` prop fact flows.
        ArgValue::PropFetch { var, prop } if !poisoned => {
            store.prop_fact(var, prop).and_then(|f| match f {
                Fact::Singleton(v) => Some((
                    arg_of_val(v),
                    format!("from ${var}->{prop}"),
                    store.prop_stratum(var, prop),
                )),
                _ => None,
            })
        }
        _ => None,
    }
}

/// Check a function call whose arguments may be propagated values (`Var`/`Call`/
/// array). Runs the native runtime check and the phpdoc declared-contract check;
/// a site where the native check fired is skipped by the phpdoc check.
pub(crate) fn check_propagated_call(
    w: &WalkCx,
    folder: &mut dyn Folder,
    in_descent: bool,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let poisoned = w.scope.poisoned;
    // The caller's own frame, for a method-call ARGUMENT whose receiver is
    // `$this`/`self::`/`parent::` (issue #386): read off the `WalkCx` as the dump
    // surface reads it, which buys the receiver spellings a frame-less seam has to
    // decline.
    let this_exact = w.this_exact;
    let enclosing_class = w.enclosing_class;
    // Resolve non-positional calls too (Gap A): the positional prefix and the named
    // arguments are contract-checked here; only the binding descent stays positional.
    let Some(site) = cx.resolve_user_fn_any(call) else { return };
    let decl = cx.fn_decl(site);
    let envelopes = cx.envelopes_of(decl.docblock.as_deref(), site.file, decl.span.start);

    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = decl.params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }

        let mut native_fired = false;
        if let Some(ty) = param.ty.as_ref() {
            // The resolution is shared with the builtin arm (ADR-0056 §9.2) — see
            // `propagated_arg_value` for why the stratum is the resolution's own.
            let resolved = propagated_arg_value(
                cx, folder, &arg.value, env, store, this_exact, enclosing_class, poisoned,
                in_descent, arg.span.start, out,
            );
            // Proof-layer consumption rule (ADR-0052 §5): the native
            // `type.argument-mismatch` fires only on an all-`Verified` premise. A
            // value proven through an `Asserted` env/heap fact stays silent (the
            // phpdoc contract check below still accepts it).
            if let Some((value, provenance, strat)) = resolved
                && strat == Stratum::Verified
                && is_type_error(cx, ty, &value)
                && !implicit_null_accepted(param, &value)
                && !object_world_guard_blind(in_descent, ty, &value)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &value,
                    Some(&provenance),
                    &decl.name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // A variable bound to a proven object (ADR-0036 heap): object-vs-type
            // definite-No (ADR-0043 stage 3). `new`/enum/const args are the direct
            // pass's job; this covers the env/heap-dependent `$x = new Foo(); f($x)`.
            // Guard-blind inside a descent (see `object_world_guard_blind`).
            if !native_fired
                && !poisoned
                && !in_descent
                && let ArgValue::Var(name) = &arg.value
                && store.is_exact(name) // No-side needs exactness (audit G1)
                && let Some(class) = store.class_of(name)
                && cx.object_is_type_error(ty, class)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &ArgValue::Var(name.clone()),
                    Some(&format!("holds a {}", simple_class(class))),
                    &decl.name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // A variable whose contract lane is a bare `Verified` resource
            // (ADR-0056 §8) — the resource sibling of the object branch above,
            // and the same shape of claim: a non-scalar the value lattice has no
            // inhabitant for, proven for this variable on this branch, judged
            // against the native parameter type.
            //
            // Not guard-blind, unlike the object branches: `object_world_guard_blind`
            // exists because a callee's in-body `instanceof` can narrow a rebound
            // object, and there is no guard in PHP that narrows a value INTO being
            // a resource — `is_resource` only confirms what the lane already says.
            if !native_fired
                && !poisoned
                && let ArgValue::Var(name) = &arg.value
                && store_holds_resource(store, name)
                && cx.resource_is_type_error(ty)
            {
                out.push(cx.resource_diagnostic(
                    arg.span.start,
                    name,
                    &decl.name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // The possibly-grade sibling (ADR-0081's 2026-08-16 amendment, issue
            // #391; the non-`Var` carriers of issue #418), placed where the phpdoc
            // check runs: after every native proof had its chance, so a definite No
            // is never shadowed by the weaker claim about the same argument.
            if !native_fired {
                check_maybe_argument_mismatch(
                    cx,
                    folder,
                    param,
                    &decl.name,
                    arg.span.start,
                    &arg.value,
                    env,
                    store,
                    this_exact,
                    enclosing_class,
                    poisoned,
                    in_descent,
                    false, // a userland parameter: PHP's table has no null carve-out
                    out,
                );
            }
        }

        // Only the propagation-carrier arg kinds (`$var`/`call()`/`$o->m()`) are the
        // propagation pass's to phpdoc-check; literal/array/`new` args are owned
        // by the direct pass (no double-report across the two passes). A method call
        // (issue #386) belongs on this side of that split for the reason the split
        // exists — it is resolved against the walk, not read off the syntax — and
        // naming it here is what keeps it from being checked by both passes or by
        // neither.
        if !native_fired
            && matches!(arg.value, ArgValue::Var(_) | ArgValue::Call(..) | ArgValue::MethodCall { .. })
            && let Some(env_e) = &envelopes
        {
            check_phpdoc_param(
                cx,
                folder,
                env_e,
                param,
                site.file,
                decl.span.start,
                &decl.name,
                arg.span.start,
                &arg.value,
                env,
                store,
                poisoned,
                in_descent,
                out,
            );
        }
    }

    // Named arguments (`f(n: <expr>)`, Gap A): bind each to its parameter by name and
    // run the same declared-contract judgment. Owned solely by this pass for function
    // calls — the direct pass never touches named arguments — so no double-report.
    if let Some(env_e) = &envelopes {
        check_named_phpdoc_params(
            cx,
            folder,
            env_e,
            &decl.params,
            call.args.len(),
            site.file,
            decl.span.start,
            &decl.name,
            &call.named_args,
            env,
            store,
            poisoned,
            in_descent,
            out,
        );
    }
}
