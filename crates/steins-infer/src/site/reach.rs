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

mod call_result;
mod consts;

use std::cell::OnceCell;
use std::collections::HashSet;

use steins_catalog::{ArgReach, PrintfFamily};
use steins_syntax::{
    ArgShape, ArgValue, CallTarget, ConstArgs, EffectRecv, FloatEvidence, NameRef, Param,
    NullEvidence, PropertyDecl, SiteKind, SiteOrigin, StaticClass, Stored, Visibility,
};

use super::{NewTarget, Reach, engine_exit, resolve_new};
use crate::Sym;
use crate::cx::Cx;
use crate::global_consts::global_const_fact;
use steins_domain::{Base, Fact, Val};
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
    /// Whether the project holds a `define()` with a computed name ([`consts`]), computed on
    /// the first project constant that asks.
    computed_define: OnceCell<bool>,
}

impl<'a> Frame<'a> {
    pub(crate) fn new(
        class_fqn: Option<&'a str>,
        params: &'a [Param],
        sites: &'a [SiteOrigin],
    ) -> Self {
        Self {
            class_fqn,
            params,
            sites,
            by_ref: OnceCell::new(),
            computed_define: OnceCell::new(),
        }
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
            ArgShape::Call(name) => call_result::function_result(cx, name),
            ArgShape::MethodCall { receiver, method } => {
                call_result::method_result(cx, self, receiver, method)
            }
            // A constant with a scalar value holds no object; one the catalog and the project
            // state nothing of may. A class constant is no object this answer can place: the
            // ToString family asks about it apart (`Operator::subject`).
            ArgShape::GlobalConst(name) => self.global_const_held(cx, name),
            ArgShape::Unknown | ArgShape::ClassConst { .. } => Held::Unknown,
        }
    }
}

