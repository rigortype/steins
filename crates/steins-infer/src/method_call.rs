//! Method and static calls on the value lane: the resolved-call outcome, `$this`
//! seeding, null-safe calls, receiver objects, and the argument checks at the
//! site.

use std::collections::HashMap;

use steins_domain::Fact;
use steins_syntax::{ArgValue, CallExpr, Callee, MethodDecl, Receiver, Scope};

use crate::Folder;
use crate::arg_check::{
    check_maybe_argument_mismatch, implicit_null_accepted, is_type_error, object_world_guard_blind,
};
use crate::builtin_returns::store_holds_resource;
use crate::contract::{TemplateShadow, template_names_of};
use crate::cx::Cx;
use crate::descent::{ThisSeed, ThisWriteBack, descend, project_method_summary, runs_with_same_this};
use crate::dispatch::resolve_call_target;
use crate::env::{
    ContractArm, Descent, HeapObj, HeapSummary, Known, ReturnSummary, Store, Stratum, arg_of_val,
};
use crate::generics::{check_callable_arg, check_named_phpdoc_params, check_phpdoc_param};
use crate::heap::{CtorDefaults, constructed_object, new_heap_object, simple_class};
use crate::project::Diagnostic;
use crate::return_arms::{bindable_args, method_return_arms_at_call};

/// Outcome of a resolved method/static call (ADR-0075): return-fact summary
/// **and** declared return arms, both computed against the store **before** the
/// assignment may unbind a self-assign receiver (`$o = $o->m(1)`).
pub(crate) struct MethodCallOutcome {
    pub(crate) summary: Option<ReturnSummary>,
    pub(crate) return_arms: Option<Vec<ContractArm>>,
    /// A **constructor** call's `$this` snapshot (ADR-0057's constructor-summary
    /// amendment, C2/C3): the object this `new` site yields, for the statement's own
    /// object build to consume. Filled only for `Callee::Construct`, and `None`
    /// wherever the descent declined (C6), which leaves the site on the ADR-0086 §4
    /// lexical floor.
    pub(crate) ctor_heap: Option<HeapSummary>,
    /// The `$this` snapshot to copy back into a **caller-named** object (ADR-0057's
    /// 2026-08-17 amendment, D2/D4): the walk's own `$this` for a same-`$this` call,
    /// the receiver's variable for an exact `$o->m()`. `None` at every decline (D5),
    /// which leaves the statement's sweep standing as the floor.
    pub(crate) this_back: Option<ThisWriteBack>,
}

