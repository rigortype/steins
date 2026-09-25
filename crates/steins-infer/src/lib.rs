//! The inference engine — now whole-project (cross-file) resolution.
//!
//! It implements the proof-layer diagnostics (ADR-0002, held to the
//! zero-false-positive bar): [`ID`] = `type.argument-mismatch`, plus the
//! effect-envelope checks. A call to a **user-defined function or method
//! resolved anywhere in the project** that passes a **literal** argument which
//! **provably** raises a runtime `TypeError` under PHP 8.1+ semantics
//! (ADR-0011), honoring the calling file's `declare(strict_types=1)`, is
//! flagged. Everything not provable is silent.
//!
//! Name resolution follows PHP semantics conservatively (ADR-0001): fully-
//! qualified / qualified / unqualified names resolve against a project symbol
//! index ([`steins_db::project_index`]) plus the builtin catalog, with
//! `use` imports and the namespace/global fallback applied. Ambiguous symbols
//! (duplicate FQN, builtin-shadowing) are never resolved — silent.
//!
//! The single-file entry points ([`check`], [`check_file`], [`diagnostics`])
//! run over a one-file project, so every same-file soundness guard keeps
//! working unchanged; [`check_project`] / [`annotate_project`] run over many.

mod absence;
mod annotate;
mod arg_check;
mod arity;
mod array_out_state;
mod assert_harness;
mod asserts;
mod assign;
mod branch;
mod builtin_returns;
mod coerce;
mod compare;
mod cond;
mod contract;
mod cx;
pub mod dam;
mod declared_property;
mod declared_receiver;
mod descent;
mod dispatch;
mod docblock_hygiene;
mod dump;
pub mod effects;
mod env;
pub mod escapes;
mod existence;
mod fold;
mod fold_args;
#[cfg(not(target_arch = "wasm32"))]
mod fold_persist;
#[cfg(not(target_arch = "wasm32"))]
mod fold_process;
#[cfg(not(target_arch = "wasm32"))]
mod affected;
#[cfg(not(target_arch = "wasm32"))]
mod generation;
#[cfg(not(target_arch = "wasm32"))]
mod summaries;
mod facts;
mod fold_table;
mod foreach_bind;
mod foreach_check;
mod generics;
mod global_consts;
mod heap;
mod ids;
mod inaccessible;
mod mechanics;
mod method_call;
mod no_effect;
mod non_object;
mod offsets;
mod operands;
mod out_params;
mod overrides;
mod predicates;
pub mod profile;
mod project;
pub mod promote;
mod purity;
mod refine;
mod resource_folds;
mod return_arms;
mod return_maybe;
mod return_missing;
mod shape_projection;
mod shapes;
mod string_context;
pub mod suppress;
mod throws;
mod transfers;
mod undefined_var;
mod unknown_vocabulary;
mod untyped;
mod walk;
mod walk_fleet;
mod walk_plan;
mod fact_util;
mod file_walk;
mod fixpoints;
mod pipeline;
mod loops;

pub use dam::{DamFacts, DamKind, DamSite, dam_facts};
pub use ids::*;
pub use purity::{EffectSummary, RegionPurity, effect_summary, region_purity_project};
pub use absence::{SAPI_PROVIDED_FUNCTIONS_EXACT, SAPI_PROVIDED_FUNCTION_PREFIXES};
pub use annotate::{
    FactKind, LineFact, annotate_facts, annotate_file, annotate_project, effect_summaries_file,
    effect_summaries_project,
};
pub use assert_harness::{AssertObservation, SubjectFact, collect_assert_types, probe_subjects};
pub use project::{
    Diagnostic, FileUnit, Fix, FixEdit, LazyTree, MagicObstacle, is_vendor_path, magic_obstacles,
    magic_obstacles_reaching, resolves_to_user_function,
};

use project::Index;
pub use walk_fleet::WALK_WORKERS_ENV;
pub use walk_plan::Divergence;

pub(crate) use fact_util::{
    arg_abstract_fact, contract_touches_class, describe_fact, fact_admitting_null, fact_is_int,
    is_pure_class_contract, join_into, phpdoc_object_guard_blind, rendered_cval, val_of_key,
};
pub(crate) use fixpoints::{Fixpoints, Gate, Sym};
pub(crate) use pipeline::{check_units, check_units_controlled};

