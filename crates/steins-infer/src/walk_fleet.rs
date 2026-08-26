//! The per-file walk's fan-out (issue #490).
//!
//! [`check_units`] walks the universe one file at a time, and — since the
//! generation layer made loading cheap (ADR-0092 §5) — that loop is what a cold
//! run and a wide edit are made of. Each file's walk reads only shared,
//! immutable state plus two `&mut`s of its own (a [`Folder`] and a diagnostic
//! sink), so the loop fans out with no change to what any single walk does.
//!
//! **A folder is never shared.** Each worker hires one of its own, configured
//! for this run by the same closure, and retires it when its chunk is done —
//! the configuration is applied at the one place a folder can be born, so it
//! cannot be forgotten. That is not a stylistic preference: a resident
//! `SidecarFolder` reused across rayon-scheduled projects once kept the
//! previous project's `php_target`, which gates ADR-0056 curated-fact admission
//! and the whole absence family, and issue #63 was two sessions of triage into
//! a corpus count that swung run to run and was invisible under
//! `RAYON_NUM_THREADS=1` (`xtask/src/gate.rs::analyze_through_generations`
//! carries the full account).
//!
//! **A worker's folder still has to hand back what it recorded.** The fold
//! table is one generation-level artifact (ADR-0092 §4) and N folders record N
//! tables, so retiring a worker *harvests* it: the fleet keeps the harvests,
//! ordered by chunk rather than by whoever finished first, and the caller
//! merges them into the run's own folder. The fleet is generic over both the
//! folder and the harvest so no downcast is involved — the caller names both
//! types, and [`WalkFleet`] is the object-safe half the walk loop sees.
//!
//! **Ordering is not the fan-out's business.** A worker writes into per-file
//! sinks; the caller merges them in unit order afterwards, exactly where the
//! sequential loop appended them. So the diagnostic vector — and therefore the
//! tail's retain, the dedup, and every downstream hash — is indistinguishable
//! from a sequential run's, which is what `warm ≡ cold` and the perf harness's
//! determinism oracle check.
//!
//! [`check_units`]: crate::check_units

use crate::fold::Folder;

/// The worker count knob: `STEINS_WALK_WORKERS`.
///
/// Unset (or `0`) means the width is decided from the machine and from the
/// walk's own size. `1` is the sequential walk — the pre-issue-#490 path, not an
/// imitation of it — which is both the escape hatch for a machine that cannot
/// afford the parallel peak and the way a measurement pins the before/after.
/// Any other value is taken literally, so an operator can trade wall clock for
/// peak memory in either direction.
///
/// An environment variable rather than a config key, following
/// [`PARANOID_ENV`]: every caller of the orchestrator gets it without a
/// signature change, and CI — which never sets it — is unaffected.
///
/// [`PARANOID_ENV`]: crate::generation::PARANOID_ENV
pub const WALK_WORKERS_ENV: &str = "STEINS_WALK_WORKERS";

/// Files one worker must be given before a second worker earns its keep.
///
/// A worker is not free: it hires its own folder, and a folder over a live
/// engine spawns its own `php` child. Below this many files per worker the
/// spawn and the cold memos cost more than the walk they would overlap, which
/// is why a 41-file package stays sequential and a 341-file one does not.
const FILES_PER_WORKER: usize = 32;

/// The stack every walk thread gets — the same number and the same reason as
/// `steins-cli`'s `WORKER_STACK_SIZE` and `xtask`'s: CST recursion costs a
/// frame per nesting level and an overflow aborts the *process*, so a walk may
/// not run on a stock 2 MiB thread. Lazily committed, so the reservation costs
/// address space and not resident memory.
#[cfg(not(target_arch = "wasm32"))]
const WALK_STACK_SIZE: usize = 256 * 1024 * 1024;

/// The walk loop's view of a fleet: how wide to fan out, and how to borrow one
/// worker's folder for the duration of one chunk.
///
/// Object-safe on purpose — [`check_units`] holds it as a `&dyn` so the
/// checking core stays non-generic — and `Sync` because the fan-out shares one
/// fleet across every worker.
///
/// [`check_units`]: crate::check_units
pub(crate) trait WalkFleet: Sync {
    /// How many workers a walk of `files` files fans out over. `1` means the
    /// walk runs in place on the caller's own folder and this fleet is never
    /// asked for anything.
    fn width(&self, files: usize) -> usize;

    /// Hire a folder, run `job` on it, then retire it — harvesting whatever it
    /// recorded under `chunk`, which is the index the harvests are ordered by.
    fn run_chunk(&self, chunk: usize, job: &mut dyn FnMut(&mut dyn Folder));
}

/// How wide a walk may fan out: what the machine allows, and whether an
/// operator named the number instead.
///
/// Read once per run and carried, so the two questions it answers — the
/// ceiling, and the width for a given walk — cannot disagree about what the
/// environment said.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerBudget {
    ceiling: usize,
    /// The operator named the width ([`WALK_WORKERS_ENV`]), so
    /// [`Self::width`] takes it literally rather than trimming it to the
    /// universe.
    named: bool,
}

