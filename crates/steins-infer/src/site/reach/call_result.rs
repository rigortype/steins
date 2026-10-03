//! What a call's result is shown to hold, read off the callee's declared return
//! (issue #877, ADR-0021's second amendment §4, ADR-0099 §6): the [`Held`] of an
//! [`ArgShape::Call`] or an [`ArgShape::MethodCall`].
//!
//! A declared return is a promise the callee's own return check enforces, so the
//! value a call yields holds at most what the type admits, whatever the arguments
//! are and whichever body runs. Three sources state one:
//!
//! * a **builtin function**'s mined row ([`steins_catalog::declared_return`]), a
//!   phpdoc-shaped spelling (`int<1, max>|0`, `list<string>`,
//!   `non-empty-string|false`) the contract lowering reads;
//! * a **project function** or **project method**'s native return hint, read as
//!   written like a parameter's ([`hint_held`]). A method on a receiver that only
//!   bounds the class needs no exactness: PHP refuses an override whose return type
//!   is not covariant, so the resolved declaration's hint binds every subclass
//!   (ADR-0049 A16, [`ChainMode::Declaration`]);
//! * an **engine method**'s mined row ([`builtin_method_row`]), for a receiver that
//!   names its class exactly or for a final `Throwable` accessor. A bound receiver
//!   of any other engine method stays unknown: the engine's return type there may
//!   be a *tentative* one, which a userland override is free to ignore
//!   (`Countable::count()`).
//!
//! The answer is never an object-holding value: a class, `object`, `mixed`,
//! `iterable`, `callable`, `static` or `self` return is [`Held::Unknown`], and so is
//! every call a gate below declines (a conditional declaration, a method the
//! enclosing scope cannot reach, a name PHP may bind to another function at run
//! time).

use std::collections::HashSet;

use steins_contract::ContractTy;
use steins_syntax::{EffectRecv, NameRef, Span, Visibility};

use super::{FloatClass, Frame, Held, hint_float_class, hint_held};
use crate::builtin_returns::{builtin_method_row, floor_target_admits};
use crate::contract::IsA;
use crate::cx::Cx;
use crate::dispatch::{ChainMode, Resolution, ResolvedMethod, resolve_in_chain_mode};
use crate::project::FnResolution;
use crate::site::engine_exit;
use crate::site::method::declared_receiver_fqn;
use crate::walk::namespace_binding_is_settled;

/// A declared return type, in the spelling its source gives it: what the gates below
/// let through, for a classifier to read ([`Self::held`], [`Self::float_class`]).
#[derive(Debug, Clone, Copy)]
enum Returned<'a> {
    /// A native hint, read from the source as written.
    Native(&'a str),
    /// A mined row's phpdoc-shaped spelling.
    Mined(&'a str),
}

impl Returned<'_> {
    /// What a value of this type holds, objects-wise.
    fn held(self) -> Held {
        match self {
            Self::Native(hint) => hint_held(hint),
            Self::Mined(declared) => declared_held(declared),
        }
    }

    /// Whether a value of this type is a float.
    fn float_class(self) -> FloatClass {
        match self {
            Self::Native(hint) => hint_float_class(hint),
            Self::Mined(declared) => steins_contract::lower_str(declared)
                .map_or(FloatClass::Unknown, |ty| contract_float_class(&ty)),
        }
    }
}

/// What the result of the plain call `name(...)` is shown to hold.
pub(super) fn function_result(cx: &Cx, name: &NameRef) -> Held {
    function_return(cx, name).map_or(Held::Unknown, Returned::held)
}

/// Whether the result of the plain call `name(...)` is a float.
pub(super) fn function_float_class(cx: &Cx, name: &NameRef) -> FloatClass {
    function_return(cx, name).map_or(FloatClass::Unknown, Returned::float_class)
}

/// The declared return of the plain call `name(...)`, where a gate lets one answer.
fn function_return<'a>(cx: &'a Cx<'_>, name: &NameRef) -> Option<Returned<'a>> {
    match cx.resolve_function(name) {
        FnResolution::Builtin(builtin) => {
            if !floor_target_admits(&builtin, cx.php_target) {
                return None;
            }
            steins_catalog::declared_return(&builtin).map(Returned::Mined)
        }
        FnResolution::User(site) => {
            let decl = cx.fn_decl(site);
            // A conditional declaration (`function_exists`-guarded) binds by load
            // order, and an unqualified name inside a namespace that only matched
            // the global function binds to a namespaced one if anything defines it.
            if decl.conditional || !namespace_binding_is_settled(cx, name, &decl.fqn) {
                return None;
            }
            native_return(cx, site.file, decl.ret_span)
        }
        FnResolution::Unknown => None,
    }
}

