//! The warm ≡ cold oracle at full strength (ADR-0092 §5, issue #489 slice A):
//! the generation lifecycle — cold build → publish → warm rebuild — driven
//! end to end over a real store on the real filesystem, against the auditor's
//! mandatory tripwire fixture.
//!
//! What these tests pin:
//!
//! * **(a) The vendor-whitespace tripwire.** A whitespace-only edit in a
//!   vendor file moves every propagated finding's origin line, so a design
//!   that caches propagated results fails it by construction. Here the ROOT
//!   package is served from artifacts (zero reparses) while the propagated
//!   effect finding it reports names the vendor origin's NEW line — and the
//!   whole findings set is byte-identical to a FRESH cold run of the edited
//!   tree.
//! * **(b) The no-change warm rebuild.** An untouched tree warm-rebuilds with
//!   zero reparses anywhere (the orchestrator's own per-package counters say
//!   so), byte-identical findings, zero fold questions going live, and the
//!   generation left exactly as it was.
//! * **(c) The poisoned artifact.** Doctored artifact bytes degrade exactly
//!   that package to reparse — cost, never meaning: findings stay
//!   byte-identical, the other package still loads. Under an unchanged
//!   generation identity the store keeps the published copy (its own
//!   same-fingerprint rule), so the poison keeps costing that package's
//!   reparse — deterministic, and the recovery story is ADR-0092 §8's
//!   "throw the cache away".
//!
//! Needs a real `php` on PATH, like the other sidecar-backed oracles; a
//! PHP-less environment skips loudly.

use std::path::{Path, PathBuf};

use steins_db::{EffectsPolicy, PluginFacts, composer};
use steins_infer::{
    Diagnostic, FinalKeyword, GenerationMode, GenerationOutcome, GenerationParams, PackageKind,
    generation_check,
};
use steins_sidecar::Sidecar;

// ---------------------------------------------------------------------------
// Fixture and plumbing.
// ---------------------------------------------------------------------------

/// The root file: a pure envelope violated only transitively through the
/// vendor package (the propagated finding whose origin line the whitespace
/// edit moves), plus a fold the recorded table serves on the warm path.
const APP: &str = "<?php\n\
    #[\\Steins\\Pure]\n\
    function appMain(): string { return \\Acme\\emit(\"x\"); }\n\
    $up = strtoupper(\"ab\");\n\
    \\PHPStan\\dumpType($up);\n";

/// The fake vendor package's one file. `echo` on line 3 is the effect origin
/// every propagated copy embeds.
const VENDOR_LIB: &str = "<?php\n\
    namespace Acme;\n\
    function emit(string $s): string { echo $s; return $s; }\n";

const COMPOSER_JSON: &str =
    r#"{"name": "fixture/app", "require": {"acme/lib": "^1.0"}, "autoload": {"psr-4": {"App\\": "src/"}}}"#;

const COMPOSER_LOCK: &str =
    r#"{"packages": [{"name": "acme/lib", "require": {}}], "packages-dev": []}"#;

fn spawn_or_skip(test: &str) -> bool {
    match Sidecar::spawn() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIP {test}: could not spawn php sidecar ({e}) — is `php` on PATH?");
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
            "steins-warm-gen-{tag}-{}-{}",
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

/// Write the two-package fixture and return the analyzed files in
/// universe-slot order (the sorted walk's order).
fn write_fixture(root: &Path) -> Vec<PathBuf> {
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("composer.json", COMPOSER_JSON);
    write("composer.lock", COMPOSER_LOCK);
    write("src/app.php", APP);
    write("vendor/acme/lib/src/lib.php", VENDOR_LIB);
    let partition = steins_db::partition::discover(&composer::discover(
        &[root.to_path_buf()],
        root,
    ));
    assert_eq!(
        partition
            .universe()
            .packages()
            .iter()
            .find(|p| p.name.as_str() == "acme/lib")
            .map(|p| p.kind),
        Some(PackageKind::Vendor),
        "the lock classifies the package"
    );
    vec![root.join("src/app.php"), root.join("vendor/acme/lib/src/lib.php")]
}

/// One full generation run over the fixture: the CLI's boundary resolution
/// (layout, partition), the orchestrator, PHP on.
fn run(root: &Path, store_root: &Path, files: &[PathBuf]) -> GenerationOutcome {
    run_with(root, store_root, files, false)
}

/// [`run`] with the paranoid walk verifier forced on.
fn run_paranoid(root: &Path, store_root: &Path, files: &[PathBuf]) -> GenerationOutcome {
    run_with(root, store_root, files, true)
}

fn run_with(
    root: &Path,
    store_root: &Path,
    files: &[PathBuf],
    paranoid: bool,
) -> GenerationOutcome {
    let layout = composer::discover(&[root.to_path_buf()], root);
    let partition = steins_db::partition::discover(&layout);
    let plugins = PluginFacts::none();
    let effects = EffectsPolicy::none();
    let params = GenerationParams {
        store_root,
        capture_root: root,
        files,
        layout: &layout,
        partition: &partition,
        plugins: &plugins,
        effects: &effects,
        warning_handler_abort: true,
        final_keyword: FinalKeyword::Enforced,
        php: true,
        paranoid,
    };
    generation_check(&params).expect("the generation lifecycle runs")
}

/// The byte-identity comparison: every field of every finding, in the CLI's
/// own sort order — stronger than comparing a rendering of them.
fn canon(mut findings: Vec<Diagnostic>) -> Vec<Diagnostic> {
    findings.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column, a.id, a.message.as_str())
            .cmp(&(b.path.as_str(), b.line, b.column, b.id, b.message.as_str()))
    });
    findings
}

