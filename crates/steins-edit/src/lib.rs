//! The Steins transform engine (ADR-0034): `EditPlan` transactions, code
//! preconditions proven against the inference engine, and dual verification.
//!
//! Layers:
//! - [`plan`] — the pure span+splice transaction ([`EditPlan`]) and its overlap
//!   discipline. No inference dependency.
//! - [`diff`] — a minimal unified-diff renderer for dry-run display.
//! - [`transform`] — the shared vocabulary: [`Refusal`], [`CompletenessOracle`],
//!   [`TransformReport`], the [`Transform`] trait.
//! - [`promote`] — the first transform, phpdoc→native parameter promotion
//!   (ADR-0034 point 4 / ADR-0037), which reaches into `steins-infer` to prove
//!   *all call sites flow the native type* — the precondition structurally
//!   unavailable to modular tools.
//!
//! ADR-0034's dual verification (post-check: zero new diagnostics after apply;
//! oracle: every site transformed-or-refused) is the safety net the CLI wires in.

pub mod diff;
pub mod plan;
pub mod promote;
pub mod transform;

pub use diff::unified_diff;
pub use plan::{ByteSpan, Edit, EditPlan, NewFile, PlanError};
pub use promote::{PhpdocToNative, plan_phpdoc_to_native};
pub use transform::{CompletenessOracle, Refusal, SiteRef, Transform, TransformReport};
