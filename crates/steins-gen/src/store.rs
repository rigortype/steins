//! The candidate-then-publish store (ADR-0092 §2). On disk, under
//! `<project>/.steins/gen/`:
//!
//! ```text
//! gen/CURRENT                    hex generation id + newline; atomic-rename swap
//! gen/<generation-hex>/          one published generation, immutable
//!   manifest                     schema, id, package roster (this crate's format)
//!   <mangled-package>.pkg        one artifact container per package
//! gen/.candidate-<nonce>/        a build in progress
//!   in-progress                  the torn-write tripwire
//!   …                            artifacts as the build writes them
//! ```
//!
//! Publication is: revalidate the sealed sources, write the manifest, drop
//! the marker, fsync, rename the candidate into place, swap `CURRENT`. A
//! candidate that never got that far is swept wholesale at the next
//! [`Store::open`] — recovery is deliberately unclever, because the §2
//! invariant (a miss changes cost, never meaning) makes throwing a candidate
//! away always correct.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::container::{ArtifactBuilder, ArtifactReader, DecodeBudget, Miss, SCHEMA_VERSION};
use crate::identity::GenerationId;
use crate::inventory::{SourceDrift, SourceInventory};
use crate::names::PackageName;

const CURRENT: &str = "CURRENT";
const CURRENT_TMP: &str = "CURRENT.tmp";
const MANIFEST: &str = "manifest";
const MARKER: &str = "in-progress";
const CANDIDATE_PREFIX: &str = ".candidate-";

/// One project's generation store, rooted at `<project>/.steins/`. Opening
/// it creates the layout and sweeps orphaned candidates; there is no other
/// startup recovery, by design.
pub struct Store {
    gen_root: PathBuf,
    budget: DecodeBudget,
}

impl Store {
    /// Open (creating if needed) with the default [`DecodeBudget`].
    pub fn open(project_root: &Path) -> io::Result<Self> {
        Self::open_with_budget(project_root, DecodeBudget::default())
    }

    /// Open with an explicit decode ceiling, applied to every artifact and
    /// manifest read through this store.
    pub fn open_with_budget(project_root: &Path, budget: DecodeBudget) -> io::Result<Self> {
        let gen_root = project_root.join(".steins").join("gen");
        fs::create_dir_all(&gen_root)?;
        sweep(&gen_root)?;
        Ok(Self { gen_root, budget })
    }

    /// The published generation `CURRENT` names, if any. `Ok(None)` means no
    /// generation was ever published; `Err(Miss)` means one was but cannot
    /// serve — either way the caller rebuilds, the reason is for reporting.
    pub fn current(&self) -> Result<Option<Generation>, Miss> {
        let raw = match fs::read_to_string(self.gen_root.join(CURRENT)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Miss::Io(e)),
        };
        let id = GenerationId::from_hex(raw.trim_end_matches('\n'))
            .ok_or(Miss::Corrupt("CURRENT does not name a generation"))?;
        self.generation(&id).map(Some)
    }

    /// Open one published generation by id.
    pub fn generation(&self, id: &GenerationId) -> Result<Generation, Miss> {
        let dir = self.gen_root.join(id.to_hex());
        if !dir.is_dir() {
            return Err(Miss::AbsentGeneration);
        }
        Generation::open(dir, *id, self.budget)
    }

    /// Start a candidate for `id`. `sources` are the sealed inventories the
    /// build reads through — the same ones whose fingerprints went into `id`
    /// (this crate cannot enforce that linkage; the builder owes it) — and
    /// they are revalidated wholesale when the candidate publishes.
    pub fn begin(
        &self,
        id: GenerationId,
        sources: Vec<SourceInventory>,
    ) -> io::Result<Candidate<'_>> {
        let dir = self.gen_root.join(format!("{CANDIDATE_PREFIX}{}", nonce()));
        fs::create_dir(&dir)?;
        write_file_synced(&dir.join(MARKER), id.to_hex().as_bytes())?;
        Ok(Candidate {
            store: self,
            dir,
            id,
            sources,
            packages: BTreeSet::new(),
            defused: false,
        })
    }
}

/// Publication failed. `Drift` means the world moved under the seal and the
/// candidate was rejected (and removed) wholesale; `Io` means the filesystem
/// failed us mid-flight — the candidate is likewise gone, never half-kept.
#[derive(Debug)]
pub enum PublishError {
    Drift(SourceDrift),
    Io(io::Error),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PublishError::Drift(d) => write!(f, "sources drifted under the seal: {d}"),
            PublishError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PublishError::Drift(d) => Some(d),
            PublishError::Io(e) => Some(e),
        }
    }
}

