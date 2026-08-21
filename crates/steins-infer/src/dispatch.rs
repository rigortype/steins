//! Class-world method resolution (ADR-0001 sound dispatch), project-wide: resolving
//! a call target along the inheritance chain, exact / guarded / static-named
//! resolution, and the private-visibility block.

use std::collections::HashSet;

use steins_syntax::{Callee, ClassDecl, MethodDecl, Receiver, StaticClass, Visibility};

use crate::contract::GenericCarry;
use crate::cx::Cx;
use crate::env::Store;
use crate::inaccessible::private_invisible;

// ---------------------------------------------------------------------------
// Class-world method resolution (ADR-0001 sound dispatch), project-wide.
// ---------------------------------------------------------------------------

/// A method resolved through a project inheritance chain.
pub(crate) struct ResolvedMethod<'a> {
    pub(crate) method: &'a MethodDecl,
    pub(crate) declaring_class: &'a ClassDecl,
    class_file: usize,
}

/// The outcome of walking a class's inheritance chain for a method.
pub(crate) enum Resolution<'a> {
    Found(ResolvedMethod<'a>),
    NotFoundChainComplete,
    Unknown,
}

/// Walk `start_fqn`'s project inheritance chain for a concrete `method`.
pub(crate) fn resolve_in_chain<'a>(cx: &Cx<'a>, start_fqn: &str, method: &str) -> Resolution<'a> {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Resolution::Unknown;
        }
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return Resolution::Unknown; // chain leaves the project
        };
        if cd.uses_traits {
            return Resolution::Unknown;
        }
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
            return if m.is_abstract {
                Resolution::Unknown
            } else {
                Resolution::Found(ResolvedMethod { method: m, declaring_class: cd, class_file: cfile })
            };
        }
        match &cd.parent {
            None => return Resolution::NotFoundChainComplete,
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
}

/// A resolved call target.
pub(crate) struct CallTarget<'a> {
    pub(crate) method: &'a MethodDecl,
    pub(crate) declaring_class: &'a ClassDecl,
    pub(crate) class_file: usize,
    pub(crate) this_exact: Option<String>,
    /// The class-level generic carries the **receiver object** holds (ADR-0032's
    /// 2026-08-15 amendment, issue #362) — what a `@return template-type<T, …>` on
    /// the target reads `T` out of.
    ///
    /// Filled by the exact `Receiver::Var` arm, which is the one arm with a heap
    /// object in hand at resolution; for a `Receiver::New`, **after** resolution, by
    /// the caller that mints it (issue #386); and by the **non-exact**
    /// `Receiver::Var` arm with that object's *declared* carries only (issue #388),
    /// a `@param Helper<Model> $h` saying as much about a descendant of `Helper` as
    /// about a `Helper`. **Empty everywhere else**, and each emptiness is a stated
    /// §3 contribution rather than an omission: a `$this` receiver saw no
    /// constructor and its enclosing docblock states no parameterization of the
    /// instance, and a static call has no receiver.
    ///
    /// The `new` arm used to be empty for a **value-IR** reason, measured in issue
    /// #374: [`Receiver::New`] carried the class reference and nothing else, so the
    /// constructor's arguments — which [`Cx::infer_generic_carry`] needs, and which
    /// the same expression in *argument* position kept as
    /// `ArgValue::New(class, args, named)` — were gone before any of this ran. They
    /// travel with the receiver now, so [`receiver_new_object`] can mint the object
    /// and fill this from its carries.
    ///
    /// [`receiver_new_object`]: crate::method_call::receiver_new_object
    pub(crate) receiver_carries: Vec<GenericCarry>,
    /// The caller variable naming the receiver **object** whose copy seeds the
    /// callee's `$this` (ADR-0086 §3, the receiver leg): the receiver is the zeroth
    /// argument, and this is how [`descend`] finds it in the caller's store.
    ///
    /// Filled by the exact `Receiver::Var` arm and by nothing else, this being the
    /// one receiver whose object is a **caller variable's** — which is all this
    /// field names. A `Receiver::New`'s object exists too, but it is minted by the
    /// caller ([`receiver_new_object`]) and handed to the descent as
    /// [`ThisSeed::ReceiverNew`], no variable being involved. Each remaining
    /// receiver seeds nothing and keeps its `$this` from [`seed_this_object`]:
    ///
    /// * `Receiver::This` — a `$this`-origin receiver is pre-escaped by
    ///   construction (ADR-0036), so [`copy_for_descent`] would drop its
    ///   non-readonly props anyway; seeding nothing is the same entry state, minus
    ///   a copy.
    /// * a **non-exact** `Receiver::Var` (a laundered `$this` alias, `clone $this`,
    ///   a declared parameter's seed) — it resolves through `resolve_guarded`, which
    ///   proves no receiver identity at all, so there is no object the callee is
    ///   entitled to (audit G1). Its *declared* carries still travel, above: they
    ///   are a fact about the class, not about the instance.
    /// * `Receiver::New` and `Callee::Construct` — no heap object exists yet at the
    ///   point the target resolves (the value-IR limit measured in issue #374).
    /// * a static call — no receiver.
    ///
    /// [`descend`]: crate::descent::descend
    /// [`receiver_new_object`]: crate::method_call::receiver_new_object
    /// [`ThisSeed::ReceiverNew`]: crate::descent::ThisSeed::ReceiverNew
    /// [`seed_this_object`]: crate::heap::seed_this_object
    /// [`copy_for_descent`]: crate::heap::copy_for_descent
    pub(crate) receiver_var: Option<String>,
}

