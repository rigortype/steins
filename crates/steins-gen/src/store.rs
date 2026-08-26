//! The candidate-then-publish store (ADR-0092 §2). On disk, under
//! `<project>/.steins/`:
//!
//! ```text
//! .gitignore                     `*` — written once, at creation (issue #525)
//! gen/.lock                      the store's advisory flock, held only around
//!                                bookkeeping critical sections (issue #491)
//! gen/CURRENT                    hex generation id + newline; atomic-rename swap
//! gen/<generation-hex>/          the published generation, immutable — and,
//!                                bar a crash, the only one (issue #529)
//!   manifest                     schema, id, package roster (this crate's format)
//!   <mangled-package>.pkg        one artifact container per package
//!   summaries                    the generation's run-dependent sidecar, if any
//! gen/.candidate-<nonce>/        a build in progress
//!   in-progress                  the torn-write tripwire; its flock, held by
//!                                the building process, is the liveness signal
//!   …                            artifacts as the build writes them
//! ```
//!
//! Publication is: revalidate the sealed sources, write the manifest, drop
//! the marker, rename the candidate into place, swap `CURRENT`, sweep what was
//! superseded. A candidate that never got that far is swept wholesale at the
//! next [`Store::open`], once the process that owned it is dead — recovery is
//! deliberately unclever, because the §2 invariant (a miss changes cost,
//! never meaning) makes throwing an ownerless candidate away always correct.
//!
//! # The bound: `CURRENT` and nothing else
//!
//! A publish used to rename its candidate into place and leave what it
//! replaced behind, so the store grew by a full artifact set per distinct
//! source state — measured at ~5 MB per invocation over a 1.26 MB source tree,
//! without limit, inside the user's own project (issue #529). It is now
//! bounded at **one** generation: a publish that fully succeeded sweeps every
//! other one, and so does [`Store::open`], which is what collects the
//! generations a crash or a schema bump left unreachable.
//!
//! **Why one is enough, concurrency included.** [`ArtifactReader`] holds an
//! open `File` for the artifact's whole life, and on POSIX unlinking a name
//! does not disturb an open descriptor — the inode outlives the directory
//! entry, until the last handle closes. So a reader that has already opened an
//! artifact cannot be harmed by a sweep at all. A reader that has *not* opened
//! one yet gets `ENOENT`, which is a [`Miss`], which degrades that package to
//! rebuild-from-source. That is the §2 invariant working rather than being
//! bent: a run whose generation is swept mid-flight does more work and
//! produces the same findings.
//!
//! Two rules keep a deletion inside someone's project honest, and the
//! lifecycle tests pin both:
//!
//! * **After the publish, never before.** The sweep runs once the rename *and*
//!   the `CURRENT` swap have both succeeded. A publish that failed anywhere —
//!   drift under the seal, a filesystem error mid-flight — removes nothing,
//!   and the generation it did not manage to replace stays authoritative.
//! * **Only names this crate writes.** A directory is swept only when its name
//!   is 64 lowercase hex digits, [`GenerationId::to_hex`]'s exact shape (the
//!   test is [`GenerationId::from_hex`] itself, so the two cannot drift).
//!   Anything else under `gen/` is left exactly where it is: a file, a
//!   directory a person put there, a name from a layout this build does not
//!   know. Being conservative about names we do not recognize costs a few
//!   stale bytes; being liberal costs someone their data.
//!
//! Candidate directories are a separate question, because unlike a published
//! generation a candidate has an owner, and a name cannot say whether that
//! owner is alive (issue #491). Liveness is advisory `flock(2)`, at two
//! grains. Store-wide, `gen/.lock` is held — blocking, and only around the
//! bookkeeping moments: [`Store::begin`], the startup sweep, and a publish's
//! `CURRENT` swap — never for the store's lifetime, so a resident process and
//! a concurrent CLI run serialize their bookkeeping and nothing else. Per
//! candidate, [`Store::begin`] locks the `in-progress` marker and the
//! [`Candidate`] holds that lock for the build's whole life; the kernel
//! releases it when the process exits, however it exits. The sweep try-locks
//! each marker: acquired means the owner is dead and the candidate goes;
//! refused means a live build and it stays; no marker at all, or any other
//! error, and the sweep declines — a crash torn mid-`begin` and a publish
//! between marker-off and rename both look like that, and stale bytes cost
//! less than someone's build. The same store lock closes the `CURRENT.tmp`
//! race: a sweep can no longer delete the temp file between a publish's
//! write and its rename.
//!
//! # The artifacts and the sidecar, and why they are apart
//!
//! An artifact holds what is a function of the **sources**; the `summaries`
//! sidecar holds what is a function of the **run**. The split is issue #519's,
//! and its whole purpose is that the first kind is byte-identical between
//! generations whenever its package did not move — so it can be *shared* with
//! the next generation ([`Candidate::adopt_artifact`]) instead of rewritten.
//! While the run-dependent bytes lived in the same container, no artifact was
//! ever equal to its predecessor and nothing could ever be shared: a one-file
//! edit rewrote every package in the universe, vendor included.
//!
//! The sidecar is one file for the whole generation rather than one per
//! package, for the same reason the fold table is (ADR-0092 §4's amendment):
//! what it holds is scoped by the run, not by a package, and one file is one
//! write and one barrier however many packages the universe has.
//!
//! # Durability: one barrier, and it is not in this file
//!
//! A generation store is a cache, and ADR-0092 §2's standing invariant — a
//! miss may change cost, never meaning — prices its two crash properties very
//! differently. **Atomicity** is worth everything: a torn generation must
//! never be read as a valid one. **Durability** is worth nothing: a generation
//! lost to a power cut costs one cold rebuild, which is the same thing a
//! schema bump costs, by design.
//!
//! `rename(2)` gives the atomicity, and it needs no `fsync` to do it. So the
//! chain here issues no barrier of its own, and each dropped one is dropped
//! against a named failure mode:
//!
//! | if this never reaches the disk | what a later `Store::open` sees | verdict |
//! |---|---|---|
//! | the in-progress marker | a marker-less `.candidate-*` directory, which every sweep declines (issue #491) | stale bytes, never meaning |
//! | the manifest | not the exact four-line shape, or the wrong id | [`Miss::Corrupt`] |
//! | the marker's *removal* | a marker inside a published generation | [`Miss::Corrupt`] |
//! | the candidate's rename | `CURRENT` names nothing | [`Miss::AbsentGeneration`] |
//! | `CURRENT` itself | not 64 hex digits, or a generation that is not there | [`Miss::Corrupt`] / [`Miss::AbsentGeneration`] |
//! | the sweep of what it superseded | a generation nothing names | collected at the next [`Store::open`] |
//!
//! Every row degrades to rebuild-from-source. That is not luck: every file
//! this module writes is strictly parsed and names its own generation, so a
//! torn one cannot pass for a whole one.
//!
//! The exception is **section bytes**, which are the one thing in a generation
//! whose loss could be undetectable — a payload of stale blocks might decode
//! to a value rather than to a miss. Those keep their barrier, in
//! [`ArtifactBuilder::write_to`], and it is taken before the rename that makes
//! them reachable. An artifact that was *shared* rather than written needs
//! none: its bytes were fsynced by the generation that wrote them.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::container::{ArtifactBuilder, ArtifactReader, DecodeBudget, Miss, SCHEMA_VERSION};
use crate::identity::GenerationId;
use crate::inventory::{SourceDrift, SourceInventory};
use crate::names::PackageName;
use crate::share::{self, ShareKind};

