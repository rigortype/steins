//! PHP's parameter coercion table (ADR-0011): strict-mode and coercive-mode
//! acceptance of a union member by a value or a fact, and the scalar conversions
//! (`php_str_to_int` and friends) they rest on.

use steins_domain::{Base, Fact, PhpStr, Refinement, Val, php_is_numeric};
use steins_syntax::{ArgValue, NativeType, ScalarType, TypeMember};

use crate::Cx;
use crate::arg_check::is_type_error;

/// Strict mode: does a single union `member` accept the non-null literal `arg`
/// *exactly* (the only implicit conversion PHP allows in strict mode is
/// int→float, so a `float` member also accepts an `int` arg)?
pub(crate) fn member_accepts_strict(m: &TypeMember, arg: &ArgValue) -> bool {
    match m {
        TypeMember::Scalar(ScalarType::Int) => matches!(arg, ArgValue::Int(_)),
        TypeMember::Scalar(ScalarType::Float) => matches!(arg, ArgValue::Int(_) | ArgValue::Float(_)),
        TypeMember::Scalar(ScalarType::String) => matches!(arg, ArgValue::Str(_)),
        TypeMember::Scalar(ScalarType::Bool) => matches!(arg, ArgValue::Bool(_)),
        TypeMember::BoolLiteral(b) => matches!(arg, ArgValue::Bool(v) if v == b),
        // Object member (ADR-0043): no scalar literal is a member of a class type
        // or intersection. Unreachable in stage 1 (`has_instance` short-circuits
        // in `is_type_error` first); explicit for stage 3.
        TypeMember::Instance { .. } | TypeMember::InstanceInter(_) => false,
    }
}

/// Coercive mode: could the non-null literal `arg` be coerced into this single
/// union `member`? `string`/`bool` are universal sinks for scalars; numeric
/// members accept int/float/bool and numeric strings only; a bool-literal member
/// accepts **only** the exact matching bool value (no coercion into it).
pub(crate) fn member_accepts_coercive(m: &TypeMember, arg: &ArgValue) -> bool {
    match m {
        // Any scalar coerces to `string` or to `bool`.
        TypeMember::Scalar(ScalarType::String) | TypeMember::Scalar(ScalarType::Bool) => true,
        // Numeric members accept numbers, bools, and numeric strings; a
        // non-numeric string is the only scalar that fails.
        TypeMember::Scalar(ScalarType::Int) | TypeMember::Scalar(ScalarType::Float) => match arg {
            ArgValue::Str(s) => php_is_numeric(s),
            ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Bool(_) => true,
            _ => false,
        },
        // No value coerces *into* a bool-literal; only the exact bool matches.
        TypeMember::BoolLiteral(b) => matches!(arg, ArgValue::Bool(v) if v == b),
        // Object member (ADR-0043): see `member_accepts_strict` — unreachable in
        // stage 1, explicit for stage 3.
        TypeMember::Instance { .. } | TypeMember::InstanceInter(_) => false,
    }
}

/// The value a parameter of type `ty` holds when `value` is passed under
/// `strict`, or `None` when the pass fatals at entry or the coercion is
/// uncertain (silence is safe — ADR-0002).
///
/// A single-scalar type reproduces the exact per-scalar coercion (precise
/// interprocedural binding). A **union** binds only when the value already
/// matches a member's own type exactly — Steins does not guess which member PHP
/// would coerce a mismatch into, so it stops the descent (silent) instead.
pub(crate) fn coerce_into_param(cx: &Cx, ty: &NativeType, value: &ArgValue) -> Option<ArgValue> {
    // ADR-0043 stage 1 — an object-bearing type binds the value verbatim, as the
    // pre-ADR-0043 untracked parameter did, so an object parameter does not abort
    // the interprocedural descent. No scalar coercion applies to objects.
    if ty.has_instance() {
        return Some(value.clone());
    }
    if is_type_error(cx, ty, value) {
        return None;
    }
    if matches!(value, ArgValue::Null) {
        return Some(ArgValue::Null);
    }
    if let [TypeMember::Scalar(scalar)] = ty.members.as_slice() {
        return coerce_scalar(*scalar, value);
    }
    // Union: bind only on an exact-type member match; otherwise silence.
    if ty.members.iter().any(|m| member_matches_exact(m, value)) {
        return Some(value.clone());
    }
    None
}

