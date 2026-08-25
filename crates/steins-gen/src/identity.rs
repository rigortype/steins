//! Generation identity (ADR-0092 §2): the fingerprint over every input that
//! can change a finding, and nothing that only changes its rendering.

use std::fmt;

use crate::container::SCHEMA_VERSION;
use crate::fingerprint::{FieldHasher, Fingerprint};
use crate::names::PackageName;

/// The identity of one frozen generation — the fingerprint
/// [`GenerationInputs::generation_id`] computes, and the name of its
/// directory in the store. Two runs whose inputs fingerprint alike may share
/// a generation; any covered input moving makes a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationId(Fingerprint);

impl GenerationId {
    /// Lowercase hex — the store's directory name and the `CURRENT` spelling.
    pub fn to_hex(&self) -> String { self.0.to_hex() }

    /// Strict inverse of [`GenerationId::to_hex`], for readers of what the
    /// store wrote (`CURRENT`, the manifest). Not a way to mint identities:
    /// only [`GenerationInputs::generation_id`] does that meaningfully.
    pub fn from_hex(s: &str) -> Option<Self> { Fingerprint::from_hex(s).map(Self) }

    pub fn as_fingerprint(&self) -> &Fingerprint { &self.0 }
}

impl fmt::Display for GenerationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(&self.0, f) }
}

/// The engine posture: the boot surface's identity fields, or engine-off.
/// Plain data here — the crate depends on no other steins crate, so the
/// builder copies the fields out of the boot surface when it wires this in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnginePosture {
    /// No sidecar: fold requests were never on the table for this run.
    Off,
    /// A live engine. Fields per ADR-0092 §2: anything that changes what the
    /// engine would answer is identity; anything cosmetic is not.
    On {
        /// The engine's `PHP_VERSION`.
        php_version: String,
        /// The engine's `PHP_INT_SIZE` in bytes (8, or 4 on a 32-bit PHP).
        int_size: u8,
        /// Loaded extensions. Hashed sorted — a set, not a load order.
        extensions: Vec<String>,
        /// The fold lane in use (ADR-0066's vocabulary).
        fold_lane: String,
    },
}

/// Everything the generation fingerprint covers, in the fixed documented
/// order it is hashed: schema version, analyzer version, per-package source
/// fingerprints, `composer.lock`, catalog pin, plugin set, engine posture,
/// finding-relevant config. The collections are hashed sorted, so the order
/// a builder happens to supply them in is immaterial; the *field* order is
/// fixed by the tags and this type, and changing what is covered is a
/// [`SCHEMA_VERSION`] bump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationInputs {
    /// The analyzer's own version — a new Steins is a new universe.
    pub analyzer_version: String,
    /// Per-package source fingerprints ([`crate::SourceInventory::fingerprint`]),
    /// one per package in the universe.
    pub packages: Vec<(PackageName, Fingerprint)>,
    /// The `composer.lock` content hash, or `None` for a project without one.
    pub composer_lock: Option<Fingerprint>,
    /// The builtin/extension catalog pin (ADR-0014).
    pub catalog_pin: String,
    /// The plugin set (ADR-0091's channel). Identity, not order.
    pub plugins: Vec<String>,
    /// The engine posture, or engine-off.
    pub engine: EnginePosture,
    /// Config inputs that change findings, as key/value pairs — the caller
    /// decides which config is finding-relevant; rendering knobs stay out.
    pub config: Vec<(String, String)>,
}

impl GenerationInputs {
    /// The generation fingerprint, domain `"steins-gen/generation"`.
    pub fn generation_id(&self) -> GenerationId {
        let mut h = FieldHasher::new("steins-gen/generation");
        h.field_u32("schema", SCHEMA_VERSION);
        h.field("analyzer", self.analyzer_version.as_bytes());
        let mut packages: Vec<_> = self.packages.iter().collect();
        packages.sort();
        for (name, source) in packages {
            h.field("package", name.as_str().as_bytes());
            h.field("source", source.as_bytes());
        }
        match &self.composer_lock {
            Some(lock) => h.field("composer.lock", lock.as_bytes()),
            None => h.field("composer.lock-absent", b""),
        };
        h.field("catalog", self.catalog_pin.as_bytes());
        let mut plugins: Vec<_> = self.plugins.iter().collect();
        plugins.sort();
        for plugin in plugins {
            h.field("plugin", plugin.as_bytes());
        }
        match &self.engine {
            EnginePosture::Off => {
                h.field("engine", b"off");
            }
            EnginePosture::On { php_version, int_size, extensions, fold_lane } => {
                h.field("engine", b"on");
                h.field("php-version", php_version.as_bytes());
                h.field("int-size", &[*int_size]);
                let mut extensions: Vec<_> = extensions.iter().collect();
                extensions.sort();
                for ext in extensions {
                    h.field("extension", ext.as_bytes());
                }
                h.field("fold-lane", fold_lane.as_bytes());
            }
        }
        let mut config: Vec<_> = self.config.iter().collect();
        config.sort();
        for (key, value) in config {
            h.field("config-key", key.as_bytes());
            h.field("config-value", value.as_bytes());
        }
        GenerationId(h.finish())
    }
}
