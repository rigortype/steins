//! `fp-gate`: run the full proof-layer pipeline over the pinned corpus.
//!
//! ADR-0013: any proof-layer diagnostic on clean-parsing code is a release
//! blocker, so the gate exits nonzero the moment one fires.
//!
//! Whole-project mode (ADR-0009/0015): each corpus package is analyzed as ONE
//! project (one salsa DB over all its `.php` files) so cross-file resolution
//! works; packages run in parallel (rayon). Parse-error files stay in the
//! project (a partial tree can only silence, never add a false positive), but
//! their own diagnostics are excluded from the gate count.
//!
//! # The gate runs the path the product runs (issue #525)
//!
//! Until the frozen-generation lifecycle became the default for `steins check`,
//! this gate called `check_project` — the library entry — and so had never once
//! run through the orchestrator the CLI actually uses. That was defensible
//! while the lifecycle was opt-in and indefensible the moment it was not: a
//! zero-false-positive release gate guarding a code path the product no longer
//! takes is guarding nothing.
//!
//! So every project here is analyzed **twice through
//! `steins_infer::generation_check`**, into a scratch store under `target/`
//! that is wiped at the start of the run:
//!
//! 1. a **cold** pass — an empty store, every file parsed, one generation
//!    published — whose findings are the gate's counts, and
//! 2. a **warm** pass over that published generation, whose findings must
//!    equal the cold pass's exactly.
//!
//! The second pass is ADR-0092 §5's warm ≡ cold oracle promoted from a fixture
//! property to a release gate, over the whole corpus rather than over whatever
//! a fixture happens to exercise. A disagreement is RED and is a soundness bug
//! (the cache changed a finding), not a cost regression. An orchestration
//! *failure* is RED too: the product degrades to cold silently and correctly,
//! but a gate that accepted the degradation would be back to grading the path
//! nobody runs.
//!
//! The store never lands in a corpus checkout (`corpus/` is a cached directory
//! in CI, and a `.steins/` inside it would make the next run's "cold" pass a
//! lie), and the corpus tree is only ever read. The scratch stores are wiped
//! before the first pass and again after the last — the first because "cold"
//! has to mean cold, the second because 131 MB over the pinned corpus is 131 MB
//! CI would otherwise archive with `target/`.
//!
//! What this gate deliberately does NOT re-assert is `generation_check` ≡
//! `check_project`: `cargo xtask perf --warm` already pins those two against
//! one findings hash over a corpus target, and duplicating it here would buy a
//! third full analysis per package for a property that already has an owner.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use steins_db::composer;
use steins_db::{EffectsPolicy, PluginFacts, ProjectLayout};
use steins_infer::{
    Diagnostic, Floor, GenerationMode, GenerationParams, Layer, RuntimePostures, generation_check,
    layer, surface_floor,
};

use crate::corpus::{PACKAGES, checkout_dir, collect_php_files, read_lock, repo_root};
use crate::corpus_local::{self, LocalProject};

/// Per-project result of the gate run (a pinned corpus package or an unpinned
/// local project). `diagnostics` holds only findings that count against the
/// gate; local-project vendor findings are excluded (ADR-0015) and tallied in
/// `vendor_suppressed`.
struct PackageReport {
    name: String,
    /// The pinned release tag, or empty for a local (unpinned) project.
    tag: String,
    /// A live working tree injected via `corpus.local.toml` (ADR-0013 §4).
    local: bool,
    file_count: usize,
    parse_error_files: Vec<String>,
    diagnostics: Vec<Diagnostic>,
    /// `phpdoc.*` contract findings (ADR-0030 relation #1): measurement mode,
    /// counted per package, excluded from red/green.
    phpdoc: Vec<Diagnostic>,
    /// `throw.*` findings (ADR-0040/0007), same measurement mode: TRUE
    /// findings saturate working code (ADR-0007), so only a per-package
    /// increase reds.
    throws: Vec<Diagnostic>,
    /// `effect.*` contract findings (ADR-0050 §9): read purity tags, Steins's
    /// own or upstream's (`@phpstan-all-methods-pure`/`@phpstan-impure`,
    /// issues #303/#311) — how `EFFECT_EXPECTED`'s first row seeded on code
    /// with no Steins annotation. `effect.unknown-label` is mechanics, stays
    /// red-on-sight in `diagnostics`.
    effects: Vec<Diagnostic>,
    /// Possibly-grade proof findings — `strict`-floored ids (ADR-0081 §8) —
    /// counted against `POSSIBLY_EXPECTED`. Definite siblings
    /// (`variable.undefined`, `property.undefined`, `type.return-missing`)
    /// stay red-on-sight in `diagnostics`.
    possibly: Vec<Diagnostic>,
    /// Triaged TRUE runtime-layer positives (see `EXPECTED_PROOF_FINDINGS`),
    /// matched at finding precision so any drift falls back into
    /// `diagnostics` and reds the gate.
    expected_true: Vec<Diagnostic>,
    /// Vendor findings suppressed from the gate count (local projects only).
    vendor_suppressed: usize,
    /// The revision recorded in `corpus.local.toml`. `None` for pinned
    /// packages (revision lives in `corpus.lock.toml`) or an unrecorded local
    /// entry.
    recorded_revision: Option<String>,
    /// The revision the local checkout is actually on, or `None` if
    /// unreadable (see [`corpus_local::checkout_revision`]).
    measured_revision: Option<String>,
    /// Whether the checkout carries uncommitted/untracked content (see
    /// [`WorktreeState`]).
    worktree: WorktreeState,
    /// The cold generation build — the pass whose findings are counted above.
    elapsed: Duration,
    /// The warm re-check over the generation the cold pass published: the
    /// second-pass cost the module docs price, and zero only when the run
    /// never got that far.
    warm_elapsed: Duration,
    /// Whether the two passes agreed (issue #525).
    parity: Parity,
}

/// What the cold pass and the warm re-check made of each other.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Parity {
    /// The warm pass found exactly what the cold pass found. The only verdict
    /// that keeps the gate green.
    Agreed,
    /// The two passes disagreed — a cache that changed a finding, which is the
    /// soundness bug this gate exists to catch, not a cost regression.
    Diverged(String),
    /// The orchestrator could not run at all. The shipped CLI degrades to cold
    /// here, quietly and correctly; the gate must not, or it grades a path
    /// nobody takes.
    Failed(String),
}

impl Parity {
    fn is_green(&self) -> bool { matches!(self, Parity::Agreed) }

    /// The one-line verdict for the report, or `None` when there is nothing to
    /// say.
    fn note(&self) -> Option<&str> {
        match self {
            Parity::Agreed => None,
            Parity::Diverged(detail) | Parity::Failed(detail) => Some(detail),
        }
    }
}

impl PackageReport {
    /// How this report's recorded baseline revision relates to the one measured.
    fn revision(&self) -> RevisionStatus {
        classify_revision(
            self.recorded_revision.as_deref(),
            self.measured_revision.as_deref(),
            self.worktree,
        )
    }
}

/// Whether a local project's working tree carries anything on top of the
/// revision it reports. Only a clean match is good evidence about the files,
/// not just the commit — a private corpus is normally a dirty checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeState {
    /// `git status --porcelain` was empty.
    Clean,
    /// Non-empty (modified/staged/untracked) — a filesystem walk, so
    /// untracked counts as dirty.
    Dirty,
    /// Undeterminable (no git, spawn failure, non-zero exit) — unknown,
    /// never assumed clean.
    Unknown,
}

impl WorktreeState {
    /// Map the tri-state `Option<bool>` [`corpus_local::checkout_is_dirty`] returns.
    fn from_dirty(dirty: Option<bool>) -> Self {
        match dirty {
            Some(true) => Self::Dirty,
            Some(false) => Self::Clean,
            None => Self::Unknown,
        }
    }
}

/// How a local project's recorded baseline revision relates to its current
/// one. Pinned packages are reproducible by construction (`corpus.lock.toml`);
/// `revision` in `corpus.local.toml` exists to collapse the
/// analyzer-vs-corpus-drift ambiguity for local (unpinned) projects.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RevisionStatus {
    /// A revision is recorded and the checkout is on it. Only a CLEAN
    /// `worktree` makes the measured files identical to the seeded ones.
    Matches { revision: String, worktree: WorktreeState },
    /// A revision is recorded and the checkout is somewhere else: the corpus
    /// moved under the baseline.
    Differs { recorded: String, measured: String },
    /// No revision is recorded. `measured` is what the checkout is on now (or
    /// `None` if even that is unknown) — printed so a human can record it.
    Unrecorded { measured: Option<String> },
    /// A revision is recorded but the checkout's own revision could not be read,
    /// so no comparison was possible.
    Unreadable { recorded: String },
}

