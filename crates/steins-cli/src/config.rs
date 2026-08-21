//! `steins.toml` — the typed sections and the loaders that read them.
//!
//! `check`, `doctor` and MCP parse the file once and strictly through
//! [`read_steins_config`] (ADR-0050 §7 / ADR-0052 §5 N2: a malformed file is
//! exit 2, never warn-and-proceed), then decompose it with the
//! `*_from_config` helpers. `transform` keeps its own lenient `--config`
//! loaders, [`load_vouches`] and [`load_partitions`] (ADR-0046 §2 / ADR-0047
//! §7). The `*_from_disk` helpers serve surfaces with no already-parsed
//! config, reading as leniently as [`allow_list_from_disk`].

use std::path::PathBuf;

use steins_db::EffectsPolicy;
use steins_edit::{PartitionMap, VouchSet};
use steins_infer::FinalKeyword;

use crate::profile;

/// `steins.toml` — `[transform.vouch]` (ADR-0046 §2) and
/// `[transform.partitions]` (ADR-0047 §7). Unknown keys ignored.
#[derive(serde::Deserialize, Default)]
pub(crate) struct SteinsConfig {
    pub(crate) transform: Option<TransformConfig>,
    pub(crate) runtime: Option<RuntimeConfig>,
    /// The `[check]` section (ADR-0050 §5): the repo's default profile selection.
    pub(crate) check: Option<CheckConfig>,
    /// The `[profile.<name>]` table (ADR-0050 §5): user-defined profiles.
    pub(crate) profile: Option<std::collections::BTreeMap<String, ProfileEntryConfig>>,
    /// The `[plugins]` section (ADR-0039/0068): the explicit plugin listing.
    pub(crate) plugins: Option<PluginsConfig>,
    /// The `[paths]` section (issue #181): the no-manifest vendor-dir config
    /// channel.
    paths: Option<PathsConfig>,
    /// The `[doctor]` section (ADR-0054 §14 deferred-with-design, issue #268):
    /// `require`'s named posture-to-failure assertions.
    pub(crate) doctor: Option<DoctorConfig>,
    /// The `[effects]` section (ADR-0084 §1): the tolerated-effects policy and
    /// the attribution table it grips.
    pub(crate) effects: Option<EffectsConfig>,
}

/// The `[effects]` section (ADR-0084 §1) — tolerated-effects policy. NOT a
/// `[profile.*]` field (ADR-0050 §10): this changes which findings exist.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectsConfig {
    /// The labels the envelope judgment discharges — policy.
    #[serde(default)]
    tolerated: Vec<String>,
    /// `[effects.attribution]`: symbol → labels its effects are *for*. Fact,
    /// not policy — inert until `tolerated` names its label.
    #[serde(default)]
    attribution: std::collections::BTreeMap<String, Vec<String>>,
}

/// The `[doctor]` section (ADR-0054 §14, issue #268): `require = [...]` turns
/// posture facts doctor otherwise only reports into its exit 1. An unknown
/// name inside `require` is validated by `doctor::known_assertions`.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorConfig {
    #[serde(default)]
    pub(crate) require: Vec<String>,
}

/// The `[paths]` section (issue #181): config channel for a project with no
/// `composer.json`, consulted only at the vendor floor.
#[derive(serde::Deserialize, Default)]
struct PathsConfig {
    /// Extra vendor directory-name sequences, `/`-separated, matched
    /// whole-component like `vendor` (`vendor_proj/` never matches).
    #[serde(rename = "vendor-dirs", default)]
    vendor_dirs: Vec<String>,
}

/// The `[plugins]` section (ADR-0039/ADR-0068 §2). `allow = [...]`
/// **replaces** `installed.json` discovery, so `allow = []` loads nothing.
/// Listing a plugin also vouches for it, lifting the vendor-root label rule.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginsConfig {
    #[serde(default)]
    allow: Vec<String>,
}

/// The `[check]` section (ADR-0050 §5): default profile name; `--profile` beats it.
#[derive(serde::Deserialize, Default)]
pub(crate) struct CheckConfig {
    profile: Option<String>,
}

/// A `[profile.<name>]` entry (ADR-0050 §5): `extends` a base, refines with
/// ADR-0022 prefix id-arrays. Facet tokens error as unknown id patterns (v1).
#[derive(serde::Deserialize, Default)]
pub(crate) struct ProfileEntryConfig {
    extends: Option<String>,
    #[serde(default)]
    enable: Vec<String>,
    #[serde(default)]
    disable: Vec<String>,
    #[serde(default)]
    warn: Vec<String>,
}

