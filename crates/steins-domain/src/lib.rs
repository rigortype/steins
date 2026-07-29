//! The four-layer value domain (ADR-0035).
//!
//! ```text
//! 1. Singleton   — one concrete value (the maximal sieve)
//! 2. OneOf       — a finite value set (cap 8)
//! 3. Refined     — base type + refinement (predicate bitset / int interval)
//! 4. General     — the bare base type
//! ```
//!
//! Arrays get their own stratum rather than a scalar layer: [`Fact::Shape`]
//! carries one canonical [`ShapeFact`] (ADR-0062), and the *degenerate* shape —
//! no fields, an untyped unsealed tail — is plain `array`, so there is no
//! array-`General` form. Layer descent is total for arrays too: an over-`CAP`
//! set of arrays widens to a **computed** shape summary instead of being
//! dropped.
//!
//! The crate owns the *algebra*: joins with **computed** layer descent
//! (widening a finite set derives the predicate summary its members satisfy —
//! precision loss is measured, never guessed), extensional membership
//! (`admits`), and trinary queries in the unified [`Certainty`].
//!
//! Design invariants, enforced by constructors, checked by property tests, and
//! **proved for every value** by the Lean 4 specification in
//! `spike/lean-domain` (ADR-0059) — which this crate is differentially checked
//! against by `tests/lean_vectors.rs`:
//!
//! - **Soundness of join**: `γ(a) ∪ γ(b) ⊆ γ(join(a, b))` — a join may lose
//!   precision (widen), never members. `join` returning `None` means "not
//!   representable" (e.g. mixed scalar bases); the caller drops the fact,
//!   which is the safe side (γ = everything).
//! - **Canonical forms**: `OneOf` is sorted/deduped with `2..=CAP` members;
//!   a `Refined` always carries real knowledge (non-empty predicate set /
//!   non-full interval) — otherwise it *is* the `General` form.
//! - **Trinary discipline**: queries return [`Certainty`]; `Maybe` is the
//!   honest answer wherever the set admits both outcomes (ADR-0031).
//!
//! `steins-infer` re-exports this crate's [`Certainty`] as the one trinary
//! project-wide (ADR-0031); its stage-1 env migrates onto [`Fact`] with
//! branch-analysis stage 2.

mod certainty;
mod fact;
mod php;
mod preds;
mod range;
mod shape;
mod value;

pub use certainty::Certainty;
pub use fact::{Fact, Refinement, CAP};
pub use php::{php_is_falsy, php_is_numeric, php_str_is_lowercase, php_str_is_uppercase};
pub use preds::StrPreds;
pub use range::IntRange;
pub use shape::{
    Cover, CoverFlavor, KeyClass, Presence, SHAPE_WIDTH_LIMIT, ShapeFact, Tail, array_is_list,
};
pub use value::{Base, Key, Val};
