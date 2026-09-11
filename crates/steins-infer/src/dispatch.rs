//! Class-world method resolution (ADR-0001 sound dispatch), project-wide: resolving
//! a call target along the inheritance chain, exact / guarded / static-named
//! resolution, and the private-visibility block.

use std::collections::HashSet;

use steins_syntax::{Callee, ClassDecl, MethodDecl, Receiver, StaticClass, Visibility};

use crate::contract::{GenericCarry, IsA};
use crate::cx::Cx;
use crate::declared_receiver::declared_receiver_conjuncts;
use crate::env::{Store, Stratum};
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

/// What a chain walk is being asked for (ADR-0049 A18).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainMode {
    /// **Dispatch**: the declaration whose body the runtime will run. An abstract
    /// declaration has no body, and a trait-using class resolves names through
    /// machinery this analyzer does not lower — both are `Unknown`.
    Dispatch,
    /// **Declaration only** (ADR-0049 A16): the declaration whose *return envelope*
    /// binds the runtime one. PHP enforces return covariance at class-declaration
    /// time, so an abstract declaration's envelope is a sound upper bound under
    /// every implementation, and so is a parent's under a trait-imported override.
    /// A name that resolves nowhere on the class or its parents while a trait is in
    /// play stays `Unknown`: the trait could be declaring it (A18).
    Declaration,
}

/// Walk `start_fqn`'s project inheritance chain for a concrete `method`.
pub(crate) fn resolve_in_chain<'a>(cx: &Cx<'a>, start_fqn: &str, method: &str) -> Resolution<'a> {
    resolve_in_chain_mode(cx, start_fqn, method, ChainMode::Dispatch)
}

/// [`resolve_in_chain`] under an explicit [`ChainMode`] — the walk both resolvers
/// share, differing only in the two refusals ADR-0049 A18 re-examined.
pub(crate) fn resolve_in_chain_mode<'a>(
    cx: &Cx<'a>,
    start_fqn: &str,
    method: &str,
    mode: ChainMode,
) -> Resolution<'a> {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Resolution::Unknown;
        }
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return Resolution::Unknown; // chain leaves the project
        };
        // A18: under `Declaration` a trait user is walked rather than refused on
        // sight, so a name the class or a parent *declares* still answers. A name
        // only a trait provides is still refused — it is nowhere on the walk, so
        // the chain runs out and the caller gets nothing either way.
        if cd.uses_traits && mode == ChainMode::Dispatch {
            return Resolution::Unknown;
        }
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
            return if m.is_abstract && mode == ChainMode::Dispatch {
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
    /// `true` exactly when [`resolve_declaration_target`] answered this target and the
    /// dispatch resolver did not — the declaration-only path of ADR-0049 A16.
    ///
    /// Read by one consumer, [`enforced_top`](Self::enforced_top): A16 states that an
    /// unrepresentable hint (`: array` among them) leaves the declaration path saying
    /// nothing, and issue #603 keeps that gate shut while opening the proven-target one.
    pub(crate) declaration_only: bool,
}

