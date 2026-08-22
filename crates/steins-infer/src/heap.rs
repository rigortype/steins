//! Heap objects (ADR-0086): `new Class(...)` allocation with constructor-seeded
//! properties, `$this` seeding with readonly bookkeeping, declared-parameter objects,
//! property assignment, binding keys, and closure values.

use std::collections::{HashMap, HashSet};

use steins_contract::ContractTy;
use steins_domain::{Certainty, Fact, Val};
use steins_phpdoc::{Type as PType, TagKind, scan_docblock};
use steins_phpdoc::ast::TypeKind as PKind;
use steins_syntax::{ArgValue, NamedArg, Param, PropertyDecl, TypeMember};

use crate::fold::Folder;
use crate::{
    PHPDOC_PROP_MISMATCH_ID, PROP_MISMATCH_ID, READONLY_REASSIGNED_ID, arg_abstract_fact,
    contract_touches_class, describe_fact,
};
use crate::arg_check::is_type_error;
use crate::coerce::coerce_fact_to_native;
use crate::contract::{
    CArg, CVal, GenericCarry, TemplateShadow, accepts, class_key, neutralize_templates,
    parse_tag_type, template_names_of,
};
use crate::cx::Cx;
use crate::descent::{ThisSeed, descend};
use crate::dispatch::resolve_exact;
use crate::env::{
    AllocId, ClosureTarget, ClosureVal, Descent, HeapObj, HeapSummary, Known, PropFact, Store,
    Stratum, arg_of_val, singleton_fact,
};
use crate::project::Diagnostic;
use crate::refine::seed_fact;
use crate::return_arms::mentioned_templates;
use crate::walk::{WalkCx, value_stratum};

/// Allocate a fresh heap object for `new Class(args)` (ADR-0036) into the walk's
/// store, under a fresh allocation id. Returns that id.
///
/// `ctor` is the constructor descent's `$this` snapshot for this very `new` site
/// when one was taken (ADR-0057's constructor-summary amendment, C4): the fresh
/// allocation then **is** that snapshot, because the allocation had no alias before
/// the constructor ran and the snapshot is therefore the whole of what happened to
/// it. `None` — no walk, or a walk that agreed on nothing (C6) — builds the
/// declaration-only object under the ADR-0086 §4 lexical gate, byte for byte as
/// before the amendment.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_new_object(
    w: &WalkCx,
    folder: &mut dyn Folder,
    class: &str,
    args: &[ArgValue],
    named: &[NamedArg],
    env: &HashMap<String, Known>,
    store: &mut Store,
    ctor: Option<&HeapSummary>,
) -> AllocId {
    let id = w.fresh_id();
    let obj = match ctor {
        Some(h) => {
            // Class and exactness are unchanged by construction (C4): the seed named
            // them and no walk alters what class an allocation is. Asserted rather
            // than recomputed — a mismatch would mean the snapshot came from another
            // site's walk.
            debug_assert!(
                h.obj.class == class && h.obj.class_exact,
                "a constructor snapshot must be the exact allocation its `new` site minted",
            );
            h.obj.clone()
        }
        None => new_heap_object(
            w.cx,
            folder,
            class,
            args,
            named,
            env,
            store,
            w.scope.poisoned,
            CtorDefaults::Lexical,
        ),
    };
    store.heap.insert(id, obj);
    id
}

/// Which literal property defaults a freshly minted `new` object keeps
/// (ADR-0057's constructor-summary amendment, C1).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CtorDefaults {
    /// The undescended floor (ADR-0086 §4): a default survives only where the
    /// constructor that runs never **mentions** the slot in its own body text. The
    /// answer for every site whose constructor no walk reaches (C6).
    Lexical,
    /// Every default stands — the seed of a constructor the descent is about to
    /// **walk** (C1). The lexical scan exists because `build_new_object` could not
    /// read the body; a walked body needs no over-approximation of itself, since
    /// the walk executes the writes that overwrite the defaults and sweeps what it
    /// hands to a body it does not read (C5).
    All,
}

