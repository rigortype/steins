//! Candidate-then-publish: atomic publication, torn-write recovery, the seal
//! rejecting a candidate whose sources moved, the artifact sharing of issue
//! #519 — what an adopted artifact is, and what happens to the other
//! generation when one of them goes — and the one-generation bound of issue
//! #529, whose whole risk is that it deletes directories inside a user's
//! project.

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

/// `CURRENT` swaps to the new generation, and the one it replaced is gone —
/// the bound of issue #529. Before it, a publish left what it superseded on
/// disk forever: one full artifact set per distinct source state, measured at
/// ~5 MB an invocation inside the user's project.
#[test]
fn a_publish_swaps_current_and_sweeps_what_it_replaced() {
    let project = TempProject::new("swap");
    let first = publish_one(&project, "v1");
    project.write("src/a.php", "<?php function a() { return 2; }\n");
    let second = publish_one(&project, "v1");
    assert_ne!(first, second);
    let store = Store::open(&project.dir).unwrap();
    assert_eq!(*store.current().unwrap().unwrap().id(), second);
    assert_eq!(project.gen_entries(), published_entries(&second));
    assert!(
        matches!(store.generation(&first), Err(Miss::AbsentGeneration)),
        "the superseded generation is gone, and asking for it is an ordinary miss"
    );
}

/// The growth table of issue #529, in miniature: edit, publish, repeat. The
/// store stays at one generation however many times the source moves.
#[test]
fn repeated_edits_leave_the_store_at_one_generation() {
    let project = TempProject::new("bounded");
    let mut last = publish_one(&project, "v1");
    for edit in 0..5 {
        project.write("src/a.php", &format!("<?php function a() {{ return {edit}; }}\n"));
        let id = publish_one(&project, "v1");
        assert_ne!(id, last, "each edit is a distinct generation");
        assert_eq!(
            project.gen_entries(),
            published_entries(&id),
            "edit {edit} left something behind"
        );
        last = id;
    }
}

/// **The concurrency argument, pinned.** A reader holds an open `File` for the
/// artifact's whole life, and on POSIX unlinking a name does not disturb an
/// open descriptor — the inode outlives the directory entry. So a sweep cannot
/// harm a reader that has already opened an artifact: it reads on, through a
/// name that no longer exists.
///
/// This is what makes bounding the store at one generation safe without any
/// reference counting, and it is a *filesystem* property rather than one this
/// crate enforces. A port to a filesystem without it must fail here, loudly,
/// rather than start serving torn reads quietly.
#[test]
fn a_sweep_leaves_a_concurrently_open_reader_reading() {
    let project = TempProject::new("open-reader");
    let first = publish_one(&project, "v1");

    // The concurrent run: it opened the published artifact and is still
    // reading through it when a second run publishes over the top.
    let store = Store::open(&project.dir).unwrap();
    let generation = store.generation(&first).unwrap();
    let mut reader = generation.artifact(&pkg("__first_party__")).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the payload");

    project.write("src/a.php", "<?php function a() { return 2; }\n");
    let second = publish_one(&project, "v1");
    assert_ne!(first, second);
    assert!(
        !project.gen_root().join(first.to_hex()).exists(),
        "the sweep really did unlink the generation being read"
    );

    // Same handle, after the directory entry is gone: still the same bytes.
    assert_eq!(
        reader.section(&sec("symbols")).unwrap(),
        b"the payload",
        "an open descriptor survives the unlink of its name"
    );
    // And a reader that had not opened yet gets the honest answer — a miss,
    // which costs a rebuild and changes no finding (ADR-0092 §2).
    assert!(matches!(
        generation.artifact(&pkg("__first_party__")),
        Err(Miss::Io(e)) if e.kind() == std::io::ErrorKind::NotFound
    ));
}

/// **A failed publish removes nothing.** The sweep runs after the rename *and*
/// the `CURRENT` swap, so a candidate rejected by the seal leaves the
/// generation it would have replaced exactly where it was — still `CURRENT`,
/// still readable, artifacts and all.
#[test]
fn a_failed_publish_sweeps_nothing() {
    let project = TempProject::new("failed-publish");
    let published = publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(&project);
    let mut candidate = store.begin(id_for(&sources, "v2"), vec![sources]).unwrap();
    candidate.write_artifact(&pkg("__first_party__"), &artifact(b"never published")).unwrap();
    // The seal breaks under the candidate: publication is refused.
    project.write("src/a.php", "<?php function a() { return 9; }\n");
    assert!(matches!(candidate.publish(), Err(PublishError::Drift(_))));

    assert_eq!(project.gen_entries(), published_entries(&published));
    let store = Store::open(&project.dir).unwrap();
    let current = store.current().unwrap().expect("the previous generation is still authoritative");
    assert_eq!(*current.id(), published);
    let mut reader = current.artifact(&pkg("__first_party__")).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the payload");
}

