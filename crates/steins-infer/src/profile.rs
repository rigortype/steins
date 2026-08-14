//! The profile engine (ADR-0050 §5–§8): named display surfaces resolved from
//! config, selecting which post-inference findings a `check` run prints.
//!
//! A **profile** is config-resolved *data* — a selection over diagnostic layers
//! (ADR-0050 §1) and ids — never a change to inference behavior (trust-toggle
//! refusal, ADR-0050 §10). The default surface is proof + mechanics (§3): a bare
//! `steins check` prints the proven-runtime-break set plus anti-rot; the contract
//! layer (`phpdoc.*`, `throw.*`) needs a named opt-up.
//!
//! # Built-ins (data, §5 / G1 amendment)
//!
//! * `default` — proof + mechanics.
//! * `throws-direct` — default **plus** `throw.undeclared` WHERE `origin = direct`
//!   (the only facet v1 defines, §4), justified by
//!   `docs/notes/20260724-g1-throw-origin-measurement.md` (158 direct vs 43,805
//!   propagated on the legacy monorepo).
//! * `contracts` — default plus the whole contract layer.
//! * `strict` — contracts plus the strict-floor ids (ADR-0062 A-G10): the offset
//!   family's `offset.undeclared` / `offset.maybe-missing` leg (issue #51).
//! * `pedantic` — contracts plus the house-style asks; a **branch off
//!   `contracts`**, not a rung above `strict` (see below).
//! * `boundary` is still **reserved** (ADR-0042): selecting or defining it is a
//!   config error until its ADR lands.
//!
//! # The rung ladder (ADR-0062 A-G10)
//!
//! Profiles select by **rung**, not layer set: the registry gives every id a
//! `surface_floor`, and a surface admits an id when `floor(id) <= rung`, over the
//! cumulative chain `default ⊂ contracts ⊂ strict`. Rung selection (vs. layer set)
//! is what lets ONE layer hold ids at two rungs (contract spans `Contracts` and
//! `Strict`).
//!
//! The built-ins are not one chain: `throws-direct` branches off `default`,
//! `pedantic` off `contracts`, each reaching one id above its own rung via
//! `enable` — orthogonal to the ladder (rung = how far up the chain, `enable` =
//! "and also this"). `pedantic` branches on purpose: "demand explicit types" and
//! "show weaker some-paths claims" are independent axes, so a rung above `strict`
//! would force strict users to inherit pedantic asks too (why those ids stay off
//! `strict`). No built-in means "everything on"; write `extends = "strict"` with
//! the pedantic ids in your own `enable` for that.
//!
//! # User profiles (§5)
//!
//! `[profile.<name>]` in `steins.toml`, with `extends` (built-in or user profile)
//! and `enable`/`disable`/`warn` as ADR-0022 prefix id-arrays. Cycles, unknown
//! names, and unknown id patterns are config errors; mechanics ids ignore
//! `disable` (§1). Facet selectors are **deferred** (§4/§11): v1 reaches `origin`
//! only via the built-in `throws-direct`, so a facet-shaped token
//! (`throw.undeclared@direct`) is rejected as an unknown id pattern.
//!
//! # Composition (§6)
//!
//! vendor filter → **profile surface** → `[[policy]]` scoped enable/disable →
//! inline ignores → baseline. `[[policy]]` (issue #15) is currently a no-op with a
//! seam for scoped enable/disable (see the CLI).

use std::collections::BTreeMap;
use std::fmt;

use crate::{
    DEBUG_PHPDOC_TYPE_ID, DEBUG_TRACE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID, DIAGNOSTIC_REGISTRY,
    Diagnostic, Facet,
    Floor, Layer, Origin, THROW_UNDECLARED_ID, UNTYPED_CLASS_CONSTANT_ID, layer, pattern_is_known,
    pattern_matches, surface_floor,
};

/// The default profile name, used when neither `--profile` nor `[check] profile`
/// selects one.
pub const DEFAULT: &str = "default";

/// The reserved profile names (ADR-0042): selecting or defining one errors until
/// its ADR lands. `strict` left this list at ADR-0062 S6 (now a built-in);
/// `boundary` remains deferred.
const RESERVED: &[&str] = &["boundary"];

/// The built-in profile names (ADR-0050 §5 / G1 amendment, extended by ADR-0062
/// A-G10's `strict` rung and by the `pedantic` branch).
const BUILTINS: &[&str] = &["default", "contracts", "throws-direct", "strict", "pedantic"];

/// Whether a surfaced finding fails the run or is merely reported (ADR-0050 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The default in every layer: a surfaced finding CI must see (exit 1).
    Fail,
    /// Demoted by a profile's `warn = [...]`: reported, exit-neutral.
    Warn,
}

impl Level {
    /// The `--format json` `level` wire spelling (`"fail"|"warn"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Fail => "fail",
            Level::Warn => "warn",
        }
    }
}

/// A user-defined `[profile.<name>]` entry (ADR-0050 §5), config-shape only.
#[derive(Debug, Clone, Default)]
pub struct UserProfile {
    /// The base profile extended (built-in or user); `None` extends `default`.
    pub extends: Option<String>,
    /// ADR-0022 prefix id-arrays forced onto the surface.
    pub enable: Vec<String>,
    /// ADR-0022 prefix id-arrays removed from the surface (mechanics ids ignore it).
    pub disable: Vec<String>,
    /// ADR-0022 prefix id-arrays demoted to `warn` (report-without-fail).
    pub warn: Vec<String>,
}