/// The object a `new Class(args)` expression allocates (ADR-0036): props populated
/// from literal property defaults and promoted constructor parameters, the readonly
/// set from `readonly`-declared properties, and the class-level carries the
/// arguments prove. Split out of [`build_new_object`] so the binding descent can
/// mint the same object for a `new` written in **argument** position (ADR-0086 §2)
/// without a second constructor of properties — `store` is read-only here (the
/// arguments resolve against the *caller's* heap) and the id is the caller's job.
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_heap_object(
    cx: &Cx,
    folder: &mut dyn Folder,
    class: &str,
    args: &[ArgValue],
    named: &[NamedArg],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    defaults: CtorDefaults,
) -> HeapObj {
    let mut obj = HeapObj::new(class.to_owned());
    obj.class_exact = true; // `new Class(...)` allocates exactly `Class` (audit G1)
    // The class-level generic parameterizations this allocation proves (ADR-0032
    // tier 3 + binding amendment, issue #295), recorded on the allocation so they
    // survive `$box = new MutableBox(1);` — read off the store before this object
    // joins it (the carry is a function of the arguments only).
    obj.targs = cx.infer_generic_carry(class, args, env, store, poisoned, folder);
    let props = cx.class_props(class);
    // Which literal defaults survive the constructor (ADR-0086 §4's stale-default
    // half). `None` means "every default stands" — the no-constructor case, and the
    // seed of a constructor this site is about to walk (ADR-0057 C1).
    let ctor_writes = match defaults {
        CtorDefaults::Lexical => ctor_touched_props(cx, class, &props),
        CtorDefaults::All => None,
    };

    // readonly set + literal defaults.
    for p in &props {
        // A hooked property (PHP 8.4 `get`/`set`) binds no value fact: writes run
        // arbitrary code, so the constructed/default value is not the stored value
        // (FP class 16). It is never readonly either (readonly + hook is a PHP fatal).
        if p.hooked {
            continue;
        }
        if p.readonly {
            obj.readonly.insert(p.name.clone());
        }
        if let Some(default) = &p.default
            && let Some(fact) = singleton_fact(default, cx.php_minor)
            // A typed slot stores the boundary-converted default (issue #48):
            // `public float $d = 3;` holds `3.0`, not `3`.
            && let Some(fact) = match p.ty.as_ref() {
                Some(ty) => coerce_fact_to_native(ty, fact),
                None => Some(fact),
            }
        {
            // Skip null-admitting facts (unsound to flow past unmodeled guards). A
            // literal default is `Verified` (no env fact consumed).
            //
            // And skip a default the running constructor **touches** (ADR-0086 §4's
            // stale-default half): `build_new_object` never walks the constructor
            // body, so `private $view = 0;` overwritten by `$this->view = $arg -
            // $n;` would otherwise stand as a proven `0` on a freshly allocated
            // object. Dropping the seed leaves the prop unknown — exactly the state
            // a declaration without a default produces, and the state every reader
            // already handles.
            if !fact_is_nullish(&fact)
                && ctor_writes.as_ref().is_none_or(|w| !w.contains(p.name.as_str()))
            {
                obj.props.insert(p.name.clone(), PropFact { fact, stratum: Stratum::Verified });
            }
            // Readonly bookkeeping is untouched by the gate: it records that the slot
            // *was written*, which is no less true when the constructor writes it
            // again. (PHP forbids a default on a readonly property outright, so the
            // two clauses cannot even meet on valid input.)
            if p.readonly {
                obj.ro_written.insert(p.name.clone());
            }
        }
    }

    // Promoted constructor params: bind each from its positional `new` argument.
    if let Some((_, ctor)) = cx.find_ctor(class) {
        // A hooked promoted param (`public int $n { set { … } }`) binds no fact — its
        // write runs arbitrary code, so the raw argument is not the stored value
        // (FP class 16). Excluded here; its value stays Unknown.
        let promoted: HashMap<&str, &&PropertyDecl> =
            props.iter().filter(|p| p.promoted && !p.hooked).map(|p| (p.name.as_str(), p)).collect();
        for (i, param) in ctor.params.iter().enumerate() {
            if param.variadic {
                break;
            }
            let Some(pd) = promoted.get(param.name.as_str()) else { continue };
            // The bound argument: the positional at this index, else a matching
            // named argument (case-sensitive PHP semantics). Positional-and-named
            // collision is a PHP fatal, so the two are disjoint on valid input.
            let bound = args
                .get(i)
                .or_else(|| named.iter().find(|n| n.name == param.name).map(|n| &n.value));
            // The value: the resolved arg literal (carrying its stratum), else the
            // param's native-type seed (`Verified`).
            let (fact, stratum) = match bound {
                Some(a) => match cx
                    .resolve_literal_strat(a, env, poisoned, folder)
                    .and_then(|(lit, strat)| {
                        singleton_fact(&lit, cx.php_minor).map(|f| (f, strat))
                    })
                    // The promoted slot stores the boundary-converted argument
                    // (issue #48): a mode-dependent conversion falls back to the
                    // native seed, which covers whatever the runtime produces.
                    .and_then(|(f, strat)| match param.ty.as_ref() {
                        Some(ty) => coerce_fact_to_native(ty, f).map(|f| (f, strat)),
                        None => Some((f, strat)),
                    }) {
                    Some((f, strat)) => (Some(f), strat),
                    None => (seed_fact(param), Stratum::Verified),
                },
                None => (seed_fact(param), Stratum::Verified),
            };
            // Skip null-admitting facts (unsound to flow past unmodeled guards).
            if let Some(fact) = fact
                && !fact_is_nullish(&fact)
            {
                obj.props.insert(pd.name.clone(), PropFact { fact, stratum });
            }
            // A promoted param is *always* written at construction — even when its
            // value is unknown, record the write (readonly.reassigned first write).
            if pd.readonly {
                obj.ro_written.insert(pd.name.clone());
            }
        }
    }

    obj
}

/// The property names the constructor that runs for `class` **mentions** as
/// `$this->{prop}` — the gate on literal-default seeding (ADR-0086 §4).
///
/// `None` means "no constructor runs", the one case in which a declared default is
/// the constructed value with nothing between them. `Some(set)` names the props whose
/// default must be dropped; an **empty** set is the constructor that touches none of
/// them, and is not the same answer as `None` only in spirit.
///
/// **Why a lexical scan.** [`build_new_object`] mints an object without walking the
/// constructor (ADR-0086 §4 keeps that gap open — a constructor's writes still yield
/// no props), so it cannot ask what the body *stores*. It can ask the weaker,
/// decidable question the ADR-0032 argument-pass gate already asks about parameters:
/// **can the body refer to this slot at all**. A mention is a mention — a write, a
/// compound assign, `++`, `??=`, a by-ref pass, even one inside a string or a comment
/// — and every one of them drops the seed. A false hit costs knowledge and nothing
/// else; a miss would leave a wrong `Verified` fact on the heap, which is the failure
/// direction this gate exists to close. The scan runs over the body's **source text**
/// ([`MethodDecl::body_span`]) rather than the linear trace, for the same reason
/// [`callee_cannot_reach_arg`] does: the trace drops nested sub-expressions, so a
/// write inside one would be invisible to it.
///
/// **Every uncertainty seeds nothing.** A constructor whose body text is unreadable,
/// or whose scope is poisoned (`extract`, `$$v`, `eval` — a slot can be written
/// without being spelled), answers with a set containing every property, so no
/// default survives. Unknown is never proof that a default stands.
///
/// So does every constructor that lets `$this` out of its own text — see
/// [`ThisReach::escapes`]. The per-property rule stays fine-grained only where
/// nothing delegates.
///
/// Promoted parameters are unaffected: their fact is the *argument*, proven at the
/// call site, and the engine writes it before any body statement runs.
///
/// [`MethodDecl::body_span`]: steins_syntax::MethodDecl::body_span
/// [`callee_cannot_reach_arg`]: crate::descent::callee_cannot_reach_arg
fn ctor_touched_props(cx: &Cx, class: &str, props: &[&PropertyDecl]) -> Option<HashSet<String>> {
    let (cfile, ctor) = cx.find_ctor(class)?;
    let all = || props.iter().map(|p| p.name.clone()).collect::<HashSet<String>>();
    // The class whose declaration owns the body being scanned — a `Foo::init(…)` in
    // that body's own text names *itself* by this spelling, the by-name twin of
    // `self::init(…)` (issue #417).
    let owner_fqn = ctor_owner_fqn(cx, class);
    // A poisoned constructor can reach a slot without spelling it.
    if cx.method_scope(cfile, &owner_fqn, &ctor.name).is_none_or(|s| s.poisoned) {
        return Some(all());
    }
    let Some(span) = ctor.body_span else { return Some(all()) };
    let Some(text) = cx.units[cfile].tree.text_at(span) else { return Some(all()) };
    let owner_name = cx.find_class(&owner_fqn).map_or(owner_fqn.as_str(), |(_, cd)| cd.name.as_str());
    let reach = this_prop_mentions(text, &owner_fqn, owner_name);
    // `$this` reached somewhere this scan cannot follow: no slot is safe.
    if reach.escapes {
        return Some(all());
    }
    // Property names are case-SENSITIVE in PHP, so the spelled set is compared as
    // written — `$this->View` is a different slot from `$this->view`.
    Some(props.iter().map(|p| p.name.clone()).filter(|n| reach.named.contains(n)).collect())
}

