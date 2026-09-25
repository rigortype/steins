//! What a generation run reads before it analyzes: the sealed capture of the
//! universe, each package loaded from the published generation or parsed from
//! its sources, and the changed files with the name delta they imply. The
//! reuse rules — the per-file gate, the verbatim shard, the two readings of an
//! unreadable old side — are set out in [`crate::generation`]'s docs; this is
//! where they are applied, and every miss degrades one package, never the run.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use steins_db::persist::{PayloadIndex, TraceIndex, contracts_section, facts_section, read_shard};
use steins_db::PackageShard;
use steins_gen::{
    DriftKind, Fingerprint, Generation, Miss, PackageKind, PackageName, SourceDrift,
    SourceInventory,
};
use steins_syntax::SourceTree;

use super::identity::analyzer_version;
use super::{GenerationError, GenerationParams, sources_section};
use crate::facts::{FileFacts, key_hash, read_facts};
use crate::fold_persist::fold_package;
use crate::project::LazyTree;
use crate::summaries::Summaries as StoredSummaries;
use crate::walk_plan::FileWalk;

/// Strict inverse of `publish::sources_payload`; any deviation is a [`Miss`].
fn read_sources(
    reader: &mut steins_gen::ArtifactReader,
) -> Result<(String, Fingerprint), Miss> {
    let corrupt = || Miss::Corrupt("sources section is not a provenance record");
    let bytes = reader.section(&sources_section())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
    let obj = value.as_object().filter(|o| o.len() == 2).ok_or_else(corrupt)?;
    let analyzer = obj.get("analyzer").and_then(|a| a.as_str()).ok_or_else(corrupt)?;
    let fingerprint = obj
        .get("fingerprint")
        .and_then(|f| f.as_str())
        .and_then(Fingerprint::from_hex)
        .ok_or_else(corrupt)?;
    Ok((analyzer.to_owned(), fingerprint))
}

/// One package's plan for this run: its identity, its slots in the universe,
/// and the fresh fingerprint the reuse decision compares against.
pub(super) struct Plan {
    pub(super) name: PackageName,
    pub(super) kind: PackageKind,
    pub(super) slots: Vec<usize>,
    pub(super) fingerprint: Fingerprint,
}

/// One package's realized state after the load-or-parse phase.
pub(super) struct PkgState {
    pub(super) loaded: usize,
    pub(super) parsed: usize,
    pub(super) disposition: &'static str,
    /// Whether every persisted `(path, slot)` matches this run's universe, so
    /// the artifact's raw bytes may be copied into the next candidate.
    pub(super) slots_stable: bool,
    /// Whether the package's *whole* source fingerprint matched the published
    /// one — that no file of it was added, removed or edited.
    ///
    /// This, and never `parsed == 0`, is what says a package contributes
    /// nothing to the name delta. The per-file gate (issue #512) separated the
    /// two: a package that lost a file has every *surviving* file loadable, so
    /// it can parse nothing at all while the names of the file it lost are
    /// gone from the universe — and those names must reach the delta, or the
    /// callers that named them replay a stale absence.
    pub(super) sources_match: bool,
    /// A miss forced the reparse (as opposed to an expected change) — the
    /// republish-even-when-current trigger.
    pub(super) degraded: bool,
}

/// Why a package loaded **no** file at all from the published generation.
/// A package that loaded some of its files and parsed the rest is not a
/// refusal — it reports one of the `mixed` dispositions instead.
enum LoadRefusal {
    NoGeneration,
    NotPublished,
    AnalyzerMoved,
    Changed,
    Miss(String),
}

impl LoadRefusal {
    fn disposition(&self) -> &'static str {
        match self {
            LoadRefusal::NoGeneration => "parsed (cold)",
            LoadRefusal::NotPublished => "parsed (new package)",
            LoadRefusal::AnalyzerMoved => "parsed (analyzer moved)",
            LoadRefusal::Changed => "parsed (sources changed)",
            LoadRefusal::Miss(_) => "parsed (artifact miss)",
        }
    }
}

