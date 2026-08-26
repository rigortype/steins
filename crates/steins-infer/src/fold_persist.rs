//! The persistence transport (ADR-0092 §4): fold results recorded during a
//! generation build and replayed from disk through the ADR-0066 table seam.
//!
//! [`RecordingEngine`] is a [`FoldEngine`] over a loaded table plus a live
//! [`ProcessEngine`]: it answers table-first, falls through to the live engine
//! on a miss, and records every answer it serves — so the table it publishes
//! at the end of a run holds exactly the rows the run consumed or newly asked
//! (mark-and-sweep by construction; no TTLs, no caps). The policy is untouched:
//! [`EngineFolder`] carries every gate exactly once (ADR-0066 §3, the issue-#63
//! lesson), and this module is a transport under it, beside `fold_process` and
//! `fold_table`.
//!
//! On disk the table is **one generation-level artifact**, not a per-package
//! one (ADR-0092 §4's 2026-08-25 amendment): a warm run rebuilding one package
//! folds through calls that can name anything, ADR-0066's replay loop drives
//! exactly one table, and a row's validity is scoped by engine identity, never
//! by source location. It lives beside the package artifacts under the
//! reserved package name [`FOLD_PACKAGE`], with two sections: the engine
//! identity ([`FoldTableIdentity`]) and the rows — the ADR-0066 wire key (the
//! JSON-RPC request minus `id`, [`request_key`]) mapped to the raw `result`,
//! the exact shapes `sw_check_replay` already exchanges.
//!
//! Failure semantics, unchanged and load-bearing: an identity mismatch is a
//! miss for the **whole** table (drop it, ask live); a malformed row is a miss
//! for **that row** (ask live, and publish the fresh answer in its place); an
//! unanswerable request widens exactly as a dead sidecar does. Nothing
//! fabricates, and a recorded row cannot outlive the identity that scopes it.

use std::collections::BTreeMap;

use steins_gen::{ArtifactBuilder, ArtifactReader, Miss, PackageName, SectionName};
use steins_sidecar::{
    ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection,
};

use crate::fold::{EngineFolder, FoldEngine, FoldPosture, fold_lane_at_width};
use crate::fold_process::ProcessEngine;
use crate::fold_table::request_key;

// ---------------------------------------------------------------------------
// The reserved artifact name and its sections.
// ---------------------------------------------------------------------------

/// The reserved [`PackageName`] of the generation-level fold artifact.
///
/// Reserved, not resolved: no Composer package can collide with it — Composer
/// names are `vendor/name` and the double-underscore spelling is the same
/// convention that keeps `__first_party__` out of vendor's namespace — and the
/// builder must never treat it as a source package (it has no sources, no
/// shard, no inventory; it is the one artifact scoped to the whole generation
/// rather than to a package, per ADR-0092 §4's 2026-08-25 amendment).
pub const FOLD_PACKAGE: &str = "__fold__";

/// The section holding the [`FoldTableIdentity`] JSON object.
pub const FOLD_IDENTITY_SECTION: &str = "identity";

/// The section holding the rows: a JSON object of [`request_key`] → the raw
/// JSON-RPC `result` for that request — the ADR-0066 replay table itself,
/// serialized.
pub const FOLD_ROWS_SECTION: &str = "rows";

/// [`FOLD_PACKAGE`] as a validated [`PackageName`].
#[must_use]
pub fn fold_package() -> PackageName {
    PackageName::new(FOLD_PACKAGE).expect("the reserved fold package name is valid")
}

fn identity_section() -> SectionName {
    SectionName::new(FOLD_IDENTITY_SECTION).expect("the fold identity section name is valid")
}

fn rows_section() -> SectionName {
    SectionName::new(FOLD_ROWS_SECTION).expect("the fold rows section name is valid")
}

// ---------------------------------------------------------------------------
// The engine identity a table is scoped by.
// ---------------------------------------------------------------------------