/// The `[runtime]` section (ADR-0037 §2): boot-truth pseudo-constants the
/// checker can't observe from source. `deny_unknown_fields` so a typo can't
/// silently keep the safe default. Abolished `zend-assertions` also lands
/// here as an unknown-key error (2026-07-25: `assert()` is a throw-guard).
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeConfig {
    /// `warning-handler = "abort" | "null"` (ADR-0049 §7): what a proven
    /// `E_WARNING` does at runtime. Default `"abort"` emits proven
    /// warning-grade findings; `"null"` silences them (app tolerates it).
    #[serde(rename = "warning-handler", default)]
    pub(crate) warning_handler: Option<String>,
    /// `final-keyword = "enforced" | "stripped"` (issue #234): default
    /// `"enforced"` is PHP's own rule; `"stripped"` declares a loader that
    /// strips it (e.g. `dg/bypass-finals`), making `FinalClass&MockObject`
    /// real under test. See [`steins_infer::FinalKeyword`].
    #[serde(rename = "final-keyword", default)]
    pub(crate) final_keyword: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct TransformConfig {
    pub(crate) vouch: Option<VouchConfig>,
    partitions: Option<PartitionsConfig>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct VouchConfig {
    /// User-vouched dynamic-code sites as `file:line` entries.
    #[serde(default)]
    pub(crate) sites: Vec<String>,
}

/// The `[transform.partitions]` section (ADR-0047 §7): observer globs and the
/// name→glob-list `sets` table. Region assignment is a pure function of these
/// plus the file path ([`PartitionMap`]).
#[derive(serde::Deserialize, Default)]
struct PartitionsConfig {
    /// Observer path-sets (tests, dev-scripts; ADR-0047 §1).
    #[serde(default)]
    observers: Vec<String>,
    /// Partition name → glob list; `BTreeMap` for deterministic iteration.
    #[serde(default)]
    sets: std::collections::BTreeMap<String, Vec<String>>,
}

/// Load the vouching valve from `steins.toml` (ADR-0046 §2): `--config` else
/// `./steins.toml`. Returns [`VouchSet`] plus warnings for a missing explicit
/// `--config`, parse error, or malformed `file:line` entry.
pub(crate) fn load_vouches(config_path: Option<&str>) -> (VouchSet, Vec<String>) {
    let mut warnings = Vec::new();
    let (path, explicit) = match config_path {
        Some(p) => (PathBuf::from(p), true),
        None => (PathBuf::from("steins.toml"), false),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            if explicit {
                warnings.push(format!("--config {}: cannot read; proceeding with no vouches", path.display()));
            }
            return (VouchSet::empty(), warnings);
        }
    };
    let config: SteinsConfig = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("{}: parse error ({e}); proceeding with no vouches", path.display()));
            return (VouchSet::empty(), warnings);
        }
    };
    let sites = config.transform.and_then(|t| t.vouch).map(|v| v.sites).unwrap_or_default();
    let mut entries: Vec<(String, u32)> = Vec::new();
    for raw in sites {
        // `file:line` — split on the LAST colon so Windows drive letters survive.
        match raw.rsplit_once(':').and_then(|(f, l)| {
            let line = l.trim().parse::<u32>().ok()?;
            (!f.trim().is_empty()).then(|| (f.trim().to_owned(), line))
        }) {
            Some(entry) => entries.push(entry),
            None => warnings.push(format!(
                "steins.toml [transform.vouch]: malformed site `{raw}` (want `file:line`); skipped"
            )),
        }
    }
    (VouchSet::from_entries(entries), warnings)
}

/// The tolerated-effects policy from an already-parsed config (ADR-0084 §1).
/// `no_tolerated` (`--no-tolerated-effects`) empties tolerance while leaving
/// attribution in place — attribution is fact, tolerance is policy.
pub(crate) fn effects_from_config(effects: Option<EffectsConfig>, no_tolerated: bool) -> EffectsPolicy {
    let effects = effects.unwrap_or_default();
    let policy = EffectsPolicy::new(effects.tolerated, effects.attribution);
    if no_tolerated { policy.without_tolerance() } else { policy }
}

/// [`effects_from_config`] for surfaces without an already-parsed config, as
/// leniently as [`allow_list_from_disk`] reads the plugin allow-list.
pub(crate) fn effects_policy_from_disk() -> EffectsPolicy {
    effects_from_config(read_steins_config().ok().flatten().and_then(|c| c.effects), false)
}

/// The `[plugins] allow` list: `Some(names)` when present (`[]` deliberately
/// loads nothing), `None` when absent (`installed.json` discovery in charge).
pub(crate) fn allow_list(plugins: Option<PluginsConfig>) -> Option<Vec<String>> {
    plugins.map(|p| p.allow)
}

/// [`allow_list`] for surfaces without an already-parsed config. Lenient: an
/// unparseable `steins.toml` leaves discovery in charge.
pub(crate) fn allow_list_from_disk() -> Option<Vec<String>> {
    allow_list(read_steins_config().ok().flatten().and_then(|c| c.plugins))
}