/// Resolve a method/static/constructor `receiver` to a project target.
pub(crate) fn resolve_call_target<'a>(
    cx: &Cx<'a>,
    receiver: &Callee,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<CallTarget<'a>> {
    match receiver {
        Callee::Construct { class } => {
            let fqn = cx.class_fqn(class);
            resolve_exact(cx, &fqn, "__construct", enclosing_class, Some(fqn.clone()))
        }
        Callee::Method { receiver: Receiver::New { class, .. }, method, .. } => {
            let fqn = cx.class_fqn(class);
            resolve_exact(cx, &fqn, method, enclosing_class, Some(fqn.clone()))
        }
        Callee::Method { receiver: Receiver::Var(v), method, .. } => {
            if poisoned {
                return None;
            }
            let obj = store.obj_of(v)?;
            let class = obj.class.clone();
            if obj.class_exact {
                // An allocation-proven receiver (`$x = new Foo(); $x->m()`) dispatches
                // exactly — the precise dispatch. It is also the one arm holding a
                // heap object, so it is the one arm that can hand the target the
                // receiver's generic carries (issue #362). Read here, before the
                // statement's own escape/sweep pass runs: a receiver call sweeps the
                // value carries it is about to read, and the read must see the state
                // the call was made against.
                let carries = obj.targs.clone();
                let mut target =
                    resolve_exact(cx, &class, method, enclosing_class, Some(class.clone()))?;
                target.receiver_carries = carries;
                // And the object itself, for the descent's `$this` seed (ADR-0086
                // §3): the receiver is the zeroth argument, so the same exactness
                // that made the dispatch precise makes the copy admissible.
                target.receiver_var = Some(v.clone());
                Some(target)
            } else {
                // A lower-bound receiver — a laundered `$this` alias (`$u = $this`),
                // `clone $this`, or a declared parameter's seed (issue #388) — is NOT
                // exact (audit G1): fall back to the same final/private override guard
                // `Receiver::This` uses, so an overridable method on it never resolves
                // to the enclosing declaration.
                let mut target = resolve_guarded(cx, &class, method, enclosing_class)?;
                // Its **declared** carries still read (issue #388). A carry names the
                // class that declares the templates, not the runtime class, so a
                // `@param Helper<Model> $h` says exactly as much about a descendant of
                // `Helper` as about a `Helper` — which is why the exactness this arm
                // lacks is not the exactness the read needs. Only declared carries can
                // be here at all: a value carry is minted where an allocation proved
                // one, and an allocation is exact.
                target.receiver_carries = obj.declared_targs();
                // `receiver_var` stays `None`: this arm proves no receiver identity,
                // so the callee is entitled to no `$this` copy (ADR-0086 §3).
                Some(target)
            }
        }
        Callee::Method { receiver: Receiver::This, method, .. } => {
            let enclosing = enclosing_class?;
            match this_exact {
                Some(exact) => resolve_exact(cx, exact, method, enclosing_class, Some(exact.to_owned())),
                None => resolve_guarded(cx, enclosing, method, enclosing_class),
            }
        }
        Callee::Static { class: StaticClass::SelfKw, method } => {
            let enclosing = enclosing_class?;
            resolve_guarded(cx, enclosing, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Parent, method } => {
            let parent = cx.parent_fqn(enclosing_class?)?;
            resolve_static_named(cx, &parent, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Named(name), method } => {
            let fqn = cx.class_fqn(name);
            resolve_static_named(cx, &fqn, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Static, .. } => None,
        // A depth-1 property-fetch receiver is never a dispatch target (ADR-0052 §7):
        // the method is not resolved from the heap object — silent, like `Dynamic`.
        Callee::Method { receiver: Receiver::Prop { .. }, .. } => None,
        Callee::Function(_) | Callee::DynamicVar(_) | Callee::Dynamic => None,
    }
}

/// Resolve an exact-receiver instance/constructor call (no override guard).
pub(crate) fn resolve_exact<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
    this_exact: Option<String>,
) -> Option<CallTarget<'a>> {
    match resolve_in_chain(cx, class, method) {
        Resolution::Found(r) if !private_blocked(&r, enclosing_class) => Some(CallTarget {
            method: r.method,
            declaring_class: r.declaring_class,
            class_file: r.class_file,
            this_exact,
            receiver_carries: Vec::new(),
            receiver_var: None,
        }),
        _ => None,
    }
}