/// The class whose declaration owns the constructor [`Cx::find_ctor`] resolved for
/// `class` — an inherited constructor's scope is keyed by the declaring class, not by
/// the subclass being allocated.
fn ctor_owner_fqn(cx: &Cx, class: &str) -> String {
    let mut cur = class.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return cur;
        }
        let Some((file, cd)) = cx.find_class(&cur) else { return cur };
        if cd.methods.iter().any(|m| m.is_constructor) {
            return cd.fqn.clone();
        }
        let Some(parent) = cd.parent.as_ref() else { return cur };
        cur = cx.units[file].tree.resolve_class_fqn(parent);
    }
}

/// What a constructor body's text reveals about the slots it can reach — the answer
/// [`ctor_touched_props`] gates literal-default seeding on.
struct ThisReach {
    /// The property names the text spells directly: `$this->view`. Meaningful only
    /// while [`Self::escapes`] is `false`.
    named: HashSet<String>,
    /// `$this` went somewhere this scan cannot follow, so **no** slot is safe and
    /// every literal default is dropped. Four shapes set it, each one a route by
    /// which a slot is written without this text spelling it:
    ///
    /// * **a bare `$this`** — not followed by `->`. Passed to a function, assigned to
    ///   a variable, returned, captured by a closure, pushed into an array: every one
    ///   of those hands out an alias that can write any property.
    /// * **`$this->m(…)`** — the delegating shape (`__construct() { $this->init(); }`
    ///   with `init()` writing `$this->view`). A method call runs a body this scan is
    ///   not reading.
    /// * **`parent::m(…)`, `self::m(…)`, `static::m(…)`, and `Foo::m(…)` spelling
    ///   the enclosing class by its own short name or FQN** — `parent::__construct()`
    ///   above all, and `Foo::init()` written from inside `Foo`'s own constructor is
    ///   `self::init()` under another spelling (issue #417). These run with the
    ///   *same* `$this`, so they are the delegating shape under a different spelling.
    /// * **`$this->$name` / `$this->{…}`** — the slot is chosen at runtime, so the
    ///   text names none.
    ///
    /// **Deliberately coarse, and the cost is unknown.** A constructor that calls one
    /// `$this` method loses the defaults of properties that method could not possibly
    /// touch. Nothing on the measured corpora moved either way, so the precision cost
    /// is not merely small — it is *unmeasured*, and recorded as such (ADR-0086 §4).
    /// Refining it needs a per-callee property-write summary: which slots can this
    /// call write? That is the same ADR-0055 Part II mutation inference the
    /// caller-side sweep refusal has been waiting on, and until it exists a wrong
    /// `Verified` default (a proof-layer false positive) is strictly worse than a
    /// dropped one (lost knowledge).
    escapes: bool,
}

/// Scan a constructor body's **source text** for [`ThisReach`]. The property-world
/// twin of [`mentions_variable`], and deliberately just as blunt.
///
/// `owner_fqn` and `owner_name` are the FQN and short name of the class whose
/// declaration the scanned body belongs to (issue #417) — a `Foo::init(…)` spelled
/// either way is `self::init(…)` under the enclosing class's own name rather than
/// the keyword, and closes the same hole. Both are compared case-insensitively,
/// matching PHP's own class-name resolution; the FQN is expected pre-normalized
/// (no leading `\`, [`ClassDecl::fqn`]'s own form) since the scan tolerates one
/// appearing in the text either way (see below).
///
/// Boundaries are what keep `$this->view` from matching `$this->viewCount`. Whitespace
/// of any kind may sit around the arrow (a `$this` and its `->` on two lines is one
/// access), and `$this?->p` is the same slot under a null guard. A match inside a
/// string literal or a comment is *accepted* as a mention, which errs toward dropping
/// the seed and so toward silence.
///
/// [`mentions_variable`]: crate::descent::mentions_variable
/// [`ClassDecl::fqn`]: steins_syntax::ClassDecl::fqn
fn this_prop_mentions(text: &str, owner_fqn: &str, owner_name: &str) -> ThisReach {
    let bytes = text.as_bytes();
    let is_name_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || !b.is_ascii();
    let skip_ws = |mut j: usize| {
        while bytes.get(j).copied().is_some_and(|b| b.is_ascii_whitespace()) {
            j += 1;
        }
        j
    };
    // `parent::m(…)` / `self::m(…)` / `static::m(…)` / `Foo::m(…)`: another body,
    // the same `$this`. The keywords and class names are case-insensitive in PHP;
    // `to_ascii_lowercase` rewrites no multi-byte character, so offsets into it are
    // offsets into `text`. A leading `\` before an FQN spelling (`\App\Foo::m(…)`)
    // is not a name byte, so the left-boundary check below admits it unchanged.
    let lower = text.to_ascii_lowercase();
    let same_this_static_call = |kw: &str| {
        let mut i = 0usize;
        while let Some(off) = lower[i..].find(kw) {
            let start = i + off;
            let mut j = start + kw.len();
            i = j;
            // A whole token: `myself::` is not `self::`.
            if start > 0 && bytes.get(start - 1).copied().is_some_and(|b| is_name_byte(b) || b == b'$')
            {
                continue;
            }
            j = skip_ws(j);
            let name_start = j;
            while bytes.get(j).copied().is_some_and(is_name_byte) {
                j += 1;
            }
            // A bare `self::CONST` reads a constant and runs nothing.
            if j > name_start && bytes.get(skip_ws(j)).copied() == Some(b'(') {
                return true;
            }
        }
        false
    };

    let owner_fqn_kw = format!("{}::", owner_fqn.to_ascii_lowercase());
    let owner_name_kw = format!("{}::", owner_name.to_ascii_lowercase());
    let mut named: HashSet<String> = HashSet::new();
    let mut escapes = ["parent::", "self::", "static::", owner_fqn_kw.as_str(), owner_name_kw.as_str()]
        .iter()
        .any(|kw| same_this_static_call(kw));
    let mut i = 0usize;
    while let Some(off) = text[i..].find("$this") {
        let mut j = i + off + "$this".len();
        i = j;
        // A longer variable that merely starts with `this` (`$thisOne`) is not `$this`.
        if bytes.get(j).copied().is_some_and(is_name_byte) {
            continue;
        }
        j = skip_ws(j);
        // `$this?->p` is a property access; a `?` that is not the nullsafe arrow's is
        // a ternary on `$this`, which is the bare shape.
        let arrow = if bytes.get(j).copied() == Some(b'?') { j + 1 } else { j };
        if !bytes[arrow..].starts_with(b"->") {
            escapes = true; // bare `$this`: an alias leaves this text
            continue;
        }
        j = skip_ws(arrow + 2);
        // `$this->$name` / `$this->{…}`: the slot is chosen at runtime.
        if matches!(bytes.get(j).copied(), Some(b'$') | Some(b'{')) {
            escapes = true;
            continue;
        }
        let start = j;
        while bytes.get(j).copied().is_some_and(is_name_byte) {
            j += 1;
        }
        if j == start {
            escapes = true; // an arrow followed by nothing this scan can read
            continue;
        }
        // `$this->m(…)` is a call into a body this scan is not reading; `$this->p` is
        // the slot itself.
        if bytes.get(skip_ws(j)).copied() == Some(b'(') {
            escapes = true;
        } else if let Ok(name) = std::str::from_utf8(&bytes[start..j]) {
            named.insert(name.to_owned());
        }
    }
    ThisReach { named, escapes }
}