/// **Only names this crate writes.** We are deleting inside someone's project,
/// so anything under `gen/` that is not a 64-lowercase-hex directory is left
/// alone — by a publish's sweep and by the startup one alike.
#[test]
fn an_unrecognized_directory_under_gen_survives_a_sweep() {
    let project = TempProject::new("unrecognized");
    let first = publish_one(&project, "v1");
    let gen_root = project.gen_root();
    // Near misses on the recognizer, plus one plainly foreign name and a file.
    // Spelled out rather than derived from the live id: on a case-insensitive
    // filesystem an uppercasing of the real name would *be* the real name.
    let strangers = [
        "notes".to_owned(),
        "A1".repeat(32),                  // uppercase: never what this crate writes
        "d4".repeat(32)[..63].to_owned(), // 63 digits
        format!("{}0", "e5".repeat(32)),  // 65
        "g".repeat(64),                   // not hex
    ];
    for name in &strangers {
        std::fs::create_dir(gen_root.join(name)).unwrap();
        std::fs::write(gen_root.join(name).join("keep-me"), b"someone's bytes").unwrap();
    }
    // A *file* whose name is generation-shaped is not a generation either.
    let decoy = "b7".repeat(32);
    std::fs::write(gen_root.join(&decoy), b"not a directory").unwrap();

    // A publish's sweep …
    project.write("src/a.php", "<?php function a() { return 2; }\n");
    let second = publish_one(&project, "v1");
    // … and the startup one.
    Store::open(&project.dir).unwrap();

    assert!(!gen_root.join(first.to_hex()).exists(), "the superseded generation did go");
    assert!(gen_root.join(second.to_hex()).is_dir(), "and the current one stayed");
    for name in &strangers {
        assert_eq!(
            std::fs::read(gen_root.join(name).join("keep-me")).unwrap(),
            b"someone's bytes",
            "{name} is not a generation this crate wrote and must be untouched"
        );
    }
    assert_eq!(std::fs::read(gen_root.join(&decoy)).unwrap(), b"not a directory");
}

/// A generation nothing names is collected at the next open — the crash
/// between the rename and the `CURRENT` swap, and the older schema version's
/// leftovers, are the same shape and get the same treatment.
#[test]
fn open_collects_a_generation_current_does_not_name() {
    let project = TempProject::new("unreachable");
    let published = publish_one(&project, "v1");
    // A generation on disk that CURRENT does not point at: exactly what a
    // crash after the rename leaves.
    let orphan = "c3".repeat(32);
    std::fs::create_dir(project.gen_root().join(&orphan)).unwrap();
    std::fs::write(project.gen_root().join(&orphan).join("manifest"), b"stale").unwrap();

    let store = Store::open(&project.dir).unwrap();
    assert_eq!(project.gen_entries(), published_entries(&published));
    assert_eq!(*store.current().unwrap().unwrap().id(), published);
}

/// An *absent* `CURRENT` says plainly that nothing is reachable, so the
/// generations under it are bytes with no reader and they go. (The other leg
/// is a `CURRENT` that cannot be *read* — a permission, an I/O failure — which
/// is no evidence that anything is stale, and there the sweep declines rather
/// than guesses. Not testable portably without breaking the filesystem.)
#[test]
fn an_absent_current_makes_every_generation_unreachable() {
    let project = TempProject::new("no-current");
    let published = publish_one(&project, "v1");
    std::fs::remove_file(project.gen_root().join("CURRENT")).unwrap();
    let store = Store::open(&project.dir).unwrap();
    assert_eq!(project.gen_entries(), Vec::<String>::new());
    assert!(store.current().unwrap().is_none());
    assert!(matches!(store.generation(&published), Err(Miss::AbsentGeneration)));
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

// ---------------------------------------------------------------------------
// Artifact sharing (issue #519)
// ---------------------------------------------------------------------------

/// The generation-level sidecar: written independently of the package roster,
/// read back whole, and absent (a miss) for a generation that never wrote one.
#[test]
fn the_summaries_sidecar_round_trips_and_is_optional() {
    let project = TempProject::new("sidecar");
    let store = Store::open(&project.dir).unwrap();
    let sources = sample_sources(&project);
    let id = id_for(&sources, "v1");
    let mut candidate = store.begin(id, vec![sources]).unwrap();
    candidate.write_artifact(&pkg("__first_party__"), &artifact(b"the payload")).unwrap();
    candidate.write_summaries(&artifact(b"the walk blocks")).unwrap();
    let published = candidate.publish().unwrap();
    assert_eq!(
        published.summaries().unwrap().section(&sec("symbols")).unwrap(),
        b"the walk blocks"
    );

    // A generation with no sidecar is an ordinary miss, not a failure.
    let other = TempProject::new("sidecar-none");
    let id = publish_one(&other, "v1");
    let store = Store::open(&other.dir).unwrap();
    assert!(store.generation(&id).unwrap().summaries().is_err());
}

/// Adoption is the whole point: the second generation's artifact is the first
/// generation's bytes, and the store says by which mechanism.
#[test]
fn an_adopted_artifact_is_the_published_bytes() {
    let project = TempProject::new("adopt");
    let first = publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let published = store.generation(&first).unwrap();

    // A second generation over the same sources, adopting rather than writing.
    project.write("src/c.php", "<?php function c() {}\n");
    let sources =
        SourceInventory::capture(&project.dir, ["src/a.php", "src/b.php", "src/c.php"]).unwrap();
    let second = id_for(&sources, "v1");
    let mut candidate = store.begin(second, vec![sources]).unwrap();
    candidate.adopt_artifact(&pkg("__first_party__"), &published).unwrap();
    candidate.publish().unwrap();

    let store = Store::open(&project.dir).unwrap();
    let mut reader = store.generation(&second).unwrap().artifact(&pkg("__first_party__")).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the payload");
}

/// Adopting a package the source generation never had fails, and leaves the
/// candidate free to write that package itself instead.
#[test]
fn adopting_an_absent_package_leaves_the_candidate_writable() {
    let project = TempProject::new("adopt-absent");
    let first = publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let published = store.generation(&first).unwrap();
    let sources = sample_sources(&project);
    let mut candidate = store.begin(id_for(&sources, "v2"), vec![sources]).unwrap();
    let err = candidate.adopt_artifact(&pkg("vendor/never"), &published).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    // The failed adoption claimed nothing: the ordinary write still works.
    candidate.write_artifact(&pkg("vendor/never"), &artifact(b"written instead")).unwrap();
    let generation = candidate.publish().unwrap();
    let mut reader = generation.artifact(&pkg("vendor/never")).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"written instead");
}

