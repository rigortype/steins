//! PHP's parameter coercion table (ADR-0011): strict-mode and coercive-mode
//! acceptance of a union member by a value or a fact, and the scalar conversions
//! (`php_str_to_int` and friends) they rest on — plus the **explicit** cast grid
//! `settype` performs (issue #595), which rests on the same conversions.

use steins_domain::{
    Base, Certainty, Fact, Key as VKey, PhpStr, Refinement, ShapeFact, StrPreds, Val,
    php_is_numeric,
};
use steins_syntax::{ArgValue, NativeType, ScalarType, TypeMember};

use crate::cx::Cx;
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
///
/// Rust's `as` saturates, which is what the **string** path wants: measured
/// (PHP 8.5.9) `(int)'1e30'` is `9223372036854775807` and
/// `(int)'-9223372036854775809'` is `PHP_INT_MIN` — php-src saturates a numeric
/// string's magnitude at the bounds. A float *value* does not behave that way,
/// which is what [`php_float_value_to_int`] exists for.
fn php_float_to_int(f: f64) -> Option<i64> {
    f.is_finite().then(|| f.trunc() as i64)
}

// ---- The explicit cast grid (issue #595) ----

/// A `settype` target type, as the literal type string names it — the column
/// header of the probed cast grid ([`php_cast_fact`]).
///
/// The spellings php-src converts under are exactly these, measured at PHP
/// 8.5.9 by calling `settype($v, $t)` for every candidate: `'int'`/`'integer'`,
/// `'float'`/`'double'`, `'string'`, `'bool'`/`'boolean'`, `'array'`, `'null'`,
/// and `'object'`. Matching is case-insensitive (`'Int'`, `'INT'` and
/// `'BOOLEAN'` all convert). Nothing else writes anything at all: `'real'`,
/// `'binary'`, `' int'`, `'int '` and `''` each raise
/// `ValueError: settype(): Argument #2 ($type) must be a valid type`, and
/// `'resource'` is recognized only far enough to raise
/// `ValueError: Cannot convert to resource type`.
///
/// One converting spelling is deliberately **not** a variant here: `'object'`
/// writes a `stdClass`, which the four-layer value domain has no member for
/// (its object stratum lives in the heap store). It leaves the caller's
/// invalidation standing, exactly as the refused spellings do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CastTarget {
    /// `'int'` / `'integer'`.
    Int,
    /// `'float'` / `'double'`.
    Float,
    /// `'string'`.
    String,
    /// `'bool'` / `'boolean'`.
    Bool,
    /// `'array'`.
    Array,
    /// `'null'`.
    Null,
}

impl CastTarget {
    /// The target a proven type string names, or `None` for a spelling this
    /// grid does not state a fact for — including the two accepted-but-declined
    /// ones (see the type doc) and every spelling php-src refuses outright.
    pub(crate) fn from_type_string(s: &str) -> Option<CastTarget> {
        match s.to_ascii_lowercase().as_str() {
            "int" | "integer" => Some(CastTarget::Int),
            "float" | "double" => Some(CastTarget::Float),
            "string" => Some(CastTarget::String),
            "bool" | "boolean" => Some(CastTarget::Bool),
            "array" => Some(CastTarget::Array),
            "null" => Some(CastTarget::Null),
            _ => None,
        }
    }
}

/// One **input class** of the cast grid: a proven value, one abstract scalar arm,
/// or one array shape. The decomposition of a [`Fact`] into the alternatives PHP
/// converts one at a time ([`cast_input_classes`]).
#[derive(Debug, Clone)]
enum CastIn {
    /// A value the walk proved.
    Val(Val),
    /// One abstract scalar arm — a base plus what is known about its values.
    Base(Base, Option<Refinement>),
    /// The `null` alternative an abstract layer's `nullable` flag carries.
    Null,
    /// One array shape.
    Array(ShapeFact),
}