/// What a load carries out of the artifact: the files it can serve, and the
/// slots the caller must parse instead.
struct LoadedPkg {
    /// `(universe slot, facts)` for the files whose own fingerprint matched
    /// and whose facts payload decoded. Their trees are **not** decoded here —
    /// the caller gives each one a deferred handle (issue #516).
    loaded: Vec<(usize, FileFacts)>,
    /// The package's other slots, in slot order: a file whose bytes moved, or
    /// one whose persisted facts could not be established. The caller parses
    /// them — [`load_trees`] never sees the source text.
    stale: Vec<usize>,
    /// Whether every persisted `(path, slot)` matches this run's universe.
    /// Together with an empty [`Self::stale`] it is the licence to serve the
    /// *old* shard verbatim as this run's: the shard's sites are universe
    /// slots and its symbols are the persisted files', so it may be reused
    /// only when every slot still names the same file *and* every one of those
    /// files came out of this same artifact. [`load_trees`] refuses the load
    /// outright in exactly the case where the caller would then take the old
    /// shard and find none, so the caller may take it on these three bits
    /// alone.
    slots_stable: bool,
    /// Whether the package's whole source fingerprint matched — see
    /// [`PkgState::sources_match`], which is what reads it.
    sources_match: bool,
    /// How many files the artifact held a row for and still could not serve:
    /// they parsed instead (a miss is cost, never meaning), and the package
    /// republishes to repair the artifact.
    missed: usize,
    /// The first of those failures, for the note.
    miss: Option<String>,
}

/// One published package artifact kept open for the whole run.
///
/// Deferred tree loads happen wherever a walk reaches a file, which is long
/// after the load phase has moved on, so the reader outlives that phase — and
/// it sits behind a lock because the handle is shared by every file of the
/// package. Reads are per-file bounded sub-ranges (`TraceIndex::read_tree`),
/// so holding the lock is one `pread` and one decode.
pub(super) struct OpenArtifact {
    pub(super) reader: Mutex<steins_gen::ArtifactReader>,
    pub(super) trace: TraceIndex,
    /// The per-file `contracts` and `facts` directories, when they decode —
    /// the republish path's copy sources.
    pub(super) contracts: Option<PayloadIndex>,
    pub(super) facts: Option<PayloadIndex>,
}

impl OpenArtifact {
    /// One file's payload from `index`, or `None` when the artifact cannot
    /// give it. Used only by the republish path, where a `None` means "encode
    /// it instead".
    pub(super) fn copy(&self, index: Option<&PayloadIndex>, path: &str) -> Option<Vec<u8>> {
        let index = index?;
        let mut reader = self.reader.lock().expect("the artifact lock is never poisoned");
        index.payload(&mut reader, path).ok()
    }
}

/// A handle that decodes one file's tree out of `open` on first use, and falls
/// back to re-parsing the sealed text if the payload will not decode.
///
/// The fallback is what makes the handle total, which `Deref` requires — and it
/// costs nothing in meaning: the payload was written from a parse of the very
/// bytes this text is (the file's content fingerprint is what licensed the
/// load), and parsing is a pure function of bytes. A payload miss here is
/// therefore a cost, silently absorbed, exactly as an eager per-file miss was.
fn deferred_tree(open: &Arc<OpenArtifact>, path: &str, text: &Arc<String>) -> LazyTree<'static> {
    let open = Arc::clone(open);
    let path = path.to_owned();
    let text = Arc::clone(text);
    LazyTree::deferred(move || {
        let mut reader = open.reader.lock().expect("the artifact lock is never poisoned");
        open.trace.read_tree(&mut reader, &path).unwrap_or_else(|_| SourceTree::parse(&text))
    })
}

/// The capture phase's product: the universe in slot order, partitioned into
/// one [`Plan`] per package, with each file's text and content hash exactly as
/// the sealed capture handed them over (issue #521).
pub(super) struct Captured {
    /// Each file's diagnostic path, by universe slot.
    pub(super) diag: Vec<String>,
    /// One plan per package, in package-name order.
    pub(super) plans: Vec<Plan>,
    /// Diagnostic path → the file's text, shared with the deferred tree
    /// handles that fall back to re-parsing it.
    pub(super) texts: HashMap<String, Arc<String>>,
    /// Each file's sealed content fingerprint, by universe slot.
    pub(super) contents: Vec<Fingerprint>,
}

