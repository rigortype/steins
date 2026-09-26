//! Interprocedural descent (Feature B, ADR-0009 budget): the entry points that
//! resolve a callee and bind its arguments — a function, a method, a value-position
//! call, a `$fn(…)` variable call — and the [`descend`] driver that walks the callee
//! under those bindings and memoizes its [`ReturnSummary`]. The two recurse into
//! each other, so they stay together here.
//!
//! Around them: `call_sweep` escapes and sweeps what a statement's calls can reach,
//! `arg_propagation` checks a propagated argument against a resolved callee,
//! `summary_join` joins the callee's exits into its summary, and `exit_facts` reads
//! what one returning exit hands back.

mod arg_propagation;
mod call_sweep;
mod exit_facts;
mod summary_join;

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::{Fact, PhpStr};
use steins_syntax::{
    ArgValue, CallExpr, Callee, NameRef, NamedArg, Param, RefKind, Scope, ScopeOwner,
};

use crate::fold::Folder;
use crate::MAX_BINDING_DEPTH;
use crate::arg_check::{
    implicit_null_accepted, is_type_error, object_world_guard_blind, render_call,
};
use crate::coerce::coerce_into_param;
use crate::cx::Cx;
use crate::dispatch::resolve_call_target;
use crate::env::{
    AllocId, BindingKey, ClosureTarget, ContractArm, Descent, ExitContribution, HeapObj, Known,
    ReturnSummary, Store, Stratum, arg_of_fact_key, arg_of_val, singleton_fact,
};
use crate::heap::{argument_heap_object, copy_for_descent, object_binding_key};
use crate::method_call::{display_of_call, nullsafe_call, receiver_new_object, this_seed_of};
use crate::project::{Diagnostic, Site};
use crate::refine::refine_contract_arms;
use crate::return_arms::{fn_return_arms, native_arms};
use crate::walk::analyze_scope;

pub(crate) use arg_propagation::{check_propagated_call, propagated_arg_value};
pub(crate) use call_sweep::{
    ThisWriteBack, apply_call_escape_and_sweep, checkable_calls, escape_and_sweep_calls,
    runs_with_same_this,
};
pub(crate) use exit_facts::{return_heap_object, return_value_fact, summary_binds};
use summary_join::join_summary;

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
#[expect(clippy::too_many_lines, reason = "cold in the #779 survey; split when next reworked")]
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
            singleton_fact(&value)
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