/// Classify a recorded revision against a measured one. Case-insensitive and
/// abbreviation-tolerant (either may prefix the other if the shorter is at
/// least [`MIN_REVISION_PREFIX`] chars; shorter than that is a difference).
/// `worktree` is carried onto a match only — an already-inconclusive verdict
/// stays inconclusive regardless of cleanliness.
fn classify_revision(
    recorded: Option<&str>,
    measured: Option<&str>,
    worktree: WorktreeState,
) -> RevisionStatus {
    let norm = |s: &str| s.trim().to_ascii_lowercase();
    match (recorded.map(&norm).filter(|s| !s.is_empty()), measured.map(&norm)) {
        (Some(recorded), Some(measured)) => {
            if revisions_agree(&recorded, &measured) {
                // Report the longer (more specific) of the two — normally the
                // measured full sha.
                let revision =
                    if measured.len() >= recorded.len() { measured } else { recorded };
                RevisionStatus::Matches { revision, worktree }
            } else {
                RevisionStatus::Differs { recorded, measured }
            }
        }
        (Some(recorded), None) => RevisionStatus::Unreadable { recorded },
        (None, measured) => RevisionStatus::Unrecorded { measured },
    }
}

/// Shortest abbreviated sha accepted as evidence of identity (git's own default
/// abbreviation floor).
const MIN_REVISION_PREFIX: usize = 7;

/// Whether two (already normalized) revision strings name the same commit, allowing
/// either to be an abbreviation of the other.
fn revisions_agree(a: &str, b: &str) -> bool {
    let shorter = a.len().min(b.len());
    shorter >= MIN_REVISION_PREFIX && (a.starts_with(b) || b.starts_with(a))
}

/// The line printed for a local project on every run, not only when a
/// tripwire trips — so the previous run's output already records the corpus
/// state before the day it moves.
fn revision_summary_line(status: &RevisionStatus) -> String {
    match status {
        RevisionStatus::Matches { revision, worktree: WorktreeState::Clean } => format!(
            "revision: {revision} — matches the revision the baselines were seeded at, working tree clean"
        ),
        RevisionStatus::Matches { revision, worktree: WorktreeState::Dirty } => format!(
            "revision: {revision} — matches the revision the baselines were seeded at, but the working tree is DIRTY (uncommitted or untracked content sits on top, so the files measured are not exactly that revision)"
        ),
        RevisionStatus::Matches { revision, worktree: WorktreeState::Unknown } => format!(
            "revision: {revision} — matches the revision the baselines were seeded at; whether the working tree is clean could not be determined"
        ),
        RevisionStatus::Differs { recorded, measured } => format!(
            "revision: {measured} — but the baselines were seeded at {recorded}; the corpus has moved since"
        ),
        RevisionStatus::Unrecorded { measured: Some(measured) } => format!(
            "revision: {measured} — UNPINNED baseline (no `revision` recorded in corpus.local.toml; add `revision = \"{measured}\"` to this project's entry to pin it)"
        ),
        RevisionStatus::Unrecorded { measured: None } => {
            "revision: unknown — not a git checkout, or git is unavailable; the baseline cannot be pinned".to_owned()
        }
        RevisionStatus::Unreadable { recorded } => format!(
            "revision: unknown — not a git checkout, or git is unavailable; the baselines were seeded at {recorded}, which nothing here can compare against"
        ),
    }
}

/// The line printed **beside a tripped tripwire** for a local project: the one
/// place where the recorded-vs-measured comparison actually decides what the
/// operator should do about the count that just went up.
fn revision_tripwire_line(status: &RevisionStatus) -> String {
    match status {
        RevisionStatus::Matches { revision, worktree: WorktreeState::Clean } => format!(
            "revision MATCHES the seeded baseline ({revision}) and the working tree is CLEAN: the files just measured are the same ones the baseline was measured on, so this increase is a GENUINE REGRESSION — triage the new finding(s), do not reseed."
        ),
        RevisionStatus::Matches { revision, worktree: WorktreeState::Dirty } => format!(
            "revision matches the seeded baseline ({revision}) BUT the working tree is DIRTY: uncommitted or untracked content sits on top of that commit, so the files just measured are NOT exactly the revision the baseline was seeded at and this increase may still be corpus-side. Check `git status` in the corpus checkout — a clean tree at this revision would make it a genuine regression."
        ),
        RevisionStatus::Matches { revision, worktree: WorktreeState::Unknown } => format!(
            "revision matches the seeded baseline ({revision}), but whether the working tree is clean could not be determined (not a git checkout, or git is unavailable): the recorded commit agrees while the measured FILES are unverified, so a regression cannot be asserted on the revision alone."
        ),
        RevisionStatus::Differs { recorded, measured } => format!(
            "revision DIFFERS: the baseline was seeded at {recorded}, this run measured {measured}. The corpus moved under the baseline, so the count change may be CORPUS DRIFT rather than a regression — re-measure against the seeded revision to separate the two, then reseed consciously (the count in xtask/fp-gate/, and `revision = \"{measured}\"` in corpus.local.toml)."
        ),
        RevisionStatus::Unrecorded { measured: Some(measured) } => format!(
            "revision UNPINNED: no `revision` is recorded for this project, so drift and regression CANNOT be told apart automatically. Record the measured revision so the next run can: `revision = \"{measured}\"`"
        ),
        RevisionStatus::Unrecorded { measured: None } => {
            "revision UNPINNED and unreadable: no `revision` is recorded and the checkout's own revision could not be read (not a git checkout, or git is unavailable), so drift and regression CANNOT be told apart automatically.".to_owned()
        }
        RevisionStatus::Unreadable { recorded } => format!(
            "revision UNCOMPARED: the baseline was seeded at {recorded}, but this checkout's revision could not be read (not a git checkout, or git is unavailable), so corpus drift cannot be ruled out."
        ),
    }
}

/// Which counter partition a finding routes into (ADR-0050 §9 / ADR-0053 §8),
/// keyed off the finding's **layer** (steins-infer registry). Exhaustive on
/// [`Layer`] so a new variant is a compile error here until its gate posture
/// is stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateBucket {
    /// proof + mechanics (and any unregistered id): red on sight (ADR-0013).
    RedOnSight,
    /// contract: measurement mode — counted, gates only on a per-package
    /// increase past the seeded baseline (ADR-0050 §9).
    Measurement,
    /// possibly-grade proof ids — `maybe-` siblings floored at `strict`
    /// (ADR-0081 §8). Same increase tripwire as contract: "some path" is the
    /// id's own yield, not a corpus defect — the zero-FP tolerance for these
    /// ids is absorbed via the `strict` opt-in, not suppression.
    Tripwire,
    /// debug (ADR-0053 §8): requested introspection, excluded from every
    /// counter — a dump is not a finding. Vacuous today (no emitter until
    /// ADR-0053 D3/D4).
    Excluded,
}

/// Route a finding's id to its [`GateBucket`] by layer. Unregistered ids are
/// conservatively red-on-sight.
fn gate_bucket(id: &str) -> GateBucket {
    match layer(id) {
        Some(Layer::Contract) => GateBucket::Measurement,
        Some(Layer::Debug) => GateBucket::Excluded,
        // Possibly-grade (ADR-0078 §1.3's `maybe-` convention): derived from the
        // registry so a new sibling takes the right posture on registration.
        Some(Layer::Proof) if surface_floor(id) == Some(Floor::Strict) => {
            GateBucket::Tripwire
        }
        Some(Layer::Proof | Layer::Mechanics) | None => GateBucket::RedOnSight,
    }
}

/// Whether a diagnostic is contract-layer (ADR-0050 §9): measurement-mode
/// partitioning, via [`gate_bucket`].
fn is_contract(d: &Diagnostic) -> bool {
    gate_bucket(d.id) == GateBucket::Measurement
}

/// Whether a diagnostic is debug-layer (ADR-0053 §8): excluded from every gate
/// counter.
fn is_debug(d: &Diagnostic) -> bool {
    gate_bucket(d.id) == GateBucket::Excluded
}

/// Whether a diagnostic is a possibly-grade proof finding (ADR-0081 §8), the
/// `strict`-floored proof ids counted against `POSSIBLY_EXPECTED`. One id
/// table, not one per family — the three ids share a posture, not a prefix.
fn is_possibly(d: &Diagnostic) -> bool {
    gate_bucket(d.id) == GateBucket::Tripwire
}

