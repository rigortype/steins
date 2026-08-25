//! The Composer-package partition builder (ADR-0092 §3, issue #486): the
//! degenerate cases pinned, the vendor→vendor edges, and the classifier.

use std::path::PathBuf;

use steins_db::partition::{PackagePartition, discover};
use steins_db::{GoverningRoot, ProjectLayout};
use steins_gen::{Package, PackageKind, PackageName};

fn name(s: &str) -> PackageName {
    PackageName::new(s).unwrap()
}

/// A one-root layout governing `/proj` with the standard `vendor/` dir — no
/// filesystem behind it; the partition core is pure.
fn layout() -> ProjectLayout {
    let dir = PathBuf::from("/proj");
    let root = GoverningRoot::new(
        dir.join("composer.json"),
        dir.clone(),
        vec![dir.join("vendor")],
        vec![dir.join("src")],
    );
    ProjectLayout::new(dir, vec![root])
}

const LOCK: &str = r#"{
    "packages": [
        {"name": "acme/lib", "require": {"acme/core": "^1.0", "php": ">=8.1", "ext-json": "*"},
         "dist": {"type": "zip"}},
        {"name": "acme/core", "require": {"php": ">=8.1"}}
    ],
    "packages-dev": [
        {"name": "dev/tool", "require": {"acme/lib": "^2.0"}},
        {"name": "local/pkg", "dist": {"type": "path"}}
    ]
}"#;

#[test]
fn no_composer_lock_yields_a_single_root_package() {
    let p = PackagePartition::from_lock(&layout(), None);
    let names: Vec<&str> = p.universe().packages().iter().map(|pk| pk.name.as_str()).collect();
    assert_eq!(names, [Package::ROOT_NAME]);
    // Everything — vendor paths included — belongs to the one root package.
    assert_eq!(p.package_of("/proj/vendor/acme/lib/src/A.php").as_str(), Package::ROOT_NAME);
    assert_eq!(p.package_of("src/App.php").as_str(), Package::ROOT_NAME);
}

#[test]
fn a_fallback_layout_partitions_like_a_lockless_project() {
    // No governing manifest at all: nothing to resolve `vendor/<name>` against,
    // so even a lock text yields the single-root partition.
    let p = PackagePartition::from_lock(&ProjectLayout::fallback(), Some(LOCK));
    assert_eq!(p.universe().packages().len(), 1);
}

#[test]
fn an_unreadable_lock_partitions_like_an_absent_one() {
    let p = PackagePartition::from_lock(&layout(), Some("{ this is not json"));
    assert_eq!(p.universe().packages().len(), 1);
    let p = PackagePartition::from_lock(&layout(), Some("[1, 2, 3]"));
    assert_eq!(p.universe().packages().len(), 1);
}

#[test]
fn locked_packages_partition_vendor_by_name() {
    let p = PackagePartition::from_lock(&layout(), Some(LOCK));
    // packages + packages-dev + the root + the stray package.
    assert_eq!(p.universe().packages().len(), 6);
    assert_eq!(p.universe().get(&name("acme/lib")).unwrap().kind, PackageKind::Vendor);
    assert_eq!(p.universe().get(&name("dev/tool")).unwrap().kind, PackageKind::Vendor);
    assert_eq!(p.package_of("/proj/vendor/acme/lib/src/A.php").as_str(), "acme/lib");
    assert_eq!(p.package_of("vendor/acme/core/Core.php").as_str(), "acme/core");
    assert_eq!(p.package_of("/proj/src/App.php").as_str(), Package::ROOT_NAME);
    assert_eq!(p.package_of("/proj/bin/console").as_str(), Package::ROOT_NAME);
}

#[test]
fn a_path_repository_package_is_first_party() {
    let p = PackagePartition::from_lock(&layout(), Some(LOCK));
    let local = p.universe().get(&name("local/pkg")).unwrap();
    assert_eq!(local.kind, PackageKind::PathRepository);
    assert!(local.kind.is_first_party(), "path repositories carry the first-party posture");
    assert!(!local.kind.trusted_from_artifacts(), "and are never trusted from artifacts");
    // Its files still classify to the package — the *kind* is what differs.
    assert_eq!(p.package_of("/proj/vendor/local/pkg/src/L.php").as_str(), "local/pkg");
}

#[test]
fn an_unclaimed_vendor_file_is_vendor_stray() {
    let p = PackagePartition::from_lock(&layout(), Some(LOCK));
    assert_eq!(p.package_of("/proj/vendor/other/pkg/A.php").as_str(), Package::VENDOR_STRAY_NAME);
    assert_eq!(p.package_of("/proj/vendor/autoload.php").as_str(), Package::VENDOR_STRAY_NAME);
    let stray = p.universe().get(&name(Package::VENDOR_STRAY_NAME)).unwrap();
    assert_eq!(stray.kind, PackageKind::VendorStray);
    assert!(!stray.kind.trusted_from_artifacts(), "stray vendor files always revalidate");
}

#[test]
fn reverse_closure_includes_transitive_dependents_and_the_root() {
    let p = PackagePartition::from_lock(&layout(), Some(LOCK));
    // dev/tool → acme/lib → acme/core, so a core change reaches all three plus
    // the root — and never the unrelated path repository.
    let set = p.universe().invalidated_by(&name("acme/core"));
    let expect = [name("acme/core"), name("acme/lib"), name("dev/tool"), name(Package::ROOT_NAME)];
    assert_eq!(set, expect.into());
}

#[test]
fn platform_requirements_are_not_packages() {
    let p = PackagePartition::from_lock(&layout(), Some(LOCK));
    assert!(p.universe().get(&name("php")).is_none());
    assert!(p.universe().get(&name("ext-json")).is_none());
    // And their edges are inert: nothing rebuilds by "php".
    assert_eq!(p.universe().invalidated_by(&name("php")).len(), 2, "itself plus the root only");
}

/// The IO boundary: `discover` reads the lock beside the outermost governing
/// manifest, and only there.
#[test]
fn discover_reads_the_lock_beside_the_outermost_manifest() {
    let dir = std::env::temp_dir().join(format!("steins-partition-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("composer.json"), r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#).unwrap();
    std::fs::write(dir.join("composer.lock"), LOCK).unwrap();
    let dir = dir.canonicalize().unwrap();

    let layout = steins_db::composer::discover(std::slice::from_ref(&dir), &dir);
    let p = discover(&layout);
    assert_eq!(p.universe().packages().len(), 6);
    let vendored = dir.join("vendor/acme/lib/src/A.php");
    assert_eq!(p.package_of(vendored.to_str().unwrap()).as_str(), "acme/lib");

    std::fs::remove_file(dir.join("composer.lock")).unwrap();
    let p = discover(&layout);
    assert_eq!(p.universe().packages().len(), 1, "no lock, single root package");
    let _ = std::fs::remove_dir_all(&dir);
}
