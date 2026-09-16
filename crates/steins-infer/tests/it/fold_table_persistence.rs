//! The persisted fold table (ADR-0092 §4, issue #488): the `replay_fold.rs`
//! differential oracle extended across the DISK boundary. Run A records fold
//! traffic live; the table is written through the steins-gen container, read
//! back, and run B answers table-first with the live engine still attached.
//!
//! What these tests pin, per the ADR: replay-from-disk means exactly what
//! ask-the-engine means (byte-identical findings, nothing re-asked); an
//! engine-identity mismatch is a miss for the whole table; a malformed row is
//! a miss for that row alone, asked live again and repaired in the published
//! table; and the published table is mark-and-sweep by construction — a row
//! this run neither consumed nor asked is not carried forward.
//!
//! Needs a real `php` on PATH, like the other sidecar-backed oracles; a
//! PHP-less environment skips loudly.

use std::path::PathBuf;

use steins_infer::{
    Diagnostic, EngineFolder, Folder, FoldTableArtifact, ProcessEngine, RecordingEngine,
    RecordingFolder, SidecarFolder, check_with, request_key,
};
use steins_sidecar::{FoldArg, Sidecar, fold_params};
use steins_syntax::SourceTree;

// ---------------------------------------------------------------------------
// Fixtures and plumbing.
// ---------------------------------------------------------------------------

/// The flagship of issue #60/#59, unchanged from `replay_fold.rs`: a project
/// call in argument position, whose body concatenates and folds through the
/// engine — the strongest single differential subject there is.
const FLAGSHIP: &str = "<?php\n\
    function greet(int $times, string $name): string {\n\
        return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
    }\n\
    \\PHPStan\\dumpType(greet(2, \"World\"));\n";

/// One fold: the smallest thing that reaches the seam.
const ONE_FOLD: &str = "<?php\n$x = strtoupper(\"ab\");\n\\PHPStan\\dumpType($x);\n";

/// Two folds — the sweep fixture: run A asks both, run B (over [`ONE_FOLD`])
/// asks only the first, and the second must not survive into B's table.
const TWO_FOLDS: &str = "<?php\n\
    $x = strtoupper(\"ab\");\n\
    $y = strtolower(\"CD\");\n\
    \\PHPStan\\dumpType($x);\n\
    \\PHPStan\\dumpType($y);\n";

/// [`ONE_FOLD`] plus a question run A never asked — the fall-through fixture.
const ONE_FOLD_PLUS: &str = "<?php\n\
    $x = strtoupper(\"ab\");\n\
    $z = ucfirst(\"xy\");\n\
    \\PHPStan\\dumpType($x);\n\
    \\PHPStan\\dumpType($z);\n";

fn spawn_or_skip(test: &str) -> bool {
    match Sidecar::spawn() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIP {test}: could not spawn php sidecar ({e}) — is `php` on PATH?");
            false
        }
    }
}

fn findings_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", folder)
}

/// A throwaway directory under the OS temp dir, cleaned on drop.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-fold-table-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Round-trip an artifact through the real container on the real filesystem —
/// every warm run below reads bytes a previous run wrote, never a value that
/// stayed in memory.
fn persist_and_reload(tag: &str, artifact: &FoldTableArtifact) -> FoldTableArtifact {
    let tmp = TempDir::new(tag);
    let path = tmp.dir.join("fold.pkg");
    artifact.to_builder().write_to(&path).expect("the artifact writes");
    let mut reader =
        steins_gen::ArtifactReader::open(&path, steins_gen::DecodeBudget::default())
            .expect("the artifact opens");
    FoldTableArtifact::read(&mut reader).expect("the artifact decodes")
}

/// Run A: a cold recording run over `src`, returning its findings and the
/// table it would publish.
fn record(src: &str) -> (Vec<Diagnostic>, FoldTableArtifact) {
    let mut folder = EngineFolder::with_engine(RecordingEngine::cold(ProcessEngine::enabled()));
    let findings = findings_with(src, &mut folder);
    let artifact = folder.published_table().expect("a live run records a publishable table");
    (findings, artifact)
}

