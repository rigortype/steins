//! The value relation's non-scalar openings (ADR-0043's 2026-10-11 amendment,
//! issues #951, #941 and #952): a closure as the object it is, the values a native
//! `array`/`object`/`iterable`/`callable` parameter rejects, and `null` at a builtin
//! position the internal-null carve-out does not cover.
//!
//! [`crate::arg_check::is_type_error`] carries the rule table for the
//! [`NativeType`] side; [`top_is_type_error`] carries the table for the tops. Both
//! were run cell by cell on PHP 8.5.11, at the argument, return and property seams,
//! in both modes.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_syntax::{
    Arg, ArgValue, ArrayKey, ClosureRef, NormKey, Param, ParamTop, ParamTopKind, normalize_array,
};

use crate::absence::{callable_class_absent, callable_function_absent, callable_method_absent};
use crate::arg_check::implicit_null_accepted;
use crate::contract::IsA;
use crate::cx::Cx;
use crate::env::{Known, Store, Stratum};
use crate::fold::Folder;
use crate::project::{Diagnostic, FnResolution};

/// The lowercase FQN of PHP's `Closure` class, the class of every closure value.
pub(crate) const CLOSURE_FQN: &str = "closure";

/// Whether a closure value is certainly a `Closure` object when the argument is
/// evaluated: an anonymous closure or arrow function always is; a first-class
/// callable `f(...)` is only when `f` resolves (a project function or a catalogued
/// builtin), since an unresolved name throws an `Error` before any type check.
pub(crate) fn closure_is_certain(cx: &Cx, c: &ClosureRef) -> bool {
    match c {
        ClosureRef::Anonymous { .. } => true,
        ClosureRef::FunctionName(r) => {
            matches!(cx.resolve_function(r), FnResolution::User(_) | FnResolution::Builtin(_))
        }
    }
}

/// [`Cx::resolve_static_value`] plus the two value kinds the native relation now
/// judges written in place: an array literal (an array whatever its elements hold)
/// and a closure that is certainly one ([`closure_is_certain`]).
pub(crate) fn resolve_native_value(
    cx: &Cx,
    v: &ArgValue,
    enclosing: Option<&str>,
) -> Option<ArgValue> {
    match v {
        ArgValue::Array(_) => Some(v.clone()),
        ArgValue::Closure(c) if closure_is_certain(cx, c) => Some(v.clone()),
        _ => cx.resolve_static_value(v, enclosing),
    }
}

/// Does passing `value` to a parameter declared with the top `top` provably raise
/// a `TypeError`? The rule table, every cell run on PHP 8.5.11 in both modes (no
/// cell of it depends on `strict_types`):
///
/// ```text
/// value                 array      object     iterable                callable
/// --------------------- ---------- ---------- ----------------------- ------------------------
/// int/float/bool        TypeError  TypeError  TypeError               TypeError
/// string                TypeError  TypeError  TypeError               TypeError iff no function
///                                                                     (or `C::m` method) by that
///                                                                     name exists — absence
///                                                                     ladder; else decline
/// null, not nullable    TypeError  TypeError  TypeError               TypeError
/// null, `?T` / `= null` accept     accept     accept                  accept
/// array literal         accept     TypeError  accept                  TypeError iff provably no
///                                                                     callable pair (below)
/// closure               TypeError  accept     TypeError (not          accept
///                                             Traversable)
/// `new C`               TypeError  accept     TypeError iff C is-a    TypeError iff C's chain is
///                                             `Traversable` is No     enumerated with no
///                                                                     `__invoke`/`__call`; else
///                                                                     decline (`stdClass` too)
/// enum case             TypeError  accept     as `new C`              decline
/// anything else         decline    decline    decline                 decline
/// ```
///
/// The callable leg's string and array rows convict only what the absence family
/// (ADR-0049) would: the same ladders, gated on the same sidecar, dam and boot
/// surface (`absence::callable_*`). A builtin class's members are never proven
/// absent — a reflected declaration refutes, it does not convict — so
/// `["stdClass", "nope"]` and `new stdClass()` decline. They are read only off a
/// **written** argument (`written`): a name that reached the argument through a
/// variable may sit under an `is_callable($name)` guard the walk does not model.
///
/// An array is a callable pair iff it has exactly two elements, at keys `0` and `1`,
/// the second a string and the first a string or an object. Any array literal that
/// fails that shape is rejected outright; one that has it is rejected iff the class
/// or the method is provably absent.
pub(crate) fn top_is_type_error(
    cx: &Cx,
    folder: &mut dyn Folder,
    top: ParamTop,
    value: &ArgValue,
    written: bool,
    store: Option<&Store>,
) -> bool {
    use ParamTopKind::{Array, Callable, Iterable, Object};
    match value {
        ArgValue::Null => !top.nullable,
        ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Bool(_) => true,
        ArgValue::Str(s) => match top.kind {
            Array | Object | Iterable => true,
            Callable => {
                let (true, Some(store)) = (written, store) else { return false };
                s.as_str().is_some_and(|n| callable_string_absent(cx, folder, n, store))
            }
        },
        ArgValue::Array(items) => match top.kind {
            Array | Iterable => false,
            Object => true,
            Callable => {
                let (true, Some(store)) = (written, store) else { return false };
                callable_array_rejected(cx, folder, items, store)
            }
        },
        ArgValue::Closure(c) => {
            closure_is_certain(cx, c) && matches!(top.kind, Array | Iterable)
        }
        ArgValue::New(..) | ArgValue::EnumCase(..) => {
            let Some(class) = cx.proven_object_class(value) else { return false };
            let class = class.trim_start_matches('\\').to_ascii_lowercase();
            if !cx.is_known_class(&class) {
                return false;
            }
            match top.kind {
                Array => true,
                Object => false,
                Iterable => cx.is_a(&class, "traversable") == IsA::No,
                Callable => {
                    let (ArgValue::New(..), Some(store)) = (value, store) else { return false };
                    callable_method_absent(cx, folder, &class, "__invoke", store)
                }
            }
        }
        _ => false,
    }
}