/// **The cast a `settype` target performs on a pre-call fact** (issue #595),
/// authored from a `php -r` grid at PHP 8.5.9 — or `None` where no sound fact
/// can be stated, which keeps the caller's by-ref invalidation.
///
/// The fact is decomposed into the alternatives PHP converts one at a time
/// ([`CastIn`]), each cast on its own, and the results folded through
/// [`Fact::join`] — so a nullable or union input is answered by the same rows a
/// single one is, and one declining alternative declines the whole cast (a join
/// with an unknown is unknown).
///
/// # The grid
///
/// | input | `'int'` | `'float'` | `'string'` | `'bool'` | `'array'` | `'null'` |
/// | --- | --- | --- | --- | --- | --- | --- |
/// | `int` | identity | `float` | `decimal-int-string` | truthiness | `list{int}` | `null` |
/// | `float` | `int` | identity | `uppercase-string&non-empty-string` | truthiness | `list{float}` | `null` |
/// | `string` | `int` | `float` | identity | truthiness | `list{string}` | `null` |
/// | `bool` | `0\|1` | `0.0\|1.0` | `'1'\|''` | identity | `list{bool}` | `null` |
/// | `null` | `0` | `0.0` | `''` | `false` | `array{}` | `null` |
/// | `array` | `0\|1` | `0.0\|1.0` | **declined** | truthiness | identity | `null` |
///
/// Witnesses for the rows that are not the obvious ones, each `php -r`-measured:
///
/// * **`null` to `'array'` is the EMPTY array**, not a one-element one:
///   `settype($v, 'array')` on `null` gives `[]`, while every scalar gives
///   `[0 => $v]`.
/// * **`array` to `'string'` declines.** PHP emits an `E_WARNING`
///   ("Array to string conversion") and writes the literal `'Array'`. Stating
///   that would be right and useless; PHPStan calls the same cell an error. The
///   row is a decline in both directions, so the name stays forgotten.
/// * **`float` to `'string'` is coarser than it looks.** The spellings measured
///   are `'0'`, `'-0'`, `'0.5'`, `'1.0E+30'`, `'2.2204460492503E-16'`, `'NAN'`,
///   `'INF'`, `'-INF'` — so the honest predicates are `uppercase-string` (no
///   spelling has a lowercase character) and `non-empty-string`, and NOT
///   `numeric-string`: `is_numeric('NAN')` is `false`. A **value**-precise
///   answer is declined outright, because the spelling is `precision`-ini
///   dependent (`1.23456789` renders `'1.23'` at `precision=3` and
///   `'1.2345678899999999'` at `17`).
/// * **`bool` reads PHP truthiness**, via the domain's own [`Fact::truthy`], so
///   `'some-string'` casts to `true` and `'0'` to `false` by the same rule
///   every guard uses — never a second falsiness table.
/// * **A string's numeric value is claimed only when PHP calls the string
///   numeric.** `(int)'12abc'` is `12` and `(int)'abc'` is `0` — a
///   leading-numeric-prefix rule this slice does not author — so a non-numeric
///   string widens to the target's base instead of guessing a prefix.
pub(crate) fn php_cast_fact(input: &Fact, target: CastTarget) -> Option<Fact> {
    // `'null'` overwrites whatever was there, and the bool cast is a question
    // about the WHOLE fact (the domain answers it once, three-valued) — neither
    // needs the per-alternative decomposition below.
    match target {
        CastTarget::Null => return Some(Fact::Singleton(Val::Null)),
        CastTarget::Bool => {
            return Some(match input.truthy() {
                Certainty::Yes => Fact::Singleton(Val::Bool(true)),
                Certainty::No => Fact::Singleton(Val::Bool(false)),
                Certainty::Maybe => Fact::General { base: Base::Bool, nullable: false },
            });
        }
        _ => {}
    }
    let mut acc: Option<Fact> = None;
    for class in cast_input_classes(input) {
        let one = cast_one(&class, target)?;
        acc = Some(match acc {
            None => one,
            Some(prev) => prev.join(&one)?,
        });
    }
    acc
}

