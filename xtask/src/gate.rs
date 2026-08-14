//! `fp-gate`: run the full proof-layer pipeline over the pinned corpus.
//!
//! ADR-0013: one proof-layer diagnostic on working code is a release blocker,
//! so this gate exits nonzero the moment any diagnostic fires on a clean-parsing
//! file — that is exactly the triage material we want surfaced, never hidden.
//!
//! Whole-project mode (ADR-0009/0015): each corpus package is analyzed as ONE
//! project (a single salsa DB holding all its `.php` files), so cross-file
//! calls, class chains, and effects resolve. Packages run in parallel (rayon);
//! within a package the analysis is one project run. Files that fail to parse
//! are still included in the project (so resolution stays complete — a partial
//! tree can only *silence*, never add a false positive), but any diagnostic that
//! lands in a parse-error file is excluded from the gate count.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use steins_db::{Project, SourceFile, SteinsDatabase, parse};
use steins_db::composer;
use steins_infer::{Diagnostic, Floor, Layer, SidecarFolder, check_project, layer, surface_floor};

use crate::corpus::{PACKAGES, checkout_dir, collect_php_files, read_lock, repo_root};
use crate::corpus_local::{self, LocalProject};

/// Per-project result of the gate run (a pinned corpus package or an unpinned
/// local project). `diagnostics` holds only the findings that count against the
/// gate; for local projects, vendor findings are excluded (ADR-0015) and tallied
/// separately in `vendor_suppressed`.
struct PackageReport {
    name: String,
    /// The pinned release tag, or empty for a local (unpinned) project.
    tag: String,
    /// A live working tree injected via `corpus.local.toml` (ADR-0013 §4).
    local: bool,
    file_count: usize,
    parse_error_files: Vec<String>,
    diagnostics: Vec<Diagnostic>,
    /// NEW `phpdoc.*` declared-contract findings, held separately: in this run
    /// they are **measurement mode** (ADR-0030 relation #1 landing) — reported and
    /// counted per package but excluded from the red/green verdict.
    phpdoc: Vec<Diagnostic>,
    /// `throw.*` findings (ADR-0040/0007), held in the same **measurement mode**
    /// as `phpdoc.*`: they are contract-layer claims about the code's own
    /// `@throws` documentation (an undeclared checked throw, a Liskov-widened
    /// override), never runtime-breakage — TRUE ones abound in working code
    /// (the checked-exception volume ADR-0007 keeps quiet by default), so they
    /// gate only as a per-package increase tripwire.
    throws: Vec<Diagnostic>,
    /// `effect.*` **contract-layer** findings (`effect.envelope-exceeded`,
    /// `effect.liskov-widened`, `effect.interop-unknown-label`), held in
    /// measurement mode under ADR-0050 §9: the recorded gate-policy delta moves
    /// them off red-on-sight onto the same per-package increase tripwire as
    /// `phpdoc.*`/`throw.*`, matching their declared-contract semantics. None of the
    /// three needs a Steins annotation to fire: `effect.interop-unknown-label`
    /// (issue #311) reads upstream's own `@phpstan-impure`, and since issue #303 the
    /// envelope pair reads upstream's purity tags the same way — which is how the
    /// private monorepo's `@phpstan-all-methods-pure` classes seeded
    /// [`EFFECT_EXPECTED`]'s first row on code that has never heard of Steins.
    /// `effect.unknown-label` is **mechanics**, not contract — it stays on the
    /// red-on-sight path in `diagnostics`, never here.
    effects: Vec<Diagnostic>,
    /// **Possibly-grade** proof findings — the `strict`-floored proof ids
    /// (ADR-0081 §8), held in the same measurement mode as `phpdoc.*`/`throw.*`:
    /// counted per package against [`POSSIBLY_EXPECTED`] and gating only on an
    /// increase. Their definite siblings (`variable.undefined`,
    /// `property.undefined`, `type.return-missing`) stay red-on-sight in
    /// `diagnostics`, never here.
    possibly: Vec<Diagnostic>,
    /// Triaged TRUE runtime-layer positives (real broken corpus code Steins
    /// correctly proves; see [`EXPECTED_PROOF_FINDINGS`]). Reported prominently
    /// but excluded from the red/green verdict — matched at finding precision so
    /// any drift falls back into `diagnostics` and reds the gate.
    expected_true: Vec<Diagnostic>,
    /// Vendor findings suppressed from the gate count (local projects only).
    vendor_suppressed: usize,
    /// The revision recorded in `corpus.local.toml` for this local project, i.e.
    /// the state its seeded baselines were measured at. `None` for pinned corpus
    /// packages (whose revision lives in the tracked `corpus.lock.toml`) and for a
    /// local entry that records none.
    recorded_revision: Option<String>,
    /// The revision the local project's checkout is actually on this run, or
    /// `None` when it could not be read (see [`corpus_local::checkout_revision`]).
    measured_revision: Option<String>,
    /// Whether that checkout also carries uncommitted or untracked content, which
    /// decides whether a matching revision may be believed (see [`WorktreeState`]).
    worktree: WorktreeState,
    elapsed: Duration,
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

/// Whether a local project's working tree carries anything on top of the revision
/// it reports — the difference between "the files measured ARE that commit" and
/// "the files measured are that commit plus whatever the operator has in flight".
///
/// This exists because a revision match alone is not evidence about the *files*,
/// and a private corpus is somebody's working checkout, where a dirty tree is the
/// normal state. The match arm is the one message that tells the operator to stop
/// looking at the corpus and go triage findings; issuing it confidently against a
/// tree that is not exactly the recorded commit is a wrong answer in the direction
/// of "don't look", which is the expensive direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeState {
    /// `git status --porcelain` was empty.
    Clean,
    /// Non-empty: modified, staged, or untracked content sits on top. The gate
    /// walks the filesystem rather than the index, so untracked counts as dirty.
    Dirty,
    /// Undeterminable (not a git checkout, no `git`, a spawn failure, a non-zero
    /// exit) — reported as unknown rather than assumed clean.
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

/// How a local project's **recorded** baseline revision relates to the revision
/// its checkout was actually sitting on when this run measured it.
///
/// This is the whole point of `revision` in `corpus.local.toml`. The pinned
/// packages are reproducible by construction (`corpus.lock.toml` records a commit
/// per package), so a count move there can only be the analyzer. A local project
/// is a live working tree that nothing here checks out, so a count move is
/// ambiguous between "the analyzer regressed" and "the corpus moved" — and
/// resolving that ambiguity after the fact costs archaeology in a repository this
/// one cannot see. Recording the measured revision collapses the ambiguity at the
/// moment the baseline is seeded, which is the only moment it is cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RevisionStatus {
    /// A revision is recorded and the checkout is on it. `worktree` decides how far
    /// that may be believed: only a CLEAN tree makes the measured files identical
    /// to the seeded ones.
    Matches { revision: String, worktree: WorktreeState },
    /// A revision is recorded and the checkout is somewhere else: the corpus moved
    /// under the baseline.
    Differs { recorded: String, measured: String },
    /// No revision is recorded. `measured` is what the checkout is on now (or
    /// `None` if even that is unknown) — printed so a human can record it.
    Unrecorded { measured: Option<String> },
    /// A revision is recorded but the checkout's own revision could not be read,
    /// so no comparison was possible.
    Unreadable { recorded: String },
}

/// Classify a recorded revision against a measured one.
///
/// Comparison is case-insensitive and **abbreviation-tolerant**: a human writing
/// `revision` by hand naturally pastes a short sha, so one value being a prefix of
/// the other counts as a match provided the shorter is at least
/// [`MIN_REVISION_PREFIX`] characters. A shorter fragment than that is not enough
/// evidence of identity and is treated as a difference — erring toward "the corpus
/// may have moved", which asks for a re-measure rather than silently blessing a
/// count.
///
/// `worktree` is carried onto a match and nowhere else: a revision that already
/// differs, or was never recorded, is inconclusive whatever the tree's cleanliness,
/// while a match is the one verdict cleanliness can overturn.
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

/// The line printed for a local project in the ordinary run — not only on RED. A
/// gate that speaks only when it is angry teaches nothing: the operator should see
/// what state the corpus was measured in on every green run too, so that the day it
/// moves, the previous run's output is already a record of where it moved from.
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
            "revision DIFFERS: the baseline was seeded at {recorded}, this run measured {measured}. The corpus moved under the baseline, so the count change may be CORPUS DRIFT rather than a regression — re-measure against the seeded revision to separate the two, then reseed consciously (the count in xtask/src/gate.rs, and `revision = \"{measured}\"` in corpus.local.toml)."
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

/// Which counter partition a finding routes into (ADR-0050 §9 / ADR-0053 §8). The
/// **layer** (read from the steins-infer registry) is the gate's partitioning
/// carrier, and this classification is the one place it is decided — exhaustive on
/// [`Layer`] so a new variant is a *compile error* here until its gate posture is
/// stated, never a silent fall-through into a counting bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateBucket {
    /// proof + mechanics (and any unregistered id, treated conservatively): gate
    /// **red on sight** (ADR-0013).
    RedOnSight,
    /// contract: **measurement mode** — reported and counted, gates only on a
    /// per-package increase past the seeded baseline (ADR-0050 §9).
    Measurement,
    /// **possibly-grade** proof ids — the `maybe-` siblings, the proof-layer rows
    /// the registry floors at `strict` (ADR-0081 §8). Measurement mode, on the same
    /// per-package increase tripwire as the contract families and for the same
    /// reason: the claim is *"some path"*, so a true finding on working code is the
    /// id's own yield rather than a defect the corpus is supposed to be free of,
    /// and the default profile a corpus user runs never shows one.
    ///
    /// This is where the zero-FP policy's FP tolerance is absorbed for these ids —
    /// not by omitting the check, and not by suppressing a finding, but by the
    /// `strict` opt-in the floor already encodes. A count that *grows* is still a
    /// regression and still reds.
    Tripwire,
    /// debug (ADR-0053 §8): requested introspection, **excluded from every counter**
    /// — not red-on-sight, not a tripwire, not `EXPECTED_PROOF_FINDINGS` material. A
    /// dump is not a finding. Vacuous today (no debug emitter until ADR-0053 D3/D4),
    /// so the gate output is byte-identical to the pre-dump run.
    Excluded,
}

/// Route a finding to its [`GateBucket`] by layer. Unregistered ids (no layer) are
/// conservatively red-on-sight, exactly as before. The `Layer::Debug` arm is what
/// keeps a future dump id out of every counter (ADR-0053 §8).
fn gate_bucket(d: &Diagnostic) -> GateBucket {
    match layer(d.id) {
        Some(Layer::Contract) => GateBucket::Measurement,
        Some(Layer::Debug) => GateBucket::Excluded,
        // A proof id floored at `strict` is a possibly-grade claim (ADR-0078 §1.3's
        // `maybe-` convention, mechanized): derived from the registry rather than
        // listed here, so a new `maybe-` sibling takes the right posture the day it
        // registers and no id list can drift from the floors.
        Some(Layer::Proof) if surface_floor(d.id) == Some(Floor::Strict) => {
            GateBucket::Tripwire
        }
        Some(Layer::Proof | Layer::Mechanics) | None => GateBucket::RedOnSight,
    }
}

/// Whether a diagnostic is **contract-layer** (ADR-0050 §9): measurement-mode
/// partitioning. A thin wrapper over [`gate_bucket`] so the layer decision lives in
/// exactly one exhaustive place.
fn is_contract(d: &Diagnostic) -> bool {
    gate_bucket(d) == GateBucket::Measurement
}

/// Whether a diagnostic is **debug-layer** (ADR-0053 §8): excluded from every gate
/// counter. Read from the same exhaustive [`gate_bucket`] partition.
fn is_debug(d: &Diagnostic) -> bool {
    gate_bucket(d) == GateBucket::Excluded
}

/// Whether a diagnostic is a **possibly-grade** proof finding (ADR-0081 §8) — the
/// `strict`-floored proof ids, counted against [`POSSIBLY_EXPECTED`] and gating
/// only on a per-package increase. One id table, not one per family: the three ids
/// share a posture, not a prefix.
fn is_possibly(d: &Diagnostic) -> bool {
    gate_bucket(d) == GateBucket::Tripwire
}

/// Whether a diagnostic is one of the measurement-mode `phpdoc.*` **contract** ids.
///
/// Selected by prefix **and** layer, the `is_effect_contract` shape. The family
/// stopped being layer-homogeneous with ADR-0078 §1.5 (issue #186): `phpdoc.*` now
/// carries the docblock-hygiene mechanics ids beside the contract ones, and a bare
/// prefix test would have counted a mechanics finding against `PHPDOC_EXPECTED`
/// *and* left it red-on-sight — double-counted, and its tripwire quietly absorbing
/// an anti-rot id the layer says must never be absorbed.
fn is_phpdoc(d: &Diagnostic) -> bool {
    d.id.starts_with("phpdoc.") && is_contract(d)
}

