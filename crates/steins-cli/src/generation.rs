//! CLI wiring for the frozen-generation lifecycle (ADR-0092 §5) — how `steins
//! check` runs by default since issue #525. The orchestrator itself is
//! library-shaped in `steins_infer::generation_check` (shared with `cargo xtask
//! perf --warm`, `cargo xtask fp-gate` and, later, the MCP server per issue
//! #491); this module is only the `steins check` glue: resolve the run's
//! boundary inputs, call the orchestrator, and hand back what the unchanged
//! downstream pipeline needs.
//!
//! **The surface** (ADR-0020 amendment, issue #525). The lifecycle is on
//! unless `--no-cache` turns it off. There is no environment variable and no
//! per-run narration: the report a user reads is the same report either way,
//! and a cache that announces itself on every invocation is noise on every
//! invocation. Where the run's disposition genuinely wants looking at, that is
//! `steins doctor`'s store section.
//!
//! **Silence is a property, not a preference.** Every way this path can
//! degrade — an unopenable store, a source that moved under the seal, a
//! corrupt artifact, a failed publish — is cost-only by ADR-0092 §2's standing
//! invariant: a miss changes what a run *pays*, never what it *finds*. A
//! complaint about one would therefore be a complaint the user can do nothing
//! useful with, on a run whose answer is already correct. So this module
//! prints nothing of its own, and a degradation falls back to the ordinary
//! cold path whose stderr is then byte-for-byte the stderr of a run that never
//! had a cache at all — which is why the boundary notices below are *collected*
//! rather than printed: printing them before the orchestrator can fail is what
//! would double them on the fallback.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use steins_db::{EffectsPolicy, PluginFacts, ProjectLayout};
use steins_infer::{Diagnostic, GenerationParams, INLINE_IGNORE, LazyTree, generation_check};

use crate::config::RuntimePostures;
use crate::project::{LoadedProject, assemble_loaded, resolve_layout};

/// What the cached path hands the downstream pipeline: the same
/// [`LoadedProject`] shape the cold path builds (salsa view for `--fix` and
/// the baseline machinery — no parse forced), the generation findings, and the
/// orchestrator's owned trees so inline-ignore scanning re-parses nothing.
pub(crate) struct CachedRun {
    pub(crate) loaded: LoadedProject,
    pub(crate) findings: Vec<Diagnostic>,
    /// The run's tree handles, in slot order. Handles rather than trees since
    /// issue #516: a warm run decodes a file's tree only where something
    /// reaches it, and the inline-suppression scan below reaches only the files
    /// it has to.
    pub(crate) trees: Vec<(String, LazyTree<'static>)>,
    /// The paths whose text spells `@steins-ignore` at all. The inline scan
    /// must visit those (they can carry `suppress.unknown` /
    /// `suppress.unmatched` with no finding of their own) and the files a
    /// finding names, and nothing else — which is what keeps a warm run from
    /// decoding the whole universe on its way out.
    pub(crate) directive_files: HashSet<String>,
    /// The boundary notices this run owes stderr, already `steins: `-less and
    /// in the cold path's own order — plugin refusals, effect label
    /// vocabulary, attribution hygiene, `[runtime]` warnings. Collected rather
    /// than printed so a degradation prints them exactly once, from the cold
    /// path, instead of twice.
    pub(crate) notices: Vec<String>,
}

/// Run the generation lifecycle for this check invocation, or `None` to fall
/// back to the ordinary cold path. Prints nothing at all — see the module docs
/// — so a `None` leaves stderr untouched for `load_project` to fill exactly as
/// it would have on a machine that never had a store.
pub(crate) fn try_generation_check(
    files: &[PathBuf],
    paths: &[String],
    plugin_allow: Option<&[String]>,
    effects: &EffectsPolicy,
    postures: &RuntimePostures,
    no_php: bool,
    runtime_warnings: &[String],
) -> Option<CachedRun> {
    let cwd = std::env::current_dir().ok()?;
    let layout = resolve_layout(paths);
    let plugins = PluginFacts::discover(&layout, plugin_allow);
    // The cold path's order, kept here so the two stderrs are one stderr:
    // `load_project` prints plugin refusals, then the effect label vocabulary,
    // then attribution hygiene; `run_check` prints the `[runtime]` warnings
    // after it returns.
    let mut notices: Vec<String> = plugins.notices().to_vec();
    notices.extend(effects.label_notices(plugins.registry()));

    // Where the sealed capture keys this run's files from (issue #506). Not
    // the layout root: a manifest-less tree has no governing root at all, and
    // the two roots answer different questions anyway — the layout root says
    // who governs the code, the capture root only says what prefix the seal's
    // keys drop. The analyzed files are the only thing that always answers the
    // second one.
    let capture_root = capture_root(files, &cwd);
    let store_root = store_root(&layout, &capture_root);
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
    // A failure here is cost, never meaning (ADR-0092 §2), so it degrades to
    // the ordinary cold path in silence — and having printed nothing yet is
    // what lets the cold path own stderr whole.
    let outcome = generation_check(&params).ok()?;
    notices.extend(outcome.attribution_notices);
    notices.extend(runtime_warnings.iter().cloned());

    // The salsa view over the same sealed texts, in the same slot order.
    let entries: Vec<(String, String)> = outcome
        .trees
        .iter()
        .map(|(path, _)| (path.clone(), outcome.texts.get(path).cloned().unwrap_or_default()))
        .collect();
    let loaded = assemble_loaded(entries, layout, plugins, effects.clone());
    let directive_files: HashSet<String> = outcome
        .texts
        .iter()
        .filter(|(_, text)| text.contains(INLINE_IGNORE))
        .map(|(path, _)| path.clone())
        .collect();
    Some(CachedRun {
        loaded,
        findings: outcome.findings,
        trees: outcome.trees,
        directive_files,
        notices,
    })
}

/// Where a run's store lives: the outermost governing root, or — manifest-less
/// — beside the analyzed code rather than beside whoever happened to invoke us
/// (issue #506: the cache belongs to the tree it caches, not to the caller's
/// working directory). `steins doctor` reports through this same function, so
/// the store it looks for is the store `check` writes.
pub(crate) fn store_root(layout: &ProjectLayout, fallback: &Path) -> PathBuf {
    layout.roots().last().map_or_else(|| fallback.to_path_buf(), |root| root.dir().to_path_buf())
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
