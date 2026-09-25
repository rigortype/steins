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
//! every `[runtime]` posture ([`RuntimePostures`]: `warning-handler`,
//! `final-keyword`, `os`), and the resolved [`ProjectLayout`] (vendor boundary + PHP target; its `Debug`
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
//! the `sources` section. So `identity_inputs` is filled once and used
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
//!
//! Published generations are swept too since issue #529 — the store holds
//! `CURRENT` and nothing else — and that one is *not* a hazard for this
//! function. Every artifact it serves from is opened here, before the publish
//! that could sweep it, and an open descriptor outlives the unlink of its
//! name; anything opened after would be a [`Miss`], which is a rebuild and the
//! same findings. The published artifacts this run adopted from are hard links
//! or clones by then, so dropping the old directory drops names, never bytes.
//!
//! **Where the run lives.** This file holds the public types, the orchestrator,
//! and the fold, analysis and report phases. Its child [`load`] holds what a
//! run reads before it analyzes (the capture, the load-or-parse, the changed
//! files and the name delta), [`publish`] what it writes after, and
//! [`identity`] what identifies it.
//!
//! [`SourceTree`]: steins_syntax::SourceTree
//! [`Miss`]: steins_gen::Miss
//! [`GenerationInputs`]: steins_gen::GenerationInputs

mod identity;
mod load;
mod publish;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use steins_db::{EffectsPolicy, PackagePartition, PluginFacts, ProjectLayout, merge_shards};
use steins_gen::{Fingerprint, Generation, SectionName, SourceDrift, SourceError, Store};
pub use steins_gen::PackageKind;

use crate::affected::{AffectedInputs, affected_files};
use crate::facts::fill_rows;
use crate::fold_persist::{FoldTableArtifact, RecordingEngine, fold_package};
use crate::project::{FileUnit, Index, LazyTree, Res};
use crate::summaries::{Summaries as StoredSummaries, read_summaries};
use crate::walk_fleet::{FolderFleet, WorkerBudget};
use crate::walk_plan::{FilePlan, FileWalk, PassTimings, UniverseVerdict, WalkControl};
use crate::{Diagnostic, Divergence, EngineFolder, ProcessEngine, RuntimePostures};

use self::identity::{RunIdentity, universe_digest};
use self::load::{Captured, Loaded, NameDelta, block_index, capture, load_or_parse, name_delta};
use self::publish::{Fold, Publishable, Summaries, publish_or_reuse};

// ---------------------------------------------------------------------------
// The orchestrator's own section: which sources an artifact was built from.
// ---------------------------------------------------------------------------

/// The section holding the package's provenance record: a JSON object with the
/// analyzer version that built the artifact and the package's source
/// fingerprint ([`SourceInventory::fingerprint`], hex). The analyzer version
/// gates the package's whole load; the fingerprint is the shortcut that says
/// *every* file is unmoved, and when it does not match the load falls to the
/// per-file gate rather than to reparsing the package.
///
/// [`SourceInventory::fingerprint`]: steins_gen::SourceInventory::fingerprint
pub const SOURCES_SECTION: &str = "sources";

