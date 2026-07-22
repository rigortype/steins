//! `fp-gate`: run the full proof-layer pipeline over the pinned corpus.
//!
//! ADR-0013: one proof-layer diagnostic on working code is a release blocker,
//! so this gate exits nonzero the moment any diagnostic fires on a clean-parsing
//! file — that is exactly the triage material we want surfaced, never hidden.
//!
//! Whole-project mode (ADR-0009/0015): each corpus package is analyzed as ONE
//! project (a single salsa DB holding all its `.php` files), so cross-file
//! calls, class chains, and effects resolve. Packages run in parallel (rayon);
//! within a package the analysis is one project run. Files that fail to parse
//! are still included in the project (so resolution stays complete — a partial
//! tree can only *silence*, never add a false positive), but any diagnostic that
//! lands in a parse-error file is excluded from the gate count.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use steins_db::{Project, SourceFile, SteinsDatabase, parse};
use steins_infer::{Diagnostic, SidecarFolder, check_project, is_vendor_path};

use crate::corpus::{PACKAGES, checkout_dir, collect_php_files, read_lock, repo_root};
use crate::corpus_local::{self, LocalProject};

/// Per-project result of the gate run (a pinned corpus package or an unpinned
/// local project). `diagnostics` holds only the findings that count against the
/// gate; for local projects, vendor findings are excluded (ADR-0015) and tallied
/// separately in `vendor_suppressed`.
struct PackageReport {
    name: String,
    /// The pinned release tag, or empty for a local (unpinned) project.
    tag: String,
    /// A live working tree injected via `corpus.local.toml` (ADR-0013 §4).
    local: bool,
    file_count: usize,
    parse_error_files: Vec<String>,
    diagnostics: Vec<Diagnostic>,
    /// NEW `phpdoc.*` declared-contract findings, held separately: in this run
    /// they are **measurement mode** (ADR-0030 relation #1 landing) — reported and
    /// counted per package but excluded from the red/green verdict.
    phpdoc: Vec<Diagnostic>,
    /// Vendor findings suppressed from the gate count (local projects only).
    vendor_suppressed: usize,
    elapsed: Duration,
}

/// Whether a diagnostic is one of the NEW measurement-mode `phpdoc.*` ids.
fn is_phpdoc(d: &Diagnostic) -> bool {
    d.id.starts_with("phpdoc.")
}

/// Entry point for `cargo xtask fp-gate`. Returns `true` if the gate is GREEN
/// (no diagnostics on clean code).
pub fn run() -> Result<bool, String> {
    let lock = read_lock();
    if lock.packages.is_empty() {
        return Err("corpus.lock.toml is empty — run `cargo xtask corpus-sync` first".to_owned());
    }
    let root = repo_root();

    // One project per package; packages analyzed in parallel.
    let reports: Result<Vec<PackageReport>, String> = PACKAGES
        .par_iter()
        .map(|pkg| {
            let dir = checkout_dir(pkg.name);
            if !dir.is_dir() {
                return Err(format!(
                    "{} not checked out at {} — run `cargo xtask corpus-sync`",
                    pkg.name,
                    dir.display()
                ));
            }
            let tag = lock.get(pkg.name).map(|e| e.tag.clone()).unwrap_or_default();
            Ok(analyze_package(pkg.name, &tag, &dir, &root))
        })
        .collect();
    let mut reports = reports?;
    // Keep a stable (canonical corpus) order for the report.
    reports.sort_by_key(|r| PACKAGES.iter().position(|p| p.name == r.name).unwrap_or(usize::MAX));

    // Private-corpus injection point (ADR-0013 §4): each `[[project]]` in the
    // optional (gitignored) `corpus.local.toml` is analyzed as one project, in
    // parallel like the packages, applying the CLI's vendor default — vendor
    // files are indexed for inference but their findings don't count.
    let locals = corpus_local::read_local()?;
    let mut local_reports: Vec<PackageReport> =
        locals.par_iter().map(analyze_local).collect();
    local_reports.sort_by(|a, b| a.name.cmp(&b.name));

    print_report(&reports, &local_reports);

    // RED on any counted finding — package diagnostics plus local *non-vendor*
    // diagnostics (vendor findings never gate; ADR-0015).
    let total_diags: usize = reports.iter().map(|r| r.diagnostics.len()).sum::<usize>()
        + local_reports.iter().map(|r| r.diagnostics.len()).sum::<usize>();
    Ok(total_diags == 0)
}

