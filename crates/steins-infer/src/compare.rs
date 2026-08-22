//! PHP scalar comparison semantics (ADR-0031): `===` and truthiness are exact;
//! `==` is settled empirically against PHP 8.5.8 (see [`php_loose_eq`]). Undecided
//! cells return `None`, which every caller reads as `Maybe` — silence, the sound
//! side.

use steins_domain::{PhpStr, php_is_numeric};
use steins_syntax::{ArgValue, ArrayKey, normalize_array};

use crate::coerce::php_str_to_float;

// ---------------------------------------------------------------------------
// PHP scalar comparison semantics (ADR-0031). `===`/truthiness are exact; `==`
// is settled EMPIRICALLY against PHP 8.5.8 (see [`php_loose_eq`]). Undecidable
// cells return `None` → the caller yields `Maybe` (silence, the sound side).
// ---------------------------------------------------------------------------

/// PHP truthiness of a proven value. Note `"0"` and `""` are the only falsy
/// non-empty/-empty strings (`"0.0"` and `"00"` are **truthy**), `0`/`0.0`/`[]`
/// are falsy. `None` for a non-concrete value.
pub(crate) fn php_truthy(v: &ArgValue) -> Option<bool> {
    match v {
        ArgValue::Null => Some(false),
        ArgValue::Bool(b) => Some(*b),
        ArgValue::Int(i) => Some(*i != 0),
        ArgValue::Float(f) => Some(*f != 0.0),
        ArgValue::Str(s) => Some(!(s.is_empty() || s == "0")),
        ArgValue::Array(items) => Some(!items.is_empty()),
        _ => None,
    }
}

/// Strict identity `===`: same runtime type AND equal value. Different concrete
/// runtime types are a definite non-identity; a non-concrete operand is `None`.
pub(crate) fn php_identical(a: &ArgValue, b: &ArgValue, php_minor: Option<(u16, u16)>) -> Option<bool> {
    use ArgValue::{Array, Bool, Float, Int, Null, Str};
    match (a, b) {
        (Int(x), Int(y)) => Some(x == y),
        (Float(x), Float(y)) => Some(x == y),
        (Str(x), Str(y)) => Some(x == y),
        (Bool(x), Bool(y)) => Some(x == y),
        (Null, Null) => Some(true),
        (Array(_), Array(_)) => php_array_identical(a, b, php_minor),
        _ if is_concrete(a) && is_concrete(b) => Some(false),
        _ => None,
    }
}

/// Deep `===` of two array literals: same length, same key order, element-wise
/// identical. A non-concrete element makes the result `None`.
fn php_array_identical(a: &ArgValue, b: &ArgValue, php_minor: Option<(u16, u16)>) -> Option<bool> {
    let (ArgValue::Array(ai), ArgValue::Array(bi)) = (a, b) else { return None };
    // Keys the project's PHP minor cannot pin down make the whole verdict
    // undecidable (ADR-0049 A12) — `===` compares key order, so a guessed key
    // would forge a `===` premise.
    let na = normalize_array(ai, php_minor)?;
    let nb = normalize_array(bi, php_minor)?;
    if na.len() != nb.len() {
        return Some(false);
    }
    for ((ka, va), (kb, vb)) in na.iter().zip(nb.iter()) {
        if ka != kb {
            return Some(false);
        }
        match php_identical(va, vb, php_minor) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => return None,
        }
    }
    Some(true)
}

/// Whether a value is a fully-known concrete value (a scalar literal or an array).
fn is_concrete(v: &ArgValue) -> bool {
    v.is_literal() || matches!(v, ArgValue::Array(_))
}

