//! The sealed source capture (ADR-0092 §2): sources are captured once behind
//! this boundary, and every later read or check answers against the seal. A
//! concurrent edit is detected at revalidation and rejects the whole
//! candidate — the torn-read class dies here, wholesale, never per-file.
//!
//! "Captured once" is meant literally since issue #521: the capture hands each
//! file's bytes back as it hashes them ([`SourceInventory::capture_keeping`]),
//! so an analysis reads its universe once rather than twice. See
//! [`SourceInventory`] for what that does to the seal's meaning — it makes the
//! bytes analyzed and the bytes fingerprinted the same *by construction*, and
//! leaves [`SourceInventory::read`] as the checked route for everything else.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use crate::fingerprint::{FieldHasher, Fingerprint};

/// What was sealed for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// Byte length at capture.
    pub size: u64,
    /// Modification time at capture. A revalidation accelerator only — an
    /// entry whose `(size, mtime)` is unmoved is trusted without re-hashing —
    /// and deliberately *not* part of the fingerprint: identity is content.
    pub mtime: SystemTime,
    /// blake3 of the file's bytes, domain `"steins-gen/file"`.
    pub content: Fingerprint,
}

/// Capture failed — the inventory was never sealed.
#[derive(Debug)]
pub enum SourceError {
    Io { path: PathBuf, error: io::Error },
    /// The path walks out of the project root (absolute outside it, or `..`).
    EscapesRoot(PathBuf),
    /// The relative path is not UTF-8, so it cannot be spelled in the seal.
    NonUtf8(PathBuf),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            SourceError::EscapesRoot(p) => write!(f, "{} escapes the project root", p.display()),
            SourceError::NonUtf8(p) => write!(f, "{} is not UTF-8", p.display()),
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SourceError::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// The world moved under the seal. Whoever sees this rejects the whole
/// candidate; there is no per-file salvage by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDrift {
    /// The offending file, relative to the root.
    pub path: String,
    pub kind: DriftKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    /// The file is gone.
    Missing,
    /// The content no longer hashes to the sealed fingerprint.
    Changed,
    /// The file exists but cannot be read or statted.
    Unreadable,
    /// The path was never captured — a read outside the boundary.
    Uncaptured,
}

impl fmt::Display for SourceDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            DriftKind::Missing => "vanished after capture",
            DriftKind::Changed => "changed after capture",
            DriftKind::Unreadable => "became unreadable after capture",
            DriftKind::Uncaptured => "was never captured",
        };
        write!(f, "{} {what}", self.path)
    }
}

impl std::error::Error for SourceDrift {}

/// One file as [`SourceInventory::capture_keeping`] saw it, at the instant it
/// was hashed: where it sat in the caller's iteration order, the key it sealed
/// under, what was sealed, and the very bytes [`SourceEntry::content`] is the
/// hash of.
///
/// The `index` is a position in the `files` iterator rather than a sealed key
/// because the caller's own indexing — universe slots, in the orchestrator's
/// case — is the thing it needs back, and two spellings of one path (`a.php`
/// and `./a.php`) collapse in the seal while staying two positions here.
pub struct Captured<'a> {
    /// The file's position in the `files` iterator, counted from 0.
    pub index: usize,
    /// The sealed key — the same normalization [`SourceInventory::capture`]
    /// applies.
    pub key: &'a str,
    /// The entry this file contributes to the seal.
    pub entry: &'a SourceEntry,
    /// The bytes that were hashed. Moving them out is the point: the caller
    /// keeps what it needs and drops the rest, so a whole capture never holds
    /// more than one file's contents of its own.
    pub bytes: Vec<u8>,
}

