//! The walk plan and its paranoid verifier (ADR-0092 §5, issue #489 slice B).
//!
//! [`check_units`] walks every file of the universe and appends what each
//! file's block produced to one diagnostic vector. Slice B lets a caller that
//! holds a published generation *replay* a file's persisted block instead of
//! walking it. Everything that makes that sound lives elsewhere — the affected
//! set ([`crate::affected`]) decides which files may replay, the `summaries`
//! section ([`crate::summaries`]) holds what they replay — and this module is
//! the seam the two meet at, plus the instrument that grades them.
//!
//! **The seam is a range, not a rewrite.** A file's walk contributes one
//! contiguous range to the run's diagnostic vector and at most one entry to
//! the run's `uncovered_matches` map; nothing else in [`check_units`] reads
//! per-file walk state. So a replay that reproduces a file's *range* and its
//! *map entry* reproduces the whole run: the ranges are appended in unit
//! order either way, the two project-wide passes (effects, throws) recompute
//! whole-universe from own-rows regardless, and the tail — the unparsable
//! retain and the dedup — sees a vector it cannot tell apart. That is the
//! entire correctness argument for the replay path, and it is why
//! [`FileWalk`] stores a range's worth of owned diagnostics rather than
//! anything reconstructed.
//!
//! **The verifier came first, deliberately.** [`WalkControl::paranoid`] walks
//! every file *anyway*, keeps the walked answer, and compares it against what
//! the plan would have replayed — naming the first divergence with its file
//! and finding. It was built before any skipping existed (where it trivially
//! reports zero would-skips), because an instrument written after the thing
//! it measures grades its author's homework. It is not fixture-shaped: it
//! holds one file's two answers at a time and reports counts, so it runs over
//! a whole corpus tree.

use std::fmt;

use crate::Diagnostic;
use crate::dam::DamFacts;
use crate::facts::FileFacts;
use crate::walk_fleet::WalkFleet;

/// What one file's walk block should do this run.
pub(crate) enum FilePlan {
    /// Walk the file — the cold behaviour, and what every ungated run does.
    Walk,
    /// Replay the file's persisted block instead of walking it.
    Replay(FileWalk),
}

/// One file's walk block, as the ledger records it and as a replay reproduces
/// it: the diagnostics the block appended, and the `uncovered_matches` entry it
/// made (`None` when it made none — the unparsable-file case, which skips the
/// block entirely).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileWalk {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) uncovered: Option<Vec<u32>>,
}

/// One file whose replayed block did not equal its freshly walked one — the
/// finding the paranoid verifier exists to catch, named at the first place the
/// two answers part.
#[derive(Debug, Clone)]
pub struct Divergence {
    /// The file's diagnostic path.
    pub path: String,
    /// The first difference, rendered: which side carries what.
    pub detail: String,
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.detail)
    }
}

/// The run's whole-universe verdicts, handed to the planner before the first
/// file is walked.
///
/// Every one of these is an input to *every* file's walk that is not that
/// file's own tree, another file's tree, or the merged index — so a file may
/// only replay a persisted block if all of them are where they were when the
/// block was persisted. The planner does not interpret them: it hashes them
/// through [`Self::fields`] and compares the digest with the one the artifact
/// carries, which is the "a whole-universe verdict moved ⇒ walk everything"
/// leg of the pinned affected set, priced coarsely on purpose.
pub(crate) struct UniverseVerdict<'a> {
    /// The dam, projected onto what a walk can *ask* it: the two clear bits
    /// and the unparsable-file set. Deliberately not the site list — a dam
    /// site's own line moving is not a verdict change, and no diagnostic
    /// message embeds one.
    pub(crate) dam: &'a DamFacts,
    /// Every unparsable file's diagnostic path, sorted (the dam's third
    /// question, which is per file rather than per universe).
    pub(crate) unparsable: Vec<&'a str>,
    /// The purity oracle: `None` when the project spells no purity-bearing
    /// callable at all (the common case, and a cheap exact answer), otherwise
    /// every symbol the oracle answers `provably_impure` for, sorted.
    pub(crate) purity: Option<Vec<String>>,
    /// The never-returning veto set, sorted.
    pub(crate) never_returning: Vec<&'a str>,
    /// The analysis PHP view's three derived values.
    pub(crate) php_minor: Option<(u16, u16)>,
    pub(crate) catalog_skew: bool,
    pub(crate) version_id: Option<(u32, Option<u32>)>,
    /// The merged property-write obstacle, rendered canonically: every name
    /// written anywhere plus the computed-name bit.
    ///
    /// This one is a deliberate widening of the pinned leg list. The obstacle
    /// is name-keyed like the symbol tables, but the pinned `footprint(F)`
    /// projection carries no property *reads*, so there is no per-file
    /// intersection to take; pricing the table as a whole-universe verdict is
    /// exact where a half-covered name leg would not be.
    pub(crate) property_writes: (Vec<&'a str>, bool),
}