// Default posture (ADR-0004): the gate folds via the PHP sidecar. Each rayon
// worker owns one resident `SidecarFolder` (thread-local), reused across the
// packages that worker analyzes.
thread_local! {
    static FOLDER: RefCell<SidecarFolder> = RefCell::new(SidecarFolder::enabled());
}

/// Analyze one package as a single project and time it.
fn analyze_package(name: &str, tag: &str, dir: &Path, root: &Path) -> PackageReport {
    let start = Instant::now();

    let mut files = Vec::new();
    collect_php_files(dir, &mut files);
    files.sort();

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(), // unreadable → empty (parses clean, contributes nothing)
        };
        inputs.push(SourceFile::new(&db, rel, text));
    }

    // Identify parse-error files (their diagnostics are excluded from the count).
    let mut parse_error_files = Vec::new();
    for &input in &inputs {
        if !parse(&db, input).parse_errors().is_empty() {
            parse_error_files.push(input.path(&db).to_owned());
        }
    }
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let project = Project::new(&db, inputs);
    let mut diags: Vec<Diagnostic> = FOLDER.with(|f| check_project(&db, project, &mut *f.borrow_mut()));
    diags.retain(|d| !parse_err_set.contains(d.path.as_str()));
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    // Measurement-mode split: `phpdoc.*` findings are reported + counted but do
    // not gate this run.
    let phpdoc: Vec<Diagnostic> = diags.iter().filter(|d| is_phpdoc(d)).cloned().collect();
    diags.retain(|d| !is_phpdoc(d));

    PackageReport {
        name: name.to_owned(),
        tag: tag.to_owned(),
        local: false,
        file_count: files.len(),
        parse_error_files,
        diagnostics: diags,
        phpdoc,
        vendor_suppressed: 0,
        elapsed: start.elapsed(),
    }
}

/// Analyze one local project (ADR-0013 §4) as a single project. Paths are made
/// project-relative so the `vendor/` predicate and the report read cleanly.
/// Vendor findings are split out of the gate count (ADR-0015).
fn analyze_local(proj: &LocalProject) -> PackageReport {
    let start = Instant::now();
    let root = Path::new(&proj.path);

    let files = corpus_local::collect_php_files(root, &proj.exclude);

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        // Project-relative path (falls back to the full path if `f` is not under
        // `root`, which cannot normally happen). Keeps `vendor/` detection and
        // the printed rows readable.
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        };
        inputs.push(SourceFile::new(&db, rel, text));
    }

    let mut parse_error_files = Vec::new();
    for &input in &inputs {
        if !parse(&db, input).parse_errors().is_empty() {
            parse_error_files.push(input.path(&db).to_owned());
        }
    }
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let project = Project::new(&db, inputs);
    let mut diags: Vec<Diagnostic> = FOLDER.with(|f| check_project(&db, project, &mut *f.borrow_mut()));
    diags.retain(|d| !parse_err_set.contains(d.path.as_str()));

    // Vendor default (ADR-0015): vendor code was fully indexed and inferred, but
    // its findings do not count against the gate. Split them out.
    let before = diags.len();
    diags.retain(|d| !is_vendor_path(&d.path));
    let vendor_suppressed = before - diags.len();
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    // Measurement-mode split (first-party only; vendor already removed above).
    let phpdoc: Vec<Diagnostic> = diags.iter().filter(|d| is_phpdoc(d)).cloned().collect();
    diags.retain(|d| !is_phpdoc(d));

    PackageReport {
        name: proj.name.clone(),
        tag: String::new(),
        local: true,
        file_count: files.len(),
        parse_error_files,
        diagnostics: diags,
        phpdoc,
        vendor_suppressed,
        elapsed: start.elapsed(),
    }
}

