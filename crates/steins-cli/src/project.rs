//! Project loading, shared by every subcommand: path validation, the `.php`
//! walk (`steins_db::walk` — the one the harnesses walk too, issue #524),
//! its real-identity dedup (issue #179), layout discovery (ADR-0015),
//! the plugin channel (ADR-0068), and [`load_project`] — the single door
//! through which `check`, `transform` and MCP (issue #117) build ONE salsa
//! project (ADR-0009/0015) so cross-file calls, class chains and effects resolve.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::walk::{self, Sources};
use steins_db::{
    EffectsPolicy, PluginFacts, Project, ProjectLayout, Resolve, SourceFile, SteinsDatabase,
    composer, project_index,
};

use crate::config::vendor_dirs_from_disk;

/// Reject explicitly-passed paths that name nothing (ADR-0050 §7 amendment):
/// previously `steins check /typo` reported an empty findings set at exit 0,
/// a false all-clear (a renamed directory kept CI green). A path that exists
/// but yields zero `.php` files still stays exit 0.
pub(crate) fn reject_missing_paths(paths: &[String]) -> Result<(), ExitCode> {
    let missing = missing_paths(paths);
    if missing.is_empty() {
        return Ok(());
    }
    for p in &missing {
        errln!("steins: path does not exist: {p}");
    }
    Err(ExitCode::from(2))
}

/// Resolve the run's [`ProjectLayout`] (ADR-0015): each governing
/// `composer.json` names its vendor dir; no manifest → [`ProjectLayout::fallback`].
/// `[paths] vendor-dirs` (issue #181) only supplies a floor a manifest beats.
pub(crate) fn resolve_layout(paths: &[String]) -> ProjectLayout {
    let Ok(cwd) = std::env::current_dir() else { return ProjectLayout::fallback() };
    let roots: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    composer::discover(&roots, &cwd).with_extra_vendor_dirs(vendor_dirs_from_disk())
}

/// The path arguments that name nothing on disk: exit 2 on the command line
/// ([`reject_missing_paths`]), a named tool error over MCP — same rule.
pub(crate) fn missing_paths(paths: &[String]) -> Vec<&String> {
    paths.iter().filter(|p| !Path::new(p.as_str()).exists()).collect()
}

/// The `.php` files `paths` names — [`steins_db::walk`], the one walk the
/// `steins` binary and the `xtask` harnesses share (issue #524). Callers that
/// want the walk's posture too (`doctor`) take [`collect_sources`].
pub(crate) fn collect_files(paths: &[String]) -> Vec<PathBuf> {
    collect_sources(&paths.iter().map(PathBuf::from).collect::<Vec<_>>()).files
}

/// [`collect_files`] keeping what the walk refused: the directory symlinks it
/// would have had to leave the tree (or re-enter it) to follow.
pub(crate) fn collect_sources(roots: &[PathBuf]) -> Sources {
    walk::php_files(roots)
}

/// One analyzed project: salsa database, [`Project`] input, parsed file
/// handles, each file's text keyed by diagnostic path. `db` owns everything
/// salsa ids point into — hand out `&loaded.db`, not moved out.
pub(crate) struct LoadedProject {
    pub(crate) db: SteinsDatabase,
    pub(crate) project: Project,
    pub(crate) inputs: Vec<SourceFile>,
    /// Each file's contents by diagnostic path (ADR-0022 baseline hash, splices).
    pub(crate) texts: HashMap<String, String>,
    pub(crate) layout: ProjectLayout,
}

