//! The sealed source capture: fingerprint stability, drift detection, and
//! the read-through boundary.

use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use steins_gen::{DriftKind, SourceInventory};

/// A throwaway directory under the OS temp dir, cleaned on drop.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-gen-inventory-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let p = self.dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn set_mtime(&self, name: &str, to: SystemTime) {
        File::options()
            .write(true)
            .open(self.dir.join(name))
            .unwrap()
            .set_modified(to)
            .unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sample_tree(tag: &str) -> TempDir {
    let t = TempDir::new(tag);
    t.write("src/a.php", "<?php function a() {}\n");
    t.write("src/sub/b.php", "<?php function b() {}\n");
    t.write("composer.json", "{}\n");
    t
}

fn capture(t: &TempDir) -> SourceInventory {
    SourceInventory::capture(&t.dir, ["src/a.php", "src/sub/b.php", "composer.json"]).unwrap()
}

/// The same tree captured from two roots fingerprints alike: paths are
/// sealed relative, so absolutization cannot move identity.
#[test]
fn fingerprint_survives_path_absolutization() {
    let t1 = sample_tree("abs-1");
    let t2 = sample_tree("abs-2");
    assert_ne!(t1.dir, t2.dir);
    let relative = capture(&t1);
    let absolute = SourceInventory::capture(
        &t2.dir,
        [t2.dir.join("src/a.php"), t2.dir.join("src/sub/b.php"), t2.dir.join("composer.json")],
    )
    .unwrap();
    assert_eq!(relative.fingerprint(), absolute.fingerprint());
}

#[test]
fn fingerprint_ignores_capture_order() {
    let t = sample_tree("order");
    let a = capture(&t);
    let b =
        SourceInventory::capture(&t.dir, ["composer.json", "src/sub/b.php", "src/a.php"]).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
}

/// mtime is a revalidation accelerator, not identity: touching a file moves
/// neither the fingerprint nor the seal's verdict.
#[test]
fn fingerprint_and_revalidation_ignore_a_bare_touch() {
    let t = sample_tree("touch");
    let sealed = capture(&t);
    let before = sealed.fingerprint();
    t.set_mtime("src/a.php", SystemTime::now() + Duration::from_secs(7));
    assert_eq!(capture(&t).fingerprint(), before);
    sealed.revalidate().unwrap();
}

#[test]
fn content_moves_the_fingerprint() {
    let t = sample_tree("content");
    let before = capture(&t).fingerprint();
    t.write("src/a.php", "<?php function a() { return 1; }\n");
    assert_ne!(capture(&t).fingerprint(), before);
}

#[test]
fn revalidate_catches_an_edit() {
    let t = sample_tree("edit");
    let sealed = capture(&t);
    t.write("src/a.php", "<?php function a() { return 1; }\n");
    let drift = sealed.revalidate().unwrap_err();
    assert_eq!(drift.path, "src/a.php");
    assert_eq!(drift.kind, DriftKind::Changed);
}

/// A size-preserving edit still moves mtime, so the re-hash path catches it.
#[test]
fn revalidate_catches_a_size_preserving_edit() {
    let t = sample_tree("same-size");
    let sealed = capture(&t);
    t.write("src/a.php", "<?php function z() {}\n");
    t.set_mtime("src/a.php", SystemTime::now() + Duration::from_secs(7));
    let drift = sealed.revalidate().unwrap_err();
    assert_eq!(drift.kind, DriftKind::Changed);
}

#[test]
fn revalidate_catches_a_deletion() {
    let t = sample_tree("deleted");
    let sealed = capture(&t);
    std::fs::remove_file(t.dir.join("src/sub/b.php")).unwrap();
    let drift = sealed.revalidate().unwrap_err();
    assert_eq!(drift.path, "src/sub/b.php");
    assert_eq!(drift.kind, DriftKind::Missing);
}

/// The documented blind spot, pinned so a change to it is a decision: an
/// edit that restores both size and mtime passes the unmoved-stat fast path.
#[test]
fn an_edit_that_restores_size_and_mtime_is_trusted() {
    let t = sample_tree("blind-spot");
    let sealed = capture(&t);
    let mtime = sealed.entry("src/a.php").unwrap().mtime;
    t.write("src/a.php", "<?php function z() {}\n");
    t.set_mtime("src/a.php", mtime);
    sealed.revalidate().unwrap();
}

/// Reads go through the seal and verify content on the way out.
#[test]
fn read_through_verifies_the_seal() {
    let t = sample_tree("read");
    let sealed = capture(&t);
    assert_eq!(sealed.read("src/a.php").unwrap(), b"<?php function a() {}\n");
    t.write("src/a.php", "<?php mutated();\n");
    assert_eq!(sealed.read("src/a.php").unwrap_err().kind, DriftKind::Changed);
    assert_eq!(sealed.read("src/nope.php").unwrap_err().kind, DriftKind::Uncaptured);
}

#[test]
fn capture_rejects_escapes_and_absences() {
    let t = sample_tree("reject");
    assert!(SourceInventory::capture(&t.dir, ["../outside.php"]).is_err());
    assert!(SourceInventory::capture(&t.dir, ["/etc/hosts"]).is_err());
    assert!(SourceInventory::capture(&t.dir, ["src/never.php"]).is_err());
}

#[test]
fn duplicates_collapse_and_entries_are_sorted() {
    let t = sample_tree("dedup");
    let sealed =
        SourceInventory::capture(&t.dir, ["src/a.php", "./src/a.php", "composer.json"]).unwrap();
    assert_eq!(sealed.len(), 2);
    let paths: Vec<&str> = sealed.files().map(|(p, _)| p).collect();
    assert_eq!(paths, ["composer.json", "src/a.php"]);
}