/// The engine identity a stored fold table is keyed under (ADR-0092 §4): the
/// boot-surface fields that decide what the engine would answer, plus the
/// strict posture axis of the row keys. On load the stored identity is
/// compared with the live engine's own boot surface, taken then and there; any
/// mismatch is a miss for the whole table — a different engine asks everything
/// again, never reinterprets.
///
/// Both sides of the comparison come through [`FoldTableIdentity::from_env`],
/// so the normalization (extensions sorted and lowercased, the lane derived by
/// the gate's own `fold_lane_at_width`) can never disagree with itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldTableIdentity {
    /// The engine's `PHP_VERSION`, verbatim.
    pub php_version: String,
    /// The engine's `PHP_INT_SIZE` in bytes; `None` = the engine did not say
    /// (an old runner), which is its own identity — never "close enough" to a
    /// reported width.
    pub int_size: Option<u32>,
    /// The loaded extensions, lowercased and sorted: a set, not a load order,
    /// the same normalization the class-world identity uses.
    pub extensions: Vec<String>,
    /// The fold lane the width admits ([`crate::FoldLane::as_str`]) — derived
    /// from `int_size` today, and still its own axis: if the lane rules move
    /// under an unchanged width, tables recorded under the old lane must drop.
    pub fold_lane: String,
    /// The strict posture axis (issue #383): whether every fold row's key
    /// carries the call site's `declare(strict_types=1)` mode. Always `true`
    /// for a table this analyzer writes — the flag is in [`request_key`]'s
    /// params — and compared like every other axis, so a table recorded under
    /// a seam that did not key strictness can never serve one that does.
    pub strict_keyed: bool,
}

impl FoldTableIdentity {
    /// The identity of the engine `env` describes — the one constructor, used
    /// for both the recording side and the loading side of the comparison.
    #[must_use]
    pub fn from_env(env: &EnvInfo) -> Self {
        let mut extensions: Vec<String> =
            env.extensions.iter().map(|e| e.to_ascii_lowercase()).collect();
        extensions.sort_unstable();
        Self {
            php_version: env.php_version.clone(),
            int_size: env.int_size,
            extensions,
            fold_lane: fold_lane_at_width(env.int_size).as_str().to_owned(),
            strict_keyed: true,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "php_version": self.php_version,
            "int_size": self.int_size,
            "extensions": self.extensions,
            "fold_lane": self.fold_lane,
            "strict_keyed": self.strict_keyed,
        })
    }

    /// Strict inverse of [`Self::to_json`]: exactly these fields, exactly
    /// these types. `None` is the whole-table miss — an identity that cannot
    /// be read cannot match anything.
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let obj = value.as_object()?;
        if obj.len() != 5 {
            return None;
        }
        let int_size = match obj.get("int_size")? {
            serde_json::Value::Null => None,
            size => Some(u32::try_from(size.as_u64()?).ok()?),
        };
        Some(Self {
            php_version: obj.get("php_version")?.as_str()?.to_owned(),
            int_size,
            extensions: obj
                .get("extensions")?
                .as_array()?
                .iter()
                .map(|e| e.as_str().map(ToOwned::to_owned))
                .collect::<Option<Vec<_>>>()?,
            fold_lane: obj.get("fold_lane")?.as_str()?.to_owned(),
            strict_keyed: obj.get("strict_keyed")?.as_bool()?,
        })
    }
}

// ---------------------------------------------------------------------------
// The artifact: identity + rows, through the steins-gen container.
// ---------------------------------------------------------------------------

/// The generation-level fold artifact, decoded: the identity its rows are
/// scoped by, and the rows themselves ([`request_key`] → raw `result`).
///
/// A cache, not an interchange format (ADR-0092 §2): the section payloads are
/// JSON because the rows already are, and the container's schema version
/// obsoletes them wholesale. Every way the bytes can be wrong comes back from
/// [`Self::read`] as a [`Miss`] — the caller runs cold — while a wrong *row*
/// inside a readable table is not detectable here at all and is degraded
/// row-by-row where it is consumed ([`RecordingEngine`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldTableArtifact {
    /// The identity of the engine that answered every row.
    pub identity: FoldTableIdentity,
    /// The rows, sorted by key so the serialization is deterministic.
    pub rows: BTreeMap<String, serde_json::Value>,
}

impl FoldTableArtifact {
    /// The container to persist this table as — two sections, ready for
    /// [`ArtifactBuilder::write_to`] or a candidate's `write_artifact` under
    /// [`fold_package`].
    #[must_use]
    pub fn to_builder(&self) -> ArtifactBuilder {
        let mut builder = ArtifactBuilder::new();
        let identity =
            serde_json::to_vec(&self.identity.to_json()).expect("an identity object serializes");
        builder.section(identity_section(), identity).expect("distinct section names");
        let mut rows = serde_json::Map::with_capacity(self.rows.len());
        for (key, value) in &self.rows {
            rows.insert(key.clone(), value.clone());
        }
        let rows =
            serde_json::to_vec(&serde_json::Value::Object(rows)).expect("a row object serializes");
        builder.section(rows_section(), rows).expect("distinct section names");
        builder
    }

