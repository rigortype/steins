//! Whether a call to a builtin can run **user code through its arguments**
//! (issue #856, ADR-0021's second 2026-10-01 amendment).
//!
//! A builtin the catalog colours pure can still hand control to userland
//! through what it was given: a coercive `string` parameter converts an object
//! through `__toString`, `count()` calls `Countable::count`, `in_array()`
//! compares loosely, `json_encode()` calls `jsonSerialize()`. The catalog says
//! which argument reaches which ([`steins_catalog::arg_reach`]); this module
//! reads the call site's [`ArgShape`]s against it and answers whether every
//! reach is ruled out, either by what the argument is shown to hold
//! ([`Held`]) or, for a coerced `string` parameter only, by the calling file's
//! `declare(strict_types=1)`.

use std::cell::OnceCell;
use std::collections::HashSet;

use steins_catalog::ArgReach;
use steins_syntax::{
    ArgShape, EffectRecv, NameRef, Param, SiteKind, SiteOrigin, StaticClass, Stored, Visibility,
};

use super::{NewTarget, Reach, engine_exit, resolve_new};
use crate::Sym;
use crate::cx::Cx;
use crate::dispatch::{ChainMode, Resolution, resolve_in_chain_mode};
use crate::project::FnResolution;

/// What an argument is shown to hold, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Held {
    /// Nothing is known: it may be an object.
    Unknown,
    /// Not an object itself, though an array may hold one: a parameter
    /// declared `array`.
    NonObject,
    /// No object at any depth: a scalar, or an array of scalars.
    ObjectFree,
}

/// The calling frame a call's argument shapes are read against: the parameter
/// list an [`ArgShape::Param`] names and every site of the frame, whose named
/// calls decide whether a variable can be rebound through a reference. A region
/// classified on its own (ADR-0076) still names its whole frame here, since a
/// call outside the region can rebind a parameter the region reads.
pub(crate) struct Frame<'a> {
    pub(crate) class_fqn: Option<&'a str>,
    pub(crate) params: &'a [Param],
    pub(crate) sites: &'a [SiteOrigin],
    /// The variables some named call of the frame may take by reference,
    /// computed on the first variable argument that asks.
    by_ref: OnceCell<HashSet<String>>,
}

impl<'a> Frame<'a> {
    pub(crate) fn new(
        class_fqn: Option<&'a str>,
        params: &'a [Param],
        sites: &'a [SiteOrigin],
    ) -> Self {
        Self { class_fqn, params, sites, by_ref: OnceCell::new() }
    }

    /// What the argument `shape` describes is shown to hold.
    pub(crate) fn held(&self, cx: &Cx, shape: &ArgShape) -> Held {
        match shape {
            ArgShape::ObjectFree => Held::ObjectFree,
            ArgShape::Array => Held::NonObject,
            ArgShape::Param { name, stores } => {
                let param = self.params.iter().find(|p| &p.name == name);
                let declared = param.map_or(Held::Unknown, |p| param_held(cx, p));
                self.variable_held(cx, name, *stores).min(declared)
            }
            ArgShape::Local { name, stores } => self.variable_held(cx, name, *stores),
            ArgShape::ThisProperty(name) => this_property_held(cx, self.class_fqn, name),
            ArgShape::Unknown => Held::Unknown,
        }
    }
}

impl Frame<'_> {
    /// What a variable every write of the frame stores `stores` into holds,
    /// unless a named call of the frame may take it by reference.
    fn variable_held(&self, cx: &Cx, name: &str, stores: Stored) -> Held {
        let by_ref = self.by_ref.get_or_init(|| passed_by_ref(cx, self.class_fqn, self.sites));
        if by_ref.contains(name) {
            return Held::Unknown;
        }
        match stores {
            Stored::ObjectFree => Held::ObjectFree,
            Stored::Array => Held::NonObject,
        }
    }
}