/// Seed the `$this` object shell for a method scope (ADR-0036): `class_fqn` (the
/// exact receiver when a descent proved one, else the enclosing class as a lower
/// bound), plus the readonly set and provably-written readonly props from the class
/// surface. `class_exact` records whether that class is exact (audit G1). `$this` is
/// pre-escaped (an overridable call on it sweeps its non-readonly props). Returns
/// `None` when the class has no tracked properties.
///
/// Seeds NO property value facts: a property's value in an arbitrary method is
/// whatever some other method last stored, which this per-scope walk doesn't
/// model, so assuming the declared default would produce null-property false
/// positives past `!== null` guards. Only facts written *in this method* flow;
/// readonly bookkeeping stays since a readonly value can't change post-construction.
///
/// That reasoning is about an entry with **no caller in hand**, which is every entry
/// this function still serves: the plain per-scope pass, and a descent whose receiver
/// proved no object (ADR-0086 §3 lists them). Where a descent *did* prove one — an
/// exact `Receiver::Var` — `$this` is seeded from that receiver's copy in [`descend`]
/// instead, and the props are the caller's own proven facts rather than an assumption
/// about what some other method stored.
pub(crate) fn seed_this_object(cx: &Cx, class_fqn: &str, class_exact: bool) -> Option<HeapObj> {
    if cx.class_props(class_fqn).is_empty() {
        return None;
    }
    let mut obj = HeapObj::new(class_fqn.to_owned());
    obj.escaped = true; // pre-escaped
    // Membership is not exactness (audit G1): `$this` is a lower bound unless the
    // caller proved the exact receiver (a binding descent) or the enclosing class
    // has no subclass (`final`/enum). The No-side consumers gate on this bit.
    obj.class_exact = class_exact;
    seed_readonly_bookkeeping(cx, &mut obj, class_fqn);
    Some(obj)
}

/// The `readonly` bookkeeping a class's own property surface contributes to a
/// **membership** seed (ADR-0036): the declared readonly set, plus the subset a
/// construction provably wrote — a promoted readonly parameter, or a readonly
/// property with a literal default — which is the first write `readonly.reassigned`
/// counts from.
///
/// Shared by the two membership seeds, `$this` ([`seed_this_object`]) and a
/// declared parameter ([`seed_declared_param_object`]), so the two can never
/// disagree about what a class guarantees. A hooked property (PHP 8.4) is never
/// readonly — readonly + hook is a PHP fatal — and holds no tracked value, so it
/// contributes nothing (FP class 16).
fn seed_readonly_bookkeeping(cx: &Cx, obj: &mut HeapObj, class_fqn: &str) {
    for p in cx.class_props(class_fqn) {
        if p.hooked || !p.readonly {
            continue;
        }
        obj.readonly.insert(p.name.clone());
        if p.promoted || p.default.is_some() {
            obj.ro_written.insert(p.name.clone());
        }
    }
}

