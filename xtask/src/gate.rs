//! `fp-gate`: run the full proof-layer pipeline over the pinned corpus.
//!
//! ADR-0013: one proof-layer diagnostic on working code is a release blocker,
//! so this gate exits nonzero the moment any diagnostic fires on a clean-parsing
//! file — that is exactly the triage material we want surfaced, never hidden.
//! Files that fail to parse are intentionally-broken fixtures (or unsupported
//! syntax); they are reported as statistics, never as gate failures, and their
//! call sites are excluded from the diagnostic count.

use std::cell::RefCell;
use std::path::Path;

use rayon::prelude::*;
use steins_db::{SourceFile, SteinsDatabase, parse};
use steins_infer::{Diagnostic, SidecarFolder, check_file};

use crate::corpus::{PACKAGES, checkout_dir, collect_php_files, read_lock, repo_root};

/// Per-package result of the gate run.
struct PackageReport {
    name: String,
    tag: String,
    file_count: usize,
    parse_error_files: Vec<String>,
    diagnostics: Vec<Diagnostic>,
}

/// Analysis outcome for a single file.
enum FileOutcome {
    Clean(Vec<Diagnostic>),
    ParseError(String), // path (repo-relative) of the un-parseable file
}

/// Entry point for `cargo xtask fp-gate`. Returns `true` if the gate is GREEN
/// (no diagnostics on clean code).
pub fn run() -> Result<bool, String> {
    let lock = read_lock();
    if lock.packages.is_empty() {
        return Err("corpus.lock.toml is empty — run `cargo xtask corpus-sync` first".to_owned());
    }
    let root = repo_root();

    let mut reports = Vec::new();
    for pkg in PACKAGES {
        let dir = checkout_dir(pkg.name);
        if !dir.is_dir() {
            return Err(format!(
                "{} not checked out at {} — run `cargo xtask corpus-sync`",
                pkg.name,
                dir.display()
            ));
        }
        let tag = lock.get(pkg.name).map(|e| e.tag.clone()).unwrap_or_default();

        let mut files = Vec::new();
        collect_php_files(&dir, &mut files);
        files.sort();

        let outcomes: Vec<FileOutcome> =
            files.par_iter().map(|f| analyze_file(f, &root)).collect();

        let mut parse_error_files = Vec::new();
        let mut diags = Vec::new();
        for outcome in outcomes {
            match outcome {
                FileOutcome::Clean(mut d) => diags.append(&mut d),
                FileOutcome::ParseError(p) => parse_error_files.push(p),
            }
        }
        diags.sort_by(|a, b| {
            (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column))
        });

        reports.push(PackageReport {
            name: pkg.name.to_owned(),
            tag,
            file_count: files.len(),
            parse_error_files,
            diagnostics: diags,
        });
    }

    print_report(&reports);
    let total_diags: usize = reports.iter().map(|r| r.diagnostics.len()).sum();
    Ok(total_diags == 0)
}

// Default posture (ADR-0004): the gate folds via the PHP sidecar. Each rayon
// worker owns one resident `SidecarFolder` (thread-local), so the corpus's few
// foldable calls reuse a handful of `php` processes rather than spawning per
// file, and the per-run fold memo is shared across every file that thread sees.
thread_local! {
    static FOLDER: RefCell<SidecarFolder> = RefCell::new(SidecarFolder::enabled());
}

/// Run the full pipeline on one file. A fresh database per file keeps the rayon
/// workers independent; folding runs outside salsa through the thread-local
/// sidecar folder (see [`check_file`]).
fn analyze_file(path: &Path, root: &Path) -> FileOutcome {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned();
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return FileOutcome::ParseError(rel), // unreadable → treat as non-gating
    };

    let db = SteinsDatabase::default();
    let input = SourceFile::new(&db, rel.clone(), text);
    let tree = parse(&db, input);
    if !tree.parse_errors().is_empty() {
        return FileOutcome::ParseError(rel);
    }
    let diags = FOLDER.with(|f| check_file(&db, input, &mut *f.borrow_mut()));
    FileOutcome::Clean(diags)
}

fn print_report(reports: &[PackageReport]) {
    println!("\n=== fp-gate: per-package findings ===\n");
    for r in reports {
        println!(
            "{} @ {} — {} files, {} parse-error files, {} diagnostics",
            r.name,
            r.tag,
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len()
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
            println!(
                "    DIAGNOSTIC {}:{}:{} [{}] {}",
                d.path, d.line, d.column, d.id, d.message
            );
        }
    }

    // Summary table.
    let name_w = reports.iter().map(|r| r.name.len()).max().unwrap_or(4).max(7);
    println!("\n=== summary ===\n");
    println!("{:<name_w$}  {:>6}  {:>12}  {:>11}", "package", "files", "parse-errors", "diagnostics");
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11));
    let (mut tf, mut tp, mut td) = (0usize, 0usize, 0usize);
    for r in reports {
        println!(
            "{:<name_w$}  {:>6}  {:>12}  {:>11}",
            r.name,
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len()
        );
        tf += r.file_count;
        tp += r.parse_error_files.len();
        td += r.diagnostics.len();
    }
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11));
    println!("{:<name_w$}  {:>6}  {:>12}  {:>11}", "TOTAL", tf, tp, td);

    println!();
    if td == 0 {
        println!("GATE GREEN — no proof-layer diagnostics on clean-parsing corpus code.");
    } else {
        println!(
            "GATE RED — {td} proof-layer diagnostic(s) on clean code. Human FP triage required (ADR-0013)."
        );
    }
}