fn sources_section() -> SectionName {
    SectionName::new(SOURCES_SECTION).expect("the sources section name is valid")
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
    /// The `[runtime]` postures (ADR-0037 §2). Every one of them is identity:
    /// `config_identity` destructures the value whole.
    pub postures: RuntimePostures,
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
    /// The `[effects.attribution]` keys naming no symbol (ADR-0084 §5), by
    /// the cold path's own [`crate::attribution_notices`] over this run's
    /// merged index — computed here because the gated path must not force a
    /// salsa parse just to print them.
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
    /// How many workers this run's walk fanned out over (issue #490); `1` is
    /// the sequential walk. The width is trimmed to the walk's own size, so a
    /// rebuild that walks a handful of files reports `1` on a machine that
    /// would have allowed a dozen.
    pub workers: usize,
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

/// Run one generation lifecycle: open the store once, load `CURRENT` if it
/// serves, rebuild what changed, analyze the whole universe, publish. See the
/// module docs for the reuse and degradation rules.
///
/// After the store opens, the run is seven phases, each reading what the ones
/// before it produced: the sealed capture (`capture`); the trees, loaded or
/// parsed per package (`load_or_parse`); the changed files and the name delta
/// (`name_delta`); the fold engine, whose boot surface completes the identity
/// and so the replay stamp (`fold_engine`, `RunIdentity`); the analysis, which
/// is the check pipeline with the walk plan seam open (`analyze`); the publish,
/// or the decision to keep `CURRENT` (`publish_or_reuse`); and the outcome
/// (`report`).
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
    let (captured, inventories) = capture(p)?;
    let capture_ms = ms(t_capture.elapsed());

    let t_trees = Instant::now();
    // The walk blocks the published generation carries — the replay
    // candidates, keyed by path over the whole universe (issue #519 moved them
    // out of the per-package artifacts, which had to become a function of the
    // sources alone to be shareable). Whether any of them may actually be
    // replayed is not knowable here: it needs the run's whole-universe
    // verdicts, which only exist once the analysis has computed them.
    let published: Option<StoredSummaries> = current
        .as_ref()
        .and_then(|generation| generation.summaries().ok())
        .and_then(|mut reader| read_summaries(&mut reader).ok());
    let mut loaded = load_or_parse(current.as_ref(), published.as_ref(), &captured, &mut notes);
    // Which files moved, and which persisted block each of the others could
    // replay. Computed here rather than beside the walk plan because the name
    // delta is a question about the *files* that changed (issue #510), not
    // about the packages holding them.
    let blocks =
        block_index(&captured.plans, &captured.diag, published.as_ref(), &captured.contents);
    let delta = name_delta(current.as_ref(), &captured, &loaded, &blocks, &mut notes);
    let trees_ms = ms(t_trees.elapsed());

    let mut fold = fold_engine(p, current.as_ref(), &mut notes);
    let identity = RunIdentity::read(p, &fold.folder);
    let replay = Replay {
        published: published.as_ref(),
        candidates: published.as_ref().map_or(0, |s| s.rows().count()),
        blocks,
        stamp: identity.stamp(p),
    };
    let analysis = analyze(p, &captured, &mut loaded, &mut fold.folder, delta, &replay);
    walk_notes(&analysis, replay.candidates, &mut notes);

    // Identity, honestly filled (see the module docs for in/out reasoning).
    let table = fold.folder.published_table();
    let publishable = Publishable {
        id: identity.generation_id(p, &captured.plans),
        inventories,
        captured: &captured,
        loaded: &loaded,
        fold: Fold { table: table.as_ref(), unchanged: fold.folder.table_unchanged() },
        summaries: Summaries {
            stamp: replay.stamp,
            universe: analysis.universe,
            diag: &captured.diag,
            contents: &captured.contents,
            ledger: &analysis.ledger,
        },
    };
    let t_persist = Instant::now();
    let (generation, shared_artifacts) =
        publish_or_reuse(&store, current.as_ref(), publishable, fold.degraded, &mut notes);
    let persist_ms = ms(t_persist.elapsed());

    Ok(report(captured, loaded, analysis, RunRecord {
        mode,
        generation,
        shared_artifacts,
        fold: FoldReport {
            loaded_rows: fold.loaded_rows,
            fresh_rows: fold.folder.fresh_keys().len(),
            table_published: table.is_some(),
        },
        notes,
        capture_ms,
        trees_ms,
        persist_ms,
    }))
}

/// The run's folder, and what the fold table did as the engine came up.
struct FoldSetup {
    folder: crate::RecordingFolder,
    /// Rows loaded from the published `__fold__` artifact (0 on a cold run or
    /// after an identity/whole-table miss).
    loaded_rows: usize,
    /// The published table was there and would not decode — a degradation,
    /// which republishes to repair it.
    degraded: bool,
}

