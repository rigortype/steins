//! The check pipeline: one run over a universe of [`FileUnit`]s, from the
//! whole-universe facts through every file's walk to the project-wide passes.
//! Every entry point in the crate root lands in [`check_units`]; the
//! frozen-generation path lands in [`check_units_controlled`] with a
//! [`WalkControl`], which is the same code path with the walk plan seam open.
//!
//! The order of the returned vector is part of that contract: parse failures
//! first, then each walked or replayed file's block in unit order at any
//! fan-out width, then the effects and throw passes, the unparsable-file filter
//! and the dedup.
//!
//! [`clock`] and [`ms`] are the phase ledger's one spelling of a timed span;
//! the run's fixpoint holder times its two fixpoints with them too.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use steins_db::{EffectsPolicy, PluginFacts, ProjectLayout};

use crate::dam::{DamFacts, DamRow};
use crate::file_walk::{FileSink, WalkInputs, fan_out};
use crate::fold::Folder;
use crate::fold_args::effective_php_view;
use crate::mechanics::emit_parse_failure;
use crate::project::{Diagnostic, FileUnit, Index};
use crate::purity::{PurityOracle, effect_diagnostics};
use crate::throws::throw_diagnostics;
use crate::walk_plan::{FilePlan, FileWalk, PassTimings, UniverseVerdict, WalkControl};
use crate::{Fixpoints, RuntimePostures, SYNTAX_UNPARSABLE_ID, facts};

/// The project checking core: direct + propagation passes over every file's
/// calls and scopes, then the one project-wide effects pass.
pub(crate) fn check_units(
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
///
/// The run is five phases, and the vector they build is ordered by them: the
/// universe ([`read_universe`]) and its parse failures, the first findings out
/// ([`emit_parse_failures`]); the plan ([`plan_files`]); the walk
/// ([`walk_files`]); the merge, in unit order ([`merge_blocks`]); and the
/// project-wide passes with the final filter and dedup ([`report`]).
///
/// [`walk_plan`]: crate::walk_plan
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_units_controlled(
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
    let universe = read_universe(units, folder, layout, facts);
    emit_parse_failures(units, &universe, &mut out);

    // The run's shared fixpoint holder (issue #489): the effect and throw
    // fixpoints are computed at most once here, lazily, and every internal
    // consumer — the purity oracle, `effect_diagnostics`, `throw_diagnostics` —
    // reads the same result. Each consumer keeps its own cheap gate, so a
    // project spelling none of the triggering constructs still pays nothing.
    let fixpoints = Fixpoints::new(units, index, plugins, policy, facts);
    passes.facts_ms = ms(t_facts);

    // The callable-purity oracle (ADR-0063 P3): the shared whole-project effect
    // fixpoint, consulted by every file's context, and built only when some
    // docblock actually spells a purity-bearing callable.
    let t_oracle = clock();
    let purity = PurityOracle::build(&fixpoints);
    let oracle_ms = ms(t_oracle);

    let plan = plan_files(control.as_deref_mut(), units.len(), index, &universe, purity.as_ref());
    let inputs = WalkInputs {
        units,
        index,
        dam: &universe.dam,
        unparsable: &universe.unparsable,
        postures,
        php_minor: universe.php_minor,
        catalog_skew: universe.catalog_skew,
        version_id: universe.version_id,
        purity: purity.as_ref(),
        layout,
        plugins,
        never_returning: &universe.never_returning,
    };

    let t_walk = clock();
    let sinks = walk_files(&inputs, folder, &plan, control.as_deref_mut());
    let uncovered_matches = merge_blocks(units, &plan, sinks, control.as_deref_mut(), &mut out);
    passes.walk_ms = ms(t_walk);

    let t_report = clock();
    report(&fixpoints, &uncovered_matches, &universe.unparsable, &mut out);
    // The two fixpoints are lazy and forced from three places (the oracle
    // above, and each reporting pass in `report`), so their own cost is
    // subtracted out of whichever span forced them rather than attributed to it.
    let (effects_ms, throws_ms) = fixpoints.spent();
    passes.effects_ms = effects_ms;
    passes.throws_ms = throws_ms;
    passes.report_ms = (oracle_ms + ms(t_report) - effects_ms - throws_ms).max(0.0);
    if let Some(control) = control {
        control.passes = passes;
    }
    out
}

/// What a run knows about the whole universe before any file walks: every
/// whole-universe verdict a walk reads except the purity oracle, which is
/// built on the fixpoints once this is in hand.
struct Universe<'u> {
    /// Per file, in unit order: its first parse error, if any, and its dam
    /// candidates.
    dam_rows: Vec<DamRow>,
    /// The whole-universe dam fact (ADR-0049 §2).
    dam: DamFacts,
    /// The analysis PHP view (issue #28): the effective minor and the catalog
    /// skew every file's context shares.
    php_minor: Option<(u16, u16)>,
    catalog_skew: bool,
    /// The `PHP_VERSION_ID` interval for the version-guard fold (issue #29).
    version_id: Option<(u32, Option<u32>)>,
    /// The files that did not parse (ADR-0079): none of their own passes run.
    unparsable: HashSet<&'u str>,
    /// The whole-run `type.return-missing` veto set (ADR-0078).
    never_returning: HashSet<String>,
}