/// Run B: a warm table-first run over `src`, live engine still attached.
fn replay(src: &str, artifact: FoldTableArtifact) -> (Vec<Diagnostic>, RecordingFolder) {
    let mut folder =
        EngineFolder::with_engine(RecordingEngine::warm(ProcessEngine::enabled(), artifact));
    let findings = findings_with(src, &mut folder);
    (findings, folder)
}

fn fold_key(name: &str, arg: &str) -> String {
    request_key(
        "fold",
        &fold_params(name, &[FoldArg::Str(arg.to_owned())], false).expect("askable"),
    )
}

// ---------------------------------------------------------------------------
// The oracles.
// ---------------------------------------------------------------------------

/// Oracle (a), the differential across the disk boundary: run B's findings are
/// byte-identical to run A's, run B's wrapped engine answered nothing (every
/// question was a recorded row), and run B publishes the same table — the
/// recorded rows ARE the run's fold surface, end to end. Run A itself is
/// pinned against a direct `SidecarFolder` run first, so "identical to run A"
/// cannot drift away from "identical to the live engine".
#[test]
fn a_persisted_table_replays_to_byte_identical_findings_and_reasks_nothing() {
    if !spawn_or_skip("a_persisted_table_replays_to_byte_identical_findings_and_reasks_nothing") {
        return;
    }
    for src in [ONE_FOLD, FLAGSHIP] {
        let (findings_a, artifact) = record(src);
        let mut direct = SidecarFolder::enabled();
        assert_eq!(
            findings_a,
            findings_with(src, &mut direct),
            "recording is a transport, not a second semantics:\n{src}"
        );
        assert!(!artifact.rows.is_empty(), "the run actually asked the engine something");

        let reloaded = persist_and_reload("differential", &artifact);
        assert_eq!(reloaded, artifact, "the container round-trips the table bytes");

        let (findings_b, folder_b) = replay(src, reloaded);
        assert_eq!(findings_b, findings_a, "replay-from-disk diverged from live on:\n{src}");
        assert_eq!(
            folder_b.fresh_keys(),
            &[] as &[String],
            "run B asked the live engine a question run A already recorded"
        );
        assert_eq!(
            folder_b.published_table().expect("run B publishes").rows,
            artifact.rows,
            "the same questions consume the same rows"
        );
    }
}

/// Oracle (a)'s positive half: a warm run whose source asks one NEW question
/// falls through for exactly that — everything the live engine heard is a key
/// run A never recorded, and the published table now covers both runs' asks.
#[test]
fn the_live_engine_hears_only_what_the_table_never_recorded() {
    if !spawn_or_skip("the_live_engine_hears_only_what_the_table_never_recorded") {
        return;
    }
    let (_, artifact) = record(ONE_FOLD);
    let reloaded = persist_and_reload("fall-through", &artifact);
    let (findings_b, folder_b) = replay(ONE_FOLD_PLUS, reloaded);

    let mut direct = SidecarFolder::enabled();
    assert_eq!(findings_b, findings_with(ONE_FOLD_PLUS, &mut direct));

    let fresh = folder_b.fresh_keys();
    assert!(!fresh.is_empty(), "the new question reached the live engine");
    for key in fresh {
        assert!(
            !artifact.rows.contains_key(key),
            "a recorded question was re-asked live: {key}"
        );
    }
    assert!(fresh.contains(&fold_key("ucfirst", "xy")), "the new fold is among them: {fresh:?}");
    let published = folder_b.published_table().expect("run B publishes");
    assert!(published.rows.contains_key(&fold_key("strtoupper", "ab")), "consumed row kept");
    assert!(published.rows.contains_key(&fold_key("ucfirst", "xy")), "fresh row recorded");
}