/// The fold table (ADR-0092 §4): warm over the published artifact when it
/// decodes, cold otherwise — a whole-table degradation, never a partial one.
fn fold_engine(
    p: &GenerationParams<'_>,
    current: Option<&Generation>,
    notes: &mut Vec<String>,
) -> FoldSetup {
    let live = if p.php { ProcessEngine::enabled() } else { ProcessEngine::new(true) };
    let mut fold_loaded_rows = 0usize;
    let mut fold_degraded = false;
    let engine = match current.filter(|g| g.has_package(&fold_package())) {
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
    FoldSetup { folder, loaded_rows: fold_loaded_rows, degraded: fold_degraded }
}

/// What this run may replay, and on whose licence (issue #489 slice B).
struct Replay<'a> {
    /// The published generation's walk blocks, when its sidecar decoded.
    published: Option<&'a StoredSummaries>,
    /// How many blocks were on offer: zero on a cold run, which has no sidecar.
    candidates: usize,
    /// Per universe slot, the block that file could replay, or `None` for a
    /// changed file ([`block_index`]).
    blocks: Vec<Option<&'a FileWalk>>,
    /// The stamp a block's sidecar must carry ([`RunIdentity::stamp`]).
    stamp: Fingerprint,
}

/// What the analysis hands on: the findings and the notices, the walk's rows
/// for the sidecar and its report, and where the time went.
struct Analysis {
    findings: Vec<Diagnostic>,
    /// The `[effects.attribution]` keys naming no symbol, off the merged index.
    attribution_notices: Vec<String>,
    walk: WalkReport,
    /// Per file, in unit order: the block its walk or replay produced.
    ledger: Vec<FileWalk>,
    /// The whole-universe verdict digest the planner was handed.
    universe: Fingerprint,
    passes: PassTimings,
    merge_ms: f64,
    analyze_ms: f64,
}

/// The analysis proper — the same `check_units` every entry point runs,
/// over an index merged from the loaded-or-rebuilt shards (the merge is
/// partition-invariant, so this equals the cold constructions exactly).
///
/// The walk plan (issue #489 slice B) is decided here too: the planner is
/// asked once, after the run's whole-universe verdicts are computed and
/// before the first file is walked, whether each file's persisted block may
/// be replayed. The walk fans out over workers whose fold tables are folded
/// back into `folder` before this returns.
fn analyze(
    p: &GenerationParams<'_>,
    captured: &Captured,
    loaded: &mut Loaded,
    folder: &mut crate::RecordingFolder,
    delta: NameDelta,
    replay: &Replay<'_>,
) -> Analysis {
    let t_analyze = Instant::now();
    let index = Index::from_merged(merge_shards(&loaded.shards));
    let merge_ms = ms(t_analyze.elapsed());
    let diag = &captured.diag;
    let units: Vec<FileUnit<'_>> =
        diag.iter().zip(&loaded.lazy).map(|(path, tree)| FileUnit { path, tree }).collect();
    let paranoid = paranoid_enabled(p);
    // `replay.blocks` — every file's persisted block, by slot — was built with
    // the delta, so the licensing check (which needs the universe digest) is
    // all the planner has left to do. Nothing may replay at all unless there
    // is a published generation to replay from (a cold run has no candidates)
    // and the name delta could be read.
    let replay_possible = delta.known && replay.candidates > 0;
    let affected: HashSet<usize> = if replay_possible {
        affected_files(&AffectedInputs {
            facts: &loaded.facts,
            changed: delta.changed,
            delta: delta.names,
        })
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
        loaded.facts[*slot].rows = None;
        loaded.copyable[*slot] = false;
    }
    fill_rows(&mut loaded.facts, &units, &index, p.plugins, p.effects);
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
        let licensed = replay.published.is_some_and(|s| s.licensed_by(&replay.stamp, &digest));
        (0..diag.len())
            .map(|slot| match replay.blocks[slot] {
                Some(block) if licensed && !affected.contains(&slot) => {
                    FilePlan::Replay(block.clone())
                }
                _ => FilePlan::Walk,
            })
            .collect()
    };
    // The walk's fan-out (issue #490). Each worker hires a folder of its own —
    // configured *here*, at the one place a worker's folder can be born, so
    // the issue-#63 hazard of a folder carrying some other run's `php_target`
    // cannot recur — over the transport this run already established: the same
    // child, the same loaded table, the same pool of what the run has asked.
    // Per-worker folders and one shared transport is the whole shape, and each
    // half of it is measured (see `RecordingEngine::live`).
    let shared = folder.shared_engine();
    let worker_target = p.layout.php_target().cloned();
    let hire = move || {
        let mut worker = EngineFolder::with_engine(RecordingEngine::worker(shared.clone()));
        worker.set_php_target(worker_target.clone());
        worker
    };
    let retire = |worker: crate::RecordingFolder| worker.harvest();
    let fleet = FolderFleet::new(WorkerBudget::read(), &hire, &retire);
    let mut control = WalkControl::new(&mut planner, paranoid, &loaded.facts, Some(&fleet));
    let findings = crate::check_units_controlled(
        &units,
        &index,
        folder,
        p.postures,
        p.layout,
        p.plugins,
        p.effects,
        Some(&mut control),
    );
    drop(units);
    // Off the merged index: the gated path must not force a salsa parse just to
    // print them.
    let attribution_notices = crate::attribution_notices(p.effects, |name| {
        !matches!(index.resolve_class(name), Res::Absent)
            || !matches!(index.resolve_function(name), Res::Absent)
    });
    let analyze_ms = ms(t_analyze.elapsed());
    let walk = WalkReport {
        walked: control.walked,
        replayed: control.replayed,
        would_skip: control.would_skip,
        workers: control.workers,
        paranoid,
        divergences: std::mem::take(&mut control.divergences),
        divergence_count: control.divergence_count,
    };
    let passes = control.passes;
    let ledger = std::mem::take(&mut control.ledger);
    // The control's borrow of the planner — and so the planner's of the two
    // values it writes — ends here; everything either produced is owned above.
    drop(control);
    // The fan-out's tables, folded back into the run's one (ADR-0092 §4). In
    // chunk order, so the merge is the same every run whatever the scheduler
    // did; a sequential walk harvests nothing and this is a no-op.
    for harvest in fleet.into_harvests() {
        folder.absorb_worker(harvest);
    }
    let universe = universe.expect("the planner runs before the first file is walked");
    Analysis { findings, attribution_notices, walk, ledger, universe, passes, merge_ms, analyze_ms }
}

