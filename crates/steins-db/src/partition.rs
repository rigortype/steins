//! Building the Composer-package partition (ADR-0092 §3, issue #486) from what
//! project discovery already found.
//!
//! The vocabulary — [`Package`], [`PackageKind`], [`PackageUniverse`] — lives
//! in `steins-gen` (it is generation identity); this module is the builder
//! that reads a real `composer.lock` and the path classifier that assigns an
//! analyzed file to its package. Layering follows `composer::discover`:
//! [`discover`] is the IO boundary (read the lock beside the outermost
//! governing manifest, once, before analysis), and [`PackagePartition`] is a
//! pure value — a replay with the same inputs classifies identically
//! (ADR-0048).
//!
//! The degenerate cases, pinned by tests:
//!
//! - **No `composer.lock`** (or no governing manifest at all): the partition
//!   is a single root package. Not a Composer-managed install, so there is no
//!   claim to divide the tree by.
//! - **Path-repository packages** (lock dist type `path`): classified
//!   [`PackageKind::PathRepository`] — first-party posture, always
//!   revalidated, never trusted from artifacts. The lock hash does not cover
//!   a live local tree's content.
//! - **Vendor files no lock entry claims** (`vendor/autoload.php`, drift, a
//!   manually dropped file): the synthetic [`PackageKind::VendorStray`]
//!   package, always revalidated. This is also where a monorepo subproject's
//!   own vendor tree lands in this slice — only the outermost root's lock is
//!   read, and unclaimed-but-vendor falling to always-revalidate is the safe
//!   direction.
//!
//! Edges: vendor→vendor from each lock entry's `require`; the root depends on
//! all of vendor by construction (autoloading is not a module system), which
//! [`PackageUniverse::invalidated_by`] encodes by putting the root in every
//! closure. Platform requirements (`php`, `ext-*`, `composer-plugin-api`) are
//! not packages; [`PackageUniverse::new`] drops edges to non-members.

use std::path::{Path, PathBuf};

use serde_json::Value;
use steins_gen::{Package, PackageKind, PackageName, PackageUniverse};

use crate::layout::{ProjectLayout, normalize};

/// Read the `composer.lock` governing `layout` — the one beside the outermost
/// governing manifest, the same root whose PHP target governs the run — and
/// build the partition. The IO happens here, once; the value returned is pure.
#[must_use]
pub fn discover(layout: &ProjectLayout) -> PackagePartition {
    let lock = layout
        .roots()
        .last()
        .and_then(|r| std::fs::read_to_string(r.dir().join("composer.lock")).ok());
    PackagePartition::from_lock(layout, lock.as_deref())
}

/// The built partition: the universe (packages + reverse-dependency edges)
/// and the path classifier that assigns every analyzed file to a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagePartition {
    /// The layout the classifier defers to for the vendor question — captured,
    /// like everything else, so the answer never depends on when it is asked.
    layout: ProjectLayout,
    universe: PackageUniverse,
    /// Each locked package's install root (`<vendor-dir>/<name>`), lexically
    /// normalized, → the package it belongs to.
    package_roots: Vec<(PathBuf, PackageName)>,
    /// The root package's name — the answer for every unclaimed non-vendor path.
    root: PackageName,
    /// The synthetic stray package — present exactly when a lock was read, and
    /// the answer for vendor paths no lock entry claims.
    stray: Option<PackageName>,
}