impl UniverseVerdict<'_> {
    /// Stream the verdicts as `(tag, bytes)` fields in a fixed order, for a
    /// caller that hashes them. Streaming rather than returning a string: the
    /// property-write and never-returning sets are universe-sized, and the
    /// digest is taken once per run.
    pub(crate) fn fields(&self, sink: &mut dyn FnMut(&str, &[u8])) {
        sink("dam.names-clear", &[u8::from(self.dam.is_clear())]);
        sink("dam.constants-clear", &[u8::from(self.dam.constants_are_clear())]);
        for path in &self.unparsable {
            sink("dam.unparsable", path.as_bytes());
        }
        match &self.purity {
            None => sink("purity.absent", b""),
            Some(answers) => {
                sink("purity.present", b"");
                for sym in answers {
                    sink("purity.impure", sym.as_bytes());
                }
            }
        }
        for name in &self.never_returning {
            sink("never-returning", name.as_bytes());
        }
        sink("php-minor", format!("{:?}", self.php_minor).as_bytes());
        sink("catalog-skew", &[u8::from(self.catalog_skew)]);
        sink("version-id", format!("{:?}", self.version_id).as_bytes());
        sink("property-writes.computed", &[u8::from(self.property_writes.1)]);
        for name in &self.property_writes.0 {
            sink("property-writes.name", name.as_bytes());
        }
    }
}

/// The planner and the ledger the generation orchestrator threads through one
/// [`check_units`] run. Absent on every other path, where every file walks and
/// nothing is recorded — the ungated behaviour is byte-identical by
/// construction, not by comparison.
///
/// [`check_units`]: crate::check_units
pub(crate) struct WalkControl<'a> {
    /// Decides the per-file plan once the run's whole-universe verdicts are in
    /// hand. Returning a vector shorter than the universe is read as "walk the
    /// rest".
    pub(crate) planner: &'a mut dyn FnMut(&UniverseVerdict<'_>) -> Vec<FilePlan>,
    /// This run's per-file facts, in unit order (issue #516) — what every
    /// whole-universe phase reads instead of a tree. Present only here, on the
    /// generation channel: every other entry point hands `check_units` no
    /// control at all and each phase reads the tree exactly as it always did.
    pub(crate) facts: &'a [FileFacts],
    /// The per-worker folders the walk loop may fan out over (issue #490), or
    /// `None` for a walk in place on the caller's own folder.
    ///
    /// It rides the control for the same reason `facts` does: only a caller
    /// that can *make* folders — configured for this run, one per worker — may
    /// ask for a fan-out, and the orchestrator is the one such caller. Every
    /// other entry point passes no control at all and walks sequentially by
    /// construction, which matters for two of them in particular: the
    /// assertType harness and the loop-subject probe
    /// ([`crate::assert_harness`]) collect through thread-local sinks that a
    /// worker thread would not see.
    pub(crate) fleet: Option<&'a dyn WalkFleet>,
    /// Walk every file *anyway* and compare, instead of trusting the plan.
    pub(crate) paranoid: bool,
    /// Per file, in unit order: what this run's block produced — the rows the
    /// caller persists.
    pub(crate) ledger: Vec<FileWalk>,
    /// Files this run actually walked.
    pub(crate) walked: usize,
    /// Files this run replayed instead of walking (always 0 under
    /// [`Self::paranoid`], where the walk is what ran).
    pub(crate) replayed: usize,
    /// Files the plan would have skipped — equal to `replayed` outside
    /// paranoid mode, and the population the verifier graded inside it.
    pub(crate) would_skip: usize,
    /// How many workers the walk loop actually fanned out over (issue #490);
    /// `1` is the sequential walk. Reported rather than assumed: the width is
    /// trimmed to the walk's own size, so a rebuild that walks two files stays
    /// sequential on a machine that would have allowed twelve.
    pub(crate) workers: usize,
    /// The first [`MAX_RECORDED_DIVERGENCES`] files whose replayed block did
    /// not equal its fresh walk. Capped so a systematically broken affected
    /// set over a 90k-file corpus reports a number and a sample rather than
    /// exhausting memory; [`Self::divergence_count`] keeps the total.
    pub(crate) divergences: Vec<Divergence>,
    /// How many divergences there were in all, capped list or not.
    pub(crate) divergence_count: usize,
    /// Where the analysis phase's time went, filled by [`check_units`] on its
    /// way out (issue #516). `analyze` was one number, and the issue's whole
    /// first move is to find out which part of it is the wall.
    ///
    /// [`check_units`]: crate::check_units
    pub(crate) passes: PassTimings,
}

