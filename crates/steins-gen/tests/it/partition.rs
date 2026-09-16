//! The partition vocabulary (ADR-0092 §3, issue #486): kinds, the reverse
//! closure over plain data, and the reserved names.

use std::collections::BTreeSet;

use steins_gen::{Package, PackageKind, PackageName, PackageUniverse};

fn name(s: &str) -> PackageName {
    PackageName::new(s).unwrap()
}

fn vendor(s: &str) -> Package {
    Package { name: name(s), kind: PackageKind::Vendor }
}

/// a/app → a/lib → a/core: a change to the deepest dependency invalidates its
/// transitive dependents, the package itself, and the root — never a sibling.
#[test]
fn invalidation_closure_walks_reverse_edges_transitively() {
    let u = PackageUniverse::new(
        vec![Package::root(), vendor("a/app"), vendor("a/lib"), vendor("a/core"), vendor("b/other")],
        &[(name("a/app"), name("a/lib")), (name("a/lib"), name("a/core"))],
    );
    let set = u.invalidated_by(&name("a/core"));
    let expect: BTreeSet<PackageName> =
        [name("a/core"), name("a/lib"), name("a/app"), name(Package::ROOT_NAME)].into();
    assert_eq!(set, expect, "transitive dependents plus the root, not b/other");
}

/// The workspace depends on all of vendor: the root is in every closure, even
/// a leaf's.
#[test]
fn the_root_is_in_every_closure() {
    let u = PackageUniverse::new(vec![Package::root(), vendor("a/leaf")], &[]);
    assert!(u.invalidated_by(&name("a/leaf")).contains(&name(Package::ROOT_NAME)));
    // And a root change invalidates the root alone — vendor does not depend on it.
    let root_change = u.invalidated_by(&name(Package::ROOT_NAME));
    assert_eq!(root_change, [name(Package::ROOT_NAME)].into());
}

/// Only locked vendor packages may ever serve from artifacts; root,
/// path-repository, and stray packages revalidate every generation.
#[test]
fn only_vendor_packages_are_trusted_from_artifacts() {
    assert!(PackageKind::Vendor.trusted_from_artifacts());
    assert!(!PackageKind::Root.trusted_from_artifacts());
    assert!(!PackageKind::PathRepository.trusted_from_artifacts());
    assert!(!PackageKind::VendorStray.trusted_from_artifacts());

    let u = PackageUniverse::new(
        vec![
            Package::root(),
            Package::vendor_stray(),
            vendor("a/lib"),
            Package { name: name("local/pkg"), kind: PackageKind::PathRepository },
        ],
        &[],
    );
    let names: Vec<&str> = u.always_revalidated().map(|p| p.name.as_str()).collect();
    assert_eq!(names, [Package::ROOT_NAME, "local/pkg", Package::VENDOR_STRAY_NAME]);
}

/// The path-repository kind carries the first-party posture; stray vendor
/// files do not become first-party merely by being untrusted.
#[test]
fn first_party_posture_covers_root_and_path_repositories() {
    assert!(PackageKind::Root.is_first_party());
    assert!(PackageKind::PathRepository.is_first_party());
    assert!(!PackageKind::Vendor.is_first_party());
    assert!(!PackageKind::VendorStray.is_first_party());
}

/// Edges naming a non-member (platform requirements the builder left in) are
/// inert, and duplicate members collapse.
#[test]
fn unknown_edge_endpoints_and_duplicate_members_are_dropped() {
    let u = PackageUniverse::new(
        vec![Package::root(), vendor("a/lib"), vendor("a/lib")],
        &[(name("a/lib"), name("php")), (name("ghost/pkg"), name("a/lib"))],
    );
    assert_eq!(u.packages().len(), 2);
    let set = u.invalidated_by(&name("a/lib"));
    assert_eq!(set, [name("a/lib"), name(Package::ROOT_NAME)].into());
}

/// The reserved names can never collide with a lock entry: a real Composer
/// package name contains a `/`, and both reserved names are slash-free.
#[test]
fn reserved_names_are_slash_free_and_valid() {
    assert!(!Package::ROOT_NAME.contains('/'));
    assert!(!Package::VENDOR_STRAY_NAME.contains('/'));
    assert_eq!(Package::root().kind, PackageKind::Root);
    assert_eq!(Package::vendor_stray().kind, PackageKind::VendorStray);
    assert_ne!(Package::root().name, Package::vendor_stray().name);
}