impl From<io::Error> for PublishError {
    fn from(e: io::Error) -> Self { PublishError::Io(e) }
}

/// A build in progress: a private directory the store will either publish
/// atomically or throw away — dropped, aborted, drifted, or swept at the
/// next startup, it vanishes wholesale.
pub struct Candidate<'s> {
    store: &'s Store,
    dir: PathBuf,
    id: GenerationId,
    sources: Vec<SourceInventory>,
    packages: BTreeSet<PackageName>,
    defused: bool,
}

impl Candidate<'_> {
    pub fn id(&self) -> &GenerationId { &self.id }

    /// Write one package's artifact container into the candidate. Each
    /// package writes once; a second write is `AlreadyExists`.
    pub fn write_artifact(
        &mut self,
        package: &PackageName,
        artifact: &ArtifactBuilder,
    ) -> io::Result<()> {
        if self.packages.contains(package) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("package {package} already written into this candidate"),
            ));
        }
        artifact.write_to(&self.dir.join(artifact_file_name(package)))?;
        self.packages.insert(package.clone());
        Ok(())
    }

    /// Publish: revalidate every sealed inventory, then manifest, marker off,
    /// fsync, rename into place, swap `CURRENT`. Any drift rejects — and
    /// removes — the whole candidate.
    pub fn publish(mut self) -> Result<Generation, PublishError> {
        for inventory in &self.sources {
            if let Err(drift) = inventory.revalidate() {
                self.defused = true;
                let _ = fs::remove_dir_all(&self.dir);
                return Err(PublishError::Drift(drift));
            }
        }
        let manifest = manifest_text(&self.id, &self.packages);
        write_file_synced(&self.dir.join(MANIFEST), manifest.as_bytes())?;
        fs::remove_file(self.dir.join(MARKER))?;
        fsync_dir(&self.dir)?;
        let final_dir = self.store.gen_root.join(self.id.to_hex());
        self.defused = true;
        if final_dir.is_dir() {
            // Same fingerprint, same meaning: the published copy wins and
            // the redundant build is discarded.
            fs::remove_dir_all(&self.dir)?;
        } else {
            fs::rename(&self.dir, &final_dir)?;
        }
        fsync_dir(&self.store.gen_root)?;
        let tmp = self.store.gen_root.join(CURRENT_TMP);
        write_file_synced(&tmp, format!("{}\n", self.id.to_hex()).as_bytes())?;
        fs::rename(&tmp, self.store.gen_root.join(CURRENT))?;
        fsync_dir(&self.store.gen_root)?;
        Ok(Generation {
            dir: final_dir,
            id: self.id,
            packages: std::mem::take(&mut self.packages),
            budget: self.store.budget,
        })
    }

    /// Throw the candidate away explicitly. (Dropping it does the same.)
    pub fn abort(mut self) {
        self.defused = true;
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Drop for Candidate<'_> {
    fn drop(&mut self) {
        if !self.defused {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// One published, immutable generation: the package roster from its
/// manifest, and a reader per artifact.
pub struct Generation {
    dir: PathBuf,
    id: GenerationId,
    packages: BTreeSet<PackageName>,
    budget: DecodeBudget,
}

impl Generation {
    fn open(dir: PathBuf, id: GenerationId, budget: DecodeBudget) -> Result<Self, Miss> {
        if dir.join(MARKER).exists() {
            return Err(Miss::Corrupt("in-progress marker inside a published generation"));
        }
        let manifest_path = dir.join(MANIFEST);
        let len = fs::metadata(&manifest_path)?.len();
        if len > budget.max_file_bytes {
            return Err(Miss::OverBudget { need: len, ceiling: budget.max_file_bytes });
        }
        let text = match fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                return Err(Miss::Corrupt("manifest is not UTF-8"));
            }
            Err(e) => return Err(e.into()),
        };
        let packages = parse_manifest(&text, &id)?;
        Ok(Self { dir, id, packages, budget })
    }

    pub fn id(&self) -> &GenerationId { &self.id }

    /// The package roster, in name order.
    pub fn packages(&self) -> impl Iterator<Item = &PackageName> {
        self.packages.iter()
    }

    pub fn has_package(&self, package: &PackageName) -> bool {
        self.packages.contains(package)
    }

    /// Open one package's artifact for section reads.
    pub fn artifact(&self, package: &PackageName) -> Result<ArtifactReader, Miss> {
        if !self.packages.contains(package) {
            return Err(Miss::AbsentPackage(package.clone()));
        }
        ArtifactReader::open(&self.dir.join(artifact_file_name(package)), self.budget)
    }
}

/// The manifest, this crate's own line format (a cache detail like the rest;
/// its version *is* [`SCHEMA_VERSION`]):
///
/// ```text
/// steins-gen manifest
/// schema 1
/// generation <hex>
/// package <name>            (repeated, sorted)
/// ```
///
/// Artifact file names are derived from package names, not stored, so the
/// manifest cannot point a package at someone else's bytes.
fn manifest_text(id: &GenerationId, packages: &BTreeSet<PackageName>) -> String {
    let mut text =
        format!("steins-gen manifest\nschema {SCHEMA_VERSION}\ngeneration {}\n", id.to_hex());
    for package in packages {
        text.push_str("package ");
        text.push_str(package.as_str());
        text.push('\n');
    }
    text
}

/// Strict inverse of [`manifest_text`]; any deviation is a miss.
fn parse_manifest(text: &str, id: &GenerationId) -> Result<BTreeSet<PackageName>, Miss> {
    let corrupt = |what| Err(Miss::Corrupt(what));
    let mut lines = text.lines();
    if lines.next() != Some("steins-gen manifest") {
        return corrupt("manifest header line");
    }
    let Some(schema) = lines.next().and_then(|l| l.strip_prefix("schema ")) else {
        return corrupt("manifest schema line");
    };
    match schema.parse::<u32>() {
        Ok(found) if found == SCHEMA_VERSION => {}
        Ok(found) => return Err(Miss::SchemaMismatch { found }),
        Err(_) => return corrupt("manifest schema line"),
    }
    let Some(hex) = lines.next().and_then(|l| l.strip_prefix("generation ")) else {
        return corrupt("manifest generation line");
    };
    if GenerationId::from_hex(hex) != Some(*id) {
        return corrupt("manifest names a different generation");
    }
    let mut packages = BTreeSet::new();
    for line in lines {
        let Some(name) = line.strip_prefix("package ") else {
            return corrupt("unrecognized manifest line");
        };
        let Ok(package) = PackageName::new(name) else {
            return corrupt("invalid package name in manifest");
        };
        if !packages.insert(package) {
            return corrupt("duplicate package in manifest");
        }
    }
    Ok(packages)
}

/// The artifact file for a package: percent-encode every byte outside
/// `[a-z0-9._-]` (uppercase included, so case-insensitive filesystems cannot
/// collide two packages), then `.pkg`. Injective, so distinct packages never
/// share a file, and the suffix keeps the namespace clear of `manifest` and
/// the marker.
fn artifact_file_name(package: &PackageName) -> String {
    let mut name = String::with_capacity(package.as_str().len() + 4);
    for b in package.as_str().bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => name.push(b as char),
            _ => name.push_str(&format!("%{b:02X}")),
        }
    }
    name.push_str(".pkg");
    name
}

/// Startup recovery, whole: anything candidate-shaped goes, and a torn
/// `CURRENT` swap loses its temp file. Published generations are never
/// touched.
fn sweep(gen_root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(gen_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let is_candidate = name.to_str().is_some_and(|n| n.starts_with(CANDIDATE_PREFIX));
        if !(is_candidate || name.to_str() == Some(CURRENT_TMP)) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn write_file_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Directory fsync makes the rename durable on unix; elsewhere it is a
/// no-op, which weakens durability, not atomicity.
fn fsync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Unique enough for a private scratch name: pid, wall nanos, process-local
/// counter. Collisions fail loudly at `create_dir`, not silently.
fn nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{:x}-{nanos:x}-{:x}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_file_names_are_mangled_and_suffixed() {
        let n = |s| artifact_file_name(&PackageName::new(s).unwrap());
        assert_eq!(n("vendor/name"), "vendor%2Fname.pkg");
        assert_eq!(n("plain-name_1.0"), "plain-name_1.0.pkg");
        assert_eq!(n("Upper"), "%55pper.pkg");
        assert_eq!(n("odd%byte"), "odd%25byte.pkg");
        assert_eq!(n("manifest"), "manifest.pkg");
    }
}