/// The variables a named call of the frame passes bare at a position its
/// callee may take by reference. The syntax layer already counted every other
/// write the frame makes ([`ArgShape::Param`], [`ArgShape::Local`]); this is
/// the half only callee resolution can answer. A builtin answers through the
/// catalog's by-value certification, a project function through its own
/// parameter list, and a name neither resolves counts as by reference.
fn passed_by_ref(cx: &Cx, class_fqn: Option<&str>, sites: &[SiteOrigin]) -> HashSet<String> {
    let mut out = HashSet::new();
    for site in sites {
        let (callee, shapes) = match (&site.kind, site.operands.as_deref()) {
            (SiteKind::Call { name, callbacks }, shapes) if callbacks.is_empty() => {
                let Some(shapes) = shapes else { continue };
                (Callee::Function(name), shapes)
            }
            // The scan forms a higher-order call from an all-positional argument
            // list only, so its shapes are there; an absent list reads as empty.
            (SiteKind::Call { name, .. }, shapes) => {
                (Callee::Function(name), shapes.unwrap_or(&[]))
            }
            (SiteKind::MethodCall { receiver, method }, Some(shapes)) => {
                (Callee::Method(receiver, method), shapes)
            }
            (SiteKind::New { class }, Some(shapes)) => (Callee::New(class), shapes),
            _ => continue,
        };
        let mut params: Option<Option<&[Param]>> = None;
        for (position, shape) in shapes.iter().enumerate() {
            let var = match shape {
                ArgShape::Param { name, .. } | ArgShape::Local { name, .. } => name,
                _ => continue,
            };
            let by_value = match &callee {
                Callee::Function(name) => function_by_value(cx, name, position),
                _ => match params.get_or_insert_with(|| callee_params(cx, class_fqn, &callee)) {
                    Some(list) => position_by_value(list, position),
                    None => engine_by_value(cx, class_fqn, &callee),
                },
            };
            if !by_value {
                out.insert(var.clone());
            }
        }
    }
    out
}

/// The callee of a call whose arguments [`passed_by_ref`] reads.
enum Callee<'a> {
    Function(&'a NameRef),
    Method(&'a EffectRecv, &'a str),
    New(&'a StaticClass),
}

/// Whether the function `name` takes its argument at `position` by value.
fn function_by_value(cx: &Cx, name: &NameRef, position: usize) -> bool {
    match cx.resolve_function(name) {
        FnResolution::Builtin(builtin) => {
            steins_catalog::by_value_arg(&builtin, position) == Some(true)
        }
        FnResolution::User(site) => position_by_value(&cx.fn_decl(site).params, position),
        FnResolution::Unknown => false,
    }
}

/// Whether a project parameter list takes the argument at `position` by
/// value. An argument past a non-variadic list binds nothing.
fn position_by_value(params: &[Param], position: usize) -> bool {
    params.get(position).or_else(|| params.last().filter(|p| p.variadic)).is_none_or(|p| !p.by_ref)
}

/// The parameter list of the project method or constructor a call runs, or
/// `None` when no project declaration answers. A method's declaration binds
/// every override's by-reference flags, which PHP checks at declaration
/// time, so the declaration the chain finds answers for `$this->` and
/// `self::` too. A constructor is exempt from that check, so it answers only
/// where the class is exact, as [`resolve_new`] decides.
fn callee_params<'a>(cx: &Cx<'a>, class_fqn: Option<&str>, callee: &Callee) -> Option<&'a [Param]> {
    let method = match callee {
        Callee::New(class) => match resolve_new(cx, class_fqn, class) {
            NewTarget::Edge(Sym::Method(class, ctor)) => return method_params(cx, &class, &ctor),
            NewTarget::Absent => return Some(&[]),
            _ => return None,
        },
        Callee::Method(_, method) => *method,
        Callee::Function(_) => return None,
    };
    let start = method_start(cx, class_fqn, callee)?;
    match resolve_in_chain_mode(cx, &start, method, ChainMode::Declaration) {
        Resolution::Found(r) => Some(&r.method.params),
        Resolution::NotFoundChainComplete | Resolution::Unknown => None,
    }
}

/// The class a method call's resolution starts at, or `None` for a receiver
/// that names no class here, or a constructor reached through `$this->`,
/// whose runtime class may declare its own.
fn method_start(cx: &Cx, class_fqn: Option<&str>, callee: &Callee) -> Option<String> {
    let Callee::Method(receiver, method) = callee else { return None };
    match receiver {
        EffectRecv::This | EffectRecv::SelfKw if method.eq_ignore_ascii_case("__construct") => None,
        EffectRecv::This | EffectRecv::SelfKw => class_fqn.map(str::to_owned),
        EffectRecv::Parent => cx.parent_fqn(class_fqn?),
        EffectRecv::ClassName(name) => Some(cx.class_fqn(name)),
        EffectRecv::Var(_) | EffectRecv::PropRead(_) => None,
    }
}

