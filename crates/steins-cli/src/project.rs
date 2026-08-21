//! Project loading, shared by every subcommand: path validation, the `.php`
//! walk and its real-identity dedup (issue #179), layout discovery (ADR-0015),
//! the plugin channel (ADR-0068), and [`load_project`] — the single door
//! through which `check`, `transform` and MCP (issue #117) build ONE salsa
//! project (ADR-0009/0015) so cross-file calls, class chains and effects resolve.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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

/// The `.php` files `paths` names, deduplicated to real identity (issue #179)
/// — see [`dedup_canonical`] for the dedup key and surviving spelling.
pub(crate) fn collect_files(paths: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for p in paths {
        collect_php_files(Path::new(p), &mut files);
    }
    dedup_canonical(files)
}

/// Deduplicate `files` by real identity, first spelling wins (push order).
/// Issue #179: a symlinked dir made one tree reachable two ways; deduping by
/// path STRING double-declared classes (ADR-0049 existence guard). Dedup KEY
/// is [`Path::canonicalize`]; uncanonicalizable paths key on themselves.
pub(crate) fn dedup_canonical(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(files.len());
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let key = file.canonicalize().unwrap_or_else(|_| file.clone());
        if seen.insert(key) {
            out.push(file);
        }
    }
    // Re-sort for determinism (read_dir order is fs-dependent); selection already done.
    out.sort();
    out
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

pub(crate) fn collect_php_files(path: &Path, out: &mut Vec<PathBuf>) {
    collect_php_files_inner(path, out, &mut HashSet::new());
}

/// The walk `collect_php_files` fronts, plus a symlink cycle guard (issue
/// #179): `visited_dirs` resets per top-level call, so it stops loops but is
/// NOT the file-level dedup — [`dedup_canonical`] collapses cross-argument duplicates.
fn collect_php_files_inner(path: &Path, out: &mut Vec<PathBuf>, visited_dirs: &mut HashSet<PathBuf>) {
    if path.is_dir() {
        // Already-entered directory is a symlink cycle: stop. canonicalize()
        // failure is walked uncached; read_dir fails harmlessly if unreadable.
        if let Ok(canon) = path.canonicalize()
            && !visited_dirs.insert(canon)
        {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else { return };
        for entry in entries.flatten() {
            collect_php_files_inner(&entry.path(), out, visited_dirs);
        }
    } else if path.extension().is_some_and(|e| e == "php") {
        out.push(path.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- dedup_canonical (issue #179) --------------------------------------

    /// Two spellings of one file collapse to one entry, first-pushed surviving.
    /// E2e repro: `tests/symlink_dedup.rs`.
    #[test]
    fn dedup_canonical_collapses_two_spellings_keeping_the_first() {
        let dir = std::env::temp_dir()
            .join(format!("steins-dedup-canonical-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/a.php"), "<?php\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();

        let first = dir.join("real/a.php");
        let second = dir.join("link/a.php"); // same file, symlinked spelling
        let out = dedup_canonical(vec![first.clone(), second]);
        assert_eq!(out, vec![first], "one real file survives, spelled as first pushed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path whose `canonicalize()` fails is never dropped: keyed on its own
    /// literal path (pre-#179 behavior).
    #[test]
    fn dedup_canonical_keeps_uncanonicalizable_paths() {
        let a = PathBuf::from("/steins-dedup-canonical-unit-does-not-exist-a.php");
        let b = PathBuf::from("/steins-dedup-canonical-unit-does-not-exist-b.php");
        let out = dedup_canonical(vec![a.clone(), b.clone(), a.clone()]);
        // `a` dedups against its own repeat but not against unrelated `b`.
        assert_eq!(out, vec![a, b]);
    }
}
