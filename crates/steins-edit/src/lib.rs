//! The Steins transform engine (ADR-0034): `EditPlan` transactions, code
//! preconditions proven against the inference engine, and dual verification.
//!
//! Layers:
//! - [`plan`] — the pure span+splice transaction ([`EditPlan`]); no inference
//!   dependency.
//! - [`diff`] — a minimal unified-diff renderer for dry-run display.
//! - [`transform`] — the shared vocabulary: [`Refusal`], [`CompletenessOracle`],
//!   [`TransformReport`], the [`Transform`] trait.
//! - [`common`] — machinery shared by the phpdoc transforms: reverse-sweep
//!   refusal reasons, candidate/value helpers, ADR-0029 phpdoc type rendering.
//! - [`regions`] — the region model (ADR-0047): pure config→region assignment
//!   ([`PartitionMap`]), threaded through the planners but not yet decided on by
//!   any of them — reserves the seam for scoped enumeration without changing a
//!   verdict.
//! - [`obstacles`] — project-global dynamic-code obstacle detection (ADR-0046
//!   §2): `eval` / dynamic-`include` sites that make "all callers proven"
//!   unknowable, the vendor presumption, and the `steins.toml` vouching valve.
//! - [`promote`] — first transform, phpdoc→native parameter promotion
//!   (ADR-0034 point 4 / ADR-0037): proves *all call sites flow the native
//!   type*, a precondition structurally unavailable to modular tools.
//! - [`honesty`] — second transform, phpdoc-honesty repair (ADR-0037 point 4 /
//!   ADR-0041 point 4): the inverse of promotion, widening a *lying*
//!   `@param`/`@return` to the proven truth from call-site / return evidence.
//! - [`envelope`] — third transform, `@throws` envelope seeding (issue #115 /
//!   ADR-0040): writes the proven-escape set behind `throw.undeclared` as
//!   declared `@throws` tags, creating or losslessly extending docblocks.
//! - [`effects_envelope`] — fifth transform, interop-envelope emission (issue
//!   #303 / ADR-0082 §7): sister of [`envelope`], writing the proven effect
//!   *bound* as upstream's purity tags (`@phpstan-impure <labels>` per
//!   declaration, `@phpstan-all-methods-pure` class-level) — nothing where a tag
//!   would be a lie or a no-op.
//! - [`loops`] — fourth transform, loop→`array_map` (ADR-0076 / ADR-0010's
//!   flagship): the first **effect-preconditioned** rewrite, gated on the engine
//!   *proving* the loop body's effect lane and throw set empty, via the effect/
//!   throw fixpoints restricted to the loop's own byte span.
//!
//! ADR-0034's dual verification (post-check: zero new diagnostics after apply;
//! oracle: every site transformed-or-refused) is the safety net the CLI wires in.

pub mod common;
pub mod diff;
pub mod effects_envelope;
pub mod envelope;
pub mod honesty;
pub mod loops;
pub mod obstacles;
pub mod plan;
pub mod promote;
pub mod regions;
pub mod transform;

pub use diff::unified_diff;
pub use effects_envelope::{EffectsEnvelope, plan_effects_envelope};
pub use envelope::{ThrowsEnvelope, plan_throws_envelope};
pub use honesty::{PhpdocHonesty, plan_phpdoc_honesty};
pub use loops::{LoopToArrayMap, LoopToArrayMapOptions, plan_loop_to_array_map};
pub use obstacles::{DynamismObstacles, VouchSet};
pub use plan::{ByteSpan, Edit, EditPlan, NewFile, PlanError};
pub use regions::{PartitionConfigError, PartitionMap, RegionId};
pub use promote::{PhpdocToNative, plan_phpdoc_to_native};
pub use transform::{
    AssertedAdmission, CompletenessOracle, Obstacle, Refusal, SiteRef, Transform, TransformReport,
};
