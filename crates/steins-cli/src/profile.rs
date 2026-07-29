//! The profile engine (ADR-0050 §5–§8): named display surfaces resolved from
//! config, selecting which post-inference findings a `check` run prints.
//!
//! A **profile** is config-resolved *data* — a selection over diagnostic layers
//! (ADR-0050 §1, read from the registry) and ids — never a change to inference
//! behavior (the trust-toggle refusal, ADR-0050 §10, holds). The default surface
//! is proof + mechanics (§3): a bare `steins check` prints exactly the
//! proven-runtime-break set plus anti-rot, and the contract layer (`phpdoc.*`,
//! `throw.*`) is reached only through a named opt-up stage.
//!
//! # Built-ins (data, §5 / G1 amendment)
//!
//! * `default` — proof + mechanics.
//! * `throws-direct` — default **plus** `throw.undeclared` WHERE `origin = direct`
//!   (the §4 facet selector, the only facet v1 defines). Measurement-justified by
//!   `docs/notes/20260724-g1-throw-origin-measurement.md` (158 direct vs 43,805
//!   propagated on the legacy monorepo).
//! * `contracts` — default plus the whole contract layer.
//! * `strict` — contracts plus the strict-floor ids (ADR-0062 A-G10): the offset
//!   family's `offset.undeclared` / `offset.maybe-missing` leg (issue #51).
//!
//! # The rung ladder (ADR-0062 A-G10)
//!
//! Profiles select by **rung**, not by layer set: the registry gives every id a
//! `surface_floor`, and a surface admits an id when `floor(id) <= rung`. The
//! built-ins are the cumulative ladder `default ⊂ contracts ⊂ strict`. This is a
//! restatement of the pre-S6 layer-set selection, not a re-levelling — see
//! [`Surface::surfaces_id`] — and it is what lets ONE layer hold ids at two rungs
//! (the contract layer now does).
//!
//! `boundary` is still a **reserved** name (ADR-0042): selecting *or* defining it
//! is a config error until its ADR lands.
//!
//! # User profiles (§5)
//!
//! `[profile.<name>]` in `steins.toml` with `extends` (a built-in or user
//! profile), and `enable` / `disable` / `warn` as ADR-0022 prefix id-arrays.
//! Cycles, unknown names, and unknown id patterns are config errors. Mechanics ids
//! ignore `disable` (§1). **Facet selectors in user profiles are deferred with
//! design** (§4/§11): v1 reaches the `origin` facet only through the built-in
//! `throws-direct` profile, so a user `enable`/`disable`/`warn` entry accepts only
//! a plain id pattern — a facet-shaped token (`throw.undeclared@direct`) is an
//! unknown id pattern and rejected clearly. This is the lenient path the ADR names.
//!
//! # Composition (§6)
//!
//! vendor filter → **profile surface** → `[[policy]]` scoped enable/disable →
//! inline ignores → baseline. `[[policy]]` is issue #15 / slice 3: this slice
//! ships the pipeline with a no-op policy stage and a clear seam (see the CLI).

use std::collections::BTreeMap;
use std::fmt;

use steins_infer::{
    DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID, DIAGNOSTIC_REGISTRY, Diagnostic, Facet,
    Floor, Layer, Origin, THROW_UNDECLARED_ID, layer, pattern_is_known, pattern_matches,
    surface_floor,
};

/// The default profile name, used when neither `--profile` nor `[check] profile`
/// selects one.
pub const DEFAULT: &str = "default";

/// The reserved profile names (ADR-0042): selecting or defining one errors until
/// its ADR lands. `strict` left this list at ADR-0062 S6 — A-G10 is its ADR — and
/// is now a built-in; `boundary` is still deferred.
const RESERVED: &[&str] = &["boundary"];

/// The built-in profile names (ADR-0050 §5 / G1 amendment, extended by ADR-0062
/// A-G10's `strict` rung).
const BUILTINS: &[&str] = &["default", "contracts", "throws-direct", "strict"];

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
    /// The base profile this one extends (a built-in or user profile). `None`
    /// extends `default`.
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
    /// A selected/extended profile name is neither a built-in nor a defined user
    /// profile.
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
            ConfigError::Unknown(n) => write!(
                f,
                "unknown profile `{n}` (built-ins: default, contracts, throws-direct, strict; or define [profile.{n}])"
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
    /// below this. Replaces the pre-S6 layer *set* — see [`Surface::layers_on`] for
    /// why that replacement is behavior-preserving.
    rung: Floor,
    /// Id patterns forced on beyond the layer set (`throws-direct` uses this for
    /// `throw.undeclared`; user profiles for `enable`).
    enable: Vec<String>,
    /// Id patterns removed from the surface (mechanics ignore this).
    disable: Vec<String>,
    /// Id patterns demoted to `warn`.
    warn: Vec<String>,
    /// The `origin = direct` facet selector (§4): when set, a surfaced
    /// `throw.undeclared` finding is kept only if its origin facet is `direct`.
    /// The only facet v1 defines, reached only through the `throws-direct` built-in.
    origin_direct_only: bool,
}