/// The user profile table, keyed by name (`BTreeMap` for deterministic iteration
/// in validation and error messages).
#[derive(Debug, Clone, Default)]
pub struct ProfileConfigs(pub BTreeMap<String, UserProfile>);

/// A config error resolving profiles (ADR-0050 §5). Every variant is a
/// usage/config error — the CLI maps it to exit 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A reserved name (`boundary`) was selected or extended as a profile.
    ReservedName(String),
    /// A reserved name was defined as `[profile.<name>]`.
    ReservedDefinition(String),
    /// A built-in name was redefined as `[profile.<name>]`.
    BuiltinRedefinition(String),
    /// A selected/extended profile name is neither built-in nor user-defined.
    Unknown(String),
    /// An `extends` chain cycles. The vector is the chain up to the repeat.
    Cycle(Vec<String>),
    /// An `enable`/`disable`/`warn` entry is not a registry-governed id pattern —
    /// including a facet-shaped token, which v1 does not accept in user profiles.
    UnknownId { profile: String, pattern: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::ReservedName(n) => write!(
                f,
                "profile `{n}` is a reserved name (deferred to its ADR); it cannot be selected or extended yet"
            ),
            ConfigError::ReservedDefinition(n) => write!(
                f,
                "[profile.{n}] uses the reserved name `{n}` (deferred to its ADR); pick another name"
            ),
            ConfigError::BuiltinRedefinition(n) => write!(
                f,
                "[profile.{n}] redefines the built-in profile `{n}`; pick another name"
            ),
            // Derived from `BUILTINS`, not hand-spelled: a hard-coded list already
            // drifted once (missed `pedantic`), misdirecting readers.
            ConfigError::Unknown(n) => write!(
                f,
                "unknown profile `{n}` (built-ins: {}; or define [profile.{n}])",
                BUILTINS.join(", ")
            ),
            ConfigError::Cycle(chain) => {
                write!(f, "profile `extends` cycle: {}", chain.join(" -> "))
            }
            ConfigError::UnknownId { profile, pattern } => write!(
                f,
                "[profile.{profile}] names unknown diagnostic id `{pattern}` \
                 (user profiles take plain ADR-0022 id patterns; facet selection is only via the built-in `throws-direct`)"
            ),
        }
    }
}

/// A resolved display surface: the layers/ids on the surface, the warn demotions,
/// and the single v1 facet selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    /// The name this surface resolved from (for the baseline capture header, §8).
    pub name: String,
    /// The **rung** on the cumulative ladder `default ⊂ contracts ⊂ strict`
    /// (ADR-0062 A-G10): an id is admitted when its registry [`Floor`] is at or
    /// below this. Replaces the pre-S6 layer *set* ([`Surface::layers_on`] covers
    /// why that's behavior-preserving).
    rung: Floor,
    /// Id patterns forced on beyond the layer set (`throws-direct` uses this for
    /// `throw.undeclared`; user profiles for `enable`).
    enable: Vec<String>,
    /// Id patterns removed from the surface (mechanics ignore this).
    disable: Vec<String>,
    /// Id patterns demoted to `warn`.
    warn: Vec<String>,
    /// The `origin = direct` facet selector (§4), the only facet v1 defines,
    /// reached only through the `throws-direct` built-in: when set, a surfaced
    /// `throw.undeclared` finding is kept only if its origin facet is `direct`.
    origin_direct_only: bool,
}

/// Whether a layer prints on **every** surface (ADR-0050 §1: mechanics is anti-rot,
/// not a strictness preference). Exhaustive on [`Layer`] on purpose: a new variant
/// becomes a *compile error* here, forcing a deliberate always-on/opt-in decision.
fn layer_always_on(l: Layer) -> bool {
    match l {
        Layer::Mechanics => true,
        Layer::Proof | Layer::Contract => false,
        // Debug (ADR-0053 §4/§8) never enters capture (`surface_ids()`/staleness);
        // its DISPLAY is decided separately, unconditionally, in
        // [`Surface::is_surfaced`] — capture/display stay split.
        Layer::Debug => false,
    }
}

impl Surface {
    fn builtin(name: &str) -> Option<Surface> {
        let base = |rung: Floor| Surface {
            name: name.to_owned(),
            rung,
            enable: Vec::new(),
            disable: Vec::new(),
            warn: Vec::new(),
            origin_direct_only: false,
        };
        match name {
            // proof + mechanics (§3 / G1 amendment: unconditional).
            "default" => Some(base(Floor::Default)),
            // default + the whole contract layer, as it stood before S6.
            "contracts" => Some(base(Floor::Contracts)),
            // contracts + the strict-floor ids (ADR-0062 A-G10): cumulative, adds
            // ids only.
            "strict" => Some(base(Floor::Strict)),
            // default + throw.undeclared WHERE origin = direct (the §4 facet).
            "throws-direct" => {
                let mut s = base(Floor::Default);
                s.enable.push(THROW_UNDECLARED_ID.to_owned());
                s.origin_direct_only = true;
                Some(s)
            }
            // contracts + the house-style asks by name, one `enable` line per id
            // (the `throws-direct` shape) — a branch, not a rung above `strict`
            // (module doc: "pedantic branches on purpose").
            "pedantic" => {
                let mut s = base(Floor::Contracts);
                s.enable.push(UNTYPED_CLASS_CONSTANT_ID.to_owned());
                Some(s)
            }
            _ => None,
        }
    }