/// Whether a diagnostic is a measurement-mode `phpdoc.*` contract id. Selected
/// by prefix AND layer (the `is_effect_contract` shape): since ADR-0078 §1.5 /
/// issue #186, `phpdoc.*` also carries docblock-hygiene mechanics ids, and a
/// bare prefix test would double-count one.
fn is_phpdoc(d: &Diagnostic) -> bool {
    d.id.starts_with("phpdoc.") && is_contract(d)
}

/// Whether a diagnostic is a measurement-mode `throw.*` contract id
/// (ADR-0040) — the prefix keys its own count table (all `throw.*` are contract).
fn is_throw(d: &Diagnostic) -> bool {
    d.id.starts_with("throw.")
}

/// Whether a diagnostic is an `effect.*` contract id (`effect.envelope-exceeded`
/// / `effect.liskov-widened`), the ADR-0050 §9 delta family. Selected by layer
/// AND prefix so `effect.unknown-label` (mechanics) stays red-on-sight.
fn is_effect_contract(d: &Diagnostic) -> bool {
    d.id.starts_with("effect.") && is_contract(d)
}

// untyped surface (ADR-0078, issue #200): `untyped.*` is a contract-layer id
// with no family table, so `!is_contract` drops it from `diagnostics` (never
// red) but nothing counts or reports it either — no tripwire, deliberately (a
// tripwire seeded pre-measurement would pin an arbitrary number). Adding an
// `UNTYPED_EXPECTED` table beside the other three turns it into one, once the
// `iterable-value` / `generics` floors are decided.

/// The seeded baselines the gate measures against (issue #775): four
/// per-package count tables and the pinned proof-layer findings. They are data
/// under `xtask/fp-gate/`, one TOML file per table, built into the binary with
/// `include_str!` — so the gate reads no file at run time, and a malformed
/// table stops it before any analysis runs.
///
/// Each table keeps the name it had as a Rust constant, which is also its file
/// stem (`PHPDOC_EXPECTED` is `phpdoc_expected.toml`), so the triage notes,
/// which cite one another by those names, still resolve. A row's note is the
/// comment block directly above it rather than a field: nothing reads a note
/// but the person moving the count, and a comment keeps each ledger's wrapping
/// and column alignment byte for byte, so a reseed stays a one-line count
/// change plus the lines that say why. The table's policy is the file's header.
struct Baselines {
    /// `PHPDOC_EXPECTED`: the `phpdoc.*` contract ids ([`is_phpdoc`], ADR-0030
    /// relation #1).
    phpdoc: CountTable,
    /// `THROW_EXPECTED`: the `throw.*` ids ([`is_throw`], ADR-0040/0007).
    throw: CountTable,
    /// `EFFECT_EXPECTED`: the `effect.*` contract ids ([`is_effect_contract`],
    /// ADR-0050 §9).
    effect: CountTable,
    /// `POSSIBLY_EXPECTED`: the possibly-grade proof ids ([`is_possibly`],
    /// ADR-0081 §8).
    possibly: CountTable,
    /// `EXPECTED_PROOF_FINDINGS`: triaged TRUE proof-layer positives (ADR-0043
    /// §5), matched by [`is_expected_true_positive`].
    proof: Vec<ExpectedProofFinding>,
}

/// Where the tables live, for the messages that name one.
const BASELINE_DIR: &str = "xtask/fp-gate";

impl Baselines {
    /// Parse the built-in tables. An error names the file it is in.
    fn load() -> Result<Self, String> {
        // One literal names both the file embedded and the file an error
        // blames, so the two cannot drift apart.
        macro_rules! table {
            ($parse:ident, $file:literal) => {
                $parse($file, include_str!(concat!("../fp-gate/", $file)))
            };
        }
        Ok(Self {
            phpdoc: table!(parse_table, "phpdoc_expected.toml")?,
            throw: table!(parse_table, "throw_expected.toml")?,
            effect: table!(parse_table, "effect_expected.toml")?,
            possibly: table!(parse_table, "possibly_expected.toml")?,
            proof: table!(parse_pins, "expected_proof_findings.toml")?,
        })
    }
}

/// A measurement-mode family's per-package **increase** tripwire table: a
/// package or local-project name and the count it is expected not to exceed.
/// A name with no row expects zero. TOML refuses a repeated key, so a name has
/// at most one row — the property a lookup's single answer rests on.
#[derive(Debug, serde::Deserialize)]
#[serde(transparent)]
struct CountTable(BTreeMap<String, usize>);

impl CountTable {
    /// The expected count for a package or local-project name (0 if untabled).
    fn expected(&self, name: &str) -> usize { self.0.get(name).copied().unwrap_or(0) }

    /// The whole table's baseline, printed beside the measured total.
    fn total(&self) -> usize { self.0.values().sum() }

    fn is_empty(&self) -> bool { self.0.is_empty() }
}

/// Parse one count table, naming its file on failure.
fn parse_table(file: &str, text: &str) -> Result<CountTable, String> {
    toml::from_str(text).map_err(|e| format!("{BASELINE_DIR}/{file}: {e}"))
}

/// A triaged TRUE proof-layer positive the corpus legitimately contains: real
/// broken code Steins correctly proves. Unlike measurement-mode `phpdoc.*`/
/// `throw.*`, this is runtime-layer (standing bar: zero, ADR-0013), so an
/// entry is a recorded exception matched at finding precision (package + id +
/// path + line + message fingerprint) — any drift re-reds the gate.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedProofFinding {
    /// Package / local-project name the finding belongs to.
    package: String,
    /// The diagnostic id (e.g. `type.argument-mismatch`).
    id: String,
    /// A suffix of the finding's project-relative path.
    path_suffix: String,
    /// The 1-based line.
    line: u32,
    /// A stable substring of the message (the acceptance fingerprint).
    message_contains: String,
}

/// `expected_proof_findings.toml`'s shape: one `[[finding]]` per pin.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PinFile {
    #[serde(default)]
    finding: Vec<ExpectedProofFinding>,
}

/// Parse the pinned findings and hold them to what matching at finding
/// precision needs: a path suffix, a line and a message fingerprint that can
/// each tell findings apart (an empty one matches every finding), and one row
/// per finding — a second row under the same package, id, path and line is a
/// pin nobody can tell from the first.
fn parse_pins(file: &str, text: &str) -> Result<Vec<ExpectedProofFinding>, String> {
    let pins = toml::from_str::<PinFile>(text)
        .map_err(|e| format!("{BASELINE_DIR}/{file}: {e}"))?
        .finding;
    let mut seen = HashSet::new();
    for p in &pins {
        let at = format!("{}:{} [{}] in {}", p.path_suffix, p.line, p.id, p.package);
        if p.path_suffix.is_empty() || p.line == 0 || p.message_contains.is_empty() {
            let why = "needs a path suffix, a line and a message fingerprint";
            return Err(format!("{BASELINE_DIR}/{file}: the pin at {at} {why}"));
        }
        if !seen.insert((&p.package, &p.id, &p.path_suffix, p.line)) {
            return Err(format!("{BASELINE_DIR}/{file}: {at} is pinned twice"));
        }
    }
    Ok(pins)
}

/// Whether `d` is a recorded, triaged TRUE proof-layer positive for `package`
/// (see `EXPECTED_PROOF_FINDINGS`) — reported but excluded from the red/green
/// verdict. Matched at finding precision so any drift re-reds the gate.
fn is_expected_true_positive(pins: &[ExpectedProofFinding], package: &str, d: &Diagnostic) -> bool {
    pins.iter().any(|e| {
        e.package == package
            && e.id == d.id
            && e.line == d.line
            && d.path.ends_with(e.path_suffix.as_str())
            && d.message.contains(e.message_contains.as_str())
    })
}

