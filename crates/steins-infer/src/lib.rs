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
mod fixpoints;

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

use absence::{check_undefined_class, check_undefined_constant};
use mechanics::{check_array_duplicate_keys, emit_parse_failure};
use overrides::check_declaration_fatals;
use return_missing::check_return_missing;

use arg_check::{implicit_null_accepted, is_type_error};
use generics::{check_callable_arg, check_phpdoc_param};

use cx::Cx;
use env::{Known, Store};
use project::Index;
use walk::{analyze_scope, in_dead};
use walk_fleet::WalkFleet;
pub use walk_fleet::WALK_WORKERS_ENV;
use walk_plan::{FilePlan, FileWalk, PassTimings, UniverseVerdict, WalkControl};
pub use walk_plan::Divergence;

use fold_args::effective_php_view;

pub(crate) use fact_util::{
    arg_abstract_fact, contract_touches_class, describe_fact, fact_admitting_null, fact_is_int,
    is_pure_class_contract, join_into, phpdoc_object_guard_blind, rendered_cval, val_of_key,
};
pub(crate) use fixpoints::{Fixpoints, Gate, Sym};

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

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use steins_db::{
    Db, EffectsPolicy, PluginFacts, Project, ProjectLayout, SourceFile, parse, project_index,
};
use steins_syntax::Span;
use steins_syntax::{ArgValue, FunctionDecl, SourceTree};
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

use docblock_hygiene::docblock_hygiene;
use purity::{PurityOracle, effect_diagnostics};
use throws::throw_diagnostics;
use undefined_var::{check_phpdoc_maybe_undefined, check_undefined_variables};
use unknown_vocabulary::{VocabularyAllowlist, unknown_vocabulary};
use untyped::untyped_surface;

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

/// The project checking core: direct + propagation passes over every file's
/// calls and scopes, then the one project-wide effects pass.
fn check_units(
    units: &[FileUnit],
    index: &Index,
    folder: &mut dyn Folder,
    postures: RuntimePostures,
    layout: &ProjectLayout,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
) -> Vec<Diagnostic> {
    check_units_controlled(units, index, folder, postures, layout, plugins, policy, None)
}