/// Partition the universe into packages and capture each behind its seal: one
/// read and one hash per file. The inventories come back apart from the rest,
/// because the publish is the one phase that reads them, and it consumes them.
pub(super) fn capture(
    p: &GenerationParams<'_>,
) -> Result<(Captured, Vec<SourceInventory>), GenerationError> {
    // The partition of this run's universe, and one sealed capture per package.
    let diag: Vec<String> = p.files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    let mut groups: BTreeMap<PackageName, Vec<usize>> = BTreeMap::new();
    for (slot, path) in diag.iter().enumerate() {
        groups.entry(p.partition.package_of(path).clone()).or_default().push(slot);
    }
    let mut plans: Vec<Plan> = Vec::with_capacity(groups.len());
    let mut inventories: Vec<SourceInventory> = Vec::with_capacity(groups.len());
    // The texts and the per-file content hashes, filled *by* the capture rather
    // than by a second pass over the universe (issue #521). The capture already
    // holds each file's bytes at the instant it hashes them, so it hands them
    // straight to the analysis: one read and one hash per file, and "what was
    // analyzed" is literally "what was fingerprinted" by construction rather
    // than by a re-read that re-verifies. Nothing is accumulated on the way —
    // each file's bytes become its `String` and are dropped — so the resident
    // cost is the `texts` map this function has always built and one file's
    // contents beyond it.
    //
    // (The per-file hashes are wanted for their own reason: issue #489 slice B
    // needs to know which files of a *changed* package actually changed, which
    // the package-level fingerprint cannot say.)
    let mut texts: HashMap<String, Arc<String>> = HashMap::with_capacity(diag.len());
    let mut contents: Vec<Option<Fingerprint>> = std::iter::repeat_n(None, diag.len()).collect();
    for (name, slots) in groups {
        let kind = p.partition.universe().get(&name).map_or(PackageKind::Root, |member| member.kind);
        let inventory = SourceInventory::capture_keeping(
            p.capture_root,
            slots.iter().map(|&s| p.files[s].as_path()),
            |captured| {
                let slot = slots[captured.index];
                contents[slot] = Some(captured.entry.content);
                texts.insert(diag[slot].clone(), Arc::new(text_of(captured.bytes)));
            },
        )
        .map_err(|error| GenerationError::Capture { package: name.to_string(), error })?;
        let fingerprint = inventory.fingerprint();
        plans.push(Plan { name, kind, slots, fingerprint });
        inventories.push(inventory);
    }

    // Sound-conservative: a slot the capture did not hand bytes back for is
    // read through the seal exactly as it was before issue #521, verification
    // and all. `capture_keeping` fires for every file of every package, so this
    // is empty on every real run — it exists so that "the capture kept it" is
    // never *assumed*, only used where it holds.
    for (plan, inventory) in plans.iter().zip(&inventories) {
        for &slot in &plan.slots {
            if contents[slot].is_some() {
                continue;
            }
            let key = inventory.key_for(&p.files[slot]).ok_or_else(|| {
                GenerationError::Sealed(SourceDrift {
                    path: diag[slot].clone(),
                    kind: DriftKind::Uncaptured,
                })
            })?;
            contents[slot] = inventory.entry(&key).map(|entry| entry.content);
            let bytes = inventory.read(&key).map_err(GenerationError::Sealed)?;
            texts.insert(diag[slot].clone(), Arc::new(text_of(bytes)));
        }
    }
    let contents: Vec<Fingerprint> = contents
        .into_iter()
        .map(|c| c.expect("every captured file has a sealed content hash"))
        .collect();
    Ok((Captured { diag, plans, texts, contents }, inventories))
}

/// The load-or-parse phase's product: per universe slot, what the analysis
/// reads of each file; per package, in plan order, what the delta and the
/// publish read of it.
pub(super) struct Loaded {
    /// Per slot: the file's tree handle — deferred for a file the artifact
    /// serves (issue #516), ready for one parsed here.
    pub(super) lazy: Vec<LazyTree<'static>>,
    /// Per slot: the file's facts, persisted or derived from this run's parse.
    pub(super) facts: Vec<FileFacts>,
    /// Per slot: whether this run's facts payload is the published one, so
    /// republishing may copy its bytes instead of re-encoding them. The
    /// analysis clears it for any file whose own rows it recomputes.
    pub(super) copyable: Vec<bool>,
    /// Per package: its realized state.
    pub(super) states: Vec<PkgState>,
    /// Per package: this run's shard, verbatim from the artifact or rebuilt
    /// from the per-file facts in hand.
    pub(super) shards: Vec<PackageShard>,
    /// Per package: the open artifact, kept for the run — a deferred tree load
    /// reads one whenever a walk reaches its file, and the republish copies
    /// per-file payloads out of it.
    pub(super) artifacts: Vec<Option<Arc<OpenArtifact>>>,
    /// Per package: the name delta's old side. `None` means one of two things,
    /// and the delta can tell them apart from the package's state: either the
    /// old shard could not be read — which makes the delta unknowable and
    /// walks the whole run, since a name whose disappearance is invisible
    /// cannot be reasoned about — or it was taken to serve as this run's shard
    /// verbatim, which only happens for a package whose sources did not move
    /// and which therefore contributes no delta.
    old_shards: Vec<Option<PackageShard>>,
}

