//! PHP value semantics the domain depends on, implemented to the letter of PHP 8.x and
//! verified against the real engine by `tests/it/php_oracle.rs` where history was treacherous.
//!
//! Every predicate takes `impl AsRef<[u8]>`, not `&str`: a PHP string is a byte string
//! (ADR-0080) that need not be valid UTF-8; `&str` callers pass through unchanged, and every
//! byte >= 0x80 is uncased, non-numeric and non-digit throughout.
//!
//! [`PhpStr`]: crate::PhpStr

/// The ASCII whitespace PHP's numeric grammar trims.
const WS: &[u8] = b" \t\n\r\x0B\x0C";

/// Trim [`WS`] from both ends, byte-wise.
fn trim_ws(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if !WS.contains(first) {
            break;
        }
        b = rest;
    }
    while let [rest @ .., last] = b {
        if !WS.contains(last) {
            break;
        }
        b = rest;
    }
    b
}

/// PHP 8 `is_numeric()`.
///
/// Grammar: optional leading whitespace, optional sign, then an integer (`digits`) or float
/// (`digits "." digits?` | `digits? "." digits`), optionally with an exponent (`[eE] sign?
/// digits`); trailing whitespace is allowed (PHP >= 8.0). Hex/binary/octal strings are NOT
/// numeric; at least one mantissa digit is required.
#[must_use]
pub fn php_is_numeric(s: impl AsRef<[u8]>) -> bool {
    let b = trim_ws(s.as_ref());
    let mut i = 0usize;

    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }

    let mut mantissa_digits = 0usize;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        mantissa_digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            mantissa_digits += 1;
        }
    }
    if mantissa_digits == 0 {
        return false;
    }

    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut exp_digits = 0usize;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            exp_digits += 1;
        }
        if exp_digits == 0 {
            return false;
        }
    }

    i == b.len()
}

/// PHP falsiness of a *string*: exactly `""` and `"0"` are falsy. (`"0.0"`, `" "`, and
/// `"00"` are all truthy — the classic traps.)
#[must_use]
pub fn php_str_is_falsy(s: impl AsRef<[u8]>) -> bool {
    let b = s.as_ref();
    b.is_empty() || b == b"0"
}

/// PHP's `strtolower($s) === $s` — PHPStan's `lowercase-string`
/// (`AccessoryLowercaseStringType`), not "made of lowercase letters": a string with no cased
/// character (`""`, `"123"`) qualifies.
///
/// Byte-oriented on ASCII letters, matching the engine: since PHP 8.2 `strtolower()` is
/// locale-independent and maps only `A-Z` to `a-z`. A UTF-8 `"Ä"` therefore qualifies too
/// (its bytes are all >= 0x80). Verified against the real engine, including multibyte cases,
/// by `tests/it/php_oracle.rs`.
#[must_use]
pub fn php_str_is_lowercase(s: impl AsRef<[u8]>) -> bool {
    !s.as_ref().iter().any(u8::is_ascii_uppercase)
}

/// PHP's `strtoupper($s) === $s` — mirrors [`php_str_is_lowercase`] as PHPStan's
/// `uppercase-string`; not exclusive (an uncased string satisfies both).
#[must_use]
pub fn php_str_is_uppercase(s: impl AsRef<[u8]>) -> bool {
    !s.as_ref().iter().any(u8::is_ascii_lowercase)
}