/// Whether a callable string provably names nothing PHP can call: a function, or a
/// `Class::method` whose class or method is provably absent. A `self`/`parent`/
/// `static` class half, or a method half that itself holds `::`, declines.
fn callable_string_absent(cx: &Cx, folder: &mut dyn Folder, name: &str, store: &Store) -> bool {
    match name.split_once("::") {
        Some((class, method)) => callable_static_pair_absent(cx, folder, class, method, store),
        None => callable_function_absent(cx, folder, name, store),
    }
}

/// Whether `[class, method]` (or `"class::method"`) provably names nothing.
fn callable_static_pair_absent(
    cx: &Cx,
    folder: &mut dyn Folder,
    class: &str,
    method: &str,
    store: &Store,
) -> bool {
    let class = class.trim_start_matches('\\');
    if method.contains("::")
        || ["self", "parent", "static"].iter().any(|k| class.eq_ignore_ascii_case(k))
    {
        return false;
    }
    let lclass = class.to_ascii_lowercase();
    // A vouch for the pair covers its class as well: `is_callable('C::m')` true
    // proves C exists, whatever the index says.
    if store.vouches_method(&lclass, method) {
        return false;
    }
    if callable_class_absent(cx, folder, class, store) {
        return true;
    }
    cx.find_class(&lclass).is_some() && callable_method_absent(cx, folder, &lclass, method, store)
}

/// Whether an array literal provably is no callable pair (see [`top_is_type_error`]).
fn callable_array_rejected(
    cx: &Cx,
    folder: &mut dyn Folder,
    items: &[(ArrayKey, ArgValue)],
    store: &Store,
) -> bool {
    // A key the source does not spell declines: it may be `0` or `1`.
    let Some(entries) = normalize_array(items) else { return false };
    if entries.len() != 2 {
        return true;
    }
    let at = |i: i64| entries.iter().find(|(k, _)| *k == NormKey::Int(i)).map(|(_, v)| v);
    let (Some(target), Some(method)) = (at(0), at(1)) else { return true };
    let method = match method {
        ArgValue::Str(m) => m,
        // Any other value PHP certainly holds is no method name.
        v if v.is_literal() || matches!(v, ArgValue::Array(_) | ArgValue::New(..)) => return true,
        _ => return false,
    };
    let Some(method) = method.as_str() else { return false };
    match target {
        ArgValue::Str(class) => class
            .as_str()
            .is_some_and(|c| callable_static_pair_absent(cx, folder, c, method, store)),
        ArgValue::New(..) => {
            if method.contains("::") {
                return false;
            }
            let Some(class) = cx.proven_object_class(target) else { return false };
            let class = class.trim_start_matches('\\').to_ascii_lowercase();
            cx.find_class(&class).is_some()
                && callable_method_absent(cx, folder, &class, method, store)
        }
        // The first half must be a class name or an object.
        ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Bool(_) | ArgValue::Null
        | ArgValue::Array(_) => true,
        _ => false,
    }
}

/// Whether judging `value` against `param`'s top needs the branch store: a `callable`
/// name (a string or array literal) or object, whose absence legs read the path's
/// `function_exists`/`method_exists`/`is_callable` vouches. The env-free direct pass
/// has no store, so it leaves these to the propagation pass (the #1060 review).
pub(crate) fn top_needs_branch_store(param: &Param, value: &ArgValue) -> bool {
    param.top.is_some_and(|t| t.kind == ParamTopKind::Callable)
        && matches!(value, ArgValue::Str(_) | ArgValue::Array(_) | ArgValue::New(..))
}