/// Whether a layer prints on **every** surface (ADR-0050 §1: mechanics is anti-rot,
/// not a strictness preference). Exhaustive on [`Layer`] on purpose: a new variant
/// (a future `boundary` layer) becomes a *compile error* here — forcing a
/// deliberate always-on/opt-in decision rather than a silent fall-through to off.
fn layer_always_on(l: Layer) -> bool {
    match l {
        Layer::Mechanics => true,
        Layer::Proof | Layer::Contract => false,
        // The debug lane (ADR-0053 §4/§8) is **never** captured in a baseline and
        // never enters `surface_ids()`, so it stays off *this* predicate — which
        // governs `surface_ids()` (the baseline capture set) and the surface-aware
        // staleness partition. Its DISPLAY is handled separately in
        // [`Surface::is_surfaced`] (default-ON on every profile, the explicit pair
        // profile-inert, `debug.var-dump` disableable — §4). Keeping capture and
        // display split is exactly the §4/§8 exemption: a dump is displayed but never
        // baselined. The exhaustive match still forces this variant to be stated.
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
            // default + the whole contract layer, as it stood before S6 — which is
            // exactly "every id whose floor is at or below `contracts`".
            "contracts" => Some(base(Floor::Contracts)),
            // contracts + the strict-floor ids (ADR-0062 A-G10). Strictly cumulative:
            // it adds ids, never removes or re-levels one.
            "strict" => Some(base(Floor::Strict)),
            // default + throw.undeclared WHERE origin = direct (the §4 facet).
            "throws-direct" => {
                let mut s = base(Floor::Default);
                s.enable.push(THROW_UNDECLARED_ID.to_owned());
                s.origin_direct_only = true;
                Some(s)
            }
            _ => None,
        }
    }

    /// Whether id `id` is on this surface, **facet-agnostic** (§8): the id-level
    /// question, used to compute the baseline capture id-set and the dormant/stale
    /// partition. Mechanics is unconditionally on (disable-exempt, §1/§5).
    ///
    /// The layer-set membership test this used to run became the ladder test
    /// `floor(id) <= rung` at ADR-0062 S6. That substitution is behavior-preserving
    /// for every pre-S6 id — proof/mechanics ids carry `Floor::Default` and were in
    /// every built-in's layer set; contract ids carry `Floor::Contracts` and were in
    /// `contracts`'s alone — and `tests/profile.rs` pins it id-by-id against the
    /// registry rather than trusting the argument.
    #[must_use]
    pub fn surfaces_id(&self, id: &str) -> bool {
        let Some(l) = layer(id) else { return false };
        if layer_always_on(l) {
            return true;
        }
        // The debug lane's capture exemption (§4/§8) is a LAYER property, decided
        // before the ladder: a dump displays everywhere but is never captured, so
        // its floor never gets a vote. Keeping this arm here (rather than inventing
        // an unreachable floor) preserves the pre-S6 reading exactly, including the
        // corner where an explicit `enable` pattern below forces a debug id on.
        let mut on = l != Layer::Debug && surface_floor(id).is_some_and(|f| f <= self.rung);
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
        // The debug lane (ADR-0053 §4) is default-ON on every profile but never in
        // `surfaces_id` (baseline-exempt, §8). The explicit pair is profile-inert (no
        // profile disables or demotes it, like mechanics); `debug.var-dump` is the one
        // profile-disableable dump (`disable = ["debug.var-dump"]`), ON by default in
        // every built-in.
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
        // The debug lane's levels are FIXED (ADR-0053 §3), never touched by a profile
        // `warn`/`enable` channel: the explicit pair fails (a committed call names a
        // function that does not exist at runtime — a guaranteed fatal), `var_dump`
        // warns (exit-neutral forever — a leftover `var_dump` is working PHP; a lint
        // rule is refused, ADR-0017). No channel promotes `debug.var-dump` to fail.
        if id == DEBUG_VAR_DUMP_ID {
            return Level::Warn;
        }
        if id == DEBUG_TYPE_ID || id == DEBUG_PHPDOC_TYPE_ID {
            return Level::Fail;
        }
        if self.warn.iter().any(|p| pattern_matches(p, id)) {
            Level::Warn
        } else {
            Level::Fail
        }
    }

    /// The named layers on this surface, sorted (ADR-0054 §9: the doctor's
    /// "surface described" line). Mechanics is always-on regardless of membership
    /// (§1); every rung carries it, so this is a faithful summary. The debug lane is
    /// display-only and never a surface layer (§8 capture/display split), so it does
    /// not appear here.
    ///
    /// Derived from the rung rather than a stored set, and byte-identical to the
    /// pre-S6 built-in sets: `default` was `{proof, mechanics}`, `contracts` added
    /// `contract`. `strict` names the SAME three layers — it is a floor within the
    /// contract layer, not a fourth layer (A-G10), which is precisely why the
    /// registry needed a floor attribute instead of another `Layer` variant.
    #[must_use]
    pub fn layers_on(&self) -> Vec<&'static str> {
        let layers: &[Layer] = match self.rung {
            Floor::Default => &[Layer::Proof, Layer::Mechanics],
            Floor::Contracts | Floor::Strict => {
                &[Layer::Proof, Layer::Mechanics, Layer::Contract]
            }
        };
        let mut v: Vec<&'static str> = layers.iter().map(|l| l.as_str()).collect();
        v.sort_unstable();
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
    /// defined profile (ADR-0050 §5). Validation is whole-table so a broken but
    /// unused `[profile.*]` is caught in CI review, not silently deferred until
    /// selected.
    ///
    /// `selected` is the effective name (the `--profile` flag or `[check] profile`;
    /// the caller resolves the flag-beats-config precedence). `None` selects
    /// `default`.
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
    use steins_infer::{
        CALL_ON_NULL_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID, EFFECT_ID,
        OFFSET_MAYBE_MISSING_ID, OFFSET_UNDECLARED_ID, PARAM_MISMATCH_ID, PHPDOC_PROP_MISMATCH_ID,
        SUPPRESS_UNMATCHED_ID, THROW_LISKOV_ID,
    };

    fn diag(id: &'static str, facet: Option<Facet>) -> Diagnostic {
        Diagnostic { id, path: "a.php".to_owned(), line: 1, column: 1, message: String::new(), facet }
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
        // `boundary` is the one still-deferred reserved name (ADR-0042); `strict`
        // left the list at ADR-0062 S6 and is exercised as a built-in below.
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
        // The deferred-with-design decision (§4/§11): user profiles do not accept
        // facet selectors; a facet-shaped token is an unknown id pattern.
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
        // ADR-0053 §4/§8: the debug lane is default-ON on every built-in profile
        // (display), yet NEVER captured in a baseline (`surfaces_id` / `surface_ids`
        // stay false — the capture/display split). The explicit pair is profile-inert;
        // `var_dump` is disableable but ON by default here (no built-in disables it).
        for profile in [None, Some("contracts"), Some("throws-direct")] {
            let s = empty().resolve(profile).unwrap();
            for id in [DEBUG_TYPE_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_VAR_DUMP_ID] {
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
    fn debug_levels_are_fixed_pair_fails_var_dump_warns() {
        // ADR-0053 §3: fixed levels, untouched by any profile channel. The explicit
        // pair fails (a committed call is a runtime fatal); `var_dump` warns
        // (exit-neutral forever). Even a `warn = ["debug.*"]` cannot demote the pair,
        // and no channel promotes `var_dump` to fail.
        let s = empty().resolve(None).unwrap();
        assert_eq!(s.level(DEBUG_TYPE_ID), Level::Fail);
        assert_eq!(s.level(DEBUG_PHPDOC_TYPE_ID), Level::Fail);
        assert_eq!(s.level(DEBUG_VAR_DUMP_ID), Level::Warn);

        let mut m = BTreeMap::new();
        m.insert(
            "p".to_owned(),
            UserProfile { warn: vec!["debug.*".to_owned()], ..Default::default() },
        );
        let w = ProfileConfigs(m).resolve(Some("p")).unwrap();
        assert_eq!(w.level(DEBUG_TYPE_ID), Level::Fail, "the pair is fail-fixed, warn cannot demote");
        assert_eq!(w.level(DEBUG_VAR_DUMP_ID), Level::Warn);
    }

    #[test]
    fn var_dump_is_profile_disableable_but_the_pair_is_inert() {
        // ADR-0053 §4: `disable = ["debug.var-dump"]` turns the incidental dump off;
        // the explicit pair is profile-inert (disable is ignored, like mechanics).
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
        // The measurement-first posture, at the surface: S6's ids exist and are
        // emitted, but no profile below `strict` displays or captures them — which
        // is what keeps default/contracts byte-identical to their pre-S6 output.
        for profile in [None, Some("contracts"), Some("throws-direct")] {
            let s = empty().resolve(profile).unwrap();
            for id in [OFFSET_UNDECLARED_ID, OFFSET_MAYBE_MISSING_ID] {
                assert!(!s.is_surfaced(&diag(id, None)), "`{id}` must not display on {profile:?}");
                assert!(!s.surfaces_id(id), "`{id}` must not be captured on {profile:?}");
            }
        }
    }

    #[test]
    fn the_ladder_is_cumulative_across_the_whole_registry() {
        // `default ⊂ contracts ⊂ strict` as SETS, checked over every registered id
        // rather than asserted: a rung may only add ids, never drop or re-level one.
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
        // `strict` is a FLOOR within the contract layer, not a fourth layer (A-G10),
        // so the doctor's "surface described" line reads identically.
        let c = empty().resolve(Some("contracts")).unwrap();
        let s = empty().resolve(Some("strict")).unwrap();
        assert_eq!(c.layers_on(), s.layers_on());
        assert_eq!(s.layers_on(), vec!["contract", "mechanics", "proof"]);
        assert_eq!(empty().resolve(None).unwrap().layers_on(), vec!["mechanics", "proof"]);
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
        // The `enable` channel is orthogonal to the ladder (it always was): a project
        // that wants ONE strict id without the rest keeps that path.
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
