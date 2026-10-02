//! `steins annotate` (ADR-0020): reprint one file with a right-margin column
//! of proven facts, or (`--format json`, issue #65) the same effect summaries
//! as a document. Never modifies the file.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{EffectsPolicy, PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    EffectSummary, FactKind, INTERNAL_PANIC_ID, LineFact, SOUND_SUBSET_NOTICE, SidecarFolder,
    annotate_project_under, effect_summaries_project,
};

use crate::Format;
use crate::config::{allow_list, effects_from_config, read_steins_config, runtime_from_config};
use crate::project::{collect_sources, load_plugins, resolve_layout};

/// `steins annotate [--no-php] [--format text|json] <file.php>` — reprint one
/// file with a right-margin column of proven facts (ADR-0020), or (JSON) the
/// same effect summaries (issue #65). Never modifies the file; exit 2 on usage or
/// config error, or when the margin carries an `internal.panic`.
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
    // `./steins.toml` parsed once, as `check` parses it (ADR-0050 §7): a malformed
    // file, an unknown `[runtime]` key included, is exit 2. The `[runtime]`
    // postures go to the margin whole, as `check` runs them (issue #787).
    let config = match read_steins_config() {
        Ok(config) => config.unwrap_or_default(),
        Err(e) => {
            errln!("steins: {e}");
            return ExitCode::from(2);
        }
    };
    let (postures, runtime_warnings) = runtime_from_config(config.runtime);
    let plugin_allow = allow_list(config.plugins);
    let effects_policy = effects_from_config(config.effects, false);
    let db = SteinsDatabase::default();
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };
    let (project, target_file) = load_annotate_project(
        &db,
        path,
        &text,
        project_dir,
        &mut folder,
        plugin_allow.as_deref(),
        effects_policy,
    );
    // After the boundary notices, where `check` prints them.
    for w in &runtime_warnings {
        errln!("steins: {w}");
    }

    // `--format json` reads the same [`EffectSummary`]s as the text margin
    // (issue #65); the effect fixpoint reads no posture, in `check` as here.
    match format {
        Format::Text => {
            let facts = annotate_project_under(&db, project, target_file, &mut folder, postures);
            out!("{}", render_annotation(&text, &facts));
            // The margin's findings come from the isolated walk (issue #895
            // D3): a panic there leaves the file's `✗` markers missing, so the
            // margin is no verdict and the run exits 2, as `check` does.
            let panicked = facts
                .iter()
                .any(|f| matches!(f.kind, FactKind::Finding { id } if id == INTERNAL_PANIC_ID));
            if panicked {
                errln!("steins: {}", crate::check::panic_notice(1));
                return ExitCode::from(2);
            }
        }
        Format::Json => print_annotate_json(&effect_summaries_project(&db, project, target_file)),
    }
    ExitCode::SUCCESS
}

/// The project `path` is annotated in (ADR-0015): every source under the
/// `--project` dir, else under the file's own dir, with `text` standing in for
/// the target. Falls back to a one-file project (no manifest, no plugin, no
/// policy) when the target isn't under that root.
fn load_annotate_project(
    db: &SteinsDatabase,
    path: &Path,
    text: &str,
    project_dir: Option<String>,
    folder: &mut SidecarFolder,
    plugin_allow: Option<&[String]>,
    effects_policy: EffectsPolicy,
) -> (Project, SourceFile) {
    // A bare relative filename has an empty (unopenable) parent — else falls
    // back silently.
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
            text.to_owned()
        } else {
            match std::fs::read(fp) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(_) => continue,
            }
        };
        let input = SourceFile::new(db, fp.to_string_lossy().into_owned(), content);
        if fp.canonicalize().map(|c| c == canon_target).unwrap_or(false) {
            target = Some(input);
        }
        inputs.push(input);
    }

    match target {
        Some(target_file) => {
            let layout = resolve_layout(&[root.to_string_lossy().into_owned()]);
            folder.set_php_target(layout.php_target().cloned());
            let plugins = load_plugins(&layout, plugin_allow);
            let project =
                Project::builder(inputs, layout, plugins).effects(effects_policy).new(db);
            (project, target_file)
        }
        None => {
            let input = SourceFile::new(db, path.to_string_lossy().into_owned(), text.to_owned());
            let project =
                Project::new(db, vec![input], ProjectLayout::fallback(), PluginFacts::none());
            (project, input)
        }
    }
}

/// `annotate --format json`'s document (issue #65): sorted proven labels,
/// sorted declared bounds (ADR-0067), exhaustiveness. `tolerated` (ADR-0084 §4)
/// joins only where discharged, as a subset of `effects`, never a removal.
///
/// `gaps` and `throws_gaps` (ADR-0099 §5) name the kinds of coverage gap behind
/// `exhaustive == false` and `throws_exhaustive == false`, each empty exactly
/// when its lane is exhaustive. They are always present, even empty, so a reader
/// never has to tell an absent key from a clean body; `tolerated` is the
/// optional one because it is a subset that most bodies lack.
/// `throws_exhaustive` is here so `throws_gaps` can be read on its own.
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
            entry.insert("gaps".to_owned(), serde_json::json!(s.gaps));
            entry.insert("throws_exhaustive".to_owned(), serde_json::json!(s.throws_exhaustive));
            entry.insert("throws_gaps".to_owned(), serde_json::json!(s.throws_gaps));
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