/// The sealed capture: `(relative path, size, mtime, content hash)` per file,
/// taken at open. Sealed at construction — there is no mutation API — and
/// checked by [`SourceInventory::revalidate`] immediately before a candidate
/// publishes.
///
/// **What makes "what was analyzed" and "what was fingerprinted" the same
/// bytes.** Two routes, and the difference between them is the strength of the
/// claim. [`SourceInventory::capture_keeping`] hands each file's contents to
/// the caller *at the moment it hashes them*, so the identity holds **by
/// construction** — there is one read and one hash, and the bytes the caller
/// analyzes are literally the bytes the fingerprint covers. Anything reading a
/// file later — after the capture has moved on, or a file the seal does not
/// hold — goes through [`SourceInventory::read`], which re-reads and
/// re-verifies against the seal, so the identity holds **by check**. A run
/// takes the first route for the universe it captured and the second for
/// whatever it did not; neither weakens the seal, and both are backstopped by
/// [`SourceInventory::revalidate`] before publication.
pub struct SourceInventory {
    root: PathBuf,
    entries: BTreeMap<String, SourceEntry>,
}

impl SourceInventory {
    /// Stat and hash every file, then seal. `files` may be absolute (under
    /// `root`) or relative to it; duplicates collapse. `root` itself is *not*
    /// part of the seal — the same tree captured from two checkouts
    /// fingerprints alike.
    ///
    /// "Duplicates" means duplicate *spellings*: a key is normalized, never
    /// resolved, so two paths reaching one real file through a directory
    /// symlink seal as two entries and the fingerprint covers those bytes
    /// twice. That is the seal behaving as designed, and it is why the `.php`
    /// walk (`steins_db::walk`, issue #524) hands over one spelling per real
    /// file — see `tests/inventory.rs`.
    ///
    /// The bytes read here are dropped; a caller that wants them should call
    /// [`SourceInventory::capture_keeping`] rather than read every file a
    /// second time.
    pub fn capture<I, P>(root: &Path, files: I) -> Result<Self, SourceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        Self::capture_keeping(root, files, |_| {})
    }

    /// [`SourceInventory::capture`], handing each file's bytes to `keep` at the
    /// instant they are hashed (issue #521).
    ///
    /// **Why this exists.** A caller that needs the sources it just sealed —
    /// which every analysis is — otherwise reads and hashes the universe
    /// twice: once here for the fingerprint, once through
    /// [`SourceInventory::read`] for the text. Handing the bytes back removes
    /// the second pass outright, and it *strengthens* the seal rather than
    /// trading it away: see the type-level docs on the two routes.
    ///
    /// **Memory.** `keep` is called once per file, with that one file's bytes,
    /// as the walk proceeds — never with the capture's accumulated contents.
    /// A caller that keeps everything pays the universe's source size, a
    /// caller that keeps nothing pays one file's, and the choice stays the
    /// caller's. `keep` is called for **every** item of `files`, duplicates
    /// included, so an index-keyed caller has no gaps to fill; the seal itself
    /// still collapses them.
    pub fn capture_keeping<I, P, F>(root: &Path, files: I, mut keep: F) -> Result<Self, SourceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
        F: FnMut(Captured<'_>),
    {
        let mut entries: BTreeMap<String, SourceEntry> = BTreeMap::new();
        for (index, file) in files.into_iter().enumerate() {
            let rel = relative_key(root, file.as_ref())?;
            let abs = root.join(&rel);
            let io_err = |error| SourceError::Io { path: abs.clone(), error };
            let meta = std::fs::metadata(&abs).map_err(&io_err)?;
            let bytes = std::fs::read(&abs).map_err(&io_err)?;
            let entry = SourceEntry {
                size: meta.len(),
                mtime: meta.modified().map_err(&io_err)?,
                content: Fingerprint::of_bytes("steins-gen/file", &bytes),
            };
            let entry = entries.entry(rel).insert_entry(entry);
            keep(Captured { index, key: entry.key(), entry: entry.get(), bytes });
        }
        Ok(Self { root: root.to_owned(), entries })
    }

    pub fn root(&self) -> &Path { &self.root }

    /// The sealed files, in relative-path order.
    pub fn files(&self) -> impl Iterator<Item = (&str, &SourceEntry)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize { self.entries.len() }

    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    pub fn entry(&self, path: &str) -> Option<&SourceEntry> { self.entries.get(path) }

    /// The sealed key a capture-time path resolves to, if the seal holds it —
    /// the same normalization [`SourceInventory::capture`] applied, so a caller
    /// that kept its own spelling (absolute, `./`-prefixed) can find the entry
    /// it captured without re-deriving the rule. `None` for a path that
    /// normalizes to nothing sealed (or escapes the root).
    pub fn key_for(&self, path: &Path) -> Option<String> {
        relative_key(&self.root, path).ok().filter(|key| self.entries.contains_key(key))
    }

    /// The package source fingerprint, domain `"steins-gen/source"`: each
    /// entry contributes its relative path, size, and content hash, in path
    /// order. mtime stays out (see [`SourceEntry::mtime`]); the root stays
    /// out, so absolutization cannot move the fingerprint.
    pub fn fingerprint(&self) -> Fingerprint {
        let mut h = FieldHasher::new("steins-gen/source");
        for (path, entry) in &self.entries {
            h.field("path", path.as_bytes());
            h.field_u64("size", entry.size);
            h.field("content", entry.content.as_bytes());
        }
        h.finish()
    }

    /// Read one sealed file *now*, verifying its bytes still hash to the seal;
    /// a mismatch is drift, not data.
    ///
    /// The route for everything the capture did not hand back
    /// ([`SourceInventory::capture_keeping`]): a file wanted after the capture
    /// has moved on, a caller that kept nothing, a path the seal does not hold
    /// at all ([`DriftKind::Uncaptured`]). The verification is exactly what
    /// makes the bytes as trustworthy as capture-time ones — it is the same
    /// identity, established by check rather than by construction — so it stays
    /// on this path whatever the other one does.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, SourceDrift> {
        let drift = |kind| SourceDrift { path: path.to_owned(), kind };
        let entry = self.entries.get(path).ok_or_else(|| drift(DriftKind::Uncaptured))?;
        let bytes = std::fs::read(self.root.join(path)).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => drift(DriftKind::Missing),
            _ => drift(DriftKind::Unreadable),
        })?;
        if Fingerprint::of_bytes("steins-gen/file", &bytes) != entry.content {
            return Err(drift(DriftKind::Changed));
        }
        Ok(bytes)
    }

    /// The pre-publish check: re-stat everything, re-hash exactly the entries
    /// whose `(size, mtime)` moved, and report the first mismatch. The
    /// unmoved-stat fast path is a deliberate economy with a known blind
    /// spot — an edit that restores both size and mtime passes — priced
    /// against re-hashing the universe on every publish.
    pub fn revalidate(&self) -> Result<(), SourceDrift> {
        for (path, sealed) in &self.entries {
            let drift = |kind| SourceDrift { path: path.clone(), kind };
            let abs = self.root.join(path);
            let meta = std::fs::metadata(&abs).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => drift(DriftKind::Missing),
                _ => drift(DriftKind::Unreadable),
            })?;
            let mtime = meta.modified().map_err(|_| drift(DriftKind::Unreadable))?;
            if meta.len() == sealed.size && mtime == sealed.mtime {
                continue;
            }
            let bytes = std::fs::read(&abs).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => drift(DriftKind::Missing),
                _ => drift(DriftKind::Unreadable),
            })?;
            if Fingerprint::of_bytes("steins-gen/file", &bytes) != sealed.content {
                return Err(drift(DriftKind::Changed));
            }
        }
        Ok(())
    }
}

/// Normalize to the sealed spelling: relative to `root`, `/`-separated,
/// no `.`/`..`, UTF-8.
fn relative_key(root: &Path, path: &Path) -> Result<String, SourceError> {
    let rel: &Path = if path.is_absolute() {
        path.strip_prefix(root).map_err(|_| SourceError::EscapesRoot(path.to_owned()))?
    } else {
        path
    };
    let mut key = String::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| SourceError::NonUtf8(path.to_owned()))?;
                if !key.is_empty() {
                    key.push('/');
                }
                key.push_str(part);
            }
            Component::CurDir => {}
            _ => return Err(SourceError::EscapesRoot(path.to_owned())),
        }
    }
    if key.is_empty() {
        return Err(SourceError::EscapesRoot(path.to_owned()));
    }
    Ok(key)
}
