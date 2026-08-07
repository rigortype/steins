//! [`PhpStr`] — a PHP string value (ADR-0080).
//!
//! A PHP string is a **byte string**: it carries no encoding, `"\xC0"` is the
//! one-byte value `0xC0`, and `"\xC0" === "\xD0"` is `false`. Steins used to
//! lower string literals through `String::from_utf8_lossy`, which turns every
//! invalid-UTF-8 byte into one U+FFFD and therefore makes distinct PHP values
//! compare **equal** in the value lane. Because equality on lowered strings is
//! a proof premise — array-key identity (ADR-0062), constant folds, `===` value
//! facts, offset absence, match-arm reachability — that collapse manufactured
//! wrong answers in both directions (issue #208: a `call.on-null` false
//! positive, a suppressed `offset.missing`, and wrong `strlen`/`count` folds).
//!
//! This type is the fix by construction: every carrier of a lowered PHP string
//! holds a `PhpStr`, and equality is byte equality, so no consumer has to
//! remember a guard.

/// A PHP string value: an arbitrary byte string.
///
/// # Canonical form
///
/// The inner representation has two arms: one holds bytes that are **not**
/// valid UTF-8, the other holds bytes that are. The constructors maintain that
/// split, so there is exactly one representation per value and the common ASCII
/// path keeps a plain `String` with no extra allocation.
///
/// Equality, ordering and hashing nonetheless go through [`PhpStr::as_bytes`]
/// rather than deriving over the arms. Two reasons: a derived `Ord` would sort
/// every `Utf8` before every `Bytes` instead of byte-lexicographically, and
/// routing through the bytes keeps equality correct even if some future
/// constructor forgets to canonicalize.
///
/// The order is *representational* — it gives `Fact` its set semantics, exactly
/// as [`crate::Val`]'s does. PHP-level `==` / `===` live in the condition
/// evaluator, never on this type.
#[derive(Debug, Clone)]
pub struct PhpStr(Repr);

#[derive(Debug, Clone)]
enum Repr {
    /// Valid UTF-8 — the overwhelmingly common case.
    Utf8(String),
    /// Not valid UTF-8. Never holds bytes that would pass `from_utf8`.
    Bytes(Vec<u8>),
}

impl PhpStr {
    /// The empty PHP string.
    #[must_use]
    pub const fn new() -> Self {
        Self(Repr::Utf8(String::new()))
    }

    /// Lower raw literal bytes, as the parser delivers them.
    ///
    /// This is the constructor the syntax layer's `Literal::String` arm uses;
    /// it is the one place the UTF-8 question is asked, and it never loses a
    /// byte.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(s) => Self(Repr::Utf8(s.to_owned())),
            Err(_) => Self(Repr::Bytes(bytes.to_vec())),
        }
    }

    /// Lower owned bytes without copying when they are valid UTF-8.
    #[must_use]
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(s) => Self(Repr::Utf8(s)),
            Err(e) => Self(Repr::Bytes(e.into_bytes())),
        }
    }

    /// The value's bytes — what PHP itself would call the string.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match &self.0 {
            Repr::Utf8(s) => s.as_bytes(),
            Repr::Bytes(b) => b,
        }
    }

    /// The value as `&str`, or `None` when it is not valid UTF-8.
    ///
    /// The name lanes (class / function / method names, effect labels, include
    /// paths, every `String`-keyed index) read through this and answer *silence*
    /// on `None`: a byte-string name resolves to nothing, which is both the
    /// sound direction and what PHP would do with it in practice.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match &self.0 {
            Repr::Utf8(s) => Some(s),
            Repr::Bytes(_) => None,
        }
    }

    /// Whether the value is valid UTF-8 (i.e. [`PhpStr::as_str`] answers).
    #[must_use]
    pub const fn is_utf8(&self) -> bool {
        matches!(self.0, Repr::Utf8(_))
    }

    /// The value's length in **bytes** — PHP's `strlen`, not a character count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }

    /// Whether the value is the empty string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    /// Spell the value as PHP source would (ADR-0080 §2.7), single-quoted.
    #[must_use]
    pub fn to_php_literal(&self) -> String {
        self.render_with('\'')
    }

    /// Spell the value as PHP source would, using `quote` for the ordinary
    /// UTF-8 case so each diagnostic keeps the quoting style it already used.
    ///
    /// A value that is **not** valid UTF-8 is always double-quoted with `\xNN`
    /// escapes, because that is the only PHP spelling that can carry those
    /// bytes — and because a message printing the lossy `'�'` where the source
    /// says `"\xC0"` names something the reader cannot act on.
    #[must_use]
    pub fn render_with(&self, quote: char) -> String {
        if let Some(s) = self.as_str() {
            return format!("{quote}{s}{quote}");
        }
        let mut out = String::from("\"");
        for &b in self.as_bytes() {
            match b {
                b'"' => out.push_str("\\\""),
                b'\\' => out.push_str("\\\\"),
                0x20..=0x7E => out.push(b as char),
                _ => out.push_str(&format!("\\x{b:02X}")),
            }
        }
        out.push('"');
        out
    }
}