fn package<'a>(outcome: &'a GenerationOutcome, name: &str) -> &'a steins_infer::PackageReport {
    outcome
        .report
        .packages
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("package {name} missing from the report"))
}

// ---------------------------------------------------------------------------
// The oracles.
// ---------------------------------------------------------------------------

/// Oracle (a): the vendor-whitespace tripwire, mandatory per the issue-#489
/// design pin. The root package loads from artifacts, yet its propagated
/// effect finding follows the vendor origin's moved line — and warm equals a
/// FRESH cold run of the edited tree, field for field.
#[test]
fn a_vendor_whitespace_edit_warm_rebuild_equals_a_fresh_cold_run() {
    if !spawn_or_skip("a_vendor_whitespace_edit_warm_rebuild_equals_a_fresh_cold_run") {
        return;
    }
    let tmp = TempDir::new("vendor-ws");
    let files = write_fixture(&tmp.dir);
    let vendor_path = files[1].to_string_lossy().into_owned();

    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(cold.report.mode, GenerationMode::Cold);
    assert!(cold.report.generation.is_some(), "the cold build publishes");
    // The tripwire is armed: a ROOT-file finding embeds the vendor origin line.
    let names_origin = |findings: &[Diagnostic], line: u32| {
        findings.iter().any(|d| {
            d.path.ends_with("app.php")
                && d.message.contains("via echo at ")
                && d.message.contains(&format!("{vendor_path} line {line}"))
        })
    };
    assert!(
        names_origin(&cold.findings, 3),
        "expected a propagated finding naming the vendor origin, got {:#?}",
        cold.findings
    );

    // The whitespace-only edit: one blank line after `<?php` — every
    // declaration and origin in the vendor file moves down one line.
    let text = std::fs::read_to_string(&files[1]).unwrap();
    std::fs::write(&files[1], text.replacen("<?php\n", "<?php\n\n", 1)).unwrap();

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    let acme = package(&warm, "acme/lib");
    assert_eq!((acme.parsed, acme.loaded), (1, 0), "the edited package reparses");
    let root_pkg = package(&warm, "__root__");
    assert_eq!((root_pkg.parsed, root_pkg.loaded), (0, 1), "the root serves from artifacts");
    assert!(
        names_origin(&warm.findings, 4),
        "the propagated origin line must move with the edit, got {:#?}",
        warm.findings
    );

    // The oracle proper: byte-identical to a FRESH cold run of the edited
    // tree (a store that never published, same source, same engine).
    let fresh_store = TempDir::new("vendor-ws-fresh");
    let fresh = run(&tmp.dir, &fresh_store.dir, &files);
    assert_eq!(fresh.report.mode, GenerationMode::Cold);
    assert_eq!(
        canon(warm.findings),
        canon(fresh.findings),
        "warm findings diverged from a fresh cold run of the edited tree"
    );
}

/// Oracle (b): a warm rebuild over an untouched tree parses zero files — the
/// orchestrator's own counters say so, package by package — reproduces the
/// findings byte-for-byte, answers every fold from the recorded table, and
/// leaves the published generation exactly where it was.
#[test]
fn an_untouched_tree_warm_rebuilds_with_zero_reparses() {
    if !spawn_or_skip("an_untouched_tree_warm_rebuilds_with_zero_reparses") {
        return;
    }
    let tmp = TempDir::new("no-change");
    let files = write_fixture(&tmp.dir);

    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(cold.report.mode, GenerationMode::Cold);
    assert!(cold.report.fold.table_published, "a live engine records a publishable table");

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    for pkg in &warm.report.packages {
        assert_eq!(pkg.parsed, 0, "package {} reparsed on an untouched tree", pkg.name);
        assert_eq!(pkg.loaded, pkg.files, "package {} did not serve from artifacts", pkg.name);
        assert_eq!(pkg.disposition, "loaded", "package {}", pkg.name);
    }
    assert_eq!(canon(warm.findings), canon(cold.findings), "no-change warm diverged from cold");
    assert!(warm.report.fold.loaded_rows > 0, "the recorded fold table was loaded");
    assert_eq!(warm.report.fold.fresh_rows, 0, "no fold question went to the live engine");
    assert_eq!(
        warm.report.generation, cold.report.generation,
        "an unchanged universe keeps its generation id"
    );
}