/// The alternatives a fact admits, as the cast grid reads them: the finite
/// layers enumerate their values, the abstract layers give one arm per base
/// plus the `null` their flag carries, and the array stratum gives its shape.
fn cast_input_classes(f: &Fact) -> Vec<CastIn> {
    let nulls = |nullable: bool| nullable.then_some(CastIn::Null);
    match f {
        Fact::Singleton(v) => vec![CastIn::Val(v.clone())],
        Fact::OneOf(vs) => vs.iter().cloned().map(CastIn::Val).collect(),
        Fact::Refined { base, refinement, nullable } => {
            std::iter::once(CastIn::Base(*base, Some(*refinement))).chain(nulls(*nullable)).collect()
        }
        Fact::General { base, nullable } => {
            std::iter::once(CastIn::Base(*base, None)).chain(nulls(*nullable)).collect()
        }
        Fact::Union { arms, nullable } => arms
            .iter()
            .map(|(base, refinement)| CastIn::Base(*base, *refinement))
            .chain(nulls(*nullable))
            .collect(),
        Fact::Shape { shape, nullable } => std::iter::once(CastIn::Array((**shape).clone()))
            .chain(nulls(*nullable))
            .collect(),
    }
}

/// One cell of the grid: what `target` writes for a single input alternative.
/// See [`php_cast_fact`] for the measured table and the cell it declines.
fn cast_one(class: &CastIn, target: CastTarget) -> Option<Fact> {
    let one = |v: Val| Some(Fact::Singleton(v));
    let pair = |a: Val, b: Val| Fact::from_vals(vec![a, b]);
    match target {
        CastTarget::Int => match class {
            CastIn::Val(Val::Int(i)) => one(Val::Int(*i)),
            CastIn::Val(Val::Float(f)) => Some(php_float_value_to_int(*f).map_or(
                Fact::General { base: Base::Int, nullable: false },
                |i| Fact::Singleton(Val::Int(i)),
            )),
            CastIn::Val(Val::Str(s)) => Some(
                php_numeric_str(s)
                    .and_then(php_str_to_int)
                    .map_or(Fact::General { base: Base::Int, nullable: false }, |i| {
                        Fact::Singleton(Val::Int(i))
                    }),
            ),
            CastIn::Val(Val::Bool(b)) => one(Val::Int(i64::from(*b))),
            CastIn::Val(Val::Null) | CastIn::Null => one(Val::Int(0)),
            CastIn::Val(Val::Array(items)) => one(Val::Int(i64::from(!items.is_empty()))),
            // An int keeps its own interval; `(int)$i` is the identity.
            CastIn::Base(Base::Int, r) => Some(match r {
                Some(refinement) => Fact::refined(Base::Int, *refinement, false),
                None => Fact::General { base: Base::Int, nullable: false },
            }),
            CastIn::Base(Base::Bool, _) => pair(Val::Int(0), Val::Int(1)),
            CastIn::Base(_, _) => Some(Fact::General { base: Base::Int, nullable: false }),
            CastIn::Array(shape) => {
                if shape.non_empty {
                    one(Val::Int(1))
                } else {
                    pair(Val::Int(0), Val::Int(1))
                }
            }
        },
        CastTarget::Float => match class {
            CastIn::Val(Val::Int(i)) => one(Val::Float(*i as f64)),
            CastIn::Val(Val::Float(f)) => one(Val::Float(*f)),
            CastIn::Val(Val::Str(s)) => Some(
                php_numeric_str(s)
                    .and_then(php_str_to_float)
                    .map_or(Fact::General { base: Base::Float, nullable: false }, |f| {
                        Fact::Singleton(Val::Float(f))
                    }),
            ),
            CastIn::Val(Val::Bool(b)) => one(Val::Float(if *b { 1.0 } else { 0.0 })),
            CastIn::Val(Val::Null) | CastIn::Null => one(Val::Float(0.0)),
            CastIn::Val(Val::Array(items)) => {
                one(Val::Float(if items.is_empty() { 0.0 } else { 1.0 }))
            }
            CastIn::Base(Base::Bool, _) => pair(Val::Float(0.0), Val::Float(1.0)),
            CastIn::Base(_, _) => Some(Fact::General { base: Base::Float, nullable: false }),
            CastIn::Array(shape) => {
                if shape.non_empty {
                    one(Val::Float(1.0))
                } else {
                    pair(Val::Float(0.0), Val::Float(1.0))
                }
            }
        },
        CastTarget::String => match class {
            CastIn::Val(Val::Int(i)) => one(Val::Str(PhpStr::from(i.to_string()))),
            CastIn::Val(Val::Str(s)) => one(Val::Str(s.clone())),
            CastIn::Val(Val::Bool(b)) => one(Val::Str(PhpStr::from(if *b { "1" } else { "" }))),
            CastIn::Val(Val::Null) | CastIn::Null => one(Val::Str(PhpStr::new())),
            CastIn::Val(Val::Float(_)) | CastIn::Base(Base::Float, _) => Some(float_string_fact()),
            // A string keeps its own predicate set; `(string)$s` is the identity.
            CastIn::Base(Base::String, r) => Some(match r {
                Some(refinement) => Fact::refined(Base::String, *refinement, false),
                None => Fact::General { base: Base::String, nullable: false },
            }),
            CastIn::Base(Base::Int, _) => Some(Fact::refined(
                Base::String,
                Refinement::Str(StrPreds::DECIMAL_INT.close()),
                false,
            )),
            CastIn::Base(Base::Bool, _) => {
                pair(Val::Str(PhpStr::from("1")), Val::Str(PhpStr::new()))
            }
            // The `E_WARNING` cell: `'Array'` is what PHP writes, and stating it
            // is not worth speaking for a program that is already wrong.
            CastIn::Val(Val::Array(_)) | CastIn::Array(_) => None,
        },
        CastTarget::Array => match class {
            // `null` is the one input that does NOT become a one-element array.
            CastIn::Val(Val::Null) | CastIn::Null => one(Val::Array(Vec::new())),
            CastIn::Val(Val::Array(items)) => one(Val::Array(items.clone())),
            CastIn::Val(v) => one(Val::Array(vec![(VKey::Int(0), v.clone())])),
            CastIn::Base(base, r) => Some(single_element_list(match r {
                Some(refinement) => Fact::refined(*base, *refinement, false),
                None => Fact::General { base: *base, nullable: false },
            })),
            CastIn::Array(shape) => {
                Some(Fact::Shape { shape: Box::new(shape.clone()), nullable: false })
            }
        },
        // Answered above, before the decomposition.
        CastTarget::Bool | CastTarget::Null => None,
    }
}