const CURRENT: &str = "CURRENT";
const CURRENT_TMP: &str = "CURRENT.tmp";
const MANIFEST: &str = "manifest";
const MARKER: &str = "in-progress";
const CANDIDATE_PREFIX: &str = ".candidate-";
/// The store-level lock file (issue #491). Never swept: not candidate-shaped,
/// not [`CURRENT_TMP`], not 64 hex digits — the unit tests pin all three.
const LOCK: &str = ".lock";
/// The generation-level run-dependent sidecar. No suffix, so it cannot collide
/// with any `<mangled-package>.pkg`.
const SUMMARIES: &str = "summaries";

/// One project's generation store, rooted at `<project>/.steins/`. Opening it
/// creates the layout, sweeps candidates whose owning process is dead (issue
/// #491), and drops every published generation `CURRENT` does not name; there
/// is no other startup recovery, by design.
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
        let steins_root = project_root.join(".steins");
        let gen_root = steins_root.join("gen");
        fs::create_dir_all(&gen_root)?;
        write_ignore_file(&steins_root);
        sweep(&gen_root)?;
        Ok(Self { gen_root, budget })
    }

    /// Look at an existing store without creating anything — the read-only
    /// door `steins doctor` reports through. `None` when no store is there,
    /// which is a posture ("nothing cached yet"), not an error.
    ///
    /// Deliberately not [`Self::open`]: that call creates the layout and
    /// sweeps, and a posture report that materialized — or tidied — the thing
    /// it is reporting on would answer its own question.
    #[must_use]
    pub fn open_existing(project_root: &Path) -> Option<Self> {
        Self::open_existing_with_budget(project_root, DecodeBudget::default())
    }

    /// [`Self::open_existing`] with an explicit decode ceiling.
    #[must_use]
    pub fn open_existing_with_budget(project_root: &Path, budget: DecodeBudget) -> Option<Self> {
        let gen_root = project_root.join(".steins").join("gen");
        gen_root.is_dir().then_some(Self { gen_root, budget })
    }

    /// Where this store's generations live (`<project>/.steins/gen`).
    #[must_use]
    pub fn gen_root(&self) -> &Path { &self.gen_root }

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
        // Under the store lock, so a concurrent open's sweep can never see
        // this directory before its marker and marker lock are both down
        // (issue #491) — which is what lets the sweep treat a marker-less
        // candidate as a torn crash rather than a build mid-`begin`.
        let _lock = StoreLock::hold(&self.gen_root)?;
        let dir = self.gen_root.join(format!("{CANDIDATE_PREFIX}{}", nonce()));
        fs::create_dir(&dir)?;
        let marker_lock = match arm_marker(&dir, &id) {
            Ok(marker) => marker,
            Err(e) => {
                // A marker-less candidate would otherwise sit as stale bytes
                // forever: no sweep will touch a directory it cannot prove
                // abandoned.
                let _ = fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        Ok(Candidate {
            store: self,
            dir,
            id,
            sources,
            packages: BTreeSet::new(),
            defused: false,
            _marker_lock: marker_lock,
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
    /// The liveness signal (issue #491): an exclusive flock on the `in-progress`
    /// marker, held for as long as the build is. The kernel releases it when
    /// this process exits, however it exits, so a sweep whose try-lock succeeds
    /// has proof the owner is dead — a pid stamp could prove no such thing.
    _marker_lock: File,
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
        self.claim(package)?;
        artifact.write_to(&self.dir.join(artifact_file_name(package)))?;
        self.packages.insert(package.clone());
        Ok(())
    }

    /// Take `from`'s artifact for `package` instead of writing one — the
    /// republish path for a package whose bytes did not move (issue #519).
    ///
    /// The caller owes the equality: this crate cannot tell whether the
    /// artifact `from` holds is the one this build would have produced, only
    /// that sharing it costs a directory entry rather than a rewrite and a
    /// barrier. What this crate does own is that sharing is *safe* — see
    /// [`crate::share`] for why an alias here can never be written through,
    /// and why removing either generation leaves the other readable.
    ///
    /// On any failure the candidate is unchanged and the package is still
    /// unclaimed, so the caller can fall back to [`Self::write_artifact`].
    pub fn adopt_artifact(
        &mut self,
        package: &PackageName,
        from: &Generation,
    ) -> io::Result<ShareKind> {
        self.claim(package)?;
        if !from.has_package(package) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("generation {} has no package {package}", from.id.to_hex()),
            ));
        }
        let name = artifact_file_name(package);
        let kind = share::share(&from.dir.join(&name), &self.dir.join(&name))?;
        self.packages.insert(package.clone());
        Ok(kind)
    }

    /// Write the generation's run-dependent sidecar. Unlike an artifact it is
    /// never shared — it is a function of the whole run, so every publish
    /// writes its own — and unlike an artifact there is exactly one of them,
    /// whatever the package count. Writes once; a second call is
    /// `AlreadyExists`.
    ///
    /// Independent of the package roster in both directions: a generation may
    /// have artifacts and no sidecar, or a sidecar and no artifacts.
    pub fn write_summaries(&mut self, summaries: &ArtifactBuilder) -> io::Result<()> {
        summaries.write_to(&self.dir.join(SUMMARIES))
    }

    /// Reserve `package`'s slot in this candidate, or say it is taken.
    fn claim(&self, package: &PackageName) -> io::Result<()> {
        if self.packages.contains(package) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("package {package} already written into this candidate"),
            ));
        }
        Ok(())
    }

    /// Publish: revalidate every sealed inventory, then manifest, marker off,
    /// rename into place, swap `CURRENT`, sweep what was superseded. Any drift
    /// rejects — and removes — the whole candidate.
    ///
    /// The sweep is last on purpose (issue #529): until both halves of the
    /// swap are down, the generation this one replaces is still the one a
    /// reader would find, and a publish that failed must leave it exactly
    /// where it was.
    ///
    /// No durability barrier is taken here; the module docs table every step
    /// against what a crash before it would leave, and every one of them is a
    /// [`Miss`] and a rebuild. The artifacts' own bytes were fsynced as they
    /// were written, which is the one place a lost write could be
    /// undetectable.
    pub fn publish(mut self) -> Result<Generation, PublishError> {
        for inventory in &self.sources {
            if let Err(drift) = inventory.revalidate() {
                self.defused = true;
                let _ = fs::remove_dir_all(&self.dir);
                return Err(PublishError::Drift(drift));
            }
        }
        let manifest = manifest_text(&self.id, &self.packages);
        write_file(&self.dir.join(MANIFEST), manifest.as_bytes())?;
        fs::remove_file(self.dir.join(MARKER))?;
        let final_dir = self.store.gen_root.join(self.id.to_hex());
        self.defused = true;
        if final_dir.is_dir() {
            // Same fingerprint, same meaning: the published copy wins and
            // the redundant build is discarded.
            fs::remove_dir_all(&self.dir)?;
        } else {
            fs::rename(&self.dir, &final_dir)?;
        }
        // The swap and its sweep are one critical section under the store
        // lock (issue #491): without it, a concurrent open's sweep could
        // delete `CURRENT.tmp` between the write and the rename and fail
        // this publish on a name that was ours.
        let _lock = StoreLock::hold(&self.store.gen_root)?;
        let tmp = self.store.gen_root.join(CURRENT_TMP);
        write_file(&tmp, format!("{}\n", self.id.to_hex()).as_bytes())?;
        fs::rename(&tmp, self.store.gen_root.join(CURRENT))?;
        // Both halves of the swap are down: this generation is what a reader
        // finds, and every other one is superseded. An artifact this candidate
        // adopted is a second directory entry on the same inode, so dropping
        // the first one drops a name and never the bytes.
        sweep_superseded(&self.store.gen_root, Some(&self.id.to_hex()));
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

    /// Open the generation's run-dependent sidecar. A generation without one
    /// is an ordinary [`Miss`] whose caller falls back to computing what it
    /// would have read.
    pub fn summaries(&self) -> Result<ArtifactReader, Miss> {
        ArtifactReader::open(&self.dir.join(SUMMARIES), self.budget)
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
/// share a file, and the suffix keeps the namespace clear of `manifest`, the
/// marker, and the suffixless [`SUMMARIES`] sidecar.
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

/// Startup recovery, under the store lock: candidates whose owner is provably
/// dead go, live builds stay (issue #491), a torn `CURRENT` swap loses its
/// temp file, and every published generation `CURRENT` does not name goes too
/// — which is how a generation orphaned by a crash between the rename and the
/// swap, or by a schema bump, is ever collected at all (issue #529).
///
/// The temp-file removal is unconditional and stays correct under the lock:
/// a publish writes and renames `CURRENT.tmp` inside its own critical
/// section, so a temp file a sweep can see is always a torn swap's leavings.
fn sweep(gen_root: &Path) -> io::Result<()> {
    let _lock = StoreLock::hold(gen_root)?;
    for entry in fs::read_dir(gen_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == CURRENT_TMP {
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
            continue;
        }
        // Only directories this crate's `begin` could have made: a
        // candidate-named *file* cannot hold a marker, so it is somebody
        // else's and stays.
        if !name.starts_with(CANDIDATE_PREFIX) || !entry.file_type()?.is_dir() {
            continue;
        }
        if candidate_owner_is_dead(&entry.path()) {
            fs::remove_dir_all(entry.path())?;
        }
    }
    match reachable(gen_root) {
        Reachable::Generation(keep) => sweep_superseded(gen_root, Some(&keep)),
        Reachable::Nothing => sweep_superseded(gen_root, None),
        Reachable::Unknown => {}
    }
    Ok(())
}

/// The liveness verdict on one candidate, under the store lock (issue #491).
/// Only a *positive* proof of death — the marker is there and its flock was
/// free for the taking — permits a sweep. A refused lock is a live build in
/// another process; a missing marker is a crash torn mid-`begin` or a publish
/// between marker-off and rename (`begin` itself runs under the store lock,
/// so it can never be caught half-done); any other failure is no evidence at
/// all. Every one of those declines, because stale bytes cost less than
/// someone's build.
fn candidate_owner_is_dead(dir: &Path) -> bool {
    let Ok(marker) = File::open(dir.join(MARKER)) else { return false };
    // The verdict outlives the lock (released as `marker` closes) because the
    // caller still holds the store lock: nothing can take a dead candidate
    // back over before the removal that follows.
    matches!(try_lock_exclusive(&marker), Ok(true))
}

/// What `CURRENT` names, as far as a sweep is concerned.
enum Reachable {
    /// `CURRENT` names this generation; every other one is superseded.
    Generation(String),
    /// Nothing is reachable: no `CURRENT` at all, or one that names no
    /// generation. Either way the next run rebuilds, so whatever is on disk is
    /// bytes no reader will ever find.
    Nothing,
    /// `CURRENT` could not be read — a permission, an I/O failure. Whether
    /// anything is superseded is then unknown, and an unreadable pointer is no
    /// evidence that a generation is stale, so the sweep declines.
    Unknown,
}

/// Read `CURRENT` for the sweep. Deliberately its own reader rather than
/// [`Store::current`]: the sweep wants the *name* `CURRENT` holds and nothing
/// else, and must not care whether the generation behind it opens.
fn reachable(gen_root: &Path) -> Reachable {
    match fs::read_to_string(gen_root.join(CURRENT)) {
        Ok(raw) => match GenerationId::from_hex(raw.trim_end_matches('\n')) {
            Some(id) => Reachable::Generation(id.to_hex()),
            None => Reachable::Nothing,
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Reachable::Nothing,
        Err(_) => Reachable::Unknown,
    }
}

/// Remove every published generation but `keep` (`None` sweeps all of them) —
/// the bound of issue #529, applied after a publish and again at every open.
///
/// **Only names this crate writes.** An entry is swept when it is a *directory*
/// whose name [`GenerationId::from_hex`] accepts — 64 lowercase hex digits, the
/// exact shape [`GenerationId::to_hex`] writes, so the recognizer and the
/// writer cannot drift apart. Everything else under `gen/` is somebody else's
/// and is left where it is. We are deleting inside a user's project directory:
/// being conservative about a name we do not recognize costs a few stale
/// bytes, and is the difference between a bug and an incident.
///
/// Best-effort throughout, and deliberately so. This is tidying, not
/// publication: a removal that fails leaves bytes behind and the next open
/// tries again, whereas failing the caller would turn a disk-space problem
/// into a lost generation.
fn sweep_superseded(gen_root: &Path, keep: Option<&str>) {
    let Ok(entries) = fs::read_dir(gen_root) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if GenerationId::from_hex(name).is_none() || Some(name) == keep {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Drop a `.gitignore` holding `*` into `.steins/` the first time the store is
/// created, the way Cargo does for `target/` (issue #525).
///
/// A cache the tool writes without being asked must not become a commit the
/// user did not ask for: a published generation is multi-megabyte, specific to
/// one analyzer version and one machine's engine posture, and worthless in
/// anyone else's checkout. Best-effort by construction — an existing file is
/// left exactly as the user left it, a failed write changes nothing, and no
/// generation depends on the file being there.
fn write_ignore_file(steins_root: &Path) {
    let path = steins_root.join(".gitignore");
    if path.exists() {
        return;
    }
    let _ = write_file(&path, b"*\n");
}

/// The store's own metadata files — the manifest, `CURRENT.tmp`. No barrier:
/// each is strictly parsed by whoever reads it, so a torn one is a [`Miss`]
/// and a rebuild rather than a lie (see the module docs). Truncating rather
/// than exclusive, because `CURRENT.tmp` is a fixed name a second publish in
/// the same process legitimately reuses.
fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    File::create(path)?.write_all(bytes)
}

/// The store-level critical-section lock: `gen/.lock`, flocked exclusively and
/// blocking, released when this value drops (issue #491). Held only around
/// bookkeeping — [`Store::begin`], the startup [`sweep`], a publish's swap —
/// and never across a build, because a resident server must serialize its
/// bookkeeping against a concurrent CLI run, not its analysis.
struct StoreLock {
    _file: File,
}

impl StoreLock {
    fn hold(gen_root: &Path) -> io::Result<Self> {
        // The file's contents are nothing; only its inode's flock matters.
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(gen_root.join(LOCK))?;
        lock_exclusive(&file)?;
        Ok(Self { _file: file })
    }
}

/// Create a candidate's `in-progress` marker and take the exclusive flock the
/// [`Candidate`] will hold for the build's lifetime (issue #491). Non-blocking
/// on principle: the file is seconds old and ours, so contention here is a
/// logic error, and a logic error must fail loudly rather than hang.
fn arm_marker(dir: &Path, id: &GenerationId) -> io::Result<File> {
    let mut marker = File::create(dir.join(MARKER))?;
    marker.write_all(id.to_hex().as_bytes())?;
    if !try_lock_exclusive(&marker)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "a freshly created candidate marker is already locked",
        ));
    }
    Ok(marker)
}

/// `flock(2)`, restarted over `EINTR`. Advisory locks are the right tool here
/// because every party is this crate: pid stamps were rejected as unsound
/// (pid recycling and pid namespaces both fabricate liveness), while a flock
/// is released by the kernel at process death and never survives its owner.
#[cfg(unix)]
fn flock(file: &File, operation: libc::c_int) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: the descriptor is open for the whole call; `flock` takes it
        // and an operation flag, and touches no memory of ours.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> io::Result<()> {
    flock(file, libc::LOCK_EX)
}

