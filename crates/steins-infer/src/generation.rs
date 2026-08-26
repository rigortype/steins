//! The generation orchestrator (ADR-0092 §5, issue #489): cold build →
//! publish → warm rebuild, composed entirely from landed pieces — the store and
//! sealed capture (#485), the Composer partition (#486), the per-package
//! payloads (#503), and the recorded fold table (#500).
//!
//! **What the warm path reuses, and why it is sound.** Two layers, landed in
//! that order.
//!
//! *Trees* (slice A, made per file by issue #512). A file whose captured
//! content fingerprint matches the one its artifact row carries loads its
//! lowered [`SourceTree`] from the `trace` section instead of re-parsing — the
//! 45–57% cost `docs/agents/profiling.md` names. The fingerprint is blake3
//! over the captured bytes and parsing is deterministic, so a loaded tree *is*
//! the reparse; everything downstream recomputes from those trees exactly as a
//! cold run recomputes from freshly parsed ones.
//!
//! The gate is per **file**, not per package, because the `trace` section is:
//! it is a directory of independently addressable payloads, and a warm run
//! already reads exactly the ones it wants. The package's own fingerprint
//! survives as a *shortcut* — when it matches, every file of the package is
//! unmoved and no row need be consulted — and the per-file fingerprint is the
//! one the `summaries` rows already carry, so "which files changed" has one
//! spelling that the load gate, the name delta and the walk plan all read (one
//! predicate, `unmoved_rows`, is what all three call). A package with no
//! matching artifact still parses everything, and so does any file whose
//! stored fingerprint cannot be established.
//!
//! A package can therefore be genuinely **mixed** — some files loaded, some
//! parsed — which the [`PackageReport`] disposition names and which forbids
//! two economies the all-or-nothing gate could take: the persisted shard
//! cannot serve verbatim (its sites are universe slots and its symbols are the
//! *old* file's, so a package with any parsed file rebuilds its shard from the
//! trees in hand — no reparse either way), and the artifact cannot be
//! republished byte-for-byte.
//!
//! *Walks* (slice B). A file whose walk block nothing could have changed
//! replays that block from the `summaries` section instead of walking. What
//! "nothing could have changed" means is [`crate::affected`]'s whole subject;
//! what a block is, and why replaying one reproduces the run, is
//! [`crate::walk_plan`]'s. The two project-wide passes (effects, throws) are
//! never replayed — they recompute whole-universe from own-rows every run,
//! which is what makes the vendor-whitespace oracle pass by construction.
//!
//! Both layers hold warm ≡ cold by construction rather than by comparison, and
//! `warm_generation.rs` pins it byte-for-byte. The skip layer additionally
//! ships its own instrument: [`PARANOID_ENV`] walks everything anyway and
//! grades every would-be skip against its fresh walk.
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
//! **The same identity, minus the packages, is the replay stamp** (slice B).
//! A tree fingerprint licenses loading a *parse*, because parsing is a pure
//! function of bytes; replaying a *finding* needs every other input above to
//! be unmoved too, and the per-package half is already gated per package by
//! the `sources` section. So [`identity_inputs`] is filled once and used
//! twice — with `packages` as the generation id, with `packages` emptied as
//! the stamp the `summaries` section carries — and the two cannot drift. This
//! is issue #489's closing re-audit answered: an under-covered input cost a
//! stale *cache* while findings were never loaded, and would now cost a stale
//! *finding*.
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
use std::sync::{Arc, Mutex};
use std::time::Instant;

use steins_db::persist::{
    PayloadIndex, TraceIndex, build_sections, contract_payload, contracts_section, facts_section,
    payload_section_bytes, read_shard, trace_payload,
};
use steins_db::{
    EffectsPolicy, PackagePartition, PackageShard, PluginFacts, ProjectLayout, merge_shards,
};
use steins_gen::{
    ArtifactBuilder, DriftKind, EnginePosture, FieldHasher, Fingerprint, Generation, GenerationId,
    GenerationInputs, Miss, PackageName, SectionName, ShareKind, SourceDrift, SourceError,
    SourceInventory, Store,
};
pub use steins_gen::PackageKind;
use steins_syntax::SourceTree;

use crate::affected::{AffectedInputs, affected_files};
use crate::facts::{FileFacts, facts_payload, fill_rows, key_hash, read_facts};
use crate::fold_persist::{FoldTableArtifact, RecordingEngine, fold_package};
use crate::project::{FileUnit, Index, LazyTree, Res};
use crate::summaries::{Summaries as StoredSummaries, SummaryRow, read_summaries, write_summaries};
use crate::walk_plan::{FilePlan, FileWalk, UniverseVerdict, WalkControl};
use crate::{Diagnostic, Divergence, EngineFolder, FinalKeyword, ProcessEngine};