/// Oracle (c): doctored artifact bytes degrade exactly that package to
/// reparse — the counter shows it, the sibling package still loads, and the
/// findings stay byte-identical. Under the unchanged identity the store keeps
/// its published copy, so the next warm run pays the same reparse — cost,
/// never meaning, deterministically.
#[test]
fn a_poisoned_package_artifact_degrades_that_package_alone() {
    if !spawn_or_skip("a_poisoned_package_artifact_degrades_that_package_alone") {
        return;
    }
    let tmp = TempDir::new("poison");
    let files = write_fixture(&tmp.dir);

    let cold = run(&tmp.dir, &tmp.dir, &files);
    let hex = cold.report.generation.clone().expect("the cold build publishes");

    // Doctor the vendor package's artifact wholesale (the mangled file name is
    // the store's own percent-encoding of `acme/lib`).
    let artifact = tmp.dir.join(".steins").join("gen").join(&hex).join("acme%2Flib.pkg");
    let len = std::fs::metadata(&artifact).expect("the vendor artifact is on disk").len();
    std::fs::write(&artifact, vec![0xFF; len as usize]).unwrap();

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    let acme = package(&warm, "acme/lib");
    assert_eq!((acme.parsed, acme.loaded), (1, 0), "the poisoned package reparses");
    assert_eq!(acme.disposition, "parsed (artifact miss)");
    let root_pkg = package(&warm, "__root__");
    assert_eq!((root_pkg.parsed, root_pkg.loaded), (0, 1), "the intact package still loads");
    let cold_canon = canon(cold.findings);
    assert_eq!(canon(warm.findings), cold_canon, "a miss changed meaning, not just cost");

    // Same identity, same verdict on the next run: the store kept its
    // published copy, so the degradation is stable rather than flaky.
    let warm_again = run(&tmp.dir, &tmp.dir, &files);
    let acme_again = package(&warm_again, "acme/lib");
    assert_eq!((acme_again.parsed, acme_again.loaded), (1, 0));
    assert_eq!(canon(warm_again.findings), cold_canon);
}

// ---------------------------------------------------------------------------
// Slice B: the walk-skipping oracles.
//
// The fixture is deliberately wider than slice A's: three first-party files in
// ONE package — so the package-granular fingerprint and the file-granular
// content hash disagree, which is exactly the case a per-file row exists for —
// plus the vendor package. `leaf.php` names nothing any other file declares,
// so it is what every scenario expects to see replayed; `app.php` calls
// `helper()`, so it is what every scenario expects to see walked.
// ---------------------------------------------------------------------------

/// Carries a finding of its own — a pure envelope violated transitively
/// through the vendor package — so the replayed blocks are not all empty and
/// the verifier has something to grade.
const SKIP_APP: &str = "<?php\n\
    namespace App;\n\
    #[\\Steins\\Pure]\n\
    function appMain(): string { return \\Acme\\emit(helper(\"x\")); }\n\
    lateBound();\n";

const SKIP_HELPER: &str = "<?php\n\
    namespace App;\n\
    function helper(string $s): string { return strtoupper($s); }\n";

/// Self-contained on purpose: no reference to any name the project declares,
/// and no comment, so its footprint cannot intersect a first-party delta.
const SKIP_LEAF: &str = "<?php\n\
    namespace Leaf;\n\
    function leafOnly(): int { return 41 + 1; }\n";

const SKIP_COMPOSER_JSON: &str =
    r#"{"name": "fixture/skip", "require": {"acme/lib": "^1.0"}, "autoload": {"psr-4": {"App\\": "src/"}}}"#;

/// Write the walk-skipping fixture; returns the files in universe-slot order.
fn write_skip_fixture(root: &Path) -> Vec<PathBuf> {
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("composer.json", SKIP_COMPOSER_JSON);
    write("composer.lock", COMPOSER_LOCK);
    write("src/app.php", SKIP_APP);
    write("src/helper.php", SKIP_HELPER);
    write("src/leaf.php", SKIP_LEAF);
    write("vendor/acme/lib/src/lib.php", VENDOR_LIB);
    vec![
        root.join("src/app.php"),
        root.join("src/helper.php"),
        root.join("src/leaf.php"),
        root.join("vendor/acme/lib/src/lib.php"),
    ]
}

fn rewrite(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
}