/// The string a float casts to, as coarsely as the measurement allows:
/// `uppercase-string` and `non-empty-string`, never `numeric-string`
/// ([`php_cast_fact`]'s float row states the spellings).
fn float_string_fact() -> Fact {
    Fact::refined(
        Base::String,
        Refinement::Str(StrPreds::UPPERCASE.union(StrPreds::NON_EMPTY).close()),
        false,
    )
}

/// The sealed one-element list `settype($v, 'array')` writes for a scalar `$v`
/// — measured: `'abc'` gives `[0 => 'abc']`, `false` gives `[0 => false]`.
///
/// Built through the same constructor an array *literal* goes through
/// ([`ShapeFact::from_witnessed_entries`]), so `settype($v, 'array')` and
/// `$v = [$v]` describe the cast's one element identically — order witnessed,
/// key required, tail sealed.
fn single_element_list(elem: Fact) -> Fact {
    let shape = ShapeFact::from_witnessed_entries(&[(VKey::Int(0), Some(elem))]);
    Fact::Shape { shape: Box::new(shape), nullable: false }
}

/// A string PHP itself calls numeric, as a `&str` — the gate on every
/// value-precise numeric claim about a string ([`php_cast_fact`]'s last
/// witness). A byte string that is not valid UTF-8 declines with it, since both
/// readers below are written over a `&str` prefix (ADR-0080 §2.5).
fn php_numeric_str(s: &PhpStr) -> Option<&str> {
    php_is_numeric(s).then(|| s.as_str()).flatten()
}

