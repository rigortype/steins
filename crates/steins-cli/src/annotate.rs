//! `steins annotate` (ADR-0020): reprint one file with a right-margin column
//! of proven facts, or (`--format json`, issue #65) the same effect summaries
//! as a document. Never modifies the file.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{
    EffectSummary, LineFact, SOUND_SUBSET_NOTICE, SidecarFolder, annotate_file, annotate_project,
    effect_summaries_file, effect_summaries_project,
};

use crate::Format;
use crate::config::{allow_list_from_disk, effects_policy_from_disk};
use crate::project::{collect_sources, load_plugins, resolve_layout};

/// `steins annotate [--no-php] [--format text|json] <file.php>` — reprint one
/// file with a right-margin column of proven facts (ADR-0020), or (JSON) the
/// same effect summaries (issue #65). Never modifies the file; exit 2 on usage error.
pub(crate) fn run_annotate(args: &[String]) -> ExitCode {
    let mut no_php = false;
    let mut format = Format::Text;
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
                    errln!("steins: --project requires a directory argument");
                    return ExitCode::from(2);
                };
                project_dir = Some(dir.clone());
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
                errln!("steins: unknown flag `{other}` for annotate");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    let [path] = paths.as_slice() else {
        errln!(
            "steins: annotate takes exactly one file (usage: steins annotate [--no-php] [--format text|json] [--project <dir>] <file.php>)"
        );
        return ExitCode::from(2);
    };
    let path = Path::new(path);
    if path.is_dir() {
        errln!("steins: annotate expects a single file, not a directory: {}", path.display());
        return ExitCode::from(2);
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            errln!("steins: cannot read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };

    // Same coverage posture as `check` (ADR-0004).
    if no_php {
        errln!("{SOUND_SUBSET_NOTICE}");
    }
    let db = SteinsDatabase::default();
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

    // Project context (ADR-0015): `--project` dir, else the file's dir. A bare
    // relative filename has an empty (unopenable) parent — else falls back silently.
    let root = project_dir.map(PathBuf::from).unwrap_or_else(|| {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    let canon_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let project_files = collect_sources(std::slice::from_ref(&root)).files;

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

    // Fall back to a one-file project if the target isn't under root. `--format
    // json` reads the same [`EffectSummary`]s as the text margin (issue #65).
    match format {
        Format::Text => {
            let facts = match target {
                Some(target_file) => {
                    let layout = resolve_layout(&[root.to_string_lossy().into_owned()]);
                    folder.set_php_target(layout.php_target().cloned());
                    let plugins = load_plugins(&layout, allow_list_from_disk().as_deref());
                    let project = Project::builder(inputs, layout, plugins)
                        .effects(effects_policy_from_disk())
                        .new(&db);
                    annotate_project(&db, project, target_file, &mut folder)
                }
                None => {
                    let input =
                        SourceFile::new(&db, path.to_string_lossy().into_owned(), text.clone());
                    annotate_file(&db, input, &mut folder)
                }
            };
            out!("{}", render_annotation(&text, &facts));
        }
        Format::Json => {
            let summaries = match target {
                Some(target_file) => {
                    let layout = resolve_layout(&[root.to_string_lossy().into_owned()]);
                    let plugins = load_plugins(&layout, allow_list_from_disk().as_deref());
                    let project = Project::builder(inputs, layout, plugins)
                        .effects(effects_policy_from_disk())
                        .new(&db);
                    effect_summaries_project(&db, project, target_file)
                }
                None => {
                    let input =
                        SourceFile::new(&db, path.to_string_lossy().into_owned(), text.clone());
                    effect_summaries_file(&db, input)
                }
            };
            print_annotate_json(&summaries);
        }
    }
    ExitCode::SUCCESS
}

/// `annotate --format json`'s document (issue #65): sorted proven labels,
/// sorted declared bounds (ADR-0067), exhaustiveness. `tolerated` (ADR-0084 §4)
/// joins only where discharged, as a subset of `effects`, never a removal.
fn print_annotate_json(summaries: &[EffectSummary]) {
    let functions: Vec<serde_json::Value> = summaries
        .iter()
        .map(|s| {
            let mut entry = serde_json::Map::new();
            entry.insert("name".to_owned(), serde_json::json!(s.symbol));
            entry.insert("line".to_owned(), serde_json::json!(s.line));
            entry.insert("effects".to_owned(), serde_json::json!(s.labels));
            if !s.tolerated.is_empty() {
                entry.insert("tolerated".to_owned(), serde_json::json!(s.tolerated));
            }
            entry.insert("declared".to_owned(), serde_json::json!(s.declared));
            entry.insert("exhaustive".to_owned(), serde_json::json!(s.exhaustive));
            serde_json::Value::Object(entry)
        })
        .collect();
    let doc = serde_json::json!({ "functions": functions });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => outln!("{s}"),
        Err(e) => errln!("steins: failed to serialize json: {e}"),
    }
}

/// Render the annotated file: lines with a proven fact padded (longest line,
/// capped at column 88) and given a `//=>` margin; facts join with `; `.
fn render_annotation(text: &str, facts: &[LineFact]) -> String {
    /// The column source lines are padded to before the margin.
    const CAP: usize = 88;
    const PREFIX: &str = "//=> ";

    let lines: Vec<&str> = text.lines().collect();

    // Group fact bodies by line, de-duplicating, order-stable.
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
            // Pad to `target` plus one space, so margins align at `target + 1`.
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