/// `[paths] vendor-dirs` (issue #181), read as leniently as
/// [`allow_list_from_disk`]: missing/unparseable → no extra dirs.
pub(crate) fn vendor_dirs_from_disk() -> Vec<String> {
    read_steins_config().ok().flatten().and_then(|c| c.paths).map(|p| p.vendor_dirs).unwrap_or_default()
}

/// Read and parse `./steins.toml` once for `check`/`doctor` (ADR-0050 §7 /
/// ADR-0052 §5 N2). `Ok(None)`: no file. `Err`: doesn't parse, INCLUDING an
/// unknown `[runtime]` key — a hard error (exit 2), never warn-and-proceed.
/// Transform's `--config` keeps its own lenient loaders (ADR-0046 §2).
pub(crate) fn read_steins_config() -> Result<Option<SteinsConfig>, String> {
    let path = PathBuf::from("steins.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    toml::from_str::<SteinsConfig>(&text)
        .map(Some)
        .map_err(|e| format!("{}: parse error ({e})", path.display()))
}

/// The `[runtime]` pseudo-constants a run analyzes under (ADR-0037 §2). Every
/// slot has a safe default, resolved by [`runtime_from_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimePostures {
    /// `warning-handler` (ADR-0049 §7 amendment): `true` for `"abort"`.
    pub(crate) warning_handler_abort: bool,
    /// `final-keyword` (issue #234), consumed by steins-contract's inhabitance judgment.
    pub(crate) final_keyword: FinalKeyword,
}

/// Derive the `[runtime]` pseudo-constants from the already-parsed config.
/// Returns [`RuntimePostures`] plus warnings for an unrecognized value on a
/// known key. Absence defaults to `"abort"`/`"enforced"`.
pub(crate) fn runtime_from_config(runtime: Option<RuntimeConfig>) -> (RuntimePostures, Vec<String>) {
    let mut warnings = Vec::new();
    let runtime = runtime.unwrap_or_default();
    // Default "abort" (ADR-0049 §7): a proven E_WARNING is a runtime break.
    let warning_handler_abort = match runtime.warning_handler.as_deref() {
        None | Some("abort") => true,
        Some("null") => false,
        Some(other) => {
            warnings.push(format!(
                "steins.toml [runtime] warning-handler: unknown value `{other}` (want \"abort\"|\"null\"); using abort"
            ));
            true
        }
    };
    // Default "enforced" is PHP's own rule (issue #234).
    let final_keyword = match runtime.final_keyword.as_deref() {
        None | Some("enforced") => FinalKeyword::Enforced,
        Some("stripped") => FinalKeyword::Stripped,
        Some(other) => {
            warnings.push(format!(
                "steins.toml [runtime] final-keyword: unknown value `{other}` (want \"enforced\"|\"stripped\"); using enforced"
            ));
            FinalKeyword::Enforced
        }
    };
    (RuntimePostures { warning_handler_abort, final_keyword }, warnings)
}

/// Derive the profile selection and user-profile table from the already-parsed
/// config (ADR-0050 §5). Pure decomposition — parsing already happened in
/// [`read_steins_config`].
pub(crate) fn profiles_from_config(
    check: Option<CheckConfig>,
    profile: Option<std::collections::BTreeMap<String, ProfileEntryConfig>>,
) -> (Option<String>, profile::ProfileConfigs) {
    let selected = check.and_then(|c| c.profile);
    let map = profile
        .unwrap_or_default()
        .into_iter()
        .map(|(name, e)| {
            (
                name,
                profile::UserProfile {
                    extends: e.extends,
                    enable: e.enable,
                    disable: e.disable,
                    warn: e.warn,
                },
            )
        })
        .collect();
    (selected, profile::ProfileConfigs(map))
}

/// Load the region map from `[transform.partitions]` (ADR-0047 §7). `Ok(None)`
/// (single-region identity) when missing/unparseable/no section. `Err` only
/// for overlapping partition path-sets (no defined assignment).
pub(crate) fn load_partitions(config_path: Option<&str>) -> Result<Option<PartitionMap>, String> {
    let path = match config_path {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("steins.toml"),
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    // Parse error already reported by `load_vouches`; proceed as identity.
    let Ok(config) = toml::from_str::<SteinsConfig>(&text) else {
        return Ok(None);
    };
    let Some(partitions) = config.transform.and_then(|t| t.partitions) else {
        return Ok(None);
    };
    // Empty section is still the identity.
    if partitions.sets.is_empty() && partitions.observers.is_empty() {
        return Ok(None);
    }
    PartitionMap::build(partitions.sets, partitions.observers)
        .map(Some)
        .map_err(|e| format!("steins.toml [transform.partitions]: {e}"))
}