    /// Whether id `id` is on this surface, **facet-agnostic** (§8): drives the
    /// baseline capture id-set and the dormant/stale partition. Mechanics is
    /// unconditionally on (disable-exempt, §1/§5).
    ///
    /// The pre-S6 layer-set test became the ladder test `floor(id) <= rung` at
    /// ADR-0062 S6 — behavior-preserving for every pre-S6 id, pinned id-by-id
    /// against the registry by `tests/profile.rs`.
    #[must_use]
    pub fn surfaces_id(&self, id: &str) -> bool {
        let Some(l) = layer(id) else { return false };
        // Debug's capture exemption is a LAYER property decided before `enable`/
        // `disable` are consulted (issue #108 regression: `enable = ["debug.type"]`
        // used to leak a debug id past this point into `layers_on()`/
        // `surface_ids()`). DISPLAY is unaffected — decided separately in
        // [`Surface::is_surfaced`].
        if l == Layer::Debug {
            return false;
        }
        if layer_always_on(l) {
            return true;
        }
        let mut on = surface_floor(id).is_some_and(|f| f <= self.rung);
        if self.enable.iter().any(|p| pattern_matches(p, id)) {
            on = true;
        }
        if self.disable.iter().any(|p| pattern_matches(p, id)) {
            on = false;
        }
        on
    }

    /// The rung this surface resolved to (ADR-0062 A-G10) — what a baseline entry
    /// records as its capture surface.
    #[must_use]
    pub const fn rung(&self) -> Floor {
        self.rung
    }

    /// Whether a concrete finding is on this surface (§5/§6). Adds the facet
    /// selector to [`Surface::surfaces_id`]: under `throws-direct` a
    /// `throw.undeclared` finding is kept only when its origin facet is `direct`.
    #[must_use]
    pub fn is_surfaced(&self, d: &Diagnostic) -> bool {
        // Debug (ADR-0053 §4) is default-ON on every profile, never in
        // `surfaces_id` (baseline-exempt, §8). The explicit pair is profile-inert;
        // `debug.var-dump` is the ONE profile-disableable dump (ADR-0074 §8:
        // `debug.trace` has no escape hatch — an annotation is always an authored
        // question; the remedy is deleting the comment).
        if let Some(Layer::Debug) = layer(d.id) {
            if d.id == DEBUG_VAR_DUMP_ID {
                return !self.disable.iter().any(|p| pattern_matches(p, d.id));
            }
            return true;
        }
        if !self.surfaces_id(d.id) {
            return false;
        }
        if self.origin_direct_only && d.id == THROW_UNDECLARED_ID {
            return d.facet == Some(Facet::Origin(Origin::Direct));
        }
        true
    }

    /// The level a surfaced id reports at (§7): `Fail` by default, `Warn` when a
    /// `warn = [...]` pattern matches. A pure function of the id (warn matches ids).
    #[must_use]
    pub fn level(&self, id: &str) -> Level {
        // Debug levels are FIXED (ADR-0053 §3), untouched by any profile channel:
        // the explicit pair fails (a named-but-nonexistent function is a guaranteed
        // fatal); `var_dump` warns forever (a leftover call is working PHP — a lint
        // rule is refused, ADR-0017); `debug.trace` warns too (ADR-0074 §8: its
        // trigger is a runtime-inert docblock, so the fail-forcing argument doesn't
        // apply).
        if id == DEBUG_VAR_DUMP_ID || id == DEBUG_TRACE_ID {
            return Level::Warn;
        }
        if id == DEBUG_TYPE_ID || id == DEBUG_PHPDOC_TYPE_ID {
            return Level::Fail;
        }
        // Mechanics ids are profile-inert (ADR-0050 §1): `disable` is already
        // powerless via `layer_always_on`, and `warn` must be too, or
        // `warn = ["suppress.*"]` would demote `suppress.unmatched` and let a
        // stale `@steins-ignore` stop failing CI (issue #108) — the rot mechanics
        // exists to prevent.
        if layer(id) == Some(Layer::Mechanics) {
            return Level::Fail;
        }
        if self.warn.iter().any(|p| pattern_matches(p, id)) {
            Level::Warn
        } else {
            Level::Fail
        }
    }

    /// The named layers on this surface, sorted (ADR-0054 §9: the doctor's
    /// "surface described" line). Mechanics is always-on (§1); debug is
    /// display-only and never a surface layer (§8), so it never appears here even
    /// though it always displays.
    ///
    /// Derived from the ids [`Surface::surfaces_id`] actually admits, not a static
    /// rung-to-layer table: a prior table read the rung alone and reported
    /// `[mechanics, proof]` for `throws-direct` (rung `Floor::Default`, same as
    /// `default`) even though it reaches `throw.undeclared` — a contract-layer id —
    /// through `enable`, hiding the layer it actually checks (issue #108).
    #[must_use]
    pub fn layers_on(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = DIAGNOSTIC_REGISTRY
            .iter()
            .filter(|(id, ..)| self.surfaces_id(id))
            .map(|(_, l, _)| l.as_str())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The resolved capture id-set (§8): every registered id this surface admits,
    /// facet-agnostic, sorted. Written into the baseline header by `--set-baseline`.
    #[must_use]
    pub fn surface_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = DIAGNOSTIC_REGISTRY
            .iter()
            .map(|(id, ..)| *id)
            .filter(|id| self.surfaces_id(id))
            .map(str::to_owned)
            .collect();
        ids.sort();
        ids
    }
}