/// Oracle (b): a doctored stored identity — a different PHP version, or a
/// table whose rows were not keyed by the call site's strict mode — drops the
/// WHOLE table. Findings stay byte-identical (everything is asked live, which
/// is a cold run), the stored rows serve nothing (the fold row the table held
/// comes back through the live engine), and the published table is rebuilt
/// under the live identity.
#[test]
fn a_doctored_identity_is_a_miss_for_the_whole_table() {
    if !spawn_or_skip("a_doctored_identity_is_a_miss_for_the_whole_table") {
        return;
    }
    let (findings_a, artifact) = record(ONE_FOLD);
    let upper = fold_key("strtoupper", "ab");
    assert!(artifact.rows.contains_key(&upper), "run A recorded the fold row");

    let doctor_version = |a: &mut FoldTableArtifact| a.identity.php_version = "7.4.33".to_owned();
    let doctor_strict = |a: &mut FoldTableArtifact| a.identity.strict_keyed = false;
    for (axis, doctor) in
        [("php_version", &doctor_version as &dyn Fn(&mut FoldTableArtifact)), ("strict_keyed", &doctor_strict)]
    {
        let mut doctored = artifact.clone();
        doctor(&mut doctored);
        let reloaded = persist_and_reload("identity", &doctored);
        let (findings_b, folder_b) = replay(ONE_FOLD, reloaded);
        assert_eq!(findings_b, findings_a, "a dropped table is a cold run, axis {axis}");
        assert!(
            folder_b.fresh_keys().contains(&upper),
            "the stored fold row must not serve under a doctored {axis}: {:?}",
            folder_b.fresh_keys()
        );
        assert_eq!(
            folder_b.published_table().expect("run B publishes").rows,
            artifact.rows,
            "the table is rebuilt fresh under the live identity, axis {axis}"
        );
    }
}

/// Oracle (c): one malformed row degrades to ask-live for that row ALONE —
/// findings byte-identical, exactly one fresh question, and the published
/// table carries the fresh answer where the rot was. Never an error surfaced
/// to analysis.
#[test]
fn a_malformed_row_is_a_miss_for_that_row_alone() {
    if !spawn_or_skip("a_malformed_row_is_a_miss_for_that_row_alone") {
        return;
    }
    let (findings_a, artifact) = record(TWO_FOLDS);
    let upper = fold_key("strtoupper", "ab");
    let lower = fold_key("strtolower", "CD");
    assert!(artifact.rows.contains_key(&upper) && artifact.rows.contains_key(&lower));

    let mut doctored = artifact.clone();
    // Shape rot: `kind: value` with no value — `parse_fold_result` would
    // collapse it into its own widen, which is exactly what must NOT serve.
    *doctored.rows.get_mut(&upper).expect("the row exists") =
        serde_json::json!({ "kind": "value" });
    let reloaded = persist_and_reload("malformed-row", &doctored);
    let (findings_b, folder_b) = replay(TWO_FOLDS, reloaded);
    assert_eq!(findings_b, findings_a, "the rotten row was re-asked, not served");
    assert_eq!(
        folder_b.fresh_keys(),
        std::slice::from_ref(&upper),
        "exactly the rotten row went live; every healthy row served"
    );
    let published = folder_b.published_table().expect("run B publishes");
    assert_eq!(
        published.rows.get(&upper),
        artifact.rows.get(&upper),
        "the fresh answer replaced the rot in the published table"
    );
    assert_eq!(published.rows, artifact.rows, "…and nothing else moved");
}

/// Oracle (d), mark-and-sweep by construction: a row run A recorded that run B
/// never asks is absent from run B's published table — no TTLs, no caps,
/// unreachable rows simply fail to survive the run.
#[test]
fn a_row_nothing_asked_is_swept_from_the_published_table() {
    if !spawn_or_skip("a_row_nothing_asked_is_swept_from_the_published_table") {
        return;
    }
    let (_, artifact) = record(TWO_FOLDS);
    let upper = fold_key("strtoupper", "ab");
    let lower = fold_key("strtolower", "CD");
    assert!(artifact.rows.contains_key(&lower), "run A recorded the row to be swept");

    let reloaded = persist_and_reload("sweep", &artifact);
    let (findings_b, folder_b) = replay(ONE_FOLD, reloaded);
    let mut direct = SidecarFolder::enabled();
    assert_eq!(findings_b, findings_with(ONE_FOLD, &mut direct));
    assert_eq!(
        folder_b.fresh_keys(),
        &[] as &[String],
        "run B's questions are a subset of run A's, all served from the table"
    );
    let published = folder_b.published_table().expect("run B publishes");
    assert!(published.rows.contains_key(&upper), "the consumed row is carried forward");
    assert!(
        !published.rows.contains_key(&lower),
        "the row nothing asked is not carried forward"
    );
    assert_eq!(published.identity, artifact.identity, "same engine, same identity");
}