/// Truncate a float **value** toward zero to an int, or `None` when the result
/// is not the engine's.
///
/// Deliberately stricter than [`php_float_to_int`], which is the *string* path's
/// reader: measured (PHP 8.5.9), `(int)1.0E+30` from a float value is
/// `5076964154930102272` — php-src's out-of-range float truncation is the
/// hardware's, not a saturation — while `(int)'1e30'` from a numeric string is
/// `PHP_INT_MAX`. `NAN` and `INF` truncate to `0`, which is a value this could
/// state but a warning-bearing one; both stay outside the range gate.
fn php_float_value_to_int(f: f64) -> Option<i64> {
    // 2^63 exactly; the open upper bound is `PHP_INT_MAX + 1`, which no float
    // below it rounds past.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    let t = f.trunc();
    (f.is_finite() && (-LIMIT..LIMIT).contains(&t)).then_some(t as i64)
}

#[cfg(test)]
mod cast_grid_tests {
    //! The `settype` cast grid (issue #595), tested as the pure function it is —
    //! no walk, no engine. Every expectation below is a `php -r` measurement at
    //! PHP 8.5.9; the table on [`php_cast_fact`] carries the witnesses.

    use super::*;
    use steins_domain::IntRange;

    fn s(v: &str) -> Fact {
        Fact::Singleton(Val::Str(PhpStr::from(v)))
    }
    fn i(v: i64) -> Fact {
        Fact::Singleton(Val::Int(v))
    }
    fn f(v: f64) -> Fact {
        Fact::Singleton(Val::Float(v))
    }
    fn b(v: bool) -> Fact {
        Fact::Singleton(Val::Bool(v))
    }
    fn general(base: Base) -> Fact {
        Fact::General { base, nullable: false }
    }
    fn cast(input: &Fact, target: CastTarget) -> Option<Fact> {
        php_cast_fact(input, target)
    }

    #[test]
    fn the_type_string_is_matched_case_insensitively_and_closed() {
        assert_eq!(CastTarget::from_type_string("int"), Some(CastTarget::Int));
        assert_eq!(CastTarget::from_type_string("integer"), Some(CastTarget::Int));
        assert_eq!(CastTarget::from_type_string("INT"), Some(CastTarget::Int));
        assert_eq!(CastTarget::from_type_string("Integer"), Some(CastTarget::Int));
        assert_eq!(CastTarget::from_type_string("float"), Some(CastTarget::Float));
        assert_eq!(CastTarget::from_type_string("double"), Some(CastTarget::Float));
        assert_eq!(CastTarget::from_type_string("bool"), Some(CastTarget::Bool));
        assert_eq!(CastTarget::from_type_string("BOOLEAN"), Some(CastTarget::Bool));
        assert_eq!(CastTarget::from_type_string("string"), Some(CastTarget::String));
        assert_eq!(CastTarget::from_type_string("array"), Some(CastTarget::Array));
        assert_eq!(CastTarget::from_type_string("null"), Some(CastTarget::Null));
        // Accepted by php-src but not expressible here: `'object'` writes a
        // `stdClass`, which is not a value-domain fact.
        assert_eq!(CastTarget::from_type_string("object"), None);
        // Refused by php-src with a `ValueError`, so nothing is written at all.
        for name in ["real", "resource", "binary", " int", "int ", "foo", ""] {
            assert_eq!(CastTarget::from_type_string(name), None, "{name} is not a type");
        }
    }