/// Entry point for `cargo xtask fp-gate`. Returns `true` if the gate is GREEN
/// (no diagnostics on clean code).
pub fn run() -> Result<bool, String> {
    // The baselines first: a malformed table stops the gate before it analyzes
    // anything.
    let baselines = Baselines::load()?;
    let lock = read_lock();
    if lock.packages.is_empty() {
        return Err("corpus.lock.toml is empty — run `cargo xtask corpus-sync` first".to_owned());
    }
    let root = repo_root();

    // Wipe every scratch store before the first pass: "cold" has to mean cold,
    // and a store left by a previous run (or restored from a CI cache) would
    // quietly make the gate measure a generation some other commit's analyzer
    // built.
    let stores = root.join("target").join("fp-gate-stores");
    if let Err(e) = std::fs::remove_dir_all(&stores)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(format!("cannot clear {} for a cold first pass: {e}", stores.display()));
    }

    // One project per package; packages analyzed in parallel.
    let reports: Result<Vec<PackageReport>, String> = PACKAGES
        .par_iter()
        .map(|pkg| {
            let dir = checkout_dir(pkg.name);
            if !dir.is_dir() {
                return Err(format!(
                    "{} not checked out at {} — run `cargo xtask corpus-sync`",
                    pkg.name,
                    dir.display()
                ));
            }
            let tag = lock.get(pkg.name).map(|e| e.tag.clone()).unwrap_or_default();
            Ok(analyze_package(pkg.name, &tag, &dir, &root, &baselines.proof))
        })
        .collect();
    let mut reports = reports?;
    // Keep a stable (canonical corpus) order for the report.
    reports.sort_by_key(|r| PACKAGES.iter().position(|p| p.name == r.name).unwrap_or(usize::MAX));

    // Private-corpus injection point (ADR-0013 §4): each `[[project]]` in the
    // optional (gitignored) `corpus.local.toml` is analyzed like a package;
    // vendor files are indexed but their findings don't count.
    let locals = corpus_local::read_local()?;
    let mut local_reports: Vec<PackageReport> =
        locals.par_iter().map(|p| analyze_local(p, &baselines.proof)).collect();
    local_reports.sort_by(|a, b| a.name.cmp(&b.name));

    // Measurement-mode regression tripwires (see `PHPDOC_EXPECTED` /
    // `THROW_EXPECTED`): a package regresses iff its count exceeds the baseline.
    let regressions = measurement_regressions(
        &reports,
        &local_reports,
        "phpdoc",
        |r| r.phpdoc.len(),
        &baselines.phpdoc,
    );
    let throw_regressions = measurement_regressions(
        &reports,
        &local_reports,
        "throw",
        |r| r.throws.len(),
        &baselines.throw,
    );
    // ADR-0050 §9 delta family: `effect.*`-contract findings gate as an increase
    // tripwire too, same shape as `phpdoc.*`/`throw.*`.
    let effect_regressions = measurement_regressions(
        &reports,
        &local_reports,
        "effect",
        |r| r.effects.len(),
        &baselines.effect,
    );
    // ADR-0081 §8: the possibly-grade proof ids gate as an increase tripwire too.
    let possibly_regressions = measurement_regressions(
        &reports,
        &local_reports,
        "possibly",
        |r| r.possibly.len(),
        &baselines.possibly,
    );

    print_report(
        &baselines,
        &reports,
        &local_reports,
        &regressions,
        &throw_regressions,
        &effect_regressions,
        &possibly_regressions,
    );

    // RED on any proof-layer finding (package + local non-vendor diagnostics;
    // vendor never gates, ADR-0015) OR any measurement-mode regression OR any
    // project whose warm re-check did not reproduce its cold pass (issue #525).
    let total_diags: usize = reports.iter().map(|r| r.diagnostics.len()).sum::<usize>()
        + local_reports.iter().map(|r| r.diagnostics.len()).sum::<usize>();
    let parity_ok = reports.iter().chain(local_reports.iter()).all(|r| r.parity.is_green());

    // Take the stores back off disk. The next run wipes them anyway, so nothing
    // depends on this — but 131 MB over the pinned corpus is 131 MB CI would
    // otherwise hand to `Swatinem/rust-cache`, which archives `target/`, on
    // every job that shares this one's cache key. Best-effort: a failure here
    // is disk, not a verdict.
    let _ = std::fs::remove_dir_all(&stores);

    Ok(total_diags == 0
        && parity_ok
        && regressions.is_empty()
        && throw_regressions.is_empty()
        && effect_regressions.is_empty()
        && possibly_regressions.is_empty())
}

/// One measurement-mode regression: a package whose count exceeds its expectation.
struct PhpdocRegression {
    name: String,
    actual: usize,
    expected: usize,
}

/// Generic measurement-mode tripwire: report packages whose `count` exceeds their
/// `expected` baseline (the only direction that gates red).
fn measurement_regressions(
    reports: &[PackageReport],
    local_reports: &[PackageReport],
    _family: &str,
    count: impl Fn(&PackageReport) -> usize,
    expected: &CountTable,
) -> Vec<PhpdocRegression> {
    reports
        .iter()
        .chain(local_reports.iter())
        .filter_map(|r| {
            let actual = count(r);
            let exp = expected.expected(&r.name);
            (actual > exp).then(|| PhpdocRegression { name: r.name.clone(), actual, expected: exp })
        })
        .collect()
}

/// Where this run's generation stores live: one directory per project, under
/// `target/` and never inside a corpus checkout.
///
/// Two reasons it is not the analyzed tree, and both bite in CI. `corpus/` is
/// restored from an `actions/cache` keyed on `corpus.lock.toml`, so a
/// `.steins/` written into it would survive into the *next* PR's run and make
/// that run's "cold" pass a warm one — the gate would then be measuring a
/// generation built by some other commit's analyzer. And the corpus is
/// checked-out third-party source: the gate reads it, full stop.
fn store_root(root: &Path, project: &str) -> PathBuf {
    let slug: String = project
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    root.join("target").join("fp-gate-stores").join(slug)
}

/// What one project's two passes produced.
struct GenerationOutcome {
    /// The cold pass's findings — the gate's counts.
    diagnostics: Vec<Diagnostic>,
    /// Files whose tree carries a parse error, read off the cold pass's own
    /// trees (they are all in hand there, so this costs no second parse).
    parse_error_files: Vec<String>,
    cold: Duration,
    warm: Duration,
    parity: Parity,
}

/// Analyze one project the way the shipped `steins check` analyzes one
/// (ADR-0092 §5, issue #525): a cold generation build into a wiped scratch
/// store, then a warm re-check over what it published, then the equality.
///
/// `files` are spelled exactly as their diagnostic paths will read, and
/// `capture_root` is the directory those spellings resolve against — the same
/// pairing the CLI derives in `steins_cli::generation`.
///
/// The PHP target is NOT set here, and that is not an omission: the
/// orchestrator reads it off the layout it is handed
/// (`folder.set_php_target(p.layout.php_target())`) and builds its own engine
/// per run, so the resident-folder hazard this function replaced cannot recur.
/// It is worth naming what that hazard was, because it was expensive: a
/// `SidecarFolder` reused across rayon-scheduled projects kept the previous
/// project's `php_target`, which gates ADR-0056 curated-fact admission and the
/// absence family, and issue #63 was two sessions of triage into a local
/// `phpdoc.*` count that swung 536↔483 run to run (invisible under
/// `RAYON_NUM_THREADS=1`) before that was found to be the cause.
fn analyze_through_generations(
    project_name: &str,
    files: &[PathBuf],
    capture_root: &Path,
    layout: &ProjectLayout,
    root: &Path,
) -> GenerationOutcome {
    let failed = |detail: String, cold: Duration, warm: Duration| GenerationOutcome {
        diagnostics: Vec::new(),
        parse_error_files: Vec::new(),
        cold,
        warm,
        parity: Parity::Failed(detail),
    };

    let store = store_root(root, project_name);
    let partition = steins_db::partition::discover(layout);
    let plugins = PluginFacts::discover(layout, None);
    // `check_project`'s own defaults, which is what the gate has always
    // measured under: no `steins.toml` governs a corpus checkout.
    let effects = EffectsPolicy::none();
    let params = GenerationParams {
        store_root: &store,
        capture_root,
        files,
        layout,
        partition: &partition,
        plugins: &plugins,
        effects: &effects,
        postures: RuntimePostures::default(),
        php: true,
        // The paranoid walk verifier stays environment-driven
        // (`STEINS_GENERATIONS_PARANOID=1`) — it walks every file and would
        // more than double the gate on every PR.
        paranoid: false,
    };

    let t = Instant::now();
    let cold = match generation_check(&params) {
        Ok(outcome) => outcome,
        Err(e) => return failed(format!("cold generation build failed: {e}"), t.elapsed(), Duration::ZERO),
    };
    let cold_elapsed = t.elapsed();
    if cold.report.mode != GenerationMode::Cold {
        return failed(
            "the scratch store already held a generation — the cold pass was not cold".to_owned(),
            cold_elapsed,
            Duration::ZERO,
        );
    }
    // Every tree is in hand on a cold pass (nothing was loaded from an
    // artifact), so this reads them rather than decoding them.
    let parse_error_files: Vec<String> = cold
        .trees
        .iter()
        .filter(|(_, tree)| !tree.parse_errors().is_empty())
        .map(|(path, _)| path.clone())
        .collect();

    let t = Instant::now();
    let warm = match generation_check(&params) {
        Ok(outcome) => outcome,
        Err(e) => {
            return GenerationOutcome {
                diagnostics: cold.findings,
                parse_error_files,
                cold: cold_elapsed,
                warm: t.elapsed(),
                parity: Parity::Failed(format!("warm re-check failed: {e}")),
            };
        }
    };
    let warm_elapsed = t.elapsed();
    let parity = if warm.report.mode == GenerationMode::Warm {
        compare_findings(&cold.findings, &warm.findings)
    } else {
        Parity::Failed(
            "the second pass ran cold — the first pass published no generation to warm from"
                .to_owned(),
        )
    };
    GenerationOutcome {
        diagnostics: cold.findings,
        parse_error_files,
        cold: cold_elapsed,
        warm: warm_elapsed,
        parity,
    }
}