impl CallTarget<'_> {
    /// The enforced top the target's return hint declares (issue #603), withheld on the
    /// declaration-only path so ADR-0049 A16's silence survives unchanged.
    pub(crate) fn enforced_top(&self) -> Option<steins_syntax::EnforcedTop> {
        if self.declaration_only { None } else { self.method.ret_top }
    }
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
            declaration_only: false,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The resolve-for-declaration path (ADR-0049 A16-A18, issue #619).
// ---------------------------------------------------------------------------

/// Resolve a call to the **declaration** its receiver's declared chain names, for
/// the return-envelope readers and nothing else (ADR-0049 A16).
///
/// [`resolve_call_target`] answers `None` for every receiver whose runtime class is
/// not proven — a non-`final` class, an overridable method, `$this` in an open
/// class — and that refusal is right for every consumer that *acts on* the resolved
/// method: [`descend`] walks its body, [`promote`] rewrites its call site,
/// [`apply_call_asserts`] applies its `@phpstan-assert` tags, and an override may
/// run instead of any of them. It is wrong for the one consumer that only asks
/// *what the call returns*: PHP enforces return covariance at class-declaration
/// time — a child cannot widen the parent's promise, and [`override_return_widens`]
/// already convicts the attempt — so the declaring method's return envelope is a
/// sound upper bound under every descendant. That is a membership-direction claim
/// about the result, which ADR-0049 A1 point 8 permits without the exactness bit.
///
/// So the two questions get two resolvers, and this is the second one. It is
/// reached only from [`method_return_arms_by_callee`], only after
/// [`resolve_call_target`] has declined, and what it yields is deliberately
/// impoverished: no exactness claim, no `this_exact`, no `receiver_var`, and no
/// carries beyond the ones the receiver's *declaration* states. Nothing here can
/// widen what a body-walking consumer sees, because no body-walking consumer calls
/// it.
///
/// The second return is the **receiver lane's minimum stratum** (A17/A13): a native
/// `C $o` is runtime-enforced and stays `Verified`, a docblock or `instanceof`-fact
/// carrier is `Asserted` and demotes the whole answer with it.
///
/// Two refusals are kept, both stated rather than incidental:
///
/// * **`@return static` / late static binding is deferred** (A19). Binding `static`
///   to the declared class is sound as an upper bound but spells the same as
///   `self`, losing the *calling* receiver's identity that `@return static` exists
///   to carry; the right mechanism is template binding over the receiver's arms,
///   which is not a dispatch question. [`override_return_widens`] already goes
///   silent on [`MethodDecl::ret_bound_keyword`] and this does the same.
/// * **A poisoned scope** answers nothing, exactly as the dispatch resolver does.
///
/// [`descend`]: crate::descent::descend
/// [`promote`]: crate::promote
/// [`apply_call_asserts`]: crate::asserts
/// [`override_return_widens`]: crate::overrides
/// [`method_return_arms_by_callee`]: crate::return_arms::method_return_arms_by_callee
pub(crate) fn resolve_declaration_target<'a>(
    cx: &Cx<'a>,
    receiver: &Callee,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<(CallTarget<'a>, Stratum)> {
    if poisoned {
        return None;
    }
    let (class, method, stratum, carries) = match receiver {
        Callee::Construct { class } => {
            (cx.class_fqn(class), "__construct".to_owned(), Stratum::Verified, Vec::new())
        }
        Callee::Method { receiver: Receiver::New { class, .. }, method, .. } => {
            (cx.class_fqn(class), method.clone(), Stratum::Verified, Vec::new())
        }
        Callee::Method { receiver: Receiver::Var(v), method, .. } => {
            let (class, stratum) = declared_receiver_class(cx, store, v)?;
            // Only the receiver's **declared** carries travel, as in the dispatch
            // resolver's non-exact arm (issue #388): a carry names the class that
            // declares the templates, not the runtime class.
            let carries = store.obj_of(v).map(|o| o.declared_targs()).unwrap_or_default();
            (class, method.clone(), stratum, carries)
        }
        // A `$this` whose exactness is unproven still names its enclosing class as a
        // declared lower bound — the same fact `resolve_guarded` reads, minus the
        // finality guard. An exact `$this` never reaches here: `resolve_call_target`
        // answered it, or its chain walk refused for a reason the mode lift below
        // re-examines.
        Callee::Method { receiver: Receiver::This, method, .. } => {
            let class = this_exact.unwrap_or(enclosing_class?).to_owned();
            (class, method.clone(), Stratum::Verified, Vec::new())
        }
        Callee::Static { class: StaticClass::SelfKw, method } => {
            (enclosing_class?.to_owned(), method.clone(), Stratum::Verified, Vec::new())
        }
        Callee::Static { class: StaticClass::Parent, method } => {
            (cx.parent_fqn(enclosing_class?)?, method.clone(), Stratum::Verified, Vec::new())
        }
        Callee::Static { class: StaticClass::Named(name), method } => {
            (cx.class_fqn(name), method.clone(), Stratum::Verified, Vec::new())
        }
        // `static::` names no class here either (A19's question, from the receiver
        // side); a depth-1 property fetch is ADR-0052 §7's limit, untouched.
        Callee::Static { class: StaticClass::Static, .. }
        | Callee::Method { receiver: Receiver::Prop { .. }, .. }
        | Callee::Function(_)
        | Callee::DynamicVar(_)
        | Callee::Dynamic => return None,
    };
    // The private-shadow rule (`ZEND_ACC_CHANGED`): from inside a class that
    // declares a PRIVATE `m`, `$c->m()` on any `$c` that is-a that class calls the
    // private one, whatever a subclass declares under the same name — a private
    // method is not virtual, so the declared chain's answer would be the wrong
    // declaration. The is-a question must be proven; an undecided hierarchy
    // answers nothing rather than either declaration.
    let shadow = enclosing_class.and_then(|enc| {
        let (efile, ecd) = cx.find_class(enc)?;
        let pm = ecd
            .methods
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(&method) && m.visibility == Visibility::Private)?;
        Some(match cx.is_a(&class, enc) {
            IsA::Yes => Some(ResolvedMethod { method: pm, declaring_class: ecd, class_file: efile }),
            IsA::No => None,
            _ => return Some(None),
        })
    });
    let r = match shadow {
        Some(Some(r)) => r,
        Some(None) => return None,
        None => {
            let Resolution::Found(r) =
                resolve_in_chain_mode(cx, &class, &method, ChainMode::Declaration)
            else {
                return None;
            };
            r
        }
    };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    // Parity with `resolve_static_named`: `P::m()` on an instance method with no
    // enclosing class is a PHP 8 `Error`, so the call never returns and there is
    // no envelope to hand the statement after it.
    if matches!(receiver, Callee::Static { class: StaticClass::Named(_), .. })
        && !r.method.is_static
        && enclosing_class.is_none()
    {
        return None;
    }
    // A19: `static`/`self`/`parent` return bounds are deferred with the reason.
    if r.method.ret_bound_keyword.is_some() {
        return None;
    }
    let target = CallTarget {
        method: r.method,
        declaring_class: r.declaring_class,
        class_file: r.class_file,
        this_exact: None,
        receiver_carries: carries,
        receiver_var: None,
        declaration_only: true,
    };
    Some((target, stratum))
}