/// What the result of a method or static call `method` on `receiver`, written in
/// `frame`, is shown to hold.
pub(super) fn method_result(
    cx: &Cx,
    frame: &Frame,
    receiver: &EffectRecv,
    method: &str,
) -> Held {
    method_return(cx, frame, receiver, method, Reading::Objects)
        .map_or(Held::Unknown, Returned::held)
}

/// Whether the result of a method or static call `method` on `receiver`, written in
/// `frame`, is a float.
pub(super) fn method_float_class(
    cx: &Cx,
    frame: &Frame,
    receiver: &EffectRecv,
    method: &str,
) -> FloatClass {
    method_return(cx, frame, receiver, method, Reading::Float)
        .map_or(FloatClass::Unknown, Returned::float_class)
}

/// What a declared return is read for: whether the value holds an object
/// ([`Reading::Objects`]) or is a float ([`Reading::Float`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    Objects,
    Float,
}

/// The declared return of a method or static call, where a gate lets one answer.
fn method_return<'a>(
    cx: &'a Cx<'_>,
    frame: &Frame,
    receiver: &EffectRecv,
    method: &str,
    reading: Reading,
) -> Option<Returned<'a>> {
    let (start, exact) = receiver_start(cx, frame, receiver)?;
    match resolve_in_chain_mode(cx, &start, method, ChainMode::Declaration) {
        Resolution::Found(found) => project_method_return(cx, frame, &start, &found),
        // `__call` may answer a name no class of a wholly project chain declares.
        Resolution::NotFoundChainComplete => None,
        Resolution::Unknown => engine_method_return(cx, frame, (&start, exact), method, reading),
    }
}

/// The class a receiver's method resolution starts at, and whether the object is
/// exactly that class (no subclass can stand in): `new Foo`, `Foo::` and `parent::`
/// name theirs exactly; `$this`, `self::` and a declared receiver bound it. `None`
/// for a trait's `$this` (the using class's, which the trait cannot name), or a
/// receiver no class is read for.
fn receiver_start(cx: &Cx, frame: &Frame, receiver: &EffectRecv) -> Option<(String, bool)> {
    let own = || frame.class_fqn.filter(|e| !cx.find_class(e).is_some_and(|(_, cd)| cd.is_trait));
    Some(match receiver {
        EffectRecv::ClassName(name) => (cx.class_fqn(name), true),
        EffectRecv::Parent => (cx.parent_fqn(own()?)?, true),
        EffectRecv::This | EffectRecv::SelfKw => (own()?.to_owned(), false),
        EffectRecv::Var(_) | EffectRecv::PropRead(_) => {
            let fqn = declared_receiver_fqn(cx, frame.class_fqn, frame.params, receiver)?;
            (fqn, false)
        }
        EffectRecv::Bound(_) | EffectRecv::StaticKw => return None,
    })
}

/// The native return hint of a project method found on `start`'s chain, where the
/// call can reach it: a conditional class binds by load order, and a method the
/// enclosing scope cannot see is a call the engine hands to `__call`.
fn project_method_return<'a>(
    cx: &'a Cx<'_>,
    frame: &Frame,
    start: &str,
    found: &ResolvedMethod,
) -> Option<Returned<'a>> {
    let declaring = found.declaring_class;
    // A conditional class binds by load order, and so does the chain it sits on: any
    // class from the receiver's up to the declaring one may be a different declaration.
    let mut cur = start.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None;
        }
        match cx.find_class(&cur) {
            Some((_, cd)) if !cd.conditional => {}
            _ => return None,
        }
        if cur.eq_ignore_ascii_case(&declaring.fqn) {
            break;
        }
        cur = cx.parent_fqn(&cur)?;
    }
    let reachable = match found.method.visibility {
        Visibility::Public => true,
        Visibility::Protected => {
            frame.class_fqn.is_some_and(|own| cx.is_a(own, &declaring.fqn) == IsA::Yes)
        }
        Visibility::Private => {
            frame.class_fqn.is_some_and(|own| own.eq_ignore_ascii_case(&declaring.fqn))
        }
    };
    let (file, _) = cx.find_class(&declaring.fqn)?;
    if reachable { native_return(cx, file, found.method.ret_span) } else { None }
}

