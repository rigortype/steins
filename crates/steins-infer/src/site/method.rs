//! Method calls and `new`: which body, or which engine class, a site runs.
//!
//! The project half of the answer is a call edge ([`method_edge`],
//! [`resolve_new`]); the engine half is the class the chain leaves the project
//! at ([`engine_exit`]) and the rows [`super::engine`] reads for it. Both lanes
//! ask the same questions of the same chains, so the constructor a `new` runs
//! cannot differ between them (issues #804, #849).
//!
//! Where the answer is a gap, the kind names the cause (ADR-0099 §5.1), and it
//! names the same one for the same site in either lane: a receiver that names no
//! class is [`GapKind::UnknownClass`], a class whose whole chain the project holds
//! and none declares the method is [`GapKind::MethodNotFound`], a `$this` or
//! `self::` method a subclass may replace is [`GapKind::NonFinalThis`], a late-bound
//! `new static` or `static::m()` is [`GapKind::DynamicCallee`], and a method of an
//! engine class the catalog knows is a missing row on the lane's axis.

use std::collections::HashSet;

use steins_syntax::{EffectRecv, Param, StaticClass, Visibility};

use super::engine::{self, MethodRow};
use super::{GapKind, Hit, HitKind};
use crate::Sym;
use crate::cx::Cx;
use crate::dispatch::{Resolution, resolve_in_chain};

/// The project body a method call runs, or why none can be pinned: the
/// [`GapKind`] of the site (see the module doc).
///
/// `$this`, `self::` and a bound name the enclosing class only as a bound; a
/// subclass may stand in for it, so it reaches a method only where no subclass
/// can replace it (final, private or in a final class). `parent::` and `Foo::`
/// name their class exactly. A declared receiver (ADR-0067) names an abstraction,
/// never a body: the whole point is that dependency injection put an unknown
/// implementation behind it.
pub(super) fn method_edge(
    cx: &Cx,
    enclosing: Option<&str>,
    receiver: &EffectRecv,
    method: &str,
) -> Result<Sym, GapKind> {
    let (start, exact) = match receiver {
        EffectRecv::This | EffectRecv::SelfKw => {
            (enclosing.ok_or(GapKind::UnknownClass)?.to_owned(), false)
        }
        EffectRecv::Parent => {
            let own = enclosing.ok_or(GapKind::UnknownClass)?;
            (cx.parent_fqn(own).ok_or(GapKind::UnknownClass)?, true)
        }
        EffectRecv::ClassName(name) => (cx.class_fqn(name), true),
        EffectRecv::Var(_) | EffectRecv::PropRead(_) => return Err(GapKind::DeclaredReceiver),
    };
    let r = match resolve_in_chain(cx, &start, method) {
        Resolution::Found(r) => r,
        // Every class of the chain is the project's and none declares the method:
        // `__call` may answer, or the call is an `Error`.
        Resolution::NotFoundChainComplete => return Err(GapKind::MethodNotFound),
        Resolution::Unknown => return Err(GapKind::UnknownClass),
    };
    if r.method.visibility == Visibility::Private
        && !enclosing.is_some_and(|e| e.eq_ignore_ascii_case(&r.declaring_class.fqn))
    {
        return Err(GapKind::OpenMethod);
    }
    if !exact {
        let declaring_final = r.declaring_class.is_final;
        if !(r.method.is_final || r.method.visibility == Visibility::Private || declaring_final) {
            return Err(GapKind::NonFinalThis);
        }
    }
    Ok(Sym::Method(r.declaring_class.fqn.clone(), r.method.name.clone()))
}

