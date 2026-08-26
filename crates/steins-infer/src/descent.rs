//! Interprocedural descent (Feature B, ADR-0009 budget): escaping and sweeping
//! calls, propagating a proven argument into a resolved callee, the project call /
//! method summaries, `$this` seeding across the call, and joining the callee's exits
//! back into the caller's env.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::{Base, Fact, PhpStr};
use steins_syntax::{
    ArgValue, CallExpr, Callee, NameRef, NamedArg, NativeType, Param, Receiver, RefKind,
    RetHintKind, ScalarType, Scope, ScopeOwner, StaticClass, StmtKind, TypeMember,
};

use crate::fold::Folder;
use crate::MAX_BINDING_DEPTH;
use crate::arg_check::{
    check_maybe_argument_mismatch, implicit_null_accepted, is_type_error, object_world_guard_blind,
    render_call,
};
use crate::builtin_returns::store_holds_resource;
use crate::coerce::{coerce_fact_to_native, coerce_into_param};
use crate::contract::IsA;
use crate::cx::Cx;
use crate::dispatch::resolve_call_target;
use crate::env::{
    AllocId, BindingKey, ClosureTarget, ContractArm, Descent, ExitContribution, HeapObj,
    HeapSummary, Known, PropFact, ReturnSummary, Store, Stratum, SummaryValue, arg_of_fact_key,
    arg_of_val, singleton_fact,
};
use crate::generics::{check_named_phpdoc_params, check_phpdoc_param};
use crate::heap::{
    CtorDefaults, argument_heap_object, copy_for_descent, new_heap_object, object_binding_key,
    simple_class,
};
use crate::method_call::{display_of_call, nullsafe_call, receiver_new_object, this_seed_of};
use crate::project::{Diagnostic, Site};
use crate::refine::refine_contract_arms;
use crate::return_arms::{fn_return_arms, native_arms};
use crate::walk::{WalkCx, analyze_scope};

/// The class FQN that lexically owns a method scope; `None` for function/top.
pub(crate) fn scope_class(scope: &Scope) -> Option<&str> {
    match &scope.owner {
        // A property hook body runs in its declaring class's scope with `$this`
        // bound (issue #544) — the same answer a method body gets, for the same reason.
        ScopeOwner::Method { class, .. } | ScopeOwner::PropertyHook { class, .. } => Some(class),
        // A closure lexically inside a method captures `$this`, but the analyzer
        // does not thread the enclosing class into the closure scope (documented).
        ScopeOwner::TopLevel | ScopeOwner::Function(_) | ScopeOwner::Closure { .. } => None,
    }
}

/// The statically-named calls a statement carries.
pub(crate) fn checkable_calls(kind: &StmtKind) -> Vec<&CallExpr> {
    match kind {
        StmtKind::Call(c) => vec![c],
        StmtKind::Return { call: Some(c), .. }
        | StmtKind::Assign { call: Some(c), .. }
        | StmtKind::PropAssign { value_call: Some(c), .. } => vec![c],
        StmtKind::Echo(cs) => cs.iter().collect(),
        _ => Vec::new(),
    }
}

/// Escape + sweep the heap for a statement's calls (ADR-0036). Passing an object
/// (as an argument, or as the `$var` receiver of a method call) escapes it. If any
/// object was passed into a call, or any call is unknown/overridable (not resolved
/// to a project target), sweep every escaped object's non-readonly props. A purely
/// local object never passed anywhere survives an unrelated unknown call — the
/// precision payoff.
pub(crate) fn apply_call_escape_and_sweep(
    w: &WalkCx,
    kind: &StmtKind,
    store: &mut Store,
    this_backs: &[ThisWriteBack],
) {
    let calls = checkable_calls(kind);
    escape_and_sweep_calls(w, &calls, store, this_backs);
}

/// One resolved call's `$this` snapshot, waiting to be copied back into the caller's
/// own object (ADR-0057's 2026-08-17 amendment, D4). Produced by
/// [`handle_method_call`] where the descent seeded `$this` from an object a caller
/// **name** still denotes, and applied by [`escape_and_sweep_calls`] after the
/// statement's sweeps — the ordering being the whole of "skip the sweep for that
/// call": the sweep clears the props, this writes the walk's truth over the result.
///
/// [`handle_method_call`]: crate::method_call::handle_method_call
pub(crate) struct ThisWriteBack {
    /// The caller variable whose object the snapshot replaces: `"this"` for a
    /// same-`$this` call, the receiver's own variable for an exact `$o->m()`.
    pub(crate) var: String,
    /// The joined snapshot. `class`/`class_exact` are asserted rather than copied —
    /// no walk alters what class an allocation is (C4's field list).
    pub(crate) obj: HeapObj,
}

/// Escape + sweep for an explicit set of calls (ADR-0036), shared by the
/// statement-position pass ([`apply_call_escape_and_sweep`]) and the guard-position
/// retained-call handling (ADR-0052 §6): a guard call's object arguments and its
/// method receiver escape, and any object passed in — or any unknown/overridable
/// call — sweeps every escaped object's non-readonly props. The receiver's var→id
/// *binding* survives (a method call does not rebind its receiver variable), so the
/// receiver stays usable on the guarded path; only its mutable props are swept.
///
/// `this_backs` carries the `$this` snapshots this statement's resolved descents came
/// back with (D4). They are applied **last**, over whatever the sweeps left, and the
/// guard position passes none — a guard call's descent has no statement rung to hand
/// one to.
pub(crate) fn escape_and_sweep_calls(
    w: &WalkCx,
    calls: &[&CallExpr],
    store: &mut Store,
    this_backs: &[ThisWriteBack],
) {
    if calls.is_empty() {
        return;
    }
    let mut object_passed = false;
    let mut unknown = false;
    for call in calls {
        if let Callee::Method { receiver: Receiver::Var(v), .. } = &call.receiver
            && store.is_bound(v)
        {
            store.mark_escaped(v);
            object_passed = true;
            // The generic-carry half of the same invalidation (ADR-0032 binding
            // amendment, issue #295): a method may rewrite the very values the carry
            // recorded (`@phpstan-self-out self<U>`). Unconditional on the receiver —
            // the callee is the receiver's own class hierarchy, which a variable
            // receiver does not pin down.
            store.sweep_targs(v);
        }
        // The receiver of a `(new C($b))->m()` passes `$b` into the constructor, at a
        // position no top-level argument list names — the same nested case the
        // argument loop below recurses for.
        if let Callee::Method { receiver: Receiver::New { args, named, .. }, .. } = &call.receiver {
            escape_nested_args(args, named, store, &mut object_passed);
        }
        for (i, arg) in call.args.iter().enumerate() {
            if let ArgValue::Var(name) = &arg.value
                && store.is_bound(name)
            {
                store.mark_escaped(name);
                object_passed = true;
                // The argument-pass leg of the same sweep (ADR-0032 binding
                // amendment). A callee that mutates the object it was handed makes
                // the carry stale just as the receiver's own method does, and the
                // failure direction there is a REPORT on correct code, not silence
                // — so the carry survives only where the callee provably cannot
                // reach the object at all.
                if !callee_cannot_reach_arg(w.cx, call, i) {
                    store.sweep_targs(name);
                }
            }
            // …and whatever a NESTED call inside this argument hands on (ADR-0075
            // §3.2): `f(g($b))` and `f($b->m())` pass `$b` just as plainly as
            // `f($b)` does, and used to escape nothing at all.
            escape_nested_calls(&arg.value, store, &mut object_passed);
        }
        if !call_is_resolved(w, call, store) {
            unknown = true;
        }
    }
    if object_passed || unknown {
        store.sweep_escaped();
    }
    // A call that runs with the SAME `$this` — `$this->m(…)`, `parent::m(…)`,
    // `self::m(…)`, `static::m(…)`, `parent::__construct(…)` above all, and a
    // `Foo::m(…)` compatible with the enclosing class-like (issue #417) — writes
    // properties this walk never executes: a descent into it seeds its own `$this`
    // (ADR-0086 §3 fills `receiver_var` for an exact `Receiver::Var` and for nothing
    // else), so its writes land in *its* store; an unresolved one is a body never read
    // at all. So it sweeps the receiver's own non-readonly props and value carries,
    // **whether or not the target resolved** — the resolved private/final case is
    // exactly the one `sweep_escaped` above never covered (ADR-0057 C5).
    //
    // Since the 2026-08-17 amendment that sweep is the **decline floor** (D5): where
    // the target resolved and the descent came back with a snapshot, `this_backs`
    // below overwrites what this sweep cleared with what the callee's walk proved.
    // The sweep runs unconditionally, so every decline lands on it for free.
    //
    // An **unescaped** `$this` is the one heap object `sweep_escaped` passes by (C1 —
    // a constructor's, and a same-`$this` copy inside one), so it is swept by the
    // `object_passed || unknown` condition instead. A non-static closure created in
    // the body binds `$this` without naming it and is invoked through exactly such an
    // unresolved call, which is why the condition is the coarse one and not a leak
    // test. Reading the bit off the object rather than off the walk's flavour states
    // the rule where it lives: `seed_this_object` pre-escapes every other `$this`, so
    // this is `false` exactly where C1 made it so.
    let same_this = calls.iter().any(|c| {
        runs_with_same_this(w.cx, &c.receiver, &*store, w.this_exact, w.enclosing_class, w.scope.poisoned)
    });
    let this_unescaped = store.obj_of("this").is_some_and(|o| !o.escaped);
    if same_this || ((object_passed || unknown) && this_unescaped) {
        store.sweep_this();
    }
    // The copy-back (D4), last and over the sweeps. Two statement-scoped guards, both
    // about a composition that cannot be ordered: an **unresolved** call anywhere in
    // this statement may reach `$this` through a closure alias, and **two** snapshots
    // for one name were each seeded from the same pre-statement object, so the second
    // would erase the first's write. Either way the floor above stands.
    if !unknown {
        for wb in this_backs {
            if this_backs.iter().filter(|o| o.var == wb.var).count() == 1 {
                store.copy_back(&wb.var, &wb.obj);
            }
        }
    }
}