/// The argument value a top is judged against, with its provenance, and whether it
/// is the argument as written. `env` is `None` at the env-free direct pass, which
/// owns every written value; with an env, a variable resolves to its `Verified`
/// singleton. Anything else declines.
pub(crate) fn top_arg_value(
    cx: &Cx,
    value: &ArgValue,
    env: Option<&HashMap<String, Known>>,
    poisoned: bool,
) -> Option<(ArgValue, Option<String>, bool)> {
    if let ArgValue::Var(name) = value {
        let k = env?.get(name).filter(|_| !poisoned)?;
        if k.stratum != Stratum::Verified {
            return None;
        }
        let v = k.singleton()?;
        let prov = match &k.bound {
            Some(b) => format!("from ${name}, {b}"),
            None => format!("from ${name}, assigned at line {}", k.line),
        };
        return Some((v, Some(prov), false));
    }
    // Only a string or array literal is a name as written; a class constant that
    // resolves to one is a name an unmodeled guard may test just as a variable is.
    let written = matches!(value, ArgValue::Str(_) | ArgValue::Array(_));
    resolve_native_value(cx, value, None).map(|v| (v, None, written))
}

/// Judge one argument against a parameter's top and push `type.argument-mismatch`
/// when it is rejected. Returns whether it fired, so the caller skips the phpdoc
/// check (no double report). Silent inside a binding descent: a rebound value may
/// sit under an `is_array()`/`is_callable()`/`instanceof` guard the walk does not
/// model, the reason [`crate::arg_check::object_world_guard_blind`] exists.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_top_argument(
    cx: &Cx,
    folder: &mut dyn Folder,
    param: &Param,
    callee: &str,
    arg: &Arg,
    env: Option<&HashMap<String, Known>>,
    store: Option<&Store>,
    poisoned: bool,
    in_descent: bool,
    out: &mut Vec<Diagnostic>,
) -> bool {
    let Some(top) = param.top else { return false };
    if in_descent {
        return false;
    }
    let Some((value, provenance, written)) = top_arg_value(cx, &arg.value, env, poisoned) else {
        return false;
    };
    if implicit_null_accepted(param, &value)
        || !top_is_type_error(cx, folder, top, &value, written, store)
    {
        return false;
    }
    out.push(cx.diagnostic_spelled(
        arg.span.start,
        &value,
        provenance.as_deref(),
        callee,
        &param.name,
        &top.render(),
    ));
    true
}

/// Whether `null` at a builtin position whose reflected type is `rendered` raises a
/// `TypeError` (ADR-0043's 2026-10-11 amendment, the C5 row PHP 8.5.11 measured as
/// `spl_object_id(null)` and `iterator_to_array(null)`).
///
/// The internal-null carve-out (ADR-0056 §9.3) is scalar-only: `null` to a builtin's
/// non-nullable scalar position is a deprecation in coercive mode, but to a position
/// with no scalar member (`object`, `array`, `callable`, a class, `Traversable|array`)
/// it is a `TypeError` in both modes. A position mixing the two (`array|string`)
/// follows the scalar rule: coercive deprecation, strict `TypeError`. `mixed`, a
/// `null` member, or any member this reading does not classify declines.
pub(crate) fn builtin_null_rejected(rendered: &str, strict: bool) -> bool {
    let Some(ty) = steins_contract::lower_str(rendered) else { return false };
    let mut shape = NullShape::default();
    if !classify_null_member(&ty, &mut shape) {
        return false;
    }
    !shape.nullable && (shape.non_scalar || shape.scalar) && (strict || !shape.scalar)
}

/// What [`builtin_null_rejected`] needs to know of a reflected position's members.
#[derive(Default)]
struct NullShape {
    nullable: bool,
    scalar: bool,
    non_scalar: bool,
}

/// Fold one member into `shape`; `false` for a member the reading declines.
fn classify_null_member(ty: &ContractTy, shape: &mut NullShape) -> bool {
    match ty {
        ContractTy::Null => shape.nullable = true,
        ContractTy::Base(_)
        | ContractTy::LitBool(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_) => shape.scalar = true,
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. }
        | ContractTy::Class(_)
        | ContractTy::ObjectAny
        | ContractTy::CallableTy { .. }
        | ContractTy::Inter(_) => shape.non_scalar = true,
        ContractTy::Union(ms) => return ms.iter().all(|m| classify_null_member(m, shape)),
        _ => return false,
    }
    true
}