/// [`check_units`] with the walk plan seam of issue #489 slice B open.
///
/// `control` is `Some` only on the frozen-generation path, where the caller
/// holds a published generation and may replay a file's persisted walk block
/// instead of walking it (see [`walk_plan`] for why a block is the right unit
/// and what makes replaying one sound). With `control` `None` — every other
/// entry point, every ungated `steins check`, every test — the planner never
/// runs, every file walks, and nothing is recorded: the default behaviour is
/// byte-identical because it is the *same* code path, not a compared one.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn check_units_controlled(
    units: &[FileUnit],
    index: &Index,
    folder: &mut dyn Folder,
    postures: RuntimePostures,
    layout: &ProjectLayout,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
    mut control: Option<&mut WalkControl<'_>>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    // Issue #516: the analysis phase was one number, and the whole first move
    // of that issue is finding out which part of it is the wall. Each span is
    // recorded where it runs and handed back on the control, which only the
    // generation orchestrator holds — every other entry point passes `None`
    // and pays two `Instant::now()` calls per run for the arithmetic.
    let mut passes = PassTimings::default();
    let t_facts = clock();

    // This run's per-file facts (issue #516), or an empty slice on every path
    // but the generation orchestrator's. Where a file has them, the phases
    // below read them instead of its tree; where it does not, they read the
    // tree exactly as they always did — the two are the same value by
    // construction, and `FileFacts::from_tree` is the one producer.
    let facts: &[facts::FileFacts] = control.as_deref().map_or(&[], |c| c.facts);

    // The whole-universe dam fact (ADR-0049 §2): one query answer per run, shared by
    // every file's context. Consumed by the absence family's conditional-decl leg.
    let dam_rows: Vec<crate::dam::DamRow> = units
        .iter()
        .enumerate()
        .map(|(fi, u)| match facts.get(fi) {
            Some(f) => (f.parse_error.clone(), f.dynamism.clone()),
            None => (facts::parse_error_of(u.tree), facts::dam_candidates_of(u.path, u.tree)),
        })
        .collect();
    let dam = crate::dam::dam_facts_from(units, layout, &dam_rows);

    // The analysis PHP view (issue #28): the TARGET the project declares
    // (`config.platform.php` / `require.php`, via the layout) is what
    // version-sensitive decisions key on; the sidecar's runtime minor is the
    // fallback when the project declares nothing. One computation per run,
    // shared by every file's context — ADR-0052 A11 (catalog skew) and
    // ADR-0049 A12 (the next-int rule, through `normalize_array`) both follow
    // this one seam.
    let runtime_minor = folder.php_minor();
    let view = effective_php_view(runtime_minor, layout.php_target());
    let (php_minor, catalog_skew) = (view.effective_minor, view.catalog_skew);
    // The PHP_VERSION_ID guard fold (issue #29) is disabled project-wide the
    // moment any file declares a userland constant of that name — constant
    // resolution is otherwise unmodeled, so the conservative reading is the
    // only sound one.
    let version_id = if units.iter().enumerate().any(|(fi, u)| match facts.get(fi) {
        Some(f) => f.version_id_declared,
        None => u.tree.php_version_id_declared(),
    }) {
        None
    } else {
        view.version_id
    };

    // The run's shared fixpoint holder (issue #489): the effect and throw
    // fixpoints are computed at most once here, lazily, and every internal
    // consumer — the purity oracle, `effect_diagnostics`, `throw_diagnostics` —
    // reads the same result. Each consumer keeps its own cheap gate, so a
    // project spelling none of the triggering constructs still pays nothing.
    let fixpoints = Fixpoints::new(units, index, plugins, policy, facts);

    // The callable-purity oracle (ADR-0063 P3): the shared whole-project effect
    // fixpoint, consulted by every file's context, and built only when some
    // docblock actually spells a purity-bearing callable.
    passes.facts_ms += ms(t_facts);
    let t_oracle = clock();
    let purity = PurityOracle::build(&fixpoints);
    let oracle_ms = ms(t_oracle);
    let t_facts = clock();

    // parse failure (ADR-0079, issue #180): `parse_errors()`'s first real consumer.
    // One finding per broken file at its first error, and then NOTHING else from
    // that file — its recovered tree may misattribute anything locally, and a
    // finding built on a misparse is the manufactured-FP shape ADR-0002 forbids
    // (§2.4). The declarations the recovery kept still sit in the index, where they
    // can only *silence* an absence claim, never fire one.
    //
    // Vendor is NOT special here, only in the dam (§2.3): a broken vendor file
    // emits the finding too and it rides the CLI's ordinary vendor filter, exactly
    // as the ADR-0046 §2 presumption prescribes.
    for (fi, u) in units.iter().enumerate() {
        emit_parse_failure(u.path, dam_rows[fi].0.as_ref(), dam.file_is_unparsable(u.path), &mut out);
    }
    let unparsable: HashSet<&str> = units
        .iter()
        .enumerate()
        .filter(|(fi, _)| dam_rows[*fi].0.is_some())
        .map(|(_, u)| u.path)
        .collect();
    // end parse failure (ADR-0079, issue #180)

    // return missing (ADR-0078, issue #199): the whole-run veto set, computed once
    // because a never-returning helper is routinely declared in a different file
    // from the body that calls it.
    let never_returning: HashSet<String> = units
        .iter()
        .enumerate()
        .flat_map(|(fi, u)| match facts.get(fi) {
            Some(f) => f.never_returning.clone(),
            None => facts::never_returning_of(u.tree),
        })
        .collect();
    // end return missing (ADR-0078, issue #199)

    // ADR-0088 §5 (issue #433): the dataflow walk's own verdict on which
    // default-less `match` statements do NOT cover their subject's Verified
    // domain, keyed by (file, span-start) — the same key the structural throw
    // scan's `ThrowKind::New` origin for the same construct carries (both trace
    // back to the same CST `Match` node). Populated below, read by
    // `throw_diagnostics` at the end.
    let mut uncovered_matches: HashMap<usize, HashSet<u32>> = HashMap::new();
    passes.facts_ms += ms(t_facts);

    // The walk plan (issue #489 slice B). Every whole-universe verdict a walk
    // can read is in hand by now, so this is the one point at which the
    // planner can be asked — and the plan it returns is per file, applied
    // inside the loop below and nowhere else.
    let mut plan: Vec<FilePlan> = match control.as_deref_mut() {
        Some(control) => {
            let verdict = UniverseVerdict {
                dam: &dam,
                unparsable: sorted(unparsable.iter().copied()),
                purity: purity.as_ref().map(PurityOracle::impurity_answers),
                never_returning: sorted(never_returning.iter().map(String::as_str)),
                php_minor,
                catalog_skew,
                version_id,
                property_writes: {
                    let (names, computed) = index.property_write_table();
                    (sorted(names.iter().map(String::as_str)), computed)
                },
            };
            (control.planner)(&verdict)
        }
        None => Vec::new(),
    };
    plan.resize_with(units.len(), || FilePlan::Walk);

    let paranoid = control.as_deref().is_some_and(|c| c.paranoid);
    let inputs = WalkInputs {
        units,
        index,
        dam: &dam,
        unparsable: &unparsable,
        postures,
        php_minor,
        catalog_skew,
        version_id,
        purity: purity.as_ref(),
        layout,
        plugins,
        never_returning: &never_returning,
    };
    // Which files this run actually walks. A replayed block is not walked —
    // except under the verifier, which walks everything precisely so it has a
    // fresh answer to grade the replay against.
    let order: Vec<usize> = (0..units.len())
        .filter(|&fi| paranoid || matches!(plan[fi], FilePlan::Walk))
        .collect();

    let t_walk = clock();
    // Per-file sinks, filled either in place or by the fan-out (issue #490),
    // and merged below in unit order either way. The merge — not the walk — is
    // what decides the diagnostic vector, so the two paths produce the same
    // bytes by construction rather than by comparison.
    let mut sinks: Vec<Option<FileSink>> =
        std::iter::repeat_with(|| None).take(units.len()).collect();
    let fleet = control
        .as_deref()
        .and_then(|c| c.fleet)
        .filter(|fleet| fleet.width(order.len()) > 1);
    let workers = match fleet {
        Some(fleet) => fan_out(&inputs, &order, fleet, &mut sinks),
        None => {
            for &fi in &order {
                sinks[fi] = Some(inputs.walk(folder, fi));
            }
            1
        }
    };
    if let Some(control) = control.as_deref_mut() {
        control.workers = workers;
    }

    for fi in 0..units.len() {
        let before = out.len();
        // A replayed file's block is appended verbatim, in the very position
        // the walk would have appended it — which is what makes the whole
        // vector (and so the tail's retain and dedup) indistinguishable.
        // Paranoid mode walks anyway and keeps the walked answer; the replayed
        // one is only ever the thing being graded.
        let replayed = match &plan[fi] {
            FilePlan::Walk => None,
            FilePlan::Replay(block) => Some(block),
        };
        if let Some(block) = replayed
            && !paranoid
        {
            out.extend_from_slice(&block.diagnostics);
            if let Some(uncovered) = &block.uncovered {
                uncovered_matches.insert(fi, uncovered.iter().copied().collect());
            }
            if let Some(control) = control.as_deref_mut() {
                control.replayed += 1;
                control.would_skip += 1;
                control.ledger.push(block.clone());
            }
            continue;
        }
        let sink = sinks[fi].take().expect("every walked file left a sink");
        out.extend(sink.diagnostics);
        let uncovered_entry = sink.uncovered;
        if let Some(uncovered) = &uncovered_entry {
            uncovered_matches.insert(fi, uncovered.iter().copied().collect());
        }
        if let Some(control) = control.as_deref_mut() {
            let walked = FileWalk {
                diagnostics: out[before..].to_vec(),
                uncovered: uncovered_entry,
            };
            control.walked += 1;
            if let Some(block) = replayed {
                control.would_skip += 1;
                control.verify(units[fi].path, block, &walked);
            }
            control.ledger.push(walked);
        }
    }

    passes.walk_ms = ms(t_walk);
    let t_report = clock();

    // --- Effects pass (ADR-0005), computed once over the whole project. ------
    out.extend(effect_diagnostics(&fixpoints));

    // --- Throw system (ADR-0040/0007): `@throws` envelope + Liskov. ----------
    out.extend(throw_diagnostics(&fixpoints, &uncovered_matches));

    // parse failure (ADR-0079, issue #180): drop whatever the two project-wide
    // passes above attributed to a broken file. §2.4 is about the file, not about
    // which pass produced the finding.
    if !unparsable.is_empty() {
        out.retain(|d| d.id == SYNTAX_UNPARSABLE_ID || !unparsable.contains(d.path.as_str()));
    }

    dedup(&mut out);
    // The two fixpoints are lazy and forced from three places (the oracle
    // above, and each reporting pass here), so their own cost is subtracted
    // out of whichever span forced them rather than attributed to it.
    let (effects_ms, throws_ms) = fixpoints.spent();
    passes.effects_ms = effects_ms;
    passes.throws_ms = throws_ms;
    passes.report_ms = (oracle_ms + ms(t_report) - effects_ms - throws_ms).max(0.0);
    if let Some(control) = control {
        control.passes = passes;
    }
    out
}