/// Escape + sweep for the calls **nested inside** one argument value (ADR-0075 §3.2,
/// issue #386). `escape_and_sweep_calls` walked a statement's top-level argument
/// list and stopped, so `f(g($b))`, `f($b->m())` and `f(new C($b))` escaped nothing
/// although the inner callee holds the object as plainly as an outer one would.
///
/// Three carriers nest a call: [`ArgValue::Call`], [`ArgValue::MethodCall`] (its
/// **receiver** included — `$b` in `f($b->m())` is handed to `m` as its `$this`) and
/// [`ArgValue::New`]. Each recurses, so depth costs nothing.
///
/// The #295 lexical gate is **not** applied at a nested position, unlike at the top
/// level: it needs a resolved callee and a position-to-parameter map, and the value
/// IR carries no resolution for a nested call. So the carry is swept
/// unconditionally, which is the silent direction.
fn escape_nested_calls(value: &ArgValue, store: &mut Store, object_passed: &mut bool) {
    match value {
        ArgValue::Call(_, args) => escape_nested_args(args, &[], store, object_passed),
        ArgValue::MethodCall { callee, args, named } => {
            match callee {
                Callee::Method { receiver: Receiver::Var(v), .. } if store.is_bound(v) => {
                    store.mark_escaped(v);
                    store.sweep_targs(v);
                    *object_passed = true;
                }
                Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
                    escape_nested_args(args, named, store, object_passed);
                }
                _ => {}
            }
            escape_nested_args(args, named, store, object_passed);
        }
        ArgValue::New(_, args, named) => escape_nested_args(args, named, store, object_passed),
        _ => {}
    }
}

/// One nested call's argument list: each heap-bound variable escapes and loses its
/// value carries, and each argument is itself recursed into. Named arguments count
/// here — a nested call is walked whole, the position-indexed judgments the top
/// level makes having no nested counterpart to skip them for.
fn escape_nested_args(
    args: &[ArgValue],
    named: &[NamedArg],
    store: &mut Store,
    object_passed: &mut bool,
) {
    for value in args.iter().chain(named.iter().map(|n| &n.value)) {
        if let ArgValue::Var(name) = value
            && store.is_bound(name)
        {
            store.mark_escaped(name);
            store.sweep_targs(name);
            *object_passed = true;
        }
        escape_nested_calls(value, store, object_passed);
    }
}

/// Whether a call runs with the **same** `$this` as the walk making it (ADR-0057 C5,
/// closed symmetrically by issue #417): `$this->m(…)`, the `self::`/`parent::`/
/// `static::` spellings of the same thing (`parent::__construct(…)` the shape that
/// matters most), and an explicitly named `Foo::m(…)` where `Foo` resolves to the
/// enclosing class-like or one of its ancestors and `m` is a non-static instance
/// method — PHP forwards `$this` to a by-name static-syntax call exactly when the
/// calling scope's `$this` is an instance of the named class (a "forwarding call",
/// distinct from the deprecated calling-a-non-static-method-statically shape), so
/// `Foo::m()` from inside `Foo` (or a subclass of `Foo`) is `self::m()` under another
/// spelling and `Bar::m()` from somewhere unrelated to `Bar` carries no `$this` at
/// all.
///
/// **Every uncertainty sweeps, on both legs it can arise on.** A completely
/// enumerated hierarchy that excludes the named class ([`IsA::No`]) is the one case
/// treated as unrelated; an incomplete one ([`IsA::Unknown`]) is not. Once the class
/// is admitted, an unresolvable method (abstract, missing from the chain, private-
/// blocked, a poisoned scope) is treated the same as a resolved non-static one — "an
/// unresolved one is a body never read at all" (ADR-0057 C5) applies here exactly as
/// it does to the keyword spellings; only a *resolved* **static** method is proven to
/// carry no `$this`.
///
/// Taken apart rather than as a [`WalkCx`] (issue #420): the seed side asks the same
/// question from [`handle_method_call`] and [`project_method_summary`], neither of
/// which holds one, and the two must never disagree — the sweep is the floor of
/// exactly the descent the seed runs.
///
/// [`handle_method_call`]: crate::method_call::handle_method_call
pub(crate) fn runs_with_same_this(
    cx: &Cx,
    receiver: &Callee,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> bool {
    match receiver {
        Callee::Method { receiver: Receiver::This, .. } => true,
        Callee::Static { class: StaticClass::SelfKw | StaticClass::Parent | StaticClass::Static, .. } => {
            true
        }
        Callee::Static { class: StaticClass::Named(name), .. } => {
            let Some(enclosing) = enclosing_class else { return false };
            let fqn = cx.class_fqn(name);
            if matches!(cx.is_a(enclosing, &fqn), IsA::No) {
                return false; // a completely enumerated hierarchy excludes it
            }
            match resolve_call_target(cx, receiver, store, this_exact, enclosing_class, poisoned) {
                Some(target) => !target.method.is_static,
                None => true, // unresolvable: sweeps, as every uncertainty does
            }
        }
        _ => false,
    }
}

/// Whether the callee of `receiver`, taking an argument at `position`, **provably
/// cannot reach** the object passed there (ADR-0032 binding amendment, issue #295)
/// — the gate that decides whether a generic value carry survives being handed to
/// a call.
///
/// **Why the effects machinery cannot answer this.** The natural oracle would be
/// "this callee does not mutate that argument", and it does not exist today:
/// ADR-0055's mutation family (`mutate.arg`/`.self`/`.instance`/`.static`) is
/// **taxonomy only** — its inference is unbuilt ([`by_ref_label`]), and no
/// property write contributes any effect label ([`steins_syntax::EffectOrigin`]
/// has arms for calls, output, exit, method calls and opaque constructs, none
/// for a property assignment; the only `mutate*` carriers are `mutate.local` and
/// a coarse `mutate`, both from builtin by-ref out-parameters, ADR-0063 §2.3).
/// [`PurityOracle`] cannot stand in for it either — actively unsound here, not
/// merely weak: its `provably_impure` returning `false` means "not proven
/// impure", and since property writes color nothing, `function mutate(Box $b) {
/// $b->value = 's'; }` has an **empty** proven finding set. Gating on purity
/// would keep the carry across exactly the call that invalidates it (ADR-0055's
/// own opening complaint: a `#[\Steins\Pure]` method writing `$this->p` passes
/// silently) — a declared envelope is no better.
///
/// **What is provable instead.** Not "does the callee mutate it" but "**can the
/// callee refer to it at all**". PHP locals are lexical, so a parameter a body
/// never spells cannot be read, written, captured, passed on, or used as a
/// receiver. Every construct that reaches a binding non-lexically (`$$v`,
/// `extract`/`compact`, `eval`, `include`, `global`, a by-ref `use`) is on the
/// ADR-0001 give-up list, sets [`Scope::poisoned`], and is refused below. The
/// scan runs over the body's **source text** ([`FunctionDecl::body_span`])
/// rather than the linear trace — the trace drops nested sub-expressions to
/// [`ArgValue::Other`] and unrecognized statements to [`StmtKind::Barrier`], so
/// `helper($b)` inside `$x = strlen($b->p) + helper($b);` would be invisible to
/// it, and a gate that misses one use keeps a stale carry — the failure
/// direction this amendment exists to close.
///
/// Every uncertainty answers `false` (sweep): an unresolved/dynamic callee, a
/// builtin (an out-parameter row is a mutation contract; no builtin takes a
/// project object it could not touch), a method/static call, a by-ref or
/// variadic position, an argument past declared arity, a poisoned callee body,
/// or unreadable body text. Unknown is never proof of non-mutation.
///
/// **Narrow by construction.** In practice this admits the callee that ignores
/// the parameter — the conformance fixture's `takesIntBox(MutableBox $box):
/// void {}` — and little else. The wider gate needs a real per-parameter
/// non-mutation judgment, whose precondition is ADR-0055 Part II's inference:
/// once a property write colors `mutate.self`/`mutate.instance`, "this callee
/// mutates nothing an argument can reach" becomes a fixpoint question.
///
/// [`PurityOracle`]: crate::purity::PurityOracle
/// [`FunctionDecl::body_span`]: steins_syntax::FunctionDecl::body_span
fn callee_cannot_reach_arg(cx: &Cx<'_>, call: &CallExpr, position: usize) -> bool {
    if !matches!(call.receiver, Callee::Function(_)) {
        return false;
    }
    // `resolve_user_fn` carries the positional-only guard, which this gate needs
    // literally: a named or spread argument defeats the position→parameter mapping
    // the whole judgment is indexed by.
    let Some(site) = cx.resolve_user_fn(call) else { return false };
    let decl = cx.fn_decl(site);
    // The parameter must exist, take its argument by value, and not be variadic —
    // the same three refusals [`arg_is_by_value`] makes, for the same reasons.
    let Some(param) = decl.params.get(position) else { return false };
    if param.by_ref || param.variadic {
        return false;
    }
    // A poisoned body can reach a binding without spelling it; the lexical argument
    // does not hold there.
    if !matches!(cx.fn_scope(site), Some((_, body)) if !body.poisoned) {
        return false;
    }
    let Some((file, _)) = cx.fn_scope(site) else { return false };
    let Some(text) = cx.units[file].tree.text_at(decl.body_span) else { return false };
    !mentions_variable(text, &param.name)
}

/// Whether `text` spells the PHP variable `$name` as a whole token.
///
/// Token boundaries are what keep `$box` from matching inside `$boxes` or
/// `$my_box`; a match inside a string literal or a comment is *accepted* as a
/// mention, which errs toward sweeping and so toward silence.
fn mentions_variable(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let needle = name.as_bytes();
    let is_name_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || !b.is_ascii();
    let mut i = 0usize;
    while let Some(off) = text[i..].find('$') {
        let dollar = i + off;
        let start = dollar + 1;
        i = start;
        // `$$v` is a variable-variable, which poisons the scope — but a body that
        // reached here is unpoisoned, so a second `$` is simply not our token.
        if bytes.get(start).copied() == Some(b'$') {
            continue;
        }
        if !bytes[start..].starts_with(needle) {
            continue;
        }
        if bytes.get(start + needle.len()).copied().is_some_and(is_name_byte) {
            continue; // a longer name that merely starts with `name`
        }
        return true;
    }
    false
}

/// Whether a call resolves to a known project/user target (ADR-0036). An unresolved
/// function (builtin/unknown/dynamic) or an unresolved-via-guard method (an
/// overridable `$this`/`self` call) counts as unknown — the sweeping side.
fn call_is_resolved(w: &WalkCx, call: &CallExpr, store: &Store) -> bool {
    match &call.receiver {
        Callee::Function(_) => w.cx.resolve_user_fn(call).is_some(),
        Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
            resolve_call_target(
                w.cx, &call.receiver, store, w.this_exact, w.enclosing_class, w.scope.poisoned,
            )
            .is_some()
        }
        Callee::DynamicVar(_) | Callee::Dynamic => false,
    }
}

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
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_propagated_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    in_descent: bool,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    // The caller's own frame, for a method-call ARGUMENT whose receiver is
    // `$this`/`self::`/`parent::` (issue #386). The dump surface reads the same two
    // off its `WalkCx`; this check is called from the same statement walk, so
    // threading them costs one call site and buys the receiver spellings a
    // frame-less seam has to decline.
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    out: &mut Vec<Diagnostic>,
) {
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

/// Attempt an interprocedural binding descent into a same-project function. Returns
/// the callee's return-fact summary (ADR-0057 amendment T0), if one was computed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_descend_function(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<ReturnSummary> {
    let site = cx.resolve_user_fn(call)?;
    let decl = cx.fn_decl(site);
    let (callee_file, callee_scope) = cx.fn_scope(site)?;
    let arg_values: Vec<&ArgValue> = call.args.iter().map(|a| &a.value).collect();
    descend(
        cx,
        folder,
        &decl.params,
        callee_file,
        callee_scope,
        &decl.fqn,
        &decl.name,
        None,
        None,
        &arg_values,
        call.span.start,
        &[],
        env,
        store,
        poisoned,
        descent,
        out,
    )
}