/// Whether a diagnostic is one of the measurement-mode `throw.*` contract ids
/// (ADR-0040) — the prefix keys its own count table (all `throw.*` are contract).
fn is_throw(d: &Diagnostic) -> bool {
    d.id.starts_with("throw.")
}

/// Whether a diagnostic is one of the `effect.*` **contract** ids
/// (`effect.envelope-exceeded` / `effect.liskov-widened`) — the ADR-0050 §9 delta
/// family. Selected by layer *and* prefix so `effect.unknown-label` (mechanics)
/// is excluded and stays red-on-sight.
fn is_effect_contract(d: &Diagnostic) -> bool {
    d.id.starts_with("effect.") && is_contract(d)
}

// untyped surface (ADR-0078, issue #200)
//
// A contract-layer id belonging to NONE of the three families above — which is
// exactly what `untyped.*` is — is **dropped by the `!is_contract` retain and
// counted nowhere**. Verified against this file when the family landed, and worth
// writing down because the reading is not obvious in either direction:
//
// * It CANNOT turn the gate red. The retain runs off [`gate_bucket`]'s layer
//   partition, not off any family prefix, so a contract id with no expected table
//   never joins the red-on-sight `diagnostics` list. This is the property that
//   matters, and it is a property of the layer — nothing about the family had to be
//   special-cased for it, unlike issue #186's `is_phpdoc` fix, where a MECHANICS id
//   was wrongly *absorbed* by a contract table (the opposite defect).
// * It is therefore also not *reported* by the gate: there is no `UNTYPED_EXPECTED`
//   row, no count column, no tripwire. Deliberate for now — a tripwire seeded before
//   the corpus measurement would pin an arbitrary number. When the measurement
//   decides the `iterable-value` / `generics` floors (ADR-0078's
//   `Contracts→Strict by measurement` rows), seeding a fourth table here beside
//   `PHPDOC_EXPECTED` / `THROW_EXPECTED` / `EFFECT_EXPECTED` is the one-family edit
//   that turns the family into an increase tripwire.
// end untyped surface (ADR-0078, issue #200)

