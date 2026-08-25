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

use std::path::PathBuf;

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

    // The store lives with the project: the outermost governing root, or the
    // working directory for a manifest-less tree.
    let store_root =
        layout.roots().last().map_or_else(|| cwd.clone(), |root| root.dir().to_path_buf());
    let partition = steins_db::partition::discover(&layout);
    let params = GenerationParams {
        store_root: &store_root,
        capture_root: &cwd,
        files,
        layout: &layout,
        partition: &partition,
        plugins: &plugins,
        effects,
        warning_handler_abort: postures.warning_handler_abort,
        final_keyword: postures.final_keyword,
        php: !no_php,
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
        "steins: experimental generations: {} run, {} package(s), {} file(s) loaded from artifacts, {} parsed; generation {}",
        match report.mode {
            GenerationMode::Cold => "cold",
            GenerationMode::Warm => "warm",
        },
        report.packages.len(),
        loaded_files,
        parsed_files,
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
