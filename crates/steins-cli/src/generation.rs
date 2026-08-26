//! CLI wiring for the experimental frozen-generation lifecycle (ADR-0092 §5,
//! issue #489 slice A). The orchestrator itself is library-shaped in
//! `steins_infer::generation_check` (shared with `cargo xtask perf --warm` and,
//! later, the MCP server per issue #491); this module is only the `steins
//! check` glue: resolve the run's boundary inputs, call the orchestrator, and
//! hand back what the unchanged downstream pipeline needs.
//!
//! **The gate.** The lifecycle activates only when
//! `STEINS_EXPERIMENTAL_GENERATIONS=1` is in the environment — read once in
//! `run_check` and plumbed as a bool, deliberately not a CLI flag: flag
//! promotion is an ADR-0020 surface decision the owner takes later. With the
//! variable unset (every CI run), `steins check` is byte-identical to today.
//! With it set, the product surface is still the same report — the gate adds
//! stderr notes and the `.steins/gen/` store, nothing else — and any
//! orchestration failure degrades to the ordinary cold path with a note.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use steins_db::EffectsPolicy;
use steins_infer::{Diagnostic, GenerationMode, GenerationParams, generation_check};
use steins_syntax::SourceTree;

use crate::config::RuntimePostures;
use crate::project::{LoadedProject, assemble_loaded, load_plugins, resolve_layout};

/// What the gated path hands the downstream pipeline: the same
/// [`LoadedProject`] shape the cold path builds (salsa view for `--fix` and
/// the baseline machinery — no parse forced), the generation findings, and the
/// orchestrator's owned trees so inline-ignore scanning re-parses nothing.
pub(crate) struct GatedRun {
    pub(crate) loaded: LoadedProject,
    pub(crate) findings: Vec<Diagnostic>,
    pub(crate) trees: Vec<(String, SourceTree)>,
}

/// Run the generation lifecycle for this check invocation, or `None` to fall
/// back to the ordinary cold path (with the reason already on stderr). Prints
/// the same boundary notices `load_project` prints — plugin refusals, effect
/// label vocabulary, `[runtime]` warnings, attribution hygiene — so the gated
/// run's stderr carries everything the cold run's does.
pub(crate) fn try_generation_check(
    files: &[PathBuf],
    paths: &[String],
    plugin_allow: Option<&[String]>,
    effects: &EffectsPolicy,
    postures: &RuntimePostures,
    no_php: bool,
    runtime_warnings: &[String],
) -> Option<GatedRun> {
    let Ok(cwd) = std::env::current_dir() else {
        errln!("steins: experimental generations: cannot resolve the working directory; running as today");
        return None;
    };
    let layout = resolve_layout(paths);
    let plugins = load_plugins(&layout, plugin_allow);
    for notice in effects.label_notices(plugins.registry()) {
        errln!("steins: {notice}");
    }
    for w in runtime_warnings {
        errln!("steins: {w}");
    }

    // Where the sealed capture keys this run's files from (issue #506). Not
    // the layout root: a manifest-less tree has no governing root at all, and
    // the two roots answer different questions anyway — the layout root says
    // who governs the code, the capture root only says what prefix the seal's
    // keys drop. The analyzed files are the only thing that always answers the
    // second one.
    let capture_root = capture_root(files, &cwd);
    // The store lives with the project: the outermost governing root, or —
    // manifest-less — beside the analyzed code rather than beside whoever
    // happened to invoke us (issue #506: the cache belongs to the tree it
    // caches, not to the caller's working directory).
    let store_root =
        layout.roots().last().map_or_else(|| capture_root.clone(), |root| root.dir().to_path_buf());
    let partition = steins_db::partition::discover(&layout);
    let params = GenerationParams {
        store_root: &store_root,
        capture_root: &capture_root,
        files,
        layout: &layout,
        partition: &partition,
        plugins: &plugins,
        effects,
        warning_handler_abort: postures.warning_handler_abort,
        final_keyword: postures.final_keyword,
        php: !no_php,
        // The verifier is environment-driven here (`STEINS_GENERATIONS_PARANOID`),
        // which `generation_check` reads for itself; the CLI never forces it.
        paranoid: false,
    };
    let outcome = match generation_check(&params) {
        Ok(outcome) => outcome,
        Err(e) => {
            // Degrade to the ordinary cold path. The fallback re-prints the
            // channel notices through `load_project`; a duplicated stderr line
            // on a failing experimental path is the cheap side of that trade.
            errln!("steins: experimental generations unavailable ({e}); running as today");
            return None;
        }
    };
    for notice in &outcome.attribution_notices {
        errln!("steins: {notice}");
    }
    let report = &outcome.report;
    let (loaded_files, parsed_files) = report
        .packages
        .iter()
        .fold((0usize, 0usize), |(l, p), pkg| (l + pkg.loaded, p + pkg.parsed));
    errln!(
        "steins: experimental generations: {} run, {} package(s), {} file(s) loaded from artifacts, {} parsed; {} walked, {} replayed; generation {}",
        match report.mode {
            GenerationMode::Cold => "cold",
            GenerationMode::Warm => "warm",
        },
        report.packages.len(),
        loaded_files,
        parsed_files,
        report.walk.walked,
        report.walk.replayed,
        report.generation.as_deref().unwrap_or("(unpublished)")
    );
    for note in &report.notes {
        errln!("steins: experimental generations: {note}");
    }

    // The salsa view over the same sealed texts, in the same slot order.
    let entries: Vec<(String, String)> = outcome
        .trees
        .iter()
        .map(|(path, _)| (path.clone(), outcome.texts.get(path).cloned().unwrap_or_default()))
        .collect();
    let loaded = assemble_loaded(entries, layout, plugins, effects.clone());
    Some(GatedRun { loaded, findings: outcome.findings, trees: outcome.trees })
}