/// The analysis phase's time, split (issue #516). Wall-clock milliseconds,
/// recorded at the one place each part runs; a part no gate ever forced reads
/// zero, which is a fact about the run rather than a missing measurement.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PassTimings {
    /// The whole-universe per-file facts every walk reads: the dam, the
    /// never-returning veto set, the parse-failure sweep.
    pub(crate) facts_ms: f64,
    /// The effects fixpoint, when some consumer's gate forced it.
    pub(crate) effects_ms: f64,
    /// The throws fixpoint, likewise.
    pub(crate) throws_ms: f64,
    /// The per-file walk loop — walks and replays together.
    pub(crate) walk_ms: f64,
    /// The two project-wide reporting passes that run off the fixpoints.
    pub(crate) report_ms: f64,
}

/// How many divergences the verifier keeps the detail of. One is already a
/// soundness bug; the rest are for telling a systematic break from a local
/// one, and twenty samples say that as well as twenty thousand.
pub(crate) const MAX_RECORDED_DIVERGENCES: usize = 20;

impl<'a> WalkControl<'a> {
    pub(crate) fn new(
        planner: &'a mut dyn FnMut(&UniverseVerdict<'_>) -> Vec<FilePlan>,
        paranoid: bool,
        facts: &'a [FileFacts],
        fleet: Option<&'a dyn WalkFleet>,
    ) -> Self {
        Self {
            planner,
            facts,
            fleet,
            paranoid,
            ledger: Vec::new(),
            walked: 0,
            replayed: 0,
            would_skip: 0,
            workers: 1,
            divergences: Vec::new(),
            divergence_count: 0,
            passes: PassTimings::default(),
        }
    }

    /// Compare a replayed block against the walked one, recording the first
    /// place they part. Called only under [`Self::paranoid`].
    pub(crate) fn verify(&mut self, path: &str, replayed: &FileWalk, walked: &FileWalk) {
        if let Some(detail) = first_divergence(replayed, walked) {
            self.divergence_count += 1;
            if self.divergences.len() < MAX_RECORDED_DIVERGENCES {
                self.divergences.push(Divergence { path: path.to_owned(), detail });
            }
        }
    }
}

/// The first place two blocks part, rendered for a human — or `None` when they
/// are equal field for field.
fn first_divergence(replayed: &FileWalk, walked: &FileWalk) -> Option<String> {
    for (i, walked_d) in walked.diagnostics.iter().enumerate() {
        match replayed.diagnostics.get(i) {
            None => {
                return Some(format!(
                    "the replay is missing the walk's finding #{i}: {}",
                    render(walked_d)
                ));
            }
            Some(replayed_d) if replayed_d != walked_d => {
                return Some(format!(
                    "finding #{i} differs: walked {} / replayed {}",
                    render(walked_d),
                    render(replayed_d)
                ));
            }
            Some(_) => {}
        }
    }
    if let Some(extra) = replayed.diagnostics.get(walked.diagnostics.len()) {
        return Some(format!(
            "the replay carries a finding the walk did not produce: {}",
            render(extra)
        ));
    }
    if replayed.uncovered != walked.uncovered {
        return Some(format!(
            "the uncovered-match set differs: walked {:?} / replayed {:?}",
            walked.uncovered, replayed.uncovered
        ));
    }
    None
}

/// One diagnostic, every field of it — a divergence report that elided a field
/// would hide exactly the class of bug this instrument exists for.
fn render(d: &Diagnostic) -> String {
    format!(
        "[{}] {}:{}:{} {:?} facet={:?} fix={:?}",
        d.id, d.path, d.line, d.column, d.message, d.facet, d.fix
    )
}