    #[test]
    fn the_int_column_matches_the_probed_values() {
        assert_eq!(cast(&i(123), CastTarget::Int), Some(i(123)));
        assert_eq!(cast(&f(123.0), CastTarget::Int), Some(i(123)));
        assert_eq!(cast(&f(0.5), CastTarget::Int), Some(i(0)));
        assert_eq!(cast(&f(-5.9), CastTarget::Int), Some(i(-5)));
        assert_eq!(cast(&s("123"), CastTarget::Int), Some(i(123)));
        assert_eq!(cast(&b(true), CastTarget::Int), Some(i(1)));
        assert_eq!(cast(&b(false), CastTarget::Int), Some(i(0)));
        assert_eq!(cast(&Fact::Singleton(Val::Null), CastTarget::Int), Some(i(0)));
        // Measured: `(int)[]` is 0 and `(int)['foo']` is 1 — the count never shows.
        assert_eq!(cast(&Fact::Singleton(Val::Array(Vec::new())), CastTarget::Int), Some(i(0)));
        let one = Fact::Singleton(Val::Array(vec![(VKey::Int(0), Val::Str(PhpStr::from("x")))]));
        assert_eq!(cast(&one, CastTarget::Int), Some(i(1)));
    }

    #[test]
    fn an_int_input_keeps_its_own_interval_and_a_bool_becomes_zero_or_one() {
        let ranged = Fact::refined(Base::Int, Refinement::Int(IntRange::new(3, 9).unwrap()), false);
        assert_eq!(cast(&ranged, CastTarget::Int), Some(ranged.clone()));
        assert_eq!(cast(&general(Base::Int), CastTarget::Int), Some(general(Base::Int)));
        assert_eq!(
            cast(&general(Base::Bool), CastTarget::Int),
            Fact::from_vals(vec![Val::Int(0), Val::Int(1)])
        );
        assert_eq!(cast(&general(Base::String), CastTarget::Int), Some(general(Base::Int)));
        assert_eq!(cast(&general(Base::Float), CastTarget::Int), Some(general(Base::Int)));
    }

    #[test]
    fn a_non_numeric_string_widens_to_the_base_rather_than_guessing_a_prefix() {
        // Measured: `(int)'abc'` is 0 and `(int)'12abc'` is 12 — a leading-prefix
        // rule this slice does not author, so the value-precise claim is dropped.
        assert_eq!(cast(&s("abc"), CastTarget::Int), Some(general(Base::Int)));
        assert_eq!(cast(&s("12abc"), CastTarget::Int), Some(general(Base::Int)));
        assert_eq!(cast(&s("abc"), CastTarget::Float), Some(general(Base::Float)));
        // `'INF'`/`'NAN'` are NOT numeric to PHP (`(float)'INF'` is 0.0), and the
        // gate is `is_numeric`, so Rust's own float parser never sees them.
        assert_eq!(cast(&s("INF"), CastTarget::Float), Some(general(Base::Float)));
        assert_eq!(cast(&s("NAN"), CastTarget::Float), Some(general(Base::Float)));
    }

    #[test]
    fn a_numeric_string_saturates_the_way_php_does() {
        // Measured: `(int)'9223372036854775808'` is `PHP_INT_MAX`, and
        // `(int)'1e30'` is too — php-src saturates a numeric string's magnitude.
        assert_eq!(cast(&s("9223372036854775808"), CastTarget::Int), Some(i(i64::MAX)));
        assert_eq!(cast(&s("1e30"), CastTarget::Int), Some(i(i64::MAX)));
        assert_eq!(cast(&s("-9223372036854775809"), CastTarget::Int), Some(i(i64::MIN)));
    }

