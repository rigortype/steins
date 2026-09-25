//! What a generation run writes after it analyzes: nothing, when this run is
//! the published generation and nothing degraded; otherwise a candidate — each
//! unmoved package's artifact shared with the published generation, every
//! other package's reassembled per file, this run's walk blocks in the one
//! sidecar, the fold table — published over `CURRENT`. A failure here is a note
//! and never the run's verdict: the findings are computed, and persistence is a
//! cache.

use std::sync::Arc;

use steins_db::persist::{
    build_sections, contract_payload, facts_section, payload_section_bytes, trace_payload,
};
use steins_db::PackageShard;
use steins_gen::{
    ArtifactBuilder, Fingerprint, Generation, GenerationId, ShareKind, SourceInventory, Store,
};

use super::identity::analyzer_version;
use super::load::{Captured, Loaded, OpenArtifact, Plan};
use super::sources_section;
use crate::facts::{FileFacts, facts_payload};
use crate::fold_persist::{FoldTableArtifact, fold_package};
use crate::project::LazyTree;
use crate::summaries::{SummaryRow, write_summaries};
use crate::walk_plan::FileWalk;

fn sources_payload(fingerprint: &Fingerprint) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "analyzer": analyzer_version(),
        "fingerprint": fingerprint.to_hex(),
    }))
    .expect("a sources record serializes")
}

/// Everything a publish writes: the identity, the sealed inventories the
/// store begins the candidate from, the run's per-package and per-file
/// products, the fold table and this run's walk blocks.
pub(super) struct Publishable<'a> {
    pub(super) id: GenerationId,
    pub(super) inventories: Vec<SourceInventory>,
    pub(super) captured: &'a Captured,
    pub(super) loaded: &'a Loaded,
    pub(super) fold: Fold<'a>,
    pub(super) summaries: Summaries<'a>,
}

/// Publish — or keep CURRENT when this run *is* the published generation
/// and nothing degraded (a degradation republishes to repair the artifact).
/// Returns the published (or confirmed-current) generation id, lowercase hex,
/// or `None` when publication failed, which is a note; and how many artifacts
/// the publish shared rather than wrote.
pub(super) fn publish_or_reuse(
    store: &Store,
    current: Option<&Generation>,
    run: Publishable<'_>,
    fold_degraded: bool,
    notes: &mut Vec<String>,
) -> (Option<String>, usize) {
    let states = &run.loaded.states;
    let total_parsed: usize = states.iter().map(|s| s.parsed).sum();
    let any_degraded = states.iter().any(|s| s.degraded) || fold_degraded;
    let reuse = current.is_some_and(|g| g.id() == &run.id) && total_parsed == 0 && !any_degraded;
    if reuse {
        notes.push("generation already current; nothing republished".to_owned());
        return (Some(run.id.to_hex()), 0);
    }
    match publish(store, current, run) {
        Ok((hex, shared)) => {
            notes.extend(shared.note());
            (Some(hex), shared.total())
        }
        Err(detail) => {
            notes.push(format!("publish failed ({detail}); this run's findings are unaffected"));
            (None, 0)
        }
    }
}