/// Load-or-parse, per package. Any miss degrades that one package.
///
/// "Load" no longer means *decode* (issue #516). A file the artifact can
/// serve gets a deferred [`LazyTree`] and its persisted [`FileFacts`]; the
/// facts answer every whole-universe phase, and the tree is decoded only if
/// something reaches it — a walk of the file, or a walk that descends into
/// it. A file the artifact cannot serve is parsed here and its facts are
/// derived from that parse, which is the same value by construction.
pub(super) fn load_or_parse(
    current: Option<&Generation>,
    published_summaries: Option<&StoredSummaries>,
    captured: &Captured,
    notes: &mut Vec<String>,
) -> Loaded {
    let Captured { diag, plans, texts, contents } = captured;
    let mut lazy_slots: Vec<Option<LazyTree<'static>>> =
        std::iter::repeat_with(|| None).take(diag.len()).collect();
    let mut fact_slots: Vec<Option<FileFacts>> =
        std::iter::repeat_with(|| None).take(diag.len()).collect();
    let mut facts_copyable: Vec<bool> = vec![false; diag.len()];
    let mut states: Vec<PkgState> = Vec::with_capacity(plans.len());
    let mut shards: Vec<PackageShard> = Vec::with_capacity(plans.len());
    let mut artifacts: Vec<Option<Arc<OpenArtifact>>> = Vec::with_capacity(plans.len());
    let mut old_shards: Vec<Option<PackageShard>> = Vec::with_capacity(plans.len());
    for plan in plans {
        let mut published = read_published(current, published_summaries, plan, diag, contents);
        let artifact = published.artifact.take();
        match published.fresh {
            Ok(loaded) => {
                let slots_stable = loaded.slots_stable;
                // The old shard serves verbatim only for a package whose
                // sources did not move at all, which took *every* one of its
                // trees out of that same artifact, and whose slots still name
                // the same files: a mixed package's shard would carry the
                // pre-edit symbols of the file it just reparsed, and a package
                // that lost one would carry the lost file's. The first
                // conjunct is also what keeps the delta loop's reading of a
                // taken (`None`) old shard exact.
                let verbatim = loaded.sources_match && loaded.stale.is_empty() && slots_stable;
                let open = artifact.as_ref().expect("a load keeps its artifact open");
                for (slot, facts) in loaded.loaded {
                    fact_slots[slot] = Some(facts);
                    facts_copyable[slot] = true;
                    lazy_slots[slot] = Some(deferred_tree(open, &diag[slot], &texts[&diag[slot]]));
                }
                for &slot in &loaded.stale {
                    let tree = SourceTree::parse(&texts[&diag[slot]]);
                    fact_slots[slot] = Some(FileFacts::from_tree(&diag[slot], &tree));
                    lazy_slots[slot] = Some(LazyTree::ready(tree));
                }
                shards.push(if verbatim {
                    published
                        .old_shard
                        .take()
                        .expect("a whole stable load without a decoded shard is refused")
                } else {
                    build_shard(plan, &fact_slots)
                });
                let parsed = loaded.stale.len();
                if let Some(detail) = &loaded.miss {
                    notes.push(format!(
                        "package {}: {} file(s) unreadable in the artifact ({detail}); reparsed",
                        plan.name, loaded.missed
                    ));
                }
                states.push(PkgState {
                    loaded: plan.slots.len() - parsed,
                    parsed,
                    disposition: match (parsed, loaded.missed) {
                        (0, _) => "loaded",
                        (_, 0) => "mixed (changed files reparsed)",
                        _ => "mixed (artifact miss)",
                    },
                    slots_stable,
                    sources_match: loaded.sources_match,
                    degraded: loaded.missed > 0,
                });
            }
            Err(refusal) => {
                for &slot in &plan.slots {
                    let tree = SourceTree::parse(&texts[&diag[slot]]);
                    fact_slots[slot] = Some(FileFacts::from_tree(&diag[slot], &tree));
                    lazy_slots[slot] = Some(LazyTree::ready(tree));
                }
                shards.push(build_shard(plan, &fact_slots));
                let degraded = matches!(refusal, LoadRefusal::Miss(_));
                if let LoadRefusal::Miss(detail) = &refusal {
                    notes.push(format!("package {}: artifact miss ({detail}); reparsed", plan.name));
                }
                states.push(PkgState {
                    loaded: 0,
                    parsed: plan.slots.len(),
                    disposition: refusal.disposition(),
                    slots_stable: false,
                    // A refused load establishes nothing about the sources, so
                    // the package answers for its whole old and new key sets.
                    sources_match: false,
                    degraded,
                });
            }
        }
        artifacts.push(artifact);
        old_shards.push(published.old_shard);
    }
    Loaded {
        lazy: lazy_slots.into_iter().map(|t| t.expect("every slot is filled above")).collect(),
        facts: fact_slots.into_iter().map(|f| f.expect("every slot is filled above")).collect(),
        copyable: facts_copyable,
        states,
        shards,
        artifacts,
        old_shards,
    }
}