/// The T0 binding descent reached from **value position** (issue #60): resolve a
/// project function by its unique simple name and compute its [`ReturnSummary`]
/// for these argument values. This is what makes `dumpType(greet(2, "World"))`,
/// `takesInt(g(1))` and `$x = f(g(1))` see the same summary the assignment form
/// `$x = greet(2, "World")` always saw — the machinery is [`descend`] verbatim,
/// only the entry point differs.
///
/// **Name resolution** is the `resolve_const_fn` precedent: an [`ArgValue::Call`]
/// carries the call's **simple name only** (lowering takes the identifier's last
/// segment; no [`NameRef`] survives into the value IR), so resolution here is
/// `unique_fn_by_simple` — the same rule the zero-argument `resolve_const_fn`
/// value lane has always used. A project with two same-named functions in
/// different namespaces declines (the statement-level descent, with the full
/// `NameRef`, still resolves those). Widening this means carrying the resolved
/// FQN in the value IR — a deliberate non-goal.
///
/// **Recursion discipline**: the caller's `descent` MUST be threaded whenever
/// one is live — the on-stack binding-key guard turns mutual recursion (`f`
/// calling `g` calling `f`) into a bounded decline instead of an unbounded tree
/// of fresh stacks. A `None` here is only correct at a **plain-pass** entry (the
/// dump surface and the propagated-argument check, both `descent.is_none()`-
/// gated), where the fresh tree is the same shape `try_descend_function` has
/// always created. Expression nesting across such trees is bounded by the
/// source's own nesting depth — each level is one bounded tree, never a loop.
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_call_summary(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    span_start: u32,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<ReturnSummary> {
    let site = value_lane_fn_site(cx, folder, name)?;
    let decl = cx.fn_decl(site);
    let (callee_file, callee_scope) = cx.fn_scope(site)?;
    let arg_values: Vec<&ArgValue> = args.iter().collect();
    descend(
        cx,
        folder,
        &decl.params,
        callee_file,
        callee_scope,
        &decl.fqn,
        &decl.name,
        None,
        None,
        &arg_values,
        span_start,
        &[],
        env,
        store,
        poisoned,
        descent,
        out,
    )
}

/// [`project_call_summary`]'s **method** twin (issue #386): the return summary of a
/// method or static call written in value position — `takesString($b->unwrap())`,
/// `dumpType($b->get())`, `Foo::m(1)`, `(new C(1))->m()`.
///
/// **One resolver, one walk.** The target comes from `resolve_call_target` and from
/// nothing else, and the descent is entered with that target's `this_exact` and its
/// `$this` seed — exactly as [`handle_method_call`] enters it. So the `BindingKey`
/// this site builds is byte-identical to the one the statement rung builds for the
/// same call, and the memo therefore treats `$x = $b->m(); f($b->m());` as one
/// entry: one body walk, one emission of whatever that body reports. A second
/// resolver, or the same resolver entered with a different `$this`, would silently
/// double both.
///
/// **Where the enclosing class is not in hand** the caller passes `None` for
/// `this_exact`/`enclosing_class` and the three receivers that need it —
/// `$this->`, `self::`, `parent::` — decline through `resolve_call_target`'s own
/// arms rather than through a refusal of this function's. `Receiver::Prop` is never
/// a target (ADR-0052 §7), also for free.
///
/// The two refusals that ARE this function's: a **nullsafe** call, whose result may
/// be `null` for a reason no summary states (ADR-0075 §3.1), and a **named**
/// argument list, which the positional binding descent cannot map — the same gate
/// `handle_method_call` applies through `CallExpr::positional_only`.
///
/// [`handle_method_call`]: crate::method_call::handle_method_call
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_method_summary(
    cx: &Cx,
    folder: &mut dyn Folder,
    callee: &Callee,
    args: &[ArgValue],
    named: &[NamedArg],
    env: &HashMap<String, Known>,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
    span_start: u32,
    mut descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<ReturnSummary> {
    if nullsafe_call(callee) || !named.is_empty() {
        return None;
    }
    let mut target = resolve_call_target(cx, callee, store, this_exact, enclosing_class, poisoned)?;
    let recv_new = receiver_new_object(
        cx,
        folder,
        callee,
        env,
        store,
        poisoned,
        span_start,
        descent.as_deref_mut(),
        out,
    );
    if let Some(obj) = &recv_new {
        target.receiver_carries = obj.targs.clone();
    }
    let callee_scope =
        cx.method_scope(target.class_file, &target.declaring_class.fqn, &target.method.name)?;
    let arg_values: Vec<&ArgValue> = args.iter().collect();
    // The same-`$this` seed, on the same terms the statement rung uses — which is the
    // point: `f($this->m())` and `$this->m();` must build the SAME `BindingKey` or
    // the memo doubles the walk and the emission. Its `$this` component is dropped
    // here, deliberately (ADR-0057's 2026-08-17 amendment, D6): this road holds the
    // caller's store by shared reference and has no channel back to the statement
    // walk that owns it, so a value-position same-`$this` call stays on the C5 sweep
    // floor — silent about props, never stale.
    let same_this = !target.method.is_static
        && store.is_bound("this")
        && runs_with_same_this(cx, callee, store, this_exact, enclosing_class, poisoned);
    descend(
        cx,
        folder,
        &target.method.params,
        target.class_file,
        callee_scope,
        &format!("{}::{}", target.declaring_class.fqn, target.method.name),
        &display_of_call(callee, &target.declaring_class.name, &target.method.name),
        target.this_exact,
        this_seed_of(None, recv_new.as_ref(), target.receiver_var.as_deref(), same_this),
        &arg_values,
        span_start,
        &[],
        env,
        store,
        poisoned,
        descent,
        out,
    )
}

/// The project function a **value-position** simple name may be trusted to mean
/// (issue #60) — `unique_fn_by_simple` hardened against two ways a written
/// simple name can target a *different* function at runtime: a **conditional
/// declaration** (the `function_exists`-guarded polyfill shape, ADR-0049 A2i,
/// where which body binds is a load-order fact — declined, the same re-damming
/// instinct the arity check applies), and a **homonym of a runtime function**
/// (a namespaced project function's unqualified call outside its namespace
/// falls back to the runtime function; a global homonym could not even have
/// loaded — both decline, the ADR-0061 posture on a shadowed builtin, pinned by
/// `a_project_function_shadowing_the_name_declines`). The runtime is asked
/// three ways, any positive answer declining: the boot-surface reflect oracle, a
/// reflected builtin return type, and (folderless — the playground) the static
/// catalog standing in for common builtins.
///
/// Not closed: a `use function … as …` alias shadowing a same-named project
/// function at one call site — the written simple name is all the value IR
/// carries (shared verbatim with `resolve_const_fn`); closing it means carrying
/// the resolved FQN in [`ArgValue::Call`], a follow-up.
pub(crate) fn value_lane_fn_site(cx: &Cx, folder: &mut dyn Folder, name: &str) -> Option<Site> {
    let site = cx.index.unique_fn_by_simple(name)?;
    let decl = cx.fn_decl(site);
    if decl.conditional {
        return None;
    }
    let runtime_knows = matches!(folder.boot_surface_function(name), Some(true))
        || folder.builtin_return_type(name).is_some()
        || steins_catalog::foldable(name)
        || steins_catalog::effect_labels(name).is_some();
    if runtime_knows {
        return None;
    }
    Some(site)
}

/// [`project_call_summary`] and [`project_method_summary`] narrowed to what a
/// **binding** can consume (issue #60, extended to methods by issue #386):
/// a `Singleton` summary as the concrete [`ArgValue`] it names, with the summary's
/// stratum. `None` for anything else — a non-call, a zero-argument call (that is
/// `resolve_const_fn`'s lane, already tried by `resolve_literal`), an abstract or
/// absent summary. A non-`Singleton` fact (say `positive-int`) is real knowledge,
/// but a bound parameter seeds from a concrete value; carrying abstract facts into
/// bindings is a documented ceiling, not an oversight.
#[allow(clippy::too_many_arguments)]
pub(crate) fn nested_call_singleton(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    span_start: u32,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<(ArgValue, Stratum)> {
    let summary = match value {
        ArgValue::Call(name, cargs) => {
            if cargs.is_empty() {
                return None;
            }
            project_call_summary(
                cx, folder, name, cargs, env, store, poisoned, span_start, descent, out,
            )?
        }
        // A method call binds like a function call (issue #386), with one difference
        // it cannot help: this seam holds no caller **frame**, only the caller's env
        // and store, so `$this->`/`self::`/`parent::` receivers decline here — the
        // `None`s below are what makes `resolve_call_target` refuse them. A
        // `$var`/`Foo::`/`(new C())` receiver needs no frame and resolves. There is
        // no zero-argument decline either: a receiver IS an entry state, so
        // `f($b->get())` binds where `f(g())` cannot.
        ArgValue::MethodCall { callee, args, named } => project_method_summary(
            cx, folder, callee, args, named, env, store, None, None, poisoned, span_start,
            descent, out,
        )?,
        _ => return None,
    };
    let sv = summary.value?;
    let Fact::Singleton(v) = &sv.fact else { return None };
    Some((arg_of_val(v), sv.stratum))
}

/// Outcome of a `$fn(...)` variable call (issue #128): the return-fact summary
/// and optional declared return arms for the assignment floor.
pub(crate) struct VarCallOutcome {
    pub(crate) summary: Option<ReturnSummary>,
    pub(crate) return_arms: Option<Vec<ContractArm>>,
}

/// Handle a `$fn(...)` variable call (ADR-0033): resolve the callee variable
/// against the env. A proven closure value → argument check against the
/// closure's params + binding descent into the closure scope (capture snapshot
/// seeded); a proven `Singleton(Str)` → resolve as a function name through the
/// normal function path. An unresolved `$fn` does nothing (opaque; the effects
/// pass taints exhaustiveness separately). Returns the callee's
/// [`ReturnSummary`] when computed (issue #128), so `$x = $fn(...)` rebinds on
/// the same rungs as free functions and methods.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_var_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> VarCallOutcome {
    let empty = VarCallOutcome { summary: None, return_arms: None };
    if scope.poisoned {
        return empty;
    }
    let Some(known) = env.get(name) else { return empty };

    // 1. Proven closure value → check args + descend into the closure scope.
    if let Some(cv) = &known.closure {
        return match &cv.target {
            ClosureTarget::Scope(def_offset) => {
                let Some(callee_scope) = cx.closure_scope(*def_offset) else {
                    return empty;
                };
                // Declared return floor first — same rung free functions/methods keep
                // when named/spread refuse binding descent (issue #128 review).
                let return_arms = closure_return_arms(cx, callee_scope);
                // Argument type check at the `$fn(...)` site (mirrors the direct /
                // propagated check for named calls, which never see a variable call).
                check_callable_args(
                    cx,
                    folder,
                    scope.poisoned,
                    descent.is_some(),
                    &callee_scope.params,
                    "closure",
                    call,
                    env,
                    out,
                );
                // Named/spread: no positional binding map — keep arms, skip summary.
                if !call.positional_only {
                    return VarCallOutcome { summary: None, return_arms };
                }
                let display = format!("closure (defined on line {})", cv.def_line);
                let arg_values: Vec<&ArgValue> = call.args.iter().map(|a| &a.value).collect();
                let summary = descend(
                    cx,
                    folder,
                    &callee_scope.params,
                    cx.cur,
                    callee_scope,
                    &format!("closure@{def_offset}"),
                    &display,
                    None,
                    None,
                    &arg_values,
                    call.span.start,
                    &cv.captures,
                    env,
                    store,
                    scope.poisoned,
                    descent,
                    out,
                );
                VarCallOutcome { summary, return_arms }
            }
            ClosureTarget::Named(nameref) => {
                dispatch_named_callable(
                    cx, folder, scope.poisoned, nameref, call, env, store, descent, out,
                )
            }
        };
    }

    // 2. Proven string value → resolve as a function name (`$fn = 'strtolower';`).
    // Named/spread still route through `dispatch_named_callable` so the declared
    // return floor is kept when binding refuses (issue #128 review) — same rung as
    // local closures and first-class callables.
    // A name lane: a byte string names no PHP function, so it resolves to nothing
    // rather than to a lossy spelling (ADR-0080 §2.5).
    if let Some(ArgValue::Str(s)) = known.singleton()
        && let Some(s) = s.as_str()
    {
        let nameref =
            NameRef { raw: s.to_owned(), kind: RefKind::Unqualified, offset: call.span.start };
        return dispatch_named_callable(
            cx, folder, scope.poisoned, &nameref, call, env, store, descent, out,
        );
    }
    empty
}

/// Declared-return contract arms of a closure scope (issue #128): the native
/// `: R` member list refined by the scope's adopted-docblock `@return` — the
/// same [`refine_contract_arms`] composition (and so the same native-vs-phpdoc
/// precedence: phpdoc refines *within* the runtime-enforced native envelope,
/// never past it) as [`fn_return_arms`]. Class arms in the `@return` resolve in
/// the closure's own file/namespace, at its definition offset.
fn closure_return_arms(cx: &Cx, callee_scope: &Scope) -> Option<Vec<ContractArm>> {
    let native: Vec<ContractTy> =
        callee_scope.ret_ty.as_ref().map(native_arms).unwrap_or_default();
    let off = match &callee_scope.owner {
        ScopeOwner::Closure { def_offset } => *def_offset,
        _ => 0,
    };
    let phpdoc =
        cx.envelopes_of(callee_scope.docblock.as_deref(), cx.cur, off).and_then(|e| e.ret);
    let resolve = |n: &str| {
        cx.resolve_pclass(cx.cur, off, n).trim_start_matches('\\').to_ascii_lowercase()
    };
    refine_contract_arms(&native, phpdoc.as_ref(), &resolve)
}

/// Dispatch a `$fn(...)` call whose target is a named free function (a first-class
/// callable or a proven string callable, ADR-0033): argument type check against the
/// resolved function's params, then normal binding descent.
#[allow(clippy::too_many_arguments)]
fn dispatch_named_callable(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    nameref: &NameRef,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> VarCallOutcome {
    let synth = synth_function_call(call, nameref);
    let return_arms = cx.resolve_user_fn_any(&synth).and_then(|site| fn_return_arms(cx, site));
    if let Some(site) = cx.resolve_user_fn(&synth) {
        let decl = cx.fn_decl(site);
        check_callable_args(
            cx, folder, poisoned, descent.is_some(), &decl.params, &decl.name, call, env, out,
        );
    }
    let summary = try_descend_function(cx, folder, &synth, env, store, poisoned, descent, out);
    VarCallOutcome { summary, return_arms }
}

/// A synthetic named-function [`CallExpr`] from a `$fn(...)` variable call and a
/// resolved function reference, so the normal function-resolution/descent path can
/// consume it (ADR-0033 first-class-callable / string-callable dispatch).
fn synth_function_call(call: &CallExpr, nameref: &NameRef) -> CallExpr {
    CallExpr {
        callee: Some(nameref.raw.clone()),
        callee_ref: Some(nameref.clone()),
        receiver: Callee::Function(nameref.raw.clone()),
        args: call.args.clone(),
        named_args: call.named_args.clone(),
        has_spread: call.has_spread,
        positional_only: call.positional_only,
        span: call.span,
        arg_conds: call.arg_conds.clone(),
    }
}

/// Argument type check for a `$fn(...)` call at the call site (ADR-0033): each
/// proven argument (literal, or resolved `$var`/fold) is checked against the
/// callable's corresponding native param type, firing `type.argument-mismatch` on
/// a proven coercive TypeError — the variable-call analogue of the direct /
/// propagated check (which never see a variable call). `display` names the callee
/// in the message (`"closure"` or the resolved function name).
#[allow(clippy::too_many_arguments)]
fn check_callable_args(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    in_descent: bool,
    params: &[Param],
    display: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }
        let Some(ty) = param.ty.as_ref() else { continue };
        // Resolve the argument to a proven value (literal directly; `$var`/fold via
        // the env). Provenance names the variable/fold source where applicable.
        // Stratum rides with the resolution (issue #127 review) — never a syntactic
        // re-read that would launder an Asserted fold into Verified.
        let resolved: Option<(ArgValue, Option<String>, Stratum)> = match &arg.value {
            v if v.is_literal() => Some((v.clone(), None, Stratum::Verified)),
            ArgValue::Var(vn) if !poisoned => env.get(vn).and_then(|k| {
                let v = k.singleton()?;
                let prov = match &k.bound {
                    Some(b) => format!("from ${vn}, {b}"),
                    None => format!("from ${vn}, assigned at line {}", k.line),
                };
                Some((v, Some(prov), k.stratum))
            }),
            ArgValue::Call(cn, cargs) => cx
                .try_fold_emit(cn, cargs, env, poisoned, folder, out)
                .map(|(lit, prov, strat)| (lit, Some(prov), strat)),
            // A proven object (`new` / enum case) or resolved class constant
            // (ADR-0043 stage 3); env-free, `self`/`parent` unavailable here.
            _ => cx
                .resolve_static_value(&arg.value, None)
                .map(|v| (v, None, Stratum::Verified)),
        };
        // Proof-layer consumption rule (ADR-0052 §5): silent on an `Asserted`
        // premise (no store here — a prop-fetch arg never resolves to a fire).
        if let Some((value, provenance, strat)) = resolved
            && strat == Stratum::Verified
            && is_type_error(cx, ty, &value)
            && !implicit_null_accepted(param, &value)
            && !object_world_guard_blind(in_descent, ty, &value)
        {
            out.push(cx.diagnostic(
                arg.span.start,
                &value,
                provenance.as_deref(),
                display,
                &param.name,
                ty,
            ));
        }
    }
}

/// What a descent's `$this` is seeded from — the receiver being the zeroth argument
/// (ADR-0086 §3), and a constructor's own allocation being the degenerate case of
/// that (ADR-0057 C1). Every variant crosses through [`copy_for_descent`]'s field
/// table; what differs is where the object comes from, whether an argument can be
/// an alias of it, and (for a constructor) whether the exits snapshot `$this`.
#[derive(Clone, Copy)]
pub(crate) enum ThisSeed<'a> {
    /// The caller variable naming an exact `Receiver::Var`'s object
    /// ([`CallTarget::receiver_var`], which states why every other receiver is
    /// `None`). Its copy is **shared** with any argument naming the same caller
    /// allocation, so `$b->m($b)` binds `$this` and the parameter to one object.
    ///
    /// [`CallTarget::receiver_var`]: crate::dispatch::CallTarget::receiver_var
    ReceiverVar(&'a str),
    /// The object a **receiver-position** `new` mints — `(new C(1))->m()`, whose
    /// constructor this site has already walked (ADR-0057 C7's third seam, issue
    /// #386). Fresh, so no argument can alias it.
    ReceiverNew(&'a HeapObj),
    /// The fresh allocation a `new C(args)` site is minting, for the **constructor**
    /// descent itself (ADR-0057 C1) — the ONE copy that is not pre-escaped, a `new`
    /// site having no caller-side object for the call to escape, and the one descent
    /// whose exits snapshot `$this` instead of a returned value (C2). The bit says
    /// what got OUT, not what may be written: the walk still sweeps its own `$this`
    /// at every call that could reach the allocation without naming it (C5).
    Ctor(&'a HeapObj),
    /// The walk's **own** `$this`, for a call that runs with the same one —
    /// `$this->m(…)`, `self::m(…)`, `parent::m(…)`, `static::m(…)`, and the by-name
    /// `Foo::m(…)` of issue #417 (ADR-0057's 2026-08-17 amendment, D1). The source is
    /// `refs["this"]` in the caller's own store, so an argument that is `$this` shares
    /// the copy exactly as `$b->m($b)` shares a receiver's.
    ///
    /// [`copy_for_descent`]'s field table applies with **`escaped` crossing
    /// verbatim** rather than forced to `true`: every other copy is pre-escaped
    /// because the call hands the caller's object over, and this call hands nothing
    /// over — the callee's `$this` IS the caller's, the same object under the same
    /// name. Inside a constructor walk that bit is `false` (C1) and the non-readonly
    /// props cross; in an ordinary method it is `true` and only the readonly props
    /// and the carries do, which is what ADR-0086 §3 already said about a
    /// `$this`-origin receiver.
    SameThis,
}

/// Interprocedural argument-binding descent into a resolved callee body. Returns the
/// callee's [`ReturnSummary`] (ADR-0057 amendment T0) when one was computed — the
/// join over its returning exits, memoized under the binding key — or `None` when no
/// descent ran (unbound, by-ref, depth-exhausted, recursive) or no summarizable exit
/// remained. The caller consumes it as the call-result value floor above the arms.
#[allow(clippy::too_many_arguments)]
pub(crate) fn descend(
    cx: &Cx,
    folder: &mut dyn Folder,
    params: &[Param],
    callee_file: usize,
    callee_scope: &Scope,
    key_name: &str,
    display_name: &str,
    body_this_exact: Option<String>,
    // What seeds the callee's `$this`, or `None` where nothing does — a free
    // function, a closure, and every receiver [`CallTarget::receiver_var`] lists as
    // proving no object.
    this_seed: Option<ThisSeed<'_>>,
    // The call's positional argument values + its span start (for the provenance
    // line). Taken apart rather than as a `&CallExpr` (issue #60): a nested call in
    // argument position exists only as an `ArgValue::Call` — no `CallExpr` is ever
    // lowered for it — and these two pieces are all the descent ever used.
    args: &[&ArgValue],
    span_start: u32,
    captures: &[(String, Fact, Stratum)],
    env: &HashMap<String, Known>,
    // The caller's heap at the call (ADR-0086 §2): an argument's object crosses into
    // the callee's store as a copy. Read-only — no callee-side write is ever visible
    // through this channel, and no caller-side name survives into the callee.
    caller_store: &Store,
    poisoned: bool,
    mut descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<ReturnSummary> {
    if callee_scope.poisoned {
        return None;
    }

    // Resolve each positional argument to a literal and try to bind it (using
    // the *caller's* env, strict mode, and folding). Each binding carries the arg's
    // trust stratum (ADR-0052 §5): the seeded callee param inherits it, so an
    // `Asserted` argument narrows into the descent without laundering to `Verified`.
    let mut bound: Vec<(String, ArgValue, Stratum)> = Vec::new();
    let mut render_args: Vec<ArgValue> = Vec::new();
    // Call-site heap entry (ADR-0086 §2): the callee's pre-populated store, the
    // per-parameter key renderings of what it holds, and the caller-allocation →
    // callee-allocation map that keeps **one copy per distinct caller object**, so
    // `f($b, $b)` binds both parameters to one callee object and the aliasing
    // structure among the arguments survives the crossing.
    let mut seed_store = Store::default();
    let mut seed_keys: Vec<(String, String)> = Vec::new();
    let mut seeded: HashMap<AllocId, AllocId> = HashMap::new();
    // The receiver is the **zeroth argument** (ADR-0086 §3), so it is seeded first and
    // through the same two helpers: `copy_for_descent` for the field table, and the
    // `seeded` map above for the aliasing rule — which is what makes `$b->m($b)` bind
    // `$this` and the parameter to ONE callee object rather than two copies that would
    // convict correct code. `analyze_scope`'s `$this` seed then finds `this` bound and
    // leaves it alone; every receiver that seeds nothing here still goes through
    // `seed_this_object` there, exactly as before.
    //
    // A **constructor** descent seeds `$this` from the fresh allocation the `new`
    // site is minting (ADR-0057 C1) — under the same field table, with `escaped`
    // decided the other way: this is the ONE copy that is not pre-escaped, because a
    // `new` site has no caller-side object for the call to escape. The allocation is
    // minted for this expression and no name outside the constructor's own `$this`
    // refers to it, so `false` is the honest bit, and it is what lets the caller's
    // `$b = new B(1)` survive a later unrelated unknown call. It says what got OUT,
    // not what may be written: the walk still sweeps its own `$this` at every call
    // that could reach the allocation without naming it (C5, `escape_and_sweep_calls`).
    //
    // A **same-`$this`** call seeds from the walk's own `$this` (the 2026-08-17
    // amendment, D1), with `escaped` crossing verbatim for the mirror-image reason:
    // the call hands nothing over, so the bit is what it was an instant earlier.
    let seeded_this: Option<AllocId> = this_seed.filter(|_| !poisoned).and_then(|seed| {
        // The caller allocation this copy stands for, for the aliasing rule — `None`
        // where there is none to alias: a `new` in receiver position and the fresh
        // allocation a constructor is handed are unique by construction, exactly as a
        // direct `new` in argument position is.
        let (obj, caller_id) = match seed {
            ThisSeed::Ctor(fresh) => {
                let mut copy = copy_for_descent(fresh);
                copy.escaped = false;
                (copy, None)
            }
            ThisSeed::ReceiverNew(fresh) => (copy_for_descent(fresh), None),
            ThisSeed::ReceiverVar(v) => {
                let caller_id = caller_store.id_of(v)?;
                (copy_for_descent(caller_store.heap.get(&caller_id)?), Some(caller_id))
            }
            ThisSeed::SameThis => {
                let caller_id = caller_store.id_of("this")?;
                let src = caller_store.heap.get(&caller_id)?;
                let mut copy = copy_for_descent(src);
                copy.escaped = src.escaped;
                (copy, Some(caller_id))
            }
        };
        // The copy IS the receiver the dispatch resolved through, so its exactness
        // cannot disagree with the one the callee walks under: every seeding receiver
        // is one `resolve_call_target` proved an exact class for, and it passed that
        // same class on as `this_exact` (a constructor's is the class it mints).
        //
        // A same-`$this` seed is the exception, and it is not an exactness leak (D1):
        // the copy's bit is a fact about the CALLER's allocation, while
        // `body_this_exact` is whatever the target's own resolution proved — `None`
        // for `parent::`, `self::` and the by-name spelling, which resolve without an
        // exact receiver. The callee walks under the weaker `$this` dispatch its
        // resolution named while holding the stronger object the caller proved, which
        // is the sound pairing; nothing is promoted either way.
        debug_assert!(
            matches!(seed, ThisSeed::SameThis)
                || (obj.class_exact && body_this_exact.as_deref() == Some(obj.class.as_str())),
            "a seeded `$this` must be the exact receiver `body_this_exact` names",
        );
        let id = seed_store.heap.len() as AllocId;
        seed_store.heap.insert(id, obj);
        seed_store.refs.insert("this".to_owned(), id);
        if let Some(caller_id) = caller_id {
            seeded.insert(caller_id, id);
        }
        Some(id)
    });
    for (i, arg_value) in args.iter().enumerate() {
        let Some(param) = params.get(i) else { break };
        if param.variadic {
            break;
        }
        // The object leg first: an argument denoting a heap object never resolves to
        // a literal (objects have no value-domain carrier — ADR-0035/0038), so the
        // two legs are disjoint by construction and this ordering costs nothing.
        if let Some(obj) = argument_heap_object(
            cx,
            folder,
            arg_value,
            env,
            caller_store,
            poisoned,
            span_start,
            descent.as_deref_mut(),
            &mut *out,
        ) {
            if param.by_ref {
                return None;
            }
            // A direct `new` has no caller allocation — it is unique by construction.
            let caller_id = match arg_value {
                ArgValue::Var(name) => caller_store.id_of(name),
                _ => None,
            };
            let id = match caller_id.and_then(|c| seeded.get(&c).copied()) {
                Some(id) => id,
                None => {
                    let id = seed_store.heap.len() as AllocId;
                    seed_store.heap.insert(id, copy_for_descent(&obj));
                    if let Some(c) = caller_id {
                        seeded.insert(c, id);
                    }
                    id
                }
            };
            seed_store.refs.insert(param.name.clone(), id);
            seed_keys.push((
                param.name.clone(),
                object_binding_key(&seed_store.heap[&id]),
            ));
            render_args.push((*arg_value).clone());
            continue;
        }
        // Direct resolution first; when it declines and the argument is itself a
        // project call, the T0 machinery answers for its own argument position
        // (issue #60): `f(g(1))` binds `g(1)`'s Singleton summary. The current
        // descent is threaded (reborrowed) so the on-stack recursion guard and
        // `MAX_BINDING_DEPTH` bound the nested resolution as they bound a
        // statement-level chain, and the nested walk emits through the same `out`.
        // Threading it into literal resolution also lets a foldable builtin whose
        // arg is a project call (`strtoupper(g($x))` inside a callee) reuse the
        // on-stack guard (issue #127); project-call-only args fall through to
        // `nested_call_singleton` with the real `out`.
        let (value, strat) = match cx.resolve_literal_under(
            arg_value,
            env,
            poisoned,
            folder,
            descent.as_deref_mut(),
            Some(&mut *out),
        ) {
            // Stratum comes from the fold/env path (includes nested project-call
            // Asserted summaries — issue #127 review). Nested project-call descents
            // for fold args emit through `out` so binding-specific findings are not
            // discarded.
            Some(vs) => vs,
            None => {
                let Some(vs) = nested_call_singleton(
                    cx,
                    folder,
                    arg_value,
                    env,
                    caller_store,
                    poisoned,
                    span_start,
                    descent.as_deref_mut(),
                    out,
                ) else {
                    continue;
                };
                vs
            }
        };
        render_args.push(value.clone());
        if param.by_ref {
            return None;
        }
        let Some(ty) = param.ty.as_ref() else {
            bound.push((param.name.clone(), value, strat));
            continue;
        };
        let coerced = coerce_into_param(cx, ty, &value)?;
        bound.push((param.name.clone(), coerced, strat));
    }

    // A closure with captures descends even with no bound args (the capture
    // snapshot drives the body); a plain function needs at least one bound arg.
    // Zero-argument factories do NOT descend in T0 (ADR-0057 §3 / A5, deferred to
    // T2's emission-suppressed summary-only walk) — they take the arm floor.
    //
    // A **seeded object counts as a binding** (ADR-0086 §2): an object-only argument
    // list carries real entry state now, so `h(new Box(1))` walks `h` where it used
    // to return here. The memo and the emission dedupe then govern that walk exactly
    // as they govern a value binding's.
    //
    // A seeded `$this` counts the same way (§3, the receiver being the zeroth
    // argument): `$b->get()` takes no arguments and still enters with the receiver's
    // proven props, which is exactly what makes it agree with `get($b)`.
    if bound.is_empty() && captures.is_empty() && seed_keys.is_empty() && seeded_this.is_none() {
        return None;
    }

    // The binding key incorporates the captured snapshot so two calls of the same
    // closure with different snapshots memoize distinctly (adversarial #1). Each
    // binding's stratum is part of the key (issue #128 review): a Verified summary
    // for `$f('hi')` must not replay as Verified when the next call is `$f($u)` with
    // `$u` Asserted Singleton('hi') — otherwise the Asserted claim launders into a
    // proof premise through the memo.
    //
    // ADR-0075 §2.1: a method body reached through `resolve_exact` is keyed by
    // declaring FQN (`Base::m`), but two exact receivers (`Sub1`, `Sub2`) can
    // inherit the same body while `$this->hook()` inside it dispatches differently.
    // When `body_this_exact` is `Some`, a `this:` pseudo-binding carries that
    // receiver so the memo never replays one receiver's result for the other.
    // Guarded resolutions pass `None` — a final/private body's inner dispatch is a
    // pure function of its declaring class.
    let mut key_binding: Vec<(String, ArgValue, Stratum)> = bound
        .iter()
        .map(|(n, v, s)| (n.clone(), v.clone(), *s))
        .collect();
    for (name, fact, strat) in captures {
        key_binding.push((format!("use:{name}"), arg_of_fact_key(fact), *strat));
    }
    // ADR-0086 §2: a seeded object names its whole entry state in the key, under the
    // same pseudo-binding spelling captures and `this:` already use. Nothing crosses
    // that this rendering does not state, so the memo stays a pure function of the
    // key (ADR-0048 §2) and a summary is never replayed — nor an emission suppressed
    // — for an object the callee would have seen differently. `Verified`: what the
    // rendering names is the runtime shape of the object, and each prop's own
    // stratum travels inside it.
    for (name, render) in &seed_keys {
        key_binding.push((
            format!("obj:{name}"),
            ArgValue::Str(PhpStr::from(render.clone())),
            Stratum::Verified,
        ));
    }
    // The `this:` pseudo-binding, in its two spellings. A `$this` seeded from the
    // receiver's copy (ADR-0086 §3) names its **whole entry state**, exactly as
    // `obj:{param}` does for an argument's: the class string alone would let
    // `$b1->m()` answer for `$b2->m()` on two boxes holding different values, replaying
    // one receiver's summary and suppressing the other's emission (ADR-0075 §2.1).
    // A **constructor** descent's seeded `$this` renders the same way and for the same
    // reason (ADR-0057 C8): `new C(1)` and `new C(2)` reach one body with different
    // entry states, and the class alone — all a constructor's key carried while it
    // proved "an identity and no state" — would replay one's summary for the other.
    // Where nothing was seeded the spelling is the exact class FQN, unchanged since
    // ADR-0075 §2.1 — a guarded resolution proves an identity and no state, and that
    // is all the key has ever had to distinguish there. (A `Receiver::New` used to be
    // in that company; since issue #386 it carries its constructor's arguments, mints
    // its object here and renders like every other seeded receiver.)
    match (&body_this_exact, seeded_this) {
        // Exact receiver is a runtime-proven identity — Verified either way, and each
        // seeded prop's own stratum travels inside the rendering.
        (_, Some(id)) => key_binding.push((
            "this:".to_owned(),
            ArgValue::Str(PhpStr::from(object_binding_key(&seed_store.heap[&id]))),
            Stratum::Verified,
        )),
        (Some(exact), None) => key_binding.push((
            "this:".to_owned(),
            ArgValue::Str(PhpStr::from(exact.clone())),
            Stratum::Verified,
        )),
        (None, None) => {}
    }
    key_binding.sort_by(|a, b| a.0.cmp(&b.0));
    let key: BindingKey = (key_name.to_owned(), key_binding);

    // Provenance names the *first* binding site; a nested descent inherits it.
    // When the call crosses files, the site names the caller's file.
    let cross = cx.cur != callee_file;
    let new_provenance;
    let (provenance, next_depth): (&str, usize) = match &descent {
        Some(d) => (d.provenance, d.depth + 1),
        None => {
            let line = cx.tree().position(span_start).line;
            let render = render_call(display_name, &render_args);
            new_provenance = if cross {
                format!("bound at {render} call at {} line {line}", cx.path())
            } else {
                format!("bound at {render} call on line {line}")
            };
            (&new_provenance, 1)
        }
    };

    // Depth exhaustion widens, never lies (ADR-0057 §3 / A5): no descent ⇒ no
    // summary ⇒ the caller keeps the arm floor.
    if next_depth > MAX_BINDING_DEPTH {
        return None;
    }

    // Bound params are always resolved literals/arrays, so `singleton_fact`
    // succeeds; a value that somehow fails conversion is simply left unbound
    // (the callee param stays unknown — sound).
    let mut bound_env: HashMap<String, Known> = bound
        .into_iter()
        .filter_map(|(name, value, strat)| {
            singleton_fact(&value, cx.php_minor)
                .map(|fact| (name, Known::value_strat(fact, 0, Some(provenance.to_owned()), strat)))
        })
        .collect();
    // Closure captures (ADR-0033): the by-value snapshot seeds the initial env,
    // UNDER the param bindings (a param of the same name shadows a capture, PHP
    // semantics — `use ($x)` is ignored if `$x` is also a parameter). The capture's
    // snapshotted stratum is restored so an Asserted claim does not launder to
    // Verified in the summary rebound to the caller (issue #128 review).
    for (name, fact, strat) in captures {
        bound_env.entry(name.clone()).or_insert_with(|| {
            Known::value_strat(fact.clone(), 0, Some(provenance.to_owned()), *strat)
        });
    }

    let child_cx = cx.at(callee_file);
    match descent {
        Some(d) => {
            // Recursion (ADR-0057 §3 / A5): the key is already on the descent stack;
            // the walk is suppressed and no summary exists yet — `None` (arm floor).
            // The enclosing exit degrades to the floor via A3 rather than dying.
            if d.stack.contains(&key) {
                return None;
            }
            // Memo hit: REPLAY the cached summary (a value, not a suppression bit) —
            // no re-walk, so no re-emitted findings. Legitimate caching (§3): the
            // summary is a pure function of the key's entry state.
            if let Some(cached) = d.memo.get(&key) {
                return cached.clone();
            }
            d.stack.push(key.clone());
            let child = Descent { provenance, depth: next_depth, stack: d.stack, memo: d.memo };
            let mut exits: Vec<ExitContribution> = Vec::new();
            let mut this_exits: Vec<ExitContribution> = Vec::new();
            analyze_scope(
                &child_cx,
                folder,
                callee_scope,
                bound_env,
                seed_store,
                body_this_exact,
                Some(child),
                None,
                None,
                None,
                Some(&mut exits),
                seeded_this.map(|_| &mut this_exits),
                out,
            );
            d.stack.pop();
            let summary = join_summary(&child_cx, callee_scope, &exits, &this_exits);
            d.memo.insert(key, summary.clone());
            summary
        }
        None => {
            let mut stack: Vec<BindingKey> = vec![key.clone()];
            let mut memo: HashMap<BindingKey, Option<ReturnSummary>> = HashMap::new();
            let child = Descent { provenance, depth: next_depth, stack: &mut stack, memo: &mut memo };
            let mut exits: Vec<ExitContribution> = Vec::new();
            let mut this_exits: Vec<ExitContribution> = Vec::new();
            analyze_scope(
                &child_cx,
                folder,
                callee_scope,
                bound_env,
                seed_store,
                body_this_exact,
                Some(child),
                None,
                None,
                None,
                Some(&mut exits),
                seeded_this.map(|_| &mut this_exits),
                out,
            );
            join_summary(&child_cx, callee_scope, &exits, &this_exits)
        }
    }
}

/// Build the [`ReturnSummary`] from a callee's collected returning-exit contributions:
/// the value component (T0 — join the value facts (A1), a factless exit contributing
/// the declared value floor (A3), the stratum `min` over exits (A4)) and, **beside**
/// it and never inside it, the heap component (T1 — [`join_heap_exits`], §2.4).
///
/// The two are independent (T1 amendment B3): each refusal below is the value
/// component's own, so a callee whose value summary dies for want of a representable
/// floor — which is EVERY object-returning factory — still crosses its allocation.
///
/// The **`$this`** component (the 2026-08-17 amendment, D3) is a third, joined from
/// its own exit list by the same `join_heap_exits` and independent of both: a
/// constructor reads it where its `new` site mints the object, and every other
/// seeded walk reads it as the copy-back into the caller's own.
///
/// `None` only when no component survived.
fn join_summary(
    cx: &Cx,
    callee_scope: &Scope,
    exits: &[ExitContribution],
    this_exits: &[ExitContribution],
) -> Option<ReturnSummary> {
    // Generators: the call result is a Generator, not the value of `return` after
    // `yield` (ADR-0057 §5) — refuse EVERY component. The one refusal the heap
    // component shares, and for the heap the reason is even plainer: the returned
    // allocation is not what the call evaluates to. For the `$this` component it is
    // plainer still — the body does not run at the call at all, so there is no exit
    // state to copy back (D5).
    if callee_scope.is_generator {
        return None;
    }
    let heap = join_heap_exits(exits);
    let this = join_heap_exits(this_exits);
    let value = join_value_component(cx, callee_scope, exits);
    (value.is_some() || heap.is_some() || this.is_some())
        .then_some(ReturnSummary { value, heap, this })
}

/// The value half of [`join_summary`] (ADR-0057 amendment T0), unchanged in content
/// by T1 — lifted into its own function only so its several refusals stop being
/// refusals of the whole summary.
fn join_value_component(
    cx: &Cx,
    callee_scope: &Scope,
    exits: &[ExitContribution],
) -> Option<SummaryValue> {
    let ret = cx.scope_return(callee_scope).map(|(ty, _)| ty);
    // A written return hint Steins cannot lower (`: object`, `: array`, `: void`,
    // `: never`, …) leaves `scope_return` as `None`, so the A2 native-oracle arms
    // are empty and `native_violates` cannot drop boundary TypeErrors (`return
    // null` under `: object`). Refuse rather than rebind an uncheckable exit as a
    // Singleton premise (ADR-0075 review).
    //
    // `: mixed` is exempt (issue #364): it is the TOTAL envelope, so the empty
    // oracle has nothing to drop — no value violates `mixed`, and no conversion
    // happens at the boundary — and the exit that crosses is the exit the body
    // proved. It reads as NO hint here and only here: `floor` stays `None` (a total
    // envelope has no single-base value floor), so a factless exit still floors the
    // whole summary out (A3), and everything outside this function keeps treating
    // it as the written hint it is.
    if ret.is_none()
        && callee_scope.ret_hint.is_some_and(|h| h.kind != RetHintKind::Mixed)
    {
        return None;
    }
    let floor = ret.and_then(native_value_floor);
    // The declared return type is a CONVERSION boundary, not just an envelope
    // (the #48 family, return edition): PHP hands the caller what the boundary
    // converts, so `return 1` under `: float` crosses as `1.0`, never the callee's
    // raw int. A2 already dropped the *violating* exits at collection; this
    // converts the admitted ones, and an admitted-but-unconvertible fact degrades
    // to the declared floor (A3 — wider, never wrong).
    let coerced: Vec<ExitContribution> = exits
        .iter()
        .map(|e| match (e, ret) {
            (ExitContribution::Fact(f, s), Some(ty)) => {
                match coerce_fact_to_native(ty, f.clone()) {
                    Some(cf) => ExitContribution::Fact(cf, *s),
                    None => ExitContribution::Floor,
                }
            }
            (ExitContribution::Fact(f, s), None) => ExitContribution::Fact(f.clone(), *s),
            // An object exit is a `Floor` on this side and always has been (T1's
            // `Heap` variant only names what the OTHER side reads): a value floor is
            // the widest thing the value domain can say about it, and for an object
            // return that floor is `None`, which is what ends the value summary.
            (ExitContribution::Floor | ExitContribution::Heap(_), _) => ExitContribution::Floor,
        })
        .collect();
    let (fact, stratum) = join_exits(&coerced, floor.as_ref())?;
    Some(SummaryValue { fact, stratum })
}

/// Join a callee's object-returning exits into the heap component (ADR-0057 §2.4,
/// per field in the T1 amendment's B3 table). Written beside the value join and never
/// inside it: the two components live and die independently.
///
/// `None` — no heap summary, the caller keeps the arm floor — whenever
///
/// * there are no exits at all, or
/// * **any** exit is not an allocation (§2.5): a scalar, `null`, an unresolved
///   expression, an untyped fall-through, or the declared floor an `Opaque`
///   `may_return` subtree contributes for the exits it hides. There is no heap shape
///   that truthfully covers such a path, so a partial summary would be a partial lie;
/// * the classes disagree (§2.4): a joined "one of Foo or Bar" is the `Member`-fact
///   shape, not the heap's, and the declared-return arms already carry that floor.
///
/// The declared return type is **not** consulted anywhere here (§2.6): a conflict
/// between the walk's proof and the declaration is the callee's own return-mismatch
/// finding, and claims do not edit proofs. The T0 amendment's A2 native oracle is the
/// value component's alone (T1 amendment B4).
fn join_heap_exits(exits: &[ExitContribution]) -> Option<HeapSummary> {
    let mut objs = Vec::with_capacity(exits.len());
    for e in exits {
        match e {
            ExitContribution::Heap(o) => objs.push(o.as_ref()),
            // Any non-allocation exit kills the summary (§2.5).
            ExitContribution::Fact(..) | ExitContribution::Floor => return None,
        }
    }
    let (first, rest) = objs.split_first()?;
    // Class agreement decides everything else: exactness is only meaningful under it,
    // and a prop of a `Foo` is not a prop of a `Bar`.
    if rest.iter().any(|o| o.class != first.class) {
        return None;
    }
    let mut joined = HeapObj::new(first.class.clone());
    // Copied, never promoted (§6.4 / A1): exact only where every path was.
    joined.class_exact = first.class_exact && rest.iter().all(|o| o.class_exact);
    // Escaped-before-return ORs (§2.4): a leak on any path means the caller must
    // rebind pre-escaped and sweep it like an object it leaked itself.
    joined.escaped = first.escaped || rest.iter().any(|o| o.escaped);
    // readonly INTERSECTS. The set is a function of the class, so disagreement is a
    // corner; where it happens the smaller set is the sound one, readonly being a
    // sweep-IMMUNITY claim (B3).
    joined.readonly =
        first.readonly.iter().filter(|n| rest.iter().all(|o| o.readonly.contains(*n))).cloned().collect();
    // ro_written likewise: a write proven on every path. Recording a one-path write
    // would let the caller's first assignment read as a `readonly.reassigned` second.
    joined.ro_written =
        first.ro_written.iter().filter(|n| rest.iter().all(|o| o.ro_written.contains(*n))).cloned().collect();
    // Carries survive only where every path carries them identically (B2) — the
    // `join_stores` intersection rule, order-independent (ADR-0048 §4).
    joined.targs =
        first.targs.iter().filter(|c| rest.iter().all(|o| o.targs.contains(c))).cloned().collect();
    // Props: present on EVERY object-returning path, joined by the existing
    // value-domain join at `min` stratum (ADR-0052 amendment 1 — a Verified arm joined
    // with an Asserted arm yields Asserted). An unjoinable pair drops the prop.
    for (name, p0) in &first.props {
        let mut fact = p0.fact.clone();
        let mut stratum = p0.stratum;
        let mut ok = true;
        for o in rest {
            match o.props.get(name) {
                Some(p) => match fact.join(&p.fact) {
                    Some(j) => {
                        fact = j;
                        stratum = stratum.min(p.stratum);
                    }
                    None => {
                        ok = false;
                        break;
                    }
                },
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            joined.props.insert(name.clone(), PropFact { fact, stratum });
        }
    }
    Some(HeapSummary { obj: joined })
}

/// Join a callee's returning-exit contributions into the value-domain summary fact
/// (ADR-0057 A1/A3/A4). Each `Fact` exit joins by the existing value-domain join;
/// each `Floor` exit contributes the declared value floor (a `None` floor — an object
/// or mixed-base return — kills the whole value summary, there being no representable
/// degraded top). Stratum is `min` over exits (N2). An empty exit set (no returning
/// exit, or every exit dropped by A2) or an unrepresentable join (mixed bases) yields
/// `None` — arm floor.
fn join_exits(exits: &[ExitContribution], floor: Option<&Fact>) -> Option<(Fact, Stratum)> {
    if exits.is_empty() {
        return None;
    }
    let mut acc: Option<Fact> = None;
    let mut stratum = Stratum::Verified;
    for e in exits {
        let (fact, s) = match e {
            ExitContribution::Fact(f, s) => (f.clone(), *s),
            // `Heap` never reaches here — `join_value_component` maps it to `Floor`
            // before this join runs — but it degrades the same way if it ever did.
            ExitContribution::Floor | ExitContribution::Heap(_) => {
                (floor?.clone(), Stratum::Verified)
            }
        };
        stratum = stratum.min(s);
        acc = Some(match acc {
            None => fact,
            Some(a) => a.join(&fact)?,
        });
    }
    Some((acc?, stratum))
}

/// The declared return type's value-domain FLOOR as a single [`Fact`] — the sound top
/// within the envelope a factless exit contributes (ADR-0057 A3). Representable only
/// when every native member shares ONE scalar base (`int`, `?int`); a union of bases,
/// an object, or a bool-literal return has no single-base value floor (`None`).
fn native_value_floor(ty: &NativeType) -> Option<Fact> {
    let mut base: Option<Base> = None;
    for m in &ty.members {
        let b = match m {
            TypeMember::Scalar(ScalarType::Int) => Base::Int,
            TypeMember::Scalar(ScalarType::Float) => Base::Float,
            TypeMember::Scalar(ScalarType::String) => Base::String,
            TypeMember::Scalar(ScalarType::Bool) => Base::Bool,
            _ => return None,
        };
        match base {
            None => base = Some(b),
            Some(x) if x == b => {}
            Some(_) => return None,
        }
    }
    base.map(|base| Fact::General { base, nullable: ty.nullable })
}

/// The returned expression's best value-domain fact and stratum at a returning exit
/// (ADR-0057 amendment T0): a bare variable's env fact (covering the assert-narrowed
/// `positive-int` case), else a literal/const/foldable `Singleton`, else a depth-1
/// property fact. `None` for any exit the value domain cannot spell (an object, an
/// unresolved call, an array offset) — a factless exit (A3).
pub(crate) fn return_value_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    let poisoned = w.scope.poisoned;
    if let ArgValue::Var(name) = value
        && let Some(known) = env.get(name)
        && let Some(fact) = known.fact.clone()
    {
        return Some((fact, known.stratum));
    }
    // Stratum from the resolution itself (issue #127 review): a fold over an
    // Asserted project-call summary stays Asserted — never re-read from the
    // syntactic tree. Scratch sink: findings for nested fold args are owned by the
    // return-check / assignment paths that resolve with a real `out`.
    if let Some((lit, strat)) = w.cx.resolve_literal_strat(value, env, poisoned, folder)
        && let Some(fact) = singleton_fact(&lit, w.cx.php_minor)
    {
        return Some((fact, strat));
    }
    if let ArgValue::PropFetch { var, prop } = value
        && !poisoned
        && let Some(fact) = store.prop_fact(var, prop).cloned()
    {
        return Some((fact, store.prop_stratum(var, prop)));
    }
    None
}

/// The **allocation** a returning exit hands back, snapshotted at the return point
/// (ADR-0057 T1, §2's source list). `None` for every other exit — a scalar, `null`, an
/// unresolved expression — and a `None` on any path kills the whole heap summary
/// (§2.5), there being no heap shape that truthfully covers a non-allocation exit.
///
/// The three sources, and the fourth by composition:
///
/// * **`return $local`** — the object the callee's store holds, verbatim. Its origin
///   does not matter (§2.3): a local `new`, an alias, or the copy ADR-0086 seeded for
///   a parameter are all just "what the walk knows about this value", and the walk's
///   knowledge is sound however the value arrived. Exactness is whatever the object
///   carries, never promoted (§6.4).
/// * **`return $this`** — the same arm; `$this` is `refs["this"]`, pre-escaped by
///   construction and membership-only unless the receiver leg proved exactness, so a
///   fluent chain gets class continuity and no forged exactness (§6's probe).
/// * **`return new Foo(...)`** — the SAME object the assignment form binds, which is
///   what makes §4's new-vs-factory equivalence a consequence rather than a
///   coincidence: the statement's own `Callee::Construct` rung already walked the
///   constructor and left its snapshot in `ctor_heap` (ADR-0057 C7), so this arm
///   consumes it exactly as `apply_assign`'s does and never walks a second time.
///   Where the walk declined, the declaration-only object under the ADR-0086 §4
///   lexical gate stands (C6). It is minted, never stored, so it consumes no
///   allocation id.
/// * **`return g(...)` / `return $o->m(...)`** — the composition arm: the inner call's
///   own heap summary is this exit's snapshot, which is how a chained factory keeps
///   its exactness across two boundaries (§2.3's "chaining composes correctly").
#[allow(clippy::too_many_arguments)]
pub(crate) fn return_heap_object(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    stmt_summary: Option<&ReturnSummary>,
    ctor_heap: Option<&HeapSummary>,
) -> Option<HeapObj> {
    if w.scope.poisoned {
        return None;
    }
    match value {
        ArgValue::Var(name) => store.obj_of(name).cloned(),
        ArgValue::New(class_ref, args, named) => {
            let class = w.cx.class_fqn(class_ref);
            Some(match ctor_heap {
                Some(h) => {
                    debug_assert!(
                        h.obj.class == class && h.obj.class_exact,
                        "a constructor snapshot must be the exact allocation its `new` site minted",
                    );
                    h.obj.clone()
                }
                None => new_heap_object(
                    w.cx,
                    folder,
                    &class,
                    args,
                    named,
                    env,
                    store,
                    false,
                    CtorDefaults::Lexical,
                ),
            })
        }
        _ => stmt_summary.and_then(|s| s.heap.as_ref()).map(|h| h.obj.clone()),
    }
}

/// Whether a summary value fact is precise enough to bind as the call-result's value
/// (ADR-0057 A3): a `Singleton`/`OneOf`/`Refined` fact is strictly more than the
/// declared arm floor and binds; a bare `General{base}` (the degraded join) carries
/// nothing beyond the arms — the arm floor stands, observably identical to no summary.
pub(crate) fn summary_binds(fact: &Fact) -> bool {
    matches!(fact, Fact::Singleton(_) | Fact::OneOf(_) | Fact::Refined { .. })
}
