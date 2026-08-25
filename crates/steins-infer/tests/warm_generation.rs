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
    vec![root.join("src/app.php"), root.join("vendor/acme/lib/src/lib.php")]
}

/// One full generation run over the fixture: the CLI's boundary resolution
/// (layout, partition), the orchestrator, PHP on.
fn run(root: &Path, store_root: &Path, files: &[PathBuf]) -> GenerationOutcome {
    let layout = composer::discover(&[root.to_path_buf()], root);
    let partition = steins_db::partition::discover(&layout);
    let member = |name: &str| {
        partition
            .universe()
            .packages()
            .iter()
            .find(|p| p.name.as_str() == name)
            .map(|p| p.kind)
    };
    assert_eq!(member("acme/lib"), Some(PackageKind::Vendor), "the lock classifies the package");
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