impl WorkerBudget {
    /// The budget this process runs under.
    #[must_use]
    pub(crate) fn read() -> Self {
        if let Some(asked) =
            std::env::var(WALK_WORKERS_ENV).ok().and_then(|v| v.parse::<usize>().ok())
            && asked > 0
        {
            return Self { ceiling: asked, named: true };
        }
        // Never more workers than hardware threads: peak memory is workers ×
        // one walk, and oversubscribing a CPU-bound loop buys nothing. A
        // machine that will not say how wide it is walks sequentially, which
        // is always correct.
        let machine =
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        Self { ceiling: machine, named: false }
    }

    /// How wide a walk of `files` files actually fans out.
    ///
    /// Trimmed to the universe unless the operator named a width: a walk of
    /// two files must not hire two folders (and so spawn two `php` children)
    /// merely because the machine has the threads for it — which is exactly
    /// what a small edit's rebuild looks like on an otherwise large project.
    #[must_use]
    pub(crate) fn width(self, files: usize) -> usize {
        let want = if self.named { files.max(1) } else { (files / FILES_PER_WORKER).max(1) };
        self.ceiling.min(want)
    }
}

// ---------------------------------------------------------------------------
// The native fleet and the pool its chunks run on.
// ---------------------------------------------------------------------------

/// A fleet of per-worker folders: one `hire` per chunk, one `retire` per chunk,
/// and the harvests kept in chunk order.
///
/// Generic over the folder `F` and the harvest `H` so the caller keeps both
/// concrete types — `F` never leaves the thread that hired it (so it need not
/// be `Send`), and only `H` crosses back.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct FolderFleet<'a, F, H> {
    budget: WorkerBudget,
    hire: &'a (dyn Fn() -> F + Sync),
    retire: &'a (dyn Fn(F) -> H + Sync),
    harvests: std::sync::Mutex<Vec<(usize, H)>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<'a, F, H> FolderFleet<'a, F, H> {
    pub(crate) fn new(
        budget: WorkerBudget,
        hire: &'a (dyn Fn() -> F + Sync),
        retire: &'a (dyn Fn(F) -> H + Sync),
    ) -> Self {
        Self { budget, hire, retire, harvests: std::sync::Mutex::new(Vec::new()) }
    }

    /// What the workers recorded, in chunk order rather than completion order
    /// — so a caller merging them (the fold table's rows) merges the same way
    /// every run, whatever the scheduler did.
    pub(crate) fn into_harvests(self) -> Vec<H> {
        let mut harvests =
            self.harvests.into_inner().expect("the harvest lock is never poisoned");
        harvests.sort_by_key(|(chunk, _)| *chunk);
        harvests.into_iter().map(|(_, h)| h).collect()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<F: Folder, H: Send> WalkFleet for FolderFleet<'_, F, H> {
    fn width(&self, files: usize) -> usize {
        self.budget.width(files)
    }

    fn run_chunk(&self, chunk: usize, job: &mut dyn FnMut(&mut dyn Folder)) {
        let mut folder = (self.hire)();
        job(&mut folder);
        // Retired here rather than kept alive to the end of the run: a folder
        // over a live engine holds a `php` child, and holding every worker's
        // child until the merge would make the peak the *sum* of the fleet
        // instead of the width of the fan-out.
        let harvest = (self.retire)(folder);
        self.harvests.lock().expect("the harvest lock is never poisoned").push((chunk, harvest));
    }
}

/// The process-wide pool every fan-out runs on, or `None` when one cannot be
/// built (the caller then walks sequentially).
///
/// **A pool of its own, deliberately.** The fan-out is nested inside callers
/// that already use rayon's global pool — `cargo xtask fp-gate` runs the corpus
/// packages in parallel and each of them reaches this loop — and a second level
/// of fan-out on the *same* pool would let one project's walk threads be
/// counted as another's. One dedicated pool, sized once, bounds the total
/// walk-thread count of the process no matter how many analyses are in flight:
/// a chunk holds its folder (and so its `php` child) only while it is running,
/// so the live-child count is bounded by this pool's width and not by the
/// number of concurrent projects.
///
/// Built once and shared for the process lifetime, because its threads are
/// expensive: [`WALK_STACK_SIZE`] each.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn walk_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads =
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("steins-walk-{i}"))
            .stack_size(WALK_STACK_SIZE)
            .build()
            .ok()
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The universe bound is a floor division, so a walk has to be worth a
    /// second worker before it hires one; the ceiling caps the rest.
    ///
    /// Constructed rather than read from the environment: `WorkerBudget::read`
    /// is process-wide state, and a test that read it would grade whatever the
    /// runner happened to export.
    #[test]
    fn an_unnamed_width_is_bounded_by_the_walk_and_by_the_ceiling() {
        let budget = WorkerBudget { ceiling: 8, named: false };
        // Sequential for anything under two workers' worth of files — the
        // small-edit rebuild, which must not hire a folder per file.
        for files in [0, 1, 2, 41, FILES_PER_WORKER * 2 - 1] {
            assert_eq!(budget.width(files), 1, "{files} files");
        }
        assert_eq!(budget.width(FILES_PER_WORKER * 3), 3);
        assert_eq!(budget.width(FILES_PER_WORKER * 10_000), 8);
    }

    /// A named width is taken literally — that is what naming it is for — but
    /// still never exceeds the walk itself: a chunk with no files would hire a
    /// folder to do nothing.
    #[test]
    fn a_named_width_is_bounded_only_by_the_walk() {
        let budget = WorkerBudget { ceiling: 4, named: true };
        assert_eq!(budget.width(2), 2);
        assert_eq!(budget.width(0), 1);
        assert_eq!(budget.width(10_000), 4);
        assert_eq!(WorkerBudget { ceiling: 1, named: true }.width(10_000), 1);
    }
}