    #[test]
    fn an_out_of_range_float_value_declines_to_the_base() {
        // Measured: `(int)1.0E+30` from a float VALUE is 5076964154930102272 —
        // the hardware's truncation, not a saturation, and not this domain's to
        // state. `NAN`/`INF` truncate to 0 with a warning; both stay out.
        assert_eq!(cast(&f(1e30), CastTarget::Int), Some(general(Base::Int)));
        assert_eq!(cast(&f(f64::NAN), CastTarget::Int), Some(general(Base::Int)));
        assert_eq!(cast(&f(f64::INFINITY), CastTarget::Int), Some(general(Base::Int)));
    }

    #[test]
    fn the_float_column_matches_the_probed_values() {
        assert_eq!(cast(&i(123), CastTarget::Float), Some(f(123.0)));
        assert_eq!(cast(&f(0.5), CastTarget::Float), Some(f(0.5)));
        assert_eq!(cast(&s("5.5"), CastTarget::Float), Some(f(5.5)));
        assert_eq!(cast(&b(true), CastTarget::Float), Some(f(1.0)));
        assert_eq!(cast(&Fact::Singleton(Val::Null), CastTarget::Float), Some(f(0.0)));
        assert_eq!(
            cast(&general(Base::Bool), CastTarget::Float),
            Fact::from_vals(vec![Val::Float(0.0), Val::Float(1.0)])
        );
        assert_eq!(cast(&general(Base::String), CastTarget::Float), Some(general(Base::Float)));
    }

    #[test]
    fn the_string_column_spells_ints_exactly_and_floats_coarsely() {
        assert_eq!(cast(&i(123), CastTarget::String), Some(s("123")));
        assert_eq!(cast(&i(-1), CastTarget::String), Some(s("-1")));
        assert_eq!(cast(&b(true), CastTarget::String), Some(s("1")));
        assert_eq!(cast(&b(false), CastTarget::String), Some(s("")));
        assert_eq!(cast(&Fact::Singleton(Val::Null), CastTarget::String), Some(s("")));
        assert_eq!(cast(&s("keep"), CastTarget::String), Some(s("keep")));
        // An abstract int spells `decimal-int-string`, the predicate PHP's own
        // write-back rule defines.
        let decimal =
            Fact::refined(Base::String, Refinement::Str(StrPreds::DECIMAL_INT.close()), false);
        assert_eq!(cast(&general(Base::Int), CastTarget::String), Some(decimal));
        // A float's spelling is `precision`-ini dependent, so even a proven float
        // gets only the predicates every measured spelling satisfies.
        let coarse = Fact::refined(
            Base::String,
            Refinement::Str(StrPreds::UPPERCASE.union(StrPreds::NON_EMPTY).close()),
            false,
        );
        assert_eq!(cast(&f(123.0), CastTarget::String), Some(coarse.clone()));
        assert_eq!(cast(&general(Base::Float), CastTarget::String), Some(coarse));
        // A string keeps its own predicates.
        let numeric =
            Fact::refined(Base::String, Refinement::Str(StrPreds::NUMERIC.close()), false);
        assert_eq!(cast(&numeric, CastTarget::String), Some(numeric));
    }

    #[test]
    fn an_array_to_string_cast_declines_in_both_directions() {
        // Measured: an `E_WARNING` and the literal `'Array'`. PHPStan calls the
        // same cell an error; either way the name stays forgotten.
        assert_eq!(cast(&Fact::Singleton(Val::Array(Vec::new())), CastTarget::String), None);
        let shape = Fact::Shape { shape: Box::new(ShapeFact::plain_array()), nullable: false };
        assert_eq!(cast(&shape, CastTarget::String), None);
        // One declining alternative declines the whole join.
        let mixed = Fact::from_vals(vec![Val::Int(1), Val::Array(Vec::new())]).unwrap();
        assert_eq!(cast(&mixed, CastTarget::String), None);
    }

