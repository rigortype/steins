//! The `persist` feature's serde exceptions (ADR-0092 §2, issue #487): the
//! places a derived impl would either fail to compile or fail to invert.
//!
//! Everything else on the lowered representation is `#[cfg_attr(feature =
//! "persist", derive(...))]` in `ast.rs`/`tree.rs`; this module carries only
//! the exceptions, so the exceptions stay enumerable. The payloads are a
//! cache, not an interchange format (ADR-0092 §2): the artifact schema
//! version — never in-band negotiation — governs their evolution, and every
//! decode failure is the reader's miss.
//!
//! A `&'static str` field would be one: serde's derive implicitly borrows
//! every `&str` field from the input, a bound no deserializer satisfies, and
//! `serde(with)` does not lift it. That is why an [`crate::EffectOrigin`]'s
//! keyword is an enum ([`crate::OutputKeyword`], [`crate::ExitKeyword`],
//! [`crate::IncludeKeyword`]) and not the spelling itself (issue #829).

/// `f64` value fields ([`crate::ArgValue::Float`]): serialized as the
/// IEEE-754 bit pattern (`u64`), so every value — the non-finite floats a
/// literal like `1e999` lowers to included, which JSON cannot spell —
/// round-trips exactly.
pub(crate) mod f64_bits {
    pub(crate) fn serialize<S: serde::Serializer>(
        v: &f64,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(v.to_bits())
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<f64, D::Error> {
        Ok(f64::from_bits(<u64 as serde::Deserialize>::deserialize(deserializer)?))
    }
}
