//! The generation orchestrator (ADR-0092 §5, issue #489 slice A): cold build →
//! publish → warm rebuild, composed entirely from landed pieces — the store and
//! sealed capture (#485), the Composer partition (#486), the per-package
//! payloads (#503), and the recorded fold table (#500).
//!
//! **What the warm path reuses, and why it is sound.** Slice A re-walks every
//! file; what it skips is parse + lowering (the 45–57% cost
//! `docs/agents/profiling.md` names). A package whose freshly captured source
//! fingerprint matches the one stored in its artifact loads its lowered
//! [`SourceTree`]s from the `trace` section and its symbol shard from the
//! `symbols` section instead of re-parsing; everything downstream — the global
//! merges, the dam, both fixpoints, every walk — recomputes from those trees
//! exactly as a cold run recomputes from freshly parsed ones. The fingerprint
//! is blake3 over the captured bytes and parsing is deterministic, so a loaded
//! tree *is* the reparse; warm ≡ cold holds by construction, and the
//! `warm_generation.rs` oracles pin it byte-for-byte.
//!
//! The [`PackageKind`] axis (#486, `trusted_from_artifacts`) draws the line
//! between *trust without revalidation* (a future economy the lock hash might
//! license for [`PackageKind::Vendor`]) and what this slice does: **every**
//! package is revalidated by content capture on every run, so a fingerprint
//! match licenses the load for every kind, first-party included — which is
//! also what the no-change oracle (zero reparses over an untouched tree)
//! requires. The kind rides the [`PackageReport`] so the caller can see the
//! posture per package.
//!
//! **Degradation, never meaning.** Every [`Miss`] — an unreadable `CURRENT`, a
//! package artifact that fails any decode, a fold table whose identity or
//! bytes are wrong — degrades exactly that package (or the fold table) to the
//! cold path for this run, never the run. A publish failure (drift under the
//! seal, IO) is a note, never an error: the findings are already computed and
//! persistence is a cache. One consequence, priced deliberately: a
//! same-identity republish defers to the already-published copy (the store's
//! own rule), so a poisoned artifact under an unchanged identity keeps costing
//! its package's reparse until any identity input moves — ADR-0092 §8's
//! recovery story ("throw the cache away") is the unclever repair.
//!
//! **What the generation identity covers** ([`GenerationInputs`], filled in
//! [`generation_check`]): the analyzer's own version (`CARGO_PKG_VERSION` —
//! one workspace version, and it subsumes the generated catalog tables baked
//! into the binary); per-package source fingerprints from the sealed capture;
//! the `composer.lock` content hash; the catalog's declared PHP pin
//! (`steins_catalog::PINNED_PHP`); the plugin channel's finding-relevant
//! content (registered labels + colorings, not package names); the engine
//! posture off this run's own recorded boot surface (or `Off`); and the
//! finding-relevant config — the `[effects]` policy (tolerance + attribution),
//! both `[runtime]` postures (`warning-handler`, `final-keyword`), and the
//! resolved [`ProjectLayout`] (vendor boundary + PHP target; its `Debug`
//! rendering is deterministic — ordered `Vec`s throughout — and
//! over-invalidating on a spelling change costs a rebuild, never meaning).
//! Deliberately left out: everything display-side — profiles/surfaces,
//! baseline flags, output format, `--vendor-diagnostics` — and the fix-it
//! machinery, none of which change what the analysis *finds*; and plugin
//! *notices*, which report refusals rather than facts.
//!
//! Lives in `steins-infer` rather than `steins-cli` (where the CLI wiring
//! stays) because the one analysis entry both temperatures must share —
//! `check_units` over a [`FileUnit`] slice — is crate-private here, and
//! because `cargo xtask perf --warm` and the MCP server (#491) both need the
//! library shape without depending on the binary crate. Native-only, like
//! `fold_persist`: the wasm graph never sees the store.
//!
//! Known and recorded (issue #491): `Store::open` sweeps candidate
//! directories, so two concurrent processes over one store can delete each
//! other's in-flight candidates. Single-process CLI use is fine; this function
//! opens the store once per run and never re-opens it mid-run.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use steins_db::persist::{
    TraceFile, TraceIndex, build_sections, contracts_section, decl_contracts, read_shard,
    symbols_section, trace_section,
};
use steins_db::{
    EffectsPolicy, PackagePartition, PackageShard, PluginFacts, ProjectLayout, merge_shards,
};
use steins_gen::{
    ArtifactBuilder, DriftKind, EnginePosture, FieldHasher, Fingerprint, Generation, GenerationId,
    GenerationInputs, Miss, PackageName, SectionName, SourceDrift, SourceError, SourceInventory,
    Store,
};
pub use steins_gen::PackageKind;
use steins_syntax::SourceTree;

use crate::affected::{AffectedInputs, affected_files};
use crate::fold_persist::{FoldTableArtifact, RecordingEngine, fold_package};
use crate::project::{FileUnit, Index, Res};
use crate::summaries::{Summaries as StoredSummaries, SummaryRow, read_summaries, write_summaries};
use crate::walk_plan::{FilePlan, FileWalk, UniverseVerdict, WalkControl};
use crate::{Diagnostic, Divergence, EngineFolder, FinalKeyword, ProcessEngine};

// ---------------------------------------------------------------------------
// The orchestrator's own section: which sources an artifact was built from.
// ---------------------------------------------------------------------------

/// The section holding the package's provenance record: a JSON object with the
/// analyzer version that built the artifact and the package's source
/// fingerprint ([`SourceInventory::fingerprint`], hex). The warm path's whole
/// reuse decision reads this section: a fingerprint match against the fresh
/// capture licenses the load, anything else reparses.
pub const SOURCES_SECTION: &str = "sources";