/// Assemble and publish the candidate. Every failure is one string for the
/// caller's note — publication is a cache write, never the run's verdict.
///
/// Returns the published hex and how many packages were **shared** rather than
/// written (issue #519): an artifact whose bytes this run would have
/// reproduced exactly is taken from the published generation by reflink or
/// hard link, which costs a directory entry instead of the package's bytes and
/// its durability barrier. In the shape ADR-0092 §3 is built for that is every
/// package but the edited one.
fn publish(
    store: &Store,
    current: Option<&Generation>,
    run: Publishable<'_>,
) -> Result<(String, Shared), String> {
    let Publishable { id, inventories, captured, loaded, fold, summaries } = run;
    let payloads = Payloads {
        lazy: &loaded.lazy,
        facts: &loaded.facts,
        copyable: &loaded.copyable,
        artifacts: &loaded.artifacts,
        diag: &captured.diag,
    };
    let mut candidate = store.begin(id, inventories).map_err(|e| format!("begin: {e}"))?;
    let mut shared = Shared::default();
    let packages = captured.plans.iter().zip(&loaded.states).zip(&loaded.shards).enumerate();
    for (package, ((plan, state), shard)) in packages {
        // A package that parsed nothing, kept its slots, rebuilt none of its
        // per-file facts and whose whole source fingerprint still matches would
        // republish the published artifact's exact bytes — so it takes them
        // instead of rewriting them. Anything else — a mixed package included —
        // reassembles the sections per file.
        //
        // The third conjunct is issue #516's: an unmoved file whose own rows
        // this run recomputed has *different* facts, and republishing the
        // artifact wholesale would carry the rows of an older universe under
        // this generation's identity, where the next run would take them as its
        // own. The fourth is what makes the `sources` section equal too, so
        // "the same bytes" covers every section rather than four of five.
        let unmoved = state.parsed == 0
            && state.slots_stable
            && state.sources_match
            && plan.slots.iter().all(|&slot| payloads.copyable[slot]);
        let adopted = unmoved
            .then_some(current)
            .flatten()
            .and_then(|generation| candidate.adopt_artifact(&plan.name, generation).ok());
        match adopted {
            Some(kind) => shared.count(kind),
            None => {
                let builder = build_artifact(plan, shard, package, &payloads);
                candidate
                    .write_artifact(&plan.name, &builder)
                    .map_err(|e| format!("write {}: {e}", plan.name))?;
            }
        }
    }
    // The walk blocks are always this run's, even where every artifact was
    // shared: an artifact is a function of the sources alone, the sidecar is a
    // function of the whole run identity, and republishing a stale stamp would
    // only refuse itself on the next run. That split is exactly what leaves an
    // unmoved package's artifact shareable at all — and one sidecar for the
    // universe is one write and one barrier however many packages there are.
    let mut sidecar = ArtifactBuilder::new();
    summaries.write(&mut sidecar, &captured.plans);
    candidate.write_summaries(&sidecar).map_err(|e| format!("write summaries: {e}"))?;
    if let Some(table) = fold.table {
        // The fold table gets the same treatment on the same terms: the engine
        // says whether the table it would publish is the one it loaded, value
        // for value, and only then are the published bytes taken.
        let adopted = fold
            .unchanged
            .then_some(current)
            .flatten()
            .and_then(|generation| candidate.adopt_artifact(&fold_package(), generation).ok());
        match adopted {
            Some(kind) => shared.count(kind),
            None => candidate
                .write_artifact(&fold_package(), &table.to_builder())
                .map_err(|e| format!("write {}: {e}", fold_package()))?,
        }
    }
    let generation = candidate.publish().map_err(|e| e.to_string())?;
    Ok((generation.id().to_hex(), shared))
}

/// The fold table on its way to disk: what this run would publish, and whether
/// that is the published generation's table unchanged.
#[derive(Clone, Copy)]
pub(super) struct Fold<'a> {
    pub(super) table: Option<&'a FoldTableArtifact>,
    pub(super) unchanged: bool,
}

/// How many artifacts a publish shared instead of writing, by mechanism.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Shared {
    reflinked: usize,
    hard_linked: usize,
    copied: usize,
}

impl Shared {
    fn count(&mut self, kind: ShareKind) {
        match kind {
            ShareKind::Reflink => self.reflinked += 1,
            ShareKind::HardLink => self.hard_linked += 1,
            ShareKind::Copy => self.copied += 1,
        }
    }

    fn total(self) -> usize {
        self.reflinked + self.hard_linked + self.copied
    }

    /// The run note, or `None` when nothing was shared. The mechanism is worth
    /// saying: a store on a filesystem with no clone and no hard link is
    /// silently paying the old price, and only this line would show it.
    fn note(self) -> Option<String> {
        (self.total() > 0).then(|| {
            let mut how: Vec<String> = Vec::new();
            for (n, kind) in [
                (self.reflinked, ShareKind::Reflink),
                (self.hard_linked, ShareKind::HardLink),
                (self.copied, ShareKind::Copy),
            ] {
                if n > 0 {
                    how.push(format!("{n} {}", kind.verb()));
                }
            }
            format!(
                "{} artifact(s) shared with the published generation rather than rewritten ({})",
                self.total(),
                how.join(", ")
            )
        })
    }
}