/// Loose equality `==`, settled **empirically against PHP 8.5.8** (`php -r`, the
/// full cross-product of `null`/`false`/`true`/`0`/`0.0`/`""`/`"0"`/`"abc"`/`"5"`/`[]`
/// recorded). The measured table (`T` = equal):
///
/// ```text
///           null false true  0   0.0   ""   "0"  "abc" "5"   []
///   null     T    T    F    T    T     T    F    F     F     T
///   false    T    T    F    T    T     T    T    F     F     T
///   true     F    F    T    F    F     F    F    T     T     F
///   0        T    T    F    T    T     F    T    F     F     F
///   0.0      T    T    F    T    T     F    T    F     F     F
///   ""       T    T    F    F    F     T    F    F     F     F
///   "0"      F    T    F    T    T     F    T    F     F     F
///   "abc"    F    F    T    F    F     F    F    T     F     F
///   "5"      F    F    T    F    F     F    F    F     T     F
///   []       T    T    F    F    F     F    F    F     F     T
/// ```
///
/// Rules reproduced (stable since PHP 8.0): a `bool` operand casts BOTH sides to
/// bool; `null` compares to the other side's zero/empty (except bool); `int`/
/// `float` vs a numeric string compares numerically, vs non-numeric compares
/// string forms; two strings compare numerically iff both numeric, else
/// byte-wise; an array is unequal to any non-null, non-bool scalar. Uncovered
/// cells (a `float` vs non-numeric string; non-trivial arrays) return `None` →
/// `Maybe`.
pub(crate) fn php_loose_eq(a: &ArgValue, b: &ArgValue, php_minor: Option<(u16, u16)>) -> Option<bool> {
    use ArgValue::{Array, Bool, Float, Int, Null, Str};
    // A `bool` on either side casts both operands to bool (subsumes null==bool).
    if matches!(a, Bool(_)) || matches!(b, Bool(_)) {
        return Some(php_truthy(a)? == php_truthy(b)?);
    }
    match (a, b) {
        (Null, Null) => Some(true),
        (Null, Int(i)) | (Int(i), Null) => Some(*i == 0),
        (Null, Float(f)) | (Float(f), Null) => Some(*f == 0.0),
        (Null, Str(s)) | (Str(s), Null) => Some(s.is_empty()),
        (Null, Array(items)) | (Array(items), Null) => Some(items.is_empty()),
        (Null, _) | (_, Null) => None,

        (Int(x), Int(y)) => Some(x == y),
        (Int(x), Float(y)) | (Float(y), Int(x)) => Some((*x as f64) == *y),
        (Float(x), Float(y)) => Some(x == y),

        (Int(i), Str(s)) | (Str(s), Int(i)) => Some(php_int_str_eq(*i, s)),
        (Float(f), Str(s)) | (Str(s), Float(f)) => php_float_str_eq(*f, s),
        (Str(x), Str(y)) => Some(php_str_eq(x, y)),

        (Array(x), Array(y)) => php_array_loose_eq(x, y, php_minor),
        // An array is never loosely equal to a (non-null, non-bool) scalar.
        (Array(_), Int(_) | Float(_) | Str(_)) | (Int(_) | Float(_) | Str(_), Array(_)) => {
            Some(false)
        }
        _ => None,
    }
}

/// `int == string`: numeric string → numeric compare; else compare the int's
/// decimal form to the string (PHP 8 semantics).
fn php_int_str_eq(i: i64, s: &PhpStr) -> bool {
    // A byte string is never numeric (every byte of a numeric string is ASCII),
    // so it always falls to the byte comparison — which is what PHP does.
    if let Some(t) = s.as_str()
        && php_is_numeric(t)
    {
        php_str_to_float(t).is_some_and(|f| (i as f64) == f)
    } else {
        i.to_string().as_bytes() == s.as_bytes()
    }
}

/// `float == string`: numeric string → numeric compare; a non-numeric string is
/// undecidable here (float→string formatting is precision-sensitive) → `None`.
fn php_float_str_eq(f: f64, s: &PhpStr) -> Option<bool> {
    if let Some(t) = s.as_str()
        && php_is_numeric(t)
    {
        Some(php_str_to_float(t).is_some_and(|g| f == g))
    } else {
        None
    }
}

/// `string == string`: both numeric strings → numeric compare; else byte compare.
fn php_str_eq(x: &PhpStr, y: &PhpStr) -> bool {
    if let (Some(a), Some(b)) = (x.as_str(), y.as_str())
        && php_is_numeric(a)
        && php_is_numeric(b)
    {
        match (php_str_to_float(a), php_str_to_float(b)) {
            (Some(a), Some(b)) => a == b,
            _ => x == y,
        }
    } else {
        x == y
    }
}

/// `array == array`: same key set with loosely-equal values (order-independent).
/// An undecidable element value makes the whole comparison `None`.
fn php_array_loose_eq(
    x: &[(ArrayKey, ArgValue)],
    y: &[(ArrayKey, ArgValue)],
    php_minor: Option<(u16, u16)>,
) -> Option<bool> {
    // As in `php_array_identical`: unproven keys make `==` undecidable, since the
    // comparison is key-set-based (ADR-0049 A12).
    let nx = normalize_array(x, php_minor)?;
    let ny = normalize_array(y, php_minor)?;
    if nx.len() != ny.len() {
        return Some(false);
    }
    for (k, va) in &nx {
        let Some((_, vb)) = ny.iter().find(|(k2, _)| k2 == k) else {
            return Some(false);
        };
        match php_loose_eq(va, vb, php_minor) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => return None,
        }
    }
    Some(true)
}