fn print_report(reports: &[PackageReport], local_reports: &[PackageReport]) {
    println!("\n=== fp-gate: per-package findings ===\n");
    if !local_reports.is_empty() {
        println!(
            "note: {} local project(s) are UNPINNED live working trees (corpus.local.toml, \
             ADR-0013 §4); their vendor findings are indexed for inference but do not gate \
             (ADR-0015).\n",
            local_reports.len()
        );
    }
    // Packages first, then local projects, in the per-project findings section.
    for r in reports.iter().chain(local_reports.iter()) {
        let ident = if r.local {
            format!("{} (local)", r.name)
        } else {
            format!("{} @ {}", r.name, r.tag)
        };
        let vendor_note = if r.local {
            format!(", {} vendor-suppressed", r.vendor_suppressed)
        } else {
            String::new()
        };
        println!(
            "{ident} — {} files, {} parse-error files, {} diagnostics{vendor_note} ({:.2}s)",
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len(),
            r.elapsed.as_secs_f64()
        );
        if !r.parse_error_files.is_empty() {
            for sample in r.parse_error_files.iter().take(5) {
                println!("    parse-error: {sample}");
            }
            if r.parse_error_files.len() > 5 {
                println!("    … and {} more", r.parse_error_files.len() - 5);
            }
        }
        for d in &r.diagnostics {
            println!("    DIAGNOSTIC {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
        }
        if !r.phpdoc.is_empty() {
            println!("    [measurement mode] {} phpdoc.* finding(s) (excluded from red/green):", r.phpdoc.len());
            for d in &r.phpdoc {
                println!("    PHPDOC {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
            }
        }
    }

    // Measurement-mode summary: the NEW `phpdoc.*` declared-contract ids, counted
    // per package but excluded from the gate verdict (ADR-0030 landing).
    let total_phpdoc: usize = reports.iter().chain(local_reports.iter()).map(|r| r.phpdoc.len()).sum();
    println!("\n=== phpdoc.* measurement mode (does NOT gate) ===\n");
    for r in reports.iter().chain(local_reports.iter()) {
        if r.phpdoc.is_empty() {
            continue;
        }
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        let (params, returns) = r
            .phpdoc
            .iter()
            .fold((0usize, 0usize), |(p, ret), d| match d.id {
                "phpdoc.param-mismatch" => (p + 1, ret),
                "phpdoc.return-mismatch" => (p, ret + 1),
                _ => (p, ret),
            });
        println!("{label} — {} phpdoc.* ({params} param, {returns} return)", r.phpdoc.len());
    }
    println!("phpdoc.* TOTAL: {total_phpdoc}");

    // Summary table: packages and local projects share one table; local rows are
    // marked `(local)`.
    let rows = || reports.iter().chain(local_reports.iter());
    let name_w = rows()
        .map(|r| r.name.len() + if r.local { " (local)".len() } else { 0 })
        .max()
        .unwrap_or(4)
        .max(7);
    println!("\n=== summary ===\n");
    println!(
        "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8}",
        "package", "files", "parse-errors", "diagnostics", "time(s)"
    );
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8));
    let (mut tf, mut tp, mut td) = (0usize, 0usize, 0usize);
    let mut ttime = 0.0f64;
    for r in rows() {
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        println!(
            "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}",
            label,
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len(),
            r.elapsed.as_secs_f64()
        );
        tf += r.file_count;
        tp += r.parse_error_files.len();
        td += r.diagnostics.len();
        ttime += r.elapsed.as_secs_f64();
    }
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8));
    println!("{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}", "TOTAL", tf, tp, td, ttime);

    println!();
    if td == 0 {
        println!("GATE GREEN — no proof-layer diagnostics on clean-parsing corpus code.");
    } else {
        println!(
            "GATE RED — {td} proof-layer diagnostic(s) on clean code. Human FP triage required (ADR-0013)."
        );
    }
}

#[cfg(test)]
mod tests {
    use steins_infer::is_vendor_path;

    // The vendor-path predicate (ADR-0015) drives the gate's local-project vendor
    // split; verify its component-boundary behavior here where the gate uses it.
    #[test]
    fn vendor_predicate_matches_directory_components_only() {
        // A `vendor/` component — top-level or nested — is vendor.
        assert!(is_vendor_path("vendor/foo/Bar.php"));
        assert!(is_vendor_path("src/vendor/foo/Bar.php"));
        assert!(is_vendor_path("/abs/mono/vendor/pkg/lib.php"));
        assert!(is_vendor_path("a\\vendor\\b.php")); // Windows separators
        // First-party paths are not vendor — including look-alikes.
        assert!(!is_vendor_path("src/app/Service.php"));
        assert!(!is_vendor_path("vendor_proj/app/Service.php")); // sibling, not a component
        assert!(!is_vendor_path("src/vendored/x.php"));
        assert!(!is_vendor_path("app/vendor.php")); // filename, not a directory
    }
}