/// The `[runtime] final-keyword` posture (issue #234), re-exported so the CLI can
/// resolve `steins.toml` into [`RuntimePostures`] without depending on
/// steins-contract directly.
pub use steins_contract::normalize::FinalKeyword;

/// The `[runtime] os` pin (ADR-0094 §3): the deployment host a project declares,
/// which fixes `PHP_OS_FAMILY`, `PHP_EOL`, `DIRECTORY_SEPARATOR` and
/// `PATH_SEPARATOR` together. Absent, each is the union of what it can be.
pub use global_consts::OsFamily;

/// The `[runtime]` pseudo-constants a run analyzes under (ADR-0037 §2): boot
/// truths no amount of reading source settles, which the project declares and
/// Steins reasons under.
///
/// One value, resolved once at the caller's config boundary and passed whole to
/// every file's analysis context, so a new posture is a field here rather than
/// an argument at every layer between. The generation identity destructures it
/// without a rest pattern, so the field does not compile until it has an
/// identity row either — a run under a different value is a different run
/// (ADR-0092 §2).
///
/// These describe the *analyzed* runtime. The coverage posture (sidecar or sound
/// subset, ADR-0004) and a generation's engine posture describe the analyzing
/// engine instead, and live elsewhere.
///
/// [`Default`] is what declaring nothing means: `"abort"`, `"enforced"`, and no
/// OS pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePostures {
    /// `warning-handler` (ADR-0049 §7 amendment). `true` = `"abort"` (the
    /// owner-confirmed realistic-app default: a warning handler converts an
    /// `E_WARNING` to an exception/halts, so a *proven* warning is a proven
    /// runtime break — warning-grade offset findings emit). `false` = `"null"`:
    /// the application tolerates the warning and continues, so warning-grade
    /// offset findings stay silent (v1 simplification: the ADR-0050
    /// layer-demotion + value-side `null`/`""` adoption is deferred). The
    /// Error-grade `offset.on-unsupported` object case (not yet implemented) is
    /// posture-independent and would emit under both.
    pub warning_handler_abort: bool,
    /// `final-keyword` (issue #234) — what the runtime this project is analyzed
    /// for does with `final`. Read by the declared-receiver lane's intersection
    /// leg (issue #238) through [`steins_contract::normalize::provably_uninhabited`],
    /// and by nothing else: the posture governs *inhabitance*, never a `final`
    /// diagnostic (#234's own out-of-scope list). [`FinalKeyword::Enforced`] is
    /// the absence default, so a project declaring nothing gets the language's
    /// own rule.
    pub final_keyword: FinalKeyword,
    /// `os` (ADR-0094 §3) — the deployment host the project declares. Read only
    /// by the global-constant resolver, and only for the four constants a host
    /// fixes together (`PHP_OS_FAMILY`, `PHP_EOL`, `DIRECTORY_SEPARATOR`,
    /// `PATH_SEPARATOR`). `None` is the default and the sound one: the union of
    /// what each can be, since a library cannot assume its host.
    pub os_pin: Option<OsFamily>,
}

impl Default for RuntimePostures {
    fn default() -> Self {
        Self { warning_handler_abort: true, final_keyword: FinalKeyword::Enforced, os_pin: None }
    }
}

/// The catalog's refusal axis, re-exported: a consumer of [`SurfaceSummary`]
/// reads the classification without naming `steins-catalog`.
pub use steins_catalog::{RefusalAxis, ResourceParam};
pub use suppress::{
    DIAGNOSTIC_IDS, DIAGNOSTIC_REGISTRY, FACET_ORIGIN, Facet, Floor, INLINE_IGNORE, InlineOutcome,
    Layer, Origin, SUPPRESS_UNKNOWN_ID, SUPPRESS_UNMATCHED_ID, apply_inline_ignores,
    declared_facet, layer, pattern_is_known, pattern_matches, surface_floor,
};

use std::collections::HashMap;