/// The heap object a parameter that is an **object by declaration** contributes to
/// its scope's entry state (ADR-0032's 2026-08-16 amendment, issue #388) — the §3
/// clause the 2026-08-09 binding amendment wrote down and ADR-0086 §4 carried
/// forward as its one open entry.
///
/// **The declaration must state one class, and both halves must agree.** The native
/// half is exactly one non-nullable [`TypeMember::Instance`]; a union, an
/// intersection, a `?Box`, a `= null` default and a scalar each say something other
/// than "this parameter is one object of one class", and each declines rather than
/// seeding something weaker. The declared half is a `@param` spelled as a plain
/// class or a plain parameterized class; **any other `@param` declines the whole
/// seed**, because at an entry point the docblock is the strongest fact available
/// (ADR-0037) and one this reader cannot read is not evidence that the native hint
/// is the whole truth — `@param Box|null` and `@param T` both say the parameter is
/// not simply a `Box`. Where both halves are written they must resolve to the same
/// class; a disagreement declines, the two declarations contradicting each other in
/// a direction no rule here can adjudicate.
///
/// **The class comes from the native hint and from nothing else.** A `@param`
/// alone contributes no object, however plainly it names a class, and the reason is
/// that [`HeapObj::class`] carries no stratum: the field feeds the proof-layer
/// dispatch [`resolve_guarded`] performs (`type.argument-mismatch` on the
/// resolved method's parameters) and the dump surface's un-`(asserted)` rung, and a
/// docblock reaching either would be exactly the laundering ADR-0052 §3 keeps the
/// arm lane out of. The native hint is PHP's own runtime guarantee, so it premises
/// both honestly. What the `@param` contributes is the **type arguments**, which
/// the native syntax cannot spell and which only contract-layer readers consume —
/// which is the contribution ADR-0032 §3's clause actually names. Lifting the
/// restriction has a stated precondition: a provenance bit on the heap class, the
/// same field the ADR-0052 §3 final-`Member` unlock will want.
///
/// **The object is a lower bound and stays one**: `class_exact` is `false` (audit
/// G1 — the runtime object may be any descendant), `escaped` is `true` (the caller
/// holds it too), and there are **no props** — a declaration states that a
/// parameter is a `Box`, never what that `Box` holds. Exactness is not promoted for
/// a `final` declared class: that is the ADR-0052 §3 final-`Member` unlock, a
/// different slice.
///
/// [`resolve_guarded`]: crate::dispatch::resolve_guarded
pub(crate) fn seed_declared_param_object(
    cx: &Cx,
    p: &Param,
    phpdoc: Option<&PType>,
    shadow: &TemplateShadow,
) -> Option<HeapObj> {
    if p.by_ref || p.variadic || p.has_null_default {
        return None;
    }
    // The native half, and the only source of the class. A hint that lowered to
    // `None` — untyped, `mixed`, `object`, `iterable` — states no class; anything
    // else that is not a single non-nullable class states something other than one
    // object of one class. Both decline.
    let ty = p.ty.as_ref()?;
    let (false, [TypeMember::Instance { fqn: native, .. }]) = (ty.nullable, ty.members.as_slice())
    else {
        return None;
    };
    let native = class_key(native);
    // The declared half, as written: `Box` or `Box<int>` and nothing else.
    let declared: Option<(&str, &[steins_phpdoc::ast::GenericArg])> = match phpdoc.map(|t| &t.kind) {
        None => None,
        Some(PKind::Identifier(base)) => Some((base.as_str(), &[])),
        Some(PKind::Generic { base, args }) => Some((base.as_str(), args.as_slice())),
        Some(_) => return None,
    };
    // Where the `@param` also names a class the two must be the same one. A
    // disagreement declines: `@param Sub $b` under `Box $b` may be a refinement the
    // author knows or a docblock that drifted, and nothing here can tell which, so
    // neither half is trusted to stand alone.
    if declared
        .is_some_and(|(base, _)| class_key(&cx.resolve_pclass(cx.cur, p.span.start, base)) != native)
    {
        return None;
    }
    let class = native;
    if !cx.is_known_class(&class) {
        return None;
    }
    let mut obj = HeapObj::new(class.clone());
    obj.escaped = true;
    seed_readonly_bookkeeping(cx, &mut obj, &class);
    // The carries: the `@param`'s own type arguments, owner-keyed to the class that
    // declares the templates and resolved where they were written. `CArg::Ty` by
    // provenance, which is what makes them sweep-immune (issue #295): a declaration
    // does not stop being true because the body called a method.
    if let Some((_, args)) = declared
        && !args.is_empty()
        && args.iter().all(|a| declared_carry_arg_readable(&a.ty, shadow))
    {
        let written: Vec<PType> = args.iter().map(|a| a.ty.clone()).collect();
        obj.targs = cx
            .mint_declared_carry(&class, &written, (cx.cur, p.span.start))
            .into_iter()
            .collect();
    }
    Some(obj)
}

/// Whether one written type argument of a declared parameterization can be carried
/// (ADR-0032's 2026-08-16 amendment). All-or-nothing per carry, the same alignment
/// rule every other carry is built under: one unreadable argument drops the whole
/// edge rather than leaving a hole a positional read would index wrongly.
///
/// Two shapes are unreadable. A **template name** — `@param Box<T> $box` under the
/// declaration's own `@template T`, or a class-level one in a method docblock — says
/// the declaration does not know what sits there; lowering it would mint a class
/// named `T` and manufacture a `No` against every spelling (the hazard the #294
/// amendment names). And a spelling the contract vocabulary lowers to
/// [`ContractTy::Opaque`] carries no more than the absence of a carry would, while
/// costing a reader ([`carg_contract_ty`]) an `Opaque` arm it would splice into a
/// declared return.
///
/// [`carg_contract_ty`]: crate::generics::carg_contract_ty
fn declared_carry_arg_readable(ty: &PType, shadow: &TemplateShadow) -> bool {
    let mut mentions = Vec::new();
    mentioned_templates(ty, shadow, &mut mentions);
    mentions.is_empty() && steins_contract::lower(ty) != ContractTy::Opaque
}