/// The name delta and the changed-file set it was computed over — the two
/// inputs [`affected_files`] takes besides the facts.
///
/// [`affected_files`]: crate::affected::affected_files
pub(super) struct NameDelta {
    /// The universe slots whose file moved: no persisted row carries the
    /// content fingerprint this run captured ([`block_index`]).
    pub(super) changed: HashSet<usize>,
    /// The names that moved, hashed the way the persisted footprints are
    /// (`facts::key_hash`), since that is the form the affected set compares
    /// against.
    pub(super) names: HashSet<u64>,
    /// Whether every old side could be read. `false` walks the whole run.
    pub(super) known: bool,
}

/// The name delta (issue #489 slice B, tightened to file granularity by
/// issue #510): over every changed package, the names its OLD shard sites
/// in a file that changed, and the names its NEW shard sites in one. A
/// package whose sources did not move contributes nothing — both its sides
/// are the same set — and a package that moved contributes only what its
/// moved *files* declare, which is what makes the delta proportional to the
/// edit rather than to the package. The one member with no site to answer
/// for it — a package's ambiguity set — rides a changed package wholesale;
/// `PackageShard::contributed_names_from` says why.
///
/// "Did not move" is the package's own source fingerprint and never "parsed
/// nothing" (issue #512, `PkgState::sources_match`): under the per-file gate
/// a package that *lost* a file loads every survivor and parses nothing at
/// all, while the names the lost file declared are gone from the universe
/// and must reach the delta.
pub(super) fn name_delta(
    current: Option<&Generation>,
    captured: &Captured,
    loaded: &Loaded,
    blocks: &[Option<&FileWalk>],
    notes: &mut Vec<String>,
) -> NameDelta {
    let diag = &captured.diag;
    let changed: HashSet<usize> = (0..diag.len()).filter(|slot| blocks[*slot].is_none()).collect();
    let now: HashMap<&str, usize> =
        diag.iter().enumerate().map(|(slot, path)| (path.as_str(), slot)).collect();
    let mut delta: HashSet<u64> = HashSet::new();
    let mut delta_known = true;
    for (i, plan) in captured.plans.iter().enumerate() {
        if loaded.states[i].sources_match && !loaded.states[i].degraded {
            continue;
        }
        match &loaded.old_shards[i] {
            // Old sites index the OLD universe, so they are resolved through
            // the old shard's own file map and compared as paths.
            Some(old) => {
                let gone = old_changed_slots(old, &changed, &now);
                delta.extend(old.contributed_names_from(&gone).iter().map(|k| key_hash(k)));
            }
            None => {
                // A name whose disappearance cannot be seen cannot be reasoned
                // about, so the sound answer is to walk everything. (The other
                // reading of `None` — the shard was taken to serve this run
                // verbatim — cannot reach here: taking it requires a load
                // whose sources matched and which did not degrade, which is
                // exactly what this loop skips.)
                delta_known = false;
                notes.push(format!(
                    "package {}: its old symbols are unreadable; walking every file",
                    plan.name
                ));
            }
        }
        let moved: HashSet<usize> =
            plan.slots.iter().copied().filter(|slot| changed.contains(slot)).collect();
        delta.extend(loaded.shards[i].contributed_names_from(&moved).iter().map(|k| key_hash(k)));
    }
    // A package the published generation had and this run does not: its names
    // vanished, and the files that referenced them must be walked. Wholesale
    // and deliberately so — every file it held left it, so no unchanged file
    // of its own is left to narrow the set by.
    if let Some(generation) = current {
        let live: HashSet<&PackageName> = captured.plans.iter().map(|plan| &plan.name).collect();
        let fold = fold_package();
        for gone in generation.packages().filter(|n| **n != fold && !live.contains(n)) {
            match generation.artifact(gone).and_then(|mut r| read_shard(&mut r)) {
                Ok(shard) => delta.extend(shard.contributed_names().iter().map(|k| key_hash(k))),
                Err(miss) => {
                    delta_known = false;
                    notes.push(format!(
                        "removed package {gone}: its old symbols are unreadable ({miss}); walking every file"
                    ));
                }
            }
        }
    }
    NameDelta { changed, names: delta, known: delta_known }
}