use steins_db::{
    Db, EffectsPolicy, PluginFacts, Project, ProjectLayout, SourceFile, parse, project_index,
};
use steins_syntax::{FunctionDecl, SourceTree};
// return missing (ADR-0078, issue #199)
pub use steins_syntax::{BodyEnd, body_end, body_has_terminator};
pub use fold::{
    EngineFolder, FoldEngine, FoldLane, FoldPosture, FoldShapeRefusal, Folder,
    MONKEY_PATCH_EXTENSIONS, NoFold, RefusalNote, SurfaceSummary,
};
#[cfg(not(target_arch = "wasm32"))]
pub use fold_persist::{
    FOLD_IDENTITY_SECTION, FOLD_PACKAGE, FOLD_ROWS_SECTION, FoldHarvest, FoldTableArtifact,
    FoldTableIdentity, RecordingEngine, RecordingFolder, RunEngine, fold_package,
};
#[cfg(not(target_arch = "wasm32"))]
pub use fold_process::{ProcessEngine, SidecarFolder};
#[cfg(not(target_arch = "wasm32"))]
pub use generation::{
    FoldReport, GenerationError, GenerationMode, GenerationOutcome, GenerationParams,
    GenerationReport, PARANOID_ENV, PackageKind, PackageReport, PhaseTimings, SOURCES_SECTION,
    WalkReport, generation_check,
};
#[cfg(not(target_arch = "wasm32"))]
pub use summaries::SUMMARIES_SECTION;
pub use fold_table::{TableEngine, TableFolder, request_key};
// end return missing (ADR-0078, issue #199)

/// The maximum depth of interprocedural argument-binding descent (Feature B).
///
/// ADR-0009 makes inference cutoffs a first-class budget discipline: a chain of
/// calls propagating a literal is followed at most this many frames deep, after
/// which the descent stops with **no** diagnostic (a cutoff names itself as
/// silence, never a manufactured finding). Direct and indirect recursion is
/// caught earlier by the on-stack binding set; this bound guards against merely
/// long, non-cyclic chains.
pub const MAX_BINDING_DEPTH: usize = 8;

/// The one-line coverage-posture notice (ADR-0004): printed to stderr when a run
/// executes as the sound subset because the PHP sidecar is unavailable, and served
/// as the browser envelope's `notice` field for the engine-off playground (ADR-0065).
///
/// The second clause is ADR-0069's: with no engine to reflect them, a builtin's
/// return type comes from the catalog's mined declaration, which is a claim rather
/// than a runtime answer — so the sentence says so where the posture is stated.
pub const SOUND_SUBSET_NOTICE: &str = "note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified";

/// The notice for issue #110's degradation mode: PHP spawned but the opening
/// handshake or a later request stopped answering. This differs from
/// [`SOUND_SUBSET_NOTICE`], where PHP is unavailable; `steins doctor` diagnoses
/// the unresponsive-process case. [`ProcessEngine`] emits this at most once per
/// run, and it never changes the exit status.
pub const SIDECAR_HANDSHAKE_NOTICE: &str = "note: PHP sidecar stopped answering — running as sound subset (degraded): findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified; run `steins doctor` for detail";

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// The proof-layer diagnostics for one file, as a memoized salsa query (sound
/// subset — [`NoFold`], no PHP). Analyzes the file as a one-file project.
#[salsa::tracked]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    check_one_file(parse(db, file), file.path(db), &mut NoFold, RuntimePostures::default())
}

/// The folding-aware check for one file (run **outside** salsa; ADR-0004),
/// analyzed as a one-file project.
#[must_use]
pub fn check_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    check_one_file(parse(db, file), file.path(db), folder, RuntimePostures::default())
}

/// The folding-aware check for a whole **project** (ADR-0009/0015): every file
/// in `project` is analyzed as one unit, so cross-file calls, class chains, and
/// effects resolve. Resolution is driven by the salsa [`project_index`] query.
/// Runs under the default [`RuntimePostures`]; [`check_project_under`] takes the
/// ones a project declares.
#[must_use]
pub fn check_project(db: &dyn Db, project: Project, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    check_project_under(db, project, folder, RuntimePostures::default())
}

