//! The four-layer value domain (ADR-0035).
//!
//! ```text
//! 1. Singleton   — one concrete value (the maximal sieve)
//! 2. OneOf       — a finite value set (cap 8)
//! 3. Refined     — base type + refinement (predicate bitset / int interval)
//! 4. General     — the bare base type
//! ```
//!
//! Arrays are a separate stratum ([`Fact::Shape`]/[`ShapeFact`], ADR-0062): the degenerate
//! shape (no fields, untyped unsealed tail) is plain `array`, so there's no array-`General`,
//! and an over-`CAP` array set widens to a computed shape summary rather than being dropped.
//! The crate's algebra: joins with computed layer descent (precision loss is measured, never
//! guessed), extensional membership (`admits`), and trinary queries via the unified
//! [`Certainty`].
//!
//! Invariants are enforced by constructors, checked by property tests, and proved for every
//! value by the Lean 4 spec in `spike/lean-domain` (ADR-0059, differentially checked against
//! `tests/lean_vectors.rs`):
//! - **Soundness of join**: `γ(a) ∪ γ(b) ⊆ γ(join(a, b))`; may widen, never lose members.
//!   `None` means "not representable", so the caller drops it.
//! - **Canonical forms**: `OneOf` is sorted/deduped, `2..=CAP` members; a `Refined` always
//!   carries real knowledge, or it *is* `General`.
//! - **Trinary discipline**: queries return [`Certainty`]; `Maybe` is honest wherever the
//!   set admits both outcomes (ADR-0031).
//!
//! `steins-infer` re-exports [`Certainty`] project-wide (ADR-0031) and builds its
//! environment on [`Fact`].

mod certainty;
mod fact;
mod php;
mod php_str;
mod preds;
mod range;
mod shape;
mod value;

pub use certainty::Certainty;
pub use fact::{CAP, Fact, Refinement, UnionArm};
pub use php::{
    php_is_falsy, php_is_numeric, php_str_is_decimal_int, php_str_is_lowercase,
    php_str_is_uppercase,
};
pub use php_str::PhpStr;
pub use preds::StrPreds;
pub use range::IntRange;
pub use shape::{
    Cover, CoverFlavor, KeyClass, Presence, SHAPE_WIDTH_LIMIT, ShapeFact, Tail, array_is_list,
    keys_are_a_list,
};
pub use value::{Base, Key, Val};