/// A monotonic instant, or `None` where the target has no clock.
///
/// `wasm32-unknown-unknown` has no time source and `Instant::now` **panics**
/// there, so the phase ledger — which is read by the generation orchestrator
/// and by nothing else — must not reach for one. The browser build measures
/// nothing and reports zeros, which is the honest answer for a target that
/// cannot measure.
fn clock() -> Option<Instant> {
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(Instant::now())
    }
}

/// Milliseconds since `t`, the one spelling the phase ledger uses. Zero for a
/// target with no clock.
fn ms(t: Option<Instant>) -> f64 {
    t.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0)
}

/// Collect an iterator of borrowed names into a sorted vector — the canonical
/// form every whole-universe verdict is digested in.
fn sorted<'a>(names: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut out: Vec<&str> = names.collect();
    out.sort_unstable();
    out
}

/// What one file's walk produced, before it is merged into the run's vector:
/// the diagnostics the block appended and the `uncovered_matches` entry it
/// made. The same two values [`FileWalk`] records — this is the in-flight form,
/// held per file so the walk can run out of unit order and the merge can put it
/// back in.
struct FileSink {
    diagnostics: Vec<Diagnostic>,
    uncovered: Option<Vec<u32>>,
}

/// Everything one file's walk reads that is neither its own `&mut` state nor
/// the file index: the run's shared, immutable inputs, gathered so the fan-out
/// (issue #490) has exactly one thing to share and one thing to prove `Sync`.
///
/// Every field is a shared borrow or a `Copy` scalar. That is the whole
/// argument for fanning the loop out: a walk mutates a [`Folder`] and a
/// diagnostic sink, both of which the worker owns, and reads this — which no
/// walk can change.
struct WalkInputs<'a> {
    units: &'a [FileUnit<'a>],
    index: &'a Index,
    dam: &'a DamFacts,
    unparsable: &'a HashSet<&'a str>,
    postures: RuntimePostures,
    php_minor: Option<(u16, u16)>,
    catalog_skew: bool,
    version_id: Option<(u32, Option<u32>)>,
    purity: Option<&'a PurityOracle<'a>>,
    layout: &'a ProjectLayout,
    /// The loaded plugin channel (ADR-0068). Read by
    /// `phpdoc.unknown-vocabulary`'s allowlist, which ADR-0091 §4.1 requires be
    /// assembled after plugin load.
    plugins: &'a PluginFacts,
    never_returning: &'a HashSet<String>,
}