fn sources_section() -> SectionName {
    SectionName::new(SOURCES_SECTION).expect("the sources section name is valid")
}

/// The analyzer's own version — the workspace version, identical across the
/// steins crates, so the CLI and this crate spell one identity.
fn analyzer_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The environment variable that turns the paranoid verifier on (issue #489
/// slice B).
///
/// **Why an instrument and not only a test.** The failure mode walk skipping
/// can have is a stale or missing finding — the zero-FP violation this project
/// exists to prevent — and the warm ≡ cold oracle only catches what a fixture
/// happens to exercise. Paranoid mode walks *every* file regardless of the
/// affected set, keeps the walked answer, and asserts that each file the
/// affected-set computation would have skipped replays byte-identically,
/// naming the first divergence with its file and finding
/// ([`GenerationReport::walk`]). Nothing about it is fixture-shaped: it holds
/// one file's two answers at a time, so it runs over a whole corpus tree.
///
/// It was built and landed **before** any skip logic existed, where it
/// trivially reports zero would-skips. That ordering is the point: an
/// instrument written after the thing it measures grades its author's
/// homework.
pub const PARANOID_ENV: &str = "STEINS_GENERATIONS_PARANOID";

/// Whether this process runs the paranoid verifier. Read once per run,
/// deliberately from the environment rather than from [`GenerationParams`]:
/// every caller of the orchestrator (the CLI, `cargo xtask perf --warm
/// --paranoid`, the MCP server of issue #491) gets it without a signature
/// change, and CI — which never sets it — is unaffected.
fn paranoid_enabled(p: &GenerationParams<'_>) -> bool {
    p.paranoid || std::env::var(PARANOID_ENV).is_ok_and(|v| v == "1")
}

/// The run's whole-universe verdict digest: one fingerprint over every input
/// a file's walk reads that is neither that file's tree, another file's tree,
/// nor the merged index (ADR-0092 §5, issue #489 slice B).
///
/// This is the "a whole-universe verdict moved ⇒ walk everything" leg of the
/// pinned affected set, made comparable across generations. The verdicts are
/// streamed as tagged fields rather than concatenated into a string because
/// two of them — the never-returning set and the property-write obstacle —
/// are universe-sized.
fn universe_digest(verdict: &UniverseVerdict<'_>) -> Fingerprint {
    let mut h = FieldHasher::new("steins-infer/universe-verdict");
    verdict.fields(&mut |tag, bytes| {
        h.field(tag, bytes);
    });
    h.finish()
}

fn sources_payload(fingerprint: &Fingerprint) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "analyzer": analyzer_version(),
        "fingerprint": fingerprint.to_hex(),
    }))
    .expect("a sources record serializes")
}

/// Strict inverse of [`sources_payload`]; any deviation is a [`Miss`].
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

// ---------------------------------------------------------------------------
// Parameters, outcome, report.
// ---------------------------------------------------------------------------

/// Everything one generation run needs, resolved by the caller at its own IO
/// boundary (the CLI's config/layout/plugin discovery, the perf harness's).
pub struct GenerationParams<'a> {
    /// Where the store lives: `<store_root>/.steins/gen/`. The CLI passes the
    /// outermost governing root; the perf harness passes a scratch directory
    /// so a measured corpus is never written into.
    pub store_root: &'a Path,
    /// What relative entries of `files` resolve against — the working
    /// directory the paths were spelled in.
    pub capture_root: &'a Path,
    /// The analyzed files in universe-slot order (the caller's sorted walk).
    /// Each file's diagnostic path is its `to_string_lossy` spelling, matching
    /// the CLI's own.
    pub files: &'a [PathBuf],
    pub layout: &'a ProjectLayout,
    pub partition: &'a PackagePartition,
    pub plugins: &'a PluginFacts,
    pub effects: &'a EffectsPolicy,
    /// The `[runtime] warning-handler` posture (ADR-0049 §7).
    pub warning_handler_abort: bool,
    /// The `[runtime] final-keyword` posture (issue #234).
    pub final_keyword: FinalKeyword,
    /// Whether the PHP sidecar may run (the inverse of the CLI's `--no-php`).
    pub php: bool,
    /// Run the paranoid walk verifier ([`PARANOID_ENV`]) whatever the
    /// environment says. OR'd with the variable, so a caller that only wants
    /// the environment to decide passes `false` — which every caller but a
    /// test and `cargo xtask perf --paranoid` does.
    pub paranoid: bool,
}

/// What one gated run produced: the findings, plus everything the caller's
/// downstream pipeline needs without re-parsing (texts read through the seal,
/// the owned lowered trees in slot order), and the run's own ledger.
pub struct GenerationOutcome {
    pub findings: Vec<Diagnostic>,
    /// Diagnostic path → the file's text, read through the sealed capture, so
    /// "what was analyzed" and "what was fingerprinted" are the same bytes.
    pub texts: HashMap<String, String>,
    /// `(diagnostic path, lowered tree)` in universe-slot order — loaded or
    /// freshly parsed, indistinguishable downstream by design.
    pub trees: Vec<(String, SourceTree)>,
    /// The `[effects.attribution]` keys naming no symbol (ADR-0084 §5),
    /// rendered exactly as `steins-cli`'s own notice — computed here because
    /// the gated path must not force a salsa parse just to print it.
    pub attribution_notices: Vec<String>,
    pub report: GenerationReport,
}

/// Which temperature the run started at: whether a published generation was
/// there to load from. Per-package dispositions live in [`PackageReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationMode {
    Cold,
    Warm,
}