/// `Ok(true)`: the lock is ours. `Ok(false)`: someone live holds it.
#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    match flock(file, libc::LOCK_EX | libc::LOCK_NB) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(e),
    }
}

/// Off unix there is no `flock(2)` and no release target either (the same
/// posture as [`crate::share`]'s clone syscalls), so locking degrades to the
/// pre-#491 behavior: every marked candidate looks abandoned and is swept.
#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> io::Result<bool> {
    Ok(true)
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

    /// What the sweep will and will not take by name (issue #529). The
    /// recognizer *is* [`GenerationId::from_hex`], so this pins the shape the
    /// sweep inherits rather than a second spelling of it: uppercase, short,
    /// long and non-hex names are all somebody else's directory.
    #[test]
    fn only_a_generations_own_name_is_recognized_for_sweeping() {
        let recognized = |n: &str| GenerationId::from_hex(n).is_some();
        let hex = "a1".repeat(32);
        assert_eq!(hex.len(), 64);
        assert!(recognized(&hex));
        assert!(!recognized(&hex.to_uppercase()), "the store never writes uppercase");
        assert!(!recognized(&hex[..63]), "63 digits is not a generation");
        assert!(!recognized(&format!("{hex}0")), "65 digits is not one either");
        assert!(!recognized(&"g".repeat(64)), "not hex at all");
        assert!(!recognized("notes"), "a directory somebody put there");
        assert!(!recognized(CURRENT));
    }

    /// The store's own lock file is not candidate-shaped, not the swap's temp
    /// file, and not generation-shaped, so no sweep path can ever take it
    /// (issue #491).
    #[test]
    fn the_lock_file_matches_no_sweep_pattern() {
        assert!(!LOCK.starts_with(CANDIDATE_PREFIX));
        assert_ne!(LOCK, CURRENT_TMP);
        assert!(GenerationId::from_hex(LOCK).is_none());
    }

    /// No package name can mangle onto one of the generation's own files: the
    /// suffix is the separation, and the sidecar has none.
    #[test]
    fn no_package_can_take_a_generation_file_name() {
        for reserved in [MANIFEST, MARKER, SUMMARIES, CURRENT, LOCK] {
            let Ok(package) = PackageName::new(reserved) else { continue };
            assert_ne!(artifact_file_name(&package), reserved);
        }
    }
}