/// Permanent gate policy for `phpdoc.*` findings (ADR-0030 relation #1).
///
/// `phpdoc.*` findings are **contract-layer** claims: they say a proven value does
/// not inhabit a *declared* `@param`/`@return` type under the no-coercion contract
/// relation. That is a statement about the code's own documentation, **not** a
/// runtime-breakage claim (`type.*`/`effect.*`, which gate red on sight per
/// ADR-0013). TRUE `phpdoc.*` findings legitimately exist in released, working
/// corpus code — a `@param int` that a test calls with the numeric string `"5"` is
/// a real declared-contract violation even though it runs fine — so they must
/// never flip the gate red merely by existing.
///
/// Instead the gate tracks their **count per package** against this deliberately
/// hand-maintained expected-count table and acts as a **regression tripwire**: a
/// package goes red only if its `phpdoc.*` count *increases* beyond the seeded
/// expectation (a genuine new finding, or a real regression in the checker),
/// while a *decrease* is a welcome improvement that never blocks. Update an entry
/// here consciously when a change to the checker legitimately moves a count.
///
/// Seeded with the post-assertion-exemption counts (the assertion-helper exemption
/// removed ~19 monorepo findings vs. the pre-exemption 352). Packages absent from
/// this table expect **zero** `phpdoc.*` findings.
///
/// **This table counts the `phpdoc.*` CONTRACT ids only** — see [`is_phpdoc`]. The
/// docblock-hygiene ids added by ADR-0078 / issue #186 share the prefix but carry
/// the mechanics layer, so they stay red-on-sight and are pinned individually in
/// [`EXPECTED_PROOF_FINDINGS`]; no count here moved when they landed.
/// `phpstan/phpstan-src` remains absent (measured 0 under the 2026-08-08 corpus
/// scoping recorded on its `THROW_EXPECTED` row).
const PHPDOC_EXPECTED: &[(&str, usize)] = &[
    // 19 → 21 (+2) with issue #327 (an array literal keeps its fact when its
    // elements do not). `ArtifactRepositoryTest` lines 45 and 68 build
    // `['type' => 'artifact', 'url' => __DIR__ . '/Fixtures/artifacts']` and pass
    // it to `ArtifactRepository::__construct(@param array{url: string})`. The
    // undeclared key `type` under that sealed shape is a TRUE contract violation
    // — the docblock omits it — and it is the SAME finding the same file's line
    // 79 already carried on the baseline, where the array is written with a
    // literal url. Those two sites were silent only because `__DIR__ . '…'` is
    // an unproven element, which used to drop the whole argument's fact along
    // with the keys; now the keys survive it and the judgment happens.
    //
    // Triaged verbatim, not reseeded blind: the unknown `url` slot is NOT what
    // fires. Measured on a fixture — `['url' => <unknown>]` against the same
    // `array{url: string}` stays SILENT (an unknown slot is `Maybe`, and Maybe
    // is silence), while the extra key fires with the slot proven or not.
    ("composer/composer", 21),
    ("sebastianbergmann/phpunit", 8),
    // 4 → 5 (+1), 2026-08-14, with ADR-0056 §8: `resource` stopped being an
    // unmodeled spelling and became a relation. `StreamHandlerTest`'s
    // `testWriteMissingResource` constructs `new StreamHandler(null)` against
    // `@param resource|string $stream` and wraps it in
    // `expectException(\LogicException::class)` — the test exists precisely
    // because the value is invalid. `null` inhabits neither arm, so it is a TRUE
    // no-coercion violation; it was silent only because `resource` lowered to an
    // opaque `Maybe` that swallowed the union's verdict. The exact shape the
    // flysystem and symfony/console entries below already record: a deliberate
    // negative-test call site the contract layer can now read.
    ("Seldaek/monolog", 5),
    // 1 → 2 (+1) with ADR-0043 stage 4 (phpdoc-side class contracts). The new
    // finding is a class-value contract: `new MountManager(['valid' => 'something
    // else'])` — a plain string in the `array<string, FilesystemOperator>` value
    // position — inside a `guarding_against_mounting_invalid_filesystems` test that
    // wraps it in `expectException(UnableToMountFilesystem::class)` and carries
    // `@phpstan-ignore-next-line`. A TRUE no-coercion violation the test documents.
    ("thephpleague/flysystem", 2),
    // 0 → 1 (+1) with ADR-0043 stage 4. `ChoiceQuestionTest` passes a literal array
    // `[..., null]` to `ChoiceQuestion::__construct(@param array<string|bool|int|
    // float|\Stringable> $choices)`; `null` is a member of none of the union arms —
    // a TRUE no-coercion contract violation (the docblock omits null). The sibling
    // `StringChoice` (a `__toString` object, implicit `\Stringable`) is correctly
    // *accepted*, not a finding — the is-a oracle honors the implicit interface.
    // 1 → 2 (+1), 2026-08-14, with ADR-0056 §8 — the monolog entry above's twin,
    // and the pair is the point: `StreamOutputTest` line 45 passes the literal
    // `"foo"` to `StreamOutput::__construct(@param resource $stream)` in a test
    // whose whole purpose is to assert the constructor rejects a non-resource.
    // A string is not a resource in any PHP mode, so TRUE.
    //
    // Two packages, two findings, and NOTHING else across 100,530 files: the
    // resource-VALUE half of the slice (a narrowed `fopen()` handle reaching a
    // typed parameter) fires nowhere in the corpus at all. That is the expected
    // shape rather than a disappointment — legacy PHP that still uses resources
    // is legacy PHP that does not type its parameters — and it is the soundness
    // signal too, since a wrong producer row would have lit up the well-typed
    // OSS packages first.
    ("symfony/console", 2),
    // 0 → 15 (+15) with ADR-0043 stage 4. Every finding is a deliberate
    // negative-test call site (`expectException(\LogicException::class)` /
    // `\PhpParser\...`) passing a wrong-typed argument to a class-typed `@param`:
    // `new Name()` vs `(string|Identifier|Expr)` (Name is-a-No either), scalar `1`
    // /`"test"` vs `(Node|Builder)` / `(string|Identifier)`, `new stdClass()` vs a
    // `\UnitEnum`-bearing union. All in `test/PhpParser/Builder*Test.php` and
    // `NodeDumperTest.php`; each asserts the runtime `LogicException` that the
    // phpdoc contract predicts — TRUE, released, working test code.
    ("nikic/PHP-Parser", 15),
    // The private monorepo (corpus.local.toml); matched by its local project name.
    // 333 → 357 (+24) with ADR-0031 branch-sensitive analysis: the structured `if`
    // walk, ternary values, and positive refinement now reach proven values that
    // were previously buried inside `Opaque` control-flow blocks, so the phpdoc
    // contract layer sees more of them.
    //
    // 357 → 404 (+47) with the ADR-0035 "refined layer goes live" milestone: the
    // env now stores the four-layer `steins_domain::Fact`, and three new sound
    // inference sources feed the contract layer — native-type parameter *seeding*
    // (`int $x` ⇒ `General{Int}`), guard *refinements* that produce Refined/General
    // facts (`$n > 0` ⇒ positive-int, `$s !== ''` ⇒ non-empty-string), and
    // `@phpstan-assert` *application* — checked via `steins_contract::admits_fact`
    // (only a definite `No` reports). 8 of the increase are the new abstract-fact
    // findings (a seeded/refined scalar flowing into an incompatible `@param`, e.g.
    // positive-int → `@param string`, non-empty-string → `@param int`, int →
    // `@param string`); the rest are concrete values the richer propagation now
    // reaches. All sampled increases are TRUE no-coercion contract violations in
    // released test code, never runtime findings — the runtime gate stays GREEN.
    // Class-shaped `@param`s are held silent against scalar facts (template safety),
    // so no template FPs. Baseline moved deliberately per ADR-0030/0035.
    //
    // 404 → 405 (+1) with the ADR-0036 object-state milestone: the new
    // `phpdoc.property-mismatch` check (a proven/abstract value assigned to a
    // property whose `@var` contract definitely rejects it). The single pxxxx
    // increase is a TRUE finding — a model class's `$id` property is `@var
    // numeric-string`, and a test assigns an int literal to it (a value that
    // is not a numeric *string*); PHPStan flags the identical `assign.propertyType`.
    // Property checks run only in the plain per-scope pass (never under a binding
    // descent, whose caller values in-body guards would narrow), so the descent-bound
    // guard-blind candidates seen mid-development do not reach the gate.
    //
    // 405 → 439 (+34) with ADR-0043 stage 4 (phpdoc-side class contracts + the
    // enum-case/class-const value resolution that feeds them). The delta was
    // baseline-diffed (a HEAD worktree) and triaged verbatim; all 34 net-new (36
    // added, 2 pre-existing FPs removed) are TRUE:
    //   - class-const string args vs `@param int`/`int[]` (a DAO's `TYPE_*`
    //     consts holding `"3"`-style numeric strings into `int`; a const list of
    //     numeric-string ids into `int[]`) — the stringly-typed DB-illusion
    //     pattern (ADR-0037), now that class-const args resolve to their literals.
    //   - proven scalars/objects vs a class-typed contract: a service-name string
    //     vs an enum param, an int literal vs a `SomeInterface|false` union,
    //     a float literal (an `@phpstan-ignore` intentional wrong type) vs a
    //     scalar|`BackedEnum` union, a prose string literal vs a `list<Model>`
    //     param, `null` assigned to a property whose `@var` names a PDO
    //     wrapper class on `disconnect()`.
    //   - sealed array-shape violations surfaced once a value became provable (its
    //     class-const/`::class`/enum elements now resolve): two finder methods'
    //     options arrays carrying a key their `@param array{…}` omits; a
    //     data-provider `expected => SomeException::class` (a *string*) where
    //     the `@return array<…, array<untyped>>` wants an array; a
    //     metadata-defaults const carrying an extra key vs its `@return array{…}`.
    // The 2 removed are pre-existing FPs the stage cleared: an unresolved const-fetch
    // *type* (`SomeClass::LIST_*`) no longer manufactures a No against an array
    // value (const-fetch types are silent), and a `[]`-vs-`non-empty-list` finding a
    // `count()===0`-guarded value could never actually reach. Runtime layer GREEN.
    //
    // 439 → 434 (−5), 2026-07-24 evening: LIVE-TREE DRIFT, not a checker change —
    // the unpinned monorepo checkout gained ~210 files during the day and some
    // previously-counted finding sites changed. Decrease adopted consciously
    // (a decrease never gates; recorded so the next reader knows the cause).
    //
    // 434 → 477 (+43) with ADR-0056 R1 (builtin return facts): a uniquely-resolved
    // builtin call now seeds its REFLECTED RETURN ENVELOPE into the value domain
    // (`trim()`/`substr()`/… ⇒ `General{String}`), the runtime's own
    // `getReturnType()` — so a request string read as
    // `trim(ParamHelper::…->asString())` and passed to a method's `@param int`
    // becomes a proven contract mismatch the phpdoc layer now sees. All 43 were
    // baseline-diffed (a stashed HEAD) and triaged verbatim: every one is a
    // `string`/`non-empty-string` value → `@param int` in a request-handling
    // controller (AJAX/Rpc/admin htdocs), PHPStan flags each identically — the
    // stringly-typed request-param → int-param pattern (same class the +47/ADR-0035
    // note above already recorded). Two render `non-empty-string`: the seeded
    // `General{String}` refined by an existing `=== ''` guard before the call — the
    // envelope composing correctly with narrowing. 0 findings disappeared; every
    // OSS package is unchanged (the soundness signal — a wrong envelope would light
    // up well-typed OSS too). Runtime/proof layer stays GREEN (0). throw.* is
    // unmoved by this change (44563 before and after).
    // 477 → 487 (+10), 2026-07-29 with DR2 (is_* guard narrowing, ADR-0064
    // seam v): the by-ref exemption now lets a request-param string's fact
    // SURVIVE a pure `is_numeric`/`is_array` guard instead of being
    // forgotten, so the guarded value reaches the call and the phpdoc layer
    // judges it. All 10 were baseline-diffed (set-diff vs the pre-slice
    // build; 0 disappeared) and triaged verbatim: every one is the standing
    // stringly-typed request-param idiom — `is_numeric($x)`-guarded
    // string/numeric-string handed to a `@param int`/`int[]` method
    // (`is_numeric` proves numeric-STRING-ness, not int-ness). PHPStan
    // reports each identically at level 6+. Every OSS package is unchanged
    // (the soundness signal); proof layer stays 0 — one proof-layer FP the
    // slice initially introduced (a refuting guard leaving a stale
    // Singleton premise on an unreachable branch) was found by this same
    // gate and fixed in-slice: a refuted fact now DROPS.
    // 487 → 497 (+10), 2026-08-02: the ADR-0072 designed unlock (shape
    // facts judged against contracts — the acceptance relation's third
    // face). Baseline-diffed against the pre-slice build: exactly 10 new,
    // 0 disappeared. Triaged verbatim, 10/10 TRUE, one class: a sealed
    // `array{…}` @param that under-declares keys its call sites provably
    // always pass (both docblock declarations read at source for the
    // sampled pair — the callee's four-key sealed shape against a caller
    // contract carrying six required keys), plus one plural-records-vs-
    // singular-record annotation where presence and value obligations
    // fail together. PHPStan reports the identical class for sealed
    // shapes. Every OSS package unchanged (the CI fp-gate stayed green on
    // the same commit); proof layer 0 throughout; nsrt LOST 0.
    // 497 → 498 (+1), 2026-08-02, with ADR-0073 (inline `@var` cast seeding,
    // PR #121): the new finding is the SAME sealed-array-shape class the
    // +10/ADR-0072 entry above already recorded, surfaced by a different
    // path — a controller's statement-level `/** @var array{9 keys} */`
    // cast now seeds the full shape it names, and the very next statement
    // hands that value to a sibling class's static setter whose declared
    // `@param array{…}` is sealed at 8 keys — one key short of the cast.
    // TRUE: read both docblocks at source (not just the finding text) —
    // the setter's contract genuinely under-declares what its one caller
    // (the inline cast says so explicitly) always passes. Every OSS
    // package unchanged; proof layer 0; the gate is GREEN again at 498.
    // 498 → 499 (+1), 2026-08-05: CORPUS STATE, not an engine change, and
    // attributed as such two ways rather than by triaging the finding.
    // (a) Every public package's count is EXACT in the same run — a checker
    //     change that manufactured a phpdoc finding would light up
    //     well-typed OSS too, and none moved. (b) Running the gate at the
    //     very commit where 498 was seeded GREEN, against today's local
    //     checkout, reproduces 499 exactly — so the movement is on the
    //     corpus side of the measurement by construction, with Steins held
    //     fixed. The local checkout was on a feature branch when 498 was
    //     seeded and has since advanced to its own master plus two pulls:
    //     813 files changed, 350 of them PHP.
    //     NOT triaged finding-by-finding, and this entry does not claim to
    //     be: identifying which finding is new needs the previous corpus
    //     state, which nobody retained. That is exactly the gap the
    //     `revision` record in the paired commit closes going forward.
    // 499 → 500 (+1), 2026-08-05, with ADR-0077 (out-parameter fact seeding,
    // PR #152): the designed unlock, and the first finding this capability
    // could ever have produced. A capture-group element read after a guard
    // that proved the match happened now carries `string` where it carried
    // nothing at all before, and that read is handed straight to a method
    // whose docblock declares an int parameter — so the annotation and the
    // value genuinely disagree. TRUE, triaged by reading the site rather
    // than inferred from the count: the group is a digit class, which is
    // what makes the annotation look plausible and is exactly no defence —
    // PCRE hands back a string whatever the group matched. It is the same
    // stringly-typed-value → `@param int` class the +43/ADR-0056 and
    // +10/DR2 entries above already recorded, reached by a new path. Every
    // OSS package is unchanged (the CI fp-gate is green on this same
    // commit — the soundness signal); proof layer 0; `throw.*` unmoved at
    // its own baseline, so `THROW_EXPECTED` does not move with this.
    // **Reseed 2026-08-09, 500 → 507, and the corpus pin moves with it**
    // (`565b106a…` → `5b026671…`, the two-file discipline). The gate had
    // printed RED on this entry for two sessions, which is the failure mode
    // a stale baseline actually has: a standing red trains the reader to
    // skim past it, and the next real regression arrives into an alarm
    // nobody reads.
    //
    // Separating drift from regression by re-measuring at the seeded
    // revision was not possible — the corpus checkout is 9.8 GB and the
    // machine had 2.1 GB free — so the accounting is indirect and is
    // recorded here rather than implied. Of the 316 files carrying any
    // finding in this run, **5 moved between the seeded revision and the
    // measured one** (`Illust/Common.php`, `Novel/BookGenerator.php`,
    // `Twitter/CardHelper.php`, `User/Common.php`,
    // `User/LoginValidator.php`), and those five carry 14 findings — a
    // channel wide enough to account for a +7 delta several times over,
    // though not a proof that it did. What *is* proven: the proof layer
    // stayed at 0 across every measurement this session, and each
    // analyzer-side movement was measured back-to-back at one checkout and
    // triaged (issue #272 alone removed 4 findings of a false-positive
    // class and added 3 verified-true declared-contract debts, 508 → 507).
    // 507 → 513 (+6), 2026-08-09, with issue #288's second half: a project
    // call's DECLARED RETURN SHAPE now seeds the caller's value lane, the
    // return-side mirror of the `@param` seeding ADR-0062 S3 already did —
    // so `$x = f();` carries `f()`'s `@return array{…}` where it previously
    // carried only an arm nothing could project. Baseline-diffed against
    // the pre-slice build at one checkout: exactly 6 new, 0 disappeared,
    // and every OSS package is unchanged (the soundness signal). All 6
    // triaged by reading BOTH docblocks at source, 6/6 TRUE, and all one
    // class — the sealed-shape under-declaration the +10/ADR-0072 and
    // +1/ADR-0073 entries above already recorded, reached by the new path:
    //   - 1 site: an HTTP wrapper whose `@return` is a three-key sealed
    //     shape (status/headers/body) handed to a queue factory whose
    //     `@param` seals at two (status/body) — the `headers` key is
    //     genuinely outside the declared parameter.
    //   - 3 sites (same callee, three callers): a ranking helper's
    //     `@return array[]` — a list of records — handed to a private
    //     method whose `@param` spells ONE record's seven-key shape while
    //     its own prose calls the parameter a list. The plural-vs-singular
    //     annotation defect the +10/ADR-0072 entry names, now with the
    //     producing side declared too.
    //   - 2 sites (same callee, two callers): an options builder whose
    //     `@return` seals ELEVEN keys, handed to a post-filter whose
    //     `@param` seals the same ten minus `fields` — the one key the
    //     caller provably always passes, and which the caller itself reads
    //     off the same value two lines earlier.
    // PHPStan reports the identical class for sealed shapes. Proof layer 0
    // throughout; `throw.*` unmoved at its own baseline (44107), so
    // `THROW_EXPECTED` does not move with this; possibly-grade unmoved
    // (150); nsrt admissible unmoved (2817).
    //
    // **513 -> 528 (+15) with issue #293**, template bounds read as
    // upper-bound contracts. Measured on the branch rebased onto the #288
    // merge, so the two waves' findings are disjoint by measurement rather
    // than by assumption. Every one is `phpdoc.param-mismatch` and every one
    // was read at its source:
    //
    // - **12 sites** call an assertion helper declared
    //   `@template T of list<string>` with `@phpstan-param T $collection`,
    //   passing lists that hold `null` or ints — `[null, 'show', 0, 'hide',
    //   1]` and `[0, 1]` among them. The helper's own bound says what it
    //   accepts; these callers do not meet it.
    // - **2 sites** call the `list<int>` sister with the wrong shape: one
    //   passes constants that are literally the strings `'0'`/`'1'`, the
    //   other a string-keyed map where a list is declared.
    // - **1 site is a genuine defect rather than a loose annotation**: it
    //   asserts *string* membership over a constant that a sibling call site
    //   asserts *int* membership over, and the helper's
    //   `@phpstan-assert value-of<T>` then narrows the value to `string`
    //   against an `@param int` on the very next call. Two of the fifteen
    //   would disappear if that were fixed upstream; the baseline is seeded
    //   at what the corpus says today, not at what it should say.
    //
    // Nothing else moved: proof layer 0, `throw.*` 44107 = baseline,
    // possibly-grade 150 = baseline, every OSS package byte-identical.
    //
    // **528 -> 526 (-2), 2026-08-09, and the corpus owner did this, not the
    // analyzer.** The +15 entry above named one site as a genuine defect
    // rather than a loose annotation — a constant asserted as holding
    // strings at one call site and ints at its sibling — and predicted that
    // fixing it upstream would take two findings with it. It was fixed, and
    // exactly two went. The prediction landing on the nose is the reason
    // this reseed is a one-line edit rather than a re-triage: the remaining
    // 526 are the same rows, read at source, that the entry above records.
    //
    // A drop never trips the tripwire, so this buys no alarm. It buys the
    // entry back its meaning, which is the whole argument of the 2026-08-09
    // reseed two entries up: a baseline above the truth swallows exactly
    // that many real regressions in silence.
    //
    // **526 -> 536 (+10), 2026-08-15, and it is two mechanisms, not one.**
    // Measured as a finding-level diff rather than a count: the 2026-08-09 seed
    // commit (86df4c6) re-run today still reports exactly 526 on the same corpus
    // checkout, so the delta below is the analyzer's and nothing else's. Twelve
    // rows are new, two are the *same two sites* with a refined rendering
    // (`mixed` -> `int|string` inside a shape, two sites in one helper, counted
    // before and after alike) — twelve minus those two is the +10. Every one of
    // the twelve was read at its source:
    //
    // - **6 sites: an undeclared key under a sealed shape**, the exact class the
    //   `composer/composer` entry at the top of this table records, arriving here
    //   from the same array-literal work (issue #327: a literal keeps its fact when
    //   its elements do not). Two hand a two-key literal to a `@phpstan-param`
    //   sealing one optional key, the second key nowhere in the declaration; one
    //   hands an extra key to a helper whose `@param` seals six other names; three
    //   hand a **list of records to a `@param string[]`**, at three more sites of a
    //   call whose fourth site was already counted in the 526. TRUE, and PHPStan
    //   reports the identical class for sealed shapes.
    // - **4 sites: a union the value domain could not hold until ADR-0085.**
    //   Two request handlers (three sites in one, one in the other) read a request
    //   parameter through a `trim()` over a parameter-helper chain and hand the
    //   result to `@param int` / `@param float`. The value is a `string` at runtime
    //   — `trim` returns one whatever it is given — so this is the numeric-string
    //   case this table's own doc comment names as the archetypal TRUE
    //   declared-contract violation. It was silent because a `string|bool` had no
    //   carrier: the single-base wall meant the argument fell back to an
    //   unjudgeable `mixed`, and Maybe is silence. `Fact::Union` gave it one, which
    //   is also why the two re-rendered rows above move their spelling and not
    //   their count.
    //
    // The corpus is byte-identical across the move (`corpus.local.toml`'s pin plus
    // an overlay whose mtimes all predate the 2026-08-09 seed), `throw.*` unmoved
    // at 43886 = baseline, possibly-grade unmoved at its own row, every OSS package
    // at its own expectation.
    //
    // Read the attribution beside [`EFFECT_EXPECTED`]'s row before reading a delay
    // into this one. Both families' fallout landed days before this reseed and
    // waited for a gate run with the private corpus mounted: `corpus.local.toml` is
    // gitignored, so the agent worktrees that landed #303, #327 and #341 all
    // measured the public packages only. That is a hole in the workflow, not in the
    // analyzer, and it is why this entry can cite mechanisms from three separate
    // merges at once.
    ("pxxxx-monorepo", 536),
];