// ---------------------------------------------------------------------------
// The orchestrator's own section: which sources an artifact was built from.
// ---------------------------------------------------------------------------

/// The section holding the package's provenance record: a JSON object with the
/// analyzer version that built the artifact and the package's source
/// fingerprint ([`SourceInventory::fingerprint`], hex). The analyzer version
/// gates the package's whole load; the fingerprint is the shortcut that says
/// *every* file is unmoved, and when it does not match the load falls to the
/// per-file gate rather than to reparsing the package.
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
/// downstream pipeline needs without re-parsing (the texts the capture sealed,
/// the owned lowered trees in slot order), and the run's own ledger.
pub struct GenerationOutcome {
    pub findings: Vec<Diagnostic>,
    /// Diagnostic path → the file's text, handed back by the capture that
    /// hashed it (issue #521), so "what was analyzed" and "what was
    /// fingerprinted" are the same bytes by construction.
    pub texts: HashMap<String, String>,
    /// `(diagnostic path, tree handle)` in universe-slot order. A handle, not
    /// a tree, since issue #516: a warm run decodes a file's tree only where
    /// something reaches it, and forcing all of them to hand the caller owned
    /// values would undo exactly that. Deref gives the tree; the caller should
    /// touch only the files it needs (the CLI's inline-suppression scan reads
    /// the files a finding names, and no others).
    pub trees: Vec<(String, LazyTree<'static>)>,
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
    /// Artifacts this publish **shared** with the published generation rather
    /// than rewriting (issue #519) — reflinked, hard-linked or, on a
    /// filesystem with neither, copied. Zero on a cold build, on a run that
    /// republished nothing, and for every package the edit actually moved.
    pub shared_artifacts: usize,
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
    /// The first few files whose replayed block did not equal its fresh walk
    /// — capped, so a systematically broken affected set over a corpus reports
    /// a sample rather than exhausting memory. Non-empty only under
    /// [`Self::paranoid`], and non-empty at all is a soundness bug in the
    /// affected set, not a cost regression.
    pub divergences: Vec<Divergence>,
    /// How many divergences there were in all, capped list or not.
    pub divergence_count: usize,
}

/// One package's disposition — the counter the warm ≡ cold oracles read:
/// `loaded + parsed == files`, and an untouched tree warm-rebuilds with
/// `parsed == 0` everywhere.
pub struct PackageReport {
    pub name: String,
    pub kind: PackageKind,
    pub files: usize,
    /// Files whose trees are served by the published artifact.
    pub loaded: usize,
    /// Files re-parsed from source this run.
    pub parsed: usize,
    /// Files whose tree was actually **decoded** — a subset of
    /// [`Self::loaded`], and the counter issue #516 exists to drive to zero on
    /// a no-change warm run. `loaded` says the artifact can serve the file;
    /// this says something asked.
    pub decoded: usize,
    /// Why, in one word: `"loaded"` (every file came from the artifact),
    /// `"mixed (…)"` (issue #512 — some did and some did not, which is what an
    /// edit inside a package looks like), or `"parsed (…)"` (none did, and the
    /// parenthesis says why the artifact could not be read from at all).
    pub disposition: &'static str,
}

impl PackageReport {
    /// Whether this package both loaded and parsed — the case the per-file
    /// gate (issue #512) added to the vocabulary.
    #[must_use]
    pub fn is_mixed(&self) -> bool {
        self.loaded > 0 && self.parsed > 0
    }
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
    /// Store open + capture: one read and one hash per file, with the texts
    /// the analysis reads falling out of that same pass (issue #521).
    pub capture_ms: f64,
    /// Loading trees/shards from artifacts, or parsing — the phase the warm
    /// path exists to shrink.
    pub trees_ms: f64,
    /// The merge + `check_units` proper — the sum of the five splits below.
    pub analyze_ms: f64,
    /// The shard merge (ADR-0092 §3), whole-universe by construction.
    pub merge_ms: f64,
    /// The whole-universe per-file facts a walk reads: the dam, the
    /// never-returning veto set, the parse-failure sweep, the PHP view.
    pub facts_ms: f64,
    /// The effects fixpoint, when a consumer's gate forced it.
    pub effects_ms: f64,
    /// The throws fixpoint, likewise.
    pub throws_ms: f64,
    /// The per-file walk loop — walks and replays together.
    pub walk_ms: f64,
    /// The two project-wide reporting passes off the fixpoints, and the
    /// attribution-notice sweep.
    pub report_ms: f64,
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
    /// Whether the package's *whole* source fingerprint matched the published
    /// one — that no file of it was added, removed or edited.
    ///
    /// This, and never `parsed == 0`, is what says a package contributes
    /// nothing to the name delta. The per-file gate (issue #512) separated the
    /// two: a package that lost a file has every *surviving* file loadable, so
    /// it can parse nothing at all while the names of the file it lost are
    /// gone from the universe — and those names must reach the delta, or the
    /// callers that named them replay a stale absence.
    sources_match: bool,
    /// A miss forced the reparse (as opposed to an expected change) — the
    /// republish-even-when-current trigger.
    degraded: bool,
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
struct OpenArtifact {
    reader: Mutex<steins_gen::ArtifactReader>,
    trace: TraceIndex,
    /// The per-file `contracts` and `facts` directories, when they decode —
    /// the republish path's copy sources.
    contracts: Option<PayloadIndex>,
    facts: Option<PayloadIndex>,
}

impl OpenArtifact {
    /// One file's payload from `index`, or `None` when the artifact cannot
    /// give it. Used only by the republish path, where a `None` means "encode
    /// it instead".
    fn copy(&self, index: Option<&PayloadIndex>, path: &str) -> Option<Vec<u8>> {
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
    let capture_ms = ms(t_capture.elapsed());

    // Load-or-parse, per package. Any miss degrades that one package.
    //
    // "Load" no longer means *decode* (issue #516). A file the artifact can
    // serve gets a deferred [`LazyTree`] and its persisted [`FileFacts`]; the
    // facts answer every whole-universe phase, and the tree is decoded only if
    // something reaches it — a walk of the file, or a walk that descends into
    // it. A file the artifact cannot serve is parsed here and its facts are
    // derived from that parse, which is the same value by construction.
    let t_trees = Instant::now();
    let mut lazy_slots: Vec<Option<LazyTree<'static>>> =
        std::iter::repeat_with(|| None).take(diag.len()).collect();
    let mut fact_slots: Vec<Option<FileFacts>> =
        std::iter::repeat_with(|| None).take(diag.len()).collect();
    // Per slot: whether this run's facts payload is the published one, so
    // republishing may copy its bytes instead of re-encoding them. Cleared
    // below for any file whose own rows this run recomputes.
    let mut facts_copyable: Vec<bool> = vec![false; diag.len()];
    let mut states: Vec<PkgState> = Vec::with_capacity(plans.len());
    let mut shards: Vec<PackageShard> = Vec::with_capacity(plans.len());
    // The open artifacts, kept for the run: a deferred tree load reads one
    // whenever a walk reaches its file, so the reader outlives this loop.
    let mut artifacts: Vec<Option<Arc<OpenArtifact>>> = Vec::with_capacity(plans.len());
    // The walk blocks the published generation carries — the replay
    // candidates, keyed by path over the whole universe (issue #519 moved them
    // out of the per-package artifacts, which had to become a function of the
    // sources alone to be shareable). Whether any of them may actually be
    // replayed is not knowable here: it needs the run's whole-universe
    // verdicts, which only exist once the analysis has computed them.
    let published_summaries: Option<StoredSummaries> = current
        .as_ref()
        .and_then(|generation| generation.summaries().ok())
        .and_then(|mut reader| read_summaries(&mut reader).ok());
    // The name delta's old side, per package, in load order. `None` means one
    // of two things, and the delta loop below can tell them apart from the
    // package's state: either the old shard could not be read — which makes
    // the delta unknowable and walks the whole run, since a name whose
    // disappearance is invisible cannot be reasoned about — or it was taken to
    // serve as this run's shard verbatim, which only happens for a package
    // whose sources did not move and which therefore contributes no delta.
    let mut old_shards: Vec<Option<PackageShard>> = Vec::with_capacity(plans.len());
    for plan in &plans {
        let mut published =
            read_published(current.as_ref(), published_summaries.as_ref(), plan, &diag, &contents);
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
    let lazy: Vec<LazyTree<'static>> =
        lazy_slots.into_iter().map(|t| t.expect("every slot is filled above")).collect();
    let mut facts: Vec<FileFacts> =
        fact_slots.into_iter().map(|f| f.expect("every slot is filled above")).collect();
    let replay_candidates: usize =
        published_summaries.as_ref().map_or(0, |s| s.rows().count());

    // Which files moved, and which persisted block each of the others could
    // replay. Computed here rather than beside the walk plan because the name
    // delta is a question about the *files* that changed (issue #510), not
    // about the packages holding them.
    let blocks = block_index(&plans, &diag, published_summaries.as_ref(), &contents);
    let changed: HashSet<usize> = (0..diag.len()).filter(|slot| blocks[*slot].is_none()).collect();

    // The name delta (issue #489 slice B, tightened to file granularity by
    // issue #510): over every changed package, the names its OLD shard sites
    // in a file that changed, and the names its NEW shard sites in one. A
    // package whose sources did not move contributes nothing — both its sides
    // are the same set — and a package that moved contributes only what its
    // moved *files* declare, which is what makes the delta proportional to the
    // edit rather than to the package. The one member with no site to answer
    // for it — a package's ambiguity set — rides a changed package wholesale;
    // `PackageShard::contributed_names_from` says why.
    //
    // "Did not move" is the package's own source fingerprint and never "parsed
    // nothing" (issue #512, `PkgState::sources_match`): under the per-file gate
    // a package that *lost* a file loads every survivor and parses nothing at
    // all, while the names the lost file declared are gone from the universe
    // and must reach the delta.
    let now: HashMap<&str, usize> =
        diag.iter().enumerate().map(|(slot, path)| (path.as_str(), slot)).collect();
    // Hashed the way the persisted footprints are (`facts::key_hash`), since
    // that is the form the affected set now compares against.
    let mut delta: HashSet<u64> = HashSet::new();
    let mut delta_known = true;
    for (i, plan) in plans.iter().enumerate() {
        if states[i].sources_match && !states[i].degraded {
            continue;
        }
        match &old_shards[i] {
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
        delta.extend(shards[i].contributed_names_from(&moved).iter().map(|k| key_hash(k)));
    }
    // A package the published generation had and this run does not: its names
    // vanished, and the files that referenced them must be walked. Wholesale
    // and deliberately so — every file it held left it, so no unchanged file
    // of its own is left to narrow the set by.
    if let Some(generation) = current.as_ref() {
        let live: HashSet<&PackageName> = plans.iter().map(|plan| &plan.name).collect();
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
    let merge_ms = ms(t_analyze.elapsed());
    let units: Vec<FileUnit<'_>> =
        diag.iter().zip(&lazy).map(|(path, tree)| FileUnit { path, tree }).collect();
    // The walk plan (issue #489 slice B). The planner is asked once, after the
    // run's whole-universe verdicts are computed and before the first file is
    // walked; it decides per file whether the persisted block may be replayed.
    let paranoid = paranoid_enabled(p);
    // `blocks` — every file's persisted block, by slot, with the package that
    // carries it — was built with the delta above, so the licensing check
    // (which needs the universe digest) is all the planner has left to do.
    // Nothing may replay at all unless there is a published generation to
    // replay from and the name delta could be read.
    let replay_possible = current.is_some() && delta_known && replay_candidates > 0;
    let affected: HashSet<usize> = if replay_possible {
        affected_files(&AffectedInputs { facts: &facts, changed, delta })
    } else {
        (0..diag.len()).collect()
    };
    // The own rows of an affected file are the one part of its facts that this
    // run must recompute: they are resolution-dependent, and `affected` is
    // exactly the over-approximation of "some resolution this file makes could
    // have moved" (see `facts.rs` for the argument, and why the tree-derived
    // half of the same row is licensed by the content fingerprint alone). An
    // affected file is walked, so its tree is in hand either way.
    for slot in &affected {
        facts[*slot].rows = None;
        facts_copyable[*slot] = false;
    }
    fill_rows(&mut facts, &units, &index, p.plugins, p.effects);
    let mut universe: Option<Fingerprint> = None;
    let mut planner = |verdict: &UniverseVerdict<'_>| -> Vec<FilePlan> {
        let digest = universe_digest(verdict);
        universe = Some(digest);
        if !replay_possible {
            return Vec::new();
        }
        // The whole-universe leg: a moved verdict refuses every row of the
        // sidecar it stamped, so every file walks. One sidecar, one licence
        // check (issue #519).
        let licensed = published_summaries
            .as_ref()
            .is_some_and(|s| s.licensed_by(&stamp, &digest));
        (0..diag.len())
            .map(|slot| match blocks[slot] {
                Some(block) if licensed && !affected.contains(&slot) => {
                    FilePlan::Replay(block.clone())
                }
                _ => FilePlan::Walk,
            })
            .collect()
    };
    let mut control = WalkControl::new(&mut planner, paranoid, &facts);
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
        divergence_count: control.divergence_count,
    };
    let passes = control.passes;
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
            walk.divergence_count,
            universe.to_hex(),
        ));
    }