/// Resolve a `$this->`/`self::` call under the override guard.
fn resolve_guarded<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    let declaring_final = r.declaring_class.is_final;
    let final_or_private =
        r.method.is_final || r.method.visibility == Visibility::Private || declaring_final;
    if !final_or_private {
        return None;
    }
    Some(CallTarget {
        method: r.method,
        declaring_class: r.declaring_class,
        class_file: r.class_file,
        this_exact: None,
        receiver_carries: Vec::new(),
        receiver_var: None,
    })
}

/// Resolve an explicit `Foo::m()` / `parent::m()` static call (exact).
fn resolve_static_named<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    if !r.method.is_static && enclosing_class.is_none() {
        return None;
    }
    Some(CallTarget {
        method: r.method,
        declaring_class: r.declaring_class,
        class_file: r.class_file,
        this_exact: None,
        receiver_carries: Vec::new(),
        receiver_var: None,
    })
}

/// Whether a resolved `private` method is invisible at the call site.
///
/// **Resolution semantics, unchanged (issue #185).** The three resolver callers use
/// this to *suppress*: a blocked method is not a callable target, so they return
/// `None` and every downstream consumer (arity, effects, summaries, dispatch) keeps
/// seeing "no target" rather than a target it must not bind. The visibility
/// *finding* is emitted by [`check_inaccessible_method`], which is this predicate's
/// one additional caller — it asks the same question for the opposite purpose, so
/// the two can never disagree about what "blocked" means.
///
/// [`check_inaccessible_method`]: crate::inaccessible::check_inaccessible_method
pub(crate) fn private_blocked(r: &ResolvedMethod, enclosing_class: Option<&str>) -> bool {
    r.method.visibility == Visibility::Private
        && private_invisible(&r.declaring_class.fqn, enclosing_class)
}