/// The expected `phpdoc.*` count for a package/local-project name (0 if untabled).
fn phpdoc_expected(name: &str) -> usize {
    PHPDOC_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for `throw.*` findings (ADR-0040/0007), identical in
/// spirit to [`PHPDOC_EXPECTED`]: an undeclared **checked** throw escaping a
/// written `@throws`, or a Liskov-widened override, is a real contract-layer
/// claim about the code's own documentation — not a runtime-breakage proof. Such
/// findings legitimately saturate working code (the very checked-exception volume
/// ADR-0007 keeps quiet by default), so they are held in measurement mode and
/// gate only as a per-package **increase** tripwire.
///
/// Seeded from the first landing run of the throw system (ADR-0040). The
/// monorepo count is dominated by two pervasive base exceptions
/// (an assertion-failure base and the app-wide base exception) thrown far below `@throws`-
/// annotated controllers — all TRUE undeclared-checked-throw findings, none
/// runtime breakage. Update an entry consciously when a checker change moves a
/// count. Packages absent expect **zero**.
///
/// The pxxxx count rose 35614 → 43963 with the closure wave (ADR-0033): throws
/// now propagate through higher-order-builtin callbacks (`array_map(closure, …)`)
/// and body-local `$fn()` closures that were previously opaque taints. Triaged
/// (5-sample, verbatim): every new finding is a TRUE undeclared-checked-throw —
/// exclusively the two pervasive base exceptions reached through a real
/// callback edge (e.g. a controller method with `@throws ErrorException`
/// calling `array_map` over a closure whose callee throws the app-wide base
/// exception). No FP: the by-ref-invalidation guard keeps the
/// local `$fn()` resolution sound, and the public corpus packages are unmoved.
// Reconciled to actual after the closure-wave Stage D (interface/parent @throws
// Liskov + `implements` lowering). The moves were triaged and are deterministic:
// the increases are new `throw.liskov-widened` findings (phpunit +4, pxxxx +1 —
// e.g. JsonMatches::fail declares InvalidJsonException while the abstraction
// Constraint::fail declares only ExpectationFailedException: a true widening),
// and the decreases (symfony/console 12→10, nikic 2→1) are `undeclared` counts
// that dropped because lowering `implements` enriched the class chain, letting
// throw subtype/absorption checks resolve where they previously widened.
const THROW_EXPECTED: &[(&str, usize)] = &[
    ("composer/composer", 93),
    ("sebastianbergmann/phpunit", 84),
    ("guzzle/guzzle", 2),
    ("Seldaek/monolog", 7),
    ("symfony/console", 10),
    ("thephpleague/flysystem", 3),
    ("nikic/PHP-Parser", 1),
    // Registered 2026-07-24 (v0.1.0 run, oracle idea A): PHPStan's own src/ as a
    // local corpus project (tests/, e2e/ excluded — deliberately-broken fixtures).
    // First run: 0 proof-layer, 0 phpdoc.*, 20 throw.undeclared. Triaged verbatim
    // (5+ samples): every finding is a TRUE undeclared checked throw escaping a
    // `@throws`-annotated declaration — e.g. FileCacheStorage::save() (@throws
    // DirectoryCreatorException) throws ShouldNotHappenException directly at :81;
    // CommandHelper::begin() (@throws InceptionNotSuccessfulException) throws
    // ShouldNotHappenException/:162 and reaches ServiceCreationException origins;
    // FileReader::read()'s CouldNotReadFileException escapes FixerApplication's
    // @throws-annotated methods. Homogeneous checked-exception debt, none runtime
    // breakage — the exact ADR-0040/0007 pattern the tripwire mode exists for.
    // 20 -> 21, 2026-07-26: the local checkout advanced (192 src/ files changed),
    // rewriting ValidateServiceTagsExtension onto the attribute collector. The new
    // `getInterfaceTagMapping()` throws ShouldNotHappenException ("Interface %s
    // claims multiple tags") and `beforeCompile()` — which calls it and declares
    // only @throws MissingImplementedInterfaceInServiceWithTagException — lets it
    // escape. Triaged verbatim against the pre-advance tree: that file held exactly
    // one such throw before and holds two now, and the other 20 findings are
    // unchanged in file, line and escaping method. A TRUE undeclared checked throw
    // of the same homogeneous shape as the seeded 20, caught transitively (the
    // throw is in the helper, the escape is attributed to the annotated caller),
    // which is what the ADR-0040 damming machinery is for.
    //
    // 2026-08-08 (#186): this project's corpus scope moved from an `exclude`
    // denylist onto the new positive `paths = ["src", "vendor"]` key in
    // corpus.local.toml — the recorded decision that PHPStan's `tests/` tree is
    // that project's own rule-fixture corpus, i.e. INPUTS written to be broken,
    // and so outside an FP gate whose bar is zero false positives on code that
    // works (the ADR-0079 §2.3 presumption for parser fixtures, applied to a
    // whole tree). Re-measured under the scoping in the same run: 21, and
    // `phpdoc.*` 0 — both UNCHANGED, because the denylist already named the same
    // directories. The key buys enforcement, not a different corpus: an allowlist
    // cannot silently readmit a fixture tree added later, and a denylist can.
    ("phpstan/phpstan-src", 21),
    // 43964 → 44372 (+408), 2026-07-24 evening: LIVE-TREE DRIFT — the unpinned
    // monorepo checkout gained ~210 files (84,038 → 84,248) during the day.
    // Triaged (3-sample verbatim, gate printout): every sampled new finding is
    // the standing homogeneous debt class — an `@throws`-annotated declaration
    // with an undeclared base-exception escape (the app-wide base exception,
    // Exception_MethodNotAllowed, a bare Throwable) — TRUE contract findings on
    // newly-landed application code, none runtime breakage. The proof layer
    // stayed at ZERO over the new files. Reseeded consciously.
    // 44372 → 44343 (−29), 2026-08-01: the standing DOWNWARD live-tree drift,
    // reseeded in its own pass (never inside a fix commit). The −29 has been
    // observed unchanged across every session and checker commit since
    // 2026-07-25 — cross-commit stability plus the #63 determinism fix rule
    // out a checker-side cause; the monorepo working tree simply moved under
    // the table (the same unpinned checkout the +408 entry above documents).
    // Today's run: 44,343 = 44,342 undeclared + 1 liskov, phpdoc EXACT and
    // proof-layer 0 alongside — a corpus change, not a behavior change. The
    // gate trips on INCREASE only, so this entry existed as a permanent
    // "below expected" nudge; this reseed retires it.
    // 44343 → 44374 (+31), 2026-08-05: the same corpus-state movement the
    // PHPDOC_EXPECTED entry for this project records, in the same run and
    // established the same two ways — every public package's throw.* count
    // is EXACT, and the gate run at the commit where 44343 was seeded GREEN
    // reproduces 44374 against today's checkout with the engine held fixed.
    // A +31 across a corpus delta of 813 files (350 PHP) is the standing
    // homogeneous checked-exception debt arriving with new application
    // code, the same class every reseed of this entry has recorded. NOT
    // triaged finding-by-finding — at this volume the only honest evidence
    // is the attribution above, and a finding-level diff would need the
    // previous corpus state, which was not retained.
    // **Reseed 2026-08-09, 44374 → 43886 (−488), downward**, with the corpus
    // pin. This one has a direct attribution rather than an inferred one:
    // the measured revision carries a commit that replaces a bespoke
    // exception class with concrete assertions across the tree, and an
    // assertion where a `throw` used to be is exactly one `throw.undeclared`
    // fewer. A downward move never trips the tripwire, so this reseed buys
    // no new alarm — it buys the entry back its meaning, since a baseline
    // 488 above the truth would swallow the next 488 real regressions in
    // silence.
    ("pxxxx-monorepo", 43886),
];

/// The expected `throw.*` count for a package/local-project name (0 if untabled).
fn throw_expected(name: &str) -> usize {
    THROW_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for the `effect.*` **contract** ids
/// (`effect.envelope-exceeded`, `effect.liskov-widened`, and since issue #311
/// `effect.interop-unknown-label`), the ADR-0050 §9 recorded delta. These moved off
/// the runtime red-on-sight path onto the identical per-package **increase**
/// tripwire as [`PHPDOC_EXPECTED`] / [`THROW_EXPECTED`], matching their
/// declared-contract semantics (a proven behavior exceeds an envelope the code
/// *declares* about itself; the program still runs).
///
/// Seeded **empty** in 2026-08, on the reading that no ADR-0006 effect envelope
/// exists in the pinned corpus or the legacy monorepo, so every package expects
/// **zero** (absent = 0). That reading was about *Steins* annotations, and the
/// row below is what it missed: the third id is the first of the three that can
/// fire on code carrying no Steins annotation at all — it reads upstream's
/// `@phpstan-impure` — and issue #303 gave the other two the same reach by
/// teaching the scanner `@phpstan-all-methods-pure`. An interop tag is an
/// envelope, so "no envelopes in the corpus" stopped being true the day the
/// corpus was read for upstream's spelling rather than ours. Update an entry
/// here consciously when a checker change legitimately moves a count.
const EFFECT_EXPECTED: &[(&str, usize)] = &[
    // **0 → 4442, 2026-08-15 — the first package to seed this table, and it
    // carries no Steins annotation.** The private monorepo declares
    // `@phpstan-all-methods-pure` on three classes, and the interop-envelope run
    // (issue #303, master 7b3ecab, 2026-08-12) taught the docblock scanner to read
    // that tag as an operative bound. Measured, not inferred: a binary built at
    // 93eff42 (the merge before #303) reports **0** effect findings over the
    // 5041-file subtree that holds all 4442 of them, and one built at 7b3ecab
    // reports **4442** — the whole count arrives in one merge.
    //
    // Attribution matters here because the first reading of this RED blamed the
    // wrong interval, the resource-value run (#341). It is not that run: the
    // findings over that same subtree are **byte-identical on both sides of it**
    // (27,332 = 27,332, nothing added, nothing removed), and re-running the gate
    // at 040658c — the commit whose record says green at 526/0 — reproduces
    // 536/4442 today. The record was wrong rather than the analyzer: this is the
    // first gate run since 2026-08-12 with the private corpus mounted at all.
    // `corpus.local.toml` is gitignored, so an agent worktree measures the public
    // packages only, and #303's fallout sat here unmeasured for three days.
    //
    // **One root, one shape.** Names below are **placeholders** — the private
    // corpus's own identifiers are not written into this repo, so do not grep for
    // these. Every finding sits inside one of the three declaring classes — a
    // generated URL builder `MyApp\Route\UrlBuilder` (4376), a validation wrapper
    // `MyApp\Util\Validator` (54), a filter wrapper `MyApp\Util\Filter` (12) —
    // across 795 call sites in 555 declared-pure methods, and every one of them
    // reaches the same house logger `MyApp\Log\Debug` down the same chain: an
    // assertion helper `MyApp\Util\Assertions` delegates to `Validator`, which
    // delegates to `Filter`, whose `true`-rejecting arm logs. A site carries one
    // finding per (label, origin) group — six for most of them, which is where 795
    // sites become 4442 rows.
    //
    // TRUE, and the corpus says so in its own source: that logging call carries a
    // `@phpstan-ignore impure.methodCall`, i.e. upstream's analyzer reports the
    // same violation at the same line and the file suppresses it there. PHPStan
    // stops at that suppression and reads the callee as pure for everyone above
    // it; the proven lane does not (ADR-0067 — a declaration never manufactures a
    // finding, and never erases one), so the effect keeps travelling to the
    // callers that declare purity over it. The propagated rows are the same fact
    // stated where it is still unfixed.
    //
    // What the count is made of, so a later reader knows before deciding whether a
    // move is a regression:
    //   * 2148 `nondet.time` — a datetime utility (`MyApp\Util\Clock`) wrapping
    //     `\time`/`\date`.
    //   * 716 `nondet.random` — `mt_rand`, the log's own sampling gate.
    //   * 716 `global.read` — a `getenv` on the CLI branch of a server-name lookup.
    //   * 86 `io` — a `file_exists` on a language file.
    //   * 776 `io.output.buffer` — **the one soft class.** `print_r($data, true)`
    //     and `var_export($x, true)` are pure in return-mode, and the source proves
    //     the `true` at each of the three origin sites; the label comes from this
    //     catalog's deliberately arg-blind row, which `effect_labels`'s own doc
    //     already records as an over-approximation. The root fix is a sibling of
    //     `narrowed_stream_labels` — a call site that proves its argument narrows
    //     the label — and it will move this row DOWN, which never trips the
    //     tripwire.
    //
    // Seeding rather than fixing is the zero-FP posture, not a retreat from it.
    // The envelope violations are real; what makes 4442 of them is a house logger
    // three inference hops under an assertion helper, which is the shape ADR-0084's
    // `[effects]` attribution exists to discharge on the corpus owner's side. Until
    // that lands, this row's job is to notice the 4443rd.
    ("pxxxx-monorepo", 4442),
];

/// The expected `effect.*`-contract count for a package/local-project name (0 if
/// untabled — the all-zero seed).
fn effect_expected(name: &str) -> usize {
    EFFECT_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for the **possibly-grade** proof ids (ADR-0081 §8):
/// `variable.maybe-undefined`, `property.maybe-undefined` and
/// `type.return-maybe-missing`, selected by floor rather than by name (see
/// [`gate_bucket`]). Same per-package **increase** tripwire as
/// [`PHPDOC_EXPECTED`] / [`THROW_EXPECTED`] / [`EFFECT_EXPECTED`].
///
/// Why a count and not per-finding pins. A definite proof id claims the program
/// breaks, so its corpus count belongs at zero and each exception is triaged
/// verbatim into [`EXPECTED_PROOF_FINDINGS`]. A possibly-grade id claims only that
/// *a* path reaches the site — the shape defensive house styles produce on purpose,
/// and the reason the registry floors these at `strict` where no default-profile
/// run shows them. Pinning each site individually would have meant hundreds of rows
/// asserting that working code is working, and the twelve
/// `type.return-maybe-missing` rows this table absorbed were already that shape.
///
/// Seeded 2026-08-09 from the run that landed the binding-presence pass, against
/// the corpus revisions recorded in `corpus.lock.toml` / `corpus.local.toml`.
/// **A count that grows is a regression and reds the gate**; a count that shrinks
/// is reported and the baseline updated consciously. Packages absent from this
/// table expect **zero**.
/// The five classes the 2026-08-09 triage found, so a future reader knows what a
/// count is made of before deciding whether a change to it is a regression:
///
/// 1. **Genuinely conditional binding** (`if`/`elseif` with no `else`, a name bound
///    only inside one arm) — TRUE, and the id's whole point.
/// 2. **A loop variable read after its loop** (`foreach (…) {…} use($i);`) — TRUE;
///    the zero-iteration path really does reach it unbound.
/// 3. **Correlated conditions** — bound under `if (count($c) > 1)` and read under a
///    second, textually identical `if (count($c) > 1)` whose body reassigns `$c`
///    in between (symfony/console `Application.php:827`). TRUE under the syntactic
///    control-flow reading this id is defined over; proving the two conditions
///    agree is path feasibility, which no reachability analysis here attempts.
/// 4. **A never-returning callee** (`$this->fail()`, `self::fail()`,
///    `markTestSkipped()`, `exitWithErrorMessage()`) — FALSE. `stmt_end` reads a
///    statement-position call as falling through because deciding otherwise needs
///    the project index, and ADR-0081 §9 defers the refinement to the emitter side
///    where the index lives. The six symfony/process rows, both Carbon rows and the
///    phpunit row are this class.
/// 5. **A binding in an argument position of a throwing call inside `try`**
///    (`$app->find($name = 'x');` then `$name` read in the `catch`) — FALSE. PHP
///    evaluates arguments before entering the callee, so the binding is done before
///    anything can throw; the pass weakens at statement granularity and cannot see
///    inside. The four symfony/console `Tests/` rows are this class.
const POSSIBLY_EXPECTED: &[(&str, usize)] = &[
    // 1 — `PluginManager.php:525`, class 1.
    ("composer/composer", 1),
    // 1 — `Application.php:409`, class 4 (`exitWithErrorMessage`).
    ("sebastianbergmann/phpunit", 1),
    // 10 — six class 1/2/3 in `Application.php`, `CompletionInput.php` and
    // `SymfonyStyle.php`; four class 5 in `Tests/`.
    ("symfony/console", 10),
    // 6 — every row class 4 (`$this->fail()` in `ProcessTest.php`).
    ("symfony/process", 6),
    // 2 — both class 4 (`markTestSkipped()` in the serialization tests).
    ("briannesbitt/Carbon", 2),
    // 10 — 8 `variable.maybe-undefined` (class 1 and class 4) plus the 2
    // `type.return-maybe-missing` rows this bucket absorbed from
    // `EXPECTED_PROOF_FINDINGS`.
    ("phpstan/phpstan-src", 10),
    // 120 → 121 (+1), 2026-08-14, with issue #330 PR2: `array_merge` joined the
    // fold allowlist, so a call to it stopped being an uncatalogued name whose
    // by-ref parameters might WRITE an argument — and an argument read became a
    // read (the issue #77 bind-free distinction). The row it surfaces is a test
    // data-provider returning `array_merge($a, $b)` where `$b` is bound only
    // inside a `foreach` over a class-constant list: bound on the paths where
    // the loop ran, undefined (PHP warns, reads null) where it did not. The
    // SAME function already carries two baselined rows of the SAME shape — an
    // inner-loop variable read after its loop — so this is the third sibling
    // becoming visible, not a new class of claim. TRUE at the possibly grade.
    //
    // Before this entry: 120 — 111 `variable.maybe-undefined` over 85,282 files
    // plus the 9 `type.return-maybe-missing` rows absorbed from
    // `EXPECTED_PROOF_FINDINGS`. Measured against the checkout this run saw,
    // which already differs from the revision `corpus.local.toml` records (the
    // same drift the `phpdoc.*` tripwire reports); reseed with that one when
    // the checkout is reconciled.
    ("pxxxx-monorepo", 121),
];

/// The expected possibly-grade count for a package/local-project name (0 if
/// untabled).
fn possibly_expected(name: &str) -> usize {
    POSSIBLY_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// A single **triaged TRUE proof-layer positive** the corpus legitimately
/// contains: real broken code that Steins correctly proves. Unlike the
/// measurement-mode `phpdoc.*`/`throw.*` families this is a *runtime-layer*
/// finding, where the standing bar is a strict **zero** (ADR-0013). An entry here
/// is not a weakening of that bar but a recorded, verbatim-triaged exception,
/// matched at **finding precision** (package + id + path + line + a message
/// fingerprint): any drift — a different line, a different message, a second
/// finding — no longer matches and re-reds the gate, so this can never mask a
/// future regression the way a bare count could.
struct ExpectedProofFinding {
    /// Package / local-project name the finding belongs to.
    package: &'static str,
    /// The diagnostic id (e.g. `type.argument-mismatch`).
    id: &'static str,
    /// A suffix of the finding's project-relative path.
    path_suffix: &'static str,
    /// The 1-based line.
    line: u32,
    /// A stable substring of the message (the acceptance fingerprint).
    message_contains: &'static str,
}

/// Triaged TRUE proof-layer positives (ADR-0043 §5 gate discipline). Each is a
/// place where real corpus code is genuinely wrong and Steins now proves it; the
/// triage lives in the comment beside the row. Adding a row is a conscious,
/// orchestrator-visible act — never a silent suppression.
const EXPECTED_PROOF_FINDINGS: &[ExpectedProofFinding] = &[
    // Surfaced by the ADR-0043 builtin-hierarchy ingestion (php-src mining): once
    // `stdClass` entered the closed hierarchy as a mined root (supers = []), the
    // is-a oracle can prove `stdClass` is-a-NO against every member of the
    // external union `MongoDB\Client|MongoDB\Driver\Manager`, so the definite-No
    // acceptance arm fires. The finding is in monolog's OWN test, which
    // deliberately constructs the invalid argument and asserts the resulting
    // TypeError:
    //   public function testConstructorShouldThrowExceptionForInvalidMongo() {
    //       $this->expectException(\TypeError::class);
    //       new MongoDBHandler(new \stdClass, 'db', 'collection');   // ← here
    //   }
    // against `__construct(Client|Manager $mongodb, …)` under `declare(strict_types=1)`.
    // Steins proves exactly the TypeError the test expects — a TRUE positive, not
    // an FP. (Verbatim triage in the ingestion session; sound because `stdClass`
    // has a fully-enumerated empty ancestor set and cannot be a subtype of either
    // external class.)
    ExpectedProofFinding {
        package: "Seldaek/monolog",
        id: "type.argument-mismatch",
        path_suffix: "tests/Monolog/Handler/MongoDBHandlerTest.php",
        line: 27,
        // Source-cased since `TypeMember::Instance` grew its `display` field
        // (diagnostics render the declared casing; matching stays lowercased).
        message_contains: "cannot become MongoDB\\Client|MongoDB\\Driver\\Manager",
    },
    // ADR-0049 S2: the flagship absence id `call.undefined-method` fired 10 times,
    // all on the legacy monorepo, all STATIC calls (`__callStatic` absent). Every
    // one was triaged verbatim against the checkout and is TRUE — a genuine call to
    // a method that exists nowhere in a final, trait-free, fully-enumerated chain,
    // so PHP would fatal `Error: Call to undefined method C::m()` at runtime. The
    // OSS packages fired zero (mature code does not call methods its own tests would
    // fatal on) — the point-2 core-yield prediction (method-absence needs no dam, so
    // the dynamism-heavy monorepo still yields) stands. Path suffixes are chosen to
    // key each finding precisely while omitting the private-corpus directory name.
    //
    // A DAO batch calls a legacy accessor that was removed/renamed: the DAO is a
    // `final class` with no such method anywhere in the tree.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "Batch/UnifyThumbnailSchemaBatch.php",
        line: 16,
        message_contains: "getLegacyArticleThumbnailArticleIds() — hierarchy fully enumerated",
    },
    // A sample test drifted out of sync with its sample class: `Sample_Common`
    // (`final`, methods `get`/`addData`/`swapData` only) is called with eight names
    // it never declares. Each `Sample_Common::x()` would fatal when the test runs —
    // exactly the ROADMAP gap-1 adoption case (a checker silent here is not adopted).
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 79,
        message_contains: "Sample_Common::getByHogeIds() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 105,
        message_contains: "Sample_Common::isPrime() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 134,
        message_contains: "Sample_Common::throwException() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 164,
        message_contains: "Sample_Common::getValuesFromExternalServer() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 179,
        message_contains: "Sample_Common::printToStandardOutput() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 192,
        // Method name omitted (it carries a private-corpus token); line 192 +
        // path + id keep this row 1:1.
        message_contains: "— hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 211,
        message_contains: "Sample_Common::setSampleCookie() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 229,
        message_contains: "Sample_Common::hasCookie() — hierarchy fully enumerated",
    },
    // An auth model calls `OAuth2Model::checkPassword()` statically, but that method
    // exists only as an *instance* method on the caller (`OAuth2ClientModel`);
    // `OAuth2Model` is `final` and declares no such method — a genuine undefined
    // static-method fatal.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "util/src/Model/Auth/OAuth2ClientModel.php",
        line: 106,
        message_contains: "OAuth2Model::checkPassword() — hierarchy fully enumerated",
    },
    // ADR-0049 S5: the userland arity arm `call.too-few-arguments` fired twice on
    // the legacy monorepo, both genuine `ArgumentCountError`s a run would hit,
    // triaged verbatim against the checkout and both TRUE. (The two grouped-`use`
    // `Query::__construct` findings that also fired were false positives from an
    // unlowered grouped-`use` import; that resolution bug is fixed in the paired
    // commit, and those findings are now correctly silent.) The OSS packages and
    // phpstan-src fired zero. Path suffixes start past the serving-domain path
    // component (the private-corpus naming rule).
    //
    // An admin mail-preview handler calls a static template helper whose
    // example-builder requires its `$lang` argument, with none passed — the
    // helper is `public static function getEmailExamples($lang)`, so the call
    // fatals with `ArgumentCountError` the moment `_postEmail()` runs.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.too-few-arguments",
        path_suffix: "email_preview.php",
        line: 64,
        message_contains: "getEmailExamples(): 0 passed, 1 required",
    },
    // An API test script passes only the host to a three-required-parameter static
    // method: `AppApi_Testing::requestToAllAppApiEndpoints($target_host,
    // $host_header, $oauth_token)` called with one argument — a provable
    // `ArgumentCountError` (1 passed, 3 required) when the script executes.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.too-few-arguments",
        path_suffix: "test/testall.php",
        line: 14,
        message_contains: "AppApi_Testing::requestToAllAppApiEndpoints(): 1 passed, 3 required",
    },
    // ADR-0078 issue #190: `call.on-non-object` fired once, on a phpunit
    // end-to-end regression fixture whose own name states the intent —
    // `dataProviderThatTriggersPhpError` (`$foo = []; $foo->bar();`, issue
    // 5451's reproduction). The provider genuinely fatals when invoked; that
    // is what the fixture is FOR. Triaged TRUE 2026-08-08.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "call.on-non-object",
        path_suffix: "regression/5451/Issue5451Test.php",
        line: 20,
        message_contains: "proven array on this path",
    },
    // ADR-0078 issue #184: `override.visibility-weakened` fired once, on another
    // phpunit end-to-end regression fixture (issue 6294's reproduction) — a class
    // deliberately committed to weaken a parent's visibility so the harness can
    // observe the engine fatal. Same category as the 5451 pin above. Triaged
    // TRUE 2026-08-08.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "override.visibility-weakened",
        path_suffix: "regression/6294/B.php",
        line: 17,
        message_contains: "weakens the visibility",
    },
    // ADR-0078 issue #187: the new mechanics id `array.duplicate-key` fired 19
    // times, all on the legacy monorepo, all TRUE — triaged verbatim against the
    // checkout on 2026-08-08. One config key is silently overwritten by a later
    // entry in the same literal (its variable-bound value is dead code); twelve
    // are duplicate integer ids in an append-grown allowlist literal; one is a
    // repeated series-options key; one is a repeated analytics path key; three
    // are duplicated test-fixture keys; one is a repeated view-parameter key.
    // Mechanics layer, red-on-sight bucket (ADR-0050 §1) — pinned here like the
    // S2 ten above, not weakened. The count is coupled to the drifted checkout
    // revision the gate already flags (the "revision DIFFERS" warning below):
    // reseeding the corpus baseline may move these lines, and the pins should be
    // re-cut at that sitting.
    //
    // A config accessor literal binds 'x_restricts' twice; the earlier value is
    // dead the moment the array is built.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Illust/Common.php",
        line: 2495,
        message_contains: "array key 'x_restricts' is declared twice",
    },
    // An append-grown integer allowlist literal: twelve ids were added more than
    // once across the literal's history, each overwriting an earlier entry with
    // an identical value — the duplication carries no information, only churn.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 1680,
        message_contains: "array key 8317821 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2450,
        message_contains: "array key 8279354 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2451,
        message_contains: "array key 8317785 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2452,
        message_contains: "array key 8318880 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2453,
        message_contains: "array key 7886722 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2454,
        message_contains: "array key 8267865 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2455,
        message_contains: "array key 8168208 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2456,
        message_contains: "array key 8315952 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2457,
        message_contains: "array key 8240621 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2458,
        message_contains: "array key 8204566 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2459,
        message_contains: "array key 8214002 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2460,
        message_contains: "array key 8166826 is declared twice",
    },
    // A series-options literal binds 'ai_type' twice.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "NovelSeries/Common.php",
        line: 1727,
        message_contains: "array key 'ai_type' is declared twice",
    },
    // An analytics referer-config literal binds the same path twice.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "UserAnalytics/RefererConfig.php",
        line: 554,
        message_contains: "array key '/novel/index.php' is declared twice",
    },
    // A test fixture rebinds 'illust_sanity_level' three separate times across
    // three separate literals in the same file — copy-paste drift, not one bug.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "SanityLevelTest.php",
        line: 29,
        message_contains: "array key 'illust_sanity_level' is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "SanityLevelTest.php",
        line: 54,
        message_contains: "array key 'illust_sanity_level' is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "SanityLevelTest.php",
        line: 79,
        message_contains: "array key 'illust_sanity_level' is declared twice",
    },
    // A controller's view-parameter literal binds 'tag' twice.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "AllController.php",
        line: 236,
        message_contains: "array key 'tag' is declared twice",
    },
    // ADR-0078 / issue #183: the declaration-fatal tracer's ONE corpus finding,
    // triaged TRUE for the analyzed runtime on 2026-08-08. A ClockMock test double
    // extends a `final` class and carries the other tool's inline ignore for the
    // same rule, whose stated reason is that the runtime strips `final` (ClockMock
    // rewrites classes through ext-uopz) — the author's own acknowledgment that the
    // declaration is illegal as written. The PHP Steins analyzes reports no uopz
    // loaded, so on that runtime the class load genuinely fatals and the message's
    // claim holds. Issue #205 tracks demoting final-immunity claims when the sidecar
    // reports a final-stripping extension; this pin is re-cut if that lands or the
    // corpus reseeds.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "class.extends-final",
        path_suffix: "tests/lib/ExDateTimeImmutableMock.php",
        line: 14,
        message_contains: "cannot extend final class ExDateTimeImmutable",
    },
    // -----------------------------------------------------------------------
    // Docblock hygiene (ADR-0078 / issue #186), triaged 2026-08-08.
    //
    // The six mechanics ids are red-on-sight like the proof layer, so every TRUE
    // corpus site is pinned here at finding precision. All eleven public-corpus
    // sites were read at source and are TRUE under Steins's own semantics; the
    // per-package triage is in the comment above each group. Nothing about the
    // private corpus is pinned by this wave — it is measured and reported only.
    // -----------------------------------------------------------------------
    //
    // nikic/PHP-Parser — the fuzzing driver builds a `$lexer` and then captures it
    // into a closure that never touches it (the closure works off the parser it
    // also captures). A dead capture in a dev tool: unread, by-value, and the body
    // holds no `compact`/`extract`/`$$`/`eval`/`include` that could consume it
    // unspelled.
    ExpectedProofFinding {
        package: "nikic/PHP-Parser",
        id: "closure.unused-use",
        path_suffix: "tools/fuzzing/target.php",
        line: 111,
        message_contains: "`use ($lexer)` is never read",
    },
    // sebastianbergmann/phpunit — the mock generator stacks three `@var` docblocks
    // in a row above one `return`. Under ADR-0073 only the LAST of a run adopts:
    // each of the first two has another docblock as its nearest following trivium,
    // which becomes the adopter for whatever comes after, so `$className` and
    // `$type` are cast by nothing at all. The third is silent, correctly. Inert
    // annotations, not wrong ones — which is exactly what the id claims.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/Framework/MockObject/Generator/Generator.php",
        line: 577,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/Framework/MockObject/Generator/Generator.php",
        line: 578,
        message_contains: "sits where nothing adopts it",
    },
    // composer/composer — the SAME virtual-parameter idiom as the symfony group
    // below, in a vendored copy of `symfony/filesystem` that lives inside a
    // functional-test FIXTURE tree (`tests/…/installed-versions2/vendor/…`):
    // `tempnam(string $dir, string $prefix/*, string $suffix = ''*/)` documents
    // `@param string $suffix` for an argument read back with `func_get_arg(2)`.
    // TRUE by the same reading. It is pinned rather than vendor-suppressed because
    // a pinned package is analyzed whole (ADR-0015's vendor split runs for local
    // projects only), which is why `steins check` on the same tree hides it and
    // the gate does not.
    ExpectedProofFinding {
        package: "composer/composer",
        id: "phpdoc.stale-param",
        path_suffix: "installed-versions2/vendor/symfony/filesystem/Filesystem.php",
        line: 586,
        message_contains: "`@param $suffix` names no parameter",
    },
    // symfony/console + symfony/process — two deliberate authoring idioms, both of
    // which nonetheless leave a tag declaring nothing:
    //
    //   * `@return list<\SIG*>` / `@param list<\SIG*> $signals` spell a WILDCARD
    //     over the `SIG*` constant family. No PHPDoc grammar admits it — PHPStan's
    //     own parser rejects it too — so the envelope is lost to every reader, not
    //     just to Steins. `phpdoc.unparsable`'s claim is precisely "the tag
    //     declares nothing", and it holds.
    //   * `SymfonyStyle`'s three progress helpers document `@param string|null
    //     $format` as a VIRTUAL parameter: the real signature is
    //     `progressStart(int $max = 0 /* , ?string $format = null *\/)` — the
    //     second argument is commented out for BC and read back with
    //     `func_get_arg(1)`. The tag names no parameter of the declaration, which
    //     is exactly what the id says, and the reason PHPStan reports
    //     `parameter.notFound` on the identical shape.
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.unparsable",
        path_suffix: "Command/SignalableCommandInterface.php",
        line: 24,
        message_contains: "does not parse (expected CloseAngle, found Wildcard)",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.stale-param",
        path_suffix: "Style/SymfonyStyle.php",
        line: 305,
        message_contains: "`@param $format` names no parameter",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.stale-param",
        path_suffix: "Style/SymfonyStyle.php",
        line: 327,
        message_contains: "`@param $format` names no parameter",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.stale-param",
        path_suffix: "Style/SymfonyStyle.php",
        line: 355,
        message_contains: "`@param $format` names no parameter",
    },
    ExpectedProofFinding {
        package: "symfony/process",
        id: "phpdoc.unparsable",
        path_suffix: "Process.php",
        line: 1276,
        message_contains: "does not parse (expected CloseAngle, found Wildcard)",
    },
    // thephpleague/flysystem — three inert `@var` casts in `MountManager`, two
    // shapes:
    //
    //   * `move()`/`copy()` (245, 262) write `/** @var … $sourceFilesystem */`
    //     immediately followed by a SINGLE-star `/* @var … $destinationFilesystem */`.
    //     The one-star form is a plain block comment, so it is not a docblock at
    //     all — and it still sits in the gap, breaking ADR-0073's strict adjacency
    //     for the docblock above it. Neither line casts anything.
    //   * `determineFilesystemAndPath()` (358) stacks two docblocks, so the first
    //     is shadowed by the second exactly as in the phpunit group above.
    //
    // This is a real divergence from tools with a laxer association rule, and it
    // is exactly what a Steins adopter needs to hear about their own file.
    ExpectedProofFinding {
        package: "thephpleague/flysystem",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/MountManager.php",
        line: 245,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "thephpleague/flysystem",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/MountManager.php",
        line: 262,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "thephpleague/flysystem",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/MountManager.php",
        line: 358,
        message_contains: "sits where nothing adopts it",
    },
    // The legacy monorepo's five, verified at source by the orchestrator on
    // 2026-08-08 and all TRUE. Path suffixes are cut to the shortest fragment that
    // still keys the row 1:1 (the private-corpus naming rule) — do not lengthen
    // them. Like every row on this unpinned corpus, a re-cut of the checkout can
    // move a line number and re-red the gate; that is the pin working, not drift
    // to be papered over.
    //
    // A `@phpstan-param` naming a parameter the signature genuinely lacks: the
    // options array it documents is built inline in the body, so the tag is
    // refactor rot.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.stale-param",
        path_suffix: "AppApi/IllustRecommend.php",
        line: 35,
        message_contains: "names no parameter",
    },
    // A stacked duplicate `@phpstan-var`: its twin adopts the property below, and
    // this one adopts nothing at all.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.misplaced-var",
        path_suffix: "Search/Illust.php",
        line: 105,
        message_contains: "sits where nothing adopts it",
    },
    // Two `@var` casts written as the LAST statement of a branch. The author means
    // them for the code after the closing brace, but next-statement adoption
    // (ADR-0073) ends at the `}` — in Steins and in every tool that adopts
    // next-statement-only — so the annotation is inert exactly as written.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.misplaced-var",
        path_suffix: "View/NovelCreateBookController.php",
        line: 313,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.misplaced-var",
        path_suffix: "View/NovelCreateBookController.php",
        line: 317,
        message_contains: "sits where nothing adopts it",
    },
    // A pseudo-tuple `@return [$total, $illust_ids]` — a spelling no PHPDoc grammar
    // admits, so the tag declares nothing to any reader.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.unparsable",
        path_suffix: "Controller/V1SearchWorks.php",
        line: 91,
        message_contains: "does not parse",
    },
    // ---------------------------------------------------------------------
    // `type.return-missing` (ADR-0078 §5, issue #199) — the reachability
    // foundation's tracer, triaged 2026-08-08.
    //
    // **Deliberately-stub test doubles**, every row. Empty or
    // never-returning-by-intent bodies carrying a real return type. Each WOULD
    // fatal `Return value must be of type T, none returned` the moment it were
    // invoked — and several exist precisely so that invoking them is invalid.
    // Nothing here is a bug in the package; nothing here is an FP either. This is
    // what the unconditional class looks like in mature OSS: not broken production
    // code, but fixtures.
    //
    // Its `maybe-` sibling `type.return-maybe-missing` used to be pinned here too,
    // on the reading that "the proof layer is gated whole, floor or not". ADR-0081
    // §8 supersedes that: a possibly-grade id claims only that *a* path reaches the
    // site, so a true finding on working code is the id's own yield rather than a
    // defect the corpus should be free of, and the `strict` floor already keeps it
    // off every default-profile run. Its eleven rows moved to the per-package
    // `POSSIBLY_EXPECTED` count, which still reds on an increase. The two registers
    // that comment used to distinguish are now two buckets.
    // ---------------------------------------------------------------------

    // Carbon — two macro bodies registered ONLY so the PHPStan extension under test
    // can read their declared return type: `Carbon::macro('foo', function ():
    // CarbonInterval {});`. The closure is never called; its body is `{}`.
    ExpectedProofFinding {
        package: "briannesbitt/Carbon",
        id: "type.return-missing",
        path_suffix: "tests/PHPStan/MacroExtensionTest.php",
        line: 66,
        message_contains: "Return value must be of type CarbonInterval, none returned",
    },
    ExpectedProofFinding {
        package: "briannesbitt/Carbon",
        id: "type.return-missing",
        path_suffix: "tests/PHPStan/MacroExtensionTest.php",
        line: 238,
        message_contains: "Return value must be of type Carbon, none returned",
    },
    // guzzle — eight `FnStream`/`MockHandler` decorator closures whose whole body is
    // `self::fail('the body must not be read')`. They exist to BE invalid: reaching
    // one is the test's failure condition. `Assert::fail()` is declared `: never` by
    // PHPUnit, which would silence these through the never-returning-callee veto —
    // but PHPUnit is not in guzzle's analysed universe here, so there is no
    // declaration to resolve. The rows are correct as things stand and are expected
    // to disappear the day cross-package callee resolution reaches them; that
    // disappearance is a gate event, which is the point of pinning at finding
    // precision.
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/CurlFactoryTest.php",
        line: 6812,
        message_contains: "Return value must be of type bool, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/CurlFactoryTest.php",
        line: 6815,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/RequestFramingTest.php",
        line: 417,
        message_contains: "Return value must be of type bool, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/RequestFramingTest.php",
        line: 423,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/StreamHandlerTest.php",
        line: 4516,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/StreamHandlerTest.php",
        line: 4519,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/PrepareBodyMiddlewareTest.php",
        line: 329,
        message_contains: "Return value must be of type ResponseInterface, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/PrepareBodyMiddlewareTest.php",
        line: 380,
        message_contains: "Return value must be of type ResponseInterface, none returned",
    },
    // phpunit — a `tests/_files` fixture implementing the `Event` interface with two
    // empty method bodies, both carrying the interface's declared return types.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "type.return-missing",
        path_suffix: "tests/_files/DummyEvent.php",
        line: 17,
        message_contains: "DummyEvent::telemetryInfo(): Return value must be of type Info",
    },
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "type.return-missing",
        path_suffix: "tests/_files/DummyEvent.php",
        line: 21,
        message_contains: "DummyEvent::asString(): Return value must be of type string",
    },
    // symfony/console — the `NonStringInput` test double: an `Input` subclass whose
    // three overrides are empty bodies carrying the parent's return types. The test
    // it serves drives the listener, never these methods.
    ExpectedProofFinding {
        package: "symfony/console",
        id: "type.return-missing",
        path_suffix: "Tests/EventListener/ErrorListenerTest.php",
        line: 118,
        message_contains: "NonStringInput::getFirstArgument(): Return value must be of type ?string",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "type.return-missing",
        path_suffix: "Tests/EventListener/ErrorListenerTest.php",
        line: 122,
        message_contains: "NonStringInput::hasParameterOption(): Return value must be of type bool",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "type.return-missing",
        path_suffix: "Tests/EventListener/ErrorListenerTest.php",
        line: 126,
        message_contains: "NonStringInput::getParameterOption(): Return value must be of type mixed",
    },
    // ---------------------------------------------------------------------
    // `variable.undefined` (ADR-0078, issue #194) — triaged at source
    // 2026-08-08. Eleven TRUE positives: reads of a name that appears as no
    // binding form anywhere in its scope, so PHP warns and the read yields null.
    // The two remaining sites from the same run were an FP shape (a same-variable
    // `empty($x) ? … : …` ternary, whose else arm runs only when `$x` is bound)
    // and are gone at source — the checker now shields both arms of a
    // same-variable `isset`/`empty` conditional, so they are not pinned here.
    //
    // The OSS one is a fixture doing its job. The ten monorepo ones split into:
    // four exception-message reads of a name that does not exist (including one
    // `$this->$mode` dynamic-property typo that meant `->mode`, and an obvious
    // `$withd` for `$width`); one logger field silently null ever since its
    // variable was renamed; one KVS setter storing `(int)$count` of a never-bound
    // name, hence always zero — a live defect; two batch-script reads of a deleted
    // `$offset` that work only by accident (`array_splice(…, null)` degrades to a
    // single whole-list pass); one deleted-parameter read on a return path; and one
    // stale test fixture. The standing re-cut note applies: a corpus reseed moves
    // these lines and must re-triage rather than re-pin blindly.
    //
    // Path suffixes are kept SHORT on purpose — several full paths carry the
    // private project's name, which must not enter a tracked file. Do not lengthen
    // them.
    //
    // The warning-observation fixture: `$a = $b;` with no `$b` anywhere, written to
    // make PHPUnit observe a PHP warning. Steins reports the one unsuppressed read
    // and stays silent on the `@`-suppressed twin beneath it.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "variable.undefined",
        path_suffix: "event/_files/PhpWarningTest.php",
        line: 18,
        message_contains: "$b is never bound",
    },
    // Exception-message reads of names that do not exist.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Model/Illust/ContentsType.php",
        line: 29,
        message_contains: "$type is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Model/Illust/IllustModel.php",
        line: 134,
        message_contains: "$withd is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Ranking.php",
        line: 109,
        message_contains: "$mode is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "PPoint/PaymentMethodCode.php",
        line: 71,
        message_contains: "$payment_method_code is never bound",
    },
    // Two reads of a deleted `$offset` in one batch script — live only because
    // `array_splice(…, null)` degrades to a single whole-list pass.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "script/get_event_circle_user.php",
        line: 78,
        message_contains: "$offset is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "script/get_event_circle_user.php",
        line: 82,
        message_contains: "$offset is never bound",
    },
    // A KVS setter storing `(int)$count` of a never-bound name: always zero.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Group/Base.php",
        line: 1381,
        message_contains: "$count is never bound",
    },
    // A deleted parameter, still read on a return path.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "stacc/api.php",
        line: 574,
        message_contains: "$display_m is never bound",
    },
    // A stale test fixture.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "tests/SampleTest.php",
        line: 215,
        message_contains: "$cookie_store is never bound",
    },
    // A logger field silently null since its variable was renamed.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Log/FluentdLogger.php",
        line: 41,
        message_contains: "$write_time is never bound",
    },
];