/// What the constructor a `new` expression runs resolves to (issue #804).
///
/// The one answer the effects pass and the throw pass both read (issue #849),
/// so the two lanes cannot disagree about *which* constructor a `new` runs.
/// What that constructor does is each lane's own question, asked of its own
/// catalog row when the constructor is the engine's.
#[derive(Debug)]
pub(crate) enum NewTarget {
    /// A project constructor, declared on the class or inherited: an edge,
    /// exactly as a method call's resolved callee is.
    Edge(Sym),
    /// No constructor anywhere on a chain the project holds end to end:
    /// nothing runs, so nothing is contributed.
    Absent,
    /// The chain leaves the project at this engine class (named by its FQN), with no
    /// project class on the way able to hold a constructor: a lane answers
    /// from its own `__construct` row for it, and taints without one.
    Engine(String),
    /// Anything else — a class no file declares and the engine does not
    /// either, a trait that may supply the constructor, an abstract one,
    /// `static` in a class a subclass can extend with its own, `self` with no
    /// class in scope — marks the body non-exhaustive, for the cause it carries:
    /// [`GapKind::DynamicCallee`] for the late-bound `static` (the same kind
    /// `static::m()` has), [`GapKind::UnknownClass`] for the rest.
    Unknown(GapKind),
}

/// Resolve the constructor `new class(...)` runs in a unit whose enclosing
/// class is `enclosing` (issue #804).
///
/// `Foo`, `self` and `parent` name one class exactly. `static` is late-bound,
/// so it resolves only in a final class, or to a final constructor, which no
/// subclass can replace. In a trait, `self` and `parent` are the using class's,
/// which the trait body cannot name, so they stay unknown there.
pub(crate) fn resolve_new(cx: &Cx, enclosing: Option<&str>, class: &StaticClass) -> NewTarget {
    let Some((start, exact)) = new_start(cx, enclosing, class) else {
        return NewTarget::Unknown(GapKind::UnknownClass);
    };
    // A class a subclass can extend, named by `static`, runs the constructor of
    // whichever class the call is late-bound to: the callee is computed.
    let unresolved =
        NewTarget::Unknown(if exact { GapKind::UnknownClass } else { GapKind::DynamicCallee });
    match resolve_in_chain(cx, &start, "__construct") {
        Resolution::Found(r) if exact || r.method.is_final => {
            NewTarget::Edge(Sym::Method(r.declaring_class.fqn.clone(), r.method.name.clone()))
        }
        Resolution::NotFoundChainComplete if exact => NewTarget::Absent,
        Resolution::Unknown if exact => {
            engine_exit(cx, &start, "__construct").map_or(unresolved, NewTarget::Engine)
        }
        _ => unresolved,
    }
}

/// The class a `new` names and whether the object is exactly that class: `None`
/// for a `self`, `parent` or `static` with no class in scope.
fn new_start(cx: &Cx, enclosing: Option<&str>, class: &StaticClass) -> Option<(String, bool)> {
    let own = || enclosing.filter(|e| !cx.find_class(e).is_some_and(|(_, cd)| cd.is_trait));
    Some(match class {
        StaticClass::Named(name) => (cx.class_fqn(name), true),
        StaticClass::SelfKw => (own()?.to_owned(), true),
        StaticClass::Parent => (own().and_then(|e| cx.parent_fqn(e))?, true),
        StaticClass::Static => {
            let e = own()?;
            (e.to_owned(), cx.find_class(e).is_some_and(|(_, cd)| cd.is_final))
        }
    })
}

/// Whether a project class on the chain of the class a `new` names hooks a
/// property, for a `new` whose constructor is the engine's ([`NewTarget::Engine`]).
/// The engine's constructor writes the properties of its own class (`$message`,
/// `$code`, `$previous`), which a hook of the subclass intercepts: the site is a
/// [`GapKind::OperatorMagicProperty`] beside the constructor's row (ADR-0099
/// §4.2, issue #875). The class is exact there, so no subclass stands in for it.
pub(crate) fn new_hooks(cx: &Cx, enclosing: Option<&str>, class: &StaticClass) -> bool {
    new_start(cx, enclosing, class)
        .is_some_and(|(start, exact)| super::operator::engine_chain_hooks(cx, &start, exact))
}

