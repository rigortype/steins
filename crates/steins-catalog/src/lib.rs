//! Curated signatures and effect entries for PHP builtins and extensions.
//!
//! # Folding gate
//!
//! [`foldable`] is the hand-curated ADR-0008 allowlist: admitted only when pure
//! and deterministic on the concrete path, else it widens (locale-, timezone-,
//! encoding-, global-, nondeterminism-sensitive functions stay excluded).
//!
//! `REFUSED` and `UNVERIFIED` fold on a proven 64-bit engine but
//! decline on 32-bit; a refused row has a recorded divergence, an unverified
//! row has none (see [`PortabilityClass`]). Other exclusions and their evidence:
//!
//! * `strtotime`/`date`/`idate` and their siblings `gmdate`/`gmmktime`/
//!   `getdate`/`localtime` are `nondet.time`, timezone-coupled even with
//!   explicit timestamps — and omitting the timestamp reads the clock, which is
//!   the argument-blind upper bound the row states (ADR-0021).
//! * `mb_*` depends on `mbstring.internal_encoding`; php-wasm 0.1.0 lacks it.
//! * `strcmp`/`strcasecmp` promise only a sign, not `memcmp`'s
//!   implementation-defined magnitude.
//! * `number_format` stays conservatively excluded despite no probed divergence.
//! * `bin2hex` is excluded per its ADR-0056 empty-in/empty-out return-fact
//!   refusal (`docs/research/phpsrc-mining/return_facts.toml`).

/// The PHP minor version the builtin catalog is pinned to (`major`, `minor`):
/// mining data (`docs/research/phpsrc-mining/hierarchy.toml`, pin
/// `6bc7c26cf6…`) is cross-checked against **PHP 8.5.8**, so reported
/// class-hierarchy edges are those of the `8.5` line.
///
/// ADR-0052 amendment A11: a catalog-backed is-a verdict used for **arm
/// deletion** is trustworthy only when the project's own PHP is on this same
/// minor line. On a skew, the narrowing engine demotes such a verdict to
/// `Unknown` (FP-safe). Only `(major, minor)` is pinned — builtin type edges
/// are stable within a minor line.
pub const PINNED_PHP: (u16, u16) = (8, 5);

/// Builtin class-hierarchy table, from `docs/research/phpsrc-mining/hierarchy.toml`
/// via `cargo xtask gen-catalog`. Consulted only by [`builtin_class_supers`].
mod hierarchy_generated;

/// Builtin class **display-name** table, from the same mining data — lowercased
/// key → the casing php-src declares. Consulted only by [`builtin_class_display`].
mod display_names_generated;

/// Builtin return-fact refinement table (ADR-0056), from
/// `docs/research/phpsrc-mining/return_facts.toml`. Consulted only by
/// [`return_fact`]. May be empty.
mod return_facts_generated;

/// **Resource-return** table (ADR-0056 §8), from
/// `docs/research/phpsrc-mining/resource_returns.toml`. Consulted only by
/// [`resource_return`].
mod resource_returns_generated;

/// Builtin **per-parameter facts** (issue #382), from
/// `docs/research/phpsrc-mining/param_facts.toml` — the engine's own arginfo,
/// which is the independent source [`out_params`] and [`invocation_shape`] are
/// checked against. Consulted by [`param_facts`] and [`param_facts_mined`].
mod param_facts_generated;

/// Builtin declared-return floor (ADR-0069, issues #73/#79), from
/// `docs/research/phpstan-mining/declared_returns.toml`. Consulted only by
/// [`declared_return`] and [`declared_return_changed_at`].
mod declared_returns_generated;

// Capture-group structure of a literal PCRE pattern (issue #149). Carries its
// own module doc, so this stays a plain comment to avoid merging headers.
pub mod preg;

mod fold;
pub use fold::{
    PortabilityClass, Refusal, RefusalAxis, foldable, foldable_entry_count, portability_class,
    portable, portable_names, refusal, refused_names, unverified_names,
};

mod effects;
pub use effects::{
    StreamTarget, WrittenWhen, by_value_arg, callables_in_array_param, effect_labels,
    method_effect_labels, narrowed_stream_labels, out_param_written_when, out_params,
    variadic_tail_is_data,
};

mod labels;
pub use labels::{
    LabelIntent, LabelRegistry, RetiredLabel, core_roots, is_core_label, is_known_label,
    known_labels, nearest_label, retired_label, subsumes,
};

mod builtins;
pub use builtins::{
    ArgSource, FailureArms, FailureCause, Invocation, InvocationShape, ParamFacts,
    builtin_class_display, builtin_class_supers, builtin_exception_parent, builtin_throws,
    declared_return, declared_return_changed_at, failure_arms, hierarchy_entry_count,
    invocation_shape, param_facts, param_facts_mined, resource_return, return_fact,
};