/// Warm ≡ cold over one project's whole finding set (ADR-0092 §5). Compares
/// the fields a finding *means* — id, path, position, message — and names the
/// first disagreement, because a corpus-scale divergence dumped whole is
/// unreadable and the first one is where triage starts.
fn compare_findings(cold: &[Diagnostic], warm: &[Diagnostic]) -> Parity {
    let key = |d: &Diagnostic| (d.id, d.path.clone(), d.line, d.column, d.message.clone());
    let mut a: Vec<_> = cold.iter().map(key).collect();
    let mut b: Vec<_> = warm.iter().map(key).collect();
    a.sort();
    b.sort();
    if a == b {
        return Parity::Agreed;
    }
    let show = |(id, path, line, column, message): &(&str, String, u32, u32, String)| {
        format!("{path}:{line}:{column} [{id}] {message}")
    };
    let first = a
        .iter()
        .zip(b.iter())
        .find(|(x, y)| x != y)
        .map_or_else(
            || match a.len().cmp(&b.len()) {
                std::cmp::Ordering::Greater => format!("cold-only: {}", show(&a[b.len()])),
                std::cmp::Ordering::Less => format!("warm-only: {}", show(&b[a.len()])),
                std::cmp::Ordering::Equal => "no first divergence (unreachable)".to_owned(),
            },
            |(x, y)| format!("cold: {} | warm: {}", show(x), show(y)),
        );
    Parity::Diverged(format!(
        "warm ≢ cold — {} cold finding(s) vs {} warm; first divergence: {first}",
        a.len(),
        b.len()
    ))
}

/// Analyze one package as a single project and time it.
fn analyze_package(
    name: &str,
    tag: &str,
    dir: &Path,
    root: &Path,
    pins: &[ExpectedProofFinding],
) -> PackageReport {
    let files = collect_php_files(dir);
    // Diagnostic paths are `root`-relative, as they have always been, and the
    // seal keys them against `root` — the same spelling-plus-root pairing the
    // CLI derives (issue #506).
    let rel: Vec<PathBuf> =
        files.iter().map(|f| f.strip_prefix(root).unwrap_or(f).to_path_buf()).collect();

    // Paths are `root`-relative above, so the layout resolves against `root`
    // (ADR-0015): the package's own `composer.json` decides what is vendor.
    // Its `require.php` is the declared target PHP range (issue #28), which
    // gates curated-fact admission and the absence family; the orchestrator
    // reads it off this layout for itself.
    let layout = composer::discover(&[dir.to_path_buf()], root);
    let run = analyze_through_generations(name, &rel, root, &layout, root);

    // Parse-error files: their diagnostics are excluded from the count.
    // ADR-0079 (#180): mostly redundant now (a failed-parse file emits only
    // `syntax.unparsable`, nothing else to drop) but still a deliberate blind
    // spot for that id — a pre-existing unparsable file can't red the gate on
    // a remedy that lives elsewhere. What it does NOT drop: a non-vendor
    // unparsable file dams the existence family for that package, only ever
    // lowering counts.
    let parse_error_files = run.parse_error_files;
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let mut diags: Vec<Diagnostic> = run.diagnostics;
    diags.retain(|d| !parse_err_set.contains(d.path.as_str()));
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    // Measurement-mode split (ADR-0050 §9): contract-layer findings are counted
    // but gate only via their per-package increase tripwire, not on sight. The
    // layer (steins-infer registry) is the gate carrier; prefix keys each count
    // table (`phpdoc.*`, `throw.*`, `effect.*`). Proof/mechanics — including
    // `effect.unknown-label` — stay red-on-sight in `diags`.
    let phpdoc: Vec<Diagnostic> = diags.iter().filter(|d| is_phpdoc(d)).cloned().collect();
    let throws: Vec<Diagnostic> = diags.iter().filter(|d| is_throw(d)).cloned().collect();
    let effects: Vec<Diagnostic> = diags.iter().filter(|d| is_effect_contract(d)).cloned().collect();
    let possibly: Vec<Diagnostic> = diags.iter().filter(|d| is_possibly(d)).cloned().collect();
    // Debug-layer findings (ADR-0053 §8) are dropped outright before the
    // contract split — a dump is requested introspection, not a finding.
    // Vacuous today (no emitter until D3/D4).
    diags.retain(|d| !is_debug(d));
    diags.retain(|d| !is_contract(d));
    diags.retain(|d| !is_possibly(d));
    // Split off triaged TRUE runtime-layer positives (reported, not gated); e.g.
    // the ADR-0049 S2 `call.undefined-method` findings pinned in
    // `EXPECTED_PROOF_FINDINGS` — any un-pinned finding still reds the gate.
    let expected_true: Vec<Diagnostic> =
        diags.iter().filter(|d| is_expected_true_positive(pins, name, d)).cloned().collect();
    diags.retain(|d| !is_expected_true_positive(pins, name, d));

    PackageReport {
        name: name.to_owned(),
        tag: tag.to_owned(),
        local: false,
        file_count: files.len(),
        parse_error_files,
        diagnostics: diags,
        phpdoc,
        throws,
        effects,
        possibly,
        expected_true,
        vendor_suppressed: 0,
        // A pinned package's revision is in the tracked `corpus.lock.toml` and the
        // sync checks it out — reproducible by construction, nothing to compare.
        recorded_revision: None,
        measured_revision: None,
        worktree: WorktreeState::Unknown,
        elapsed: run.cold,
        warm_elapsed: run.warm,
        parity: run.parity,
    }
}