/// The mined return row of an engine method, for the engine class `start`'s chain
/// leaves the project at ([`engine_exit`]): an exact receiver reads the row of the
/// method it runs; a bound one only that of a final `Throwable` accessor, which no
/// subclass replaces.
///
/// The accessor gates below are about **objects**: an accessor whose property may hold one
/// runs its `__toString` inside the accessor. They say nothing of whether the result is a
/// float, which the declared return (`string`, `int`) enforces whatever the property held, so a
/// [`Reading::Float`] skips them.
fn engine_method_return<'a>(
    cx: &'a Cx<'_>,
    frame: &Frame,
    (start, exact): (&str, bool),
    method: &str,
    reading: Reading,
) -> Option<Returned<'a>> {
    let exit = engine_exit(cx, start, method)?;
    // `getMessage()` and `getCode()` read an untyped property (`protected $message`,
    // `protected $code`) a subclass may fill with an object, which the return then
    // converts (`__toString` runs in the accessor) or hands back: no receiver is shown
    // to hold a string, `parent::` and `Foo::` run on `$this` and `new` is not told
    // apart from them. `getFile()` and `getLine()` read typed properties, which hold a string
    // or an int unless a subclass unsets one and declares `__get`: the engine then reads the
    // property through it (`unset($this->file)`, `__get` returning an object whose `__toString`
    // runs in the accessor, witnessed on PHP 8.5.11), so they are read only where no such
    // subclass can exist (see [`property_read_is_direct`]). `getTrace()` and
    // `getTraceAsString()` read a private typed property no subclass reaches.
    let accessor = method.to_ascii_lowercase();
    if reading == Reading::Objects && ["getmessage", "getcode"].contains(&accessor.as_str()) {
        return None;
    }
    if reading == Reading::Objects
        && ["getfile", "getline"].contains(&accessor.as_str())
        && !property_read_is_direct(cx, frame, (start, exact))
    {
        return None;
    }
    if !exact && steins_catalog::final_method_effect_labels(&exit, method).is_none() {
        return None;
    }
    builtin_method_row(&exit, method, cx.php_target).map(|(declared, _)| Returned::Mined(declared))
}

/// Whether an engine accessor called on an object of `start`'s class reads its property
/// directly, where no class can declare `__get` for it to fall back on once a subclass
/// `unset`s the property. No project class on the chain from `start` declares `__get`, and
/// no subclass can: the class is final, or the receiver is exactly `start` and the enclosing
/// class is no instance of it (`parent::` and `Foo::` run on `$this`, which a subclass of the
/// enclosing class may be).
fn property_read_is_direct(cx: &Cx, frame: &Frame, (start, exact): (&str, bool)) -> bool {
    let mut cur = start.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    while seen.insert(cur.to_ascii_lowercase()) {
        let Some((file, cd)) = cx.find_class(&cur) else { break };
        if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case("__get")) {
            return false;
        }
        match &cd.parent {
            Some(parent) => cur = cx.units[file].tree.resolve_class_fqn(parent),
            None => break,
        }
    }
    cx.class_has_no_subclass(start)
        || (exact && frame.class_fqn.is_none_or(|own| cx.is_a(own, start) == IsA::No))
}

/// The native type written at `ret` in `file`.
fn native_return<'a>(cx: &'a Cx<'_>, file: usize, ret: Option<Span>) -> Option<Returned<'a>> {
    ret.and_then(|span| cx.units[file].tree.source_slice(span)).map(Returned::Native)
}

/// What a value of the phpdoc-shaped type `declared` (a mined row) holds.
fn declared_held(declared: &str) -> Held {
    steins_contract::lower_str(declared).map_or(Held::Unknown, |ty| contract_held(&ty))
}