/// Per universe slot, the persisted walk block that file could replay and the
/// package index carrying it — or `None`, which is this run's definition of a
/// **changed file**.
///
/// A slot is `None` when the published generation had no row for its path, or
/// when the row's content fingerprint differs from the one this run captured.
/// That is the file-level notion of change the design pins, and it is
/// deliberately not semantic: a callee whose lines merely moved changes a
/// caller's descent-provenance message, so *any* byte moving makes the file
/// changed. It is also why a **changed package's unchanged files** can still
/// replay — the package fingerprint says the package moved, the row says which
/// of its files did — and, since issue #512, why they can still *load*: one
/// predicate, [`unmoved_rows`], answers both questions, so the load gate and
/// the name delta cannot disagree about which files moved.
pub(super) fn block_index<'a>(
    plans: &[Plan],
    diag: &[String],
    summaries: Option<&'a StoredSummaries>,
    contents: &[Fingerprint],
) -> Vec<Option<&'a FileWalk>> {
    let mut out: Vec<Option<&FileWalk>> = vec![None; diag.len()];
    let Some(summaries) = summaries else { return out };
    for plan in plans {
        for (slot, walk) in unmoved_rows(plan, diag, contents, summaries) {
            out[slot] = Some(walk);
        }
    }
    out
}

/// One package's persisted rows whose file did not move: `(universe slot,
/// block)` for every slot of `plan` the section holds a row for under the
/// content fingerprint this run captured.
///
/// This is the project's one spelling of "this file is unchanged". Two callers
/// read it and must never diverge: [`block_index`], which turns its complement
/// into the `changed` set the name delta and the affected set are computed
/// from, and [`read_published`], which hands the slots to [`load_trees`] as
/// the licence to load their trees rather than parse them (issue #512). The
/// fingerprint compared is the one the `summaries` row already carries — there
/// is deliberately no second per-file fingerprint anywhere, because two of
/// them could disagree.
fn unmoved_rows<'a>(
    plan: &Plan,
    diag: &[String],
    contents: &[Fingerprint],
    summaries: &'a StoredSummaries,
) -> Vec<(usize, &'a FileWalk)> {
    let rows: HashMap<&str, (&Fingerprint, &FileWalk)> =
        summaries.rows().map(|(path, content, walk)| (path, (content, walk))).collect();
    plan.slots
        .iter()
        .filter_map(|&slot| {
            let (content, walk) = rows.get(diag[slot].as_str())?;
            (**content == contents[slot]).then_some((slot, *walk))
        })
        .collect()
}

/// Everything the published generation can say about one package: the old
/// shard, the artifact kept open for deferred tree loads, and the load attempt
/// proper.
struct Published {
    /// This package's OLD shard — the delta's old side, and the verbatim
    /// shard when the load reuses it. `None` when the artifact could not give
    /// it, which makes the name delta *unknowable* and walks the whole run
    /// (see [`name_delta`]).
    old_shard: Option<PackageShard>,
    /// The open artifact, kept alive for this run's deferred tree loads and
    /// for the republish path's per-file byte copies. `Some` whenever the
    /// artifact opened at all, even where the load was refused.
    artifact: Option<Arc<OpenArtifact>>,
    fresh: Result<LoadedPkg, LoadRefusal>,
}

impl Published {
    /// The shape for a package the published generation could not tell us
    /// anything about at all.
    fn refused(refusal: LoadRefusal, old_shard: Option<PackageShard>) -> Self {
        Self { old_shard, artifact: None, fresh: Err(refusal) }
    }
}

/// Which of a package's OLD file slots hold a file that moved — the old side
/// of the file-granular delta (issue #510).
///
/// Old slots index the *old* universe, so the comparison goes through paths:
/// the old shard's own file map names each old slot, and this run's `now` map
/// says where (and whether) that path lives today. A path this run does not
/// have at all — deleted, or reclassified into a package that reads it
/// differently — counts as moved, which is how a vanished declaration's name
/// reaches the delta and its callers get walked.
fn old_changed_slots(
    old: &PackageShard,
    changed: &HashSet<usize>,
    now: &HashMap<&str, usize>,
) -> HashSet<usize> {
    old.files()
        .filter(|(path, _)| now.get(path).is_none_or(|slot| changed.contains(slot)))
        .map(|(_, slot)| slot)
        .collect()
}