impl Frame<'_> {
    /// Whether the value `evidence` describes is a float ([`FloatEvidence`], a printf call's
    /// [`ConstArgs::float_evidence`]): [`FloatClass::Yes`] where every value it admits is a
    /// float, [`FloatClass::No`] where none is, [`FloatClass::Unknown`] otherwise.
    ///
    /// The syntax layer names the form or the declaration and withholds a variable some write
    /// of the frame may leave a float in that it cannot name; the declared types, the
    /// constants and the by-reference question are read here. A parameter is as its declared
    /// type says while no write rebinds it, and a local is as the writes the scan carried
    /// say; a conditional is as its branches agree.
    pub(crate) fn float_class(&self, cx: &Cx, evidence: &FloatEvidence) -> FloatClass {
        match evidence {
            FloatEvidence::NoFloat => FloatClass::No,
            FloatEvidence::Float => FloatClass::Yes,
            FloatEvidence::OneOf(branches) => {
                FloatClass::agree(branches.iter().map(|b| self.float_class(cx, b)))
            }
            FloatEvidence::GlobalConst(name) => global_const_fact(cx, name)
                .map_or(FloatClass::Unknown, |(fact, _)| fact_float_class(&fact)),
            FloatEvidence::ClassConst { class, name } => {
                // A typed constant holds its declared type, so `const float X = 1` is a float
                // whatever the literal says: a typed one is read by its declaration first.
                let literal = cx.resolve_class_const(class, name, self.class_fqn);
                match (cx.class_const_declared_type(class, name, self.class_fqn), literal) {
                    (Some(None), Some(ArgValue::Float(_))) => FloatClass::Yes,
                    (Some(None), Some(v)) if scalar_not_float(&v) => FloatClass::No,
                    (Some(Some(hint)), Some(ArgValue::Int(_) | ArgValue::Float(_)))
                        if declared_float(hint) =>
                    {
                        FloatClass::Yes
                    }
                    (Some(Some(hint)), Some(_)) if hint_non_float(hint) => FloatClass::No,
                    _ => FloatClass::Unknown,
                }
            }
            FloatEvidence::StaticProperty { class, name } => {
                let start = match class {
                    StaticClass::Named(r) => Some(cx.class_fqn(r)),
                    StaticClass::SelfKw => self.class_fqn.map(str::to_owned),
                    StaticClass::Parent => self.class_fqn.and_then(|own| cx.parent_fqn(own)),
                    StaticClass::Static => None,
                };
                start
                    .and_then(|start| property_hint(cx, &start, (name, true), hint_float_class))
                    .unwrap_or(FloatClass::Unknown)
            }
            FloatEvidence::Shape { shape, unwritten, writes } => {
                self.shape_float_class(cx, (shape, *unwritten), writes)
            }
        }
    }

    /// Whether the value `evidence` describes is shown never to be `null`
    /// ([`NullEvidence`], a time-family call's [`ConstArgs::timestamps`]). A parameter is as its
    /// declared type says while no write rebinds it (the scan names only a parameter the frame
    /// never writes, and a named call that takes it by reference rebinds it here); a property
    /// and a call are as their declarations say. A conditional is non-`null` where both of its
    /// branches are.
    pub(crate) fn non_null(&self, cx: &Cx, evidence: &NullEvidence) -> bool {
        match evidence {
            NullEvidence::NonNull => true,
            NullEvidence::MayNull => false,
            NullEvidence::OneOf(branches) => branches.iter().all(|b| self.non_null(cx, b)),
            NullEvidence::Shape(shape) => match shape {
                ArgShape::Param { name, .. } => {
                    let Some(param) = self.params.iter().find(|p| &p.name == name) else {
                        return false;
                    };
                    // `int $t = null` is implicitly nullable.
                    !param.has_null_default
                        && !self.rebound_by_call(cx, name)
                        && param
                            .hint_span
                            .and_then(|span| cx.tree().source_slice(span))
                            .is_some_and(hint_non_null)
                }
                ArgShape::ThisProperty(name) => {
                    this_property_hint(cx, self.class_fqn, name, hint_non_null).unwrap_or(false)
                }
                ArgShape::Call(name) => call_result::function_non_null(cx, name),
                ArgShape::MethodCall { receiver, method } => {
                    call_result::method_non_null(cx, self, receiver, method)
                }
                _ => false,
            },
        }
    }

    /// [`Self::float_class`] of a variable, `$this->name` or call shape, with the evidence of
    /// the writes the scan carried for a variable.
    fn shape_float_class(
        &self,
        cx: &Cx,
        (shape, unwritten): (&ArgShape, bool),
        writes: &[FloatEvidence],
    ) -> FloatClass {
        let written = FloatClass::agree(writes.iter().map(|w| self.float_class(cx, w)));
        match shape {
            ArgShape::Param { name, .. } => {
                if self.rebound_by_call(cx, name) {
                    return FloatClass::Unknown;
                }
                let param = self.params.iter().find(|p| &p.name == name);
                let hint = param.and_then(|p| p.hint_span);
                let nullable_default = param.is_some_and(|p| p.has_null_default);
                match hint.and_then(|span| cx.tree().source_slice(span)).map(hint_float_class) {
                    Some(FloatClass::No) if writes.is_empty() || written == FloatClass::No => {
                        FloatClass::No
                    }
                    // `float $f = null` is implicitly nullable: called with the default it
                    // holds `null`, exactly as `?float` may.
                    Some(FloatClass::Yes) if unwritten && !nullable_default => FloatClass::Yes,
                    _ => FloatClass::Unknown,
                }
            }
            // A local starts `null`, so only what every write stores can show it no float.
            ArgShape::Local { name, .. } => {
                let no_float = writes.is_empty() || written == FloatClass::No;
                if no_float && !self.rebound_by_call(cx, name) {
                    FloatClass::No
                } else {
                    FloatClass::Unknown
                }
            }
            ArgShape::ThisProperty(name) => {
                this_property_hint(cx, self.class_fqn, name, hint_float_class)
                    .unwrap_or(FloatClass::Unknown)
            }
            ArgShape::Call(name) => call_result::function_float_class(cx, name),
            ArgShape::MethodCall { receiver, method } => {
                call_result::method_float_class(cx, self, receiver, method)
            }
            ArgShape::ObjectFree
            | ArgShape::Array
            | ArgShape::Unknown
            | ArgShape::GlobalConst(_)
            | ArgShape::ClassConst { .. } => FloatClass::Unknown,
        }
    }

    /// Whether the container of an offset write is shown to hold no string, so the
    /// write stores an element (or calls `offsetSet`) and converts the value
    /// nothing. The syntax layer builds this operand's shape only for a variable
    /// the frame never leaves a string ([`ArgShape::Param`] or [`ArgShape::Local`]
    /// with [`Stored::Array`], `FrameBindings::container_shape`), which leaves the
    /// by-reference question to answer here as for any variable; `$this->p` is
    /// read against the property's declared type, which admits no `string`,
    /// `mixed` or `callable`.
    pub(crate) fn container_not_string(&self, cx: &Cx, shape: &ArgShape) -> bool {
        match shape {
            ArgShape::Array => true,
            ArgShape::Param { name, stores: Stored::Array }
            | ArgShape::Local { name, stores: Stored::Array } => !self.rebound_by_call(cx, name),
            ArgShape::ThisProperty(name) => {
                this_property_hint(cx, self.class_fqn, name, hint_non_string).unwrap_or(false)
            }
            _ => false,
        }
    }

    /// Whether a named call of the frame may take the variable `name` by
    /// reference, and so rebind it. The syntax layer counts every other write
    /// ([`steins_syntax::DynamicSite::Call`]'s `var`); this is the half only
    /// callee resolution answers, as for an [`ArgShape::Param`].
    pub(crate) fn rebound_by_call(&self, cx: &Cx, name: &str) -> bool {
        let by_ref = self.by_ref.get_or_init(|| passed_by_ref(cx, self.class_fqn, self.sites));
        by_ref.contains(name)
    }

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
        EffectRecv::Var(_)
        | EffectRecv::PropRead(_)
        | EffectRecv::Bound(_)
        | EffectRecv::StaticKw => None,
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
    this_property_hint(cx, class_fqn, name, hint_held).unwrap_or(Held::Unknown)
}

