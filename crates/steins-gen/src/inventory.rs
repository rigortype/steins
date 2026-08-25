//! The sealed source capture (ADR-0092 §2): sources are captured once behind
//! this boundary, and every later read or check answers against the seal. A
//! concurrent edit is detected at revalidation and rejects the whole
//! candidate — the torn-read class dies here, wholesale, never per-file.

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

/// The sealed capture: `(relative path, size, mtime, content hash)` per file,
/// taken at open. Sealed at construction — there is no mutation API — and
/// checked by [`SourceInventory::revalidate`] immediately before a candidate
/// publishes. Later slices read source *through* the seal
/// ([`SourceInventory::read`]), which is what makes "what was analyzed" and
/// "what was fingerprinted" the same bytes.
pub struct SourceInventory {
    root: PathBuf,
    entries: BTreeMap<String, SourceEntry>,
}

impl SourceInventory {
    /// Stat and hash every file, then seal. `files` may be absolute (under
    /// `root`) or relative to it; duplicates collapse. `root` itself is *not*
    /// part of the seal — the same tree captured from two checkouts
    /// fingerprints alike.
    pub fn capture<I, P>(root: &Path, files: I) -> Result<Self, SourceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut entries = BTreeMap::new();
        for file in files {
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
            entries.insert(rel, entry);
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

    /// Read one sealed file, verifying its bytes still hash to the seal. The
    /// read path later slices go through; a mismatch is drift, not data.
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