/// Read what the published generation holds for one package. Any decode
/// failure is a [`LoadRefusal::Miss`] for this package alone; only the *old
/// names* being unreadable is wider, because a name whose disappearance cannot
/// be seen cannot be reasoned about.
fn read_published(
    generation: Option<&Generation>,
    summaries: Option<&StoredSummaries>,
    plan: &Plan,
    diag: &[String],
    contents: &[Fingerprint],
) -> Published {
    let Some(generation) = generation else {
        // A cold run: the published universe is empty, which is a *known*
        // empty old side rather than an unknown one. Nothing replays anyway.
        return Published::refused(LoadRefusal::NoGeneration, Some(PackageShard::default()));
    };
    if !generation.has_package(&plan.name) {
        // A package the generation never had contributes no old names — again
        // known, not unknown, so its arrival is an ordinary delta.
        return Published::refused(LoadRefusal::NotPublished, Some(PackageShard::default()));
    }
    let miss = |m: Miss| LoadRefusal::Miss(m.to_string());
    let mut reader = match generation.artifact(&plan.name) {
        Ok(reader) => reader,
        Err(m) => return Published::refused(miss(m), None),
    };
    // The old shard, always: it is the delta's old side whether or not the
    // sources moved, and it is the verbatim shard when they did not.
    let old_shard = read_shard(&mut reader).ok();
    // The walk blocks are never a load refusal: a package the sidecar has no
    // rows for simply walks every file. They are consulted here because their
    // rows are also the per-file provenance gate (issue #512) — which files of
    // this package the load may take. They live in the generation's `summaries`
    // sidecar rather than in the artifact (issue #519), because they are a
    // function of the run and the artifact must stay a function of the sources
    // to be shareable.
    let unmoved: HashSet<usize> = summaries
        .map(|s| unmoved_rows(plan, diag, contents, s).into_iter().map(|(slot, _)| slot).collect())
        .unwrap_or_default();
    let (analyzer, stored) = match read_sources(&mut reader) {
        Ok(sources) => sources,
        Err(m) => return Published::refused(miss(m), old_shard),
    };
    let trace = match TraceIndex::open(&mut reader) {
        Ok(trace) => trace,
        Err(m) => return Published::refused(miss(m), old_shard),
    };
    let contracts = PayloadIndex::open(&mut reader, contracts_section()).ok();
    let facts = PayloadIndex::open(&mut reader, facts_section()).ok();
    let open = Arc::new(OpenArtifact { reader: Mutex::new(reader), trace, contracts, facts });
    let fresh =
        load_trees(&open, plan, diag, &unmoved, old_shard.is_some(), &analyzer, stored);
    Published { old_shard, artifact: Some(open), fresh }
}

