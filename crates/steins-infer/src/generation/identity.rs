//! What identifies a generation (ADR-0092 §2): the analyzer's own version; the
//! identity inputs, filled once and used twice — as the generation id with
//! every package's source fingerprint, and as the replay stamp without them;
//! and the universe verdict digest a replayed block must also match. The config
//! rows are a persisted format, pinned by this module's tests: a byte that
//! moves in them invalidates every existing store.

use steins_db::PluginFacts;
use steins_gen::{EnginePosture, FieldHasher, Fingerprint, GenerationId, GenerationInputs};

use super::GenerationParams;
use super::load::Plan;
use crate::walk_plan::UniverseVerdict;
use crate::RuntimePostures;

/// The analyzer's own version — **a new Steins is a new universe**, and this is
/// the value that has to make that true (issue #563).
///
/// The workspace version alone did not. Two builds of `0.1.6` shared a store, so
/// the second could be served findings the first computed — which inverts
/// ADR-0092 §2's standing invariant: a miss costs time and never changes an
/// answer, and a hit was changing one. The exposed cases were every build that
/// is not a tagged release (`cargo install --git`, a nightly, a local build) and
/// every A/B a contributor runs in one working tree.
///
/// `STEINS_ANALYZER_FINGERPRINT` is a content hash of `crates/*/src`, stamped by
/// this crate's build script; see it for why the sources rather than the git
/// revision, and what the choice costs. A released binary has fixed sources and
/// keeps a stable identity across rebuilds.
pub(super) fn analyzer_version() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), "+", env!("STEINS_ANALYZER_FINGERPRINT"))
}

/// The run's whole-universe verdict digest: one fingerprint over every input
/// a file's walk reads that is neither that file's tree, another file's tree,
/// nor the merged index (ADR-0092 §5, issue #489 slice B).
///
/// This is the "a whole-universe verdict moved ⇒ walk everything" leg of the
/// pinned affected set, made comparable across generations. The verdicts are
/// streamed as tagged fields rather than concatenated into a string because
/// two of them — the never-returning set and the property-write obstacle —
/// are universe-sized.
pub(super) fn universe_digest(verdict: &UniverseVerdict<'_>) -> Fingerprint {
    let mut h = FieldHasher::new("steins-infer/universe-verdict");
    verdict.fields(&mut |tag, bytes| {
        h.field(tag, bytes);
    });
    h.finish()
}

/// The identity inputs the run establishes for itself rather than takes from
/// its params: the `composer.lock` content hash, and the engine posture off
/// the folder's own recorded boot surface. With [`identity_inputs`] they are
/// the whole identity but its per-package half.
pub(super) struct RunIdentity {
    composer_lock: Option<Fingerprint>,
    engine: EnginePosture,
}

impl RunIdentity {
    pub(super) fn read(p: &GenerationParams<'_>, folder: &crate::RecordingFolder) -> Self {
        let engine = posture_of(folder.engine_identity().as_ref());
        let composer_lock = p
            .layout
            .roots()
            .last()
            .and_then(|root| std::fs::read(root.dir().join("composer.lock")).ok())
            .map(|bytes| Fingerprint::of_bytes("steins-gen/composer.lock", &bytes));
        Self { composer_lock, engine }
    }

    /// The replay stamp: the generation identity with the per-package source
    /// fingerprints left out, because those are gated per package by the
    /// `sources` section already. Everything else — the analyzer version, the
    /// lock, the catalog pin, the plugin channel, the engine posture, the
    /// finding-relevant config — must be unmoved before one persisted finding
    /// may be replayed. (This is the re-audit the issue asks for: under slice A
    /// an under-covered input cost a stale *cache*; here it would cost a stale
    /// *finding*, so the gate is the whole identity rather than its package
    /// half.)
    pub(super) fn stamp(&self, p: &GenerationParams<'_>) -> Fingerprint {
        *GenerationInputs {
            packages: Vec::new(),
            ..identity_inputs(p, self.composer_lock, self.engine.clone())
        }
        .generation_id()
        .as_fingerprint()
    }