/// **The aliasing invariant.** Two generations may name the same bytes, so
/// removing either one must leave the other whole — the property a hard link
/// makes non-obvious and this test pins. Since issue #529 the publish that
/// adopts is also the publish that sweeps the generation it adopted from, so
/// this is the bound leaning on the invariant directly.
#[test]
fn sweeping_the_adopted_from_generation_leaves_the_artifact_readable() {
    let project = TempProject::new("share-unlink");
    let first = publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let published = store.generation(&first).unwrap();
    project.write("src/c.php", "<?php function c() {}\n");
    let sources =
        SourceInventory::capture(&project.dir, ["src/a.php", "src/b.php", "src/c.php"]).unwrap();
    let second = id_for(&sources, "v1");
    let mut candidate = store.begin(second, vec![sources]).unwrap();
    candidate.adopt_artifact(&pkg("__first_party__"), &published).unwrap();
    candidate.publish().unwrap();
    drop(published);

    // The older generation went with the publish that superseded it.
    assert!(
        !project.gen_root().join(first.to_hex()).exists(),
        "the sweep unlinked the generation the artifact was adopted from"
    );
    let store = Store::open(&project.dir).unwrap();
    let mut reader = store.current().unwrap().unwrap().artifact(&pkg("__first_party__")).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the payload");
}

/// **The mutation invariant.** The only writer of an artifact refuses a name
/// that already exists, so no later run can truncate bytes another generation
/// is still naming.
#[test]
fn a_write_never_truncates_an_existing_artifact() {
    let project = TempProject::new("no-truncate");
    let id = publish_one(&project, "v1");
    let path = project.gen_root().join(id.to_hex()).join("__first_party__.pkg");
    let err = artifact(b"an overwrite").write_to(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    let store = Store::open(&project.dir).unwrap();
    let mut reader = store.current().unwrap().unwrap().artifact(&pkg("__first_party__")).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the payload");
}

/// **Crash safety with sharing.** A candidate that adopted a published
/// artifact and then died mid-build is swept like any other, and the
/// generation it borrowed from is untouched and still readable — the adopted
/// name was a second directory entry, and losing it loses nothing.
#[test]
fn a_torn_candidate_that_adopted_leaves_its_source_intact() {
    let project = TempProject::new("torn-adopt");
    let published = publish_one(&project, "v1");
    let store = Store::open(&project.dir).unwrap();
    let source = store.generation(&published).unwrap();
    project.write("src/c.php", "<?php function c() {}\n");
    let sources =
        SourceInventory::capture(&project.dir, ["src/a.php", "src/b.php", "src/c.php"]).unwrap();
    let mut candidate = store.begin(id_for(&sources, "v2"), vec![sources]).unwrap();
    candidate.adopt_artifact(&pkg("__first_party__"), &source).unwrap();
    candidate.write_summaries(&artifact(b"half-written blocks")).unwrap();
    // The crash: the process dies before publish. Forgetting the candidate
    // keeps its Drop from tidying up, which is exactly a torn state on disk.
    std::mem::forget(candidate);
    drop(source);
    assert!(
        project.gen_entries().iter().any(|n| n.starts_with(".candidate-")),
        "the torn candidate is on disk before recovery"
    );

    let store = Store::open(&project.dir).unwrap();
    assert_eq!(project.gen_entries(), published_entries(&published));
    let current = store.current().unwrap().unwrap();
    assert_eq!(*current.id(), published);
    let mut reader = current.artifact(&pkg("__first_party__")).unwrap();
    assert_eq!(
        reader.section(&sec("symbols")).unwrap(),
        b"the payload",
        "sweeping the candidate unlinked its name, never the bytes"
    );
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