/// The parameters of the project method `class::method`.
fn method_params<'a>(cx: &Cx<'a>, class: &str, method: &str) -> Option<&'a [Param]> {
    let (_, decl) = cx.find_class(class)?;
    decl.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)).map(|m| m.params.as_slice())
}

/// Whether a call no project declaration answers runs an engine constructor or
/// method that takes every argument by value: `parent::__construct()` or `new`
/// reaching an engine exception ([`steins_catalog::engine_constructor_by_value`]),
/// or a method the catalog rows for its arguments ([`steins_catalog::method_arg_reach`]),
/// none of which takes a parameter by reference (checked with
/// `ReflectionParameter::isPassedByReference` on PHP 8.5.11), so a variable
/// handed to `PDO::query()` or `DateTime::createFromFormat()` is not rebound.
fn engine_by_value(cx: &Cx, class_fqn: Option<&str>, callee: &Callee) -> bool {
    let start = match callee {
        Callee::New(class) => match resolve_new(cx, class_fqn, class) {
            NewTarget::Engine(fqn) => fqn,
            _ => return false,
        },
        Callee::Method(EffectRecv::Parent | EffectRecv::ClassName(_), method)
            if method.eq_ignore_ascii_case("__construct") =>
        {
            match method_start(cx, class_fqn, callee).and_then(|s| engine_exit(cx, &s, method)) {
                Some(fqn) => fqn,
                None => return false,
            }
        }
        Callee::Method(_, method) => {
            return method_start(cx, class_fqn, callee)
                .and_then(|start| engine_exit(cx, &start, method))
                .is_some_and(|fqn| steins_catalog::method_arg_reach(&fqn, method).is_some());
        }
        _ => return false,
    };
    steins_catalog::engine_constructor_by_value(&start)
}

/// What a parameter the frame never rebinds holds: whatever its native type
/// admits. The hint is read as written, since the lowered type has no
/// spelling for `array`; any member other than a scalar, `array`, `null`,
/// `false` or `true` (a class, `object`, `iterable`, `callable`, `mixed`,
/// `self`, an intersection) admits an object. An untyped parameter holds
/// anything.
fn param_held(cx: &Cx, param: &Param) -> Held {
    param.hint_span.and_then(|span| cx.tree().source_slice(span)).map_or(Held::Unknown, hint_held)
}

/// What a value of the native type spelled `hint` holds ([`param_held`]).
fn hint_held(hint: &str) -> Held {
    let hint = hint.trim();
    let hint = hint.strip_prefix('?').unwrap_or(hint);
    let mut held = Held::ObjectFree;
    for member in hint.split('|').map(str::trim) {
        match member.to_ascii_lowercase().as_str() {
            "int" | "float" | "string" | "bool" | "null" | "false" | "true" => {}
            "array" => held = Held::NonObject,
            _ => return Held::Unknown,
        }
    }
    held
}

/// What `$this->name` holds: the declared type of the property the frame's
/// class reaches by that name, read like a parameter's. A property found in
/// the class or an ancestor answers, except a private one of an ancestor,
/// which `$this->name` does not reach from here (the read is of a dynamic or
/// magic property instead), and a hooked one, whose hook is arbitrary user
/// code. A property no class of the chain declares holds anything.
fn this_property_held(cx: &Cx, class_fqn: Option<&str>, name: &str) -> Held {
    let Some(start) = class_fqn else { return Held::Unknown };
    let mut cur = start.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    while seen.insert(cur.to_ascii_lowercase()) {
        let Some((file, class)) = cx.find_class(&cur) else { break };
        if class.hooked_properties.iter().any(|p| p == name) {
            break;
        }
        if let Some(prop) = class.properties.iter().find(|p| p.name == name && !p.is_static) {
            let private = prop.visibility == Visibility::Private;
            if (private && !cur.eq_ignore_ascii_case(start)) || prop.hooked {
                break;
            }
            let hint = prop.hint_span.and_then(|span| cx.units[file].tree.source_slice(span));
            return hint.map_or(Held::Unknown, hint_held);
        }
        match &class.parent {
            Some(parent) => cur = cx.units[file].tree.resolve_class_fqn(parent),
            None => break,
        }
    }
    Held::Unknown
}