/// The oracle the fan-out exists to keep: a walk spread over N workers
/// produces the run a walk in place produces, finding for finding and block
/// for block.
///
/// In-crate rather than in `tests/` because the width has to be *chosen* — a
/// fixture that relied on [`WorkerBudget::read`] would grade whatever the
/// runner happened to export, and one that relied on the file count would
/// grade the machine's core count. Here the budget is constructed, so the same
/// universe is walked at every width from one to one-file-per-worker and every
/// answer is compared against the sequential one.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod fan_out {
    use super::*;

    use steins_db::{EffectsPolicy, PluginFacts, ProjectLayout};
    use steins_syntax::SourceTree;

    use crate::project::{FileUnit, Index, LazyTree};
    use crate::walk_plan::{FilePlan, FileWalk, UniverseVerdict, WalkControl};
    use crate::{Diagnostic, FinalKeyword, NoFold};

    /// One declaration file plus callers that each violate it — so every
    /// caller's block is non-empty and the merge has something to get wrong,
    /// and so the walk has to resolve across files rather than within one.
    fn sources() -> Vec<(String, String)> {
        let mut out = vec![(
            "src/decl.php".to_owned(),
            "<?php\nfunction takesInt(int $n): int { return $n; }\n".to_owned(),
        )];
        for i in 0..16 {
            out.push((
                format!("src/call{i:02}.php"),
                format!("<?php\ntakesInt(\"nope{i}\");\ntakesInt({i}.5);\n"),
            ));
        }
        out
    }

    /// Walk the fixture at `width`, returning the findings and the per-file
    /// ledger, plus the width the loop actually used.
    fn walk_at(width: usize, named: bool) -> (Vec<Diagnostic>, Vec<FileWalk>, usize) {
        let sources = sources();
        let trees: Vec<LazyTree<'static>> =
            sources.iter().map(|(_, text)| LazyTree::ready(SourceTree::parse(text))).collect();
        let units: Vec<FileUnit<'_>> = sources
            .iter()
            .zip(&trees)
            .map(|((path, _), tree)| FileUnit { path, tree })
            .collect();
        let index = Index::from_units(&units);

        let hire = || NoFold;
        let retire = |_: NoFold| ();
        let fleet = FolderFleet::new(WorkerBudget { ceiling: width, named }, &hire, &retire);
        let mut planner = |_: &UniverseVerdict<'_>| -> Vec<FilePlan> { Vec::new() };
        let mut control = WalkControl::new(&mut planner, false, &[], Some(&fleet));
        let findings = crate::check_units_controlled(
            &units,
            &index,
            &mut NoFold,
            true,
            FinalKeyword::Enforced,
            &ProjectLayout::fallback(),
            &PluginFacts::none(),
            &EffectsPolicy::none(),
            Some(&mut control),
        );
        (findings, std::mem::take(&mut control.ledger), control.workers)
    }

    #[test]
    fn every_width_produces_the_sequential_run() {
        let (expected, expected_ledger, workers) = walk_at(1, true);
        assert_eq!(workers, 1, "a named width of one is the walk in place");
        assert!(!expected.is_empty(), "the fixture actually reports something");
        assert_eq!(expected_ledger.len(), sources().len(), "one block per file, in unit order");

        for width in 2..=sources().len() {
            let (findings, ledger, workers) = walk_at(width, true);
            assert_eq!(workers, width, "the loop fanned out to the width it was given");
            assert_eq!(findings, expected, "width {width} did not produce the sequential run");
            assert_eq!(ledger, expected_ledger, "width {width} recorded a different ledger");
        }
    }

    /// The universe bound applies to the *walk*, so a fixture this small is
    /// sequential at the default budget however wide the machine is — the
    /// property that keeps a small edit's rebuild from hiring a folder per
    /// file.
    #[test]
    fn a_small_walk_stays_in_place_under_the_default_budget() {
        let ceiling = WorkerBudget::read().ceiling;
        assert_eq!(walk_at(ceiling, false).2, 1);
    }
}