    /// Decode a fold artifact. Anything wrong — an absent section, bytes that
    /// are not JSON, an identity or row object of the wrong shape — is a
    /// [`Miss`], and a miss here means the whole table: the caller runs with
    /// no table and the build records afresh (cost, never meaning).
    pub fn read(reader: &mut ArtifactReader) -> Result<Self, Miss> {
        let identity = section_json(reader, &identity_section())?;
        let identity = FoldTableIdentity::from_json(&identity)
            .ok_or(Miss::Corrupt("fold identity section is not an engine identity"))?;
        let rows = section_json(reader, &rows_section())?;
        let serde_json::Value::Object(rows) = rows else {
            return Err(Miss::Corrupt("fold rows section is not a key-to-result object"));
        };
        let rows = rows.into_iter().collect();
        Ok(Self { identity, rows })
    }
}

fn section_json(reader: &mut ArtifactReader, name: &SectionName) -> Result<serde_json::Value, Miss> {
    let bytes = reader.section(name)?;
    serde_json::from_slice(&bytes).map_err(|_| Miss::Corrupt("fold section is not JSON"))
}

// ---------------------------------------------------------------------------
// The recording table-first transport.
// ---------------------------------------------------------------------------

/// The warm-path [`FoldEngine`] (ADR-0092 §4): table-first over recorded rows,
/// live [`ProcessEngine`] on a miss, recording everything it serves.
///
/// Three properties, each an oracle in `tests/fold_table_persistence.rs`:
///
/// * **Replay is not a second semantics.** A hit answers through the same
///   `parse_*_result` readers [`TableEngine`] uses, a miss through the same
///   [`ProcessEngine::call_raw`] dispatch the replay loop's answerer uses, so
///   a warm run's findings are byte-identical to a live run's.
/// * **Mark-and-sweep by construction.** [`Self::artifact`] returns exactly
///   [`Self::recorded_rows`] — the rows this run consumed or newly asked. A
///   loaded row nothing asked is not carried forward; there is no TTL and no
///   cap, because unreachable rows simply fail to survive the run.
/// * **A malformed row is a miss for that row.** A loaded row whose `result`
///   no longer parses is asked live again — never surfaced to analysis as an
///   error, never served as a widen it did not earn — and the fresh answer
///   replaces it in the published table.
///
/// [`TableEngine`]: crate::fold_table::TableEngine
pub struct RecordingEngine {
    live: ProcessEngine,
    /// The rows loaded from a published artifact — empty on a cold run, and
    /// emptied wholesale when the identity gate refuses ([`Self::warm`]).
    loaded: BTreeMap<String, serde_json::Value>,
    /// The rows this run consumed or newly asked — the next artifact's rows.
    recorded: BTreeMap<String, serde_json::Value>,
    /// The keys the live engine answered (in first-answer order): the misses.
    /// A warm run over an unchanged source records none — the differential
    /// oracle's second assertion.
    fresh: Vec<String>,
}

impl RecordingEngine {
    /// A cold recording engine: no table, every question live, every answer
    /// recorded. The generation build's engine.
    #[must_use]
    pub fn cold(live: ProcessEngine) -> Self {
        Self { live, loaded: BTreeMap::new(), recorded: BTreeMap::new(), fresh: Vec::new() }
    }

    /// A warm recording engine over a decoded artifact, **identity-gated**:
    /// the live engine's own boot surface is taken now and compared with the
    /// stored identity, and on any mismatch — a different engine, or one that
    /// does not describe itself at all — the whole table is a miss and this is
    /// [`Self::cold`]. The gate costs one `env` round trip per load, the same
    /// price ADR-0066's boot-surface amendment already accepted.
    #[must_use]
    pub fn warm(mut live: ProcessEngine, artifact: FoldTableArtifact) -> Self {
        let live_identity = live
            .call_raw("env", steins_sidecar::env_params())
            .and_then(|raw| steins_sidecar::parse_env_result(&raw))
            .map(|env| FoldTableIdentity::from_env(&env));
        let loaded = if live_identity.as_ref() == Some(&artifact.identity) {
            artifact.rows
        } else {
            BTreeMap::new()
        };
        Self { live, loaded, recorded: BTreeMap::new(), fresh: Vec::new() }
    }