/// Whether a `new` of a project class creates a `Throwable` whose chain hooks a
/// property. The engine sets `$file` and `$line` when it creates any exception or
/// error, before and whatever its constructor, so a hook on either runs at the
/// `new` even for an own empty constructor or a project parent's (witnessed);
/// [`new_hooks`] covers the `new` that reaches the engine's constructor itself.
pub(crate) fn throwable_creation_hooks(
    cx: &Cx,
    enclosing: Option<&str>,
    class: &StaticClass,
) -> bool {
    use crate::contract::IsA;
    new_start(cx, enclosing, class).is_some_and(|(start, exact)| {
        let throwable = cx.is_a(&start, "Throwable");
        // A hook on the named class's own chain counts unless the walk shows it is no
        // `Throwable`; one on a class that may stand in for a late-bound `static` only
        // when the walk shows the named class is one (an `Unknown` there would charge
        // every `new static` of a vendor-parented class).
        (throwable != IsA::No && super::operator::engine_chain_hooks(cx, &start, true))
            || (!exact
                && throwable == IsA::Yes
                && super::operator::engine_chain_hooks(cx, &start, false))
    })
}

/// Whether the object `parent::__construct(...)` runs the engine's constructor
/// on may hook a property: `$this`, which is the enclosing class or a subclass of
/// it, so the enclosing class's chain and every class that may stand in for it.
pub(crate) fn parent_constructor_hooks(cx: &Cx, enclosing: Option<&str>) -> bool {
    enclosing.is_some_and(|e| super::operator::engine_chain_hooks(cx, e, false))
}

/// The engine class `start`'s chain leaves the project at, when no project
/// class on the way can hold `method`: none declares it, and none uses a trait
/// that could supply it. The exit class must be absent from the project and
/// declared by the mined engine hierarchy under that FQN
/// ([`engine::declares_engine_class`]), the gates [`engine_method`] holds a
/// catalogued method to, for its reasons. `start` itself is the exit when no
/// project file declares it.
pub(crate) fn engine_exit(cx: &Cx, start: &str, method: &str) -> Option<String> {
    let mut cur = start.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None;
        }
        let Some((file, cd)) = cx.find_class(&cur) else { break };
        if cd.uses_traits || cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(method)) {
            return None;
        }
        cur = cx.units[file].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
    (engine::declares_engine_class(&cur) && cx.class_absent(&cur)).then_some(cur)
}

/// How a `new` expression's class reads in a finding: `new Clock`, `new static`.
pub(crate) fn new_origin(class: &StaticClass) -> String {
    let spelled = match class {
        StaticClass::Named(name) => name.simple(),
        StaticClass::SelfKw => "self",
        StaticClass::Static => "static",
        StaticClass::Parent => "parent",
    };
    format!("new {spelled}")
}

/// What the engine's catalog says of a method call whose receiver draws no
/// project edge.
pub(super) enum EngineMethod {
    /// The catalog's row for the method (issue #67); an empty one is a
    /// catalogued-pure method. `hooked` is whether a project class that may be the
    /// object the method runs on hooks a property, which the engine's code reads
    /// or writes through its hook (`getMessage()` reads `$message`): the call is
    /// a [`GapKind::OperatorMagicProperty`] beside the row (issue #875).
    Row { hit: Hit, hooked: bool },
    /// The chain leaves the project at an engine class that gives no row an
    /// answer for this receiver.
    Gap(GapKind),
    /// The call reaches no engine class.
    NotEngine,
}

