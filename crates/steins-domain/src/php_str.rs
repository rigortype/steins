//! [`PhpStr`] — a PHP string value (ADR-0080).
//!
//! A PHP string is a **byte string**: it carries no encoding, `"\xC0"` is the one-byte value
//! `0xC0`, and `"\xC0" === "\xD0"` is `false`. Lossy UTF-8 decoding collapses distinct
//! invalid-byte values into the same string and silently corrupts array-key identity
//! (ADR-0062), constant folds, `===` facts, offset absence, and match-arm reachability
//! (issue #208).
//!
//! This type is the fix by construction: every carrier of a lowered PHP string holds a
//! `PhpStr`, with byte equality, so no consumer needs a guard.

/// A PHP string value: an arbitrary byte string.
///
/// # Canonical form
///
/// The inner representation has two arms — valid UTF-8 and not — kept to exactly one per
/// value by the constructors; the common ASCII path stays a plain `String` with no extra
/// allocation.
///
/// Equality, ordering and hashing go through [`PhpStr::as_bytes`] rather than deriving over
/// the arms — a derived `Ord` would sort every `Utf8` before every `Bytes`, and routing
/// through bytes stays correct even if a future constructor forgets to canonicalize. Order
/// is *representational*, giving `Fact` set semantics like [`crate::Val`]; PHP-level
/// `==`/`===` live in the condition evaluator, never here.
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
    /// The syntax layer's `Literal::String` arm uses this — the one place the UTF-8 question
    /// is asked, and it never loses a byte.
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
    /// Name lanes (class/function/method names, effect labels, include paths, `String`-keyed
    /// indices) read through this and answer *silence* on `None` — the sound direction, and
    /// what PHP does with it anyway.
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

    /// Spell the value as PHP source would, using `quote` for the ordinary UTF-8 case so
    /// each diagnostic keeps its existing quoting style.
    ///
    /// Non-UTF-8 values are always double-quoted with `\xNN` escapes — the only PHP spelling
    /// that can carry those bytes (vs. an unactionable lossy `'�'` where the source says
    /// `"\xC0"`).
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

/// Lets byte-oriented predicates (`php_is_numeric`, `StrPreds::of`, …) take a `PhpStr` and a
/// `&str` through one signature.
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

/// Compare against a Rust string slice by bytes — for the many sites testing a value against
/// a fixed ASCII spelling (`"0"`, `""`, a builtin's name).
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

// The `persist` serde impls (ADR-0092 §2, issue #487). Hand-written, not
// derived, for two reasons: the canonical-form invariant (the `Utf8` arm holds
// exactly the values `from_utf8` admits) must survive deserialization, which a
// derived `Deserialize` on the private enum would not guarantee; and the
// common UTF-8 value should cost a JSON string, not an array of byte numbers.
// A UTF-8 value serializes as a string, a non-UTF-8 one as a byte sequence,
// and both deserialize through the canonicalizing constructors — so a
// round-trip is byte-exact and always canonical. A cache format (no external
// consumer): the artifact schema version governs its evolution.
#[cfg(feature = "persist")]
impl serde::Serialize for PhpStr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.as_str() {
            Some(s) => serializer.serialize_str(s),
            None => serializer.serialize_bytes(self.as_bytes()),
        }
    }
}

#[cfg(feature = "persist")]
impl<'de> serde::Deserialize<'de> for PhpStr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = PhpStr;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string or a byte sequence")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<PhpStr, E> {
                Ok(PhpStr::from(v))
            }

            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<PhpStr, E> {
                Ok(PhpStr::from_bytes(v))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<PhpStr, A::Error> {
                let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
                while let Some(b) = seq.next_element::<u8>()? {
                    bytes.push(b);
                }
                Ok(PhpStr::from_vec(bytes))
            }
        }
        deserializer.deserialize_any(Visitor)
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

    // The defect this type kills: distinct invalid bytes decoding to the same U+FFFD
    // `String` (issue #187/#208).
    #[test]
    fn distinct_invalid_bytes_are_distinct_values() {
        let c0 = PhpStr::from_bytes(&[0xC0]);
        let d0 = PhpStr::from_bytes(&[0xD0]);
        assert_ne!(c0, d0);
        assert_ne!(hash_of(&c0), hash_of(&d0));
        assert!(!c0.is_utf8());
        assert_eq!(c0.len(), 1, "PHP strlen(\"\\xC0\") is 1, not U+FFFD's 3");
    }

    // A genuine U+FFFD is distinct from any invalid byte (#187 guard case).
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