/// What `read` makes of the declared type of the property `$this->name` reaches,
/// under the rule of [`this_property_held`] (an ancestor's private property, a hooked
/// one and an undeclared one answer nothing), or `None` for an untyped one.
fn this_property_hint<T>(
    cx: &Cx,
    class_fqn: Option<&str>,
    name: &str,
    read: impl Fn(&str) -> T,
) -> Option<T> {
    property_hint(cx, class_fqn?, (name, false), read)
}

/// [`this_property_hint`] for the property `name` (`is_static` or not) found from the class
/// `start` up its parent chain.
fn property_hint<T>(
    cx: &Cx,
    start: &str,
    (name, is_static): (&str, bool),
    read: impl Fn(&str) -> T,
) -> Option<T> {
    let mut cur = start.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    while seen.insert(cur.to_ascii_lowercase()) {
        let Some((file, class)) = cx.find_class(&cur) else { break };
        if class.hooked_properties.iter().any(|p| p == name) {
            break;
        }
        let declared = |p: &&PropertyDecl| p.name == name && p.is_static == is_static;
        if let Some(prop) = class.properties.iter().find(declared) {
            let private = prop.visibility == Visibility::Private;
            if (private && !cur.eq_ignore_ascii_case(start)) || prop.hooked {
                break;
            }
            let hint = prop.hint_span.and_then(|span| cx.units[file].tree.source_slice(span));
            return hint.map(read);
        }
        match &class.parent {
            Some(parent) => cur = cx.units[file].tree.resolve_class_fqn(parent),
            None => break,
        }
    }
    None
}

/// Whether a value is a float, as far as a declared type, a constant or a form shows
/// ([`Frame::float_class`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FloatClass {
    /// Every value it admits is a float.
    Yes,
    /// No value it admits is a float.
    No,
    /// Neither: it may or may not be.
    Unknown,
}