/// The **builtin-class catalog** answer for a method-call site whose receiver
/// draws no project edge (issue #67): the row a catalogued engine method
/// contributes.
///
/// Three gates stand between a method call and a row, and each one is the
/// FP-safe side of a question the analyzer cannot otherwise answer:
///
/// * the receiver must **name a class**: `new PDO(...)->query()`, `PDO::…` and
///   `parent::…` name one exactly; `$this` and `self::` name the enclosing
///   class, and an ADR-0067 declared receiver the one class its native type
///   names, each only as a bound a subclass may stand in for. A `$pdo->query()`
///   on a variable the frame writes names no class (a dynamic site);
/// * the class's chain must **leave the project** at the class the row is keyed
///   by, with no project class on the way declaring the method or using a trait
///   that could ([`engine_exit`]). A project `PDO` shadows the catalog, because
///   its body is the truth and [`method_edge`] already drew that edge;
/// * the exit FQN must be **an engine class by that name** — the mined hierarchy
///   keys every engine class by its FQN (`PDO`, `Random\RandomException`), so an
///   unimported `PDO` inside `namespace App;` is `App\PDO`, which the hierarchy
///   does not declare: some class of the user's that Steins simply has not
///   indexed, and coloring it `io.db` would be the guess this analyzer does not
///   make.
///
/// A bound receiver reaches only a row whose method the engine declares final
/// ([`engine::method_effects`], issue #847): `$this->getTrace()` in a project
/// exception's constructor runs `Exception::getTrace` whatever subclass `$this`
/// is. `parent::__construct(...)` into an engine class runs the constructor `new`
/// would (issue #804).
pub(super) fn engine_method(
    cx: &Cx,
    enclosing: Option<&str>,
    params: &[Param],
    receiver: &EffectRecv,
    method: &str,
) -> EngineMethod {
    let bound_this = matches!(receiver, EffectRecv::This | EffectRecv::SelfKw);
    let Some((start, exact, origin)) = engine_start(cx, enclosing, params, receiver, method) else {
        return EngineMethod::NotEngine;
    };
    match engine_row(cx, &start, method, (exact, bound_this), origin) {
        EngineMethod::Row { hit, .. } => {
            let hooked = receiver_hooks(cx, enclosing, receiver, (&start, exact));
            EngineMethod::Row { hit, hooked }
        }
        other => other,
    }
}

/// Whether the object a method call runs the engine's code on may hook a property
/// ([`EngineMethod::Row`]): a hook on the chain of the class the receiver names, a
/// class that may stand in for a bound receiver, and, for `parent::m()` and
/// `Foo::m()`, which run on `$this` when the frame's class is one of them, the
/// frame's own class and its subclasses.
fn receiver_hooks(
    cx: &Cx,
    enclosing: Option<&str>,
    receiver: &EffectRecv,
    (start, exact): (&str, bool),
) -> bool {
    if super::operator::engine_chain_hooks(cx, start, exact) {
        return true;
    }
    let on_this = matches!(receiver, EffectRecv::Parent | EffectRecv::ClassName(_));
    on_this
        && enclosing.is_some_and(|own| {
            cx.is_a(own, start) != crate::contract::IsA::No
                && super::operator::engine_chain_hooks(cx, own, false)
        })
}

/// The engine class a method call's chain leaves the project at, for the throw
/// lane's gap kind: it reads no row, only which class the site names.
pub(super) fn engine_class_of(
    cx: &Cx,
    enclosing: Option<&str>,
    params: &[Param],
    receiver: &EffectRecv,
    method: &str,
) -> Option<String> {
    let (start, ..) = engine_start(cx, enclosing, params, receiver, method)?;
    engine_exit(cx, &start, method)
}

/// The class the chain starts at, whether the receiver names it exactly, and the
/// source spelling a finding names the call by.
fn engine_start(
    cx: &Cx,
    enclosing: Option<&str>,
    params: &[Param],
    receiver: &EffectRecv,
    method: &str,
) -> Option<(String, bool, String)> {
    Some(match receiver {
        EffectRecv::ClassName(name) => {
            (cx.class_fqn(name), true, format!("{}::{method}", name.simple()))
        }
        EffectRecv::Parent => (cx.parent_fqn(enclosing?)?, true, format!("parent::{method}")),
        EffectRecv::This => (enclosing?.to_owned(), false, format!("$this->{method}")),
        EffectRecv::SelfKw => (enclosing?.to_owned(), false, format!("self::{method}")),
        EffectRecv::Var(var) => {
            let fqn = declared_receiver_fqn(cx, enclosing, params, receiver)?;
            (fqn, false, format!("${var}->{method}"))
        }
        EffectRecv::PropRead(prop) => {
            let fqn = declared_receiver_fqn(cx, enclosing, params, receiver)?;
            (fqn, false, format!("$this->{prop}->{method}"))
        }
    })
}