/// The walk's own notes: every paranoid divergence, the replay count whenever
/// anything was on offer, and under the verifier the universe verdict.
fn walk_notes(analysis: &Analysis, replay_candidates: usize, notes: &mut Vec<String>) {
    let walk = &analysis.walk;
    for divergence in &walk.divergences {
        notes.push(format!("PARANOID DIVERGENCE {divergence}"));
    }
    if replay_candidates > 0 {
        notes.push(format!(
            "{} file(s) replayed a persisted walk block, {} walked ({replay_candidates} block(s) were on offer)",
            walk.replayed, walk.walked
        ));
    }
    if walk.paranoid {
        // Under the verifier, say which universe verdict this run computed:
        // over a corpus tree, two runs whose digests differ walked everything
        // for that reason, and the auditor should see that rather than infer
        // it from a would-skip count of zero.
        notes.push(format!(
            "paranoid: {} file(s) walked, {} would have been skipped, {} divergence(s); universe verdict {}",
            walk.walked,
            walk.would_skip,
            walk.divergence_count,
            analysis.universe.to_hex(),
        ));
    }
}

/// What the orchestrator itself decided and timed, for [`report`] to file
/// beside what the phases produced: the temperature the run started at, what
/// it published, what the fold table did, the notes, and the three spans the
/// analysis does not time for itself.
struct RunRecord {
    mode: GenerationMode,
    generation: Option<String>,
    shared_artifacts: usize,
    fold: FoldReport,
    notes: Vec<String>,
    capture_ms: f64,
    trees_ms: f64,
    persist_ms: f64,
}

/// The outcome: the findings, the texts and tree handles the caller's
/// downstream pipeline reads, and the run's ledger.
fn report(
    captured: Captured,
    loaded: Loaded,
    analysis: Analysis,
    run: RunRecord,
) -> GenerationOutcome {
    let Captured { diag, plans, texts, .. } = captured;
    let lazy = loaded.lazy;
    let packages = plans
        .iter()
        .zip(&loaded.states)
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
    let Analysis { findings, attribution_notices, walk, passes, merge_ms, analyze_ms, .. } =
        analysis;
    GenerationOutcome {
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
            mode: run.mode,
            generation: run.generation,
            packages,
            fold: run.fold,
            walk,
            timings: PhaseTimings {
                capture_ms: run.capture_ms,
                trees_ms: run.trees_ms,
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
                persist_ms: run.persist_ms,
            },
            shared_artifacts: run.shared_artifacts,
            notes: run.notes,
        },
    }
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