/// The run's ledger: what happened per package, what the fold table served,
/// where the time went, and which generation ended up published.
pub struct GenerationReport {
    pub mode: GenerationMode,
    /// The published (or confirmed-current) generation id, lowercase hex;
    /// `None` when publication failed (see [`Self::notes`]).
    pub generation: Option<String>,
    pub packages: Vec<PackageReport>,
    pub fold: FoldReport,
    pub walk: WalkReport,
    pub timings: PhaseTimings,
    /// Degradations and publication outcomes, human-readable, in order.
    pub notes: Vec<String>,
}

/// What the run's walks did (ADR-0092 §5, issue #489 slice B): how many files
/// were walked, how many replayed a persisted block instead, and — under the
/// paranoid verifier — whether the two ever disagreed.
pub struct WalkReport {
    /// Files this run actually walked.
    pub walked: usize,
    /// Files that replayed their persisted walk block. Always 0 under
    /// [`Self::paranoid`], where the walk is what ran.
    pub replayed: usize,
    /// Files the affected-set computation would have skipped — equal to
    /// [`Self::replayed`] outside paranoid mode, and the population the
    /// verifier graded inside it.
    pub would_skip: usize,
    /// Whether [`PARANOID_ENV`] was set for this run.
    pub paranoid: bool,
    /// Every file whose replayed block did not equal its fresh walk. Non-empty
    /// only under [`Self::paranoid`], and non-empty at all is a soundness bug
    /// in the affected set, not a cost regression.
    pub divergences: Vec<Divergence>,
}

/// One package's disposition — the counter the warm ≡ cold oracles read:
/// `loaded + parsed == files`, and an untouched tree warm-rebuilds with
/// `parsed == 0` everywhere.
pub struct PackageReport {
    pub name: String,
    pub kind: PackageKind,
    pub files: usize,
    /// Files whose trees came from the published artifact.
    pub loaded: usize,
    /// Files re-parsed from source this run.
    pub parsed: usize,
    /// Why, in one word ("loaded", "parsed (sources changed)", …).
    pub disposition: &'static str,
}

/// What the fold table did this run (ADR-0092 §4 through #500's transport).
pub struct FoldReport {
    /// Rows loaded from the published `__fold__` artifact (0 on a cold run or
    /// after an identity/whole-table miss).
    pub loaded_rows: usize,
    /// Questions the live engine had to answer (0 on a no-change warm run).
    pub fresh_rows: usize,
    /// Whether this run had a publishable table (a live engine that described
    /// itself); `false` under `--no-php` or a dead sidecar.
    pub table_published: bool,
}

/// Wall-clock milliseconds per phase, for the perf harness's cold/warm split.
#[derive(Debug, Clone, Copy)]
pub struct PhaseTimings {
    /// Store open + capture + fingerprints + reading texts through the seal.
    pub capture_ms: f64,
    /// Loading trees/shards from artifacts, or parsing — the phase the warm
    /// path exists to shrink.
    pub trees_ms: f64,
    /// The merge + `check_units` proper.
    pub analyze_ms: f64,
    /// Candidate build + publish (or the decision to keep `CURRENT`).
    pub persist_ms: f64,
}

impl PhaseTimings {
    #[must_use]
    pub fn total_ms(&self) -> f64 {
        self.capture_ms + self.trees_ms + self.analyze_ms + self.persist_ms
    }
}

/// The orchestration could not start (nothing was analyzed). Everything after
/// analysis — publication included — degrades to notes instead, because
/// findings in hand outrank a cache. The CLI maps any of these to "run as
/// today" with a stderr note.
#[derive(Debug)]
pub enum GenerationError {
    /// The store could not be opened or created.
    Store(io::Error),
    /// One package's sources could not be captured behind the seal.
    Capture { package: String, error: SourceError },
    /// A sealed file could not be read back (moved under the seal mid-run).
    Sealed(SourceDrift),
}

impl fmt::Display for GenerationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenerationError::Store(e) => write!(f, "cannot open the generation store: {e}"),
            GenerationError::Capture { package, error } => {
                write!(f, "cannot capture sources of package {package}: {error}")
            }
            GenerationError::Sealed(drift) => write!(f, "sealed source unreadable: {drift}"),
        }
    }
}

impl std::error::Error for GenerationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GenerationError::Store(e) => Some(e),
            GenerationError::Capture { error, .. } => Some(error),
            GenerationError::Sealed(drift) => Some(drift),
        }
    }
}

// ---------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------

/// One package's plan for this run: its identity, its slots in the universe,
/// and the fresh fingerprint the reuse decision compares against.
struct Plan {
    name: PackageName,
    kind: PackageKind,
    slots: Vec<usize>,
    fingerprint: Fingerprint,
}

/// One package's realized state after the load-or-parse phase.
struct PkgState {
    loaded: usize,
    parsed: usize,
    disposition: &'static str,
    /// Whether every persisted `(path, slot)` matches this run's universe, so
    /// the artifact's raw bytes may be copied into the next candidate.
    slots_stable: bool,
    /// A miss forced the reparse (as opposed to an expected change) — the
    /// republish-even-when-current trigger.
    degraded: bool,
}

/// Why a package did not load from the published generation.
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

/// What a successful load carries out of the artifact.
struct LoadedPkg {
    /// `(universe slot, tree)` for every file of the package.
    trees: Vec<(usize, SourceTree)>,
    /// The decoded shard, when the persisted slots still match this universe.
    shard: Option<PackageShard>,
    slots_stable: bool,
}