    /// The rows this run consumed or newly asked, so far.
    #[must_use]
    pub fn recorded_rows(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.recorded
    }

    /// The keys the live engine answered this run, in first-answer order.
    #[must_use]
    pub fn fresh_keys(&self) -> &[String] {
        &self.fresh
    }

    /// The artifact this run would publish: the recorded rows under the
    /// identity of the engine that answered them — read off the run's own
    /// recorded `env` row, so the identity and the rows come from one reply
    /// and a row can never be published under an identity it was not asked
    /// under. `None` when the run recorded no readable `env` — an engine that
    /// never described itself has nothing publishable.
    #[must_use]
    pub fn artifact(&self) -> Option<FoldTableArtifact> {
        let env = self.recorded.get(&request_key("env", &steins_sidecar::env_params()))?;
        let env = steins_sidecar::parse_env_result(env)?;
        Some(FoldTableArtifact {
            identity: FoldTableIdentity::from_env(&env),
            rows: self.recorded.clone(),
        })
    }

    /// What the live engine delivered over the whole run (issue #245) — the
    /// wrapped transport's own ledger, unchanged: a run that answered from the
    /// table throughout has an unengaged posture, which is true (no child was
    /// needed) and is the warm path working, not a degradation.
    #[must_use]
    pub fn posture(&self) -> FoldPosture {
        self.live.posture()
    }

    /// The identity of whatever answered this run's `env` row, without the
    /// rows themselves — [`Self::artifact`]'s first half.
    ///
    /// Split out for issue #489 slice B, whose replay gate needs the engine
    /// posture *before* the first file is walked (a persisted finding may only
    /// be replayed under the engine that produced it). Reads the same recorded
    /// row `artifact` reads, so the two can never disagree; `None` until
    /// something has asked, and for an engine that never describes itself.
    #[must_use]
    pub fn identity(&self) -> Option<FoldTableIdentity> {
        let env = self.recorded.get(&request_key("env", &steins_sidecar::env_params()))?;
        steins_sidecar::parse_env_result(env).map(|env| FoldTableIdentity::from_env(&env))
    }

    /// Answer one request: the loaded table first, the live engine on a miss
    /// — including the malformed-row miss, where `parse` refuses the stored
    /// bytes — recording whatever answered.
    fn ask<T>(
        &mut self,
        method: &str,
        params: serde_json::Value,
        parse: impl Fn(&serde_json::Value) -> Option<T>,
    ) -> Option<T> {
        let key = request_key(method, &params);
        if let Some(raw) = self.loaded.get(&key)
            && let Some(answer) = parse(raw)
        {
            let raw = raw.clone();
            self.recorded.insert(key, raw);
            return Some(answer);
        }
        // No row, or a malformed one `parse` refused — either way a miss,
        // for this key alone: ask live.
        let raw = self.live.call_raw(method, params)?;
        let answer = parse(&raw);
        self.fresh.push(key.clone());
        self.recorded.insert(key, raw);
        answer
    }
}

impl FoldEngine for RecordingEngine {
    fn env(&mut self) -> Option<EnvInfo> {
        self.ask("env", steins_sidecar::env_params(), steins_sidecar::parse_env_result)
    }

    fn reflect(&mut self, target: &str) -> Option<Reflection> {
        self.ask("reflect", steins_sidecar::reflect_params(target), |raw| {
            steins_sidecar::parse_reflection_result(raw, target)
        })
    }

    fn reflect_class(&mut self, target: &str) -> Option<ClassReflection> {
        self.ask("reflect_class", steins_sidecar::reflect_class_params(target), |raw| {
            steins_sidecar::parse_class_reflection_result(raw, target)
        })
    }