// ---------------------------------------------------------------------------
// The builtin-receiver path (issue #673): where the project chain runs out.
// ---------------------------------------------------------------------------

/// What the builtin class-method return table is to be asked about at a call
/// site: the builtin class the receiver's declared chain ends at, the method
/// name, whether the call was written in static form, and the stratum the
/// receiver's own declaration rides at.
pub(crate) struct BuiltinCallee {
    pub(crate) class: String,
    pub(crate) method: String,
    /// `Foo::m()` / `self::m()` / `parent::m()`, as opposed to `$o->m()`. PHP
    /// lets an instance receiver call a static method, so only this direction
    /// constrains: a `::` call on a row the engine does not declare `static` is
    /// an `Error` unless it is forwarding `$this` from inside the class.
    pub(crate) static_call: bool,
    pub(crate) stratum: Stratum,
    /// Whether the call site sits inside a class at all — the one thing a
    /// static-form call needs beyond the row itself (see [`static_call`]).
    ///
    /// [`static_call`]: Self::static_call
    pub(crate) inside_class: bool,
}

/// Read a call's receiver for the **builtin** class-method return table
/// (issue #673) — the third resolver, reached only after both
/// [`resolve_call_target`] and [`resolve_declaration_target`] have declined.
///
/// The receiver is read exactly as [`resolve_declaration_target`] reads it
/// (ADR-0049 A17's declared-receiver lane: an allocation, an `instanceof` fact, a
/// narrowed one-class contract lane, a declared heap object), so a `SplFileObject $f`
/// parameter, a `new DOMDocument()` and a `?PDO` past its null guard all arrive here
/// the same way. What differs is where the name is then looked up.
///
/// **A builtin class never reaches [`resolve_in_chain`] as a project class.** It
/// has no `ClassDecl`, so `cx.find_class` misses and the chain walk answers
/// `Unknown` at its first step — which is why the two resolvers above declined and
/// why this one exists. [`builtin_root`] resumes that walk at exactly the point it
/// stopped: it follows the project `extends` chain and returns the first name the
/// project does not declare, refusing outright if any project class on the way
/// **declares the method itself**. That refusal is what keeps the two answers from
/// disagreeing for a project class extending a builtin — a child's own declaration
/// is the sharper and the correct answer, and it was already given by
/// `resolve_declaration_target`; this table answers the inherited names alone.
pub(crate) fn resolve_builtin_callee(
    cx: &Cx,
    receiver: &Callee,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<BuiltinCallee> {
    if poisoned {
        return None;
    }
    let (class, method, static_call, stratum) = match receiver {
        Callee::Method { receiver: Receiver::New { class, .. }, method, .. } => {
            (cx.class_fqn(class), method.clone(), false, Stratum::Verified)
        }
        Callee::Method { receiver: Receiver::Var(v), method, .. } => {
            let (class, stratum) = declared_receiver_class(cx, store, v)?;
            (class, method.clone(), false, stratum)
        }
        Callee::Method { receiver: Receiver::This, method, .. } => {
            let class = this_exact.unwrap_or(enclosing_class?).to_owned();
            (class, method.clone(), false, Stratum::Verified)
        }
        Callee::Static { class: StaticClass::SelfKw, method } => {
            (enclosing_class?.to_owned(), method.clone(), true, Stratum::Verified)
        }
        Callee::Static { class: StaticClass::Parent, method } => {
            (cx.parent_fqn(enclosing_class?)?, method.clone(), true, Stratum::Verified)
        }
        Callee::Static { class: StaticClass::Named(name), method } => {
            (cx.class_fqn(name), method.clone(), true, Stratum::Verified)
        }
        // The same four silences the declaration path keeps: `static::` names no
        // class (A19 from the receiver side), a depth-1 property fetch is
        // ADR-0052 §7's limit, a constructor is the ADR-0036 exactness lane and
        // has no return envelope, and a function/dynamic callee has no receiver.
        Callee::Construct { .. }
        | Callee::Static { class: StaticClass::Static, .. }
        | Callee::Method { receiver: Receiver::Prop { .. }, .. }
        | Callee::Function(_)
        | Callee::DynamicVar(_)
        | Callee::Dynamic => return None,
    };
    Some(BuiltinCallee {
        class: builtin_root(cx, &class, &method)?,
        method,
        static_call,
        stratum,
        inside_class: enclosing_class.is_some(),
    })
}

/// The **builtin** class a project `extends` chain ends at, when the chain
/// declares `method` nowhere along the way.
///
/// Three outcomes, and the two refusals are the load-bearing half:
///
/// * a project class on the chain **declares the method** — refuse. The project's
///   own declaration is the answer, `resolve_declaration_target` already gave it,
///   and a table row on the inherited name must never compete with it.
/// * the chain **ends inside the project** (a root class with no `extends`) —
///   refuse. Nothing builtin is involved; the name is simply absent, which is the
///   absence family's question and not this table's.
/// * the chain **leaves the project** — that name is the answer, whether it is the
///   receiver's own class (`SplFileObject $f`, never declared here) or a builtin
///   ancestor of one that is (`class Reader extends SplFileObject`).
///
/// Only `extends` is followed, as [`resolve_in_chain_mode`] follows only `extends`:
/// a method reached through a builtin *interface* the project class implements is
/// not on this walk. A `use`-ing class that does not declare the name refuses, for
/// ADR-0049 A18's reason — trait method bodies and return types are not lowered, so
/// "not found on the class" cannot be read as "inherited from the parent" while a
/// trait could be declaring it.
fn builtin_root(cx: &Cx, class: &str, method: &str) -> Option<String> {
    let mut cur = class.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None; // a cycle in the declared chain
        }
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return Some(cur); // the chain left the project: a builtin name
        };
        if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(method)) {
            return None;
        }
        if cd.uses_traits {
            return None;
        }
        cur = cx.units[cfile].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
}