/// The row a call of `method` on `start`'s chain answers from, when the chain
/// leaves the project at an engine class ([`engine_exit`]).
fn engine_row(
    cx: &Cx,
    start: &str,
    method: &str,
    (exact, bound_this): (bool, bool),
    origin: String,
) -> EngineMethod {
    let Some(fqn) = engine_exit(cx, start, method) else { return EngineMethod::NotEngine };
    match engine::method_effects(&fqn, method, exact) {
        MethodRow::Labels(labels) => EngineMethod::Row {
            hit: Hit {
                kind: HitKind::Method,
                callee: fqn,
                method: method.to_owned(),
                spelled: origin.clone(),
                origin,
                labels: labels.to_vec(),
                throws: &[],
            },
            hooked: false,
        },
        // A row the engine's class has, that a subclass may replace: `$this` and
        // `self::` are a non-final `$this` as for a project method, and any other
        // bound (a declared receiver) is an open method.
        MethodRow::Open if bound_this => EngineMethod::Gap(GapKind::NonFinalThis),
        MethodRow::Open => EngineMethod::Gap(GapKind::OpenMethod),
        MethodRow::Missing => EngineMethod::Gap(engine::missing_row(&fqn, GapKind::NoEffectRow)),
    }
}

/// The one class or interface an ADR-0067 declared receiver's native type
/// names ([`sole_object_fqn`]), or `None` for any other receiver or type.
pub(crate) fn declared_receiver_fqn(
    cx: &Cx,
    enclosing: Option<&str>,
    params: &[Param],
    receiver: &EffectRecv,
) -> Option<String> {
    sole_object_fqn(declared_receiver_type(cx, enclosing, params, receiver)?)
}

/// The native type an ADR-0067 declared receiver (`$r` a never-written
/// parameter, `$this->repo` a never-written property) is declared with, whole:
/// the operator resolver reads every member of a union, where a method call
/// needs the one class ([`declared_receiver_fqn`]). `None` for any other
/// receiver, and for a type the lowering does not model (`array`, `mixed`,
/// `iterable`, `object`: such a member collapses the whole hint).
pub(crate) fn declared_receiver_type<'a>(
    cx: &Cx<'a>,
    enclosing: Option<&str>,
    params: &'a [Param],
    receiver: &EffectRecv,
) -> Option<&'a steins_syntax::NativeType> {
    let ty = match receiver {
        // `f(Repo $r) { $r->find(); }` — the parameter's own declared type. The
        // syntax gate already proved this frame never writes `$r`, so the binding
        // still holds what the signature typed.
        EffectRecv::Var(name) => params.iter().find(|p| &p.name == name)?.ty.as_ref()?,
        // `$this->repo->find()` — the declared (or constructor-promoted) type of
        // the property, inherited members included.
        EffectRecv::PropRead(prop) => {
            cx.class_props(enclosing?).into_iter().find(|p| &p.name == prop)?.ty.as_ref()?
        }
        EffectRecv::This | EffectRecv::SelfKw | EffectRecv::Parent | EffectRecv::ClassName(_) => {
            return None;
        }
    };
    Some(ty)
}

/// The FQN of a declared type that names **exactly one** object type, or `None`
/// for a union, an intersection, or a scalar. A nullable single object type still
/// qualifies: `null` never reaches the method, so the interface's envelope still
/// bounds every call that actually happens.
fn sole_object_fqn(ty: &steins_syntax::NativeType) -> Option<String> {
    match ty.members.as_slice() {
        [steins_syntax::TypeMember::Instance { fqn, .. }] => Some(fqn.clone()),
        _ => None,
    }
}