    /// The generation id: the same inputs as [`Self::stamp`], with every
    /// package's source fingerprint.
    pub(super) fn generation_id(self, p: &GenerationParams<'_>, plans: &[Plan]) -> GenerationId {
        GenerationInputs {
            packages: plans.iter().map(|plan| (plan.name.clone(), plan.fingerprint)).collect(),
            ..identity_inputs(p, self.composer_lock, self.engine)
        }
        .generation_id()
    }
}

/// The engine posture from the run's own recorded boot surface, or
/// [`EnginePosture::Off`] for an engine that never described itself
/// (`--no-php`, a dead sidecar, an old runner).
fn posture_of(identity: Option<&crate::FoldTableIdentity>) -> EnginePosture {
    match identity {
        Some(identity) => EnginePosture::On {
            php_version: identity.php_version.clone(),
            // `None` (an engine that did not say) is encoded as 0 — a width no
            // real engine reports, so it stays its own identity.
            int_size: identity.int_size.and_then(|s| u8::try_from(s).ok()).unwrap_or(0),
            extensions: identity.extensions.clone(),
            fold_lane: identity.fold_lane.clone(),
        },
        None => EnginePosture::Off,
    }
}

/// Everything the generation identity covers except the per-package source
/// fingerprints — see the module docs for what is in and what is deliberately
/// out. Filled once and used twice: whole (plus the packages) as the
/// generation id, and with `packages` emptied as the replay stamp of issue
/// #489 slice B, so the two can never drift apart.
fn identity_inputs(
    p: &GenerationParams<'_>,
    composer_lock: Option<Fingerprint>,
    engine: EnginePosture,
) -> GenerationInputs {
    GenerationInputs {
        analyzer_version: analyzer_version().to_owned(),
        packages: Vec::new(),
        composer_lock,
        catalog_pin: format!(
            "php-{}.{}",
            steins_catalog::PINNED_PHP.0,
            steins_catalog::PINNED_PHP.1
        ),
        plugins: plugin_identity(p.plugins),
        engine,
        config: config_identity(p),
    }
}

/// The plugin channel's finding-relevant content as identity strings: the
/// registered labels and the accepted colorings. Hashed sorted by
/// [`GenerationInputs::generation_id`], so the order here is immaterial.
///
/// A third row, `vocab:<name>`, is owed the moment ADR-0091 §4.1's vocabulary
/// registration kind lands on the manifest — registered vocabulary is half of
/// `phpdoc.unknown-vocabulary`'s allowlist, and a generation that omits it
/// stays warm across a plugin-set change that moved the findings. See the
/// note on the `PHPDOC_UNKNOWN_VOCABULARY_ID` registry entry (suppress.rs).
fn plugin_identity(plugins: &PluginFacts) -> Vec<String> {
    let mut out: Vec<String> =
        plugins.registry().extensions().iter().map(|label| format!("label:{label}")).collect();
    for (name, labels) in plugins.colorings() {
        out.push(format!("effect:{name}={}", labels.join(",")));
    }
    out
}

/// The finding-relevant config as identity pairs — see the module docs for
/// what is covered and what is deliberately out.
///
/// The postures are destructured with no rest pattern, so a new
/// [`RuntimePostures`] field does not compile until it has a row here. Every
/// posture decides findings, so a posture without a row would let two runs of
/// one build under different values share a generation, and the second would
/// replay what the first found (ADR-0092 §2).
fn config_identity(p: &GenerationParams<'_>) -> Vec<(String, String)> {
    let RuntimePostures { warning_handler_abort, final_keyword, os_pin } = p.postures;
    let mut config = vec![
        ("effects.tolerated".to_owned(), p.effects.tolerated().join(",")),
        ("runtime.warning-handler-abort".to_owned(), warning_handler_abort.to_string()),
        ("runtime.final-keyword".to_owned(), format!("{final_keyword:?}")),
        // ADR-0094 §3: the pin decides `PHP_EOL` and its three siblings, so it
        // decides findings — a run under a different pin is a different run.
        ("runtime.os".to_owned(), format!("{os_pin:?}")),
        ("layout".to_owned(), format!("{:?}", p.layout)),
    ];
    for key in p.effects.attribution_keys() {
        config.push((
            format!("effects.attribution:{key}"),
            p.effects.function_attribution(key).join(","),
        ));
    }
    config
}