/// [`check_project`] under the `[runtime]` postures a project declares
/// (ADR-0037 §2), passed whole: the entry point for a caller that has resolved
/// `steins.toml`, and the one every `check_project_with_*` below delegates to.
#[must_use]
pub fn check_project_under(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
    postures: RuntimePostures,
) -> Vec<Diagnostic> {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    // One `LazyTree` per file, borrowing the database's own parse: the salsa
    // path holds every tree already, so nothing here is ever deferred.
    let lazy: Vec<LazyTree<'_>> =
        handles.iter().map(|&f| LazyTree::borrowed(parse(db, f))).collect();
    let units: Vec<FileUnit> = handles
        .iter()
        .zip(&lazy)
        .map(|(&f, tree)| FileUnit { path: f.path(db), tree })
        .collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);
    check_units(
        &units,
        &index,
        folder,
        postures,
        project.layout(db),
        project.plugins(db),
        project.effects(db),
    )
}

/// [`check_project`] with the `warning-handler` posture declared (ADR-0049 §7):
/// `warning_handler_abort` is `true` for the default `"abort"` — proven
/// warning-grade offset findings emit — and `false` for `"null"`, which silences
/// them. Every other posture keeps its default. (The former `zend_assertions` knob
/// was abolished by the 2026-07-25 owner ruling — `assert($expr)` is `Verified`
/// unconditionally.)
#[must_use]
pub fn check_project_with_runtime(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
) -> Vec<Diagnostic> {
    let postures = RuntimePostures { warning_handler_abort, ..RuntimePostures::default() };
    check_project_under(db, project, folder, postures)
}

/// [`check_project_with_runtime`] plus the `[runtime] final-keyword` posture
/// (issue #234, consumed by #238). The OS pin keeps its default.
#[must_use]
pub fn check_project_with_postures(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
    final_keyword: FinalKeyword,
) -> Vec<Diagnostic> {
    let postures =
        RuntimePostures { warning_handler_abort, final_keyword, ..RuntimePostures::default() };
    check_project_under(db, project, folder, postures)
}

/// [`check_project_with_postures`] plus the `[runtime] os` pin (ADR-0094 §3):
/// every [`RuntimePostures`] field spelled as an argument of its own.
#[must_use]
pub fn check_project_with_os(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
    final_keyword: FinalKeyword,
    os_pin: Option<OsFamily>,
) -> Vec<Diagnostic> {
    let postures = RuntimePostures { warning_handler_abort, final_keyword, os_pin };
    check_project_under(db, project, folder, postures)
}

/// The pure single-file check (sound subset). Kept for unit tests and callers
/// that never execute PHP. `functions` is accepted for signature stability; the
/// tree's own function list is authoritative.
#[must_use]
pub fn check(tree: &SourceTree, functions: &[FunctionDecl], path: &str) -> Vec<Diagnostic> {
    check_with(tree, functions, path, &mut NoFold)
}

/// The folding-aware single-file check core, analyzed as a one-file project.
#[must_use]
pub fn check_with(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<Diagnostic> {
    let _ = functions; // authoritative list comes from `tree.functions()`
    check_one_file(tree, path, folder, RuntimePostures::default())
}

/// The single-file check with a folder **and** the `warning-handler` posture
/// (`warning_handler_abort`, ADR-0049 §7). Kept for tests that must exercise both a
/// live folder (the offset family is gated on [`Folder::absence_family_available`],
/// ADR-0049 A9) and a chosen `warning-handler` posture. (The former `zend_assertions`
/// knob was abolished by the 2026-07-25 owner ruling — `assert($expr)` is `Verified`
/// unconditionally, so no runtime knob controls its stratum.)
#[must_use]
pub fn check_full(
    tree: &SourceTree,
    path: &str,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
) -> Vec<Diagnostic> {
    let postures = RuntimePostures { warning_handler_abort, ..RuntimePostures::default() };
    check_one_file(tree, path, folder, postures)
}

/// Every single-file entry point's body: `tree` analyzed as a one-file project
/// with its own index, the no-manifest layout, no plugin and no effects policy.
/// The entry points differ only in where the tree comes from and in the folder
/// and postures they pass.
fn check_one_file(
    tree: &SourceTree,
    path: &str,
    folder: &mut dyn Folder,
    postures: RuntimePostures,
) -> Vec<Diagnostic> {
    let lazy = LazyTree::borrowed(tree);
    let units = [FileUnit { path, tree: &lazy }];
    let index = Index::from_units(&units);
    check_units(
        &units,
        &index,
        folder,
        postures,
        &ProjectLayout::fallback(),
        &PluginFacts::none(),
        &EffectsPolicy::none(),
    )
}