    fn fold(&mut self, name: &str, args: &[FoldArg], strict: bool) -> FoldResult {
        // Not askable (a non-finite float has no JSON spelling) — the same
        // early widen every transport gives, and no key exists to record.
        let Some(params) = steins_sidecar::fold_params(name, args, strict) else {
            return FoldResult::widen("unrepresentable argument");
        };
        let key = request_key("fold", &params);
        if let Some(raw) = self.loaded.get(&key) {
            // A recorded widen or throw is an ANSWER (the engine said so); only
            // a shape `parse_fold_result` would mistake for one is a miss.
            if steins_sidecar::fold_result_is_well_formed(raw) {
                let raw = raw.clone();
                let answer = steins_sidecar::parse_fold_result(&raw);
                self.recorded.insert(key, raw);
                return answer;
            }
        }
        // The same decline a dead sidecar gives; a miss is not a row.
        let Some(raw) = self.live.call_raw("fold", params) else {
            return FoldResult::widen("no sidecar");
        };
        let answer = steins_sidecar::parse_fold_result(&raw);
        self.fresh.push(key.clone());
        self.recorded.insert(key, raw);
        answer
    }

    fn preg_compile(&mut self, pattern: &str) -> Option<PregCompile> {
        self.ask(
            "preg_compile",
            steins_sidecar::preg_compile_params(pattern),
            steins_sidecar::parse_preg_compile_result,
        )
    }

    fn constant_defined(&mut self, name: &str) -> Option<ConstantDefined> {
        self.ask("defined", steins_sidecar::defined_params(name), steins_sidecar::parse_defined_result)
    }

    /// The wrapped transport's own generation counter: a table row cannot be
    /// replaced mid-run, but the live child underneath can, and the policy's
    /// env-memo refresh must see that exactly as it does on the direct path.
    fn restarts(&self) -> u32 {
        self.live.restarts()
    }
}

/// The warm-path folder: the shared policy over the recording transport.
pub type RecordingFolder = EngineFolder<RecordingEngine>;

impl EngineFolder<RecordingEngine> {
    /// The artifact this run would publish ([`RecordingEngine::artifact`]).
    #[must_use]
    pub fn published_table(&self) -> Option<FoldTableArtifact> {
        self.engine.artifact()
    }

    /// The keys the live engine answered this run ([`RecordingEngine::fresh_keys`]).
    #[must_use]
    pub fn fresh_keys(&self) -> &[String] {
        self.engine.fresh_keys()
    }

    /// The live engine's delivery ledger ([`RecordingEngine::posture`]).
    #[must_use]
    pub fn posture(&self) -> FoldPosture {
        self.engine.posture()
    }

    /// The engine identity behind this folder ([`RecordingEngine::identity`]).
    #[must_use]
    pub fn engine_identity(&self) -> Option<FoldTableIdentity> {
        self.engine.identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use steins_gen::DecodeBudget;

    /// A throwaway directory under the OS temp dir, cleaned on drop.
    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "steins-fold-persist-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn env(version: &str, int_size: Option<u32>, extensions: &[&str]) -> EnvInfo {
        EnvInfo {
            php_version: version.to_owned(),
            extensions: extensions.iter().map(|&e| e.to_owned()).collect(),
            sapi: "cli".to_owned(),
            int_size,
        }
    }

    fn sample_artifact() -> FoldTableArtifact {
        let mut rows = BTreeMap::new();
        rows.insert(
            request_key("env", &steins_sidecar::env_params()),
            serde_json::json!({
                "php_version": "8.5.9",
                "extensions": ["Core", "standard"],
                "sapi": "cli",
                "int_size": 8,
            }),
        );
        rows.insert(
            request_key(
                "fold",
                &steins_sidecar::fold_params(
                    "strtoupper",
                    &[FoldArg::Str("ab".to_owned())],
                    false,
                )
                .expect("askable"),
            ),
            serde_json::json!({ "kind": "value", "type": "string", "value": "AB" }),
        );
        FoldTableArtifact {
            identity: FoldTableIdentity::from_env(&env("8.5.9", Some(8), &["Core", "standard"])),
            rows,
        }
    }

    #[test]
    fn the_reserved_names_are_spellable() {
        assert_eq!(fold_package().as_str(), "__fold__");
        assert_eq!(identity_section().as_str(), FOLD_IDENTITY_SECTION);
        assert_eq!(rows_section().as_str(), FOLD_ROWS_SECTION);
    }

