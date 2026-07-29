//! PHP value semantics the domain depends on, implemented to the letter of
//! PHP 8.x and (where history was treacherous) verified against the real
//! engine by `tests/php_oracle.rs` — the ask-the-real-thing discipline
//! applied to unit semantics.

/// PHP 8 `is_numeric()`.
///
/// Grammar: optional leading whitespace (`" \t\n\r\v\f"`), optional sign,
/// then an integer (`digits`), or a float (`digits "." digits?` |
/// `digits? "." digits`), optionally followed by an exponent
/// (`[eE] sign? digits`); trailing whitespace is allowed (PHP >= 8.0).
/// Hex/binary/octal strings are NOT numeric. At least one digit must appear
/// in the mantissa.
#[must_use]
pub fn php_is_numeric(s: &str) -> bool {
    const WS: &[char] = &[' ', '\t', '\n', '\r', '\u{0B}', '\u{0C}'];
    let s = s.trim_start_matches(WS).trim_end_matches(WS);
    let b = s.as_bytes();
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

/// PHP falsiness of a *string*: exactly `""` and `"0"` are falsy.
/// (`"0.0"`, `" "`, and `"00"` are all truthy — the classic traps.)
#[must_use]
pub fn php_str_is_falsy(s: &str) -> bool {
    s.is_empty() || s == "0"
}

/// PHP's `strtolower($s) === $s` — the definition of PHPStan's
/// `lowercase-string` (`AccessoryLowercaseStringType`), not "made of lowercase
/// letters": a string with no cased character at all (`""`, `"123"`) qualifies.
///
/// Byte-oriented on the ASCII letters, because that is what the engine does:
/// since PHP 8.2 `strtolower()` is locale-independent and maps exactly `A-Z`
/// to `a-z`, leaving every other byte alone. A UTF-8 `"Ä"` is therefore a
/// `lowercase-string` under this rule *and* under the engine — its bytes are
/// all >= 0x80, so no `A-Z` byte occurs. `tests/php_oracle.rs` asks the real
/// engine, including the multibyte cases.
#[must_use]
pub fn php_str_is_lowercase(s: &str) -> bool {
    !s.as_bytes().iter().any(u8::is_ascii_uppercase)
}

/// PHP's `strtoupper($s) === $s` — the mirror of [`php_str_is_lowercase`], and
/// the definition of PHPStan's `uppercase-string`. The two are *not* exclusive:
/// a string with no cased character satisfies both.
#[must_use]
pub fn php_str_is_uppercase(s: &str) -> bool {
    !s.as_bytes().iter().any(u8::is_ascii_lowercase)
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
        // Uncased strings satisfy both — the lattice fact the fixtures state.
        // Multibyte: UTF-8 bytes are all >= 0x80, so no ASCII-cased byte occurs
        // and `strtolower`/`strtoupper` leave the value alone (oracle-checked).
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
        // A single cased character disqualifies the whole string.
        assert!(!php_str_is_lowercase("abC"));
        assert!(!php_str_is_uppercase("ABc"));
    }
}
