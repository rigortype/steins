//! The Composer-package partition of the analyzed universe (ADR-0092 §3):
//! identity vocabulary only.
//!
//! The universe partitions into Composer packages — vendor per
//! `composer.lock`, first-party as the root package (or packages, where the
//! ADR-0047 partition vocabulary applies) — and invalidation is a changed
//! package plus its reverse-dependency closure. This module owns the *shape*
//! of that statement: what a package is, which kinds exist, and how the
//! closure reads off the edges. It is plain data by design — the builder that
//! reads a real `composer.lock` lives with project discovery in `steins-db`,
//! and hands the result here so the identity layer (this crate) and the
//! payload owners (#486–#489) speak one vocabulary.
//!
//! Degenerate cases are pinned by the builder, not here, but their vocabulary
//! is: a project without a `composer.lock` is a single [`PackageKind::Root`]
//! package; a path-repository package (dist type `path`) is
//! [`PackageKind::PathRepository`] — first-party posture, never trusted from
//! artifacts; a file under vendor/ claimed by no lock entry belongs to the
//! synthetic always-revalidate [`PackageKind::VendorStray`] package.

use std::collections::{BTreeMap, BTreeSet};

use crate::names::PackageName;

/// What kind of partition member a package is — the axis that decides whether
/// a persisted artifact may ever stand in for re-analysis (ADR-0092 §2's
/// staleness-never-serves corollary applied per package).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PackageKind {
    /// The first-party package: everything no vendor package claims. Always
    /// rebuilt from source; it is what every edit session edits.
    Root,
    /// A locked vendor package (a `composer.lock` entry, installed under
    /// `vendor/<name>/`). The only kind whose artifacts may be trusted across
    /// generations — its sources change exactly when the lock entry does.
    Vendor,
    /// A path-repository package (lock dist type `path`): a lock entry whose
    /// files are a live local tree, so the lock hash says nothing about their
    /// content. First-party posture — always revalidated, never trusted from
    /// artifacts. For now the distinction is classification only (#486); the
    /// persistence slices act on it.
    PathRepository,
    /// The synthetic package that claims every file under vendor/ no lock
    /// entry claims (a manually dropped file, lock/install drift). Nothing
    /// vouches for such a file's provenance, so it is always revalidated.
    VendorStray,
}

impl PackageKind {
    /// Whether a persisted artifact of this package may serve a later
    /// generation whose fingerprint matches. Only [`PackageKind::Vendor`]:
    /// every other kind's sources can move without any identity input moving.
    #[must_use]
    pub fn trusted_from_artifacts(self) -> bool {
        matches!(self, PackageKind::Vendor)
    }

    /// Whether this kind carries the first-party posture (diagnostics on,
    /// transform candidacy) — the root and path-repository packages.
    #[must_use]
    pub fn is_first_party(self) -> bool {
        matches!(self, PackageKind::Root | PackageKind::PathRepository)
    }
}

/// One member of the partition: a name and its kind. Plain data — where its
/// files live and what they contain is the builder's business (`steins-db`),
/// not identity's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: PackageName,
    pub kind: PackageKind,
}

impl Package {
    /// The root package's name. A real Composer package name always contains
    /// exactly one `/`, so this (Composer's own internal default for an
    /// unnamed root) can never collide with a lock entry.
    pub const ROOT_NAME: &'static str = "__root__";

    /// The synthetic stray-vendor package's name. Slash-free, so it can never
    /// collide with a lock entry either.
    pub const VENDOR_STRAY_NAME: &'static str = "vendor-stray";

    /// The root package.
    #[must_use]
    pub fn root() -> Self {
        Self { name: PackageName::new(Self::ROOT_NAME).expect("valid by construction"), kind: PackageKind::Root }
    }

    /// The synthetic always-revalidate package for unclaimed vendor files.
    #[must_use]
    pub fn vendor_stray() -> Self {
        Self {
            name: PackageName::new(Self::VENDOR_STRAY_NAME).expect("valid by construction"),
            kind: PackageKind::VendorStray,
        }
    }
}

/// The partition plus its reverse-dependency edges: every package in the
/// universe, and for each, who depends on it. The invalidation boundary of
/// ADR-0092 §3 reads straight off this: a changed package rebuilds itself
/// plus its reverse closure, and the root — which depends on all of vendor,
/// because autoloading is not a module system — is in every closure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageUniverse {
    /// The members, sorted by name (deduplicated: the first kind offered for
    /// a name wins; the builder never offers two).
    packages: Vec<Package>,
    /// Reverse edges: dependency → its direct dependents. Built from the
    /// forward `require` edges at construction; only edges between known
    /// packages are kept.
    dependents: BTreeMap<PackageName, BTreeSet<PackageName>>,
}

impl PackageUniverse {
    /// Assemble the universe from its members and the forward `require`
    /// edges (`(dependent, dependency)` pairs, the direction a lock entry
    /// spells them). Edges naming a package outside `packages` — platform
    /// requirements like `php` or `ext-json` the builder chose not to filter
    /// — are dropped: they are not members, so nothing can rebuild by them.
    #[must_use]
    pub fn new(mut packages: Vec<Package>, requires: &[(PackageName, PackageName)]) -> Self {
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        packages.dedup_by(|a, b| a.name == b.name);
        let known: BTreeSet<&PackageName> = packages.iter().map(|p| &p.name).collect();
        let mut dependents: BTreeMap<PackageName, BTreeSet<PackageName>> = BTreeMap::new();
        for (dependent, dependency) in requires {
            if known.contains(dependent) && known.contains(dependency) {
                dependents.entry(dependency.clone()).or_default().insert(dependent.clone());
            }
        }
        Self { packages, dependents }
    }

    /// The members, sorted by name.
    #[must_use]
    pub fn packages(&self) -> &[Package] {
        &self.packages
    }

    /// The member named `name`, if any.
    #[must_use]
    pub fn get(&self, name: &PackageName) -> Option<&Package> {
        self.packages.binary_search_by(|p| p.name.cmp(name)).ok().map(|i| &self.packages[i])
    }

    /// The rebuild set of a change to `changed`: the package itself, its
    /// transitive dependents along the reverse edges, and every root package
    /// (the workspace depends on all of vendor). This is the edge-derived
    /// boundary only — packages whose kind is never
    /// [trusted from artifacts](PackageKind::trusted_from_artifacts) rebuild
    /// every generation regardless, which [`Self::always_revalidated`] lists.
    #[must_use]
    pub fn invalidated_by(&self, changed: &PackageName) -> BTreeSet<PackageName> {
        let mut out: BTreeSet<PackageName> = BTreeSet::new();
        let mut queue: Vec<&PackageName> = vec![changed];
        while let Some(name) = queue.pop() {
            if !out.insert(name.clone()) {
                continue;
            }
            if let Some(deps) = self.dependents.get(name) {
                queue.extend(deps.iter());
            }
        }
        out.extend(
            self.packages.iter().filter(|p| p.kind == PackageKind::Root).map(|p| p.name.clone()),
        );
        out
    }

    /// The packages that rebuild every generation no matter what changed:
    /// every kind whose artifacts are never trusted (root, path-repository,
    /// vendor-stray).
    pub fn always_revalidated(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter().filter(|p| !p.kind.trusted_from_artifacts())
    }
}
