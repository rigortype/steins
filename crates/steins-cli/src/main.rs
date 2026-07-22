//! The `steins` binary.
//!
//! Two commands exist for this milestone (ADR-0020 documents the eventual
//! six-command surface; the rest are deliberately NOT stubbed). `check` walks
//! `.php` files, runs the salsa pipeline, prints proof-layer diagnostics, and
//! exits 1 if any finding was reported. `annotate` reprints a single file with a
//! right-margin column of *proven* inferred facts (the Rigor-style display).

mod baseline;
mod sha256;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{Project, SourceFile, SteinsDatabase, parse as parse_tree};
use steins_infer::{
    Diagnostic, LineFact, SOUND_SUBSET_NOTICE, SidecarFolder, annotate_file, annotate_project,
    apply_inline_ignores, check_project,
};
use steins_syntax::SourceTree;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("check") => run_check(&args[1..]),
        Some("annotate") => run_annotate(&args[1..]),
        Some(other) => {
            eprintln!("steins: unknown command `{other}` (available: check, annotate)");
            ExitCode::from(2)
        }
        None => {
            eprintln!(
                "usage: steins check [--format text|json] [--no-php] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>"
            );
            eprintln!("       steins annotate [--no-php] <file.php>");
            ExitCode::from(2)
        }
    }
}