/// The load proper: the provenance gate, then the per-file **facts** — never a
/// tree (issue #516).
///
/// The gate is per file (issue #512). `unmoved` is the set of this package's
/// slots whose persisted `summaries` row carries the content fingerprint this
/// run captured; a file in it is served from the artifact, and every other file
/// of the package goes into [`LoadedPkg::stale`] for the caller to parse. The
/// package-level fingerprint stays as the shortcut it always was: when it
/// matches, *every* file is unmoved by construction and no row is consulted at
/// all — so a package whose `summaries` section is absent or will not decode
/// still loads whole, exactly as it did before that gate existed.
///
/// What is decoded here is the file's facts payload, which is small and which
/// every whole-universe phase then reads instead of a tree. The tree itself
/// gets a deferred handle in the caller. That is the whole of issue #516's
/// first item: a file the artifact serves costs one facts decode, and a tree
/// decode only if a walk reaches it.
///
/// Sound-conservative in every direction: a file with no row, a row that does
/// not match, a path the trace directory does not list, an absent facts
/// directory and a facts payload that will not decode all parse. A package that
/// ends up serving *nothing* is reported as the refusal it would have been
/// before, so the disposition vocabulary keeps its old spellings for the old
/// cases.
///
/// `have_shard` says whether the artifact's symbols section decoded — the
/// caller keeps the shard itself, because it is the delta's old side as much
/// as it is the verbatim shard.
fn load_trees(
    open: &Arc<OpenArtifact>,
    plan: &Plan,
    diag: &[String],
    unmoved: &HashSet<usize>,
    have_shard: bool,
    analyzer: &str,
    stored: Fingerprint,
) -> Result<LoadedPkg, LoadRefusal> {
    if analyzer != analyzer_version() {
        return Err(LoadRefusal::AnalyzerMoved);
    }
    // The whole-package shortcut, and the one case where a file may load
    // without a row of its own to vouch for it.
    let whole = stored == plan.fingerprint;
    if !whole && unmoved.is_empty() {
        return Err(LoadRefusal::Changed);
    }
    let persisted: HashMap<&str, usize> = open.trace.files().collect();
    let mut slots_stable = persisted.len() == plan.slots.len();
    let mut loaded: Vec<(usize, FileFacts)> = Vec::with_capacity(plan.slots.len());
    let mut stale: Vec<usize> = Vec::new();
    let mut missed = 0usize;
    let mut first_miss: Option<String> = None;
    for &slot in &plan.slots {
        let path = diag[slot].as_str();
        if persisted.get(path) != Some(&slot) {
            slots_stable = false;
        }
        if !whole && !unmoved.contains(&slot) {
            stale.push(slot);
            continue;
        }
        match open
            .facts
            .as_ref()
            .ok_or(Miss::AbsentSection(facts_section()))
            .and_then(|index| {
                let mut reader = open.reader.lock().expect("the artifact lock is never poisoned");
                index.payload(&mut reader, path)
            })
            .and_then(|bytes| read_facts(&bytes))
        {
            Ok(facts) if persisted.contains_key(path) => loaded.push((slot, facts)),
            Ok(_) => {
                // The facts are readable but the trace directory has no entry,
                // so nothing could serve this file's tree if a walk asked.
                missed += 1;
                first_miss.get_or_insert_with(|| "no trace entry for the file".to_owned());
                stale.push(slot);
            }
            Err(m) => {
                // This file alone: the directory and every other payload still
                // serve, and the caller parses this one.
                missed += 1;
                first_miss.get_or_insert_with(|| m.to_string());
                stale.push(slot);
            }
        }
    }
    if loaded.is_empty() {
        return Err(first_miss.map_or(LoadRefusal::Changed, LoadRefusal::Miss));
    }
    // `whole` is a statement about the sources, not about what was loaded: a
    // per-file miss can reparse a file of a package whose bytes never moved.
    let sources_match = whole;
    // The shard's sites are universe slots; it may only serve verbatim when
    // the sources did not move, every persisted slot still names the same
    // file, and nothing was reparsed — the caller's `verbatim`, spelled the
    // same way here so its `expect` cannot fire. Otherwise the caller rebuilds
    // it from the per-file shards in hand — still no reparse.
    if sources_match && stale.is_empty() && slots_stable && !have_shard {
        return Err(LoadRefusal::Miss("symbols section is not a shard".to_owned()));
    }
    Ok(LoadedPkg { loaded, stale, slots_stable, sources_match, missed, miss: first_miss })
}

/// Build one package's shard from its files' persisted-or-derived per-file
/// shards (issue #516).
///
/// Before this, the rebuild called `PackageShard::add_file` over every file's
/// *tree*, which is why an edit anywhere in a package decoded every tree it
/// held — fatal in the ordinary first-party shape, where one package holds
/// everything. [`PackageShard::absorb_file`] folds the same contribution in
/// from the per-file shard the facts carry, re-slotted; the equality is pinned
/// in `steins-db`.
fn build_shard(plan: &Plan, facts: &[Option<FileFacts>]) -> PackageShard {
    let mut shard = PackageShard::default();
    for &slot in &plan.slots {
        let facts = facts[slot].as_ref().expect("the package's facts are in hand");
        shard.absorb_file(&facts.shard, slot);
    }
    shard
}

/// One sealed file's bytes as the text the analysis reads — the same
/// lossy-UTF-8 spelling `steins-cli`'s cold path produces (`project.rs`), and
/// the same one [`super::generation_check`] produced when it read through the seal.
///
/// Written as `from_utf8` with a lossy fallback rather than as
/// `from_utf8_lossy(&bytes).into_owned()` so that the ordinary case — a valid
/// UTF-8 source file — takes the buffer the capture already allocated instead
/// of copying the universe a second time. The invalid case is byte-for-byte
/// what it always was: `U+FFFD` per ill-formed sequence.
fn text_of(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::text_of;

    /// The optimized spelling is the old one, byte for byte, on valid and
    /// ill-formed input alike — the property the whole capture-once change
    /// rests on being invisible to what is analyzed.
    #[test]
    fn text_of_equals_from_utf8_lossy() {
        for case in [
            b"<?php echo 1;\n".to_vec(),
            Vec::new(),
            "<?php // \u{3042}\u{3044}\n".as_bytes().to_vec(),
            // A lone continuation byte, and a truncated three-byte sequence.
            b"<?php \x80 \xe3\x81 end\n".to_vec(),
            vec![0xff, 0xfe, 0xfd],
        ] {
            assert_eq!(
                text_of(case.clone()),
                String::from_utf8_lossy(&case).into_owned(),
                "{case:?}"
            );
        }
    }
}