/// Run one generation lifecycle: open the store once, load `CURRENT` if it
/// serves, rebuild what changed, analyze the whole universe, publish. See the
/// module docs for the reuse and degradation rules.
pub fn generation_check(p: &GenerationParams<'_>) -> Result<GenerationOutcome, GenerationError> {
    let t_capture = Instant::now();
    let store = Store::open(p.store_root).map_err(GenerationError::Store)?;
    let mut notes: Vec<String> = Vec::new();
    let current: Option<Generation> = match store.current() {
        Ok(current) => current,
        Err(miss) => {
            notes.push(format!("published generation unreadable ({miss}); building cold"));
            None
        }
    };
    let mode = if current.is_some() { GenerationMode::Warm } else { GenerationMode::Cold };

    // The partition of this run's universe, and one sealed capture per package.
    let diag: Vec<String> = p.files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    let mut groups: BTreeMap<PackageName, Vec<usize>> = BTreeMap::new();
    for (slot, path) in diag.iter().enumerate() {
        groups.entry(p.partition.package_of(path).clone()).or_default().push(slot);
    }
    let mut plans: Vec<Plan> = Vec::with_capacity(groups.len());
    let mut inventories: Vec<SourceInventory> = Vec::with_capacity(groups.len());
    for (name, slots) in groups {
        let kind = p.partition.universe().get(&name).map_or(PackageKind::Root, |member| member.kind);
        let inventory =
            SourceInventory::capture(p.capture_root, slots.iter().map(|&s| p.files[s].as_path()))
                .map_err(|error| GenerationError::Capture { package: name.to_string(), error })?;
        let fingerprint = inventory.fingerprint();
        plans.push(Plan { name, kind, slots, fingerprint });
        inventories.push(inventory);
    }

    // Texts through the seal: what is analyzed is what was fingerprinted. The
    // per-file content hashes come off the same seal (issue #489 slice B needs
    // to know which files of a *changed* package actually changed, which the
    // package-level fingerprint cannot say).
    let mut texts: HashMap<String, String> = HashMap::with_capacity(diag.len());
    let mut contents: Vec<Option<Fingerprint>> =
        std::iter::repeat_n(None, diag.len()).collect();
    for (plan, inventory) in plans.iter().zip(&inventories) {
        for &slot in &plan.slots {
            let key = inventory.key_for(&p.files[slot]).ok_or_else(|| {
                GenerationError::Sealed(SourceDrift {
                    path: diag[slot].clone(),
                    kind: DriftKind::Uncaptured,
                })
            })?;
            contents[slot] = inventory.entry(&key).map(|entry| entry.content);
            let bytes = inventory.read(&key).map_err(GenerationError::Sealed)?;
            texts.insert(diag[slot].clone(), String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    let contents: Vec<Fingerprint> = contents
        .into_iter()
        .map(|c| c.expect("every captured file has a sealed content hash"))
        .collect();
    let capture_ms = ms(t_capture.elapsed());

    // Load-or-parse, per package. Any miss degrades that one package.
    let t_trees = Instant::now();
    let mut tree_slots: Vec<Option<SourceTree>> =
        std::iter::repeat_with(|| None).take(diag.len()).collect();
    let mut states: Vec<PkgState> = Vec::with_capacity(plans.len());
    let mut shards: Vec<PackageShard> = Vec::with_capacity(plans.len());
    // Per package, the walk blocks the published generation carries — the
    // replay candidates. Whether any of them may actually be replayed is not
    // knowable here: it needs the run's whole-universe verdicts, which only
    // exist once the analysis has computed them.
    let mut published_summaries: Vec<Option<StoredSummaries>> = Vec::with_capacity(plans.len());
    // The name delta's old side, per package, in load order. `None` means a
    // package's old contribution to the name space could not be read, which
    // makes the delta unknowable and walks the whole run: a name whose
    // disappearance is invisible cannot be reasoned about.
    let mut old_names: Vec<Option<Vec<String>>> = Vec::with_capacity(plans.len());
    for plan in &plans {
        let published = read_published(current.as_ref(), plan, &diag);
        published_summaries.push(published.summaries);
        old_names.push(published.old_names);
        match published.fresh {
            Ok(loaded) => {
                let slots_stable = loaded.slots_stable;
                for (slot, tree) in loaded.trees {
                    tree_slots[slot] = Some(tree);
                }
                shards.push(match loaded.shard {
                    Some(shard) => shard,
                    None => build_shard(plan, &tree_slots, &diag),
                });
                states.push(PkgState {
                    loaded: plan.slots.len(),
                    parsed: 0,
                    disposition: "loaded",
                    slots_stable,
                    degraded: false,
                });
            }
            Err(refusal) => {
                for &slot in &plan.slots {
                    tree_slots[slot] = Some(SourceTree::parse(&texts[&diag[slot]]));
                }
                shards.push(build_shard(plan, &tree_slots, &diag));
                let degraded = matches!(refusal, LoadRefusal::Miss(_));
                if let LoadRefusal::Miss(detail) = &refusal {
                    notes.push(format!("package {}: artifact miss ({detail}); reparsed", plan.name));
                }
                states.push(PkgState {
                    loaded: 0,
                    parsed: plan.slots.len(),
                    disposition: refusal.disposition(),
                    slots_stable: false,
                    degraded,
                });
            }
        }
    }
    let trees: Vec<SourceTree> =
        tree_slots.into_iter().map(|t| t.expect("every slot is filled above")).collect();
    let replay_candidates: usize =
        published_summaries.iter().flatten().map(|s| s.rows().count()).sum();

    // The name delta (issue #489 slice B): the union of the key sets of every
    // changed package's OLD and NEW shards. An untouched package contributes
    // nothing — both its sides are the same set — which is what makes the
    // delta proportional to the edit rather than to the universe.
    let mut delta: HashSet<String> = HashSet::new();
    let mut delta_known = true;
    for (i, plan) in plans.iter().enumerate() {
        if states[i].parsed == 0 && !states[i].degraded {
            continue;
        }
        match &old_names[i] {
            Some(names) => delta.extend(names.iter().cloned()),
            None => {
                // A name whose disappearance cannot be seen cannot be reasoned
                // about, so the sound answer is to walk everything.
                delta_known = false;
                notes.push(format!(
                    "package {}: its old symbols are unreadable; walking every file",
                    plan.name
                ));
            }
        }
        delta.extend(shards[i].contributed_names());
    }
    // A package the published generation had and this run does not: its names
    // vanished, and the files that referenced them must be walked.
    if let Some(generation) = current.as_ref() {
        let live: HashSet<&PackageName> = plans.iter().map(|plan| &plan.name).collect();
        let fold = fold_package();
        for gone in generation.packages().filter(|n| **n != fold && !live.contains(n)) {
            match generation.artifact(gone).and_then(|mut r| read_shard(&mut r)) {
                Ok(shard) => delta.extend(shard.contributed_names()),
                Err(miss) => {
                    delta_known = false;
                    notes.push(format!(
                        "removed package {gone}: its old symbols are unreadable ({miss}); walking every file"
                    ));
                }
            }
        }
    }
    let trees_ms = ms(t_trees.elapsed());

    // The fold table (ADR-0092 §4): warm over the published artifact when it
    // decodes, cold otherwise — a whole-table degradation, never a partial one.
    let live = if p.php { ProcessEngine::enabled() } else { ProcessEngine::new(true) };
    let mut fold_loaded_rows = 0usize;
    let mut fold_degraded = false;
    let engine = match current.as_ref().filter(|g| g.has_package(&fold_package())) {
        Some(generation) => {
            match generation.artifact(&fold_package()).and_then(|mut r| FoldTableArtifact::read(&mut r))
            {
                Ok(artifact) => {
                    fold_loaded_rows = artifact.rows.len();
                    RecordingEngine::warm(live, artifact)
                }
                Err(miss) => {
                    fold_degraded = true;
                    notes.push(format!("fold table miss ({miss}); folding cold"));
                    RecordingEngine::cold(live)
                }
            }
        }
        None => RecordingEngine::cold(live),
    };
    let mut folder = EngineFolder::with_engine(engine);
    folder.set_php_target(p.layout.php_target().cloned());
    // Force the engine's own `env` row now. `check_units` asks for it first
    // thing anyway (`folder.php_minor()` is its second statement), so this is
    // the same round trip at the same memo, moved earlier — and it is what
    // makes the engine posture, and therefore the replay stamp, available
    // before the first file is walked rather than after the last.
    crate::Folder::php_minor(&mut folder);
    let engine_posture = posture_of(folder.engine_identity().as_ref());
    let composer_lock = p
        .layout
        .roots()
        .last()
        .and_then(|root| std::fs::read(root.dir().join("composer.lock")).ok())
        .map(|bytes| Fingerprint::of_bytes("steins-gen/composer.lock", &bytes));
    // The replay stamp: the generation identity with the per-package source
    // fingerprints left out, because those are gated per package by the
    // `sources` section already. Everything else — the analyzer version, the
    // lock, the catalog pin, the plugin channel, the engine posture, the
    // finding-relevant config — must be unmoved before one persisted finding
    // may be replayed. (This is the re-audit the issue asks for: under slice A
    // an under-covered input cost a stale *cache*; here it would cost a stale
    // *finding*, so the gate is the whole identity rather than its package
    // half.)
    let stamp = *GenerationInputs {
        packages: Vec::new(),
        ..identity_inputs(p, composer_lock, engine_posture.clone())
    }
    .generation_id()
    .as_fingerprint();

    // The analysis proper — the same `check_units` every entry point runs,
    // over an index merged from the loaded-or-rebuilt shards (the merge is
    // partition-invariant, so this equals the cold constructions exactly).
    let t_analyze = Instant::now();
    let index = Index::from_merged(merge_shards(&shards));
    let units: Vec<FileUnit<'_>> =
        diag.iter().zip(&trees).map(|(path, tree)| FileUnit { path, tree }).collect();
    // The walk plan (issue #489 slice B). The planner is asked once, after the
    // run's whole-universe verdicts are computed and before the first file is
    // walked; it decides per file whether the persisted block may be replayed.
    let paranoid = paranoid_enabled(p);
    // Every file's persisted block, by slot, with the package that carries it
    // — so the licensing check (which needs the universe digest) is the only
    // thing left to do inside the planner.
    let blocks = block_index(&plans, &diag, &published_summaries, &contents);
    let changed: HashSet<usize> =
        (0..diag.len()).filter(|slot| blocks[*slot].is_none()).collect();
    // Nothing may replay at all unless there is a published generation to
    // replay from and the name delta could be read.
    let replay_possible = current.is_some() && delta_known && replay_candidates > 0;
    let affected = if replay_possible {
        affected_files(&AffectedInputs { trees: &trees, changed, delta })
    } else {
        (0..diag.len()).collect()
    };
    let mut universe: Option<Fingerprint> = None;
    let mut planner = |verdict: &UniverseVerdict<'_>| -> Vec<FilePlan> {
        let digest = universe_digest(verdict);
        universe = Some(digest);
        if !replay_possible {
            return Vec::new();
        }
        (0..diag.len())
            .map(|slot| match &blocks[slot] {
                // The whole-universe leg: a moved verdict refuses every row of
                // the section it stamped, so the file walks.
                Some((package, block))
                    if !affected.contains(&slot)
                        && published_summaries[*package]
                            .as_ref()
                            .is_some_and(|s| s.licensed_by(&stamp, &digest)) =>
                {
                    FilePlan::Replay((*block).clone())
                }
                _ => FilePlan::Walk,
            })
            .collect()
    };
    let mut control = WalkControl::new(&mut planner, paranoid);
    let findings = crate::check_units_controlled(
        &units,
        &index,
        &mut folder,
        p.warning_handler_abort,
        p.final_keyword,
        p.layout,
        p.plugins,
        p.effects,
        Some(&mut control),
    );
    drop(units);
    let attribution_notices = attribution_notices(&index, p.effects);
    let analyze_ms = ms(t_analyze.elapsed());
    let walk = WalkReport {
        walked: control.walked,
        replayed: control.replayed,
        would_skip: control.would_skip,
        paranoid,
        divergences: std::mem::take(&mut control.divergences),
    };
    let ledger = std::mem::take(&mut control.ledger);
    // The control's borrow of the planner — and so the planner's of the two
    // values it writes — ends here; everything either produced is owned above.
    drop(control);
    let universe = universe.expect("the planner runs before the first file is walked");
    for divergence in &walk.divergences {
        notes.push(format!("PARANOID DIVERGENCE {divergence}"));
    }
    if replay_candidates > 0 {
        notes.push(format!(
            "{} file(s) replayed a persisted walk block, {} walked ({replay_candidates} block(s) were on offer)",
            walk.replayed, walk.walked
        ));
    }
    if paranoid {
        // Under the verifier, say which universe verdict this run computed:
        // over a corpus tree, two runs whose digests differ walked everything
        // for that reason, and the auditor should see that rather than infer
        // it from a would-skip count of zero.
        notes.push(format!(
            "paranoid: {} file(s) walked, {} would have been skipped, {} divergence(s); universe verdict {}",
            walk.walked,
            walk.would_skip,
            walk.divergences.len(),
            universe.to_hex(),
        ));
    }

    // Identity, honestly filled (see the module docs for in/out reasoning).
    let fold_table = folder.published_table();
    let fold_fresh = folder.fresh_keys().len();
    let inputs = GenerationInputs {
        packages: plans.iter().map(|plan| (plan.name.clone(), plan.fingerprint)).collect(),
        ..identity_inputs(p, composer_lock, engine_posture)
    };
    let id = inputs.generation_id();

    // Publish — or keep CURRENT when this run *is* the published generation
    // and nothing degraded (a degradation republishes to repair the artifact).
    let t_persist = Instant::now();
    let total_parsed: usize = states.iter().map(|s| s.parsed).sum();
    let any_degraded = states.iter().any(|s| s.degraded) || fold_degraded;
    let reuse =
        current.as_ref().is_some_and(|g| g.id() == &id) && total_parsed == 0 && !any_degraded;
    let generation_hex = if reuse {
        notes.push("generation already current; nothing republished".to_owned());
        Some(id.to_hex())
    } else {
        match publish(
            &store,
            id,
            inventories,
            &plans,
            &states,
            &shards,
            &trees,
            &diag,
            current.as_ref(),
            fold_table.as_ref(),
            &Summaries {
                stamp,
                universe,
                diag: &diag,
                contents: &contents,
                ledger: &ledger,
            },
        ) {
            Ok(hex) => Some(hex),
            Err(detail) => {
                notes.push(format!("publish failed ({detail}); this run's findings are unaffected"));
                None
            }
        }
    };
    let persist_ms = ms(t_persist.elapsed());

    let packages = plans
        .iter()
        .zip(&states)
        .map(|(plan, state)| PackageReport {
            name: plan.name.to_string(),
            kind: plan.kind,
            files: plan.slots.len(),
            loaded: state.loaded,
            parsed: state.parsed,
            disposition: state.disposition,
        })
        .collect();
    Ok(GenerationOutcome {
        findings,
        texts,
        trees: diag.into_iter().zip(trees).collect(),
        attribution_notices,
        report: GenerationReport {
            mode,
            generation: generation_hex,
            packages,
            fold: FoldReport {
                loaded_rows: fold_loaded_rows,
                fresh_rows: fold_fresh,
                table_published: fold_table.is_some(),
            },
            walk,
            timings: PhaseTimings { capture_ms, trees_ms, analyze_ms, persist_ms },
            notes,
        },
    })
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
/// of its files did.
fn block_index<'a>(
    plans: &[Plan],
    diag: &[String],
    summaries: &'a [Option<StoredSummaries>],
    contents: &[Fingerprint],
) -> Vec<Option<(usize, &'a FileWalk)>> {
    let mut out: Vec<Option<(usize, &FileWalk)>> = vec![None; diag.len()];
    for (package, (plan, summaries)) in plans.iter().zip(summaries).enumerate() {
        let Some(summaries) = summaries else { continue };
        let rows: HashMap<&str, (&Fingerprint, &FileWalk)> =
            summaries.rows().map(|(path, content, walk)| (path, (content, walk))).collect();
        for &slot in &plan.slots {
            if let Some((content, walk)) = rows.get(diag[slot].as_str())
                && **content == contents[slot]
            {
                out[slot] = Some((package, walk));
            }
        }
    }
    out
}

/// Everything the published generation can say about one package: the delta's
/// old side, the walk blocks it carries, and the load attempt proper.
struct Published {
    /// The names this package's OLD shard contributed, or `None` when the
    /// artifact could not give them — which makes the name delta *unknowable*,
    /// and the whole run walks (see [`generation_check`]).
    old_names: Option<Vec<String>>,
    /// The package's persisted walk blocks, read whatever the source
    /// fingerprint said: a changed package's *unchanged* files may still
    /// replay, since each row carries its own file's content hash.
    summaries: Option<StoredSummaries>,
    fresh: Result<LoadedPkg, LoadRefusal>,
}

impl Published {
    /// The shape for a package the published generation could not tell us
    /// anything about at all.
    fn refused(refusal: LoadRefusal, old_names: Option<Vec<String>>) -> Self {
        Self { old_names, summaries: None, fresh: Err(refusal) }
    }
}

/// Read what the published generation holds for one package. Any decode
/// failure is a [`LoadRefusal::Miss`] for this package alone; only the *old
/// names* being unreadable is wider, because a name whose disappearance cannot
/// be seen cannot be reasoned about.
fn read_published(
    generation: Option<&Generation>,
    plan: &Plan,
    diag: &[String],
) -> Published {
    let Some(generation) = generation else {
        // A cold run: the published universe is empty, which is a *known*
        // empty old side rather than an unknown one. Nothing replays anyway.
        return Published::refused(LoadRefusal::NoGeneration, Some(Vec::new()));
    };
    if !generation.has_package(&plan.name) {
        // A package the generation never had contributes no old names — again
        // known, not unknown, so its arrival is an ordinary delta.
        return Published::refused(LoadRefusal::NotPublished, Some(Vec::new()));
    }
    let miss = |m: Miss| LoadRefusal::Miss(m.to_string());
    let mut reader = match generation.artifact(&plan.name) {
        Ok(reader) => reader,
        Err(m) => return Published::refused(miss(m), None),
    };
    // The old shard, always: it is the delta's old side whether or not the
    // sources moved, and it is the verbatim shard when they did not.
    let old_shard = read_shard(&mut reader).ok();
    let old_names = old_shard.as_ref().map(PackageShard::contributed_names);
    // The walk blocks are never a load refusal: the trees are in hand either
    // way, and a package without them simply walks every file.
    let summaries = read_summaries(&mut reader).ok();
    let fresh = load_trees(&mut reader, plan, diag, old_shard);
    Published { old_names, summaries, fresh }
}

/// The load proper: the provenance gate, then the per-file trace shards.
fn load_trees(
    reader: &mut steins_gen::ArtifactReader,
    plan: &Plan,
    diag: &[String],
    old_shard: Option<PackageShard>,
) -> Result<LoadedPkg, LoadRefusal> {
    let miss = |m: Miss| LoadRefusal::Miss(m.to_string());
    let (analyzer, stored) = read_sources(reader).map_err(miss)?;
    if analyzer != analyzer_version() {
        return Err(LoadRefusal::AnalyzerMoved);
    }
    if stored != plan.fingerprint {
        return Err(LoadRefusal::Changed);
    }
    let trace = TraceIndex::open(reader).map_err(miss)?;
    let persisted: HashMap<String, usize> =
        trace.files().map(|(path, slot)| (path.to_owned(), slot)).collect();
    let mut slots_stable = persisted.len() == plan.slots.len();
    let mut loaded: Vec<(usize, SourceTree)> = Vec::with_capacity(plan.slots.len());
    for &slot in &plan.slots {
        let path = diag[slot].as_str();
        if persisted.get(path) != Some(&slot) {
            slots_stable = false;
        }
        let tree = trace.read_tree(reader, path).map_err(miss)?;
        loaded.push((slot, tree));
    }
    // The shard's sites are universe slots; it may only serve verbatim when
    // every persisted slot still names the same file. Otherwise the caller
    // rebuilds it from the loaded trees — still no reparse.
    let shard = if slots_stable {
        Some(old_shard.ok_or(LoadRefusal::Miss("symbols section is not a shard".to_owned()))?)
    } else {
        None
    };
    Ok(LoadedPkg { trees: loaded, shard, slots_stable })
}

/// Build one package's shard from its (loaded or fresh) trees.
fn build_shard(plan: &Plan, tree_slots: &[Option<SourceTree>], diag: &[String]) -> PackageShard {
    let mut shard = PackageShard::default();
    for &slot in &plan.slots {
        let tree = tree_slots[slot].as_ref().expect("the package's trees are in hand");
        shard.add_file(slot, &diag[slot], tree);
    }
    shard
}

/// Assemble and publish the candidate. Every failure is one string for the
/// caller's note — publication is a cache write, never the run's verdict.
#[allow(clippy::too_many_arguments)]
fn publish(
    store: &Store,
    id: GenerationId,
    inventories: Vec<SourceInventory>,
    plans: &[Plan],
    states: &[PkgState],
    shards: &[PackageShard],
    trees: &[SourceTree],
    diag: &[String],
    current: Option<&Generation>,
    fold_table: Option<&FoldTableArtifact>,
    summaries: &Summaries<'_>,
) -> Result<String, String> {
    let mut candidate = store.begin(id, inventories).map_err(|e| format!("begin: {e}"))?;
    for ((plan, state), shard) in plans.iter().zip(states).zip(shards) {
        // An untouched package with stable slots republishes its exact bytes;
        // anything else reassembles the sections from the trees in hand.
        let copied = (state.parsed == 0 && state.slots_stable)
            .then(|| current.and_then(|generation| copy_artifact(generation, plan)))
            .flatten();
        let mut builder = match copied {
            Some(builder) => builder,
            None => build_artifact(plan, shard, trees, diag),
        };
        // The `summaries` section is always this run's, even where the other
        // four sections were byte-copied: the copied ones are functions of the
        // sources alone, this one is a function of the whole run identity, and
        // republishing a stale stamp would only refuse itself on the next run.
        summaries.write(&mut builder, plan);
        candidate
            .write_artifact(&plan.name, &builder)
            .map_err(|e| format!("write {}: {e}", plan.name))?;
    }
    if let Some(table) = fold_table {
        candidate
            .write_artifact(&fold_package(), &table.to_builder())
            .map_err(|e| format!("write {}: {e}", fold_package()))?;
    }
    let generation = candidate.publish().map_err(|e| e.to_string())?;
    Ok(generation.id().to_hex())
}

/// One package's artifact from scratch: the #503 sections plus the provenance
/// record the warm path's reuse decision reads.
fn build_artifact(
    plan: &Plan,
    shard: &PackageShard,
    trees: &[SourceTree],
    diag: &[String],
) -> ArtifactBuilder {
    let mut contracts = Vec::new();
    let mut files: Vec<TraceFile<'_>> = Vec::with_capacity(plan.slots.len());
    for &slot in &plan.slots {
        contracts.extend(decl_contracts(slot, &trees[slot]));
        files.push(TraceFile { path: &diag[slot], slot, tree: &trees[slot] });
    }
    let mut builder = build_sections(shard, &contracts, &files);
    builder
        .section(sources_section(), sources_payload(&plan.fingerprint))
        .expect("distinct section names");
    builder
}

/// This run's walk blocks on their way to disk: the two stamps that license
/// replaying them, the per-file content hashes that say which files moved, and
/// the ledger `check_units` filled in unit order.
struct Summaries<'a> {
    stamp: Fingerprint,
    universe: Fingerprint,
    diag: &'a [String],
    contents: &'a [Fingerprint],
    ledger: &'a [FileWalk],
}

impl Summaries<'_> {
    /// Add one package's rows to its artifact. A run whose ledger is short —
    /// which cannot happen, since `check_units` records every unit — writes
    /// the rows it has; the reader keys by path and a missing row simply
    /// cannot be replayed.
    fn write(&self, builder: &mut ArtifactBuilder, plan: &Plan) {
        let rows: Vec<SummaryRow<'_>> = plan
            .slots
            .iter()
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

/// Copy an untouched package's artifact bytes section-for-section into a new
/// builder. `None` on any read failure — the caller rebuilds from trees.
fn copy_artifact(generation: &Generation, plan: &Plan) -> Option<ArtifactBuilder> {
    let mut reader = generation.artifact(&plan.name).ok()?;
    let mut builder = ArtifactBuilder::new();
    for section in [symbols_section(), contracts_section(), trace_section(), sources_section()] {
        let bytes = reader.section(&section).ok()?;
        builder.section(section, bytes).ok()?;
    }
    Some(builder)
}

/// The `[effects.attribution]` config-hygiene notices, mirroring
/// `steins-cli/src/project.rs::attribution_notices` byte-for-byte (the gated
/// path computes them here so no salsa parse is forced just to print them).
fn attribution_notices(index: &Index, policy: &EffectsPolicy) -> Vec<String> {
    if policy.is_empty() {
        return Vec::new();
    }
    let known = |name: &str| {
        !matches!(index.resolve_class(name), Res::Absent)
            || !matches!(index.resolve_function(name), Res::Absent)
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

/// The engine posture from the run's own recorded boot surface, or
/// [`EnginePosture::Off`] for an engine that never described itself
/// (`--no-php`, a dead sidecar, an old runner).
fn posture_of(identity: Option<&crate::FoldTableIdentity>) -> EnginePosture {
    match identity {
        Some(identity) => EnginePosture::On {
            php_version: identity.php_version.clone(),
            // `None` (an engine that did not say) is encoded as 0 — a width no
            // real engine reports, so it stays its own identity.
            int_size: identity.int_size.and_then(|s| u8::try_from(s).ok()).unwrap_or(0),
            extensions: identity.extensions.clone(),
            fold_lane: identity.fold_lane.clone(),
        },
        None => EnginePosture::Off,
    }
}

/// Everything the generation identity covers except the per-package source
/// fingerprints — see the module docs for what is in and what is deliberately
/// out. Filled once and used twice: whole (plus the packages) as the
/// generation id, and with `packages` emptied as the replay stamp of issue
/// #489 slice B, so the two can never drift apart.
fn identity_inputs(
    p: &GenerationParams<'_>,
    composer_lock: Option<Fingerprint>,
    engine: EnginePosture,
) -> GenerationInputs {
    GenerationInputs {
        analyzer_version: analyzer_version().to_owned(),
        packages: Vec::new(),
        composer_lock,
        catalog_pin: format!(
            "php-{}.{}",
            steins_catalog::PINNED_PHP.0,
            steins_catalog::PINNED_PHP.1
        ),
        plugins: plugin_identity(p.plugins),
        engine,
        config: config_identity(p),
    }
}

/// The plugin channel's finding-relevant content as identity strings: the
/// registered labels and the accepted colorings. Hashed sorted by
/// [`GenerationInputs::generation_id`], so the order here is immaterial.
fn plugin_identity(plugins: &PluginFacts) -> Vec<String> {
    let mut out: Vec<String> =
        plugins.registry().extensions().iter().map(|label| format!("label:{label}")).collect();
    for (name, labels) in plugins.colorings() {
        out.push(format!("effect:{name}={}", labels.join(",")));
    }
    out
}

/// The finding-relevant config as identity pairs — see the module docs for
/// what is covered and what is deliberately out.
fn config_identity(p: &GenerationParams<'_>) -> Vec<(String, String)> {
    let mut config = vec![
        ("effects.tolerated".to_owned(), p.effects.tolerated().join(",")),
        (
            "runtime.warning-handler-abort".to_owned(),
            p.warning_handler_abort.to_string(),
        ),
        ("runtime.final-keyword".to_owned(), format!("{:?}", p.final_keyword)),
        ("layout".to_owned(), format!("{:?}", p.layout)),
    ];
    for key in p.effects.attribution_keys() {
        config.push((
            format!("effects.attribution:{key}"),
            p.effects.function_attribution(key).join(","),
        ));
    }
    config
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