/// A first-party edit walks what could have seen it and replays the rest —
/// including the *unchanged files of the changed package*, which is what a
/// per-file content hash buys over the package fingerprint.
#[test]
fn a_first_party_edit_replays_an_unrelated_file_and_walks_the_caller() {
    if !spawn_or_skip("a_first_party_edit_replays_an_unrelated_file_and_walks_the_caller") {
        return;
    }
    let tmp = TempDir::new("skip-first-party");
    let files = write_skip_fixture(&tmp.dir);

    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(cold.report.mode, GenerationMode::Cold);
    assert_eq!(cold.report.walk.replayed, 0, "a cold build replays nothing");
    assert_eq!(cold.report.walk.walked, files.len());

    // Change `helper.php`'s body only: the root package's fingerprint moves,
    // so all three of its files reload — but only helper.php's bytes moved.
    rewrite(
        &files[1],
        "<?php\nnamespace App;\nfunction helper(string $s): string { return strtolower($s); }\n",
    );

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    let (walked, replayed) = (warm.report.walk.walked, warm.report.walk.replayed);
    assert!(replayed > 0, "nothing was replayed: {:#?}", warm.report.notes);
    assert_eq!(walked + replayed, files.len());
    // leaf.php and the untouched vendor file replay; helper.php (changed) and
    // app.php (its footprint names the changed `helper`) walk.
    assert_eq!((walked, replayed), (2, 2), "notes: {:#?}", warm.report.notes);

    let fresh_store = TempDir::new("skip-first-party-fresh");
    let fresh = run(&tmp.dir, &fresh_store.dir, &files);
    assert_eq!(fresh.report.mode, GenerationMode::Cold);
    assert_eq!(
        canon(warm.findings),
        canon(fresh.findings),
        "a replayed file diverged from a fresh cold run"
    );
}

/// The `delta_names` leg: a symbol **added** in one file moves an *untouched*
/// file's absence finding. Nothing about app.php changed — only the name space
/// under it — so a run without this leg would replay app.php's stale finding,
/// which is the zero-FP violation walk skipping is capable of.
#[test]
fn a_symbol_addition_moves_an_untouched_files_absence_finding() {
    if !spawn_or_skip("a_symbol_addition_moves_an_untouched_files_absence_finding") {
        return;
    }
    let tmp = TempDir::new("skip-delta");
    let files = write_skip_fixture(&tmp.dir);

    let cold = run(&tmp.dir, &tmp.dir, &files);
    let claims_absent = |findings: &[Diagnostic]| {
        findings
            .iter()
            .any(|d| d.path.ends_with("app.php") && d.message.contains("lateBound"))
    };
    assert!(claims_absent(&cold.findings), "the tripwire is armed: {:#?}", cold.findings);

    // Define the missing function — in a *different* file, leaving app.php's
    // own bytes untouched.
    rewrite(
        &files[1],
        "<?php\nnamespace App;\nfunction helper(string $s): string { return strtoupper($s); }\nfunction lateBound(): void {}\n",
    );

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    assert!(
        !claims_absent(&warm.findings),
        "the absence finding survived the symbol that refutes it: {:#?}",
        warm.findings
    );
    assert!(warm.report.walk.replayed > 0, "the run degraded to walking everything");

    let fresh_store = TempDir::new("skip-delta-fresh");
    let fresh = run(&tmp.dir, &fresh_store.dir, &files);
    assert_eq!(canon(warm.findings), canon(fresh.findings));
}

/// The delta's OLD side, isolated. A symbol **removed** leaves no call edge
/// behind — nothing resolves to the file any more — so only the old shard's
/// key set can pull the caller back in. Asserted through the walk counters,
/// because "which file walked" is what the leg decides.
#[test]
fn a_removed_symbol_walks_its_caller_through_the_old_shards_names() {
    if !spawn_or_skip("a_removed_symbol_walks_its_caller_through_the_old_shards_names") {
        return;
    }
    let tmp = TempDir::new("skip-delta-old");
    let files = write_skip_fixture(&tmp.dir);
    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(cold.report.mode, GenerationMode::Cold);

    // helper.php keeps nothing: after this, no name resolves to it at all.
    rewrite(&files[1], "<?php\nnamespace App;\n");

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    // helper.php walks because it changed; app.php walks because `f:app\helper`
    // — a key only the OLD shard still carries — is in its footprint.
    assert_eq!(
        (warm.report.walk.walked, warm.report.walk.replayed),
        (2, 2),
        "notes: {:#?}",
        warm.report.notes
    );
    let fresh_store = TempDir::new("skip-delta-old-fresh");
    assert_eq!(canon(warm.findings), canon(run(&tmp.dir, &fresh_store.dir, &files).findings));
}