/// Check + descend one method / static / constructor call. Returns the callee's
/// summary and declared return arms when the target resolves (ADR-0075), and — for a
/// constructor — the `$this` snapshot the object build consumes (ADR-0057 C7). A
/// constructor's *value* summary stays unread, and for the reason ADR-0075 §3 gave:
/// a constructor evaluates to an object, and an object is not a value.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_method_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    mut descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> MethodCallOutcome {
    let empty =
        MethodCallOutcome { summary: None, return_arms: None, ctor_heap: None, this_back: None };
    let Some(mut target) =
        resolve_call_target(cx, &call.receiver, store, this_exact, enclosing_class, scope.poisoned)
    else {
        return empty;
    };

    // The receiver's own object for `(new C(1))->m()` (issue #386): minted — and its
    // constructor walked — right here, this being the receiver `new`'s only site
    // (ADR-0057 C7's third seam). It is the one receiver form whose object did not
    // exist when the target resolved, so both readers that wanted it are filled here:
    // the class-level generic carries below, and the `$this` seed further down.
    let recv_new = receiver_new_object(
        cx,
        folder,
        &call.receiver,
        env,
        store,
        scope.poisoned,
        call.span.start,
        descent.as_deref_mut(),
        out,
    );
    if let Some(obj) = &recv_new {
        target.receiver_carries = obj.targs.clone();
    }

    // Capture arms at resolution — before any later store mutation at the assign site.
    // A `?->` call rebinds neither arms nor summary (ADR-0075 §3.1): its result is
    // `null` whenever the receiver is, which neither of them says.
    let return_arms = if matches!(call.receiver, Callee::Construct { .. })
        || nullsafe_call(&call.receiver)
    {
        None
    } else {
        // With the call's arguments in hand, so a METHOD-level `@template` binds
        // from them too (issue #363). The receiver's carries are untouched — the
        // two readers index different state and neither sees the other's names.
        let bindable = bindable_args(call);
        method_return_arms_at_call(
            cx,
            folder,
            &target,
            &bindable,
            env,
            store,
            scope.poisoned,
        )
    };

    let callee_name = format!("{}::{}", target.declaring_class.name, target.method.name);
    let class_templates = template_names_of(target.declaring_class.docblock.as_deref());
    // Runs for every argument shape — positional prefix and named arguments
    // (Gap A: `new Foo(n: 0)` / `$o->m(n: 0)` were previously skipped wholesale
    // by the `positional_only` guard below).
    check_method_args(
        cx,
        folder,
        target.method,
        target.class_file,
        &class_templates,
        &callee_name,
        call,
        env,
        store,
        this_exact,
        enclosing_class,
        scope.poisoned,
        descent.is_some(),
        out,
    );

    // The binding descent maps positional arguments to parameters, so it stays
    // positional-only (named/spread parameter binding is not modeled here); the
    // contract check above already covered the arguments.
    if !call.positional_only {
        return MethodCallOutcome { summary: None, return_arms, ctor_heap: None, this_back: None };
    }
    let Some(callee_scope) =
        cx.method_scope(target.class_file, &target.declaring_class.fqn, &target.method.name)
    else {
        return MethodCallOutcome { summary: None, return_arms, ctor_heap: None, this_back: None };
    };
    let display = display_of_call(&call.receiver, &target.declaring_class.name, &target.method.name);
    let arg_values: Vec<&ArgValue> = call.args.iter().map(|a| &a.value).collect();
    // A constructor descent's `$this` seed (ADR-0057 C1): the very object this `new`
    // site is minting, with every literal default and every promoted parameter, built
    // through the SAME `new_heap_object` the object build will fall back to. Skipped
    // for a poisoned caller, whose `new` binds nothing anyway.
    let ctor_seed: Option<HeapObj> = match &call.receiver {
        Callee::Construct { class } if !scope.poisoned => {
            let positional: Vec<ArgValue> = arg_values.iter().map(|v| (*v).clone()).collect();
            Some(new_heap_object(
                cx,
                folder,
                &cx.class_fqn(class),
                &positional,
                &call.named_args,
                env,
                store,
                false,
                CtorDefaults::All,
            ))
        }
        _ => None,
    };
    // The same-`$this` seed (ADR-0057's 2026-08-17 amendment, D1): a call the walk
    // makes with its OWN `$this` hands the callee a copy of it, so a delegating
    // `$this->init()` and a `parent::__construct()` write the object this walk holds
    // rather than a store that dies at the boundary. A resolved **static** target
    // carries no `$this` (issue #417's other half) and seeds nothing.
    let same_this = !target.method.is_static
        && store.is_bound("this")
        && runs_with_same_this(
            cx, &call.receiver, store, this_exact, enclosing_class, scope.poisoned,
        );
    let seed = this_seed_of(
        ctor_seed.as_ref(),
        recv_new.as_ref(),
        target.receiver_var.as_deref(),
        same_this,
    );
    // The caller name the copy-back writes into, decided from the same seed so the two
    // can never name different objects (D2): the walk's own `$this`, or the receiver's
    // variable. A constructor's snapshot goes to the `new` site instead, and a
    // receiver-position `new` mints an object no name survives to observe.
    let back_var: Option<String> = match &seed {
        Some(ThisSeed::SameThis) => Some("this".to_owned()),
        Some(ThisSeed::ReceiverVar(v)) => Some((*v).to_owned()),
        Some(ThisSeed::Ctor(_) | ThisSeed::ReceiverNew(_)) | None => None,
    };
    // ADR-0075: the same `descend` path functions use; the walk-trace rebinds the
    // value summary for method/static calls and the heap snapshot for constructors.
    let summary = descend(
        cx,
        folder,
        &target.method.params,
        target.class_file,
        callee_scope,
        &format!("{}::{}", target.declaring_class.fqn, target.method.name),
        &display,
        target.this_exact,
        seed,
        &arg_values,
        call.span.start,
        &[],
        env,
        store,
        scope.poisoned,
        descent,
        out,
    );
    // The copy-back the statement applies after its sweeps (D4). `None` at every
    // decline, which is what leaves the C5 sweep standing as the floor.
    let this_back = back_var.zip(summary.as_ref().and_then(|s| s.this.clone())).map(
        |(var, snapshot)| ThisWriteBack { var, obj: snapshot.obj },
    );
    match ctor_seed {
        // A constructor's summary is its `$this` component and nothing else (ADR-0075
        // §3 as superseded): the value component cannot exist, an object being no
        // value, and the heap one describes a `return` a constructor cannot write.
        Some(_) => MethodCallOutcome {
            summary: None,
            return_arms,
            ctor_heap: summary.and_then(|s| s.this),
            this_back: None,
        },
        // The body was walked for its diagnostics either way; what a `?->` refuses is
        // the rebind (ADR-0075 §3.1) — and the copy-back with it, the receiver being
        // the very thing that may be `null`.
        None if nullsafe_call(&call.receiver) => {
            MethodCallOutcome { summary: None, return_arms, ctor_heap: None, this_back: None }
        }
        None => MethodCallOutcome { summary, return_arms, ctor_heap: None, this_back },
    }
}