/// PHP's array-key cast identity for integer-like strings — PHPStan's `decimal-int-string`
/// (`AccessoryDecimalIntegerStringType`): the string spells an integer the way PHP writes
/// one back, so `$a[$s]` casts to an `int` key instead of staying a string key.
///
/// The engine's rule (`ZEND_HANDLE_NUMERIC_STR`):
/// * optional leading `-`, then only ASCII digits — no `+`, whitespace, `.`/`e`, or
///   hex/octal/binary prefix;
/// * no leading zero unless the whole string is `"0"`; `"-0"` does NOT qualify (PHP writes
///   zero back as `"0"`), even though `is_numeric("-0")` is true;
/// * must fit a platform `int`: `"9223372036854775808"` (one past `PHP_INT_MAX`) stays a
///   string key, `"-9223372036854775808"` (`PHP_INT_MIN`) does not.
///
/// Strictly narrower than [`php_is_numeric`]: `"007"`, `"+1"`, `"00"`, `"1.2"`, `"18E+3"`,
/// `" 1 "` are all numeric but keep string identity. `tests/it/php_oracle.rs` verifies against
/// the real engine via `is_int(array_key_first(...))`.
#[must_use]
pub fn php_str_is_decimal_int(s: impl AsRef<[u8]>) -> bool {
    let b = s.as_ref();
    let digits = if b.first() == Some(&b'-') { &b[1..] } else { b };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if digits[0] == b'0' && b.len() > 1 {
        return false;
    }
    // All-ASCII, so valid UTF-8; the parse enforces the `PHP_INT_MAX` bound.
    std::str::from_utf8(b).is_ok_and(|s| s.parse::<i64>().is_ok())
}

/// PHP falsiness of a scalar value, expressed over the domain's [`Val`](crate::Val).
///
/// Falsy: `false`, `0`, `0.0` (and `-0.0`), `""`, `"0"`, `null`, `[]`.
#[must_use]
pub fn php_is_falsy(v: &crate::Val) -> bool {
    use crate::Val;
    match v {
        Val::Bool(b) => !b,
        Val::Int(i) => *i == 0,
        Val::Float(f) => *f == 0.0,
        Val::Str(s) => php_str_is_falsy(s),
        Val::Null => true,
        Val::Array(items) => items.is_empty(),
    }
}