/// Change is file-level, not semantic: a callee edit that moves only line
/// numbers still walks its caller, and the caller's descent-provenance message
/// follows the callee's new line. This is slice A's vendor-whitespace oracle,
/// re-asserted with skipping active.
#[test]
fn a_line_only_callee_edit_still_walks_the_caller() {
    if !spawn_or_skip("a_line_only_callee_edit_still_walks_the_caller") {
        return;
    }
    let tmp = TempDir::new("skip-provenance");
    let files = write_fixture(&tmp.dir);
    let vendor_path = files[1].to_string_lossy().into_owned();
    let names_origin = |findings: &[Diagnostic], line: u32| {
        findings.iter().any(|d| {
            d.path.ends_with("app.php")
                && d.message.contains("via echo at ")
                && d.message.contains(&format!("{vendor_path} line {line}"))
        })
    };

    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert!(names_origin(&cold.findings, 3), "{:#?}", cold.findings);

    // Whitespace only: every declaration and origin in the callee moves down a
    // line, and nothing means anything different.
    let text = std::fs::read_to_string(&files[1]).unwrap();
    rewrite(&files[1], &text.replacen("<?php\n", "<?php\n\n", 1));

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert!(
        names_origin(&warm.findings, 4),
        "the caller replayed a message naming the callee's old line: {:#?}",
        warm.findings
    );
    let fresh_store = TempDir::new("skip-provenance-fresh");
    assert_eq!(canon(warm.findings), canon(run(&tmp.dir, &fresh_store.dir, &files).findings));
}

/// A whole-universe verdict moving walks everything — priced coarsely on
/// purpose. An `eval` appearing anywhere flips the dam's names-clear bit, which
/// every absence ladder in every file reads.
#[test]
fn a_dam_flip_walks_every_file() {
    if !spawn_or_skip("a_dam_flip_walks_every_file") {
        return;
    }
    let tmp = TempDir::new("skip-dam");
    let files = write_skip_fixture(&tmp.dir);
    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert!(cold.report.generation.is_some());

    // Prove the fixture would otherwise skip: an untouched warm run replays.
    let untouched = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(untouched.report.walk.replayed, files.len(), "the baseline must skip everything");

    rewrite(
        &files[1],
        "<?php\nnamespace App;\nfunction helper(string $s): string { eval($s); return $s; }\n",
    );
    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    assert_eq!(warm.report.walk.replayed, 0, "a dam flip must walk everything");
    assert_eq!(warm.report.walk.walked, files.len());

    let fresh_store = TempDir::new("skip-dam-fresh");
    assert_eq!(canon(warm.findings), canon(run(&tmp.dir, &fresh_store.dir, &files).findings));
}

/// An untouched tree skips every walk — the shape the M5 target is aimed at —
/// and still produces the cold findings byte for byte.
#[test]
fn an_untouched_tree_replays_every_walk() {
    if !spawn_or_skip("an_untouched_tree_replays_every_walk") {
        return;
    }
    let tmp = TempDir::new("skip-no-change");
    let files = write_skip_fixture(&tmp.dir);
    let cold = run(&tmp.dir, &tmp.dir, &files);
    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    assert_eq!(warm.report.walk.walked, 0, "notes: {:#?}", warm.report.notes);
    assert_eq!(warm.report.walk.replayed, files.len());
    assert_eq!(canon(warm.findings), canon(cold.findings));
}

// ---------------------------------------------------------------------------
// Issue #510: the delta is file-granular, not package-granular.
//
// One package, four files, no vendor tree — the ordinary shape of a
// first-party repo, and the shape where a package-granular delta degenerates:
// any edit puts every name the package declares into the delta, so every file
// that names anything at all is affected. `sibling.php` is the discriminator.
// It names `otherFn` — a declaration of its own package, in a file that did
// not move — so a package-granular delta walks it and a file-granular one does
// not.
// ---------------------------------------------------------------------------

const DELTA_APP: &str = "<?php\n\
    namespace App;\n\
    function appMain(): string { return helper(\"x\"); }\n";

const DELTA_HELPER: &str = "<?php\n\
    namespace App;\n\
    function helper(string $s): string { return strtoupper($s); }\n";

/// Names `otherFn`, whose declaration is in another file of this same package
/// and never moves — and `missingHere`, which nothing declares, so the block
/// this file replays carries a real absence finding rather than nothing.
const DELTA_SIBLING: &str = "<?php\n\
    namespace App;\n\
    function siblingMain(): int { missingHere(); return otherFn(); }\n";

const DELTA_OTHER: &str = "<?php\n\
    namespace App;\n\
    function otherFn(): int { return 1; }\n";

const DELTA_COMPOSER_JSON: &str =
    r#"{"name": "fixture/delta", "autoload": {"psr-4": {"App\\": "src/"}}}"#;

const DELTA_COMPOSER_LOCK: &str = r#"{"packages": [], "packages-dev": []}"#;

