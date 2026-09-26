//! A run whose PHP child dies mid-run publishes nothing a later run could
//! replay (issue #784).
//!
//! The death is induced at the transport, the way
//! `steins-sidecar/tests/it/protocol.rs` induces it, and never by asking the
//! analysed source to cause it: the fault is a hook run once the boot surface
//! is recorded, which hands the run's live child the respawn cap's worth of
//! memory bombs directly. That is the one shape that separates the two stamps
//! issue #784 is about — the engine *identity* was answered, so a block
//! published by the dying run would carry the same replay stamp a healthy run
//! computes, and only the loss says the block is the sound subset.
//!
//! A unit test rather than a `tests/it` oracle because the hook is: a fault
//! seam has no business in the public surface. Needs a real `php` on PATH,
//! like the other sidecar-backed oracles; a PHP-less environment skips loudly.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};

use steins_db::{EffectsPolicy, PluginFacts, composer};
use steins_sidecar::{FoldArg, Sidecar};

use super::{GenerationMode, GenerationOutcome, GenerationParams, generation_check};
use crate::{FoldEngine, PREG_INVALID_PATTERN_ID, RecordingFolder, RuntimePostures};

// ---------------------------------------------------------------------------
// The fault seam.
// ---------------------------------------------------------------------------

type Fault = Box<dyn FnOnce(&mut RecordingFolder)>;

thread_local! {
    /// The fault the next run on this thread injects. Per thread, so the
    /// harness's parallel tests cannot hand each other a fault; the walk's
    /// workers never read it, since it is taken on the run's own thread before
    /// any of them is hired.
    static AFTER_BOOT: RefCell<Option<Fault>> = const { RefCell::new(None) };
}

/// Run this thread's pending fault, if any, against the run's folder.
pub(super) fn after_boot(folder: &mut RecordingFolder) {
    if let Some(fault) = AFTER_BOOT.with(|slot| slot.borrow_mut().take()) {
        fault(folder);
    }
}

/// Kill the run's child until the transport gives up. One death is not enough:
/// the next request respawns a child and is answered, so the run loses only the
/// bomb's own reply. Past [`steins_sidecar::RESPAWN_CAP`] every later request
/// widens, which is what a dead fold surface costs the walk.
fn abandon_the_transport(folder: &mut RecordingFolder) {
    let bomb = [FoldArg::Str("x".to_owned()), FoldArg::Int(2_000_000_000)];
    for _ in 0..=steins_sidecar::RESPAWN_CAP {
        let _ = folder.engine_mut().fold("str_repeat", &bomb, true);
    }
    let posture = folder.posture();
    assert!(posture.abandoned && posture.losses > 0, "the transport outlived the fault: {posture:?}");
}

// ---------------------------------------------------------------------------
// Fixture and plumbing.
// ---------------------------------------------------------------------------

/// PCRE refuses the pattern, and only the engine can say so: the finding is
/// the sidecar's, so it is exactly what a dead one leaves out.
const REGEX: &str = "<?php declare(strict_types = 1);\n\
    namespace App;\n\
    function matches(string $s): void { preg_match('/(unclosed/', $s); }\n";

/// A file that names nothing of `REGEX`'s and asks the engine nothing.
const OTHER: &str = "<?php declare(strict_types = 1);\n\
    namespace App;\n\
    function other(): int { return 1; }\n";

const COMPOSER_JSON: &str = r#"{"name": "fixture/lost", "autoload": {"psr-4": {"App\\": "src/"}}}"#;

const COMPOSER_LOCK: &str = r#"{"packages": [], "packages-dev": []}"#;

fn php_or_skip(test: &str) -> bool {
    match Sidecar::spawn() {
        Ok(_) => true,
        Err(e) => {
            // Not `eprintln!`: steins-cli's output-seam test scans every
            // crate's `src/` for raw printing, and this module lives there.
            let _ = writeln!(
                std::io::stderr(),
                "SKIP {test}: could not spawn php sidecar ({e}) — is `php` on PATH?"
            );
            false
        }
    }
}

/// A throwaway directory under the OS temp dir, cleaned on drop.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-lost-answer-{tag}-{}-{}",
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