impl FloatClass {
    /// What a value is when it is one of `parts`: a float only if every part is, none only
    /// if no part is. An empty list is [`Self::No`], the value of nothing written.
    fn agree(parts: impl IntoIterator<Item = Self>) -> Self {
        parts.into_iter().fold(None, |acc, part| match acc {
            None => Some(part),
            Some(acc) if acc == part => Some(acc),
            Some(_) => Some(Self::Unknown),
        })
        .unwrap_or(Self::No)
    }
}

/// What a value the fact `fact` describes holds: the scalar layers hold no object, whatever the
/// other layers are (an array shape's elements, an object) is not read.
fn fact_held(fact: &Fact) -> Held {
    match fact {
        Fact::Singleton(_)
        | Fact::OneOf(_)
        | Fact::Refined { .. }
        | Fact::General { .. }
        | Fact::Union { .. } => Held::ObjectFree,
        _ => Held::Unknown,
    }
}

/// Whether `v` is a scalar literal that is no float.
fn scalar_not_float(v: &ArgValue) -> bool {
    matches!(v, ArgValue::Int(_) | ArgValue::Str(_) | ArgValue::Bool(_) | ArgValue::Null)
}

/// Whether the declared type `hint` of a constant makes an integer or float literal a float:
/// `float` or `?float` (PHP converts the integer on the way in).
fn declared_float(hint: &str) -> bool {
    matches!(hint.trim().to_ascii_lowercase().as_str(), "float" | "?float")
}

/// Whether a value the fact `fact` describes is a float: a literal or a set of literals is as
/// its members are, a scalar base as the base is (a nullable one may be `null`), and any other
/// layer is not read.
fn fact_float_class(fact: &Fact) -> FloatClass {
    let of = |v: &Val| if matches!(v, Val::Float(_)) { FloatClass::Yes } else { FloatClass::No };
    match fact {
        Fact::Singleton(v) => of(v),
        Fact::OneOf(vals) => FloatClass::agree(vals.iter().map(of)),
        Fact::Refined { base, nullable: false, .. } | Fact::General { base, nullable: false } => {
            if *base == Base::Float { FloatClass::Yes } else { FloatClass::No }
        }
        _ => FloatClass::Unknown,
    }
}

/// Whether a value of the native type spelled `hint` is a float: [`FloatClass::Yes`] for
/// exactly `float` (an integer is converted on the way in, in either calling mode),
/// [`FloatClass::No`] where no member is `float` or `mixed`, otherwise
/// [`FloatClass::Unknown`]. A nullable or union type with a float member may be
/// something else.
pub(crate) fn hint_float_class(hint: &str) -> FloatClass {
    let hint = hint.trim();
    if hint.eq_ignore_ascii_case("float") {
        FloatClass::Yes
    } else if hint_non_float(hint) {
        FloatClass::No
    } else {
        FloatClass::Unknown
    }
}

/// Whether the type spelled `hint` admits no float: no member is `float` or `mixed`.
/// An integer, string or boolean parameter is bound as such (a float argument is
/// converted or refused at the call), an object type holds no float, and a union
/// holds one only through `float`.
fn hint_non_float(hint: &str) -> bool {
    let mut members = hint
        .split(|c: char| matches!(c, '|' | '&' | '(' | ')' | '?') || c.is_whitespace())
        .filter(|m| !m.is_empty())
        .peekable();
    members.peek().is_some()
        && members.all(|m| !matches!(m.to_ascii_lowercase().as_str(), "float" | "mixed"))
}

/// Whether the type spelled `hint` excludes `null`: no `?` prefix and no member `null`,
/// `mixed` or `void`. An untyped declaration has no hint to read and is never asked.
pub(crate) fn hint_non_null(hint: &str) -> bool {
    let hint = hint.trim();
    let mut members = hint
        .split(|c: char| matches!(c, '|' | '&' | '(' | ')' | '?') || c.is_whitespace())
        .filter(|m| !m.is_empty())
        .peekable();
    !hint.starts_with('?')
        && members.peek().is_some()
        && members.all(|m| !matches!(m.to_ascii_lowercase().as_str(), "null" | "mixed" | "void"))
}