/// The `$this` seed of a method/constructor descent, from the four sources
/// [`handle_method_call`] and [`project_method_summary`] may hold — mutually
/// exclusive by construction (a call has one receiver), ordered here only so the
/// exclusion is stated once rather than at each caller.
pub(crate) fn this_seed_of<'a>(
    ctor: Option<&'a HeapObj>,
    recv_new: Option<&'a HeapObj>,
    receiver_var: Option<&'a str>,
    same_this: bool,
) -> Option<ThisSeed<'a>> {
    ctor.map(ThisSeed::Ctor)
        .or_else(|| recv_new.map(ThisSeed::ReceiverNew))
        .or_else(|| receiver_var.map(ThisSeed::ReceiverVar))
        .or_else(|| same_this.then_some(ThisSeed::SameThis))
}

/// Whether a call short-circuits to `null` on a `null` receiver — the `?->` form
/// (ADR-0075 §3.1). Neither the callee's summary nor its declared return arms
/// describe such a result, so both are declined at every rung that would rebind
/// them; everything else about the call is judged as usual.
pub(crate) fn nullsafe_call(receiver: &Callee) -> bool {
    matches!(receiver, Callee::Method { nullsafe: true, .. })
}

/// The object a **receiver-position** `new` mints — `(new C(1))->m()` — with its
/// constructor walked (ADR-0057 C7's third seam, issue #386), or `None` for every
/// other receiver.
///
/// The lowering builds no `Callee::Construct` call for a receiver `new`, exactly as
/// it builds none for an argument-position one, so this is that site's only site and
/// the `new` is still walked once. `constructed_object` is the shared body: the
/// snapshot its exits agree on, or the ADR-0086 §4 lexical floor at every decline.
#[allow(clippy::too_many_arguments)]
pub(crate) fn receiver_new_object(
    cx: &Cx,
    folder: &mut dyn Folder,
    receiver: &Callee,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    span_start: u32,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<HeapObj> {
    let Callee::Method { receiver: Receiver::New { class, args, named }, .. } = receiver else {
        return None;
    };
    if poisoned {
        return None;
    }
    let class = cx.class_fqn(class);
    Some(constructed_object(cx, folder, &class, args, named, env, store, span_start, descent, out))
}

/// The provenance render base for a bound method/constructor call.
pub(crate) fn display_of_call(receiver: &Callee, declaring_class: &str, method: &str) -> String {
    match receiver {
        Callee::Construct { class } => format!("new {}", class.simple()),
        _ => format!("{declaring_class}::{method}"),
    }
}

/// Check the arguments of a resolved method/constructor call at its call site
/// (native runtime check plus the phpdoc declared-contract check; no double-report).
/// `class_file` locates the callee method's docblock context for class-name
/// resolution.
#[allow(clippy::too_many_arguments)]
fn check_method_args(
    cx: &Cx,
    folder: &mut dyn Folder,
    method: &MethodDecl,
    class_file: usize,
    class_templates: &TemplateShadow,
    callee_name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    // The CALLER's frame, for a method-call argument with a `$this`/`self::`/
    // `parent::` receiver — the caller's own, never the callee's (issue #386).
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
    in_descent: bool,
    out: &mut Vec<Diagnostic>,
) {
    let mut envelopes = cx.envelopes_of(method.docblock.as_deref(), class_file, method.span.start);
    // Class-level `@template` names shadow same-named classes in every member
    // docblock of the class-like (issue #5) — the second, idempotent shadow stage.
    if let Some(e) = &mut envelopes {
        e.shadow_templates(class_templates);
    }
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = method.params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }

        let mut native_fired = false;
        if let Some(ty) = param.ty.as_ref() {
            // Resolved value + provenance + trust stratum (issue #127 review): uses
            // the stratum that arrived with the resolution, not a syntactic
            // re-read that would launder an Asserted fold to Verified.
            let resolved: Option<(ArgValue, Option<String>, Stratum)> = match &arg.value {
                v if v.is_literal() => Some((v.clone(), None, Stratum::Verified)),
                ArgValue::Var(name) if !poisoned => env.get(name).and_then(|k| {
                    let v = k.singleton()?;
                    let prov = match &k.bound {
                        Some(b) => format!("from ${name}, {b}"),
                        None => format!("from ${name}, assigned at line {}", k.line),
                    };
                    Some((v, Some(prov), k.stratum))
                }),
                ArgValue::Call(name, args) => {
                    if args.is_empty() {
                        cx.resolve_const_fn(name)
                            .map(|(lit, line)| {
                                (
                                    lit,
                                    Some(format!("from {name}(), defined at line {line}")),
                                    Stratum::Verified,
                                )
                            })
                            .or_else(|| {
                                cx.try_fold_emit(name, args, env, poisoned, folder, out)
                                    .map(|(l, p, s)| (l, Some(p), s))
                            })
                    } else {
                        cx.try_fold_emit(name, args, env, poisoned, folder, out)
                            .map(|(l, p, s)| (l, Some(p), s))
                    }
                }
                // A nested method / static call (issue #386), the twin of the
                // function-call arm the propagated check runs — same rungs, same
                // `Verified`-only gate, same plain-pass restriction.
                ArgValue::MethodCall { callee, args, named } if !in_descent => {
                    project_method_summary(
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
                        arg.span.start,
                        None,
                        out,
                    )
                    .and_then(|s| {
                        let sv = s.value?;
                        let Fact::Singleton(v) = &sv.fact else { return None };
                        Some((
                            arg_of_val(v),
                            Some(format!("returned from {}", arg.value.render())),
                            sv.stratum,
                        ))
                    })
                }
                // A property read `$o->p` (ADR-0036): a `Singleton` prop fact flows.
                ArgValue::PropFetch { var, prop } if !poisoned => {
                    store.prop_fact(var, prop).and_then(|f| match f {
                        Fact::Singleton(v) => Some((
                            arg_of_val(v),
                            Some(format!("from ${var}->{prop}")),
                            store.prop_stratum(var, prop),
                        )),
                        _ => None,
                    })
                }
                // A proven object (`new` / enum case) or resolved class constant
                // (ADR-0043 stage 3). Env-free; `self`/`parent` at the call site are
                // not available here, so only a written class name resolves.
                _ => cx
                    .resolve_static_value(&arg.value, None)
                    .map(|v| (v, None, Stratum::Verified)),
            };
            // Proof-layer consumption rule (ADR-0052 §5): silent on an `Asserted`
            // premise; the phpdoc contract check below still accepts it.
            if let Some((value, prov, strat)) = resolved
                && strat == Stratum::Verified
                && is_type_error(cx, ty, &value)
                && !implicit_null_accepted(param, &value)
                && !object_world_guard_blind(in_descent, ty, &value)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &value,
                    prov.as_deref(),
                    callee_name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // A variable bound to a proven object (ADR-0036 heap): the object-vs-type
            // definite-No, rendered against the variable (ADR-0043 stage 3).
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
                    callee_name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // The resource sibling of the branch above (ADR-0056 §8); see the
            // twin in the propagation pass for why it is not guard-blind.
            if !native_fired
                && !poisoned
                && let ArgValue::Var(name) = &arg.value
                && store_holds_resource(store, name)
                && cx.resource_is_type_error(ty)
            {
                out.push(cx.resource_diagnostic(
                    arg.span.start,
                    name,
                    callee_name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // The possibly-grade sibling, method-call twin (issue #391; the
            // non-`Var` carriers of issue #418).
            if !native_fired {
                check_maybe_argument_mismatch(
                    cx,
                    folder,
                    param,
                    callee_name,
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

        if !native_fired
            && let Some(env_e) = &envelopes
        {
            check_phpdoc_param(
                cx,
                folder,
                env_e,
                param,
                class_file,
                method.span.start,
                callee_name,
                arg.span.start,
                &arg.value,
                env,
                store,
                poisoned,
                in_descent,
                out,
            );
        }
        // Callable-signature variance (issue #11) for a closure / first-class
        // callable argument to a method against a signature-bearing `callable(...)`
        // @param. The closure's declared signature is a static CST fact, so this is
        // safe to run at the resolved call site.
        if let ArgValue::Closure(closure) = &arg.value
            && let Some(env_e) = &envelopes
        {
            check_callable_arg(cx, env_e, param, callee_name, arg.span.start, closure, out);
        }
    }

    // Named arguments (`$o->m(n: <expr>)` / `new Foo(n: <expr>)`, Gap A): bind each
    // to its parameter by name and run the same declared-contract judgment. This is
    // the sole contract lane for method/constructor calls, so no double-report.
    if let Some(env_e) = &envelopes {
        check_named_phpdoc_params(
            cx,
            folder,
            env_e,
            &method.params,
            call.args.len(),
            class_file,
            method.span.start,
            callee_name,
            &call.named_args,
            env,
            store,
            poisoned,
            in_descent,
            out,
        );
    }
}