impl WalkInputs<'_> {
    /// Walk unit `fi` on `folder`, into a sink of its own.
    fn walk(&self, folder: &mut dyn Folder, fi: usize) -> FileSink {
        let mut diagnostics = Vec::new();
        let uncovered = walk_one_file(
            self.units,
            self.index,
            folder,
            fi,
            self.dam,
            self.unparsable,
            self.postures,
            self.php_minor,
            self.catalog_skew,
            self.version_id,
            self.purity,
            self.layout,
            self.plugins,
            self.never_returning,
            &mut diagnostics,
        );
        FileSink { diagnostics, uncovered }
    }
}

/// The shared inputs really are shared: if any of them ever grows interior
/// mutability, the fan-out stops compiling here rather than racing at runtime.
/// Named individually rather than only through [`WalkInputs`] so the failure
/// says *which* input moved.
const _: () = {
    const fn sync<T: Sync + ?Sized>() {}
    sync::<Index>();
    sync::<DamFacts>();
    sync::<ProjectLayout>();
    sync::<SourceTree>();
    sync::<LazyTree<'_>>();
    sync::<[FileUnit<'_>]>();
    sync::<PurityOracle<'_>>();
    sync::<PluginFacts>();
    sync::<WalkInputs<'_>>();
};

/// Walk `order` across `fleet`'s workers, filling each file's own sink
/// (issue #490).
///
/// The universe is cut into one contiguous chunk per worker rather than
/// scheduled file by file, and that is deliberate on two counts: a chunk hires
/// exactly one folder, so the fleet's live folders — and so its `php` children
/// — are bounded by the fan-out's width whatever the scheduler does; and
/// contiguous slots are mostly one package, so two workers rarely contend for
/// the same artifact reader.
///
/// Nothing here decides the diagnostic vector: the caller merges the sinks in
/// unit order afterwards, exactly where the sequential loop appended them.
/// Returns the width it actually fanned out to, which the ledger reports.
#[cfg(not(target_arch = "wasm32"))]
fn fan_out(
    inputs: &WalkInputs<'_>,
    order: &[usize],
    fleet: &dyn WalkFleet,
    sinks: &mut [Option<FileSink>],
) -> usize {
    use rayon::prelude::*;

    let Some(pool) = walk_fleet::walk_pool() else {
        // No pool to be had. The caller's own folder is not reachable from
        // here, so the honest answer is one of the fleet's own, walking the
        // whole order on this thread — the sequential answer, one folder over.
        fleet.run_chunk(0, &mut |folder| {
            for &fi in order {
                sinks[fi] = Some(inputs.walk(folder, fi));
            }
        });
        return 1;
    };
    let workers = fleet.width(order.len());
    let chunks: Vec<(usize, &[usize])> =
        order.chunks(order.len().div_ceil(workers)).enumerate().collect();
    let walked: Vec<Vec<(usize, FileSink)>> = pool.install(|| {
        chunks
            .into_par_iter()
            .map(|(chunk, slots)| {
                let mut produced = Vec::with_capacity(slots.len());
                fleet.run_chunk(chunk, &mut |folder| {
                    for &fi in slots {
                        produced.push((fi, inputs.walk(folder, fi)));
                    }
                });
                produced
            })
            .collect()
    });
    for (fi, sink) in walked.into_iter().flatten() {
        sinks[fi] = Some(sink);
    }
    workers
}

/// The browser build has no threads and never carries a fleet, so the fan-out
/// is a walk in place on the one folder a chunk is given.
#[cfg(target_arch = "wasm32")]
fn fan_out(
    inputs: &WalkInputs<'_>,
    order: &[usize],
    fleet: &dyn WalkFleet,
    sinks: &mut [Option<FileSink>],
) -> usize {
    fleet.run_chunk(0, &mut |folder| {
        for &fi in order {
            sinks[fi] = Some(inputs.walk(folder, fi));
        }
    });
    1
}

/// One file's walk block, lifted out of [`check_units_controlled`]'s loop
/// verbatim so the replay seam has something to be a peer of. Appends
/// everything the block produces to `out` and returns the `uncovered_matches`
/// entry it makes — `None` for an unparsable file, which makes none.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn walk_one_file(
    units: &[FileUnit],
    index: &Index,
    folder: &mut dyn Folder,
    fi: usize,
    dam: &DamFacts,
    unparsable: &HashSet<&str>,
    postures: RuntimePostures,
    php_minor: Option<(u16, u16)>,
    catalog_skew: bool,
    version_id: Option<(u32, Option<u32>)>,
    purity: Option<&PurityOracle<'_>>,
    layout: &ProjectLayout,
    plugins: &PluginFacts,
    never_returning: &HashSet<String>,
    out: &mut Vec<Diagnostic>,
) -> Option<Vec<u32>> {
    {
        // parse failure (ADR-0079, issue #180): the broken file's own passes do not
        // run at all. The project-wide passes below (effects, throws) are filtered
        // by path instead — they walk the whole universe in one go, so there is no
        // per-file switch to turn off there.
        if unparsable.contains(units[fi].path) {
            return None;
        }
        let cx = Cx::new_with(
            units,
            index,
            fi,
            dam,
            postures,
            php_minor,
            catalog_skew,
            version_id,
            purity,
            layout.php_target(),
        );

        // --- Propagation pass FIRST: it walks every scope and, as a side
        // product, proves dead regions (decided branches, unreachable tails) —
        // the env-free direct pass below must not report inside them
        // (live-path discipline, ADR-0002/0031). Binding descents contribute
        // nothing here: their deadness is per-binding, not universal. ---------
        let mut dead_spans: Vec<Span> = Vec::new();
        let mut uncovered_spans: Vec<Span> = Vec::new();
        for scope in cx.tree().scopes() {
            analyze_scope(
                &cx,
                folder,
                scope,
                HashMap::new(),
                Store::default(),
                None,
                None,
                None,
                Some(&mut dead_spans),
                Some(&mut uncovered_spans),
                None,
                None,
                out,
            );
        }
        // Sorted and deduplicated: the consumer builds a `HashSet` from this,
        // so the canonical form costs nothing and makes the persisted row and
        // the verifier's comparison order-free.
        let mut uncovered_entry: Vec<u32> = uncovered_spans.iter().map(|s| s.start).collect();
        uncovered_entry.sort_unstable();
        uncovered_entry.dedup();

        // --- The `class.undefined` pass (ADR-0049 §5 / S4): the file's hard-error
        // class references, judged once each. A reference in a proven-dead region is
        // skipped — which IS this id's guard leg (a `class_exists('X')` whose class
        // meets the firing conditions folds its branch dead under the same closure).
        for r in cx.tree().hard_class_refs() {
            if in_dead(&dead_spans, r.offset) {
                continue;
            }
            check_undefined_class(&cx, folder, r, out);
        }

        // --- The `constant.undefined` pass (ADR-0078, issue #198): the file's bare
        // constant fetches, judged once each, with the same dead-region skip — which
        // IS this id's guard leg, exactly as it is for `class.undefined` above.
        for r in cx.tree().const_refs() {
            if in_dead(&dead_spans, r.offset) {
                continue;
            }
            check_undefined_constant(&cx, folder, r, out);
        }

        // --- `array.duplicate-key` (ADR-0078, issue #187): every literal array
        // in the file, judged once each. No dead-region gate — unlike the
        // passes above, this is a mechanics finding about how the literal is
        // WRITTEN, not a proof of a live runtime path, so it fires the same
        // whether or not the array is ever reached. -----------------------
        check_array_duplicate_keys(&cx, out);

        // --- The declaration-fatal pass (ADR-0078 / issue #183): the file's own
        // class-like declarations, judged against the enumerated declaration graph.
        // Sidecar-free (a positive claim about resolved declarations, not an absence
        // of a symbol) and dam-free (the immunity asymmetry — no runtime construct
        // adds a method to a declared class), so it runs beside the pass above
        // without borrowing its ladder. -------------------------------------------
        check_declaration_fatals(&cx, &dead_spans, out);

        // --- Docblock hygiene (ADR-0078 / issue #186): the mechanics-layer
        // anti-rot family. Textual premises only — no env, no folder, no dead-region
        // filter: an annotation that names a subject the code no longer has is rot
        // wherever it sits, including in a branch that never runs.
        docblock_hygiene(&cx, out);

        // --- `phpdoc.unknown-vocabulary` (ADR-0091 §6 / issue #479): a
        // hyphenated identifier in a type position that denotes nothing. Reads
        // the docblock and the loaded plugin channel, nothing else — the claim
        // is about what the annotation can possibly mean, so no env, no folder,
        // no dead-region filter, exactly as the hygiene family above. The
        // allowlist is assembled here because it is a per-project value: the
        // question must be asked after plugin load (ADR-0091 §4.1).
        unknown_vocabulary(&cx, &VocabularyAllowlist::for_project(plugins), out);

        // --- The untyped surface (ADR-0078 / issue #200): the contract-layer
        // `untyped.*` family. Declaration reading only — no env, no folder, no
        // dead-region filter, no sidecar: a declaration that withholds its type
        // withholds it wherever it sits. -----------------------------------------
        untyped_surface(&cx, out);

        // --- `type.return-missing` (ADR-0078 / issue #199): the reachability
        // foundation's tracer. Declaration premise plus a structural terminality
        // verdict, so — like the two passes above — no env, no folder, no
        // dead-region filter: a body that runs off its end does so wherever it
        // sits, and the judgement is about the body's own shape.
        check_return_missing(&cx, never_returning, out);

        // --- `variable.undefined` (ADR-0078 / issue #194): every read of a name
        // its scope never binds. A per-scope textual/structural pass over the
        // lowering-computed firing set, plus the warning-handler posture and the
        // out-parameter subtraction. No dead-region filter and no folder: the
        // premise is that the scope's own text holds no binding form, which is
        // true wherever the read sits. -------------------------------------------
        check_undefined_variables(&cx, out);
        check_phpdoc_maybe_undefined(&cx, out);

        // --- Direct pass: literal / array / `new` arguments at every function
        // call site (env-free; propagation adds `$var`/folded resolution). Native
        // scalar checks and the phpdoc declared-contract check both run here; a
        // site where the native check fired is skipped by the phpdoc check (no
        // double-report; ADR-0030). Calls in proven-dead regions are skipped. ---
        let empty_env: HashMap<String, Known> = HashMap::new();
        let empty_classes: Store = Store::default();
        for call in cx.tree().calls() {
            if in_dead(&dead_spans, call.span.start) {
                continue;
            }
            // Resolve the positional prefix of a mixed call too (Gap A) — the guard
            // that keeps the binding descent positional-only lives on the descent path.
            let Some(site) = cx.resolve_user_fn_any(call) else { continue };
            let decl = cx.fn_decl(site);
            let envelopes = cx.envelopes_of(decl.docblock.as_deref(), site.file, decl.span.start);
            for (i, arg) in call.args.iter().enumerate() {
                let Some(param) = decl.params.get(i) else { break };
                if param.variadic {
                    break;
                }
                if param.by_ref {
                    continue;
                }
                let mut native_fired = false;
                // Env-free resolution: a literal, a proven object (`new` / enum
                // case), or a resolved class constant (ADR-0043 stage 3). At file
                // scope there is no enclosing class for `self`/`parent`.
                if let Some(ty) = param.ty.as_ref()
                    && let Some(checkable) = cx.resolve_static_value(&arg.value, None)
                    && is_type_error(&cx, ty, &checkable)
                    && !implicit_null_accepted(param, &checkable)
                {
                    out.push(cx.diagnostic(
                        arg.span.start,
                        &checkable,
                        None,
                        &decl.name,
                        &param.name,
                        ty,
                    ));
                    native_fired = true;
                }
                // The direct pass owns env-free arg kinds (literal / array / `new`,
                // plus enum-case / class-const object values — ADR-0043 stage 4);
                // `$var`/`call()` resolution — and their phpdoc check — belong to the
                // propagation pass, so the two never both fire on one arg.
                let env_free = arg.value.is_literal()
                    || matches!(
                        arg.value,
                        ArgValue::Array(_) | ArgValue::New(..) | ArgValue::EnumCase(..) | ArgValue::ClassConst(..)
                    );
                if !native_fired
                    && env_free
                    && let Some(env) = &envelopes
                {
                    check_phpdoc_param(
                        &cx,
                        folder,
                        env,
                        param,
                        site.file,
                        decl.span.start,
                        &decl.name,
                        arg.span.start,
                        &arg.value,
                        &empty_env,
                        &empty_classes,
                        false,
                        false, // in_descent — the direct pass is never a descent
                        out,
                    );
                }
                // Callable-signature variance (issue #11): a closure / first-class
                // callable argument against a signature-bearing `callable(...)`
                // @param. Env-free (a closure's declared signature is a static CST
                // fact), so the direct pass owns it — no overlap with the
                // propagation pass, which owns `$var`/`call()` arg kinds.
                if let ArgValue::Closure(closure) = &arg.value
                    && let Some(env) = &envelopes
                {
                    check_callable_arg(&cx, env, param, &decl.name, arg.span.start, closure, out);
                }
            }
        }

        Some(uncovered_entry)
    }
}

/// Drop exact-duplicate diagnostics, preserving first-occurrence order.
fn dedup(out: &mut Vec<Diagnostic>) {
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
}