#[cfg(test)]
mod domain_tests {
    //! Unit tests for the ADR-0031/0035 domain skeleton: the unified [`Certainty`]
    //! algebra, [`Fact`] joins (agree / OneOf / cap overflow), and the empirically
    //! settled PHP comparison primitives.
    use steins_domain::Certainty;
    use crate::env::Known;
    use steins_domain::{Base, Fact, Val};
    use steins_domain::PhpStr;
    use crate::compare::{php_identical, php_loose_eq, php_truthy};
    use crate::env::singleton_fact;
    use steins_syntax::ArgValue;

    fn sing(v: ArgValue) -> Fact {
        // Scalars only here — no array literal, so the minor is immaterial.
        singleton_fact(&v, None).expect("literal converts")
    }

    #[test]
    fn certainty_algebra() {
        use Certainty::{Maybe, No, Yes};
        // not swaps the poles, fixes Maybe.
        assert_eq!(Yes.not(), No);
        assert_eq!(No.not(), Yes);
        assert_eq!(Maybe.not(), Maybe);
        // and: No dominates, then Maybe.
        assert_eq!(Yes.and(Yes), Yes);
        assert_eq!(Yes.and(No), No);
        assert_eq!(Yes.and(Maybe), Maybe);
        assert_eq!(No.and(Maybe), No);
        // or: Yes dominates, then Maybe.
        assert_eq!(No.or(No), No);
        assert_eq!(No.or(Yes), Yes);
        assert_eq!(No.or(Maybe), Maybe);
        assert_eq!(Yes.or(Maybe), Yes);
    }