/// Whether `d` is a recorded, triaged TRUE proof-layer positive for `package`
/// (see [`EXPECTED_PROOF_FINDINGS`]) — reported but excluded from the red/green
/// verdict. Matched at finding precision so any drift re-reds the gate.
fn is_expected_true_positive(package: &str, d: &Diagnostic) -> bool {
    EXPECTED_PROOF_FINDINGS.iter().any(|e| {
        e.package == package
            && e.id == d.id
            && e.line == d.line
            && d.path.ends_with(e.path_suffix)
            && d.message.contains(e.message_contains)
    })
}

/// Entry point for `cargo xtask fp-gate`. Returns `true` if the gate is GREEN
/// (no diagnostics on clean code).
pub fn run() -> Result<bool, String> {
    let lock = read_lock();
    if lock.packages.is_empty() {
        return Err("corpus.lock.toml is empty — run `cargo xtask corpus-sync` first".to_owned());
    }
    let root = repo_root();

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
            Ok(analyze_package(pkg.name, &tag, &dir, &root))
        })
        .collect();
    let mut reports = reports?;
    // Keep a stable (canonical corpus) order for the report.
    reports.sort_by_key(|r| PACKAGES.iter().position(|p| p.name == r.name).unwrap_or(usize::MAX));

    // Private-corpus injection point (ADR-0013 §4): each `[[project]]` in the
    // optional (gitignored) `corpus.local.toml` is analyzed as one project, in
    // parallel like the packages, applying the CLI's vendor default — vendor
    // files are indexed for inference but their findings don't count.
    let locals = corpus_local::read_local()?;
    let mut local_reports: Vec<PackageReport> =
        locals.par_iter().map(analyze_local).collect();
    local_reports.sort_by(|a, b| a.name.cmp(&b.name));

    // Measurement-mode regression tripwires (see `PHPDOC_EXPECTED` /
    // `THROW_EXPECTED`): a package regresses iff a count *exceeds* its seeded
    // expectation. Both `phpdoc.*` and `throw.*` are contract-layer.
    let regressions = phpdoc_regressions(&reports, &local_reports);
    let throw_regressions = measurement_regressions(&reports, &local_reports, "throw", |r| r.throws.len(), throw_expected);
    // ADR-0050 §9 delta family: `effect.*`-contract findings gate as an increase
    // tripwire too. Empty today (no corpus envelopes), so no package can regress.
    let effect_regressions = measurement_regressions(&reports, &local_reports, "effect", |r| r.effects.len(), effect_expected);
    // ADR-0081 §8: the possibly-grade proof ids gate as an increase tripwire too.
    let possibly_regressions = measurement_regressions(&reports, &local_reports, "possibly", |r| r.possibly.len(), possibly_expected);

    print_report(
        &reports,
        &local_reports,
        &regressions,
        &throw_regressions,
        &effect_regressions,
        &possibly_regressions,
    );

    // RED on any counted proof-layer finding — package diagnostics plus local
    // *non-vendor* diagnostics (vendor findings never gate; ADR-0015) — OR on any
    // measurement-mode count that has regressed past its expected baseline.
    let total_diags: usize = reports.iter().map(|r| r.diagnostics.len()).sum::<usize>()
        + local_reports.iter().map(|r| r.diagnostics.len()).sum::<usize>();
    Ok(total_diags == 0
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

/// Compare every package's `phpdoc.*` count to its expected baseline, returning the
/// ones that have *increased* (the only direction that gates red).
fn phpdoc_regressions(
    reports: &[PackageReport],
    local_reports: &[PackageReport],
) -> Vec<PhpdocRegression> {
    measurement_regressions(reports, local_reports, "phpdoc", |r| r.phpdoc.len(), phpdoc_expected)
}

/// Generic measurement-mode tripwire: report packages whose `count` exceeds their
/// `expected` baseline (the only direction that gates red).
fn measurement_regressions(
    reports: &[PackageReport],
    local_reports: &[PackageReport],
    _family: &str,
    count: impl Fn(&PackageReport) -> usize,
    expected: impl Fn(&str) -> usize,
) -> Vec<PhpdocRegression> {
    reports
        .iter()
        .chain(local_reports.iter())
        .filter_map(|r| {
            let actual = count(r);
            let exp = expected(&r.name);
            (actual > exp).then(|| PhpdocRegression { name: r.name.clone(), actual, expected: exp })
        })
        .collect()
}

// Default posture (ADR-0004): the gate folds via the PHP sidecar. Each rayon
// worker owns one resident `SidecarFolder` (thread-local), reused across the
// packages that worker analyzes.
//
// Reuse makes the folder CARRY STATE between projects, so every use must go
// through [`check_under_target`] — see its comment for what forgetting cost.
thread_local! {
    static FOLDER: RefCell<SidecarFolder> = RefCell::new(SidecarFolder::enabled());
}

/// Check `project` on the resident folder, configured for **this** project's
/// declared PHP target (issue #28).
///
/// The ONE way to reach `FOLDER`, because the pairing is not optional. A resident
/// folder reused across projects keeps the previous project's `php_target`, and that
/// target gates the ADR-0056 curated return-fact admission (`range == {PINNED_PHP}`
/// exactly) and the absence family (`target_admits_runtime`). Analyzing a project
/// under a *different* project's declared target therefore silently changes which
/// facts are seeded.
///
/// That is exactly what issue #63 was: `analyze_local` called `check_project` on the
/// resident folder directly and never set the target, so each local project was
/// judged under whichever corpus package's target its rayon worker happened to hold
/// — `>=8.4.1`, `7.2.5`, `^7.4 || ^8.0`, whatever the work-stealing produced. The
/// local corpus's `phpdoc.*` count swung between 536 and 483 run to run on unchanged
/// code, and with `RAYON_NUM_THREADS=1` (one worker, always the same leftover
/// target) it looked perfectly stable. Two sessions' triage was spent on a
/// "regression" that was this.
///
/// Taking the target by argument rather than reading it back off the layout keeps the
/// call sites honest: there is no way to check a project without saying what it
/// targets.
fn check_under_target(
    db: &SteinsDatabase,
    project: Project,
    php_target: Option<steins_db::PhpTarget>,
) -> Vec<Diagnostic> {
    FOLDER.with(|f| {
        let mut folder = f.borrow_mut();
        folder.set_php_target(php_target);
        check_project(db, project, &mut *folder)
    })
}

/// Analyze one package as a single project and time it.
fn analyze_package(name: &str, tag: &str, dir: &Path, root: &Path) -> PackageReport {
    let start = Instant::now();

    let mut files = Vec::new();
    collect_php_files(dir, &mut files);
    files.sort();

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(), // unreadable → empty (parses clean, contributes nothing)
        };
        inputs.push(SourceFile::new(&db, rel, text));
    }

    // Identify parse-error files (their diagnostics are excluded from the count).
    //
    // ADR-0079 (issue #180) makes most of this redundant and one part of it a
    // deliberate blind spot. Redundant: the analyzer itself now emits nothing but
    // `syntax.unparsable` from a file that failed to parse, so there are no
    // inference findings left here to drop. The blind spot: this retain also drops
    // that new id, so a pre-existing unparsable corpus file cannot turn the gate red
    // on a finding whose remedy lives in someone else's repository. What it does NOT
    // drop is the *consequence* — a non-vendor unparsable file is a dam site, so the
    // existence family goes silent across that package, which can only lower counts.
    let mut parse_error_files = Vec::new();
    for &input in &inputs {
        if !parse(&db, input).parse_errors().is_empty() {
            parse_error_files.push(input.path(&db).to_owned());
        }
    }
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    // Paths are `root`-relative above, so the layout resolves against `root`
    // (ADR-0015): the package's own `composer.json` decides what is vendor.
    let layout = composer::discover(&[dir.to_path_buf()], root);
    // The declared target PHP range (issue #28) gates the folder's absence
    // family and curated-fact admission — the gate measures the analyzer as the
    // CLI ships it, so the corpus packages' own `require.php` declarations
    // apply here too. The resident folder drops target-dependent memos on the
    // change, so cross-package reuse stays sound.
    let php_target = layout.php_target().cloned();
    let plugins = steins_db::PluginFacts::discover(&layout, None);
    let project = Project::new(&db, inputs, layout, plugins);
    let mut diags: Vec<Diagnostic> = check_under_target(&db, project, php_target);
    diags.retain(|d| !parse_err_set.contains(d.path.as_str()));
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    // Measurement-mode split (ADR-0050 §9): contract-layer findings are reported +
    // counted but do not gate on sight (only their per-package increase tripwire
    // does). The **layer** (from the steins-infer registry) is the gate carrier;
    // the family prefix keys each separate count table — `phpdoc.*`, `throw.*`, and
    // the ADR-0050 §9 `effect.*` delta. Proof and mechanics stay red-on-sight in
    // `diags` (`is_contract` keeps `effect.unknown-label`, a mechanics id, there).
    let phpdoc: Vec<Diagnostic> = diags.iter().filter(|d| is_phpdoc(d)).cloned().collect();
    let throws: Vec<Diagnostic> = diags.iter().filter(|d| is_throw(d)).cloned().collect();
    let effects: Vec<Diagnostic> = diags.iter().filter(|d| is_effect_contract(d)).cloned().collect();
    let possibly: Vec<Diagnostic> = diags.iter().filter(|d| is_possibly(d)).cloned().collect();
    // Debug-layer findings (ADR-0053 §8) are excluded from every counter before the
    // contract split and the red-on-sight retain: a dump is requested introspection,
    // not a finding. Dropped outright — not reported, not counted. Vacuous today (no
    // debug emitter until D3/D4), so `diags` is byte-identical to the pre-dump run.
    diags.retain(|d| !is_debug(d));
    diags.retain(|d| !is_contract(d));
    diags.retain(|d| !is_possibly(d));
    // Split off triaged TRUE runtime-layer positives (reported, not gated). The
    // ADR-0049 S2 flagship `call.undefined-method` now flows through this
    // red-on-sight channel like any proof-layer id, with its triaged TRUE corpus
    // findings pinned in `EXPECTED_PROOF_FINDINGS` (any un-pinned finding reds).
    let expected_true: Vec<Diagnostic> =
        diags.iter().filter(|d| is_expected_true_positive(name, d)).cloned().collect();
    diags.retain(|d| !is_expected_true_positive(name, d));

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
        elapsed: start.elapsed(),
    }
}