/// Lets every byte-oriented PHP predicate (`php_is_numeric`, `StrPreds::of`, …)
/// take a `PhpStr` and a `&str` through one signature.
impl AsRef<[u8]> for PhpStr {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Default for PhpStr {
    fn default() -> Self {
        Self::new()
    }
}

impl From<String> for PhpStr {
    fn from(s: String) -> Self {
        Self(Repr::Utf8(s))
    }
}

impl From<&str> for PhpStr {
    fn from(s: &str) -> Self {
        Self(Repr::Utf8(s.to_owned()))
    }
}

impl PartialEq for PhpStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for PhpStr {}

impl PartialOrd for PhpStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PhpStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl std::hash::Hash for PhpStr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

/// Compare against a Rust string slice by bytes — the ergonomic form for the
/// many sites testing a value against a fixed ASCII spelling (`"0"`, `""`,
/// a builtin's name).
impl PartialEq<str> for PhpStr {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for PhpStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash_of(s: &PhpStr) -> u64 {
        let mut h = DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    }

    /// The defect this type exists to kill: two distinct single invalid bytes
    /// used to decode to the same U+FFFD-bearing `String` (issue #187/#208).
    #[test]
    fn distinct_invalid_bytes_are_distinct_values() {
        let c0 = PhpStr::from_bytes(&[0xC0]);
        let d0 = PhpStr::from_bytes(&[0xD0]);
        assert_ne!(c0, d0);
        assert_ne!(hash_of(&c0), hash_of(&d0));
        assert!(!c0.is_utf8());
        assert_eq!(c0.len(), 1, "PHP strlen(\"\\xC0\") is 1, not U+FFFD's 3");
    }

    /// A genuine U+FFFD in the source is its own value, distinct from any
    /// invalid byte — the case the #187 guard had to punish and no longer does.
    #[test]
    fn a_real_replacement_char_is_not_an_invalid_byte() {
        let real = PhpStr::from("\u{FFFD}");
        assert!(real.is_utf8());
        assert_ne!(real, PhpStr::from_bytes(&[0xC0]));
        assert_eq!(real.len(), 3);
    }

    #[test]
    fn valid_utf8_round_trips_through_as_str() {
        let s = PhpStr::from_bytes("héllo".as_bytes());
        assert_eq!(s.as_str(), Some("héllo"));
        assert_eq!(s, PhpStr::from("héllo"));
    }

    #[test]
    fn a_byte_string_declines_the_name_lane() {
        assert_eq!(PhpStr::from_bytes(&[0xC0]).as_str(), None);
    }

    #[test]
    fn order_is_byte_lexicographic_across_the_arms() {
        let a = PhpStr::from("a");
        let hi = PhpStr::from_bytes(&[0xC0]);
        assert!(a < hi, "0x61 sorts before 0xC0 regardless of representation");
        assert!(PhpStr::from_bytes(&[0xC0]) < PhpStr::from_bytes(&[0xD0]));
    }

    #[test]
    fn from_vec_canonicalizes_like_from_bytes() {
        assert_eq!(PhpStr::from_vec(vec![0xC0]), PhpStr::from_bytes(&[0xC0]));
        assert_eq!(PhpStr::from_vec(b"ok".to_vec()), PhpStr::from("ok"));
        assert!(PhpStr::from_vec(b"ok".to_vec()).is_utf8());
    }

    #[test]
    fn rendering_spells_invalid_bytes_as_php_escapes() {
        assert_eq!(PhpStr::from("ok").to_php_literal(), "'ok'");
        assert_eq!(PhpStr::from_bytes(&[0xC0]).to_php_literal(), r#""\xC0""#);
        assert_eq!(PhpStr::from_bytes(&[b'a', 0xD0]).to_php_literal(), r#""a\xD0""#);
    }

    /// The quote choice belongs to the caller for the ordinary case, so each
    /// diagnostic keeps its existing spelling; a byte string ignores it,
    /// because only a double-quoted PHP literal can carry `\xNN`.
    #[test]
    fn the_quote_is_the_callers_for_utf8_only() {
        assert_eq!(PhpStr::from("ok").render_with('"'), r#""ok""#);
        assert_eq!(PhpStr::from("ok").render_with('\''), "'ok'");
        assert_eq!(PhpStr::from_bytes(&[0xC0]).render_with('\''), r#""\xC0""#);
    }

    #[test]
    fn empty_is_the_default() {
        assert!(PhpStr::new().is_empty());
        assert_eq!(PhpStr::default(), PhpStr::from(""));
    }
}