    // Identity, honestly filled (see the module docs for in/out reasoning).
    let fold_table = folder.published_table();
    let fold_fresh = folder.fresh_keys().len();
    let fold_unchanged = folder.table_unchanged();
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
    let mut shared_artifacts = 0usize;
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
            current.as_ref(),
            Fold { table: fold_table.as_ref(), unchanged: fold_unchanged },
            &Summaries {
                stamp,
                universe,
                diag: &diag,
                contents: &contents,
                ledger: &ledger,
            },
            &Payloads {
                lazy: &lazy,
                facts: &facts,
                copyable: &facts_copyable,
                artifacts: &artifacts,
                diag: &diag,
            },
        ) {
            Ok((hex, shared)) => {
                shared_artifacts = shared.total();
                notes.extend(shared.note());
                Some(hex)
            }
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
            decoded: plan.slots.iter().filter(|&&slot| lazy[slot].was_loaded()).count(),
            disposition: state.disposition,
        })
        .collect();
    Ok(GenerationOutcome {
        findings,
        // Every deferred handle is dropped with `lazy` below, so the texts come
        // back without a copy in the ordinary case.
        texts: texts
            .into_iter()
            .map(|(path, text)| {
                (path, Arc::try_unwrap(text).unwrap_or_else(|shared| (*shared).clone()))
            })
            .collect(),
        trees: diag.into_iter().zip(lazy).collect(),
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
            timings: PhaseTimings {
                capture_ms,
                trees_ms,
                analyze_ms,
                merge_ms,
                facts_ms: passes.facts_ms,
                effects_ms: passes.effects_ms,
                throws_ms: passes.throws_ms,
                walk_ms: passes.walk_ms,
                // The attribution sweep runs after `check_units` returns, so
                // it is this phase's residue rather than one of its spans.
                report_ms: passes.report_ms
                    + (analyze_ms
                        - merge_ms
                        - passes.facts_ms
                        - passes.effects_ms
                        - passes.throws_ms
                        - passes.walk_ms
                        - passes.report_ms)
                        .max(0.0),
                persist_ms,
            },
            shared_artifacts,
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
/// of its files did — and, since issue #512, why they can still *load*: one
/// predicate, [`unmoved_rows`], answers both questions, so the load gate and
/// the name delta cannot disagree about which files moved.
fn block_index<'a>(
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
    /// (see [`generation_check`]).
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

/// Assemble and publish the candidate. Every failure is one string for the
/// caller's note — publication is a cache write, never the run's verdict.
///
/// Returns the published hex and how many packages were **shared** rather than
/// written (issue #519): an artifact whose bytes this run would have
/// reproduced exactly is taken from the published generation by reflink or
/// hard link, which costs a directory entry instead of the package's bytes and
/// its durability barrier. In the shape ADR-0092 §3 is built for that is every
/// package but the edited one.
#[allow(clippy::too_many_arguments)]
fn publish(
    store: &Store,
    id: GenerationId,
    inventories: Vec<SourceInventory>,
    plans: &[Plan],
    states: &[PkgState],
    shards: &[PackageShard],
    current: Option<&Generation>,
    fold: Fold<'_>,
    summaries: &Summaries<'_>,
    payloads: &Payloads<'_>,
) -> Result<(String, Shared), String> {
    let mut candidate = store.begin(id, inventories).map_err(|e| format!("begin: {e}"))?;
    let mut shared = Shared::default();
    for (package, ((plan, state), shard)) in plans.iter().zip(states).zip(shards).enumerate() {
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
                let builder = build_artifact(plan, shard, package, payloads);
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
    summaries.write(&mut sidecar, plans);
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
struct Fold<'a> {
    table: Option<&'a FoldTableArtifact>,
    unchanged: bool,
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
struct Summaries<'a> {
    stamp: Fingerprint,
    universe: Fingerprint,
    diag: &'a [String],
    contents: &'a [Fingerprint],
    ledger: &'a [FileWalk],
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

/// One sealed file's bytes as the text the analysis reads — the same
/// lossy-UTF-8 spelling `steins-cli`'s cold path produces (`project.rs`), and
/// the same one [`generation_check`] produced when it read through the seal.
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

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
