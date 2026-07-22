//! The `steins` binary.
//!
//! Two commands exist for this milestone (ADR-0020 documents the eventual
//! six-command surface; the rest are deliberately NOT stubbed). `check` walks
//! `.php` files, runs the salsa pipeline, prints proof-layer diagnostics, and
//! exits 1 if any finding was reported. `annotate` reprints a single file with a
//! right-margin column of *proven* inferred facts (the Rigor-style display).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{
    Diagnostic, LineFact, SOUND_SUBSET_NOTICE, SidecarFolder, annotate_file, annotate_project,
    check_project,
};

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
            eprintln!("usage: steins check [--format text|json] [--no-php] <paths...>");
            eprintln!("       steins annotate [--no-php] <file.php>");
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

    // Project mode (ADR-0009/0015): all `.php` files across the given paths form
    // ONE project (one salsa DB), so cross-file calls, class chains, and effects
    // resolve.
    let mut inputs: Vec<SourceFile> = Vec::new();
    for file_path in &files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                eprintln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        inputs.push(SourceFile::new(&db, file_path.to_string_lossy().into_owned(), text));
    }
    let project = Project::new(&db, inputs);
    let findings: Vec<Diagnostic> = check_project(&db, project, &mut folder);

    match format {
        Format::Text => print_text(&findings),
        Format::Json => print_json(&findings),
    }

    if findings.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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
