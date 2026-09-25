//! What a statement's calls do to the caller's heap (ADR-0036): an object handed
//! to a call escapes, and a passed object or an unresolved call sweeps every
//! escaped object's non-readonly props ([`escape_and_sweep_calls`]). A call that
//! runs with the same `$this` sweeps `$this` as well, and a resolved descent's
//! snapshot is copied back over the sweep ([`ThisWriteBack`]).

use steins_syntax::{ArgValue, CallExpr, Callee, NamedArg, Receiver, StaticClass, StmtKind};

use crate::contract::IsA;
use crate::cx::Cx;
use crate::dispatch::resolve_call_target;
use crate::env::{HeapObj, Store};
use crate::walk::WalkCx;

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
            // An OBJECT argument escapes here; a heap resource in the same
            // position takes the closer/keeper/escape verdict of the statement's
            // `resource_call_effects` instead (ADR-0097 §2.4) — a nested position
            // below escapes both kinds alike.
            if let ArgValue::Var(name) = &arg.value
                && store.is_object(name)
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
/// [`project_method_summary`]: crate::descent::project_method_summary
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
/// [`Scope::poisoned`]: steins_syntax::Scope::poisoned
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