/// Analyze one local project (ADR-0013 §4) as a single project. Paths are made
/// project-relative so the `vendor/` predicate and the report read cleanly.
/// Vendor findings are split out of the gate count (ADR-0015).
fn analyze_local(proj: &LocalProject) -> PackageReport {
    let start = Instant::now();
    let root = Path::new(&proj.path);

    // Read what the tree is on BEFORE walking it, so the revision and cleanliness
    // reported beside a count describe the state that count was taken at. Both
    // reads degrade to "unknown" rather than failing or asserting.
    let measured_revision = corpus_local::checkout_revision(root);
    let worktree = WorktreeState::from_dirty(corpus_local::checkout_is_dirty(root));

    let files = corpus_local::collect_php_files_in(root, &proj.paths, &proj.exclude);

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        // Project-relative path (falls back to the full path if `f` is not under
        // `root`, which cannot normally happen). Keeps `vendor/` detection and
        // the printed rows readable.
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        };
        inputs.push(SourceFile::new(&db, rel, text));
    }

    // The same exclusion, and the same ADR-0079 reading, as the corpus path above.
    //
    // ADR-0079 §4 asks the corpus to be swept for pre-existing unparsable files
    // before the id ships, and for what it finds to be recorded as corpus facts
    // rather than rediscovered as surprises. Swept 2026-08-08; the local root holds
    // exactly three, each with a cascade of further errors behind its first:
    //
    //   vendor/apache/thrift/lib/php/lib/Thrift/Transport/TCurlClient.php:95  (+8)
    //   vendor/apache/thrift/lib/php/lib/Thrift/Transport/THttpClient.php:100 (+8)
    //   php-openid/Tests/Auth/OpenID/HMAC.php:66                              (+6)
    //
    // All three are VENDOR, so §2.3's presumption applies to all three: none is a
    // dam site, none makes its class-likes member-incomplete, and each one's
    // `syntax.unparsable` finding is dropped by the retain below in any case. The
    // consequence worth writing down is that the dam never engages on this corpus
    // today — so the §2.5 member-incomplete leg is exercised by fixtures alone
    // (`crates/steins-infer/tests/parse_failure_dam.rs`) until a NON-vendor break
    // appears here, at which point the existence family goes silent for that
    // package and these counts fall. They cannot rise.
    let mut parse_error_files = Vec::new();
    for &input in &inputs {
        if !parse(&db, input).parse_errors().is_empty() {
            parse_error_files.push(input.path(&db).to_owned());
        }
    }
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let layout = composer::discover(&[root.to_path_buf()], root);
    // A local project declares a target like any other (issue #63): this call used
    // to go straight to the resident folder, inheriting whichever corpus package's
    // target the worker last held.
    let php_target = layout.php_target().cloned();
    let plugins = steins_db::PluginFacts::discover(&layout, None);
    let project = Project::new(&db, inputs, layout.clone(), plugins);
    let mut diags: Vec<Diagnostic> = check_under_target(&db, project, php_target);
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
    let expected_true: Vec<Diagnostic> =
        diags.iter().filter(|d| is_expected_true_positive(&proj.name, d)).cloned().collect();
    diags.retain(|d| !is_expected_true_positive(&proj.name, d));

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
        elapsed: start.elapsed(),
    }
}

