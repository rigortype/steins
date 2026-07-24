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
//!
//! `strict` and `boundary` are **reserved** names (ADR-0042): selecting *or*
//! defining one is a config error until their ADR lands.
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

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use steins_infer::{
    DIAGNOSTIC_REGISTRY, Diagnostic, Facet, Layer, Origin, THROW_UNDECLARED_ID, layer,
    pattern_is_known, pattern_matches,
};

/// The default profile name, used when neither `--profile` nor `[check] profile`
/// selects one.
pub const DEFAULT: &str = "default";

/// The reserved profile names (ADR-0042): selecting or defining one errors until
/// the boundary-profile ADR lands.
const RESERVED: &[&str] = &["strict", "boundary"];

/// The built-in profile names shipped in v1 (ADR-0050 §5 / G1 amendment).
const BUILTINS: &[&str] = &["default", "contracts", "throws-direct"];

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
    /// A reserved name (`strict`/`boundary`) was selected or extended as a profile.
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
                "unknown profile `{n}` (built-ins: default, contracts, throws-direct; or define [profile.{n}])"
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
    /// Layers whose findings are on the surface.
    layers: HashSet<Layer>,
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
/// (ADR-0053's planned `Debug`, a future `boundary` layer) becomes a *compile
/// error* here — forcing a deliberate always-on/opt-in decision rather than a
/// silent fall-through to off.
fn layer_always_on(l: Layer) -> bool {
    match l {
        Layer::Mechanics => true,
        Layer::Proof | Layer::Contract => false,
    }
}

impl Surface {
    fn builtin(name: &str) -> Option<Surface> {
        let base = |layers: &[Layer]| Surface {
            name: name.to_owned(),
            layers: layers.iter().copied().collect(),
            enable: Vec::new(),
            disable: Vec::new(),
            warn: Vec::new(),
            origin_direct_only: false,
        };
        match name {
            // proof + mechanics (§3 / G1 amendment: unconditional).
            "default" => Some(base(&[Layer::Proof, Layer::Mechanics])),
            // default + the whole contract layer.
            "contracts" => Some(base(&[Layer::Proof, Layer::Mechanics, Layer::Contract])),
            // default + throw.undeclared WHERE origin = direct (the §4 facet).
            "throws-direct" => {
                let mut s = base(&[Layer::Proof, Layer::Mechanics]);
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
    #[must_use]
    pub fn surfaces_id(&self, id: &str) -> bool {
        let Some(l) = layer(id) else { return false };
        if layer_always_on(l) {
            return true;
        }
        let mut on = self.layers.contains(&l);
        if self.enable.iter().any(|p| pattern_matches(p, id)) {
            on = true;
        }
        if self.disable.iter().any(|p| pattern_matches(p, id)) {
            on = false;
        }
        on
    }

    /// Whether a concrete finding is on this surface (§5/§6). Adds the facet
    /// selector to [`Surface::surfaces_id`]: under `throws-direct` a
    /// `throw.undeclared` finding is kept only when its origin facet is `direct`.
    #[must_use]
    pub fn is_surfaced(&self, d: &Diagnostic) -> bool {
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
        if self.warn.iter().any(|p| pattern_matches(p, id)) {
            Level::Warn
        } else {
            Level::Fail
        }
    }

    /// The resolved capture id-set (§8): every registered id this surface admits,
    /// facet-agnostic, sorted. Written into the baseline header by `--set-baseline`.
    #[must_use]
    pub fn surface_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = DIAGNOSTIC_REGISTRY
            .iter()
            .map(|(id, _)| *id)
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
        CALL_ON_NULL_ID, EFFECT_ID, PARAM_MISMATCH_ID, PHPDOC_PROP_MISMATCH_ID,
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
        assert_eq!(
            empty().resolve(Some("strict")),
            Err(ConfigError::ReservedName("strict".to_owned()))
        );
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
        m.insert("strict".to_owned(), UserProfile::default());
        assert_eq!(
            ProfileConfigs(m).resolve(None),
            Err(ConfigError::ReservedDefinition("strict".to_owned()))
        );

        let mut m = BTreeMap::new();
        m.insert("default".to_owned(), UserProfile::default());
        assert_eq!(
            ProfileConfigs(m).resolve(None),
            Err(ConfigError::BuiltinRedefinition("default".to_owned()))
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
    fn surface_ids_are_facet_agnostic_and_layered() {
        let d = empty().resolve(None).unwrap();
        assert!(!d.surface_ids().iter().any(|i| i == THROW_UNDECLARED_ID));
        assert!(d.surface_ids().iter().any(|i| i == CALL_ON_NULL_ID));

        let td = empty().resolve(Some("throws-direct")).unwrap();
        assert!(td.surface_ids().iter().any(|i| i == THROW_UNDECLARED_ID));
        assert!(!td.surface_ids().iter().any(|i| i == PHPDOC_PROP_MISMATCH_ID));
    }
}