    /// The identity is derived, never restated: the lane comes from the same
    /// three-case function the fold gate branches on, the extensions are a
    /// set, and the strict axis is the seam's own keying convention.
    #[test]
    fn the_identity_derives_its_lane_and_normalizes_its_extensions() {
        let id = FoldTableIdentity::from_env(&env("8.5.9", Some(8), &["standard", "Core"]));
        assert_eq!(id.fold_lane, "full");
        assert_eq!(id.extensions, vec!["core".to_owned(), "standard".to_owned()]);
        assert!(id.strict_keyed);
        assert_eq!(FoldTableIdentity::from_env(&env("8.5.2", Some(4), &[])).fold_lane, "portable_subset");
        assert_eq!(FoldTableIdentity::from_env(&env("8.1.0", None, &[])).fold_lane, "declined");
        // Load order is not identity.
        assert_eq!(
            FoldTableIdentity::from_env(&env("8.5.9", Some(8), &["a", "b"])),
            FoldTableIdentity::from_env(&env("8.5.9", Some(8), &["b", "a"])),
        );
    }

    #[test]
    fn the_identity_json_round_trips_and_rejects_other_shapes() {
        for id in [
            FoldTableIdentity::from_env(&env("8.5.9", Some(8), &["Core"])),
            FoldTableIdentity::from_env(&env("8.5.2", None, &[])),
        ] {
            assert_eq!(FoldTableIdentity::from_json(&id.to_json()), Some(id));
        }
        let good = FoldTableIdentity::from_env(&env("8.5.9", Some(8), &["Core"])).to_json();
        let mut extra = good.clone();
        extra["sapi"] = serde_json::json!("cli");
        let mut wrong_type = good.clone();
        wrong_type["int_size"] = serde_json::json!("8");
        let mut mixed_list = good;
        mixed_list["extensions"] = serde_json::json!(["core", 7]);
        for bad in [serde_json::json!({}), serde_json::json!(42), extra, wrong_type, mixed_list] {
            assert_eq!(FoldTableIdentity::from_json(&bad), None, "{bad}");
        }
    }

    #[test]
    fn the_artifact_round_trips_through_the_container() {
        let tmp = TempDir::new("round-trip");
        let path = tmp.path("fold.pkg");
        let artifact = sample_artifact();
        artifact.to_builder().write_to(&path).unwrap();
        let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
        assert_eq!(FoldTableArtifact::read(&mut reader).unwrap(), artifact);
    }

    /// Every way the *table* can be wrong is a whole-table miss at decode; a
    /// wrong *row* is not detectable here and degrades at consumption instead.
    #[test]
    fn a_misshapen_section_is_a_whole_table_miss() {
        let tmp = TempDir::new("misshapen");
        let cases = [
            ("no-rows", vec![(identity_section(), b"{}".to_vec())]),
            ("identity-not-json", vec![
                (identity_section(), b"not json".to_vec()),
                (rows_section(), b"{}".to_vec()),
            ]),
            ("identity-wrong-shape", vec![
                (identity_section(), b"{}".to_vec()),
                (rows_section(), b"{}".to_vec()),
            ]),
            ("rows-not-object", vec![
                (
                    identity_section(),
                    serde_json::to_vec(&sample_artifact().identity.to_json()).unwrap(),
                ),
                (rows_section(), b"[1, 2]".to_vec()),
            ]),
        ];
        for (tag, sections) in cases {
            let path = tmp.path(tag);
            let mut builder = ArtifactBuilder::new();
            for (name, bytes) in sections {
                builder.section(name, bytes).unwrap();
            }
            builder.write_to(&path).unwrap();
            let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
            assert!(FoldTableArtifact::read(&mut reader).is_err(), "{tag}");
        }
    }

    /// The identity gate needs a live boot surface: an engine that cannot
    /// describe itself matches no stored identity, so the whole table drops —
    /// a loaded row is never served on an identity nobody established, and
    /// with the live engine also mute, everything widens as a dead sidecar
    /// does. Nothing recorded, nothing publishable.
    #[test]
    fn an_engine_with_no_boot_surface_drops_the_whole_table() {
        let mut engine = RecordingEngine::warm(ProcessEngine::new(true), sample_artifact());
        assert_eq!(
            engine.fold("strtoupper", &[FoldArg::Str("ab".to_owned())], false),
            FoldResult::widen("no sidecar"),
            "the loaded row for this exact key must not answer"
        );
        assert_eq!(engine.env(), None);
        assert!(engine.fresh_keys().is_empty(), "nothing was answered, so nothing is fresh");
        assert!(engine.recorded_rows().is_empty());
        assert_eq!(engine.artifact(), None, "a run with no engine publishes no table");
    }
}