/// Analyze one local project (ADR-0013 §4) as a single project. Paths are made
/// project-relative so the `vendor/` predicate and the report read cleanly.
/// Vendor findings are split out of the gate count (ADR-0015).
fn analyze_local(proj: &LocalProject, pins: &[ExpectedProofFinding]) -> PackageReport {
    let root = Path::new(&proj.path);

    // Read the tree's state BEFORE walking it, so revision/cleanliness match
    // the count they're reported beside. Both degrade to "unknown" rather
    // than failing.
    let measured_revision = corpus_local::checkout_revision(root);
    let worktree = WorktreeState::from_dirty(corpus_local::checkout_is_dirty(root));

    let files = corpus_local::collect_php_files_in(root, &proj.paths, &proj.exclude);
    // Project-relative paths (falling back to the full path if a file is not
    // under `root`, which cannot normally happen). Keeps `vendor/` detection
    // and the printed rows readable, and gives the seal its root.
    let rel: Vec<PathBuf> =
        files.iter().map(|f| f.strip_prefix(root).unwrap_or(f).to_path_buf()).collect();

    let layout = composer::discover(&[root.to_path_buf()], root);
    let run = analyze_through_generations(&proj.name, &rel, root, &layout, &repo_root());

    // Same exclusion/ADR-0079 reading as the corpus path above. Swept
    // 2026-08-08; the local root holds exactly three pre-existing unparsable
    // files, each with further errors cascading behind its first:
    //
    //   vendor/apache/thrift/lib/php/lib/Thrift/Transport/TCurlClient.php:95  (+8)
    //   vendor/apache/thrift/lib/php/lib/Thrift/Transport/THttpClient.php:100 (+8)
    //   php-openid/Tests/Auth/OpenID/HMAC.php:66                              (+6)
    //
    // All three are VENDOR, so none is a dam site and each `syntax.unparsable`
    // is dropped below anyway — the §2.5 member-incomplete leg stays exercised
    // by fixtures alone (`crates/steins-infer/tests/it/parse_failure_dam.rs`)
    // until a NON-vendor break appears here, at which point these counts can
    // only fall, never rise.
    let parse_error_files = run.parse_error_files;
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let mut diags: Vec<Diagnostic> = run.diagnostics;
    diags.retain(|d| !parse_err_set.contains(d.path.as_str()));

    // Vendor default (ADR-0015): vendor code was fully indexed and inferred, but
    // its findings do not count against the gate. Split them out.
    let before = diags.len();
    diags.retain(|d| !layout.is_vendor(&d.path));
    let vendor_suppressed = before - diags.len();
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    // Measurement-mode split (first-party only; vendor already removed above).
    // Same ADR-0050 §9 layer-driven partition as `analyze_package`.
    let phpdoc: Vec<Diagnostic> = diags.iter().filter(|d| is_phpdoc(d)).cloned().collect();
    let throws: Vec<Diagnostic> = diags.iter().filter(|d| is_throw(d)).cloned().collect();
    let effects: Vec<Diagnostic> = diags.iter().filter(|d| is_effect_contract(d)).cloned().collect();
    let possibly: Vec<Diagnostic> = diags.iter().filter(|d| is_possibly(d)).cloned().collect();
    // Debug-layer findings (ADR-0053 §8): excluded from every counter (see
    // `analyze_package`). Vacuous until D3/D4 — byte-identical gate output today.
    diags.retain(|d| !is_debug(d));
    diags.retain(|d| !is_contract(d));
    diags.retain(|d| !is_possibly(d));
    // Split off triaged TRUE runtime-layer positives (reported, not gated).
    let expected_true: Vec<Diagnostic> = diags
        .iter()
        .filter(|d| is_expected_true_positive(pins, &proj.name, d))
        .cloned()
        .collect();
    diags.retain(|d| !is_expected_true_positive(pins, &proj.name, d));

    PackageReport {
        name: proj.name.clone(),
        tag: String::new(),
        local: true,
        file_count: files.len(),
        parse_error_files,
        diagnostics: diags,
        phpdoc,
        throws,
        effects,
        possibly,
        expected_true,
        vendor_suppressed,
        recorded_revision: proj.revision.clone(),
        measured_revision,
        worktree,
        elapsed: run.cold,
        warm_elapsed: run.warm,
        parity: run.parity,
    }
}