impl ProfileConfigs {
    /// Resolve the *selected* profile into its surface, after validating every
    /// defined profile (ADR-0050 §5) — whole-table, so a broken but unused
    /// `[profile.*]` is caught in CI review, not deferred until selected.
    /// `selected` is the effective name (`--profile` or `[check] profile`, caller
    /// resolves precedence); `None` selects `default`.
    pub fn resolve(&self, selected: Option<&str>) -> Result<Surface, ConfigError> {
        // Whole-table validation: no defined profile may shadow a reserved or
        // built-in name, and every defined profile must resolve (patterns, extends
        // targets, no cycles).
        for name in self.0.keys() {
            if RESERVED.contains(&name.as_str()) {
                return Err(ConfigError::ReservedDefinition(name.clone()));
            }
            if BUILTINS.contains(&name.as_str()) {
                return Err(ConfigError::BuiltinRedefinition(name.clone()));
            }
        }
        for name in self.0.keys() {
            self.resolve_named(name, &mut Vec::new())?;
        }

        let name = selected.unwrap_or(DEFAULT);
        if RESERVED.contains(&name) {
            return Err(ConfigError::ReservedName(name.to_owned()));
        }
        self.resolve_named(name, &mut Vec::new())
    }

    /// Resolve one profile name (built-in or user) into a surface, following
    /// `extends` with cycle detection.
    fn resolve_named(&self, name: &str, stack: &mut Vec<String>) -> Result<Surface, ConfigError> {
        if RESERVED.contains(&name) {
            return Err(ConfigError::ReservedName(name.to_owned()));
        }
        if let Some(s) = Surface::builtin(name) {
            return Ok(s);
        }
        let Some(up) = self.0.get(name) else {
            return Err(ConfigError::Unknown(name.to_owned()));
        };
        if stack.iter().any(|n| n == name) {
            stack.push(name.to_owned());
            return Err(ConfigError::Cycle(stack.clone()));
        }

        // Validate this profile's id patterns before recursing, so the error names
        // the profile that owns the bad pattern. Rejects facet-shaped tokens (§4).
        for p in up.enable.iter().chain(&up.disable).chain(&up.warn) {
            if !pattern_is_known(p) {
                return Err(ConfigError::UnknownId {
                    profile: name.to_owned(),
                    pattern: p.clone(),
                });
            }
        }

        stack.push(name.to_owned());
        let base = up.extends.as_deref().unwrap_or(DEFAULT);
        let mut surface = self.resolve_named(base, stack)?;
        stack.pop();

        surface.name = name.to_owned();
        surface.enable.extend(up.enable.iter().cloned());
        surface.disable.extend(up.disable.iter().cloned());
        surface.warn.extend(up.warn.iter().cloned());
        Ok(surface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ARRAY_DUPLICATE_KEY_ID, CALL_ON_NULL_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID,
        EFFECT_ID, OFFSET_MAYBE_MISSING_ID, OFFSET_UNDECLARED_ID, PARAM_MISMATCH_ID,
        PHPDOC_PROP_MISMATCH_ID, SUPPRESS_UNMATCHED_ID, THROW_LISKOV_ID,
    };
    // docblock hygiene (ADR-0078, issue #186)
    use crate::{
        CLOSURE_UNUSED_USE_ID, PHPDOC_MISPLACED_VAR_ID, PHPDOC_STALE_PARAM_ID, PHPDOC_STALE_VAR_ID,
        PHPDOC_THROWS_NOT_THROWABLE_ID, PHPDOC_UNPARSABLE_ID,
    };

    fn diag(id: &'static str, facet: Option<Facet>) -> Diagnostic {
        Diagnostic { id, path: "a.php".to_owned(), line: 1, column: 1, message: String::new(), facet, fix: None }
    }

    fn empty() -> ProfileConfigs {
        ProfileConfigs(BTreeMap::new())
    }

    #[test]
    fn default_is_proof_plus_mechanics_only() {
        let s = empty().resolve(None).unwrap();
        assert_eq!(s.name, "default");
        // proof + mechanics on:
        assert!(s.is_surfaced(&diag(CALL_ON_NULL_ID, None)));
        assert!(s.is_surfaced(&diag(SUPPRESS_UNMATCHED_ID, None)));
        // contract off:
        assert!(!s.is_surfaced(&diag(PARAM_MISMATCH_ID, None)));
        assert!(!s.is_surfaced(&diag(EFFECT_ID, None)));
        assert!(!s.is_surfaced(&diag(THROW_UNDECLARED_ID, Some(Facet::Origin(Origin::Direct)))));
    }

    #[test]
    fn contracts_adds_the_whole_contract_layer() {
        let s = empty().resolve(Some("contracts")).unwrap();
        assert!(s.is_surfaced(&diag(CALL_ON_NULL_ID, None))); // proof still on
        assert!(s.is_surfaced(&diag(SUPPRESS_UNMATCHED_ID, None))); // mechanics still on
        assert!(s.is_surfaced(&diag(PARAM_MISMATCH_ID, None)));
        assert!(s.is_surfaced(&diag(THROW_LISKOV_ID, None)));
        assert!(s.is_surfaced(&diag(THROW_UNDECLARED_ID, Some(Facet::Origin(Origin::Propagated)))));
    }

    #[test]
    fn throws_direct_selects_the_origin_facet() {
        let s = empty().resolve(Some("throws-direct")).unwrap();
        // proof + mechanics on; contract layer otherwise off:
        assert!(s.is_surfaced(&diag(CALL_ON_NULL_ID, None)));
        assert!(!s.is_surfaced(&diag(PARAM_MISMATCH_ID, None)));
        assert!(!s.is_surfaced(&diag(THROW_LISKOV_ID, None)));
        // throw.undeclared: direct on, propagated off.
        assert!(s.is_surfaced(&diag(THROW_UNDECLARED_ID, Some(Facet::Origin(Origin::Direct)))));
        assert!(!s.is_surfaced(&diag(THROW_UNDECLARED_ID, Some(Facet::Origin(Origin::Propagated)))));
        // ...but the id IS in the capture surface set (facet-agnostic, §8).
        assert!(s.surfaces_id(THROW_UNDECLARED_ID));
    }

    #[test]
    fn mechanics_ignore_disable() {
        let mut m = BTreeMap::new();
        m.insert(
            "p".to_owned(),
            UserProfile { disable: vec!["suppress.*".to_owned()], ..Default::default() },
        );
        let s = ProfileConfigs(m).resolve(Some("p")).unwrap();
        assert!(s.is_surfaced(&diag(SUPPRESS_UNMATCHED_ID, None)), "mechanics ignores disable");
    }

    #[test]
    fn mechanics_ignore_warn_too() {
        // issue #108: `warn` must be as powerless as `disable` against mechanics.
        let mut m = BTreeMap::new();
        m.insert(
            "quiet".to_owned(),
            UserProfile { extends: Some("default".to_owned()), warn: vec!["suppress.*".to_owned()], ..Default::default() },
        );
        let s = ProfileConfigs(m).resolve(Some("quiet")).unwrap();
        assert!(s.is_surfaced(&diag(SUPPRESS_UNMATCHED_ID, None)), "mechanics still surfaced");
        assert_eq!(
            s.level(SUPPRESS_UNMATCHED_ID),
            Level::Fail,
            "warn cannot demote a mechanics id — a stale @steins-ignore must keep failing CI"
        );
    }

    /// ADR-0078 §1.5: `phpdoc.*` spans contract and mechanics ids (issue #186);
    /// mechanics stays disable/warn-proof even under a family pattern.
    #[test]
    fn a_phpdoc_family_disable_cannot_reach_the_mechanics_ids() {
        let mut m = BTreeMap::new();
        m.insert(
            "quiet".to_owned(),
            UserProfile {
                extends: Some("contracts".to_owned()),
                disable: vec!["phpdoc.*".to_owned()],
                warn: vec!["phpdoc.*".to_owned()],
                ..Default::default()
            },
        );
        let s = ProfileConfigs(m).resolve(Some("quiet")).unwrap();
        // The contract member of the family IS disableable — the prefix matched it.
        assert!(!s.is_surfaced(&diag(PARAM_MISMATCH_ID, None)), "contract ids obey disable");
        // The mechanics members are not, on either channel.
        for id in [
            PHPDOC_UNPARSABLE_ID,
            PHPDOC_STALE_PARAM_ID,
            PHPDOC_STALE_VAR_ID,
            PHPDOC_MISPLACED_VAR_ID,
            PHPDOC_THROWS_NOT_THROWABLE_ID,
            CLOSURE_UNUSED_USE_ID,
        ] {
            assert_eq!(layer(id), Some(Layer::Mechanics), "`{id}` must be a mechanics id");
            assert!(s.is_surfaced(&diag(id, None)), "`{id}` must ignore a family disable");
            assert_eq!(s.level(id), Level::Fail, "`{id}` must ignore a family warn");
        }
    }

    /// The hygiene family is on the bare `steins check` surface (floor `default`),
    /// like every other mechanics id.
    #[test]
    fn the_hygiene_family_is_on_the_default_surface() {
        let s = empty().resolve(None).unwrap();
        for id in [
            PHPDOC_UNPARSABLE_ID,
            PHPDOC_STALE_PARAM_ID,
            PHPDOC_STALE_VAR_ID,
            PHPDOC_MISPLACED_VAR_ID,
            PHPDOC_THROWS_NOT_THROWABLE_ID,
            CLOSURE_UNUSED_USE_ID,
        ] {
            assert_eq!(surface_floor(id), Some(Floor::Default), "`{id}` floors at default");
            assert!(s.is_surfaced(&diag(id, None)), "`{id}` prints on a bare check");
        }
    }

    #[test]
    fn array_duplicate_key_ignores_disable_and_warn_too() {
        // Issue #187: pins the mechanics disable/warn-proof behavior for this id too.
        let mut m = BTreeMap::new();
        m.insert(
            "quiet".to_owned(),
            UserProfile {
                disable: vec!["array.duplicate-key".to_owned()],
                warn: vec!["array.*".to_owned()],
                ..Default::default()
            },
        );
        let s = ProfileConfigs(m).resolve(Some("quiet")).unwrap();
        assert!(s.is_surfaced(&diag(ARRAY_DUPLICATE_KEY_ID, None)), "disable cannot turn it off");
        assert_eq!(s.level(ARRAY_DUPLICATE_KEY_ID), Level::Fail, "warn cannot demote it either");
    }

    #[test]
    fn user_profile_extends_and_warn_demotes() {
        let mut m = BTreeMap::new();
        m.insert(
            "migration".to_owned(),
            UserProfile {
                extends: Some("contracts".to_owned()),
                warn: vec!["throw.*".to_owned()],
                ..Default::default()
            },
        );
        let s = ProfileConfigs(m).resolve(Some("migration")).unwrap();
        assert!(s.is_surfaced(&diag(THROW_LISKOV_ID, None)));
        assert_eq!(s.level(THROW_LISKOV_ID), Level::Warn, "warn demotes");
        assert_eq!(s.level(CALL_ON_NULL_ID), Level::Fail, "others still fail");
    }

    #[test]
    fn flag_selection_of_reserved_name_errors() {
        // `boundary` is the one still-deferred reserved name (ADR-0042).
        assert_eq!(
            empty().resolve(Some("boundary")),
            Err(ConfigError::ReservedName("boundary".to_owned()))
        );
    }

    #[test]
    fn unknown_profile_errors() {
        assert_eq!(empty().resolve(Some("nope")), Err(ConfigError::Unknown("nope".to_owned())));
    }

    #[test]
    fn defining_reserved_or_builtin_errors() {
        let mut m = BTreeMap::new();
        m.insert("boundary".to_owned(), UserProfile::default());
        assert_eq!(
            ProfileConfigs(m).resolve(None),
            Err(ConfigError::ReservedDefinition("boundary".to_owned()))
        );

        let mut m = BTreeMap::new();
        m.insert("default".to_owned(), UserProfile::default());
        assert_eq!(
            ProfileConfigs(m).resolve(None),
            Err(ConfigError::BuiltinRedefinition("default".to_owned()))
        );

        // `strict` is a built-in now, so redefining it is the builtin error.
        let mut m = BTreeMap::new();
        m.insert("strict".to_owned(), UserProfile::default());
        assert_eq!(
            ProfileConfigs(m).resolve(None),
            Err(ConfigError::BuiltinRedefinition("strict".to_owned()))
        );
    }

    #[test]
    fn extends_cycle_errors() {
        let mut m = BTreeMap::new();
        m.insert(
            "a".to_owned(),
            UserProfile { extends: Some("b".to_owned()), ..Default::default() },
        );
        m.insert(
            "b".to_owned(),
            UserProfile { extends: Some("a".to_owned()), ..Default::default() },
        );
        match ProfileConfigs(m).resolve(Some("a")) {
            Err(ConfigError::Cycle(_)) => {}
            other => panic!("expected cycle, got {other:?}"),
        }
    }

    #[test]
    fn facet_shaped_token_is_rejected_as_unknown_id() {
        // Deferred-with-design (§4/§11): user profiles reject facet selectors.
        let mut m = BTreeMap::new();
        m.insert(
            "p".to_owned(),
            UserProfile { enable: vec!["throw.undeclared@direct".to_owned()], ..Default::default() },
        );
        assert_eq!(
            ProfileConfigs(m).resolve(Some("p")),
            Err(ConfigError::UnknownId {
                profile: "p".to_owned(),
                pattern: "throw.undeclared@direct".to_owned(),
            })
        );
    }

    #[test]
    fn unused_broken_profile_still_errors() {
        // Whole-table validation: a broken but *unselected* profile is caught.
        let mut m = BTreeMap::new();
        m.insert(
            "broken".to_owned(),
            UserProfile { enable: vec!["not.an.id".to_owned()], ..Default::default() },
        );
        assert!(matches!(
            ProfileConfigs(m).resolve(Some("contracts")),
            Err(ConfigError::UnknownId { .. })
        ));
    }

    #[test]
    fn debug_lane_displays_default_on_every_profile_but_is_baseline_exempt() {
        // ADR-0053 §4/§8: default-ON display everywhere, never baseline-captured.
        for profile in [None, Some("contracts"), Some("throws-direct")] {
            let s = empty().resolve(profile).unwrap();
            for id in [DEBUG_TYPE_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_VAR_DUMP_ID, DEBUG_TRACE_ID] {
                assert!(
                    s.is_surfaced(&diag(id, None)),
                    "`{id}` must display on every built-in profile ({profile:?})"
                );
                assert!(
                    !s.surfaces_id(id),
                    "`{id}` must stay off the baseline capture predicate ({profile:?})"
                );
                assert!(
                    !s.surface_ids().iter().any(|c| c == id),
                    "`{id}` must be excluded from the baseline capture set (§4 exemption)"
                );
            }
        }
    }

    #[test]
    fn an_enable_pattern_cannot_pull_a_debug_id_into_the_capture_surface() {
        // issue #108 (PR #133): `enable = ["debug.type"]` used to leak a debug id
        // into `layers_on()`/`surface_ids()`; `surfaces_id` now excludes debug first.
        let mut m = BTreeMap::new();
        m.insert(
            "debug-enabled".to_owned(),
            UserProfile { enable: vec![DEBUG_TYPE_ID.to_owned()], ..Default::default() },
        );
        let s = ProfileConfigs(m).resolve(Some("debug-enabled")).unwrap();
        assert!(s.is_surfaced(&diag(DEBUG_TYPE_ID, None)), "still displays — enable didn't need to help");
        assert!(!s.surfaces_id(DEBUG_TYPE_ID), "enable cannot pull a debug id into the capture predicate");
        assert!(
            !s.surface_ids().iter().any(|c| c == DEBUG_TYPE_ID),
            "enable cannot pull a debug id into the baseline capture set"
        );
        assert_eq!(
            s.layers_on(),
            vec!["mechanics", "proof"],
            "debug must not appear in the surface's layer list even under an explicit enable"
        );
    }

    #[test]
    fn debug_levels_are_fixed_pair_fails_var_dump_warns() {
        // ADR-0053 §3: fixed levels, untouched by any profile channel.
        let s = empty().resolve(None).unwrap();
        assert_eq!(s.level(DEBUG_TYPE_ID), Level::Fail);
        assert_eq!(s.level(DEBUG_PHPDOC_TYPE_ID), Level::Fail);
        assert_eq!(s.level(DEBUG_VAR_DUMP_ID), Level::Warn);
        // `debug.trace` warns too, fixed (ADR-0074 §8): a runtime-inert docblock.
        assert_eq!(s.level(DEBUG_TRACE_ID), Level::Warn);

        let mut m = BTreeMap::new();
        m.insert(
            "p".to_owned(),
            UserProfile { warn: vec!["debug.*".to_owned()], ..Default::default() },
        );
        let w = ProfileConfigs(m).resolve(Some("p")).unwrap();
        assert_eq!(w.level(DEBUG_TYPE_ID), Level::Fail, "the pair is fail-fixed, warn cannot demote");
        assert_eq!(w.level(DEBUG_VAR_DUMP_ID), Level::Warn);
        assert_eq!(w.level(DEBUG_TRACE_ID), Level::Warn);
    }

    #[test]
    fn var_dump_is_profile_disableable_but_the_pair_is_inert() {
        // ADR-0053 §4: `var_dump` is disableable; the explicit pair ignores disable.
        let mut m = BTreeMap::new();
        m.insert(
            "quiet".to_owned(),
            UserProfile { disable: vec!["debug.var-dump".to_owned()], ..Default::default() },
        );
        let s = ProfileConfigs(m).resolve(Some("quiet")).unwrap();
        assert!(!s.is_surfaced(&diag(DEBUG_VAR_DUMP_ID, None)), "var_dump disabled by profile");
        assert!(s.is_surfaced(&diag(DEBUG_TYPE_ID, None)), "the explicit pair stays inert");
        assert!(s.is_surfaced(&diag(DEBUG_PHPDOC_TYPE_ID, None)), "the explicit pair stays inert");

        // Disabling the pair is a no-op (profile-inert): they still display.
        let mut m2 = BTreeMap::new();
        m2.insert(
            "try".to_owned(),
            UserProfile { disable: vec!["debug.type".to_owned()], ..Default::default() },
        );
        let s2 = ProfileConfigs(m2).resolve(Some("try")).unwrap();
        assert!(s2.is_surfaced(&diag(DEBUG_TYPE_ID, None)), "the explicit pair ignores disable");
    }

    #[test]
    fn trace_annotation_has_no_profile_disable_escape_hatch() {
        // ADR-0074 §8: unlike `var-dump`, `@psalm-trace` is always an authored
        // question, so `disable = ["debug.trace"]` is a no-op.
        let mut m = BTreeMap::new();
        m.insert(
            "mute".to_owned(),
            UserProfile { disable: vec!["debug.trace".to_owned()], ..Default::default() },
        );
        let s = ProfileConfigs(m).resolve(Some("mute")).unwrap();
        assert!(s.is_surfaced(&diag(DEBUG_TRACE_ID, None)), "debug.trace is profile-inert");
        // `debug.var-dump` stays the ONE disableable debug id — the special case
        // is not extended.
        assert!(s.is_surfaced(&diag(DEBUG_VAR_DUMP_ID, None)), "var_dump untouched by this profile");
    }

    #[test]
    fn strict_is_contracts_plus_the_strict_floor_ids() {
        let s = empty().resolve(Some("strict")).unwrap();
        assert_eq!(s.name, "strict");
        assert_eq!(s.rung(), Floor::Strict);
        // Everything contracts shows, still shown.
        assert!(s.is_surfaced(&diag(CALL_ON_NULL_ID, None))); // proof
        assert!(s.is_surfaced(&diag(SUPPRESS_UNMATCHED_ID, None))); // mechanics
        assert!(s.is_surfaced(&diag(PARAM_MISMATCH_ID, None))); // contract
        assert!(s.is_surfaced(&diag(THROW_LISKOV_ID, None)));
        // …plus the strict-floor ids.
        assert!(s.is_surfaced(&diag(OFFSET_UNDECLARED_ID, None)));
        assert!(s.is_surfaced(&diag(OFFSET_MAYBE_MISSING_ID, None)));
    }

    #[test]
    fn the_strict_floor_ids_are_invisible_below_strict() {
        // 2026-07-29 ruling: `offset.undeclared` promoted to contracts (zero corpus
        // findings); `offset.maybe-missing` stays strict until discharge lands.
        for profile in [None, Some("throws-direct")] {
            let s = empty().resolve(profile).unwrap();
            for id in [OFFSET_UNDECLARED_ID, OFFSET_MAYBE_MISSING_ID] {
                assert!(!s.is_surfaced(&diag(id, None)), "`{id}` must not display on {profile:?}");
                assert!(!s.surfaces_id(id), "`{id}` must not be captured on {profile:?}");
            }
        }
        let c = empty().resolve(Some("contracts")).unwrap();
        assert!(c.is_surfaced(&diag(OFFSET_UNDECLARED_ID, None)), "promoted to contracts");
        assert!(c.surfaces_id(OFFSET_UNDECLARED_ID), "promoted to contracts");
        assert!(!c.is_surfaced(&diag(OFFSET_MAYBE_MISSING_ID, None)), "still strict-only");
        assert!(!c.surfaces_id(OFFSET_MAYBE_MISSING_ID), "still strict-only");
    }

    #[test]
    fn the_ladder_is_cumulative_across_the_whole_registry() {
        // `default ⊂ contracts ⊂ strict` as SETS, checked over every registered id.
        let d = empty().resolve(None).unwrap();
        let c = empty().resolve(Some("contracts")).unwrap();
        let s = empty().resolve(Some("strict")).unwrap();
        for &(id, ..) in DIAGNOSTIC_REGISTRY {
            assert!(!d.surfaces_id(id) || c.surfaces_id(id), "`{id}`: default ⊄ contracts");
            assert!(!c.surfaces_id(id) || s.surfaces_id(id), "`{id}`: contracts ⊄ strict");
        }
        assert!(d.surface_ids().len() < c.surface_ids().len(), "contracts adds ids");
        assert!(c.surface_ids().len() < s.surface_ids().len(), "strict adds ids");
    }

    #[test]
    fn strict_names_the_same_three_layers_as_contracts() {
        // `strict` is a FLOOR within the contract layer, not a fourth layer.
        let c = empty().resolve(Some("contracts")).unwrap();
        let s = empty().resolve(Some("strict")).unwrap();
        assert_eq!(c.layers_on(), s.layers_on());
        assert_eq!(s.layers_on(), vec!["contract", "mechanics", "proof"]);
        assert_eq!(empty().resolve(None).unwrap().layers_on(), vec!["mechanics", "proof"]);
    }

    #[test]
    fn pedantic_branches_off_contracts_and_takes_nothing_from_strict() {
        // `pedantic` = `contracts` + pedantic-floor ids by name, not a strict+ rung.
        let c = empty().resolve(Some("contracts")).unwrap();
        let s = empty().resolve(Some("strict")).unwrap();
        let p = empty().resolve(Some("pedantic")).unwrap();

        assert_eq!(p.rung(), Floor::Contracts, "the rung is contracts; `enable` does the rest");
        assert!(p.surfaces_id(UNTYPED_CLASS_CONSTANT_ID), "the pedantic id is on");
        assert!(!c.surfaces_id(UNTYPED_CLASS_CONSTANT_ID), "…and on NO other built-in");
        assert!(!s.surfaces_id(UNTYPED_CLASS_CONSTANT_ID), "…including strict");
        assert!(!empty().resolve(None).unwrap().surfaces_id(UNTYPED_CLASS_CONSTANT_ID));

        // Contracts ⊂ pedantic: the branch adds, it does not drop.
        for &(id, ..) in DIAGNOSTIC_REGISTRY {
            assert!(!c.surfaces_id(id) || p.surfaces_id(id), "`{id}`: contracts ⊄ pedantic");
        }
        // pedantic and strict are INCOMPARABLE — a `pedantic`-as-rung would lose this.
        assert!(s.surfaces_id(OFFSET_MAYBE_MISSING_ID) && !p.surfaces_id(OFFSET_MAYBE_MISSING_ID));
        assert!(p.surfaces_id(UNTYPED_CLASS_CONSTANT_ID) && !s.surfaces_id(UNTYPED_CLASS_CONSTANT_ID));
        assert_eq!(p.layers_on(), vec!["contract", "mechanics", "proof"]);
    }

    #[test]
    fn no_builtin_carries_the_pedantic_rung() {
        // Makes `Floor::Pedantic` mean "off unless named" (else every id rides along).
        for name in BUILTINS {
            let s = empty().resolve(Some(name)).unwrap();
            assert_ne!(s.rung(), Floor::Pedantic, "`{name}` must not rung at pedantic");
        }
    }

    #[test]
    fn throws_direct_names_the_contract_layer_it_actually_checks() {
        // issue #108: `throws-direct` reaches `throw.undeclared` via `enable`, not
        // its rung — `layers_on` must derive from `surfaces_id`, not a rung table.
        let td = empty().resolve(Some("throws-direct")).unwrap();
        assert_eq!(td.layers_on(), vec!["contract", "mechanics", "proof"]);
    }

    #[test]
    fn a_user_profile_inherits_the_rung_it_extends() {
        let mut m = BTreeMap::new();
        m.insert(
            "house".to_owned(),
            UserProfile { extends: Some("strict".to_owned()), ..Default::default() },
        );
        let s = ProfileConfigs(m).resolve(Some("house")).unwrap();
        assert_eq!(s.rung(), Floor::Strict);
        assert!(s.is_surfaced(&diag(OFFSET_MAYBE_MISSING_ID, None)));
    }

    #[test]
    fn a_default_profile_can_still_enable_one_strict_id_explicitly() {
        // `enable` is orthogonal to the ladder — one strict id without the rest.
        let mut m = BTreeMap::new();
        m.insert(
            "just-undeclared".to_owned(),
            UserProfile { enable: vec![OFFSET_UNDECLARED_ID.to_owned()], ..Default::default() },
        );
        let s = ProfileConfigs(m).resolve(Some("just-undeclared")).unwrap();
        assert!(s.is_surfaced(&diag(OFFSET_UNDECLARED_ID, None)));
        assert!(!s.is_surfaced(&diag(OFFSET_MAYBE_MISSING_ID, None)), "the other stays off");
        assert!(!s.is_surfaced(&diag(PARAM_MISMATCH_ID, None)), "the rung is still default");
    }

    #[test]
    fn surface_ids_are_facet_agnostic_and_layered() {
        let d = empty().resolve(None).unwrap();
        assert!(!d.surface_ids().iter().any(|i| i == THROW_UNDECLARED_ID));
        assert!(d.surface_ids().iter().any(|i| i == CALL_ON_NULL_ID));

        let td = empty().resolve(Some("throws-direct")).unwrap();
        assert!(td.surface_ids().iter().any(|i| i == THROW_UNDECLARED_ID));
        assert!(!td.surface_ids().iter().any(|i| i == PHPDOC_PROP_MISMATCH_ID));
    }
}
