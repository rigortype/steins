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

/// PHP falsiness of a scalar value, expressed over the domain's [`Val`].
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
}