/// What a value of the lowered type `ty` holds: scalars and `null` no object at any
/// depth, an array no object itself and none at all when its keys and values hold
/// none, a union what its weakest member holds, an intersection what its strongest
/// does (the value is in every member). Whatever can be an object, or is not
/// modeled (`mixed`, a class, `object`, `iterable`, `callable`, a resource, `static`),
/// is [`Held::Unknown`].
fn contract_held(ty: &ContractTy) -> Held {
    match ty {
        ContractTy::Null
        | ContractTy::Never
        | ContractTy::Base(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::StrOpaque
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_) => Held::ObjectFree,
        ContractTy::ArrayAny { .. } => Held::NonObject,
        ContractTy::ListOf { elem, .. } => elements_held([&**elem]),
        ContractTy::MapOf { key, val, .. } => elements_held([&**key, &**val]),
        // A shape that is not sealed and types no tail admits extra keys of any type.
        ContractTy::Shape { sealed: false, unsealed: None, .. } => Held::NonObject,
        ContractTy::Shape { fields, unsealed, .. } => {
            let tail =
                unsealed.iter().flat_map(|(key, val)| key.as_deref().into_iter().chain([&**val]));
            elements_held(fields.iter().map(|field| &field.ty).chain(tail))
        }
        ContractTy::Union(members) => {
            members.iter().map(contract_held).min().unwrap_or(Held::Unknown)
        }
        ContractTy::Inter(members) => {
            members.iter().map(contract_held).max().unwrap_or(Held::Unknown)
        }
        _ => Held::Unknown,
    }
}

/// Whether a value of the lowered type `ty` is a float: [`FloatClass::Yes`] for `float` and a
/// float literal alone, [`FloatClass::No`] where every member of a union, or some member of an
/// intersection, is an integer, string, boolean, `null`, an array or an object type, and
/// [`FloatClass::Unknown`] for `mixed`, a union that mixes the two, and any type not modeled.
fn contract_float_class(ty: &ContractTy) -> FloatClass {
    match ty {
        ContractTy::Null
        | ContractTy::Never
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::StrOpaque
        | ContractTy::LitInt(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_)
        | ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::Shape { .. }
        | ContractTy::Class(_)
        | ContractTy::ObjectAny => FloatClass::No,
        ContractTy::Base(steins_domain::Base::Float) | ContractTy::LitFloat(_) => FloatClass::Yes,
        ContractTy::Base(_) => FloatClass::No,
        ContractTy::Union(members) if !members.is_empty() => {
            FloatClass::agree(members.iter().map(contract_float_class))
        }
        ContractTy::Inter(members)
            if members.iter().any(|m| contract_float_class(m) == FloatClass::No) =>
        {
            FloatClass::No
        }
        _ => FloatClass::Unknown,
    }
}

/// What an array holds, given the types of its keys and values: no object at any
/// depth when every one holds none, and otherwise an array that may hold one.
fn elements_held<'a>(parts: impl IntoIterator<Item = &'a ContractTy>) -> Held {
    if parts.into_iter().all(|part| contract_held(part) == Held::ObjectFree) {
        Held::ObjectFree
    } else {
        Held::NonObject
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spellings the catalog mines (issue #877): the classifier reads the
    /// phpdoc forms, not only native hints.
    #[test]
    fn a_mined_spelling_is_read_by_what_it_admits() {
        let held = |spelling: &str| declared_held(spelling);
        for scalar in [
            "int<1, max>|0",
            "uppercase-string",
            "non-empty-string|false",
            "string|null",
            "bool",
            "float",
            "numeric-string",
            "int-mask<1, 2, 4>",
            "'->'|'::'",
            "never",
        ] {
            assert_eq!(held(scalar), Held::ObjectFree, "{scalar}");
        }
        for list in ["list<string>", "list<int|string>", "array<string, int>", "string[]"] {
            assert_eq!(held(list), Held::ObjectFree, "{list}");
        }
        // An array that may hold an object is an array, not an object-free one.
        for array in ["array", "array<int, object>", "list<mixed>", "array<string, Foo>"] {
            assert_eq!(held(array), Held::NonObject, "{array}");
        }
        assert_eq!(held("iterable|array"), Held::Unknown);
        assert_eq!(held("string|array"), Held::NonObject);
        assert_eq!(held("string|null|array"), Held::NonObject);
        assert_eq!(held("array{a: int, b: string}"), Held::ObjectFree);
        assert_eq!(held("array{a: int, b: object}"), Held::NonObject);
        // An unsealed shape with no typed tail admits extra keys of any type.
        assert_eq!(held("array{a: int, ...}"), Held::NonObject);
        assert_eq!(held("list{int, ...}"), Held::NonObject);
        assert_eq!(held("array{a: int, ...<string, int>}"), Held::ObjectFree);
        for object in [
            "mixed", "GMP", "object", "\\Foo", "static", "self", "$this", "callable", "resource",
            "iterable", "string|Foo", "int|object", "?Closure", "string|false|DateTime",
        ] {
            assert_eq!(held(object), Held::Unknown, "{object}");
        }
        // A spelling the parser cannot read is no answer.
        assert_eq!(held("list<"), Held::Unknown);
    }
}
