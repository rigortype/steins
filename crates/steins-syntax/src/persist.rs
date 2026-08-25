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
//! The one hand-written `Deserialize` is [`EffectOrigin`]'s. Its
//! `Output`/`Exit` variants carry a `&'static str` keyword, and serde's
//! derive *implicitly borrows* every `&str` field from the input — a bound
//! (`'de: 'static`) no deserializer satisfies, and one `serde(with)` does not
//! lift. The inverse goes through [`EffectOriginWire`], a twin whose variant
//! and field names match the derived `Serialize`'s output exactly; the
//! keyword interns against the closed table the lowering constructs from, so
//! an unknown spelling is a decode error, never a leaked allocation.

use serde::Deserialize;

use crate::ast::{
    CallbackRef, ConstArgs, EffectOrigin, EffectRecv, NameRef, RefTarget, Span,
};

/// `&'static str` keyword serialization ([`EffectOrigin::Output`] /
/// [`EffectOrigin::Exit`]): the spelling, verbatim.
pub(crate) mod keyword {
    /// Every `keyword` spelling `lower_effect.rs` constructs. Adding one
    /// there without extending this table makes the steins-db round-trip
    /// tests fail loudly, which is the point.
    pub(crate) const KNOWN: [&str; 5] = ["echo", "print", "inline HTML", "exit", "die"];

    pub(crate) fn serialize<S: serde::Serializer>(
        v: &&'static str,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(v)
    }

    /// The `&'static str` a stored spelling denotes, or `None` for a
    /// spelling the lowering never wrote — the strict inverse's error case.
    pub(crate) fn intern(spelled: &str) -> Option<&'static str> {
        KNOWN.iter().find(|k| **k == spelled).copied()
    }
}

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

/// [`EffectOrigin`]'s wire twin: the same variants, the same field names, the
/// same (externally tagged) representation — differing only in the keyword
/// fields, which land as owned strings and intern on conversion. Kept
/// adjacent to nothing else on purpose: a new `EffectOrigin` variant that
/// misses this twin fails the steins-db round-trip tests at once.
#[derive(Deserialize)]
enum EffectOriginWire {
    Call { name: NameRef, span: Span, arg_targets: Option<Vec<RefTarget>>, const_args: ConstArgs },
    Output { keyword: String, span: Span },
    Exit { keyword: String, span: Span },
    MethodCall { receiver: EffectRecv, method: String, span: Span },
    Opaque { span: Span },
    HigherOrder {
        callee: NameRef,
        callbacks: Vec<(usize, CallbackRef)>,
        arg_count: usize,
        arg_targets: Vec<RefTarget>,
        const_args: ConstArgs,
        span: Span,
    },
    Callback { cbref: CallbackRef, span: Span },
}

impl<'de> serde::Deserialize<'de> for EffectOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let unknown =
            |kw: &str| serde::de::Error::custom(format!("unknown effect-origin keyword {kw:?}"));
        Ok(match EffectOriginWire::deserialize(deserializer)? {
            EffectOriginWire::Call { name, span, arg_targets, const_args } => {
                EffectOrigin::Call { name, span, arg_targets, const_args }
            }
            EffectOriginWire::Output { keyword: kw, span } => EffectOrigin::Output {
                keyword: keyword::intern(&kw).ok_or_else(|| unknown(&kw))?,
                span,
            },
            EffectOriginWire::Exit { keyword: kw, span } => EffectOrigin::Exit {
                keyword: keyword::intern(&kw).ok_or_else(|| unknown(&kw))?,
                span,
            },
            EffectOriginWire::MethodCall { receiver, method, span } => {
                EffectOrigin::MethodCall { receiver, method, span }
            }
            EffectOriginWire::Opaque { span } => EffectOrigin::Opaque { span },
            EffectOriginWire::HigherOrder {
                callee,
                callbacks,
                arg_count,
                arg_targets,
                const_args,
                span,
            } => EffectOrigin::HigherOrder {
                callee,
                callbacks,
                arg_count,
                arg_targets,
                const_args,
                span,
            },
            EffectOriginWire::Callback { cbref, span } => EffectOrigin::Callback { cbref, span },
        })
    }
}