/// The fact a **native-typed slot** actually stores after PHP's typed-boundary
/// conversion (issue #48) — a typed property write, a promoted-param
/// construction, a literal property default — or `None` when no sound fact can
/// be stored and the slot must stay Unknown.
///
/// PHP converts at every typed boundary; recording the *assigned* fact verbatim
/// is how #48's soundness hole opened: an int written to a `float` property read
/// back as int `1`, `=== 1` folded true on a value the runtime holds as `1.0`.
///
/// Deliberately narrower than [`coerce_scalar`]'s coercive-mode table: a stored
/// fact must be right whether or not `declare(strict_types=1)` is set, and this
/// seam does not consult the file's mode. Only **mode-independent** outcomes
/// are stored:
///
/// - a value/base whose runtime type exactly matches a union member stores
///   as-is (no conversion in either mode);
/// - an int into a `float`-member/no-`int`-member type stores as the float it
///   becomes — the ONE implicit conversion PHP performs in both modes (the
///   strict-mode int→float widening exception) — value-precisely for finite
///   layers, `General` float for abstract ones (the domain has no float
///   refinement, so widening drops it: wider, never wrong);
/// - `null` into a nullable type stores as-is;
/// - everything else drops the fact: strict types fatals the write (no fact is
///   soundest), coercive mode's conversion may be computable but storing
///   nothing is still sound.
///
/// An object-bearing type stores verbatim (ADR-0043 stage 1: treated as
/// untracked; object rvalues are excluded before storage).
pub(crate) fn coerce_fact_to_native(ty: &NativeType, fact: Fact) -> Option<Fact> {
    if ty.has_instance() {
        return Some(fact);
    }
    // "int arrives, a float slot converts it": a float member with no int member.
    let float_slot = ty.members.iter().any(|m| matches!(m, TypeMember::Scalar(ScalarType::Float)))
        && !ty.members.iter().any(|m| matches!(m, TypeMember::Scalar(ScalarType::Int)));
    match fact {
        Fact::Singleton(v) => coerce_val_to_native(ty, float_slot, v).map(Fact::Singleton),
        Fact::OneOf(vs) => {
            let coerced: Option<Vec<Val>> =
                vs.into_iter().map(|v| coerce_val_to_native(ty, float_slot, v)).collect();
            // int→float can merge previously-distinct members; `from_vals` re-dedupes.
            coerced.and_then(Fact::from_vals)
        }
        // A union keeps only the arms the native type admits (the parameter is
        // the gate). Losing every arm is no fact rather than an empty one.
        Fact::Union { arms, nullable } => {
            let kept: Vec<(Base, Option<Refinement>)> =
                arms.into_iter().filter(|(b, _)| native_has_base(ty, *b)).collect();
            Fact::union(kept, nullable)
        }
        Fact::Refined { base, refinement, nullable } => {
            if native_has_base(ty, base) {
                Some(Fact::Refined { base, refinement, nullable })
            } else if base == Base::Int && float_slot {
                Some(Fact::General { base: Base::Float, nullable })
            } else {
                None
            }
        }
        Fact::General { base, nullable } => {
            if native_has_base(ty, base) {
                Some(Fact::General { base, nullable })
            } else if base == Base::Int && float_slot {
                Some(Fact::General { base: Base::Float, nullable })
            } else {
                None
            }
        }
        // A tracked native type is scalars-only (an `array` member lowers the
        // whole hint to `None`), so an array fact never inhabits one.
        Fact::Shape { .. } => None,
    }
}

/// The [`Val`] half of [`coerce_fact_to_native`]: exact-member match keeps the
/// value, int→float converts value-precisely (PHP's long→double is the same
/// IEEE conversion as `as f64`), `null` needs the nullable flag, anything else
/// is mode-dependent and drops.
fn coerce_val_to_native(ty: &NativeType, float_slot: bool, v: Val) -> Option<Val> {
    match &v {
        Val::Null => ty.nullable.then_some(v),
        Val::Int(i) if float_slot => Some(Val::Float(*i as f64)),
        Val::Int(_) | Val::Float(_) | Val::Str(_) | Val::Bool(_) => ty
            .members
            .iter()
            .any(|m| native_member_matches_val(m, &v))
            .then_some(v),
        Val::Array(_) => None,
    }
}

/// Whether a union `member` matches the runtime type of scalar value `v`
/// exactly — the [`Val`]-shaped sibling of [`member_matches_exact`].
fn native_member_matches_val(m: &TypeMember, v: &Val) -> bool {
    match (m, v) {
        (TypeMember::Scalar(ScalarType::Int), Val::Int(_))
        | (TypeMember::Scalar(ScalarType::Float), Val::Float(_))
        | (TypeMember::Scalar(ScalarType::String), Val::Str(_))
        | (TypeMember::Scalar(ScalarType::Bool), Val::Bool(_)) => true,
        (TypeMember::BoolLiteral(b), Val::Bool(x)) => x == b,
        _ => false,
    }
}