fn print_report(
    baselines: &Baselines,
    reports: &[PackageReport],
    local_reports: &[PackageReport],
    regressions: &[PhpdocRegression],
    throw_regressions: &[PhpdocRegression],
    effect_regressions: &[PhpdocRegression],
    possibly_regressions: &[PhpdocRegression],
) {
    println!("\n=== fp-gate: per-package findings ===\n");
    if !local_reports.is_empty() {
        println!(
            "note: {} local project(s) are UNPINNED live working trees (corpus.local.toml, \
             ADR-0013 §4); their vendor findings are indexed for inference but do not gate \
             (ADR-0015).\n",
            local_reports.len()
        );
    }
    // Packages first, then local projects, in the per-project findings section.
    for r in reports.iter().chain(local_reports.iter()) {
        let ident = if r.local {
            format!("{} (local)", r.name)
        } else {
            format!("{} @ {}", r.name, r.tag)
        };
        let vendor_note = if r.local {
            format!(", {} vendor-suppressed", r.vendor_suppressed)
        } else {
            String::new()
        };
        println!(
            "{ident} — {} files, {} parse-error files, {} diagnostics{vendor_note} ({:.2}s)",
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len(),
            r.elapsed.as_secs_f64()
        );
        // Printed on every run for a local project, green included — not only
        // when a tripwire trips.
        if r.local {
            println!("    {}", revision_summary_line(&r.revision()));
        }
        if !r.parse_error_files.is_empty() {
            for sample in r.parse_error_files.iter().take(5) {
                println!("    parse-error: {sample}");
            }
            if r.parse_error_files.len() > 5 {
                println!("    … and {} more", r.parse_error_files.len() - 5);
            }
        }
        for d in &r.diagnostics {
            println!("    DIAGNOSTIC {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
        }
        if !r.expected_true.is_empty() {
            println!(
                "    [expected TRUE positive] {} triaged real-bug finding(s) (excluded from red/green — see EXPECTED_PROOF_FINDINGS):",
                r.expected_true.len()
            );
            for d in &r.expected_true {
                println!("    TRUE-POSITIVE {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
            }
        }
        if !r.phpdoc.is_empty() {
            println!("    [measurement mode] {} phpdoc.* finding(s) (excluded from red/green):", r.phpdoc.len());
            for d in &r.phpdoc {
                println!("    PHPDOC {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
            }
        }
    }

    // `phpdoc.*` declared-contract ids, counted per package against
    // `PHPDOC_EXPECTED`. They do not gate on existence (TRUE contract findings
    // live in released code, ADR-0030); only an increase past baseline reds.
    let total_phpdoc: usize = reports.iter().chain(local_reports.iter()).map(|r| r.phpdoc.len()).sum();
    let total_expected = baselines.phpdoc.total();
    println!("\n=== phpdoc.* measurement mode (contract layer — gates only on INCREASE) ===\n");
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = baselines.phpdoc.expected(&r.name);
        if r.phpdoc.is_empty() && expected == 0 {
            continue;
        }
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        let (params, returns) = r
            .phpdoc
            .iter()
            .fold((0usize, 0usize), |(p, ret), d| match d.id {
                "phpdoc.param-mismatch" => (p + 1, ret),
                "phpdoc.return-mismatch" => (p, ret + 1),
                _ => (p, ret),
            });
        let actual = r.phpdoc.len();
        let marker = match actual.cmp(&expected) {
            std::cmp::Ordering::Greater => "  ⬆ REGRESSION (exceeds expected)",
            std::cmp::Ordering::Less => "  ⬇ improved (below expected — update baseline when intentional)",
            std::cmp::Ordering::Equal => "",
        };
        println!(
            "{label} — {actual} phpdoc.* ({params} param, {returns} return) [expected {expected}]{marker}"
        );
    }
    println!("phpdoc.* TOTAL: {total_phpdoc} (expected baseline {total_expected})");
    print_tripwire("phpdoc.*", regressions, local_reports);

    // `throw.*` contract-layer ids (ADR-0040), counted against `THROW_EXPECTED`,
    // gating only on increase. Volume is far larger than `phpdoc.*` (checked-
    // exception saturation), so only counts and a small sample print.
    let total_throw: usize = reports.iter().chain(local_reports.iter()).map(|r| r.throws.len()).sum();
    let total_throw_expected = baselines.throw.total();
    println!("\n=== throw.* measurement mode (contract layer — gates only on INCREASE) ===\n");
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = baselines.throw.expected(&r.name);
        if r.throws.is_empty() && expected == 0 {
            continue;
        }
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        let (undecl, liskov) = r.throws.iter().fold((0usize, 0usize), |(u, l), d| match d.id {
            "throw.undeclared" => (u + 1, l),
            "throw.liskov-widened" => (u, l + 1),
            _ => (u, l),
        });
        let actual = r.throws.len();
        let marker = match actual.cmp(&expected) {
            std::cmp::Ordering::Greater => "  ⬆ REGRESSION (exceeds expected)",
            std::cmp::Ordering::Less => "  ⬇ improved (below expected — update baseline when intentional)",
            std::cmp::Ordering::Equal => "",
        };
        println!(
            "{label} — {actual} throw.* ({undecl} undeclared, {liskov} liskov) [expected {expected}]{marker}"
        );
        // A tiny sample so a regression is triageable without a 35k-line dump.
        if actual > expected {
            for d in r.throws.iter().take(3) {
                println!("    THROW {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
            }
        }
    }
    println!("throw.* TOTAL: {total_throw} (expected baseline {total_throw_expected})");
    print_tripwire("throw.*", throw_regressions, local_reports);

    // `effect.*` contract ids (ADR-0050 §9 delta). Suppressed while dormant —
    // prints nothing unless a finding lands, the table is seeded, or a
    // regression trips — kept the report byte-identical pre-convergence. Off
    // since 2026-08-12, when #303's interop-envelope run made the private
    // monorepo's purity tags fire (see [`EFFECT_EXPECTED`]'s seeded row).
    let total_effect: usize = reports.iter().chain(local_reports.iter()).map(|r| r.effects.len()).sum();
    if total_effect > 0 || !baselines.effect.is_empty() || !effect_regressions.is_empty() {
        let total_effect_expected = baselines.effect.total();
        println!("\n=== effect.* measurement mode (contract layer — gates only on INCREASE) ===\n");
        for r in reports.iter().chain(local_reports.iter()) {
            let expected = baselines.effect.expected(&r.name);
            if r.effects.is_empty() && expected == 0 {
                continue;
            }
            let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
            let (envelope, liskov) = r.effects.iter().fold((0usize, 0usize), |(e, l), d| match d.id {
                "effect.envelope-exceeded" => (e + 1, l),
                "effect.liskov-widened" => (e, l + 1),
                _ => (e, l),
            });
            let actual = r.effects.len();
            let marker = match actual.cmp(&expected) {
                std::cmp::Ordering::Greater => "  ⬆ REGRESSION (exceeds expected)",
                std::cmp::Ordering::Less => "  ⬇ improved (below expected — update baseline when intentional)",
                std::cmp::Ordering::Equal => "",
            };
            println!(
                "{label} — {actual} effect.* ({envelope} envelope, {liskov} liskov) [expected {expected}]{marker}"
            );
            if actual > expected {
                for d in r.effects.iter().take(3) {
                    println!("    EFFECT {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
                }
            }
        }
        println!("effect.* TOTAL: {total_effect} (expected baseline {total_effect_expected})");
        print_tripwire("effect.*", effect_regressions, local_reports);
    }

    // Possibly-grade proof ids (ADR-0081 §8), the `strict`-floored rows,
    // counted against `POSSIBLY_EXPECTED`, gating only on increase. Every
    // finding prints — the volume is triageable and there's no prefix to
    // search under.
    let total_possibly: usize =
        reports.iter().chain(local_reports.iter()).map(|r| r.possibly.len()).sum();
    println!(
        "\n=== possibly-grade proof ids, strict floor (measurement mode — gates only on INCREASE) ===\n"
    );
    let total_possibly_expected = baselines.possibly.total();
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = baselines.possibly.expected(&r.name);
        if r.possibly.is_empty() && expected == 0 {
            continue;
        }
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        let actual = r.possibly.len();
        let marker = match actual.cmp(&expected) {
            std::cmp::Ordering::Greater => "  ⬆ REGRESSION (exceeds expected)",
            std::cmp::Ordering::Less => {
                "  ⬇ improved (below expected — update baseline when intentional)"
            }
            std::cmp::Ordering::Equal => "",
        };
        let by_id = |want: &str| r.possibly.iter().filter(|d| d.id == want).count();
        println!(
            "{label} — {actual} possibly-grade ({} variable, {} property, {} return) [expected {expected}]{marker}",
            by_id("variable.maybe-undefined"),
            by_id("property.maybe-undefined"),
            by_id("type.return-maybe-missing"),
        );
        for d in &r.possibly {
            println!("    POSSIBLY {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
        }
    }
    println!(
        "possibly-grade TOTAL: {total_possibly} (expected baseline {total_possibly_expected})"
    );
    print_tripwire("possibly-grade", possibly_regressions, local_reports);

    // Summary table: packages and local projects share one table; local rows are
    // marked `(local)`.
    let rows = || reports.iter().chain(local_reports.iter());
    let name_w = rows()
        .map(|r| r.name.len() + if r.local { " (local)".len() } else { 0 })
        .max()
        .unwrap_or(4)
        .max(7);
    // Warm ≡ cold over the whole corpus (ADR-0092 §5, issue #525). Printed
    // before the summary because a divergence explains every number above it:
    // the counts are the cold pass's, and if the warm pass disagreed then the
    // cache the shipped CLI uses is not reporting what the gate calibrated.
    println!("\n=== generation parity (warm ≡ cold, ADR-0092 §5) ===\n");
    let broken: Vec<&PackageReport> = rows().filter(|r| !r.parity.is_green()).collect();
    if broken.is_empty() {
        println!(
            "parity: OK — all {} project(s) reproduced their cold findings from the published generation.",
            rows().count()
        );
    } else {
        println!("parity: BROKEN — {} project(s):", broken.len());
        for r in &broken {
            println!("    {} — {}", r.name, r.parity.note().unwrap_or("(no detail)"));
        }
    }

    println!("\n=== summary ===\n");
    println!(
        "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8}  {:>8}",
        "package", "files", "parse-errors", "diagnostics", "cold(s)", "warm(s)"
    );
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8 + 2 + 8));
    let (mut tf, mut tp, mut td) = (0usize, 0usize, 0usize);
    let (mut tcold, mut twarm) = (0.0f64, 0.0f64);
    for r in rows() {
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        println!(
            "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}  {:>8.2}",
            label,
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len(),
            r.elapsed.as_secs_f64(),
            r.warm_elapsed.as_secs_f64()
        );
        tf += r.file_count;
        tp += r.parse_error_files.len();
        td += r.diagnostics.len();
        tcold += r.elapsed.as_secs_f64();
        twarm += r.warm_elapsed.as_secs_f64();
    }
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8 + 2 + 8));
    println!(
        "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}  {:>8.2}",
        "TOTAL", tf, tp, td, tcold, twarm
    );

    println!();
    let measurement_ok =
        regressions.is_empty() && throw_regressions.is_empty() && effect_regressions.is_empty();
    match (td == 0, measurement_ok, broken.is_empty()) {
        (true, true, true) => {
            println!(
                "GATE GREEN — no proof-layer diagnostics on clean-parsing corpus code, \
                 no phpdoc.*/throw.* regression past the expected baselines, and every \
                 project's warm re-check reproduced its cold findings."
            );
        }
        (false, _, _) => {
            println!(
                "GATE RED — {td} proof-layer diagnostic(s) on clean code. Human FP triage required (ADR-0013)."
            );
        }
        (true, false, _) => {
            println!(
                "GATE RED — {} package(s) regressed past their expected phpdoc.*/throw.* baseline \
                 (see the tripwire lists above). Investigate the new finding(s); update \
                 PHPDOC_EXPECTED / THROW_EXPECTED in xtask/fp-gate/ only once the change is \
                 understood and intended.",
                regressions.len() + throw_regressions.len()
            );
        }
        (true, true, false) => {
            println!(
                "GATE RED — {} project(s) failed the warm ≡ cold parity check (see above). A \
                 divergence is a SOUNDNESS bug in the generation cache, not a cost regression: \
                 `steins check` ships that cache on by default, so a finding the warm pass \
                 loses is a finding the product loses.",
                broken.len()
            );
        }
    }
}

/// Print one measurement family's tripwire verdict, and — for a tripped local
/// project — the recorded-vs-measured revision line: a raised count on a
/// pinned package can only be the analyzer, but on a live tree it's
/// ambiguous, and this is where the operator decides to triage or re-measure.
fn print_tripwire(family: &str, regressions: &[PhpdocRegression], local_reports: &[PackageReport]) {
    if regressions.is_empty() {
        println!("{family} tripwire: OK — no package exceeds its expected baseline.");
        return;
    }
    println!("{family} tripwire: TRIPPED — the following packages regressed:");
    for reg in regressions {
        println!("    {} — {} > expected {}", reg.name, reg.actual, reg.expected);
        if let Some(local) = local_reports.iter().find(|r| r.name == reg.name) {
            println!("        {}", revision_tripwire_line(&local.revision()));
        }
    }
}

#[cfg(test)]
mod tests {
    use steins_infer::is_vendor_path;

    use super::{
        RevisionStatus, WorktreeState, classify_revision, revision_summary_line,
        revision_tripwire_line,
    };

    // Synthetic revisions only. A real private-corpus sha must never enter a
    // tracked file, test fixtures included.
    const REV_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const REV_B: &str = "fedcba9876543210fedcba9876543210fedcba98";

    /// Classify with a clean tree — the common case for the revision-comparison
    /// tests, which are about the shas rather than the working tree.
    fn classify(recorded: Option<&str>, measured: Option<&str>) -> RevisionStatus {
        classify_revision(recorded, measured, WorktreeState::Clean)
    }