fn run_check(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut no_php = false;
    let mut set_baseline = false;
    let mut ignore_baseline = false;
    let mut baseline_path: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-php" => {
                no_php = true;
                i += 1;
            }
            "--set-baseline" => {
                set_baseline = true;
                i += 1;
            }
            "--ignore-baseline" => {
                ignore_baseline = true;
                i += 1;
            }
            "--baseline" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("steins: --baseline requires a path argument");
                    return ExitCode::from(2);
                };
                baseline_path = Some(value.clone());
                i += 2;
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

    // Project mode (ADR-0009/0015): all `.php` files across the given paths form
    // ONE project (one salsa DB), so cross-file calls, class chains, and effects
    // resolve. `texts` keeps each file's contents by diagnostic path so the
    // baseline hash can read the flagged line's neighborhood (ADR-0022).
    let mut inputs: Vec<SourceFile> = Vec::new();
    let mut texts: HashMap<String, String> = HashMap::new();
    for file_path in &files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                eprintln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        let path = file_path.to_string_lossy().into_owned();
        texts.insert(path.clone(), text.clone());
        inputs.push(SourceFile::new(&db, path, text));
    }
    let project = Project::new(&db, inputs.clone());
    let findings: Vec<Diagnostic> = check_project(&db, project, &mut folder);

    // Inline `@steins-ignore` applies first (ADR-0023): a finding suppressed
    // inline never reaches — nor consumes — the baseline channel.
    let trees: Vec<&SourceTree> = inputs.iter().map(|&sf| parse_tree(&db, sf)).collect();
    let file_pairs: Vec<(String, &SourceTree)> =
        inputs.iter().zip(trees.iter()).map(|(&sf, &t)| (sf.path(&db).to_owned(), t)).collect();
    let inline = apply_inline_ignores(findings, &file_pairs);

    // Which baseline file to consult (ADR-0022): `--set-baseline` and an explicit
    // `--baseline` both name a file; otherwise the default is auto-loaded when it
    // exists, unless `--ignore-baseline` bypasses it.
    let baseline_file: Option<PathBuf> = if set_baseline {
        Some(PathBuf::from(baseline_path.as_deref().unwrap_or(baseline::DEFAULT_FILE)))
    } else if ignore_baseline {
        None
    } else if let Some(p) = &baseline_path {
        Some(PathBuf::from(p))
    } else if Path::new(baseline::DEFAULT_FILE).exists() {
        Some(PathBuf::from(baseline::DEFAULT_FILE))
    } else {
        None
    };

    if set_baseline {
        let file = baseline_file.expect("set-baseline names a file");
        return write_baseline(&file, &inline.kept, &texts);
    }

    // Baseline channel: partition the inline survivors into baselined (suppressed,
    // excluded from exit) and reported. `--ignore-baseline` / no file → all report.
    let (reported, baselined, stale) = match &baseline_file {
        Some(file) => match std::fs::read_to_string(file) {
            Ok(text) => match_baseline(file, &text, inline.kept, &texts),
            Err(_) => (inline.kept, 0, 0),
        },
        None => (inline.kept, 0, 0),
    };

    // Displayed = object-level survivors + meta-diagnostics (which are exempt from
    // both channels). Sorted for deterministic output.
    let mut displayed = reported;
    displayed.extend(inline.meta);
    displayed.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column, a.id).cmp(&(b.path.as_str(), b.line, b.column, b.id))
    });

    match format {
        Format::Text => print_text(&displayed, inline.suppressed, baselined, stale),
        Format::Json => print_json(&displayed, inline.suppressed, baselined),
    }

    if displayed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Write a baseline file from the inline-surviving findings (ADR-0022
/// `--set-baseline`). Never affects exit code: writing is a maintenance action.
fn write_baseline(file: &Path, findings: &[Diagnostic], texts: &HashMap<String, String>) -> ExitCode {
    let dir = baseline::base_dir(file);
    let entries: Vec<baseline::Entry> = findings
        .iter()
        .map(|d| {
            let rel = baseline::relativize(&dir, &d.path);
            let hash = texts
                .get(&d.path)
                .map_or_else(String::new, |t| baseline::entry_hash(d.id, &rel, t, d.line));
            baseline::Entry { id: d.id.to_owned(), path: rel, hash }
        })
        .collect();
    let n = entries.len();
    match std::fs::write(file, baseline::render(entries)) {
        Ok(()) => {
            eprintln!("steins: wrote {n} baseline entries to {}", file.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("steins: cannot write baseline {}: {e}", file.display());
            ExitCode::from(2)
        }
    }
}

/// Match inline-surviving `findings` against a baseline file's entries. Returns
/// `(reported, baselined_count, stale_count)`.
fn match_baseline(
    file: &Path,
    text: &str,
    findings: Vec<Diagnostic>,
    texts: &HashMap<String, String>,
) -> (Vec<Diagnostic>, usize, usize) {
    let entries = baseline::parse(text);
    let dir = baseline::base_dir(file);
    let mut matcher = baseline::Matcher::new(&entries);
    let mut reported = Vec::new();
    let mut baselined = 0usize;
    for d in findings {
        let rel = baseline::relativize(&dir, &d.path);
        let hash = texts
            .get(&d.path)
            .map_or_else(String::new, |t| baseline::entry_hash(d.id, &rel, t, d.line));
        if matcher.take(d.id, &rel, &hash) {
            baselined += 1;
        } else {
            reported.push(d);
        }
    }
    (reported, baselined, matcher.stale_count())
}

/// `steins annotate [--no-php] <file.php>` — reprint one file with a right-margin
/// column of proven inferred facts (ADR-0020). Never modifies the file; output
/// goes to stdout. Exits 2 on a usage error (directory, missing/extra args).
fn run_annotate(args: &[String]) -> ExitCode {
    let mut no_php = false;
    let mut project_dir: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-php" => {
                no_php = true;
                i += 1;
            }
            "--project" => {
                let Some(dir) = args.get(i + 1) else {
                    eprintln!("steins: --project requires a directory argument");
                    return ExitCode::from(2);
                };
                project_dir = Some(dir.clone());
                i += 2;
            }
            other if other.starts_with('-') => {
                eprintln!("steins: unknown flag `{other}` for annotate");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    let [path] = paths.as_slice() else {
        eprintln!(
            "steins: annotate takes exactly one file (usage: steins annotate [--no-php] [--project <dir>] <file.php>)"
        );
        return ExitCode::from(2);
    };
    let path = Path::new(path);
    if path.is_dir() {
        eprintln!("steins: annotate expects a single file, not a directory: {}", path.display());
        return ExitCode::from(2);
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            eprintln!("steins: cannot read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };

    // Same coverage posture as `check` (ADR-0004).
    if no_php {
        eprintln!("{SOUND_SUBSET_NOTICE}");
    }
    let db = SteinsDatabase::default();
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

    // The project context for cross-file facts (ADR-0015): the `--project`
    // directory, else the file's own directory. Every `.php` file under it is
    // parsed into one project so `annotate` sees cross-file resolution.
    let root = project_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")));

    let canon_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut project_files = Vec::new();
    collect_php_files(&root, &mut project_files);
    project_files.sort();
    project_files.dedup();

    let mut inputs: Vec<SourceFile> = Vec::new();
    let mut target: Option<SourceFile> = None;
    for fp in &project_files {
        let content = if fp.canonicalize().map(|c| c == canon_target).unwrap_or(false) {
            text.clone()
        } else {
            match std::fs::read(fp) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(_) => continue,
            }
        };
        let input = SourceFile::new(&db, fp.to_string_lossy().into_owned(), content);
        if fp.canonicalize().map(|c| c == canon_target).unwrap_or(false) {
            target = Some(input);
        }
        inputs.push(input);
    }

    // If the target file was not found under the root (e.g. an explicit path
    // outside the project dir), fall back to a one-file project.
    let facts = match target {
        Some(target_file) => {
            let project = Project::new(&db, inputs);
            annotate_project(&db, project, target_file, &mut folder)
        }
        None => {
            let input = SourceFile::new(&db, path.to_string_lossy().into_owned(), text.clone());
            annotate_file(&db, input, &mut folder)
        }
    };

    print!("{}", render_annotation(&text, &facts));
    ExitCode::SUCCESS
}

/// Render the annotated file: each source line reprinted verbatim, and lines
/// with a proven fact padded (to the longest line, capped at column 88) and
/// given a `//=>` margin. Multiple facts on one line join with `; `.
fn render_annotation(text: &str, facts: &[LineFact]) -> String {
    /// The column source lines are padded to before the margin (cap: longer
    /// lines simply get a single separating space).
    const CAP: usize = 88;
    const PREFIX: &str = "//=> ";

    let lines: Vec<&str> = text.lines().collect();

    // Group fact bodies by line, de-duplicating identical bodies, order-stable.
    let mut by_line: std::collections::BTreeMap<u32, Vec<String>> = std::collections::BTreeMap::new();
    for f in facts {
        let bodies = by_line.entry(f.line).or_default();
        let body = f.body();
        if !bodies.contains(&body) {
            bodies.push(body);
        }
    }

    let target = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).min(CAP);

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let line_no = i as u32 + 1;
        out.push_str(line);
        if let Some(bodies) = by_line.get(&line_no) {
            let width = line.chars().count();
            // Pad up to `target`, then always exactly one separating space — so
            // margins align at column `target + 1`, and an over-long line (width
            // >= target) simply gets that single space.
            let pad = target.saturating_sub(width) + 1;
            for _ in 0..pad {
                out.push(' ');
            }
            out.push_str(PREFIX);
            out.push_str(&bodies.join("; "));
        }
        out.push('\n');
    }
    out
}

fn print_text(findings: &[Diagnostic], suppressed: usize, baselined: usize, stale: usize) {
    for d in findings {
        println!("{}:{}:{}: error[{}]: {}", d.path, d.line, d.column, d.id, d.message);
    }
    // Suppression accounting (ADR-0022/0023), each line printed only when nonzero.
    if suppressed > 0 {
        println!("{suppressed} diagnostics suppressed by inline ignores");
    }
    if baselined > 0 {
        println!("{baselined} findings in baseline");
    }
    if stale > 0 {
        println!("{stale} baseline entries no longer match (stale — rerun --set-baseline)");
    }
}

fn print_json(findings: &[Diagnostic], suppressed: usize, baselined: usize) {
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
    let doc = serde_json::json!({
        "findings": array,
        "suppressed": suppressed,
        "baselined": baselined,
    });
    match serde_json::to_string_pretty(&doc) {
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
