//! The Steins transform engine (ADR-0034): `EditPlan` transactions, code
//! preconditions proven against the inference engine, and dual verification.
//!
//! Layers:
//! - [`plan`] — the pure span+splice transaction ([`EditPlan`]) and its overlap
//!   discipline. No inference dependency.
//! - [`diff`] — a minimal unified-diff renderer for dry-run display.
//! - [`transform`] — the shared vocabulary: [`Refusal`], [`CompletenessOracle`],
//!   [`TransformReport`], the [`Transform`] trait.
//! - [`common`] — the machinery the two phpdoc transforms genuinely share: the
//!   reverse-sweep refusal reasons, candidate/value helpers, and the value-domain
//!   → ADR-0029 phpdoc **type rendering**.
//! - [`obstacles`] — project-global dynamic-code obstacle detection (ADR-0046 §2):
//!   `eval` / dynamic-`include` sites that make "all callers proven" unknowable,
//!   the vendor presumption, and the `steins.toml` vouching valve.
//! - [`promote`] — the first transform, phpdoc→native parameter promotion
//!   (ADR-0034 point 4 / ADR-0037), which reaches into `steins-infer` to prove
//!   *all call sites flow the native type* — the precondition structurally
//!   unavailable to modular tools.
//! - [`honesty`] — the second transform, phpdoc-honesty repair (ADR-0037 point 4
//!   / ADR-0041 point 4): the inverse of promotion, widening a *lying*
//!   `@param`/`@return` to the proven truth from call-site / return evidence.
//!
//! ADR-0034's dual verification (post-check: zero new diagnostics after apply;
//! oracle: every site transformed-or-refused) is the safety net the CLI wires in.

pub mod common;
pub mod diff;
pub mod honesty;
pub mod obstacles;
pub mod plan;
pub mod promote;
pub mod transform;

pub use diff::unified_diff;
pub use honesty::{PhpdocHonesty, plan_phpdoc_honesty};
pub use obstacles::{DynamismObstacles, VouchSet};
pub use plan::{ByteSpan, Edit, EditPlan, NewFile, PlanError};
pub use promote::{PhpdocToNative, plan_phpdoc_to_native};
pub use transform::{
    CompletenessOracle, Obstacle, Refusal, SiteRef, Transform, TransformReport,
};