/// The caller-side heap object an argument denotes, for the binding descent's
/// call-site heap entry (ADR-0086 §2). Two argument forms carry an object across:
/// a **variable** bound in the caller's [`Store::refs`] (objects live on the heap,
/// never in `env` — which is exactly why `resolve_literal_under` declines them),
/// and a **direct `new`** in argument position, which mints the object the
/// assignment form `$x = new C(...)` would have minted, against the caller's own
/// heap. Everything else — `clone`, an enum case, a property fetch, a nested call
/// returning an object — is out of the argument leg (ADR-0086 §4).
///
/// The `new` arm runs the site's **constructor summary** (ADR-0057 C7): argument
/// position is the one place where the lowering builds no `Callee::Construct` call,
/// so the minting site here is also the only site the walk can ride, and running it
/// here is what keeps `f(new C(1))` from being the one position that stays dark.
#[allow(clippy::too_many_arguments)]
pub(crate) fn argument_heap_object(
    cx: &Cx,
    folder: &mut dyn Folder,
    arg: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    span_start: u32,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<HeapObj> {
    if poisoned {
        return None;
    }
    match arg {
        ArgValue::Var(name) => store.obj_of(name).cloned(),
        ArgValue::New(class_ref, args, named) => {
            let class = cx.class_fqn(class_ref);
            Some(constructed_object(
                cx, folder, &class, args, named, env, store, span_start, descent, out,
            ))
        }
        _ => None,
    }
}

/// The object a `new Class(args)` expression in **argument** position yields
/// (ADR-0057's constructor-summary amendment): the fresh allocation, seeded into the
/// constructor descent as `$this`, replaced by the snapshot that descent's exits
/// agree on. Falls back to the declaration-only object under the ADR-0086 §4 lexical
/// gate wherever the descent declines (C6) — including the named-argument list,
/// which the positional-only descent refuses exactly as `f(x: 1)` is refused.
#[allow(clippy::too_many_arguments)]
pub(crate) fn constructed_object(
    cx: &Cx,
    folder: &mut dyn Folder,
    class: &str,
    args: &[ArgValue],
    named: &[NamedArg],
    env: &HashMap<String, Known>,
    store: &Store,
    span_start: u32,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> HeapObj {
    let floor = |folder: &mut dyn Folder| {
        new_heap_object(cx, folder, class, args, named, env, store, false, CtorDefaults::Lexical)
    };
    if !named.is_empty() {
        return floor(folder);
    }
    let seed = new_heap_object(cx, folder, class, args, named, env, store, false, CtorDefaults::All);
    let arg_refs: Vec<&ArgValue> = args.iter().collect();
    let summary = ctor_heap_summary(
        cx, folder, class, &seed, &arg_refs, span_start, env, store, descent, out,
    );
    match summary {
        Some(h) => h.obj,
        None => floor(folder),
    }
}

/// Run the constructor descent for one `new Class(args)` site and return the
/// snapshot its exits agree on (ADR-0057 C2/C3), or `None` on every decline of C6.
///
/// The shared half of the two seams C7 names: the `Callee::Construct` rung in the
/// statement walk, and the argument-position mint above. Neither ever runs for the
/// same site, so a `new` is walked exactly once.
#[allow(clippy::too_many_arguments)]
fn ctor_heap_summary(
    cx: &Cx,
    folder: &mut dyn Folder,
    class: &str,
    seed: &HeapObj,
    args: &[&ArgValue],
    span_start: u32,
    env: &HashMap<String, Known>,
    caller_store: &Store,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<HeapSummary> {
    // `Callee::Construct`'s own resolution (`resolve_call_target`): the constructor
    // the chain runs for this class, walked under `$this` proven exactly `class`.
    // No constructor, an abstract one, or a chain that leaves the project — decline.
    //
    // `enclosing_class` is `None`: the argument-position mint runs inside `descend`,
    // which knows the caller's file and not its class-like, so a **private**
    // constructor declines here where the statement rung would resolve it. Losing a
    // walk costs knowledge; the shape it loses (`f(new C())` on a private `C`) does
    // not compile.
    let target = resolve_exact(cx, class, "__construct", None, Some(class.to_owned()))?;
    let callee_scope =
        cx.method_scope(target.class_file, &target.declaring_class.fqn, &target.method.name)?;
    let summary = descend(
        cx,
        folder,
        &target.method.params,
        target.class_file,
        callee_scope,
        &format!("{}::{}", target.declaring_class.fqn, target.method.name),
        &format!("new {}", simple_class(class)),
        target.this_exact,
        Some(ThisSeed::Ctor(seed)),
        args,
        span_start,
        &[],
        env,
        caller_store,
        false,
        descent,
        out,
    )?;
    summary.this
}

/// The copy of a caller object that crosses the binding descent (ADR-0086 §2's
/// field table). `class`, `class_exact`, `readonly`, `ro_written` and `targs` cross
/// verbatim — a by-value call changes none of them, and exactness is **copied,
/// never promoted** (audit G1). Two fields are decided here:
///
/// * `escaped` is always `true`. The caller's object is marked escaped by this very
///   call (the statement walk's step 1a runs right after the descent), so a copy
///   claiming `false` would let an unknown call *inside* the callee skip the sweep
///   it owes.
/// * a **non-readonly** prop crosses only from an object the caller alone can reach
///   (`escaped == false`). Any other route — a static property, an array, a global,
///   another object's property — marks the source escaped at the moment it is taken,
///   and a write through that alias is invisible to the callee's copy. readonly props
///   cross regardless: the language guarantees no one rewrites them.
///
/// A prop whose fact the binding key cannot name does not cross at all, so the key
/// stays a faithful name for the entry state and the memo a pure function of it
/// (ADR-0048 §2). Strata cross with their facts — an `Asserted` prop stays
/// `Asserted` inside the callee (ADR-0052 amendment 1, no laundering).
pub(crate) fn copy_for_descent(src: &HeapObj) -> HeapObj {
    let mut copy = src.clone();
    copy.escaped = true;
    copy.props.retain(|name, p| {
        (!src.escaped || src.readonly.contains(name)) && key_prop_value(&p.fact).is_some()
    });
    copy
}

/// A property fact reduced to the [`BindingKey`]'s vocabulary, or `None` when the
/// key cannot name it (ADR-0086 §2). The `arg_of_fact_key` precedent, minus its
/// `Other` fallback: a capture may collapse to `Other` because the *capture itself*
/// still enters the callee's env, but a prop the key cannot spell must not cross at
/// all, or the memo would replay one entry state's summary for another.
///
/// [`BindingKey`]: crate::env::BindingKey
fn key_prop_value(fact: &Fact) -> Option<ArgValue> {
    match fact {
        Fact::Singleton(v) => match arg_of_val(v) {
            ArgValue::Other => None,
            a => Some(a),
        },
        _ => None,
    }
}

/// The canonical rendering of a seeded argument object for the [`BindingKey`]
/// (ADR-0086 §2): class, exactness, the sorted readonly bookkeeping, the sorted
/// `(prop, value, stratum)` list of the props that crossed, and the carries. The
/// memo replays a cached summary — and suppresses the re-emission that would come
/// with a re-walk (ADR-0075 §2.1) — only for an entry state this names exactly, so
/// `h(new Box(1))` can never answer for `h(new Box('s'))`. No [`AllocId`] enters it:
/// ids are walk-local and counter-derived (ADR-0048 §4).
///
/// [`BindingKey`]: crate::env::BindingKey
pub(crate) fn object_binding_key(obj: &HeapObj) -> String {
    let mut readonly: Vec<&str> = obj.readonly.iter().map(String::as_str).collect();
    readonly.sort_unstable();
    let mut written: Vec<&str> = obj.ro_written.iter().map(String::as_str).collect();
    written.sort_unstable();
    let mut props: Vec<String> = obj
        .props
        .iter()
        .filter_map(|(name, p)| {
            key_prop_value(&p.fact).map(|v| format!("{name}={v:?}/{:?}", p.stratum))
        })
        .collect();
    props.sort();
    let carries: Vec<String> = obj.targs.iter().map(carry_binding_key).collect();
    format!(
        "{}{} ro[{}] rw[{}] p[{}] t[{}]",
        obj.class,
        if obj.class_exact { "!" } else { "" },
        readonly.join(","),
        written.join(","),
        props.join(","),
        carries.join(","),
    )
}

/// One [`GenericCarry`] rendered for [`object_binding_key`]. The written `site` is
/// part of what the carry *means* (it resolves a [`CArg::Ty`]'s class names), so it
/// is named too — a finer key is never wrong, only less shared.
fn carry_binding_key(c: &GenericCarry) -> String {
    let args: Vec<String> = c
        .args
        .iter()
        .map(|a| match a {
            CArg::Val(v) => format!("v{}", cval_binding_key(v)),
            CArg::Ty(t) => format!("t{t:?}"),
        })
        .collect();
    format!("{}@{:?}<{}>", c.owner, c.site, args.join(","))
}

/// One [`CVal`] rendered for [`carry_binding_key`] — [`CVal`] carries no `Debug`
/// (it holds [`GenericCarry`], which is deliberately structural), so the rendering
/// walks it.
fn cval_binding_key(v: &CVal) -> String {
    match v {
        CVal::Scalar(a) => format!("{a:?}"),
        CVal::Array(items) => {
            let parts: Vec<String> =
                items.iter().map(|(k, v)| format!("{k:?}=>{}", cval_binding_key(v))).collect();
            format!("[{}]", parts.join(","))
        }
        CVal::Object(class, carries) => {
            let cs: Vec<String> = carries.iter().map(carry_binding_key).collect();
            format!("{class}{{{}}}", cs.join(","))
        }
        CVal::Resource => "resource".to_owned(),
    }
}

/// Whether a fact admits `null` — such a fact must never be *seeded* into a
/// property (ADR-0036): property reads bypass the guard-narrowing that would clear
/// a `!== null` check, so a seeded nullable/null property fact flowing into a
/// non-null sink is a false positive. Explicitly-written facts still flow (they are
/// sound within the linear trace); only construction-time seeding is filtered.
fn fact_is_nullish(f: &Fact) -> bool {
    match f {
        Fact::Singleton(v) => matches!(v, Val::Null),
        Fact::OneOf(vs) => vs.iter().any(|v| matches!(v, Val::Null)),
        // A union carries `null` beside its arms, never inside one.
        Fact::Union { nullable, .. } => *nullable,
        Fact::Refined { nullable, .. } | Fact::General { nullable, .. } => *nullable,
        // The array stratum (ADR-0062 `Fact::Shape`) has no property-seeding
        // consumer. Answering `true` keeps it out of the heap entirely, which is
        // the no-knowledge side of this filter.
        Fact::Shape { .. } => true,
    }
}

/// Apply a `$var->prop = <rvalue>` / `$this->prop = <rvalue>` property assignment
/// (ADR-0036): run the property checks (native `type.property-mismatch`, `@var`
/// `phpdoc.property-mismatch`, `readonly.reassigned`), then record the prop's new
/// fact in the heap. An unknown receiver (no tracked object) records nothing (but
/// an object rvalue still escapes — it is now reachable via the property).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_prop_assign(
    w: &WalkCx,
    folder: &mut dyn Folder,
    target_var: &str,
    prop: &str,
    value: &ArgValue,
    span_start: u32,
    guarded: bool,
    checks_enabled: bool,
    env: &HashMap<String, Known>,
    store: &mut Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if w.scope.poisoned {
        return;
    }
    // An object rvalue stored into a property escapes (now reachable via the prop).
    if let ArgValue::Var(src) = value
        && store.is_bound(src)
    {
        store.mark_escaped(src);
    }
    let Some(id) = store.id_of(target_var) else {
        return;
    };
    let class = store.heap.get(&id).expect("bound id present").class.clone();

    // Resolve the rvalue to a proven literal (for the native check) and a fact
    // (for storage + the abstract phpdoc check). The rvalue's trust stratum
    // (ADR-0052 §5) gates the proof-layer native check and is recorded on the prop.
    // Stratum rides with the resolution so an Asserted fold doesn't launder
    // (issue #127).
    let proven = cx.resolve_literal_strat_ex(value, env, false, folder, None, Some(&mut *out));
    let (proven_lit, rvalue_strat) = match &proven {
        Some((lit, strat)) => (Some(lit.clone()), *strat),
        None => (None, value_stratum(value, env, Some(&*store))),
    };
    let prop_fact_val: Option<Fact> = proven_lit.as_ref().and_then(|l| singleton_fact(l, cx.php_minor)).or_else(|| {
        match value {
            ArgValue::PropFetch { var: rv, prop: rp } => store.prop_fact(rv, rp).cloned(),
            _ => arg_abstract_fact(value, env, false).cloned(),
        }
    });

    // Locate the property declaration on the object's class surface (for its native
    // type and `@var` contract).
    let pdecl = cx.class_props(&class).into_iter().find(|p| p.name == prop && !p.is_static);

    // A hooked property (PHP 8.4 `get`/`set`) routes this write through arbitrary
    // user code: the stored value is whatever the `set` hook computes, not `value`,
    // so neither property-mismatch id is sound and no fact may be recorded (FP
    // class 16). `pdecl` covers the promoted-param spelling, whose declaration
    // survives lowering; a class-body hooked declaration does not, so its name is
    // asked for separately — which matters now that a constructor's writes leave the
    // walk (ADR-0057's constructor-summary amendment) and could carry an FP-16 fact
    // to the caller.
    let hooked = pdecl.is_some_and(|pd| pd.hooked) || cx.class_body_hooked(&class, prop);

    // 1. Native `type.property-mismatch` — a proven literal against a native prop
    // type. Skip promoted props (checked as constructor args; no double-report).
    let mut native_fired = false;
    if checks_enabled
        && !hooked
        && rvalue_strat == Stratum::Verified
        && let Some(pd) = pdecl
        && !pd.promoted
        && let Some(ty) = pd.ty.as_ref()
        && let Some(lit) = proven_lit.as_ref()
        && lit.is_literal()
        && is_type_error(cx, ty, lit)
    {
        let pos = cx.tree().position(span_start);
        let mode = if cx.strict() { "strict" } else { "coercive" };
        out.push(Diagnostic {
            id: PROP_MISMATCH_ID,
            facet: None,
            fix: None,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: format!(
                "Cannot assign {} to property {}::${} of type {} — proven TypeError ({} mode)",
                lit.render(), simple_class(&class), prop, ty.render(), mode,
            ),
        });
        native_fired = true;
    }

    // 2. phpdoc `@var` `phpdoc.property-mismatch` — a proven or abstract value that
    // provably does not inhabit the property's `@var` contract (definite No only).
    if checks_enabled
        && !hooked
        && !native_fired
        && let Some(pd) = pdecl
        && let Some(mut var_ty) = pd.docblock.as_deref().and_then(parse_var_type)
        && let Some((cfile, cdecl)) = cx.find_class(&class)
    {
        // Class-level `@template` names shadow same-named classes in this property's
        // `@var` type (issue #5) — a property is a member docblock too.
        neutralize_templates(&mut var_ty, &template_names_of(cdecl.docblock.as_deref()));
        let coff = pd.span.start;
        let violates = match proven_lit.as_ref().map(|l| CVal::Scalar(l.clone())) {
            Some(cv) if matches!(cv, CVal::Scalar(ref v) if v.is_literal()) => {
                accepts(cx, cfile, coff, &var_ty, &cv) == Certainty::No
            }
            _ => arg_abstract_fact(value, env, false).is_some_and(|fact| {
                let cty = steins_contract::lower(&var_ty);
                !contract_touches_class(&cty)
                    && steins_contract::admits_fact(&cty, fact) == Certainty::No
            }),
        };
        if violates {
            let rendered = proven_lit
                .as_ref()
                .map(ArgValue::render)
                .or_else(|| arg_abstract_fact(value, env, false).map(describe_fact))
                .unwrap_or_else(|| value.render());
            let pos = cx.tree().position(span_start);
            out.push(Diagnostic {
                id: PHPDOC_PROP_MISMATCH_ID,
                facet: None,
                fix: None,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: format!(
                    "value {rendered} assigned to property {}::${prop} violates declared @var {var_ty} — declared contract violation",
                    simple_class(&class),
                ),
            });
        }
    }

    // Whether the rvalue is an object handle (computed before the mutable borrow).
    let rval_is_object = matches!(value, ArgValue::Var(src) if store.refs.contains_key(src));

    // 3. `readonly.reassigned` — a second proven write to a readonly property on
    // this (unguarded) path. `guarded` (inside a branch) suppresses it: the second
    // write is not proven on every path (ADR-0036 conservative side).
    let obj = store.heap.get_mut(&id).expect("bound id present");
    let is_readonly = obj.readonly.contains(prop);
    if checks_enabled && is_readonly && obj.ro_written.contains(prop) && !guarded {
        let pos = cx.tree().position(span_start);
        out.push(Diagnostic {
            id: READONLY_REASSIGNED_ID,
            facet: None,
            fix: None,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: format!(
                "Cannot modify readonly property {}::${prop} — proven Error",
                simple_class(&class),
            ),
        });
    }

    // 4. Record the prop's new fact (or drop it when the rvalue is not representable
    // / is an object handle). Mark the readonly write for the reassign check.
    match prop_fact_val {
        // A hooked property never records a fact — its stored value is the `set`
        // hook's arbitrary-code result, not `value` (FP class 16).
        Some(fact) if !rval_is_object && !hooked => {
            // A typed slot stores what PHP's boundary conversion makes of the
            // rvalue, not the rvalue (issue #48). Mode-independently unanswerable
            // facts drop to Unknown (sound both ways).
            let stored = match pdecl.and_then(|pd| pd.ty.as_ref()) {
                Some(ty) => coerce_fact_to_native(ty, fact),
                None => Some(fact),
            };
            match stored {
                Some(fact) => {
                    obj.props.insert(prop.to_owned(), PropFact { fact, stratum: rvalue_strat });
                }
                None => {
                    obj.props.remove(prop);
                }
            }
        }
        _ => {
            obj.props.remove(prop);
        }
    }
    if is_readonly {
        obj.ro_written.insert(prop.to_owned());
    }
}

/// The simple (last-segment) class name of an FQN, for a diagnostic message.
pub(crate) fn simple_class(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

/// Parse the first `@var` tag's type out of a property docblock (ADR-0036), or
/// `None` when absent/unparseable — the property carries no phpdoc contract.
fn parse_var_type(docblock: &str) -> Option<PType> {
    for tag in scan_docblock(docblock) {
        if matches!(tag.kind, TagKind::Var) {
            return parse_tag_type(&tag.type_text);
        }
    }
    None
}

/// Build a [`ClosureVal`] from a lowered [`ClosureRef`] at its creation site,
/// snapshotting the by-value captures from the definition-site `env` (ADR-0033).
/// A capture with no proven scalar fact is omitted; a captured closure is not
/// re-snapshot (nested capture is not modeled). Each captured fact keeps its
/// trust stratum so descent seeding cannot launder Asserted into Verified
/// (issue #128).
pub(crate) fn build_closure_val(
    cx: &Cx,
    cref: &steins_syntax::ClosureRef,
    line: u32,
    env: &HashMap<String, Known>,
) -> Option<ClosureVal> {
    use steins_syntax::ClosureRef;
    match cref {
        ClosureRef::Anonymous { def_offset, captures } => {
            let mut snapshot: Vec<(String, Fact, Stratum)> = Vec::new();
            for name in captures {
                if let Some(k) = env.get(name)
                    && let Some(f) = &k.fact
                    // A `Fact::Shape` is deliberately NOT captured (ADR-0062 S3): the
                    // descent key collapses every non-`Singleton` fact to `Other`, so
                    // a captured shape carries no binding information.
                    && !matches!(f, Fact::Shape { .. })
                {
                    snapshot.push((name.clone(), f.clone(), k.stratum));
                }
            }
            Some(ClosureVal { target: ClosureTarget::Scope(*def_offset), captures: snapshot, def_line: line })
        }
        ClosureRef::FunctionName(nameref) => {
            let _ = cx;
            Some(ClosureVal { target: ClosureTarget::Named(nameref.clone()), captures: Vec::new(), def_line: line })
        }
    }
}