/// Write the fixture; returns the analyzed files in universe-slot order.
fn write_fixture(root: &Path) -> Vec<PathBuf> {
    for (rel, content) in [
        ("composer.json", COMPOSER_JSON),
        ("composer.lock", COMPOSER_LOCK),
        ("src/Other.php", OTHER),
        ("src/Regex.php", REGEX),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    vec![root.join("src/Other.php"), root.join("src/Regex.php")]
}

/// One generation run over the fixture, PHP on, with `fault` injected once the
/// boot surface is recorded.
fn run(root: &Path, files: &[PathBuf], fault: Option<Fault>) -> GenerationOutcome {
    let layout = composer::discover(&[root.to_path_buf()], root);
    let partition = steins_db::partition::discover(&layout);
    let plugins = PluginFacts::none();
    let effects = EffectsPolicy::none();
    let params = GenerationParams {
        store_root: root,
        capture_root: root,
        files,
        layout: &layout,
        partition: &partition,
        plugins: &plugins,
        effects: &effects,
        postures: RuntimePostures::default(),
        php: true,
        paranoid: false,
    };
    AFTER_BOOT.with(|slot| *slot.borrow_mut() = fault);
    let outcome = generation_check(&params).expect("the generation lifecycle runs");
    AFTER_BOOT.with(|slot| assert!(slot.borrow().is_none(), "the fault was never injected"));
    outcome
}

fn invalid_patterns(outcome: &GenerationOutcome) -> usize {
    outcome.findings.iter().filter(|d| d.id == PREG_INVALID_PATTERN_ID).count()
}

// ---------------------------------------------------------------------------
// The oracles.
// ---------------------------------------------------------------------------

/// Issue #784's repro, with the death induced at the transport. The dying run
/// loses the finding, which is the degraded run's honest answer; what it must
/// not do is publish that answer, because the next run's stamp would license
/// replaying it. The next run walks instead, finds the pattern, and publishes;
/// the one after replays that, so a healthy tree keeps its warm path.
#[test]
fn a_run_that_lost_a_fold_answer_leaves_nothing_to_replay() {
    if !php_or_skip("a_run_that_lost_a_fold_answer_leaves_nothing_to_replay") {
        return;
    }
    let tmp = TempDir::new("cold");
    let files = write_fixture(&tmp.dir);

    let dying = run(&tmp.dir, &files, Some(Box::new(abandon_the_transport)));
    assert_eq!(invalid_patterns(&dying), 0, "the fault did not reach the pattern's question");
    assert_eq!(dying.report.generation, None, "a run that lost an answer published");

    let healthy = run(&tmp.dir, &files, None);
    assert_eq!(invalid_patterns(&healthy), 1, "the lost finding was replayed");
    assert_eq!(healthy.report.walk.replayed, 0, "notes: {:#?}", healthy.report.notes);
    assert_eq!(healthy.report.mode, GenerationMode::Cold, "notes: {:#?}", healthy.report.notes);
    assert!(healthy.report.generation.is_some(), "notes: {:#?}", healthy.report.notes);

    let warm = run(&tmp.dir, &files, None);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    assert_eq!(warm.report.walk.walked, 0, "notes: {:#?}", warm.report.notes);
    assert_eq!(warm.report.walk.replayed, files.len());
    assert_eq!(invalid_patterns(&warm), 1);
}

/// A dying run over a store that already holds a healthy generation leaves it
/// as `CURRENT`: the run after it is warm from the generation the dying run
/// found, not cold, and replays blocks a run that lost nothing computed.
#[test]
fn a_run_that_lost_a_fold_answer_keeps_the_published_generation() {
    if !php_or_skip("a_run_that_lost_a_fold_answer_keeps_the_published_generation") {
        return;
    }
    let tmp = TempDir::new("warm");
    let files = write_fixture(&tmp.dir);

    let cold = run(&tmp.dir, &files, None);
    let published = cold.report.generation.clone().expect("the cold build publishes");
    assert_eq!(invalid_patterns(&cold), 1);

    // Another pattern PCRE refuses: the published fold table cannot answer it,
    // so the dying run walks `Regex.php` and has to ask the dead child.
    std::fs::write(&files[1], REGEX.replace("(unclosed", "(still-unclosed")).unwrap();
    let dying = run(&tmp.dir, &files, Some(Box::new(abandon_the_transport)));
    assert_eq!(invalid_patterns(&dying), 0, "the fault did not reach the pattern's question");
    assert_eq!(dying.report.generation, None, "a run that lost an answer published");

    let healthy = run(&tmp.dir, &files, None);
    assert_eq!(healthy.report.mode, GenerationMode::Warm, "notes: {:#?}", healthy.report.notes);
    assert_eq!(invalid_patterns(&healthy), 1, "the lost finding was replayed");
    assert_ne!(healthy.report.generation.as_deref(), Some(published.as_str()));
    assert_eq!(healthy.report.walk.replayed, 1, "`Other.php` replays from the healthy generation");
}
