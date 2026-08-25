//! Content fingerprints (ADR-0092 §2): blake3 over explicitly tagged,
//! length-prefixed fields, domain-separated per kind. Never `Hash` output,
//! never serialization bytes — a fingerprint survives any layout change that
//! does not change the covered facts.

use std::fmt;

/// A 256-bit content fingerprint. Compared bitwise; rendered as 64 hex chars.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Length of the hex rendering ([`Fingerprint::to_hex`]).
    pub const HEX_LEN: usize = 64;

    pub(crate) fn from_hash(hash: blake3::Hash) -> Self { Self(*hash.as_bytes()) }

    /// Fingerprint of one raw blob under a domain string — for single-blob
    /// inputs like `composer.lock`, where there is only one field to tag.
    pub fn of_bytes(kind: &str, bytes: &[u8]) -> Self {
        let mut h = FieldHasher::new(kind);
        h.field("blob", bytes);
        h.finish()
    }

    pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }

    /// Lowercase hex, [`Fingerprint::HEX_LEN`] chars. Round-trips through
    /// [`Fingerprint::from_hex`].
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(Self::HEX_LEN);
        for b in self.0 {
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
            out.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
        }
        out
    }

    /// Strict inverse of [`Fingerprint::to_hex`]: exactly 64 lowercase hex
    /// chars, or `None`. Uppercase is rejected — the store never writes it,
    /// so accepting it would let two spellings name one generation.
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != Self::HEX_LEN || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16).unwrap() as u8;
            let lo = (chunk[1] as char).to_digit(16).unwrap() as u8;
            bytes[i] = (hi << 4) | lo;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.to_hex()) }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.to_hex())
    }
}

/// The one way fingerprints are computed. Every field lands in the stream as
/// `tag || u64_le(len) || bytes`, and the whole stream is domain-separated by
/// a per-kind context string (`"steins-gen/source"`, `"steins-gen/generation"`,
/// …). The length prefix makes field boundaries explicit — adjacent fields
/// cannot collide by concatenation — and the tags make field *identity*
/// explicit, so a fingerprint never depends on the order a struct happens to
/// lay its fields out in.
pub struct FieldHasher {
    inner: blake3::Hasher,
}

impl FieldHasher {
    /// Start a fingerprint under the domain `kind`. Two hashers with
    /// different kinds never collide, whatever their fields.
    pub fn new(kind: &str) -> Self { Self { inner: blake3::Hasher::new_derive_key(kind) } }

    /// Absorb one tagged field.
    pub fn field(&mut self, tag: &str, bytes: &[u8]) -> &mut Self {
        self.inner.update(tag.as_bytes());
        self.inner.update(&(bytes.len() as u64).to_le_bytes());
        self.inner.update(bytes);
        self
    }

    /// Absorb a `u32` field, little-endian.
    pub fn field_u32(&mut self, tag: &str, value: u32) -> &mut Self {
        self.field(tag, &value.to_le_bytes())
    }

    /// Absorb a `u64` field, little-endian.
    pub fn field_u64(&mut self, tag: &str, value: u64) -> &mut Self {
        self.field(tag, &value.to_le_bytes())
    }

    pub fn finish(&self) -> Fingerprint { Fingerprint::from_hash(self.inner.finalize()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let fp = Fingerprint::of_bytes("steins-gen/test", b"payload");
        assert_eq!(Fingerprint::from_hex(&fp.to_hex()), Some(fp));
    }

    #[test]
    fn from_hex_is_strict() {
        let fp = Fingerprint::of_bytes("steins-gen/test", b"payload");
        let hex = fp.to_hex();
        assert_eq!(Fingerprint::from_hex(&hex.to_uppercase()), None);
        assert_eq!(Fingerprint::from_hex(&hex[..63]), None);
        assert_eq!(Fingerprint::from_hex(&format!("{hex}0")), None);
        assert_eq!(Fingerprint::from_hex(""), None);
    }

    #[test]
    fn domains_separate() {
        let a = Fingerprint::of_bytes("steins-gen/a", b"same");
        let b = Fingerprint::of_bytes("steins-gen/b", b"same");
        assert_ne!(a, b);
    }

    #[test]
    fn length_prefix_blocks_concatenation_collisions() {
        let mut h1 = FieldHasher::new("steins-gen/test");
        h1.field("t", b"ab").field("t", b"c");
        let mut h2 = FieldHasher::new("steins-gen/test");
        h2.field("t", b"a").field("t", b"bc");
        assert_ne!(h1.finish(), h2.finish());
    }
}
