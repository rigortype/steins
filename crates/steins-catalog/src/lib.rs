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
//! * `strtotime`/`date`/`idate` and their siblings `gmdate`/`gmmktime`/`mktime`/
//!   `getdate`/`localtime` read the default timezone (the zone cell, ADR-0101 §3.14), and the
//!   clock too where the timestamp is omitted: the row is the upper bound over both.
//! * `mb_*` depends on `mbstring.internal_encoding`; php-wasm 0.1.0 lacks it.
//! * `strcmp`/`strcasecmp` promise only a sign, not `memcmp`'s
//!   implementation-defined magnitude.
//! * `number_format` stays conservatively excluded despite no probed divergence; the effect
//!   lane certifies it pure at a call site (ADR-0101 §4), which is not the fold's permission.
//! * `bin2hex` is excluded per its ADR-0056 empty-in/empty-out return-fact
//!   refusal (`docs/research/phpsrc-mining/return_facts.toml`).

/// The PHP minor version the builtin catalog is pinned to (`major`, `minor`):
/// the class hierarchy (`docs/research/phpsrc-mining/hierarchy.toml`) is mined from
/// the stubs at php-src tag **`php-8.5.11`**, so a row is a class PHP 8.5 declares when
/// its extension is built in, and the reported hierarchy edges are those of the `8.5`
/// line. Membership is the pin's, not the running engine's: `new \SNMPException` is
/// answered by its row on a build without ext-snmp, and `new \Uri\InvalidUriException`
/// on an 8.4 runtime, exactly as the function rows are. `steins doctor` reports the skew
/// and `require catalog-pin-match` refuses it.
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

/// Builtin class **second-name** table, from the same mining data — lowercased
/// second name → the declared name of the class both spellings name. Consulted
/// only by [`builtin_class_alias`], and through it by [`builtin_class_supers`]
/// and [`builtin_class_display`].
mod class_aliases_generated;

/// Builtin return-fact refinement table (ADR-0056), from
/// `docs/research/phpsrc-mining/return_facts.toml`. Consulted only by
/// [`return_fact`]. May be empty.
mod return_facts_generated;

/// **Resource-return** table (ADR-0056 §8), from
/// `docs/research/phpsrc-mining/resource_returns.toml`. Consulted only by
/// [`resource_return`].
mod resource_returns_generated;

/// **Resource-parameter** table (ADR-0097 §2.5), from
/// `docs/research/phpsrc-mining/resource_params.toml` — the consumer twin of the
/// table above. Consulted only by [`resource_param`].
mod resource_params_generated;

/// Builtin **per-parameter facts** (issue #382), from
/// `docs/research/phpsrc-mining/param_facts.toml` — the engine's own arginfo,
/// which is the independent source [`out_params`] and [`invocation_shape`] are
/// checked against. Consulted by [`param_facts`] and [`param_facts_mined`].
mod param_facts_generated;

/// Builtin declared-return floor (ADR-0069, issues #73/#79), from
/// `docs/research/phpstan-mining/declared_returns.toml`. Consulted only by
/// [`declared_return`] and [`declared_return_changed_at`].
mod declared_returns_generated;

/// Builtin declared-return floor, `Class::method`-keyed (ADR-0069, issue #673),
/// from `docs/research/phpstan-mining/declared_method_returns.toml`. Consulted only
/// by [`declared_method_return`] and [`declared_method_return_changed_at`].
mod declared_method_returns_generated;

/// The **migrated-class** table (ADR-0097 §2.6), from
/// `docs/research/phpstan-mining/migrated_resource_classes.toml` — every class
/// the pinned engine declares where PHPStan's functionMap still says `resource`.
/// Consulted only by [`is_migrated_resource_class`].
mod migrated_resource_classes_generated;

/// **Engine constants** (ADR-0094 §2, issue #598), from
/// `docs/research/phpsrc-mining/constants.toml` — the spec-fixed value of every
/// constant the mined build's extensions register. Consulted only by
/// [`engine_constant`]. The host-dependent, width and version constants are NOT
/// here: ADR-0094 §3 answers those by class, in the resolver.
mod constants_generated;

// Capture-group structure of a literal PCRE pattern (issue #149). Carries its
// own module doc, so this stays a plain comment to avoid merging headers.
pub mod preg;

// Each re-export list is one name per line, in byte order (so types first): a
// new table's name is then one added line, not a rewrapped block (issue #777).
mod fold;
pub use fold::{
    FoldAllocation,
    PortabilityClass,
    Refusal,
    RefusalAxis,
    fold_allocation,
    foldable,
    foldable_entry_count,
    portability_class,
    portable,
    portable_names,
    refusal,
    refused_names,
    unverified_names,
};

mod effects;
pub use effects::{
    CallbackCarrier,
    CallbackCarriers,
    CarrierShape,
    StreamTarget,
    WrittenWhen,
    by_value_arg,
    by_value_arg_frame,
    callables_in_array_param,
    callback_carriers,
    certified_at_call_site,
    effect_labels,
    engine_constructor_by_value,
    final_method_effect_labels,
    method_effect_labels,
    narrowed_output_labels,
    narrowed_setlocale_labels,
    narrowed_stream_labels,
    out_param_written_when,
    out_params,
    pure_at_arity,
    variadic_tail_is_data,
};

mod setting;
pub use setting::{IniAccess, IniCall, SettingCell, ini_call, ini_cell, narrowed_ini_labels};

mod setting_reads;
pub use setting_reads::{
    ClockGate, DateGate, GateArg, PrecisionGate, RenderDepth, Rendered, SettingReadGate,
    clock_gate, date_gate, date_method_gate, precision_gate, setting_read_gate,
};

mod knowledge;
pub use knowledge::{
    FlagGatedThrow,
    flag_gated_throw,
    knows,
    throws_at_arity,
    throws_of,
    throws_of_literals,
};

mod reach;
pub use reach::{
    ArgReach,
    FormatReading,
    MethodReachRow,
    PrintfFamily,
    ReachRow,
    arg_reach,
    format_reach,
    format_reads_locale,
    method_arg_reach,
    printf_family,
    read_format,
    strict_flag_position,
};

mod labels;
pub use labels::{
    LabelIntent,
    LabelRegistry,
    RetiredLabel,
    core_roots,
    is_core_label,
    is_known_label,
    known_labels,
    nearest_label,
    retired_label,
    subsumes,
};

mod builtins;
pub use builtins::{
    ArgSource,
    ConstRow,
    ConstValue,
    FailureArms,
    FailureCause,
    Invocation,
    InvocationShape,
    ParamFacts,
    ResourceKind,
    ResourceParam,
    ResourceReturn,
    builtin_class_alias,
    builtin_class_display,
    builtin_class_supers,
    builtin_throws,
    declared_method_return,
    declared_method_return_blocked,
    declared_method_return_changed_at,
    declared_return,
    declared_return_changed_at,
    engine_constant,
    engine_class_declarations,
    engine_constant_count,
    failure_arms,
    hierarchy_entry_count,
    invocation_shape,
    is_migrated_resource_class,
    method_throws,
    param_facts,
    param_facts_mined,
    resource_param,
    resource_return,
    return_fact,
};