/// Whether the native type carries the FULL scalar member for `base`. A
/// `true`/`false` literal member deliberately does not count — a `General` bool
/// covers both values, and only one inhabits the slot.
fn native_has_base(ty: &NativeType, base: Base) -> bool {
    ty.members.iter().any(|m| {
        matches!(
            (m, base),
            (TypeMember::Scalar(ScalarType::Int), Base::Int)
                | (TypeMember::Scalar(ScalarType::Float), Base::Float)
                | (TypeMember::Scalar(ScalarType::String), Base::String)
                | (TypeMember::Scalar(ScalarType::Bool), Base::Bool)
        )
    })
}

/// Whether a union `member` matches the *runtime type* of the non-null literal
/// `value` exactly (no coercion) — used to decide when a union binding is safe.
fn member_matches_exact(m: &TypeMember, value: &ArgValue) -> bool {
    match (m, value) {
        (TypeMember::Scalar(ScalarType::Int), ArgValue::Int(_))
        | (TypeMember::Scalar(ScalarType::Float), ArgValue::Float(_))
        | (TypeMember::Scalar(ScalarType::String), ArgValue::Str(_))
        | (TypeMember::Scalar(ScalarType::Bool), ArgValue::Bool(_)) => true,
        (TypeMember::BoolLiteral(b), ArgValue::Bool(v)) => v == b,
        // Object member (ADR-0043): scalar literals never match a class type.
        _ => false,
    }
}

/// The value a single-scalar parameter holds after coercion (the per-scalar
/// PHP 8 coercion table), or `None` when the conversion is uncertain.
fn coerce_scalar(scalar: ScalarType, value: &ArgValue) -> Option<ArgValue> {
    Some(match (scalar, value) {
        (ScalarType::Int, ArgValue::Int(_))
        | (ScalarType::Float, ArgValue::Float(_))
        | (ScalarType::String, ArgValue::Str(_))
        | (ScalarType::Bool, ArgValue::Bool(_)) => value.clone(),

        (ScalarType::Float, ArgValue::Int(i)) => ArgValue::Float(*i as f64),

        // A byte string's numeric cast is declined rather than guessed: both readers
        // are written over a `&str` prefix (ADR-0080 §2.5).
        (ScalarType::Int, ArgValue::Str(s)) => ArgValue::Int(php_str_to_int(s.as_str()?)?),
        (ScalarType::Float, ArgValue::Str(s)) => ArgValue::Float(php_str_to_float(s.as_str()?)?),
        (ScalarType::Int, ArgValue::Float(f)) => ArgValue::Int(php_float_to_int(*f)?),
        (ScalarType::Int, ArgValue::Bool(b)) => ArgValue::Int(i64::from(*b)),
        (ScalarType::Float, ArgValue::Bool(b)) => ArgValue::Float(if *b { 1.0 } else { 0.0 }),
        (ScalarType::Bool, ArgValue::Int(i)) => ArgValue::Bool(*i != 0),
        (ScalarType::Bool, ArgValue::Float(f)) => ArgValue::Bool(*f != 0.0),
        (ScalarType::Bool, ArgValue::Str(s)) => ArgValue::Bool(!(s.is_empty() || s == "0")),
        (ScalarType::String, ArgValue::Int(i)) => ArgValue::Str(PhpStr::from(i.to_string())),
        (ScalarType::String, ArgValue::Bool(b)) => {
            ArgValue::Str(PhpStr::from(if *b { "1" } else { "" }))
        }

        _ => return None,
    })
}

/// Whitespace PHP trims before interpreting a numeric string.
fn php_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'))
}

/// Convert a PHP numeric string to the int it coerces to.
fn php_str_to_int(s: &str) -> Option<i64> {
    let t = php_trim(s);
    if let Ok(i) = t.parse::<i64>() {
        return Some(i);
    }
    php_float_to_int(t.parse::<f64>().ok()?)
}

/// Convert a PHP numeric string to the float it coerces to.
pub(crate) fn php_str_to_float(s: &str) -> Option<f64> {
    php_trim(s).parse::<f64>().ok()
}

/// Truncate a float toward zero to an int (PHP scalar coercion).
fn php_float_to_int(f: f64) -> Option<i64> {
    f.is_finite().then(|| f.trunc() as i64)
}