/// Write the one-package fixture; returns the files in universe-slot order.
fn write_delta_fixture(root: &Path) -> Vec<PathBuf> {
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("composer.json", DELTA_COMPOSER_JSON);
    write("composer.lock", DELTA_COMPOSER_LOCK);
    write("src/app.php", DELTA_APP);
    write("src/helper.php", DELTA_HELPER);
    write("src/other.php", DELTA_OTHER);
    write("src/sibling.php", DELTA_SIBLING);
    vec![
        root.join("src/app.php"),
        root.join("src/helper.php"),
        root.join("src/other.php"),
        root.join("src/sibling.php"),
    ]
}

/// The tightening (issue #510). An edit to `helper.php` walks the file that
/// changed and the file that names it — and **replays the two files of the
/// same package that name only declarations which did not move**. That last
/// clause is what a package-granular delta cannot do: `sibling.php` names
/// `otherFn`, so the whole package's key set pulls it in, and in a one-package
/// project the whole universe with it.
#[test]
fn an_edit_replays_a_sibling_naming_only_unmoved_declarations() {
    if !spawn_or_skip("an_edit_replays_a_sibling_naming_only_unmoved_declarations") {
        return;
    }
    let tmp = TempDir::new("delta-per-file");
    let files = write_delta_fixture(&tmp.dir);
    assert_eq!(files.len(), 4);

    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(cold.report.mode, GenerationMode::Cold);
    assert_eq!(cold.report.packages.len(), 1, "one package is the point of this fixture");
    assert_eq!(cold.report.walk.walked, files.len());

    // The whole universe is one package, so the fingerprint of every file's
    // package moves; only helper.php's bytes do.
    rewrite(
        &files[1],
        "<?php\nnamespace App;\nfunction helper(string $s): string { return strtolower($s); }\n",
    );

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    // helper.php walks because it changed, app.php because `f:app\helper` — a
    // name the changed file sites — is in its footprint. other.php names
    // nothing and sibling.php names only `otherFn` and `missingHere`, neither
    // of which any changed file declares in either generation.
    assert_eq!(
        (warm.report.walk.walked, warm.report.walk.replayed),
        (2, 2),
        "notes: {:#?}",
        warm.report.notes
    );

    let fresh_store = TempDir::new("delta-per-file-fresh");
    assert_eq!(
        canon(warm.findings),
        canon(run(&tmp.dir, &fresh_store.dir, &files).findings),
        "a replayed file diverged from a fresh cold run"
    );
}

/// The other direction: an edit to a file **nothing names** walks that file
/// alone. Under a package-granular delta the edit still moved every name the
/// package declares, so `app.php` was walked for naming `helper` — a
/// declaration in a file that did not move.
#[test]
fn an_edit_nothing_names_walks_only_the_edited_file() {
    if !spawn_or_skip("an_edit_nothing_names_walks_only_the_edited_file") {
        return;
    }
    let tmp = TempDir::new("delta-unnamed");
    let files = write_delta_fixture(&tmp.dir);
    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(cold.report.mode, GenerationMode::Cold);

    // `siblingMain` is called by nothing, so no footprint and no call edge
    // reaches this file.
    rewrite(
        &files[3],
        "<?php\nnamespace App;\nfunction siblingMain(): int { missingHere(); return otherFn() + 1; }\n",
    );

    let warm = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(warm.report.mode, GenerationMode::Warm);
    assert_eq!(
        (warm.report.walk.walked, warm.report.walk.replayed),
        (1, 3),
        "notes: {:#?}",
        warm.report.notes
    );
    let fresh_store = TempDir::new("delta-unnamed-fresh");
    assert_eq!(canon(warm.findings), canon(run(&tmp.dir, &fresh_store.dir, &files).findings));
}