/// The class a `$var` receiver is **declared** to be, and the stratum that
/// declaration rides at (ADR-0049 A17): the declared-receiver lane's carrier, not a
/// heap object.
///
/// The heap object is the first refusal of the three #619 measured — the dispatch
/// resolver's non-exact arm opens `store.obj_of(v)?`, and
/// [`seed_declared_param_object`] declines a `?C` hint, a union, an `@param object`,
/// and every `@param` spelling other than a plain class. A17 lifts it by reading the
/// receiver the way S6 does, in the order that prefers the narrowest fact:
///
/// 1. an **allocation-proven** class (`class_exact`) — `Verified`, and the only
///    reason it reaches this function at all is a chain walk A18 re-examines;
/// 2. the **`instanceof` fact** bound by branch analysis when it names exactly one
///    class — the `@param object $foo` witness. Read at `Asserted`: [`Member`]
///    carries no stratum of its own, and an `assert($foo instanceof Foo)` narrowing
///    is `Asserted` by ADR-0052 §5, so the whole carrier takes the weaker grade
///    rather than guessing per site;
/// 3. the **narrowed contract-arm lane**, when what survives is a single class arm
///    — a `?Reservation` parameter past its null guard is exactly this — at the
///    lane's own minimum stratum (A13);
/// 4. a **declared heap object**, which is what the dispatch resolver already had.
///
/// A surviving lane of two or more class arms declines: the answer would be the
/// union of two declarations' return envelopes, and a [`CallTarget`] names one
/// method. That is a floor this slice does not build, not a soundness limit.
///
/// [`seed_declared_param_object`]: crate::heap::seed_declared_param_object
/// [`Member`]: crate::env::Member
pub(crate) fn declared_receiver_class(cx: &Cx, store: &Store, var: &str) -> Option<(String, Stratum)> {
    if let Some(obj) = store.obj_of(var)
        && obj.class_exact
    {
        return Some((obj.class.clone(), Stratum::Verified));
    }
    if let Some(m) = store.members.get(var)
        && let [only] = m.yes.as_slice()
    {
        return Some((only.clone(), Stratum::Asserted));
    }
    if let Some(arms) = store.contract_arms(var) {
        if let Some(lane) = declared_receiver_conjuncts(cx, arms)
            && let [conjuncts] = lane.as_slice()
            && let [only] = conjuncts.as_slice()
        {
            let stratum = arms.iter().fold(Stratum::Verified, |acc, a| acc.min(a.stratum));
            return Some((only.clone(), stratum));
        }
        return None;
    }
    store.obj_of(var).map(|o| (o.class.clone(), Stratum::Verified))
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
        declaration_only: false,
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
        declaration_only: false,
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
