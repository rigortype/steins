//! A package whose analyzer moved decodes none of its trace or facts payloads
//! (issue #828).
//!
//! A change to either payload's format takes no `SCHEMA_VERSION` bump: both
//! are decoded only once `load_trees` has matched the artifact's `sources`
//! record against this build's analyzer version, and a change to the code that
//! writes them moves that version. This pins the order that rule rests on.
//! Output equality cannot, because a decode that runs before the gate and is
//! then thrown away leaves every finding as it was, so the two decode sites
//! record themselves here instead.
//!
//! Two seams, both test-only. A run can stamp a foreign analyzer version,
//! which is what a generation another build published looks like: its
//! `sources` records, its identity and its replay stamp all name that build.
//! And each decode of a trace or facts payload out of a published artifact
//! logs its path, which the oracle filters to its own fixture, so parallel
//! tests cannot count for each other.
//!
//! A unit test rather than a `tests/it` oracle because the seams are, like the
//! fault seam beside it (issue #784). PHP stays off: the question is the gate,
//! not the fold.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use steins_db::{EffectsPolicy, PluginFacts, composer};

use super::{GenerationMode, GenerationOutcome, GenerationParams, generation_check};
use crate::{Progress, RuntimePostures};

// ---------------------------------------------------------------------------
// The seams.
// ---------------------------------------------------------------------------

thread_local! {
    /// The analyzer version the next run on this thread stamps in place of its
    /// own. Per thread, like the fault seam: the identity, the `sources` record
    /// and the gate all read `analyzer_version` on the run's own thread.
    static FOREIGN_ANALYZER: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// The analyzer version this thread's run stamps instead of its own, if any.
pub(super) fn foreign_analyzer() -> Option<&'static str> {
    FOREIGN_ANALYZER.with(Cell::get)
}

/// Which payload a decode read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Payload {
    Trace,
    Facts,
}

/// Every payload decode out of a published artifact, with the file's
/// diagnostic path. Process-wide rather than per thread, because a deferred
/// tree decodes on whichever walk worker first reaches its file.
static DECODES: Mutex<Vec<(Payload, String)>> = Mutex::new(Vec::new());

/// Log one decode. The crate decodes these payloads in two places, and both
/// call this before they read a byte: the deferred tree handle
/// (`load::deferred_tree`, the one caller of `TraceIndex::read_tree`) and the
/// facts read in `load::load_trees` (the one caller of `read_facts` outside
/// its own tests). A third decode site has to call it too, or this oracle
/// cannot see it.
pub(super) fn decoded(payload: Payload, path: &str) {
    DECODES.lock().unwrap_or_else(PoisonError::into_inner).push((payload, path.to_owned()));
}

/// Take this fixture's decodes out of the log, as `(trees, facts)`.
fn take_decodes(root: &Path) -> (usize, usize) {
    let mut log = DECODES.lock().unwrap_or_else(PoisonError::into_inner);
    let (mut trees, mut facts) = (0, 0);
    log.retain(|(payload, path)| {
        let ours = Path::new(path).starts_with(root);
        if ours {
            match payload {
                Payload::Trace => trees += 1,
                Payload::Facts => facts += 1,
            }
        }
        !ours
    });
    (trees, facts)
}

// ---------------------------------------------------------------------------
// Fixture and plumbing.
// ---------------------------------------------------------------------------

/// The version the publishing build stamps: any string this build's own
/// version cannot equal.
const FOREIGN: &str = "0.0.0+another-build";

const MAIN: &str = "<?php declare(strict_types = 1);\n\
    namespace App;\n\
    function main(): string { return \\Acme\\greet(helper()); }\n";

const HELPER: &str = "<?php declare(strict_types = 1);\n\
    namespace App;\n\
    function helper(): string { return 'x'; }\n";

const VENDOR: &str = "<?php declare(strict_types = 1);\n\
    namespace Acme;\n\
    function greet(string $s): string { return $s; }\n";

const COMPOSER_JSON: &str =
    r#"{"name": "fixture/gate", "require": {"acme/lib": "^1.0"}, "autoload": {"psr-4": {"App\\": "src/"}}}"#;

const COMPOSER_LOCK: &str =
    r#"{"packages": [{"name": "acme/lib", "require": {}}], "packages-dev": []}"#;

/// A throwaway directory under the OS temp dir, cleaned on drop.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-gate-order-{}-{}",
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

/// Write the two-package fixture; returns the analyzed files in universe-slot
/// order.
fn write_fixture(root: &Path) -> Vec<PathBuf> {
    for (rel, content) in [
        ("composer.json", COMPOSER_JSON),
        ("composer.lock", COMPOSER_LOCK),
        ("src/helper.php", HELPER),
        ("src/main.php", MAIN),
        ("vendor/acme/lib/src/lib.php", VENDOR),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    vec![
        root.join("src/helper.php"),
        root.join("src/main.php"),
        root.join("vendor/acme/lib/src/lib.php"),
    ]
}

/// One generation run over the fixture as the build `analyzer` names, or as
/// this one. PHP is off, and every file walks, so every tree handle the run
/// holds is forced.
fn run(root: &Path, files: &[PathBuf], analyzer: Option<&'static str>) -> GenerationOutcome {
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
        php: false,
        paranoid: true,
        progress: &Progress::off(),
    };
    FOREIGN_ANALYZER.with(|slot| slot.set(analyzer));
    let outcome = generation_check(&params);
    FOREIGN_ANALYZER.with(|slot| slot.set(None));
    outcome.expect("the generation lifecycle runs")
}

// ---------------------------------------------------------------------------
// The oracle.
// ---------------------------------------------------------------------------

/// Issue #828: over a generation another build published, every package is
/// refused at the gate and decodes nothing, trees and facts alike. The control
/// is the same store once this build has republished it: every file loads, and
/// the same counters see a tree and a facts decode for each, so a zero above
/// is the gate's and not a counter that never counts.
#[test]
fn a_package_whose_analyzer_moved_decodes_no_trace_or_facts_payload() {
    let tmp = TempDir::new();
    let files = write_fixture(&tmp.dir);

    let foreign = run(&tmp.dir, &files, Some(FOREIGN));
    assert_eq!(foreign.report.mode, GenerationMode::Cold);
    assert!(foreign.report.generation.is_some(), "notes: {:#?}", foreign.report.notes);
    assert_eq!(take_decodes(&tmp.dir), (0, 0), "a cold run has nothing to decode");

    let moved = run(&tmp.dir, &files, None);
    assert_eq!(moved.report.mode, GenerationMode::Warm, "notes: {:#?}", moved.report.notes);
    assert_eq!(moved.report.packages.len(), 2, "the root package and the vendor one");
    for package in &moved.report.packages {
        assert_eq!(package.disposition, "parsed (analyzer moved)", "{}", package.name);
        assert_eq!(
            (package.loaded, package.decoded, package.parsed),
            (0, 0, package.files),
            "{}",
            package.name
        );
    }
    assert_eq!(take_decodes(&tmp.dir), (0, 0), "(trees, facts) decoded past a moved analyzer");

    let current = run(&tmp.dir, &files, None);
    for package in &current.report.packages {
        assert_eq!(package.disposition, "loaded", "{}", package.name);
        assert_eq!(package.decoded, package.files, "{}", package.name);
    }
    assert_eq!(
        take_decodes(&tmp.dir),
        (files.len(), files.len()),
        "(trees, facts) the control decoded"
    );
}