    #[test]
    fn the_bool_column_is_the_domains_own_truthiness() {
        assert_eq!(cast(&s("some-string"), CastTarget::Bool), Some(b(true)));
        assert_eq!(cast(&s("0"), CastTarget::Bool), Some(b(false)));
        assert_eq!(cast(&s(""), CastTarget::Bool), Some(b(false)));
        assert_eq!(cast(&i(0), CastTarget::Bool), Some(b(false)));
        assert_eq!(cast(&i(123), CastTarget::Bool), Some(b(true)));
        assert_eq!(cast(&f(0.0), CastTarget::Bool), Some(b(false)));
        assert_eq!(cast(&Fact::Singleton(Val::Null), CastTarget::Bool), Some(b(false)));
        assert_eq!(cast(&general(Base::String), CastTarget::Bool), Some(general(Base::Bool)));
        assert_eq!(
            cast(&Fact::Singleton(Val::Array(Vec::new())), CastTarget::Bool),
            Some(b(false))
        );
    }

    #[test]
    fn the_array_column_wraps_every_scalar_and_empties_null() {
        // Measured: `settype($v, 'array')` on `'abc'` gives `[0 => 'abc']`, but
        // on `null` gives `[]` — the one input that is not wrapped.
        let wrapped =
            Fact::Singleton(Val::Array(vec![(VKey::Int(0), Val::Str(PhpStr::from("abc")))]));
        assert_eq!(cast(&s("abc"), CastTarget::Array), Some(wrapped));
        assert_eq!(
            cast(&Fact::Singleton(Val::Null), CastTarget::Array),
            Some(Fact::Singleton(Val::Array(Vec::new())))
        );
        // An abstract input becomes the same one-element list, abstractly.
        assert_eq!(
            cast(&general(Base::Int), CastTarget::Array),
            Some(single_element_list(general(Base::Int)))
        );
        // An array input is the identity.
        let shape = Fact::Shape { shape: Box::new(ShapeFact::plain_array()), nullable: false };
        assert_eq!(cast(&shape, CastTarget::Array), Some(shape));
    }

    #[test]
    fn the_null_column_overwrites_whatever_was_there() {
        for input in [s("abc"), i(1), f(1.0), b(true), general(Base::String)] {
            assert_eq!(cast(&input, CastTarget::Null), Some(Fact::Singleton(Val::Null)));
        }
    }

    #[test]
    fn a_nullable_input_casts_its_null_alternative_too() {
        // `?string` to `'int'` is `int` — the string arm widens to `int` and the
        // null arm's `0` joins into it.
        let nullable = Fact::General { base: Base::String, nullable: true };
        assert_eq!(cast(&nullable, CastTarget::Int), Some(general(Base::Int)));
        // `?bool` to `'int'` is `0|1`: `null` casts to 0, already a member.
        let nb = Fact::General { base: Base::Bool, nullable: true };
        assert_eq!(cast(&nb, CastTarget::Int), Fact::from_vals(vec![Val::Int(0), Val::Int(1)]));
        // `?int` to `'string'` admits `''`, which is not a decimal-int-string —
        // the domain's own join drops every predicate the two spellings do not
        // share, leaving the casing one they do (`uncased-string`).
        let ni = Fact::General { base: Base::Int, nullable: true };
        let uncased = Fact::refined(
            Base::String,
            Refinement::Str(StrPreds::LOWERCASE.union(StrPreds::UPPERCASE).close()),
            false,
        );
        assert_eq!(cast(&ni, CastTarget::String), Some(uncased));
    }

    #[test]
    fn a_finite_set_casts_member_by_member() {
        let set = Fact::from_vals(vec![Val::Str(PhpStr::from("a")), Val::Str(PhpStr::from("b"))])
            .unwrap();
        assert_eq!(cast(&set, CastTarget::Bool), Some(b(true)));
        let mixed = Fact::from_vals(vec![Val::Int(0), Val::Int(7)]).unwrap();
        assert_eq!(
            cast(&mixed, CastTarget::String),
            Fact::from_vals(vec![Val::Str(PhpStr::from("0")), Val::Str(PhpStr::from("7"))])
        );
    }
}