/// Whether a call to the builtin `name` with these argument shapes, written in
/// a file whose strictness is `strict`, may run user code through an
/// argument ([`steins_catalog::arg_reach`]). `shapes` is `None` where the
/// positions cannot be read: a named or spread argument list, or a builtin
/// handed to another as a callback, which calls it with arguments of its own
/// choosing and in coercive mode whatever the calling file declares (PHP
/// 8.5.11: `array_map('strlen', [$o])` runs `__toString` under
/// `strict_types=1`). `handled` lists the positions the caller has already
/// answered for, an invoker's resolved callback.
pub(crate) fn reaches_user_code(
    cx: &Cx,
    frame: &Frame,
    name: &str,
    shapes: Option<&[ArgShape]>,
    strict: bool,
    handled: &[usize],
) -> bool {
    let Some(row) = steins_catalog::arg_reach(name) else { return true };
    let Some(shapes) = shapes else { return row.reaches_blind(strict) };
    operands_reach(cx, frame, shapes, strict, handled, |position| row.at(position))
}

/// Whether some operand of a call reaches user code, given what each position
/// does with its argument (`at`): the one rule a builtin function's row
/// ([`reaches_user_code`]) and an engine method's ([`method_reaches_user_code`])
/// are both held to. A position in `handled` is answered for already.
fn operands_reach(
    cx: &Cx,
    frame: &Frame,
    shapes: &[ArgShape],
    strict: bool,
    handled: &[usize],
    at: impl Fn(usize) -> ArgReach,
) -> bool {
    shapes.iter().enumerate().filter(|(position, _)| !handled.contains(position)).any(
        |(position, shape)| match at(position) {
            ArgReach::Inert => false,
            ArgReach::Coerced => !strict && frame.held(cx, shape) == Held::Unknown,
            ArgReach::Object => frame.held(cx, shape) == Held::Unknown,
            ArgReach::Nested => frame.held(cx, shape) != Held::ObjectFree,
            ArgReach::Callback | ArgReach::Autoload => true,
        },
    )
}

/// Whether a call to `class::method`, an engine method or constructor, with
/// these argument shapes, written in a file whose strictness is `strict`, may
/// run user code through an argument (issue #858,
/// [`steins_catalog::method_arg_reach`]). `class` is the engine class the call
/// resolves to, not the class it is spelled with: a project exception that
/// inherits `RuntimeException`'s constructor asks for `RuntimeException`.
///
/// The rule is [`reaches_user_code`]'s, position for position. A method the
/// catalog rows on another axis and not on this one reaches blind
/// (`None` is "the catalog states nothing", never "inert"); so does a call whose
/// positions the site did not record: a named or spread argument list, or a
/// receiver the scan does not name by class (`$r->m($o)`, `(new Foo)->m($o)`).
pub(crate) fn method_reaches_user_code(
    cx: &Cx,
    frame: &Frame,
    (class, method): (&str, &str),
    shapes: Option<&[ArgShape]>,
    strict: bool,
) -> bool {
    let Some(row) = steins_catalog::method_arg_reach(class, method) else { return true };
    let Some(shapes) = shapes else { return row.reaches_blind(strict) };
    operands_reach(cx, frame, shapes, strict, &[], |position| row.at(position))
}

/// Whether the builtin `name`, handed to another builtin as a callback, may
/// run user code through the arguments it is then called with: those are of
/// the invoker's choosing, and the call is coercive whatever the calling file
/// declares ([`reaches_user_code`]).
pub(crate) fn callback_reaches_user_code(name: &str) -> bool {
    steins_catalog::arg_reach(name).is_none_or(|row| row.reaches_blind(false))
}

/// [`reaches_user_code`] as the [`Reach`] a resolved site records, in the file
/// whose strictness `cx` carries.
pub(crate) fn builtin_reach(
    cx: &Cx,
    frame: &Frame,
    name: &str,
    shapes: Option<&[ArgShape]>,
    handled: &[usize],
) -> Reach {
    if reaches_user_code(cx, frame, name, shapes, cx.strict(), handled) {
        Reach::Possible
    } else {
        Reach::RuledOut
    }
}

/// [`method_reaches_user_code`] as the [`Reach`] a resolved site records, in the
/// file whose strictness `cx` carries.
pub(crate) fn engine_method_reach(
    cx: &Cx,
    frame: &Frame,
    callee: (&str, &str),
    shapes: Option<&[ArgShape]>,
) -> Reach {
    if method_reaches_user_code(cx, frame, callee, shapes, cx.strict()) {
        Reach::Possible
    } else {
        Reach::RuledOut
    }
}