#[cfg(test)]
mod tests {
    use super::analyzer_version;

    /// Issue #563: the analyzer version has to make its own doc comment true —
    /// "a new Steins is a new universe". `CARGO_PKG_VERSION` alone did not, so
    /// two builds of one version shared a store and the second could be served
    /// findings the first computed.
    #[test]
    fn the_analyzer_version_carries_more_than_the_package_version() {
        let v = analyzer_version();
        assert!(v.starts_with(env!("CARGO_PKG_VERSION")), "{v}");
        assert_ne!(v, env!("CARGO_PKG_VERSION"), "the package version alone cannot separate builds");
        let (_, fingerprint) = v.split_once('+').expect("version+fingerprint");
        assert_eq!(fingerprint.len(), 16, "a 64-bit hash rendered as hex: {fingerprint}");
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()), "{fingerprint}");
        // Not the degenerate hash: an empty source walk would leave the FNV-1a
        // offset basis untouched, and a fingerprint that never moves is the bug
        // this replaces wearing a longer string.
        assert_ne!(fingerprint, "cbf29ce484222325", "the source walk found nothing to hash");
    }
    use std::path::{Path, PathBuf};

    use steins_db::{EffectsPolicy, PackagePartition, PluginFacts, ProjectLayout};

    use super::{GenerationParams, config_identity};
    use crate::{FinalKeyword, OsFamily, RuntimePostures};

    /// Render `config_identity` for fixed params, as `(key, value)` string pairs.
    fn identity_rows(
        effects: &EffectsPolicy,
        warning_handler_abort: bool,
        final_keyword: FinalKeyword,
        os_pin: Option<OsFamily>,
    ) -> Vec<(String, String)> {
        let layout = ProjectLayout::fallback();
        let partition = PackagePartition::from_lock(&layout, None);
        let plugins = PluginFacts::none();
        let files: [PathBuf; 0] = [];
        let params = GenerationParams {
            store_root: Path::new("store"),
            capture_root: Path::new("capture"),
            files: &files,
            layout: &layout,
            partition: &partition,
            plugins: &plugins,
            effects,
            postures: RuntimePostures { warning_handler_abort, final_keyword, os_pin },
            php: false,
            paranoid: false,
        };
        config_identity(&params)
    }

    fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect()
    }

    /// The config identity rows are hashed into every published generation id
    /// and every replay stamp, so their keys, values and order are a persisted
    /// format: a byte that moves here invalidates every existing store. Pinned
    /// for the defaults and for every `[runtime]` posture moved off its default.
    #[test]
    fn config_identity_rows_are_byte_stable() {
        let layout = r#"ProjectLayout { cwd: "", roots: [], extra_vendor_dirs: [] }"#;
        assert_eq!(
            identity_rows(&EffectsPolicy::none(), true, FinalKeyword::Enforced, None),
            rows(&[
                ("effects.tolerated", ""),
                ("runtime.warning-handler-abort", "true"),
                ("runtime.final-keyword", "Enforced"),
                ("runtime.os", "None"),
                ("layout", layout),
            ]),
        );
        let effects = EffectsPolicy::new(
            vec!["io".to_owned(), "db".to_owned()],
            vec![("App\\Log::debug".to_owned(), vec!["io".to_owned()])],
        );
        assert_eq!(
            identity_rows(&effects, false, FinalKeyword::Stripped, Some(OsFamily::Linux)),
            rows(&[
                ("effects.tolerated", "db,io"),
                ("runtime.warning-handler-abort", "false"),
                ("runtime.final-keyword", "Stripped"),
                ("runtime.os", "Some(Linux)"),
                ("layout", layout),
                ("effects.attribution:App\\Log::debug", "io"),
            ]),
        );
    }
}