    // The vendor-path predicate (ADR-0015) drives the gate's local-project vendor
    // split; verify its component-boundary behavior here where the gate uses it.
    #[test]
    fn vendor_predicate_matches_directory_components_only() {
        // A `vendor/` component — top-level or nested — is vendor.
        assert!(is_vendor_path("vendor/foo/Bar.php"));
        assert!(is_vendor_path("src/vendor/foo/Bar.php"));
        assert!(is_vendor_path("/abs/mono/vendor/pkg/lib.php"));
        assert!(is_vendor_path("a\\vendor\\b.php")); // Windows separators
        // First-party paths are not vendor — including look-alikes.
        assert!(!is_vendor_path("src/app/Service.php"));
        assert!(!is_vendor_path("vendor_proj/app/Service.php")); // sibling, not a component
        assert!(!is_vendor_path("src/vendored/x.php"));
        assert!(!is_vendor_path("app/vendor.php")); // filename, not a directory
    }

    #[test]
    fn a_recorded_revision_equal_to_the_measured_one_matches() {
        assert_eq!(
            classify(Some(REV_A), Some(REV_A)),
            RevisionStatus::Matches { revision: REV_A.to_owned(), worktree: WorktreeState::Clean }
        );
        // Case and surrounding whitespace are incidental, not a difference.
        assert_eq!(
            classify(Some(format!("  {}  ", REV_A.to_ascii_uppercase()).as_str()), Some(REV_A)),
            RevisionStatus::Matches { revision: REV_A.to_owned(), worktree: WorktreeState::Clean }
        );
    }

    #[test]
    fn a_hand_written_abbreviation_matches_the_full_measured_sha() {
        // Humans paste short shas; the full measured one is reported back.
        assert_eq!(
            classify(Some(&REV_A[..12]), Some(REV_A)),
            RevisionStatus::Matches { revision: REV_A.to_owned(), worktree: WorktreeState::Clean }
        );
        // Too short to be evidence of identity — treated as a difference, which
        // asks for a re-measure instead of silently blessing the count.
        assert_eq!(
            classify(Some(&REV_A[..4]), Some(REV_A)),
            RevisionStatus::Differs {
                recorded: REV_A[..4].to_owned(),
                measured: REV_A.to_owned()
            }
        );
    }

    #[test]
    fn a_recorded_revision_unlike_the_measured_one_differs() {
        assert_eq!(
            classify(Some(REV_A), Some(REV_B)),
            RevisionStatus::Differs { recorded: REV_A.to_owned(), measured: REV_B.to_owned() }
        );
    }

    #[test]
    fn an_absent_revision_is_unrecorded_not_an_error() {
        assert_eq!(
            classify(None, Some(REV_A)),
            RevisionStatus::Unrecorded { measured: Some(REV_A.to_owned()) }
        );
        // An empty string is a missing value, not a revision.
        assert_eq!(
            classify(Some("   "), Some(REV_A)),
            RevisionStatus::Unrecorded { measured: Some(REV_A.to_owned()) }
        );
        // Neither side known (a non-git path, or no git): still legal, still no panic.
        assert_eq!(classify(None, None), RevisionStatus::Unrecorded { measured: None });
    }

    #[test]
    fn an_unreadable_checkout_leaves_a_recorded_revision_uncompared() {
        assert_eq!(
            classify(Some(REV_A), None),
            RevisionStatus::Unreadable { recorded: REV_A.to_owned() }
        );
    }

    #[test]
    fn the_tripwire_line_names_a_match_a_regression_and_a_difference_drift() {
        let matching = revision_tripwire_line(&classify(Some(REV_A), Some(REV_A)));
        assert!(matching.contains("MATCHES"), "{matching}");
        assert!(matching.contains("CLEAN"), "{matching}");
        assert!(matching.contains("GENUINE REGRESSION"), "{matching}");
        assert!(matching.contains(REV_A), "{matching}");

        let differing = revision_tripwire_line(&classify(Some(REV_A), Some(REV_B)));
        assert!(differing.contains("DIFFERS"), "{differing}");
        assert!(differing.contains("CORPUS DRIFT"), "{differing}");
        // Both revisions are named, so the reader knows what to re-measure against.
        assert!(differing.contains(REV_A) && differing.contains(REV_B), "{differing}");
        assert!(differing.contains("reseed"), "{differing}");

        let unrecorded = revision_tripwire_line(&classify(None, Some(REV_B)));
        assert!(unrecorded.contains("UNPINNED"), "{unrecorded}");
        assert!(unrecorded.contains("CANNOT be told apart"), "{unrecorded}");
        // Copy-pasteable into corpus.local.toml.
        assert!(unrecorded.contains(&format!("revision = \"{REV_B}\"")), "{unrecorded}");
    }

    #[test]
    fn a_dirty_tree_carries_onto_a_match_and_nowhere_else() {
        // Cleanliness is what makes a matching revision believable about the FILES,
        // so it is recorded on the match…
        assert_eq!(
            classify_revision(Some(REV_A), Some(REV_A), WorktreeState::Dirty),
            RevisionStatus::Matches { revision: REV_A.to_owned(), worktree: WorktreeState::Dirty }
        );
        assert_eq!(
            classify_revision(Some(REV_A), Some(REV_A), WorktreeState::Unknown),
            RevisionStatus::Matches { revision: REV_A.to_owned(), worktree: WorktreeState::Unknown }
        );
        // …and nowhere else: an already-inconclusive verdict is inconclusive
        // whatever the tree looks like, so the other states are unaffected by it.
        for tree in [WorktreeState::Clean, WorktreeState::Dirty, WorktreeState::Unknown] {
            assert_eq!(
                classify_revision(Some(REV_A), Some(REV_B), tree),
                RevisionStatus::Differs { recorded: REV_A.to_owned(), measured: REV_B.to_owned() }
            );
            assert_eq!(
                classify_revision(None, Some(REV_B), tree),
                RevisionStatus::Unrecorded { measured: Some(REV_B.to_owned()) }
            );
            assert_eq!(
                classify_revision(Some(REV_A), None, tree),
                RevisionStatus::Unreadable { recorded: REV_A.to_owned() }
            );
        }
    }

    #[test]
    fn a_dirty_or_unverified_tree_withholds_the_regression_verdict() {
        // The point of the hedge: only a CLEAN match may say "genuine regression".
        // A dirty one must not tell the operator to stop looking at the corpus.
        let dirty = revision_tripwire_line(&classify_revision(
            Some(REV_A),
            Some(REV_A),
            WorktreeState::Dirty,
        ));
        assert!(dirty.contains("DIRTY"), "{dirty}");
        assert!(dirty.contains("NOT exactly"), "{dirty}");
        assert!(dirty.contains("may still be corpus-side"), "{dirty}");
        assert!(!dirty.contains("GENUINE REGRESSION"), "{dirty}");

        let unknown = revision_tripwire_line(&classify_revision(
            Some(REV_A),
            Some(REV_A),
            WorktreeState::Unknown,
        ));
        // Unknown says so rather than implying clean.
        assert!(unknown.contains("could not be determined"), "{unknown}");
        assert!(!unknown.contains("GENUINE REGRESSION"), "{unknown}");
        assert!(!unknown.contains("CLEAN"), "{unknown}");

        // The summary line makes the same three distinctions on every run.
        let s_clean =
            revision_summary_line(&classify_revision(Some(REV_A), Some(REV_A), WorktreeState::Clean));
        assert!(s_clean.contains("working tree clean"), "{s_clean}");
        let s_dirty =
            revision_summary_line(&classify_revision(Some(REV_A), Some(REV_A), WorktreeState::Dirty));
        assert!(s_dirty.contains("DIRTY"), "{s_dirty}");
        let s_unknown = revision_summary_line(&classify_revision(
            Some(REV_A),
            Some(REV_A),
            WorktreeState::Unknown,
        ));
        assert!(s_unknown.contains("could not be determined"), "{s_unknown}");
    }

    #[test]
    fn the_summary_line_reports_the_measured_revision_in_every_case() {
        assert!(
            revision_summary_line(&classify(Some(REV_A), Some(REV_A))).contains(REV_A)
        );
        let drifted = revision_summary_line(&classify(Some(REV_A), Some(REV_B)));
        assert!(drifted.contains(REV_B) && drifted.contains(REV_A), "{drifted}");
        let unpinned = revision_summary_line(&classify(None, Some(REV_B)));
        assert!(unpinned.contains("UNPINNED"), "{unpinned}");
        assert!(unpinned.contains(&format!("revision = \"{REV_B}\"")), "{unpinned}");
        // Degraded reads say so plainly rather than pretending to a comparison.
        assert!(revision_summary_line(&classify(None, None)).contains("unknown"));
        assert!(
            revision_summary_line(&classify(Some(REV_A), None)).contains("unknown")
        );
    }
}