/// Whether the type spelled `hint` admits no string: no member is `string`,
/// `mixed` or `callable` (a function name is one). A class name, `array`,
/// `iterable`, `object` and the numbers and booleans hold none.
fn hint_non_string(hint: &str) -> bool {
    let mut members = hint
        .split(|c: char| matches!(c, '|' | '&' | '(' | ')' | '?') || c.is_whitespace())
        .filter(|m| !m.is_empty())
        .peekable();
    members.peek().is_some()
        && members.all(|m| !matches!(m.to_ascii_lowercase().as_str(), "string" | "mixed" | "callable"))
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
///
/// `consts` is the call's literal arguments, where the call has any to read
/// ([`CallSiteReach`]): a literal printf format says which values reach `__toString`
/// (only a `%s` does), and a literal `true` strict flag says `in_array` and
/// `array_search` compare by identity, which runs no user code.
pub(crate) fn reaches_user_code(
    cx: &Cx,
    frame: &Frame,
    name: &str,
    (shapes, consts): (Option<&[ArgShape]>, Option<&ConstArgs>),
    strict: bool,
    handled: &[usize],
) -> bool {
    let Some(row) = steins_catalog::arg_reach(name) else { return true };
    let Some(shapes) = shapes else { return row.reaches_blind(strict) };
    let refined = consts.and_then(|consts| CallSiteReach::of(name, consts));
    operands_reach(cx, frame, shapes, strict, handled, |position| {
        refined.as_ref().and_then(|r| r.at(position)).unwrap_or_else(|| row.at(position))
    })
}

/// What a call's **literal arguments** say about the reach of its other positions,
/// where the catalog's row cannot (ADR-0021's 2026-10-03 note on call-site
/// refinements).
enum CallSiteReach {
    /// A printf-family call with a literal format the parser reads: each value reaches
    /// what its conversions say ([`steins_catalog::format_reach`]).
    Format { family: PrintfFamily, per_value: Vec<ArgReach> },
    /// `in_array` or `array_search` with a literal `true` strict flag: the needle and the
    /// haystack are compared by identity.
    Strict,
}

impl CallSiteReach {
    /// The refinement `consts` give a call to `name`, or `None` where the row stands.
    fn of(name: &str, consts: &ConstArgs) -> Option<Self> {
        if let Some(family) = steins_catalog::printf_family(name) {
            let reading = steins_catalog::read_format(literal_format(consts, &family)?)?;
            return Some(Self::Format { family, per_value: reading.reach });
        }
        let flag = steins_catalog::strict_flag_position(name)?;
        let strict =
            consts.bools.iter().any(|&(position, value)| usize::from(position) == flag && value);
        strict.then_some(Self::Strict)
    }

    /// The reach at call `position`, or `None` where the row answers.
    fn at(&self, position: usize) -> Option<ArgReach> {
        match self {
            Self::Format { family, per_value } => family.reach_at(per_value, position),
            Self::Strict => (position < 2).then_some(ArgReach::Inert),
        }
    }
}

/// The literal format of a printf-family call, when the call site wrote one as a
/// string literal: the argument at the family's format position.
pub(crate) fn literal_format<'c>(consts: &'c ConstArgs, family: &PrintfFamily) -> Option<&'c str> {
    let target = match family.format_position() {
        0 => consts.first.as_ref(),
        1 => consts.second.as_ref(),
        _ => None,
    };
    match target {
        Some(CallTarget::Literal(format)) => Some(format),
        _ => None,
    }
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
    args: (Option<&[ArgShape]>, Option<&ConstArgs>),
    handled: &[usize],
) -> Reach {
    if reaches_user_code(cx, frame, name, args, cx.strict(), handled) {
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
