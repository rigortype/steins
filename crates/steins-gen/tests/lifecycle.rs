//! Candidate-then-publish: atomic publication, torn-write recovery, and the
//! seal rejecting a candidate whose sources moved.

use std::path::{Path, PathBuf};

use steins_gen::{
    ArtifactBuilder, DecodeBudget, EnginePosture, GenerationId, GenerationInputs, Miss,
    PackageName, PublishError, SCHEMA_VERSION, SectionName, SourceInventory, Store,
};

/// A throwaway project directory under the OS temp dir, cleaned on drop.
struct TempProject {
    dir: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-gen-lifecycle-{tag}-{}-{}",
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

    fn gen_root(&self) -> PathBuf {
        self.dir.join(".steins").join("gen")
    }

    fn gen_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.gen_root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn pkg(name: &str) -> PackageName {
    PackageName::new(name).unwrap()
}

fn sec(name: &str) -> SectionName {
    SectionName::new(name).unwrap()
}

fn sample_sources(project: &TempProject) -> SourceInventory {
    if !project.dir.join("src/a.php").exists() {
        project.write("src/a.php", "<?php function a() {}\n");
        project.write("src/b.php", "<?php function b() {}\n");
    }
    SourceInventory::capture(&project.dir, ["src/a.php", "src/b.php"]).unwrap()
}

/// What `.steins/gen/` holds after a clean publish of `id`, sorted.
fn published_entries(id: &GenerationId) -> Vec<String> {
    let mut expected = vec!["CURRENT".to_owned(), id.to_hex()];
    expected.sort();
    expected
}

fn id_for(sources: &SourceInventory, tag: &str) -> GenerationId {
    GenerationInputs {
        analyzer_version: tag.to_owned(),
        packages: vec![(pkg("__first_party__"), sources.fingerprint())],
        composer_lock: None,
        catalog_pin: "pin".to_owned(),
        plugins: vec![],
        engine: EnginePosture::Off,
        config: vec![],
    }
    .generation_id()
}

fn artifact(payload: &[u8]) -> ArtifactBuilder {
    let mut builder = ArtifactBuilder::new();
    builder.section(sec("symbols"), payload.to_vec()).unwrap();
    builder
}

/// Publish one first-party generation into `project`, returning its id.
fn publish_one(project: &TempProject, tag: &str) -> GenerationId {
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(project);
    let id = id_for(&sources, tag);
    let mut candidate = store.begin(id, vec![sources]).unwrap();
    candidate.write_artifact(&pkg("__first_party__"), &artifact(b"the payload")).unwrap();
    let published = candidate.publish().unwrap();
    assert_eq!(*published.id(), id);
    id
}

#[test]
fn publish_then_reopen_round_trips() {
    let project = TempProject::new("round-trip");
    let id = publish_one(&project, "v1");
    // A fresh open — a second process, in effect — sees the same generation.
    let store = Store::open(&project.dir).unwrap();
    let current = store.current().unwrap().expect("a generation was published");
    assert_eq!(*current.id(), id);
    let packages: Vec<&PackageName> = current.packages().collect();
    assert_eq!(packages, [&pkg("__first_party__")]);
    assert!(current.has_package(&pkg("__first_party__")));
    let mut reader = current.artifact(&pkg("__first_party__")).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the payload");
}

#[test]
fn empty_store_has_no_current() {
    let project = TempProject::new("empty");
    let store = Store::open(&project.dir).unwrap();
    assert!(store.current().unwrap().is_none());
}

#[test]
fn current_swaps_between_generations_and_both_stay_readable() {
    let project = TempProject::new("swap");
    let first = publish_one(&project, "v1");
    project.write("src/a.php", "<?php function a() { return 2; }\n");
    let second = publish_one(&project, "v1");
    assert_ne!(first, second);
    let store = Store::open(&project.dir).unwrap();
    assert_eq!(*store.current().unwrap().unwrap().id(), second);
    assert_eq!(*store.generation(&first).unwrap().id(), first);
}

/// The torn-write rule: a candidate that never published — its in-progress
/// marker still down — is swept wholesale at the next open, and what was
/// published stays untouched.
#[test]
fn torn_candidate_is_swept_at_startup() {
    let project = TempProject::new("torn");
    let published = publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(&project);
    let id = id_for(&sources, "v2");
    let mut candidate = store.begin(id, vec![sources]).unwrap();
    candidate.write_artifact(&pkg("__first_party__"), &artifact(b"half-written")).unwrap();
    // The crash: the process dies mid-build. Forgetting the candidate keeps
    // its Drop from tidying up, which is exactly a torn state on disk.
    std::mem::forget(candidate);
    let torn: Vec<String> =
        project.gen_entries().iter().filter(|n| n.starts_with(".candidate-")).cloned().collect();
    assert_eq!(torn.len(), 1, "the torn candidate is on disk before recovery");
    assert!(
        project.gen_root().join(&torn[0]).join("in-progress").exists(),
        "the marker is down while unpublished"
    );

    let store = Store::open(&project.dir).unwrap();
    assert_eq!(project.gen_entries(), published_entries(&published));
    assert_eq!(*store.current().unwrap().unwrap().id(), published);
}

/// Seal-then-modify: an edit between capture and publish rejects the whole
/// candidate — no artifact of it survives, and CURRENT never moves.
#[test]
fn seal_then_modify_rejects_the_candidate() {
    let project = TempProject::new("drift");
    let published = publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(&project);
    let id = id_for(&sources, "v2");
    let mut candidate = store.begin(id, vec![sources]).unwrap();
    candidate.write_artifact(&pkg("__first_party__"), &artifact(b"soon stale")).unwrap();
    project.write("src/a.php", "<?php function a() { return 3; }\n");
    match candidate.publish() {
        Err(PublishError::Drift(drift)) => assert_eq!(drift.path, "src/a.php"),
        other => panic!("expected Drift, got {:?}", other.map(|_| ())),
    }
    assert_eq!(project.gen_entries(), published_entries(&published));
    let store = Store::open(&project.dir).unwrap();
    assert_eq!(*store.current().unwrap().unwrap().id(), published);
}

#[test]
fn abort_and_drop_both_remove_the_candidate() {
    let project = TempProject::new("abort");
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(&project);
    let id = id_for(&sources, "v1");
    let candidate = store.begin(id, vec![sources]).unwrap();
    candidate.abort();
    assert_eq!(project.gen_entries(), Vec::<String>::new());

    let sources = sample_sources(&project);
    let candidate = store.begin(id_for(&sources, "v2"), vec![sources]).unwrap();
    drop(candidate);
    assert_eq!(project.gen_entries(), Vec::<String>::new());
}

#[test]
fn a_package_writes_once_per_candidate() {
    let project = TempProject::new("dup-package");
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(&project);
    let id = id_for(&sources, "v1");
    let mut candidate = store.begin(id, vec![sources]).unwrap();
    candidate.write_artifact(&pkg("__first_party__"), &artifact(b"once")).unwrap();
    let err = candidate.write_artifact(&pkg("__first_party__"), &artifact(b"twice")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

/// Publishing the same identity twice is idempotent: the published copy
/// wins, the redundant candidate evaporates.
#[test]
fn republishing_the_same_id_is_idempotent() {
    let project = TempProject::new("idempotent");
    let id = publish_one(&project, "v1");
    let again = publish_one(&project, "v1");
    assert_eq!(id, again);
    let store = Store::open(&project.dir).unwrap();
    assert_eq!(project.gen_entries(), published_entries(&id));
    assert_eq!(*store.current().unwrap().unwrap().id(), id);
}

fn scribble(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

#[test]
fn corrupt_current_is_a_miss() {
    let project = TempProject::new("bad-current");
    publish_one(&project, "v1");
    scribble(&project.gen_root().join("CURRENT"), "not a generation id\n");
    let store = Store::open(&project.dir).unwrap();
    assert!(matches!(store.current(), Err(Miss::Corrupt(_))));
}

#[test]
fn current_naming_a_missing_generation_is_a_miss() {
    let project = TempProject::new("dangling-current");
    let id = publish_one(&project, "v1");
    std::fs::remove_dir_all(project.gen_root().join(id.to_hex())).unwrap();
    let store = Store::open(&project.dir).unwrap();
    assert!(matches!(store.current(), Err(Miss::AbsentGeneration)));
}

#[test]
fn corrupt_manifest_is_a_miss() {
    let project = TempProject::new("bad-manifest");
    let id = publish_one(&project, "v1");
    let manifest = project.gen_root().join(id.to_hex()).join("manifest");
    scribble(
        &manifest,
        &format!("steins-gen manifest\nschema {SCHEMA_VERSION}\ngeneration deadbeef\n"),
    );
    let store = Store::open(&project.dir).unwrap();
    assert!(matches!(store.current(), Err(Miss::Corrupt(_))));
}

#[test]
fn manifest_schema_drift_is_a_miss() {
    let project = TempProject::new("manifest-schema");
    let id = publish_one(&project, "v1");
    let manifest = project.gen_root().join(id.to_hex()).join("manifest");
    let foreign = SCHEMA_VERSION + 1;
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace(&format!("schema {SCHEMA_VERSION}"), &format!("schema {foreign}"));
    scribble(&manifest, &text);
    let store = Store::open(&project.dir).unwrap();
    assert!(matches!(store.current(), Err(Miss::SchemaMismatch { found }) if found == foreign));
}

/// A marker inside a published generation cannot happen through this crate;
/// if it is ever seen, the generation does not serve.
#[test]
fn a_marker_inside_a_published_generation_is_a_miss() {
    let project = TempProject::new("marker");
    let id = publish_one(&project, "v1");
    scribble(&project.gen_root().join(id.to_hex()).join("in-progress"), "");
    let store = Store::open(&project.dir).unwrap();
    assert!(matches!(store.current(), Err(Miss::Corrupt(_))));
}

#[test]
fn an_unlisted_package_is_a_miss() {
    let project = TempProject::new("absent-package");
    publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let current = store.current().unwrap().unwrap();
    assert!(matches!(
        current.artifact(&pkg("vendor/never")),
        Err(Miss::AbsentPackage(_))
    ));
}

/// The store's budget flows down to artifact reads.
#[test]
fn the_stores_budget_bounds_artifact_reads() {
    let project = TempProject::new("budget");
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(&project);
    let id = id_for(&sources, "v1");
    let mut candidate = store.begin(id, vec![sources]).unwrap();
    candidate.write_artifact(&pkg("__first_party__"), &artifact(&[0xEE; 4096])).unwrap();
    candidate.publish().unwrap();
    // 512 bytes covers the manifest but not the 4 KiB artifact.
    let store =
        Store::open_with_budget(&project.dir, DecodeBudget { max_file_bytes: 512 }).unwrap();
    let current = store.current().unwrap().unwrap();
    assert!(matches!(
        current.artifact(&pkg("__first_party__")),
        Err(Miss::OverBudget { ceiling: 512, .. })
    ));
}