/// Read the universe: the whole-universe facts every file's context shares,
/// each computed once per run, off the per-file `facts` where the run has them
/// and off the trees where it does not.
fn read_universe<'u>(
    units: &[FileUnit<'u>],
    folder: &mut dyn Folder,
    layout: &ProjectLayout,
    facts: &[facts::FileFacts],
) -> Universe<'u> {
    // The whole-universe dam fact (ADR-0049 §2): one query answer per run, shared by
    // every file's context. Consumed by the absence family's conditional-decl leg.
    let dam_rows: Vec<DamRow> = units
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

    // parse failure (ADR-0079, issue #180): the broken files, whose findings stop
    // at the one `emit_parse_failures` gives each.
    let unparsable: HashSet<&str> = units
        .iter()
        .enumerate()
        .filter(|(fi, _)| dam_rows[*fi].0.is_some())
        .map(|(_, u)| u.path)
        .collect();

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

    Universe { dam_rows, dam, php_minor, catalog_skew, version_id, unparsable, never_returning }
}

/// The run's first findings, before any file walks.
///
/// parse failure (ADR-0079, issue #180): `parse_errors()`'s first real consumer.
/// One finding per broken file at its first error, and then NOTHING else from
/// that file — its recovered tree may misattribute anything locally, and a
/// finding built on a misparse is the manufactured-FP shape ADR-0002 forbids
/// (§2.4). The declarations the recovery kept still sit in the index, where they
/// can only *silence* an absence claim, never fire one.
///
/// Vendor is NOT special here, only in the dam (§2.3): a broken vendor file
/// emits the finding too and it rides the CLI's ordinary vendor filter, exactly
/// as the ADR-0046 §2 presumption prescribes.
fn emit_parse_failures(units: &[FileUnit], universe: &Universe<'_>, out: &mut Vec<Diagnostic>) {
    let Universe { dam_rows, dam, .. } = universe;
    for (fi, u) in units.iter().enumerate() {
        emit_parse_failure(u.path, dam_rows[fi].0.as_ref(), dam.file_is_unparsable(u.path), out);
    }
}

/// The walk plan (issue #489 slice B): one [`FilePlan`] per unit. Every
/// whole-universe verdict a walk can read is in hand once the universe is read
/// and the purity oracle built, so this is the one point at which the planner
/// can be asked — and the plan it returns is per file, applied by the walk and
/// the merge and nowhere else. Without a control every file walks.
fn plan_files(
    control: Option<&mut WalkControl<'_>>,
    file_count: usize,
    index: &Index,
    universe: &Universe<'_>,
    purity: Option<&PurityOracle<'_>>,
) -> Vec<FilePlan> {
    let mut plan: Vec<FilePlan> = match control {
        Some(control) => {
            let verdict = UniverseVerdict {
                dam: &universe.dam,
                unparsable: sorted(universe.unparsable.iter().copied()),
                purity: purity.map(PurityOracle::impurity_answers),
                never_returning: sorted(universe.never_returning.iter().map(String::as_str)),
                php_minor: universe.php_minor,
                catalog_skew: universe.catalog_skew,
                version_id: universe.version_id,
                property_writes: {
                    let (names, computed) = index.property_write_table();
                    (sorted(names.iter().map(String::as_str)), computed)
                },
            };
            (control.planner)(&verdict)
        }
        None => Vec::new(),
    };
    plan.resize_with(file_count, || FilePlan::Walk);
    plan
}