/// What the republish path needs per file: the tree handle, the facts, whether
/// the published payloads may be copied, and the artifacts to copy them from.
struct Payloads<'a> {
    lazy: &'a [LazyTree<'static>],
    facts: &'a [FileFacts],
    /// Per slot: whether this run's facts equal the published ones (an unmoved
    /// file whose own rows this run did not recompute). Also what licenses
    /// copying the file's `trace` and `contracts` payloads, which are the
    /// weaker claim — a function of the bytes alone.
    copyable: &'a [bool],
    /// Per package, in plan order, the open artifact to copy from.
    artifacts: &'a [Option<Arc<OpenArtifact>>],
    diag: &'a [String],
}

/// One package's artifact: the #503 sections, the `facts` section of issue
/// #516, and the provenance record the warm path's reuse decision reads.
///
/// **Per file, and copied where it can be.** A package that must be
/// reassembled is not a package whose every file moved: an edit in one file of
/// a 30k-file root package leaves 29,999 payloads identical, and re-encoding
/// them would need their trees — the very decode this slice exists to avoid.
/// So each of the three per-file sections takes the published payload verbatim
/// for a file that did not move, and encodes only what did.
fn build_artifact(
    plan: &Plan,
    shard: &PackageShard,
    package: usize,
    p: &Payloads<'_>,
) -> ArtifactBuilder {
    let old = p.artifacts[package].as_ref();
    let mut contracts = Vec::with_capacity(plan.slots.len());
    let mut trace = Vec::with_capacity(plan.slots.len());
    let mut facts = Vec::with_capacity(plan.slots.len());
    for &slot in &plan.slots {
        let path = &p.diag[slot];
        let published = old.filter(|_| p.copyable[slot]);
        contracts.push((
            path.clone(),
            slot,
            published
                .and_then(|open| open.copy(open.contracts.as_ref(), path))
                .unwrap_or_else(|| contract_payload(&p.lazy[slot])),
        ));
        trace.push((
            path.clone(),
            slot,
            published
                .and_then(|open| {
                    let mut reader =
                        open.reader.lock().expect("the artifact lock is never poisoned");
                    open.trace.payload(&mut reader, path).ok()
                })
                .unwrap_or_else(|| trace_payload(&p.lazy[slot])),
        ));
        facts.push((
            path.clone(),
            slot,
            published
                .and_then(|open| open.copy(open.facts.as_ref(), path))
                .unwrap_or_else(|| facts_payload(&p.facts[slot])),
        ));
    }
    let mut builder = build_sections(shard, &contracts, &trace);
    builder
        .section(facts_section(), payload_section_bytes(&facts))
        .expect("distinct section names");
    builder
        .section(sources_section(), sources_payload(&plan.fingerprint))
        .expect("distinct section names");
    builder
}

/// This run's walk blocks on their way to disk: the two stamps that license
/// replaying them, the per-file content hashes that say which files moved, and
/// the ledger `check_units` filled in unit order.
pub(super) struct Summaries<'a> {
    pub(super) stamp: Fingerprint,
    pub(super) universe: Fingerprint,
    pub(super) diag: &'a [String],
    pub(super) contents: &'a [Fingerprint],
    pub(super) ledger: &'a [FileWalk],
}

impl Summaries<'_> {
    /// Fill the generation's sidecar: every package's rows, in plan then slot
    /// order. A run whose ledger is short — which cannot happen, since
    /// `check_units` records every unit — writes the rows it has; the reader
    /// keys by path and a missing row simply cannot be replayed.
    fn write(&self, builder: &mut ArtifactBuilder, plans: &[Plan]) {
        let rows: Vec<SummaryRow<'_>> = plans
            .iter()
            .flat_map(|plan| plan.slots.iter())
            .filter_map(|&slot| {
                Some(SummaryRow {
                    path: &self.diag[slot],
                    slot,
                    content: self.contents[slot],
                    walk: self.ledger.get(slot)?,
                })
            })
            .collect();
        write_summaries(builder, &self.stamp, &self.universe, &rows);
    }
}