impl PackagePartition {
    /// Build the partition from an already-read lock. `None` — and a lock
    /// that does not parse as a JSON object, and a layout with no governing
    /// root to resolve `vendor/<name>` against — all mean the same honest
    /// thing: a single root package.
    #[must_use]
    pub fn from_lock(layout: &ProjectLayout, lock: Option<&str>) -> Self {
        let root = PackageName::new(Package::ROOT_NAME).expect("valid by construction");
        let single_root = |layout: &ProjectLayout| Self {
            layout: layout.clone(),
            universe: PackageUniverse::new(vec![Package::root()], &[]),
            package_roots: Vec::new(),
            root: root.clone(),
            stray: None,
        };
        let Some(outermost) = layout.roots().last() else { return single_root(layout) };
        let Some(entries) = lock.and_then(parse_lock_entries) else { return single_root(layout) };

        let vendor_root = outermost.vendor_roots().first().cloned();
        let mut packages = vec![Package::root(), Package::vendor_stray()];
        let mut requires: Vec<(PackageName, PackageName)> = Vec::new();
        let mut package_roots: Vec<(PathBuf, PackageName)> = Vec::new();
        for entry in entries {
            if let Some(dir) = &vendor_root {
                let mut install = dir.clone();
                for part in entry.name.as_str().split('/') {
                    install.push(part);
                }
                package_roots.push((normalize(&install), entry.name.clone()));
            }
            for dep in entry.requires {
                requires.push((entry.name.clone(), dep));
            }
            packages.push(Package { name: entry.name, kind: entry.kind });
        }
        Self {
            layout: layout.clone(),
            universe: PackageUniverse::new(packages, &requires),
            package_roots,
            root,
            stray: Some(PackageName::new(Package::VENDOR_STRAY_NAME).expect("valid by construction")),
        }
    }

    /// The universe this partition spans: its members and their edges.
    #[must_use]
    pub fn universe(&self) -> &PackageUniverse {
        &self.universe
    }

    /// The package owning `path` (absolute, or relative to the layout's
    /// captured working directory): the lock entry whose install root covers
    /// it, else the stray package for a vendor path, else the root. Total —
    /// every analyzed file belongs to exactly one member.
    #[must_use]
    pub fn package_of(&self, path: &str) -> &PackageName {
        let abs = self.absolutize(path);
        for (dir, name) in &self.package_roots {
            if abs.starts_with(dir) {
                return name;
            }
        }
        match &self.stray {
            Some(stray) if self.layout.is_vendor(path) => stray,
            _ => &self.root,
        }
    }

    /// Resolve `path` against the layout's captured working directory and
    /// normalize it lexically, mirroring the layout's own discipline.
    fn absolutize(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() { normalize(p) } else { normalize(&self.layout.cwd().join(p)) }
    }
}

/// One lock entry the partition cares about: its name, its kind, and its
/// `require` edges (platform requirements included — the universe drops edges
/// to non-members, so filtering here would be a second implementation of the
/// same rule).
struct LockEntry {
    name: PackageName,
    kind: PackageKind,
    requires: Vec<PackageName>,
}

/// Parse the lock's `packages` + `packages-dev` arrays. `None` when the text
/// is not a JSON object — an unreadable lock partitions like an absent one,
/// the same not-fatal discipline `composer::read_root` applies to a broken
/// manifest. Entries whose `name` is missing or unspellable are skipped.
fn parse_lock_entries(text: &str) -> Option<Vec<LockEntry>> {
    let json: Value = serde_json::from_str(text).ok()?;
    json.as_object()?;
    let mut out: Vec<LockEntry> = Vec::new();
    for section in ["packages", "packages-dev"] {
        let Some(list) = json.get(section).and_then(Value::as_array) else { continue };
        for entry in list {
            let Some(name) = entry.get("name").and_then(Value::as_str) else { continue };
            let Ok(name) = PackageName::new(name) else { continue };
            let dist_is_path = entry
                .get("dist")
                .and_then(|d| d.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|t| t == "path");
            let kind = if dist_is_path { PackageKind::PathRepository } else { PackageKind::Vendor };
            let requires = entry
                .get("require")
                .and_then(Value::as_object)
                .map(|reqs| reqs.keys().filter_map(|k| PackageName::new(k).ok()).collect())
                .unwrap_or_default();
            out.push(LockEntry { name, kind, requires });
        }
    }
    Some(out)
}