/// Load `files` as ONE project (ADR-0009/0015): one salsa DB, so cross-file
/// calls resolve. Single door: `check`, `transform`, MCP (issue #117) all come
/// through here. An unreadable file is reported on stderr and left out.
pub(crate) fn load_project(
    files: &[PathBuf],
    paths: &[String],
    allow: Option<&[String]>,
    effects: EffectsPolicy,
) -> LoadedProject {
    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::new();
    let mut texts: HashMap<String, String> = HashMap::new();
    for file_path in files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                errln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        let path = file_path.to_string_lossy().into_owned();
        texts.insert(path.clone(), text.clone());
        inputs.push(SourceFile::new(&db, path, text));
    }
    let layout = resolve_layout(paths);
    // The plugin channel (ADR-0068), read once at the boundary like the layout.
    let plugins = load_plugins(&layout, allow);
    // Tolerated-effects vocabulary judged against this run's registry (ADR-0084 §5).
    for notice in effects.label_notices(plugins.registry()) {
        errln!("steins: {notice}");
    }
    let project =
        Project::builder(inputs.clone(), layout.clone(), plugins).effects(effects).new(&db);
    // Attribution keys checked against the symbol table. Never a diagnostic:
    // a key naming an unvendored class is a stale config line.
    for notice in attribution_notices(&db, project) {
        errln!("steins: {notice}");
    }
    LoadedProject { db, project, inputs, texts, layout }
}

/// Assemble a [`LoadedProject`] from texts already in hand — the experimental
/// generation path (issue #489), whose sources came back from the sealed
/// capture and whose trees are owned by the orchestrator. Builds the same salsa
/// view [`load_project`] builds (so `--fix`'s post-check and the baseline
/// machinery work unchanged) but triggers no parse and prints no notices: the
/// gated caller printed the channel notices itself, and the attribution check
/// came back from the orchestrator without forcing a salsa parse.
///
/// `entries` must be in the orchestrator's universe-slot order, so the salsa
/// project and the generation analysis agree on file identity.
pub(crate) fn assemble_loaded(
    entries: Vec<(String, String)>,
    layout: ProjectLayout,
    plugins: PluginFacts,
    effects: EffectsPolicy,
) -> LoadedProject {
    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(entries.len());
    let mut texts: HashMap<String, String> = HashMap::with_capacity(entries.len());
    for (path, text) in entries {
        texts.insert(path.clone(), text.clone());
        inputs.push(SourceFile::new(&db, path, text));
    }
    let project =
        Project::builder(inputs.clone(), layout.clone(), plugins).effects(effects).new(&db);
    LoadedProject { db, project, inputs, texts, layout }
}

/// Load the plugin channel (ADR-0068) for `layout`, reporting every load-time
/// refusal on stderr. Never a diagnostic — the zero-FP banner covers the
/// user's code, not a third party's packaging mistake.
pub(crate) fn load_plugins(layout: &ProjectLayout, allow: Option<&[String]>) -> PluginFacts {
    let facts = PluginFacts::discover(layout, allow);
    for notice in facts.notices() {
        errln!("steins: {notice}");
    }
    facts
}

/// `[effects.attribution]` keys naming no symbol (ADR-0084 §5). Tried against
/// all four symbol kinds; for `Class::method` only the class resolves.
fn attribution_notices(db: &SteinsDatabase, project: Project) -> Vec<String> {
    let policy = project.effects(db);
    if policy.is_empty() {
        return Vec::new();
    }
    let index = project_index(db, project);
    let known = |name: &str| {
        !matches!(index.resolve_class(name), Resolve::Absent)
            || !matches!(index.resolve_function(name), Resolve::Absent)
            // Same test the checker uses to decide builtin vs. unresolved userland call.
            || steins_catalog::effect_labels(name).is_some()
            || steins_catalog::out_params(name).is_some()
            || steins_catalog::builtin_class_display(name).is_some()
    };
    policy
        .attribution_keys()
        .filter(|key| {
            let symbol = key.trim_start_matches('\\');
            let named = symbol.split("::").next().unwrap_or(symbol);
            !known(named) && !known(&named.to_ascii_lowercase())
        })
        .map(|key| {
            format!("steins.toml [effects.attribution]: \"{key}\" names no symbol this project defines")
        })
        .collect()
}