/// The two members that had no site when #510 was filed — the constants and
/// the `class_alias` edges — have one now, because measurement asked. So a
/// file that names an alias and a constant replays through an edit elsewhere
/// in their package, and still walks when the file that *writes* them moves.
#[test]
fn an_alias_and_a_constant_answer_for_the_file_that_writes_them() {
    if !spawn_or_skip("an_alias_and_a_constant_answer_for_the_file_that_writes_them") {
        return;
    }
    let tmp = TempDir::new("delta-sited-aliases");
    let write = |rel: &str, content: &str| {
        let path = tmp.dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("composer.json", DELTA_COMPOSER_JSON);
    write("composer.lock", DELTA_COMPOSER_LOCK);
    write("src/edited.php", "<?php\nnamespace App;\nfunction unrelated(): int { return 1; }\n");
    write(
        "src/legacy.php",
        "<?php\nnamespace App;\nconst SIZE = 2;\nclass_alias('App\\\\Thing', 'App\\\\LegacyThing');\n",
    );
    write("src/thing.php", "<?php\nnamespace App;\nclass Thing { public int $n = 1; }\n");
    write(
        "src/user.php",
        "<?php\nnamespace App;\nfunction useIt(\\App\\LegacyThing $t): int { return $t->n + SIZE; }\n",
    );
    let files: Vec<PathBuf> = ["edited", "legacy", "thing", "user"]
        .iter()
        .map(|name| tmp.dir.join(format!("src/{name}.php")))
        .collect();

    let cold = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(cold.report.mode, GenerationMode::Cold);

    // The positive control: the file that writes the alias and the constant
    // moves, so the file that names them is walked.
    rewrite(
        &files[1],
        "<?php\nnamespace App;\nconst SIZE = 3;\nclass_alias('App\\\\Thing', 'App\\\\LegacyThing');\n",
    );
    let moved = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(
        (moved.report.walk.walked, moved.report.walk.replayed),
        (2, 2),
        "notes: {:#?}",
        moved.report.notes
    );

    // And the tightening: an edit elsewhere in the same package leaves that
    // file replaying. Site-less, both the alias ends and `k:App\SIZE` would be
    // in every edit's delta and user.php would walk with them.
    rewrite(&files[0], "<?php\nnamespace App;\nfunction unrelated(): int { return 2; }\n");
    let elsewhere = run(&tmp.dir, &tmp.dir, &files);
    assert_eq!(
        (elsewhere.report.walk.walked, elsewhere.report.walk.replayed),
        (1, 3),
        "notes: {:#?}",
        elsewhere.report.notes
    );

    let fresh_store = TempDir::new("delta-sited-aliases-fresh");
    assert_eq!(canon(elsewhere.findings), canon(run(&tmp.dir, &fresh_store.dir, &files).findings));
}

/// The tightened leg under the verifier: both edits re-run with every file
/// walked anyway, every would-be skip graded against its fresh walk. A
/// tightening is only worth having if this is what it grades to.
#[test]
fn the_paranoid_verifier_grades_the_tightened_delta_clean() {
    if !spawn_or_skip("the_paranoid_verifier_grades_the_tightened_delta_clean") {
        return;
    }
    let scenarios: [(&str, usize, &str); 3] = [
        ("no-change", 1, DELTA_HELPER),
        (
            "named-callee",
            1,
            "<?php\nnamespace App;\nfunction helper(string $s): string { return strtolower($s); }\n",
        ),
        (
            "unnamed-file",
            3,
            "<?php\nnamespace App;\nfunction siblingMain(): int { missingHere(); return otherFn() + 1; }\n",
        ),
    ];
    for (tag, slot, content) in scenarios {
        let tmp = TempDir::new(&format!("delta-paranoid-{tag}"));
        let files = write_delta_fixture(&tmp.dir);
        let cold = run_paranoid(&tmp.dir, &tmp.dir, &files);
        assert!(cold.report.walk.divergences.is_empty(), "{tag}: cold");

        rewrite(&files[slot], content);
        let warm = run_paranoid(&tmp.dir, &tmp.dir, &files);
        assert_eq!(warm.report.mode, GenerationMode::Warm, "{tag}");
        assert_eq!(warm.report.walk.walked, files.len(), "{tag}: paranoid walks everything");
        assert!(
            warm.report.walk.divergences.is_empty(),
            "{tag}: the tightened delta let a stale block through: {:#?}",
            warm.report.walk.divergences
        );
        assert!(warm.report.walk.would_skip > 0, "{tag}: nothing was graded");
        let fresh_store = TempDir::new(&format!("delta-paranoid-{tag}-fresh"));
        assert_eq!(
            canon(warm.findings),
            canon(run(&tmp.dir, &fresh_store.dir, &files).findings),
            "{tag}"
        );
    }
}

// ---------------------------------------------------------------------------
// The paranoid verifier, over the same fixtures.
// ---------------------------------------------------------------------------

/// Every skipping scenario re-run with the verifier on: every file is walked
/// anyway, every would-be skip is graded against its fresh walk, and no
/// divergence is found. This is the instrument a corpus run uses, exercised at
/// fixture scale in CI — and the assertion that it *graded* something is as
/// load bearing as the assertion that it found nothing.
#[test]
fn the_paranoid_verifier_grades_every_scenario_clean() {
    if !spawn_or_skip("the_paranoid_verifier_grades_every_scenario_clean") {
        return;
    }
    let scenarios: Vec<(&str, Option<(usize, &str)>)> = vec![
        ("no-change", None),
        (
            "first-party-edit",
            Some((1, "<?php\nnamespace App;\nfunction helper(string $s): string { return strtolower($s); }\n")),
        ),
        ("removed-symbol", Some((1, "<?php\nnamespace App;\n"))),
        (
            "line-shift",
            Some((1, "<?php\n\nnamespace App;\nfunction helper(string $s): string { return strtoupper($s); }\n")),
        ),
        (
            "new-symbol",
            Some((2, "<?php\nnamespace Leaf;\nfunction leafOnly(): int { return 41 + 1; }\nfunction extra(): int { return 1; }\n")),
        ),
        (
            "dam-flip",
            Some((1, "<?php\nnamespace App;\nfunction helper(string $s): string { eval($s); return $s; }\n")),
        ),
    ];
    for (tag, edit) in scenarios {
        let tmp = TempDir::new(&format!("paranoid-{tag}"));
        let files = write_skip_fixture(&tmp.dir);
        let cold = run_paranoid(&tmp.dir, &tmp.dir, &files);
        assert!(cold.report.walk.paranoid, "{tag}: the verifier did not run");
        assert!(
            cold.report.walk.divergences.is_empty(),
            "{tag}: cold: {:#?}",
            cold.report.walk.divergences
        );

        if let Some((slot, content)) = edit {
            rewrite(&files[slot], content);
        }
        let warm = run_paranoid(&tmp.dir, &tmp.dir, &files);
        assert_eq!(warm.report.mode, GenerationMode::Warm, "{tag}");
        assert_eq!(warm.report.walk.walked, files.len(), "{tag}: paranoid walks everything");
        assert_eq!(warm.report.walk.replayed, 0, "{tag}: paranoid keeps the walked answer");
        assert!(
            warm.report.walk.divergences.is_empty(),
            "{tag}: the affected set let a stale block through: {:#?}",
            warm.report.walk.divergences
        );
        // A verifier that graded nothing proved nothing about this scenario.
        // The dam flip is the one shape where zero is the right answer: the
        // whole-universe leg refuses every row before the affected set is
        // consulted at all.
        if tag == "dam-flip" {
            assert_eq!(warm.report.walk.would_skip, 0, "{tag}");
        } else {
            assert!(warm.report.walk.would_skip > 0, "{tag}: nothing was graded");
        }

        // And a paranoid run's findings are still a cold run's findings.
        let fresh_store = TempDir::new(&format!("paranoid-{tag}-fresh"));
        assert_eq!(
            canon(warm.findings),
            canon(run(&tmp.dir, &fresh_store.dir, &files).findings),
            "{tag}"
        );
    }
}

/// The verifier catches what it exists to catch. A persisted block doctored to
/// carry a message the walk does not produce is exactly the shape a missing
/// affected-set leg would have; paranoid mode names the file and the finding
/// instead of shipping it, and the run's own findings stay the walked ones.
#[test]
fn the_paranoid_verifier_names_a_planted_divergence() {
    if !spawn_or_skip("the_paranoid_verifier_names_a_planted_divergence") {
        return;
    }
    let tmp = TempDir::new("paranoid-planted");
    let files = write_skip_fixture(&tmp.dir);
    let cold = run(&tmp.dir, &tmp.dir, &files);
    let hex = cold.report.generation.clone().expect("the cold build publishes");
    assert!(!cold.findings.is_empty(), "the fixture must produce a finding to doctor");

    // The artifact is a container of sections and the summaries payload is
    // JSON inside one of them, so planting a divergence is a byte patch of a
    // message string — no writer needed, and nothing else in the artifact
    // moves. The sections are written in order and `summaries` is last, so the
    // LAST occurrence of a message word is the persisted block's copy; an
    // earlier one would be the trace or contract payload, where a patch would
    // move the walk and the replay together and prove nothing. The word comes
    // from app.php's `call.undefined-function` message — a *walk* finding, and
    // so one a block actually holds (the effect and throw passes are
    // whole-universe and are never persisted).
    let artifact = tmp.dir.join(".steins").join("gen").join(&hex).join("__root__.pkg");
    let bytes = std::fs::read(&artifact).expect("the root artifact is on disk");
    let needle = b"lateBound";
    let at = bytes
        .windows(needle.len())
        .rposition(|w| w == needle)
        .expect("the persisted blocks carry the absence finding's message");
    let mut doctored = bytes.clone();
    doctored[at..at + needle.len()].copy_from_slice(b"lateBounD");
    std::fs::write(&artifact, doctored).unwrap();

    let warm = run_paranoid(&tmp.dir, &tmp.dir, &files);
    assert!(
        !warm.report.walk.divergences.is_empty(),
        "the planted divergence went unnoticed: {:#?}",
        warm.report.notes
    );
    let named = warm.report.walk.divergences[0].to_string();
    assert!(named.contains("app.php"), "the divergence must name the file: {named}");
    assert!(named.contains("lateBoun"), "the divergence must name the finding: {named}");
    // The walked answer is what the run reports, so a poisoned block is a loud
    // note and never a wrong finding.
    let fresh_store = TempDir::new("paranoid-planted-fresh");
    assert_eq!(canon(warm.findings), canon(run(&tmp.dir, &fresh_store.dir, &files).findings));
}