/// A float as PHP spells it — `(string) $f` under `precision = -1`, with `.0`
/// appended to an integral finite spelling. This is PHPStan's
/// `ConstantFloatType::castFloatToString`, the form every float in a dumped type
/// or a diagnostic takes (`1.5`, `5.0`, `1.0E+17`, `5.0E-324`, `INF`, `-0.0`).
///
/// The digits are the shortest that round-trip (Rust's `{:e}` and PHP's
/// `zend_gcvt` agree there). The layout is `zend_gcvt`'s with 17 significant
/// digits: with `p` the position of the decimal point (`1.5` has `p = 1`), an
/// exponent form is used when `p < -3` or `p > 17` — so `0.0001` and
/// `10000000000000000.0` (1e16) stay fixed while `1.0E-5` and `1.0E+17` do not —
/// and that form always shows a fractional digit and a signed exponent.
#[must_use]
pub fn php_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NAN".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF" } else { "-INF" }.to_owned();
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').expect("`{:e}` always has an exponent");
    let exp: i32 = exp.parse().expect("`{:e}` exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // `{:e}` writes one digit before the point, so the point sits after `exp + 1`.
    let point = exp + 1;
    if !(-3..=17).contains(&point) {
        let (lead, rest) = digits.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{lead}.{rest}E{esign}{}", exp.unsigned_abs());
    }
    let len = i32::try_from(digits.len()).expect("at most 17 digits");
    let body = if point <= 0 {
        format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
    } else if point >= len {
        format!("{digits}{}.0", "0".repeat((point - len) as usize))
    } else {
        let (int, frac) = digits.split_at(point as usize);
        format!("{int}.{frac}")
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_grammar() {
        for ok in ["5", "-5", "+5", "5.", ".5", "5.5", "1e3", "1E+3", "5.e3", " 5 ", "\t5\n", "007"] {
            assert!(php_is_numeric(ok), "expected numeric: {ok:?}");
        }
        for no in ["", ".", "e5", "5e", "5e+", "abc", "0x1A", "0b101", "1_000", "5,5", "++5", "5..5", "NAN", "INF"] {
            assert!(!php_is_numeric(no), "expected non-numeric: {no:?}");
        }
    }

    #[test]
    fn string_falsiness_traps() {
        assert!(php_str_is_falsy(""));
        assert!(php_str_is_falsy("0"));
        for truthy in ["0.0", " ", "00", "false", "0x0"] {
            assert!(!php_str_is_falsy(truthy), "expected truthy: {truthy:?}");
        }
    }

    #[test]
    fn casing_is_the_identity_test_not_a_letter_test() {
        // Uncased strings, including multibyte, satisfy both.
        for both in ["", "123", "-", "\u{00c4}\u{00e4}", "\u{65e5}\u{672c}\u{8a9e}"] {
            assert!(php_str_is_lowercase(both), "expected lowercase: {both:?}");
            assert!(php_str_is_uppercase(both), "expected uppercase: {both:?}");
        }
        for lower in ["abc", "a1", "snake_case", "1e5"] {
            assert!(php_str_is_lowercase(lower), "expected lowercase: {lower:?}");
            assert!(!php_str_is_uppercase(lower), "expected not uppercase: {lower:?}");
        }
        for upper in ["ABC", "A1", "SCREAMING_CASE", "1E5"] {
            assert!(php_str_is_uppercase(upper), "expected uppercase: {upper:?}");
            assert!(!php_str_is_lowercase(upper), "expected not lowercase: {upper:?}");
        }
        assert!(!php_str_is_lowercase("abC"));
        assert!(!php_str_is_uppercase("ABc"));
    }

    #[test]
    fn decimal_int_is_the_array_key_cast_not_is_numeric() {
        for yes in ["0", "1", "1234", "-1", "123", "9223372036854775807", "-9223372036854775808"] {
            assert!(php_str_is_decimal_int(yes), "expected decimal-int: {yes:?}");
        }
        // Numeric but not canonical: survives as a string key.
        for no in ["007", "+1", "00", "-0", "1.2", "18E+3", "1e5", " 1", "1 ", "0.0", "-0.0"] {
            assert!(php_is_numeric(no), "fixture check — {no:?} should be numeric");
            assert!(!php_str_is_decimal_int(no), "expected not decimal-int: {no:?}");
        }
        // One past PHP_INT_MAX.
        assert!(!php_str_is_decimal_int("9223372036854775808"));
        for no in ["", "abc", "-", "1,3", "0x1A", "１２３"] {
            assert!(!php_str_is_decimal_int(no), "expected not decimal-int: {no:?}");
        }
    }

    /// The spellings PHP 8.5/8.6 print for `(string) $f` under `precision = -1`
    /// (with PHPStan's `.0`), witnessed on the engine; `tests/it/php_oracle.rs`
    /// compares a wider set live.
    #[test]
    fn php_float_repr_matches_the_engines_spelling() {
        for (f, want) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (1.5, "1.5"),
            (-2.25, "-2.25"),
            (0.1, "0.1"),
            (0.30000000000000004, "0.30000000000000004"),
            (1e-4, "0.0001"),
            (1.5e-4, "0.00015"),
            (1e-5, "1.0E-5"),
            (3e-10, "3.0E-10"),
            (100.0, "100.0"),
            (123_456_789_012_345.0, "123456789012345.0"),
            (1e16, "10000000000000000.0"),
            (1.5e16, "15000000000000000.0"),
            (1e17, "1.0E+17"),
            (1e19, "1.0E+19"),
            (1e100, "1.0E+100"),
            (f64::MAX, "1.7976931348623157E+308"),
            (-f64::MAX, "-1.7976931348623157E+308"),
            (f64::from_bits(1), "5.0E-324"),
            (f64::MIN_POSITIVE, "2.2250738585072014E-308"),
            (f64::INFINITY, "INF"),
            (f64::NEG_INFINITY, "-INF"),
            (f64::NAN, "NAN"),
        ] {
            assert_eq!(php_float_repr(f), want, "{f:?}");
        }
    }
}