/// The directory the sealed capture keys `files` against: their common
/// ancestor, or `cwd` when the run cannot use one.
///
/// `SourceInventory::capture` keys every file by `strip_prefix(root)` and
/// reads it back as `root.join(key)`, so the root has to be a directory prefix
/// of the *spellings the CLI resolved* — which is why passing `cwd` was wrong
/// (issue #506): `steins check /some/tree` from anywhere else failed capture on
/// its first file and dropped the whole lifecycle to the cold path.
///
/// The degenerate cases, deliberately:
///
/// * **No files.** A path argument that exists but holds no `.php` file
///   reaches the gate with an empty universe (`check` only exits early on a
///   *missing* path), and nothing is captured — `cwd`, exactly as before.
/// * **One file.** Its parent directory; the key is the bare file name.
/// * **Nothing shared but the filesystem root.** `/` — correct, only verbose
///   in the keys, which are internal to the seal.
/// * **A relative spelling anywhere in the set.** `cwd`, because a relative
///   key is joined onto the root verbatim: the working directory is the only
///   root that resolves it. That is also today's behavior and today's correct
///   answer — `steins check .` and `steins check src/` are what produce
///   relative spellings. A mixed invocation whose absolute member sits outside
///   `cwd` still degrades to the cold path with a note, as it does today.
///
/// Components are compared as spelled, never normalized: the root must strip
/// off the files' own leading components, so a `..` the caller wrote survives
/// in both or in neither.
fn capture_root(files: &[PathBuf], cwd: &Path) -> PathBuf {
    if files.is_empty() || files.iter().any(|f| f.is_relative()) {
        return cwd.to_path_buf();
    }
    /// The file's own directory. `parent()` is `None` only for a bare root,
    /// which the `.php` walk never yields as a file.
    fn dir_of(file: &Path) -> &Path { file.parent().unwrap_or(file) }

    let mut common: Vec<&OsStr> = dir_of(&files[0]).components().map(Component::as_os_str).collect();
    for file in &files[1..] {
        let shared = dir_of(file)
            .components()
            .zip(&common)
            .take_while(|(c, want)| c.as_os_str() == **want)
            .count();
        common.truncate(shared);
    }
    if common.is_empty() {
        // No shared component at all (distinct Windows prefixes; unreachable
        // where every path starts at `/`). Nothing better than today's answer.
        return cwd.to_path_buf();
    }
    let mut root = PathBuf::new();
    for part in common {
        root.push(part);
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_of(files: &[&str], cwd: &str) -> String {
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        capture_root(&files, Path::new(cwd)).to_string_lossy().into_owned()
    }

    /// The invocation the gate was written for keeps its root: relative
    /// spellings only resolve against the directory they were spelled in.
    #[test]
    fn relative_spellings_keep_the_working_directory() {
        assert_eq!(root_of(&["src/a.php", "src/b.php"], "/work"), "/work");
        assert_eq!(root_of(&["a.php"], "/work"), "/work");
        // Mixed: the relative member pins it, absolute members strip `/work`.
        assert_eq!(root_of(&["src/a.php", "/work/lib/b.php"], "/work"), "/work");
    }

    /// Issue #506 proper: an absolute out-of-cwd target captures against
    /// itself instead of failing `strip_prefix` on its first file.
    #[test]
    fn an_absolute_target_captures_against_its_own_tree() {
        assert_eq!(root_of(&["/tree/src/a.php", "/tree/src/b.php"], "/elsewhere"), "/tree/src");
        assert_eq!(root_of(&["/tree/src/a.php", "/tree/tests/b.php"], "/elsewhere"), "/tree");
    }

    /// A single file argument keys against its parent directory.
    #[test]
    fn a_single_file_captures_against_its_parent() {
        assert_eq!(root_of(&["/tree/src/a.php"], "/elsewhere"), "/tree/src");
        assert_eq!(root_of(&["/a.php"], "/elsewhere"), "/");
    }

    /// Nothing in common but the filesystem root: verbose keys, still correct.
    #[test]
    fn unrelated_trees_fall_back_to_the_filesystem_root() {
        assert_eq!(root_of(&["/one/a.php", "/two/b.php"], "/elsewhere"), "/");
    }

    /// An empty universe never captures anything; keep the prior answer.
    #[test]
    fn an_empty_file_set_keeps_the_working_directory() {
        assert_eq!(root_of(&[], "/work"), "/work");
    }

    /// A `..` the caller wrote is kept, so `strip_prefix` still matches the
    /// spelling the seal is handed.
    #[test]
    fn a_dotdot_spelling_is_stripped_as_spelled() {
        assert_eq!(root_of(&["/tree/src/../lib/a.php"], "/elsewhere"), "/tree/src/../lib");
    }
}