    #[test]
    fn fact_join_agree_keeps_singleton() {
        // The env now stores `steins_domain::Fact`; joins go through the domain
        // algebra. Equal singletons stay a Singleton and resolve to the value.
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(5))).unwrap();
        assert!(matches!(j, Fact::Singleton(Val::Int(5))));
        let k = Known::value(j, 0, None);
        assert_eq!(k.singleton(), Some(ArgValue::Int(5)));
    }

    #[test]
    fn fact_join_differ_forms_oneof_and_dedups() {
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j, Fact::OneOf(vs) if vs.len() == 2));
        // A OneOf never resolves to a single proven value.
        assert_eq!(Known::value(j.clone(), 0, None).singleton(), None);
        // Re-joining an already-present value dedups.
        let j2 = j.join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j2, Fact::OneOf(vs) if vs.len() == 2));
    }

    #[test]
    fn fact_join_overflow_widens_to_refined() {
        // Beyond the OneOf cap the domain widens to a *computed* Refined summary
        // (an int interval), rather than dropping — abstract facts now flow
        // through the env (ADR-0035 stage 2). The widened fact resolves no value.
        let full = Fact::from_vals((0..steins_domain::CAP as i64).map(Val::Int).collect()).unwrap();
        assert!(matches!(full, Fact::OneOf(_)));
        let widened = full.join(&sing(ArgValue::Int(999))).unwrap();
        assert!(matches!(widened, Fact::Refined { base: Base::Int, .. }));
        assert_eq!(Known::value(widened, 0, None).singleton(), None);
    }

    #[test]
    fn loose_eq_measured_cells_php_8_5_8() {
        use ArgValue::{Bool, Int, Null, Str};
        let s = |x: &str| Str(x.into());
        // A representative slice of the recorded PHP 8.5.8 table.
        assert_eq!(php_loose_eq(&Null, &Null, Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Null, &Int(0), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Null, &s(""), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Null, &s("0"), Some((8, 5))), Some(false)); // the PHP 8 trap
        assert_eq!(php_loose_eq(&Null, &Bool(false), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("0"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("abc"), Some((8, 5))), Some(false));
        assert_eq!(php_loose_eq(&Bool(true), &s("abc"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s("abc"), Some((8, 5))), Some(false)); // PHP 8, not PHP 7
        assert_eq!(php_loose_eq(&Int(0), &s("0"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s(""), Some((8, 5))), Some(false));
        assert_eq!(php_loose_eq(&s("0"), &s(""), Some((8, 5))), Some(false));
        assert_eq!(php_loose_eq(&s("5"), &s("5"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&s("5"), &Int(5), Some((8, 5))), Some(true));
    }

    #[test]
    fn truthiness_edge_cells() {
        use ArgValue::{Array, Float, Int, Null, Str};
        assert_eq!(php_truthy(&Str("0".into())), Some(false)); // "0" is falsy
        assert_eq!(php_truthy(&Str("0.0".into())), Some(true)); // but "0.0" is truthy
        assert_eq!(php_truthy(&Str(PhpStr::new())), Some(false));
        assert_eq!(php_truthy(&Int(0)), Some(false));
        assert_eq!(php_truthy(&Float(0.0)), Some(false));
        assert_eq!(php_truthy(&Null), Some(false));
        assert_eq!(php_truthy(&Array(vec![])), Some(false)); // [] is falsy
    }

    #[test]
    fn identical_is_type_strict() {
        use ArgValue::{Float, Int};
        assert_eq!(php_identical(&Int(5), &Int(5), Some((8, 5))), Some(true));
        assert_eq!(php_identical(&Int(5), &Float(5.0), Some((8, 5))), Some(false)); // 5 === 5.0 is false
    }

    /// ADR-0049 A12: the next-auto-index rule for negative keys changed in PHP
    /// 8.3, so an array `===` verdict is a function of the *project's* minor —
    /// and is unproven when no minor was reported.
    #[test]
    fn negative_key_arrays_compare_per_the_project_minor() {
        use steins_syntax::ArrayKey;
        let s = |x: &str| ArgValue::Str(x.into());
        let arr = |items: Vec<(ArrayKey, ArgValue)>| ArgValue::Array(items);

        // `[-5 => 'a', 'b']` — the omitted key is where the two rules disagree.
        let auto = arr(vec![(ArrayKey::Int(-5), s("a")), (ArrayKey::Auto, s("b"))]);
        // `[-5 => 'a', -4 => 'b']` (the 8.3+ landing) and `[-5 => 'a', 0 => 'b']`
        // (the pre-8.3 landing), both written with explicit keys.
        let at_minus_4 = arr(vec![(ArrayKey::Int(-5), s("a")), (ArrayKey::Int(-4), s("b"))]);
        let at_zero = arr(vec![(ArrayKey::Int(-5), s("a")), (ArrayKey::Int(0), s("b"))]);

        // Witnessed on PHP 8.5.8:
        //   php -r 'var_export([-5=>"a","b"] === [-5=>"a",-4=>"b"]);' → true
        //   php -r 'var_export([-5=>"a","b"] === [-5=>"a",0=>"b"]);'  → false
        assert_eq!(php_identical(&auto, &at_minus_4, Some((8, 5))), Some(true));
        assert_eq!(php_identical(&auto, &at_zero, Some((8, 5))), Some(false));

        // A project on 8.1/8.2 floors the auto index at 0 — the verdicts invert.
        for minor in [(8, 1), (8, 2)] {
            assert_eq!(php_identical(&auto, &at_minus_4, Some(minor)), Some(false), "{minor:?}");
            assert_eq!(php_identical(&auto, &at_zero, Some(minor)), Some(true), "{minor:?}");
        }

        // No reported minor: unproven, not guessed. This is the leg that keeps a
        // wrong key out of the proof layer.
        assert_eq!(php_identical(&auto, &at_minus_4, None), None);
        assert_eq!(php_identical(&auto, &at_zero, None), None);
        assert_eq!(php_loose_eq(&auto, &at_minus_4, None), None);

        // A version-independent literal still decides under an unknown minor —
        // the widening stays narrow.
        let list = arr(vec![(ArrayKey::Auto, s("a"))]);
        let list_explicit = arr(vec![(ArrayKey::Int(0), s("a"))]);
        assert_eq!(php_identical(&list, &list_explicit, None), Some(true));
    }

    /// The same premise on the fact side: an unresolvable key drops the
    /// `Val::Array` singleton rather than recording a guessed one.
    #[test]
    fn unproven_negative_key_drops_the_singleton_fact() {
        use steins_syntax::ArrayKey;
        let arr = ArgValue::Array(vec![
            (ArrayKey::Int(-5), ArgValue::Str("a".into())),
            (ArrayKey::Auto, ArgValue::Str("b".into())),
        ]);
        assert!(singleton_fact(&arr, None).is_none());
        assert!(singleton_fact(&arr, Some((8, 5))).is_some());
        assert!(singleton_fact(&arr, Some((8, 1))).is_some());
    }
}