/// The walk: every file the plan walks, each into a sink of its own, in place
/// on `folder` or fanned out over the control's fleet (issue #490). Returns
/// the sinks in unit order — `None` for a file that did not walk — and records
/// the width it walked at on the control.
fn walk_files(
    inputs: &WalkInputs<'_>,
    folder: &mut dyn Folder,
    plan: &[FilePlan],
    control: Option<&mut WalkControl<'_>>,
) -> Vec<Option<FileSink>> {
    let paranoid = control.as_deref().is_some_and(|c| c.paranoid);
    // Which files this run actually walks. A replayed block is not walked —
    // except under the verifier, which walks everything precisely so it has a
    // fresh answer to grade the replay against.
    let order: Vec<usize> = (0..inputs.units.len())
        .filter(|&fi| paranoid || matches!(plan[fi], FilePlan::Walk))
        .collect();

    // Per-file sinks, filled either in place or by the fan-out (issue #490),
    // and merged in unit order either way. The merge — not the walk — is
    // what decides the diagnostic vector, so the two paths produce the same
    // bytes by construction rather than by comparison.
    let mut sinks: Vec<Option<FileSink>> =
        std::iter::repeat_with(|| None).take(inputs.units.len()).collect();
    let fleet = control
        .as_deref()
        .and_then(|c| c.fleet)
        .filter(|fleet| fleet.width(order.len()) > 1);
    let workers = match fleet {
        Some(fleet) => fan_out(inputs, &order, fleet, &mut sinks),
        None => {
            for &fi in &order {
                sinks[fi] = Some(inputs.walk(folder, fi));
            }
            1
        }
    };
    if let Some(control) = control {
        control.workers = workers;
    }
    sinks
}

/// The merge: each file's block appended to `out` in unit order — a walked
/// file's sink, or a replayed file's persisted block — and recorded on the
/// control's ledger, where the paranoid verifier grades a replay against the
/// walk. Returns the run's `uncovered_matches`.
fn merge_blocks(
    units: &[FileUnit],
    plan: &[FilePlan],
    mut sinks: Vec<Option<FileSink>>,
    mut control: Option<&mut WalkControl<'_>>,
    out: &mut Vec<Diagnostic>,
) -> HashMap<usize, HashSet<u32>> {
    let paranoid = control.as_deref().is_some_and(|c| c.paranoid);
    // ADR-0088 §5 (issue #433): the dataflow walk's own verdict on which
    // default-less `match` statements do NOT cover their subject's Verified
    // domain, keyed by (file, span-start) — the same key the structural throw
    // scan's `ThrowKind::New` origin for the same construct carries (both trace
    // back to the same CST `Match` node). Populated here, read by
    // `throw_diagnostics` in `report`.
    let mut uncovered_matches: HashMap<usize, HashSet<u32>> = HashMap::new();
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
    uncovered_matches
}

/// The project-wide passes, once every file's block is in: the effects pass,
/// the throw system, then the unparsable-file filter and the dedup — the last
/// two touches the vector gets.
fn report(
    fixpoints: &Fixpoints<'_>,
    uncovered_matches: &HashMap<usize, HashSet<u32>>,
    unparsable: &HashSet<&str>,
    out: &mut Vec<Diagnostic>,
) {
    // --- Effects pass (ADR-0005), computed once over the whole project. ------
    out.extend(effect_diagnostics(fixpoints));

    // --- Throw system (ADR-0040/0007): `@throws` envelope + Liskov. ----------
    out.extend(throw_diagnostics(fixpoints, uncovered_matches));

    // parse failure (ADR-0079, issue #180): drop whatever the two project-wide
    // passes above attributed to a broken file. §2.4 is about the file, not about
    // which pass produced the finding.
    if !unparsable.is_empty() {
        out.retain(|d| d.id == SYNTAX_UNPARSABLE_ID || !unparsable.contains(d.path.as_str()));
    }

    dedup(out);
}

/// A monotonic instant, or `None` where the target has no clock.
///
/// `wasm32-unknown-unknown` has no time source and `Instant::now` **panics**
/// there, so the phase ledger — which is read by the generation orchestrator
/// and by nothing else — must not reach for one. The browser build measures
/// nothing and reports zeros, which is the honest answer for a target that
/// cannot measure.
pub(crate) fn clock() -> Option<Instant> {
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
pub(crate) fn ms(t: Option<Instant>) -> f64 {
    t.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0)
}

/// Collect an iterator of borrowed names into a sorted vector — the canonical
/// form every whole-universe verdict is digested in.
fn sorted<'a>(names: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut out: Vec<&str> = names.collect();
    out.sort_unstable();
    out
}

/// Drop exact-duplicate diagnostics, preserving first-occurrence order.
fn dedup(out: &mut Vec<Diagnostic>) {
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
}
