//! `steins effect-diff` (issue #69): capture per-function effect summaries into
//! their own sidecar file ([`crate::effect_baseline`], untouched by `check`),
//! or diff the current run against a captured past. Informational: exits 0
//! even with changes; only usage/file errors exit 2.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::effect_summaries_project;

use crate::config::allow_list_from_disk;
use crate::project::{collect_files, load_plugins, reject_missing_paths, resolve_layout};
use crate::{Format, baseline, effect_baseline};

/// `steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json]
/// <paths...>` (issue #69) — capture per-function effect summaries, or diff
/// against a captured past. Own sidecar file, untouched by `check`.
/// Informational: exits 0 even with changes; only usage/file errors exit 2.
pub(crate) fn run_effect_diff(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut set_baseline = false;
    let mut baseline_path: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--set-baseline" => {
                set_baseline = true;
                i += 1;
            }
            "--baseline" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --baseline requires a path argument");
                    return ExitCode::from(2);
                };
                baseline_path = Some(value.clone());
                i += 2;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                match value.as_str() {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    other => {
                        errln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other if other.starts_with('-') => {
                errln!("steins: unknown flag `{other}` for effect-diff");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    if paths.is_empty() {
        errln!("steins: no paths given");
        return ExitCode::from(2);
    }
    if let Err(code) = reject_missing_paths(&paths) {
        return code;
    }

    let files = collect_files(&paths);

    // No sidecar, no folder: effect summaries are a pure static fixpoint, so this
    // command never needs `php` and takes no `--no-php`.
    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::new();
    for file_path in &files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                errln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        inputs.push(SourceFile::new(&db, file_path.to_string_lossy().into_owned(), text));
    }
    let layout = resolve_layout(&paths);
    let plugins = load_plugins(&layout, allow_list_from_disk().as_deref());
    let project = Project::new(&db, inputs.clone(), layout.clone(), plugins);

    // Sidecar file, resolved like the diagnostic baseline's: `--baseline` path
    // else the default name. Entry paths relative to its directory (ADR-0022).
    let file = PathBuf::from(baseline_path.as_deref().unwrap_or(effect_baseline::DEFAULT_FILE));
    let dir = baseline::base_dir(&file);
    let current = capture_effect_entries(&db, project, &inputs, &layout, &dir);

    if set_baseline {
        let n = current.len();
        return match std::fs::write(&file, effect_baseline::render(current)) {
            Ok(()) => {
                errln!("steins: wrote {n} effect summaries to {}", file.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                errln!("steins: cannot write effect baseline {}: {e}", file.display());
                ExitCode::from(2)
            }
        };
    }

    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            errln!(
                "steins: cannot read effect baseline {}: {e} (run --set-baseline to capture one)",
                file.display()
            );
            return ExitCode::from(2);
        }
    };
    let doc = match effect_baseline::parse(&text) {
        Ok(d) => d,
        Err(e) => {
            errln!("steins: {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };

    let report = effect_baseline::diff(&doc.functions, &current);
    match format {
        Format::Text => {
            for event in &report.events {
                outln!("{}", event.line());
            }
            if let Some(footer) = effect_baseline::footer(&report) {
                outln!("{footer}");
            }
        }
        Format::Json => print_effect_diff_json(&report),
    }
    ExitCode::SUCCESS
}

/// The current run's effect summaries as baseline entries, one per non-vendor
/// function/method (vendor excluded, ADR-0015).
fn capture_effect_entries(
    db: &SteinsDatabase,
    project: Project,
    inputs: &[SourceFile],
    layout: &ProjectLayout,
    dir: &Path,
) -> Vec<effect_baseline::Entry> {
    let mut entries = Vec::new();
    for &input in inputs {
        let path = input.path(db).to_owned();
        if layout.is_vendor(&path) {
            continue;
        }
        let rel = baseline::relativize(dir, &path);
        // One whole-project fixpoint per file (same cost `annotate` pays per call).
        for s in effect_summaries_project(db, project, input) {
            entries.push(effect_baseline::Entry {
                file: rel.clone(),
                symbol: s.qualified,
                proven: s.labels,
                declared: s.declared,
                exhaustive: s.exhaustive,
            });
        }
    }
    entries
}

/// `effect-diff --format json`: `events` array plus footer counts, for CI.
fn print_effect_diff_json(report: &effect_baseline::Diff) {
    let events: Vec<serde_json::Value> = report
        .events
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.file,
                "symbol": e.symbol,
                "category": e.category.as_str(),
                "label": e.label,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "events": events,
        "compared": report.compared,
        "not_in_baseline": report.not_in_baseline,
        "no_longer_present": report.no_longer_present,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => outln!("{s}"),
        Err(e) => errln!("steins: failed to serialize json: {e}"),
    }
}