fn print_report(
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
        // What state the corpus was measured in — printed on EVERY run for a local
        // project, green included, not only when a tripwire trips.
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

    // Measurement-mode summary: the `phpdoc.*` declared-contract ids, counted per
    // package against the `PHPDOC_EXPECTED` baseline. These do NOT gate on their
    // own existence (TRUE contract-layer findings live in released code, ADR-0030);
    // a package gates red only if its count *increased* past the baseline.
    let total_phpdoc: usize = reports.iter().chain(local_reports.iter()).map(|r| r.phpdoc.len()).sum();
    let total_expected: usize = PHPDOC_EXPECTED.iter().map(|(_, c)| *c).sum();
    println!("\n=== phpdoc.* measurement mode (contract layer — gates only on INCREASE) ===\n");
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = phpdoc_expected(&r.name);
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

    // Measurement-mode summary for the `throw.*` contract-layer ids (ADR-0040):
    // counted per package against `THROW_EXPECTED`, gating only on INCREASE. The
    // volume is far larger than `phpdoc.*` (checked-exception saturation), so only
    // per-package counts and a small sample print — never every finding.
    let total_throw: usize = reports.iter().chain(local_reports.iter()).map(|r| r.throws.len()).sum();
    let total_throw_expected: usize = THROW_EXPECTED.iter().map(|(_, c)| *c).sum();
    println!("\n=== throw.* measurement mode (contract layer — gates only on INCREASE) ===\n");
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = throw_expected(&r.name);
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

    // Measurement-mode summary for the `effect.*` **contract** ids (ADR-0050 §9
    // delta: `effect.envelope-exceeded` / `effect.liskov-widened`). The section is
    // **suppressed while dormant** — it prints nothing unless an effect finding
    // lands, the expected table is seeded, or a regression trips — which kept the
    // gate report byte-identical to the pre-convergence run while the family was
    // vacuous, and surfaced it the day the corpus grew an envelope. That day was
    // 2026-08-12: the interop-envelope run (#303) reads upstream's purity tags, the
    // private monorepo carries three of them, and the dormancy guard has been off
    // ever since (see [`EFFECT_EXPECTED`]'s seeded row).
    let total_effect: usize = reports.iter().chain(local_reports.iter()).map(|r| r.effects.len()).sum();
    if total_effect > 0 || !EFFECT_EXPECTED.is_empty() || !effect_regressions.is_empty() {
        let total_effect_expected: usize = EFFECT_EXPECTED.iter().map(|(_, c)| *c).sum();
        println!("\n=== effect.* measurement mode (contract layer — gates only on INCREASE) ===\n");
        for r in reports.iter().chain(local_reports.iter()) {
            let expected = effect_expected(&r.name);
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

    // Measurement-mode summary for the **possibly-grade** proof ids (ADR-0081 §8):
    // the `strict`-floored rows, counted per package against `POSSIBLY_EXPECTED`
    // and gating only on INCREASE. Every finding prints — unlike `throw.*` the
    // volume is triageable, and unlike `phpdoc.*` a reader has no prefix to go
    // looking for these under.
    let total_possibly: usize =
        reports.iter().chain(local_reports.iter()).map(|r| r.possibly.len()).sum();
    println!(
        "\n=== possibly-grade proof ids, strict floor (measurement mode — gates only on INCREASE) ===\n"
    );
    let total_possibly_expected: usize = POSSIBLY_EXPECTED.iter().map(|(_, c)| *c).sum();
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = possibly_expected(&r.name);
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
    println!("\n=== summary ===\n");
    println!(
        "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8}",
        "package", "files", "parse-errors", "diagnostics", "time(s)"
    );
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8));
    let (mut tf, mut tp, mut td) = (0usize, 0usize, 0usize);
    let mut ttime = 0.0f64;
    for r in rows() {
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        println!(
            "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}",
            label,
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len(),
            r.elapsed.as_secs_f64()
        );
        tf += r.file_count;
        tp += r.parse_error_files.len();
        td += r.diagnostics.len();
        ttime += r.elapsed.as_secs_f64();
    }
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8));
    println!("{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}", "TOTAL", tf, tp, td, ttime);

    println!();
    let measurement_ok =
        regressions.is_empty() && throw_regressions.is_empty() && effect_regressions.is_empty();
    match (td == 0, measurement_ok) {
        (true, true) => {
            println!(
                "GATE GREEN — no proof-layer diagnostics on clean-parsing corpus code, \
                 and no phpdoc.*/throw.* regression past the expected baselines."
            );
        }
        (false, _) => {
            println!(
                "GATE RED — {td} proof-layer diagnostic(s) on clean code. Human FP triage required (ADR-0013)."
            );
        }
        (true, false) => {
            println!(
                "GATE RED — {} package(s) regressed past their expected phpdoc.*/throw.* baseline \
                 (see the tripwire lists above). Investigate the new finding(s); update \
                 PHPDOC_EXPECTED / THROW_EXPECTED in xtask/src/gate.rs only once the change is \
                 understood and intended.",
                regressions.len() + throw_regressions.len()
            );
        }
    }
}

/// Print one measurement family's tripwire verdict, and — for a tripped **local**
/// project — the recorded-vs-measured revision line beside it.
///
/// That line is where the whole mechanism earns its keep: a raised count on a
/// pinned package can only be the analyzer, but on a live working tree it is
/// ambiguous, and the operator is standing right here, at the moment of the RED,
/// deciding whether to triage findings or re-measure the corpus.
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
