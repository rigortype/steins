//! The `steins` binary.
//!
//! Only `check` exists for this milestone (ADR-0020 documents the eventual
//! six-command surface; the others are deliberately NOT stubbed). `check` walks
//! `.php` files, runs the salsa pipeline, prints proof-layer diagnostics, and
//! exits 1 if any finding was reported.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{SourceFile, SteinsDatabase};
use steins_infer::{Diagnostic, SOUND_SUBSET_NOTICE, SidecarFolder, check_file};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("check") => run_check(&args[1..]),
        Some(other) => {
            eprintln!("steins: unknown command `{other}` (only `check` is available)");
            ExitCode::from(2)
        }
        None => {
            eprintln!("usage: steins check [--format text|json] [--no-php] <paths...>");
            ExitCode::from(2)
        }
    }
}

fn run_check(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut no_php = false;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-php" => {
                no_php = true;
                i += 1;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                match value.as_str() {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    other => {
                        eprintln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    if paths.is_empty() {
        eprintln!("steins: no paths given");
        return ExitCode::from(2);
    }

    let mut files = Vec::new();
    for p in &paths {
        collect_php_files(Path::new(p), &mut files);
    }
    files.sort();
    files.dedup();

    // Coverage posture (ADR-0004): with `--no-php` the run is the sound subset,
    // surfaced up front as a startup notice. Without the flag we fold via a
    // lazily-spawned sidecar (spawned only on the first foldable call); if `php`
    // turns out to be unavailable, the folder emits the same notice itself.
    if no_php {
        eprintln!("{SOUND_SUBSET_NOTICE}");
    }

    let db = SteinsDatabase::default();
    // One folder for the whole run: it owns the resident sidecar and the fold
    // memo, so a repeated call across files never re-spawns or re-folds.
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };
    let mut findings: Vec<Diagnostic> = Vec::new();
    for file_path in &files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                eprintln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        let input = SourceFile::new(&db, file_path.to_string_lossy().into_owned(), text);
        findings.extend(check_file(&db, input, &mut folder).iter().cloned());
    }

    match format {
        Format::Text => print_text(&findings),
        Format::Json => print_json(&findings),
    }

    if findings.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn print_text(findings: &[Diagnostic]) {
    for d in findings {
        println!("{}:{}:{}: error[{}]: {}", d.path, d.line, d.column, d.id, d.message);
    }
}

fn print_json(findings: &[Diagnostic]) {
    let array: Vec<serde_json::Value> = findings
        .iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id,
                "path": d.path,
                "line": d.line,
                "column": d.column,
                "message": d.message,
            })
        })
        .collect();
    match serde_json::to_string_pretty(&array) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("steins: failed to serialize json: {e}"),
    }
}

/// Recursively collect `.php` files under `path` (or `path` itself if it is a
/// `.php` file).
fn collect_php_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else { return };
        for entry in entries.flatten() {
            collect_php_files(&entry.path(), out);
        }
    } else if path.extension().is_some_and(|e| e == "php") {
        out.push(path.to_path_buf());
    }
}
