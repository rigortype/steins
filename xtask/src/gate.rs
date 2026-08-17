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
    /// issues #303/#311) — how [`EFFECT_EXPECTED`]'s first row seeded on code
    /// with no Steins annotation. `effect.unknown-label` is mechanics, stays
    /// red-on-sight in `diagnostics`.
    effects: Vec<Diagnostic>,
    /// Possibly-grade proof findings — `strict`-floored ids (ADR-0081 §8) —
    /// counted against [`POSSIBLY_EXPECTED`]. Definite siblings
    /// (`variable.undefined`, `property.undefined`, `type.return-missing`)
    /// stay red-on-sight in `diagnostics`.
    possibly: Vec<Diagnostic>,
    /// Triaged TRUE runtime-layer positives (see [`EXPECTED_PROOF_FINDINGS`]),
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

/// Route a finding to its [`GateBucket`] by layer. Unregistered ids are
/// conservatively red-on-sight.
fn gate_bucket(d: &Diagnostic) -> GateBucket {
    match layer(d.id) {
        Some(Layer::Contract) => GateBucket::Measurement,
        Some(Layer::Debug) => GateBucket::Excluded,
        // Possibly-grade (ADR-0078 §1.3's `maybe-` convention): derived from the
        // registry so a new sibling takes the right posture on registration.
        Some(Layer::Proof) if surface_floor(d.id) == Some(Floor::Strict) => {
            GateBucket::Tripwire
        }
        Some(Layer::Proof | Layer::Mechanics) | None => GateBucket::RedOnSight,
    }
}

/// Whether a diagnostic is contract-layer (ADR-0050 §9): measurement-mode
/// partitioning, via [`gate_bucket`].
fn is_contract(d: &Diagnostic) -> bool {
    gate_bucket(d) == GateBucket::Measurement
}

/// Whether a diagnostic is debug-layer (ADR-0053 §8): excluded from every gate
/// counter.
fn is_debug(d: &Diagnostic) -> bool {
    gate_bucket(d) == GateBucket::Excluded
}

/// Whether a diagnostic is a possibly-grade proof finding (ADR-0081 §8), the
/// `strict`-floored proof ids counted against [`POSSIBLY_EXPECTED`]. One id
/// table, not one per family — the three ids share a posture, not a prefix.
fn is_possibly(d: &Diagnostic) -> bool {
    gate_bucket(d) == GateBucket::Tripwire
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
    // The issue #391 wave (2026-08-16), `phpdoc.maybe-argument-mismatch`: a
    // builtin whose declared return is `T|false`/`T|null` handed straight into a
    // native `T` with no check in between — the argument side's possibly grade on
    // an `Asserted` premise (the declared-return floor is Asserted by ADR-0069, so
    // this whole family lands on the contract id, never on `type.*`). Every one is
    // TRUE against its source, and the shape is one line of PHP each. Counted here
    // rather than in POSSIBLY_EXPECTED because the id is `Layer::Contract`, which
    // is what routes it to this bucket (ADR-0081 §8: the layer decides the bucket,
    // the floor decides the surface).
    //   21 → 24 (+3): `file_get_contents()` into `new JsonManipulator(string
    //   $contents)` (RequireCommand.php:597) and into `stripWhitespace(string
    //   $source)` (Compiler.php:227); `inet_pton()` into `ipMapTo6(string
    //   $binary)` (NoProxyPattern.php:246).
    //
    // The issue #423 wave (2026-08-17, ADR-0056 §9) is the same judgment reaching
    // two places it could not reach before, and it moves six packages. Both halves
    // land on the CONTRACT id for the same reason the #391 wave did — the premise
    // is the ADR-0069 declared-return floor, which is `Asserted` — so none of this
    // touches the proof layer, and the corpus-wide `diagnostics` column stayed 0
    // through the whole wave.
    //
    //   (a) **A builtin callee is judged now.** `strlen($maybeFalse)` used to be
    //       silent because a builtin's parameters had no type source; they have
    //       one (the sidecar's own `getParameters()`), so a `T|false` handed to a
    //       builtin's native `T` reads exactly as it already read at a project
    //       callee.
    //   (b) **A builtin call written directly in argument position carries a
    //       premise now.** `f(realpath($p))` reached none, while
    //       `$r = realpath($p); f($r)` reached one off the very same rungs — one
    //       call written two ways answering differently. The Call carrier now
    //       reads the builtin ladder in the assignment path's own order.
    //
    // Every row below was read against its source line; not one is a guarded site.
    //
    //   24 → 31 (+7), all shape (b) except the last: `realpath()` into
    //   `Filesystem::normalizePath(string $path)` (AutoloadGenerator.php:217, 218)
    //   and into `findShortestPathCode(string $from/$to)` (224, 225);
    //   `file_get_contents()` into `Locker::__construct(string
    //   $composerFileContents)` (Factory.php:428) and into
    //   `JsonFile::parseJson(?string $json)` (JsonLoader.php:44); `json_encode()`
    //   into `new JsonManipulator(string $contents)`
    //   (Test/Json/JsonManipulatorTest.php:3231).
    ("composer/composer", 31),
    //   8 → 12 (+4): `realpath()` into a `string` parameter four times — `new
    //   TestCase($filename)` twice in `Runner/Phpt/TestCaseTest.php`, `new
    //   PhptTestCase($filename)` in `ListTestIdsCommandTest.php`, and
    //   `ExcludeList::addDirectory($directory)` in `ExcludeListTest.php`.
    //   12 → 51 (+39), 2026-08-17 (issue #423), every one shape (b) and every one
    //   the same two-line idiom repeated across the assertion suite: a fixture is
    //   read or encoded inline and handed straight to the assertion's `string`
    //   parameter. 22 are `file_get_contents()` into `Assert::assertStringEquals
    //   File*`/`assertStringNotEqualsFile*`/`assertXmlString*XmlFile`/
    //   `assertStringEqualsStringIgnoringLineEndings`; 5 are `json_encode()` into
    //   `assertJsonStringEqualsJsonFile`/`assertJsonStringNotEqualsJsonFile`/`new
    //   JsonMatches`; 11 are `realpath()` into `Issue::from(string $file)` /
    //   `Reader::read(string $baselineFile)` across the `Runner/Baseline` tests;
    //   1 is `ini_get()` into `assertStringStartsWith(string $string)`
    //   (TextUI/PhpHandlerTest.php:41). All TRUE at the possibly grade: the false
    //   arm of each of those four builtins is real, and a test that never sees it
    //   is a path claim this grade deliberately does not make.
    //   51 → 55 measured on the gate's OWN engine (CI pins PHP 8.4): phpunit's
    //   composer.json pins `config.platform.php` at 8.4.1, so an 8.4 runtime is
    //   admitted as a witness and the sidecar answers `builtin_param_types`,
    //   while an 8.5 runtime is outside the declared target and the builtin arm
    //   declines (issue #28's posture — a runtime the project does not ship on
    //   proves nothing). The four extra rows are all builtin CALLEES with a
    //   builtin `T|false` argument, one shape and TRUE at the possibly grade:
    //   `file_get_contents()` into `json_decode()` (build/scripts/phar-manifest.php)
    //   and into `preg_match_all()` (Framework/Assert/FunctionsTest.php ×2),
    //   `getmypid()` into `posix_kill()` (end-to-end/_files/…/InterruptTest.php).
    //   A local 8.5 run therefore reads 51 here and stays under the tripwire; the
    //   seeded count is the CI engine's, which is the one the gate is calibrated on.
    ("sebastianbergmann/phpunit", 55),
    // 0 → 4 (+4), 2026-08-17 (issue #423), all shape (a) — the tempnam idiom:
    // `$certFile` / `$tmpfname` carry `non-falsy-string|false` and go straight
    // into `rename(string $from)` (Handler/CurlFactoryTest.php:4031, 4045, 4061)
    // and `unlink(string $filename)` (Handler/StreamHandlerTest.php:807). The
    // builtin sink is the only new part; the argument's type was already read.
    ("guzzle/guzzle", 4),
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
    // 5 → 6 (+1), 2026-08-17 (issue #423), shape (a): `json_encode()` straight
    // into `substr(string $string, …)` (Formatter/JsonFormatterTest.php:238).
    // `json_encode` really does answer `false` on malformed UTF-8, and `substr`
    // under `strict_types` really does fatal on it.
    ("Seldaek/monolog", 6),
    // 1 → 2 (+1) with ADR-0043 stage 4 (phpdoc-side class contracts). The new
    // finding is a class-value contract: `new MountManager(['valid' => 'something
    // else'])` — a plain string in the `array<string, FilesystemOperator>` value
    // position — inside a `guarding_against_mounting_invalid_filesystems` test that
    // wraps it in `expectException(UnableToMountFilesystem::class)` and carries
    // `@phpstan-ignore-next-line`. A TRUE no-coercion violation the test documents.
    // 2 → 3 (+1), 2026-08-16, issue #391: `file_get_contents()` into
    // `computeFingerPrint(string $publicKey)` (SftpConnectionProviderTest.php:189).
    ("thephpleague/flysystem", 3),
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
    // 15 → 16 (+1), 2026-08-16, issue #391: `json_encode()` into
    // `JsonDecoder::decode(string $json)` (JsonDecoderTest.php:21). Its sibling in
    // this package — `preg_replace()`'s `string|null` into `indentString(string
    // $str)` — carries an all-`Verified` premise and so lands on the proof id, in
    // POSSIBLY_EXPECTED below, not here. One package, one judgment, two buckets:
    // the stratum split, visible in the gate.
    // 16 → 17 (+1), 2026-08-17 (issue #423), shape (a): `array_splice($this->
    // visitors, $index, 1, [])` in `NodeTraverser.php:56`, where `$index` is the
    // `int|string` key of a `foreach` over a declared array. `array_splice`'s
    // `$offset` is a native `int`, so the string arm fatals — TRUE at the
    // possibly grade, and contract-layer because the `int|string` is a docblock's
    // claim about the array's keys rather than anything PHP enforces.
    ("nikic/PHP-Parser", 17),
    // 0 → 9 (+9), 2026-08-17 (issue #423), all shape (a). Two in `src/`:
    // `preg_split('/[_.-]+/', $completeLocale)` with `$completeLocale` a
    // `string|false` (AbstractTranslator.php:353), and `array_splice($arguments,
    // key($timezoneParameters), …)` where `key()` is `int|string|null` against a
    // native `int $offset` (Factory.php:757). Seven in `tests/`: four
    // `date('Y-m-d', strtotime(…))` (`int|false` into `?int`, CreateTest.php:268,
    // 273, 278, 283), two `json_decode(json_encode(Carbon::now()))`
    // (JsonSerializationTest.php:27 in both the Carbon and CarbonImmutable
    // suites), and `trim(file_get_contents(…))`
    // (CarbonInterval/ConstructTest.php:563). Each is one line of PHP and each
    // false/null arm is real.
    ("briannesbitt/Carbon", 9),
    // The private monorepo (corpus.local.toml); matched by its local project name.
    //
    // Ledger of every move, oldest first. Standing conditions unless a row says
    // otherwise: proof layer 0, every OSS package unchanged (the soundness signal —
    // a wrong checker change lights up well-typed OSS first), `throw.*` and
    // possibly-grade at their own baselines, PHPStan reporting the identical class.
    // A decrease never gates; it is adopted consciously and recorded so the next
    // reader knows the cause.
    //
    //   333 → 357  ADR-0031 branch-sensitive analysis: values previously buried in
    //              `Opaque` control flow reach the contract layer.
    //   357 → 404  ADR-0035 refined layer — native-type seeding, guard refinements,
    //              `@phpstan-assert` application. 8 abstract-fact findings, the rest
    //              concrete; all TRUE no-coercion violations in released test code.
    //              Class-shaped `@param`s stay silent against scalar facts (template
    //              safety), so no template FPs.
    //   404 → 405  ADR-0036 object state: the first `phpdoc.property-mismatch` — an
    //              int literal assigned to a `@var numeric-string` property. Property
    //              checks run only in the plain per-scope pass, never under a binding
    //              descent, whose caller values in-body guards would narrow.
    //   405 → 439  ADR-0043 stage 4 (class contracts + the enum-case/class-const
    //              resolution feeding them). 36 added, 2 pre-existing FPs removed, all
    //              34 net TRUE: class-const numeric strings into `int`/`int[]` (the
    //              ADR-0037 DB-illusion pattern); proven scalars/objects against
    //              class-typed contracts; sealed-shape violations that became provable
    //              once their const/`::class`/enum elements resolved.
    //   439 → 434  2026-07-24, LIVE-TREE DRIFT rather than a checker change — the
    //              unpinned checkout gained ~210 files during the day.
    //   434 → 477  ADR-0056 R1 builtin return facts: a uniquely-resolved builtin seeds
    //              its reflected return envelope (`trim()` ⇒ `General{String}`), which
    //              then reaches a `@param int`. All 43 triaged, every one the
    //              stringly-typed request-param → int-param pattern; two render
    //              `non-empty-string`, the envelope composing with an existing
    //              `=== ''` guard.
    //   477 → 487  2026-07-29, DR2 `is_*` guard narrowing (ADR-0064 seam v): a fact now
    //              SURVIVES a pure guard instead of being forgotten. All 10 the same
    //              idiom — `is_numeric` proves numeric-STRING-ness, not int-ness. One
    //              proof-layer FP the slice introduced (a refuted fact left standing on
    //              an unreachable branch) was caught by this gate and fixed in-slice.
    //   487 → 497  2026-08-02, ADR-0072 shape facts judged against contracts. 10/10
    //              TRUE, one class: a sealed `array{…}` `@param` under-declaring keys
    //              its call sites provably always pass.
    //   497 → 498  2026-08-02, ADR-0073 inline `@var` cast seeding (PR #121) — the same
    //              sealed-shape class reached by a new path.
    //   498 → 499  2026-08-05, CORPUS STATE rather than an engine change, attributed
    //              two ways instead of by triage: every public package is exact in the
    //              same run, and the gate re-run at the commit that seeded 498
    //              reproduces 499 against today's checkout. NOT triaged
    //              finding-by-finding — that needs the previous corpus state, which
    //              nobody retained, and which the `revision` record now closes.
    //   499 → 500  2026-08-05, ADR-0077 out-parameter seeding (PR #152): a capture
    //              group read after a guard that proved the match carries `string` into
    //              a `@param int`. The group being a digit class is what makes the
    //              annotation look plausible and is exactly no defence — PCRE hands
    //              back a string whatever matched.
    //   500 → 507  **Reseed 2026-08-09**, corpus pin moving with it (`565b106a…` →
    //              `5b026671…`, the two-file discipline). Re-measuring at the seeded
    //              revision was impossible (9.8 GB checkout, 2.1 GB free), so the
    //              accounting is indirect and recorded rather than implied: of the 316
    //              files carrying findings, 5 moved between the seeded and measured
    //              revisions and carry 14 findings — a channel wide enough to account
    //              for +7 several times over, not a proof that it did. Each
    //              analyzer-side movement was measured back-to-back at one checkout
    //              (issue #272 alone removed 4 FPs and added 3 verified debts, 508 →
    //              507). The gate had printed RED here for two sessions, which is the
    //              failure mode a stale baseline actually has: a standing red trains
    //              the reader to skim past the next real regression.
    //   507 → 513  2026-08-09, issue #288 — a project call's declared RETURN shape now
    //              seeds the caller's value lane, the mirror of ADR-0062 S3's `@param`
    //              seeding. 6/6 TRUE by reading both docblocks, all the sealed-shape
    //              under-declaration class: a 3-key HTTP wrapper shape into a 2-key
    //              `@param`; a list-of-records into a one-record `@param` (3 sites, one
    //              callee); an 11-key options builder into a 10-key post-filter (2
    //              sites), short exactly the key the caller always passes.
    //   513 → 528  issue #293, template bounds read as upper-bound contracts. Measured
    //              on the branch rebased onto the #288 merge, so the two waves are
    //              disjoint by measurement rather than assumption. 12 sites pass lists
    //              holding `null` or ints to a `@template T of list<string>` assertion
    //              helper; 2 pass the wrong shape to its `list<int>` sister; 1 is a
    //              genuine defect rather than a loose annotation — a constant asserted
    //              as string membership at one site and int at its sibling — predicted
    //              to take 2 findings with it when fixed. Seeded at what the corpus
    //              says today, not at what it should say.
    //   528 → 526  2026-08-09, and the corpus owner did this, not the analyzer: the
    //              predicted fix landed and took exactly its two findings. The
    //              prediction landing on the nose is why this was a one-line edit
    //              rather than a re-triage.
    //   526 → 536  2026-08-15, two mechanisms rather than one. Measured as a
    //              finding-level diff: the 526-seed commit (86df4c6), re-run today
    //              against the same checkout, still reports exactly 526, so the delta
    //              is the analyzer's. Twelve rows are new; two are the same two sites
    //              re-rendered (`mixed` -> `int|string` inside a shape), which is what
    //              makes twelve a +10.
    //              · 6 sites — an undeclared key under a sealed shape, the class the
    //                `composer/composer` entry records, from the same array-literal
    //                work (issue #327: a literal keeps its fact when its elements do
    //                not). A two-key literal into a one-optional-key `@phpstan-param`
    //                (2); an extra key into a six-name `@param` (1); a list of records
    //                into a `@param string[]` (3), at three more sites of a call whose
    //                fourth was already inside the 526.
    //              · 4 sites — a `string|bool` with no carrier until ADR-0085's
    //                `Fact::Union`: one base per fact meant the argument fell back to
    //                an unjudgeable `mixed`, and Maybe is silence. The value is a
    //                `string` at runtime, so this is the numeric-string archetype this
    //                table's own doc names. The two re-rendered rows are the same
    //                mechanism moving a spelling and not a count.
    //              Both families' fallout predates the reseed: `corpus.local.toml` is
    //              gitignored, so the agent worktrees that landed #303, #327 and #341
    //              measured the public packages only — a hole in the workflow, not in
    //              the analyzer. See [`EFFECT_EXPECTED`]'s row.
    //   536 → 539  2026-08-16, ADR-0057 T1 (issue #378): a static factory now rebinds
    //              an exact receiver, so three method calls that resolved to nothing
    //              before are judged against their `@param`. All three are the same
    //              shape and all three are true positives: a test deliberately hands
    //              `false` to a `@param string` / `@param (int|string)` method to
    //              exercise the callee's own assertion (the corpus marks each with a
    //              `@phpstan-ignore-next-line`), and the third row is that `false`
    //              flowing on inside the callee's descent to a private helper's
    //              `@param string`. Measured as a finding-level diff against the
    //              same-day baseline run: exactly these three rows are new, nothing
    //              moved elsewhere. Seeded at what the corpus says today.
    //   539 → 543  2026-08-16, the value IR carrying method calls (issue #386): an
    //              array literal with a method-call element no longer collapses to
    //              an unrepresentable `Other`, so four shape literals now meet the
    //              sealed `array{…}` they are declared against — two `@return`
    //              shapes and one `@param` shape at two call sites — and each
    //              carries a key the declaration does not name. The same
    //              undeclared-key-under-a-sealed-shape family the 526 → 536 row
    //              triaged, and true positives by the same PHPStan reading (a
    //              sealed shape admits no extra key). Finding-level diff against
    //              the #385 run: exactly these four rows; one `throw.*` row on
    //              phpstan-src (local) went away (21 → 20), a removal the tripwire
    //              does not gate on.
    //   543 → 551  2026-08-16, issue #391's `phpdoc.maybe-argument-mismatch` (the
    //              Asserted-premise half of the possibly-grade argument pair,
    //              strict floor, counted here because it is `Layer::Contract`).
    //              Eight rows, one shape: a builtin's `string|false` /
    //              `string|bool` / `int|null` (`file_get_contents`, `tempnam`,
    //              `realpath`, a `T|false` return read through a docblock) handed
    //              straight into a native `string`/`int` parameter under
    //              `strict_types` — the same shape the public 9 carry. Finding-level
    //              diff against the #388 run: exactly these eight rows plus the
    //              possibly-grade rows below; no other id moved (repair A's
    //              `assert(... instanceof)` widening added nothing here).
    //   551 → 553  2026-08-17, issue #423 (builtin parameter types): two
    //              `phpdoc.maybe-argument-mismatch` rows, one shape — a
    //              `tempnam()`-family `non-falsy-string|false` handed straight to
    //              `file_put_contents()` / `unlink()`'s native `string` under
    //              `strict_types`, the builtin twin of the shape #391 seeded. Both
    //              read against the source: neither is guarded. Finding-level diff
    //              against the same-day master run: exactly these two rows and the
    //              one proof-layer row baselined in `EXPECTED_PROOF_FINDINGS`;
    //              nothing else moved.
    ("pxxxx-monorepo", 553),
];

/// The expected `phpdoc.*` count for a package/local-project name (0 if untabled).
fn phpdoc_expected(name: &str) -> usize {
    PHPDOC_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for `throw.*` findings (ADR-0040/0007), identical in
/// spirit to [`PHPDOC_EXPECTED`]: an undeclared **checked** throw escaping a
/// written `@throws`, or a Liskov-widened override, is a contract-layer claim
/// about the code's own documentation, not a runtime-breakage proof. Such
/// findings saturate working code (the checked-exception volume ADR-0007 keeps
/// quiet by default), so they are held in measurement mode and gate only as a
/// per-package **increase** tripwire. Packages absent expect **zero**.
///
/// Seeded from the first landing run of the throw system (ADR-0040); the monorepo
/// count is dominated by two pervasive base exceptions thrown far below
/// `@throws`-annotated controllers. It rose 35614 → 43963 with the closure wave
/// (ADR-0033), which propagates throws through higher-order-builtin callbacks and
/// body-local `$fn()` closures that were previously opaque taints — triaged
/// 5-sample, every one a TRUE undeclared checked throw through a real callback
/// edge, with the by-ref-invalidation guard keeping `$fn()` resolution sound.
// Reconciled to actual after closure-wave Stage D (interface/parent `@throws`
// Liskov + `implements` lowering): the increases are new `throw.liskov-widened`
// (phpunit +4, pxxxx +1 — an override declaring a narrower exception than the
// abstraction it implements), and the decreases (symfony/console 12→10, nikic
// 2→1) are `undeclared` counts that fell because lowering `implements` enriched
// the class chain, letting subtype/absorption checks resolve where they widened.
const THROW_EXPECTED: &[(&str, usize)] = &[
    ("composer/composer", 93),
    ("sebastianbergmann/phpunit", 84),
    ("guzzle/guzzle", 2),
    ("Seldaek/monolog", 7),
    ("symfony/console", 10),
    ("thephpleague/flysystem", 3),
    ("nikic/PHP-Parser", 1),
    // Registered 2026-07-24 (v0.1.0 run, oracle idea A): PHPStan's own `src/` as a
    // local project. First run 0 proof-layer / 0 phpdoc.* / 20 throw.undeclared,
    // triaged verbatim — homogeneous checked-exception debt (a `ShouldNotHappen`
    // escaping a narrower `@throws`), the exact ADR-0040/0007 pattern tripwire mode
    // exists for.
    //   20 → 21  2026-07-26, the checkout advanced (192 src/ files). One rewritten
    //            extension grew a second `ShouldNotHappenException` escaping a caller
    //            declaring something else. Triaged against the pre-advance tree: that
    //            file held one before and two now, the other 20 unchanged in file,
    //            line and escaping method — a transitive catch, which is what the
    //            damming machinery is for.
    //   2026-08-08 (#186): scope moved from an `exclude` denylist to the positive
    //            `paths = ["src", "vendor"]` key — PHPStan's `tests/` is that
    //            project's own rule-fixture corpus, INPUTS written to be broken, so
    //            outside a gate whose bar is zero FPs on working code (ADR-0079 §2.3
    //            applied to a whole tree). Re-measured unchanged at 21 / 0, because
    //            the denylist already named the same directories. The key buys
    //            enforcement: an allowlist cannot silently readmit a fixture tree
    //            added later, a denylist can.
    ("phpstan/phpstan-src", 21),
    // Ledger. Every move on this row has been corpus state, not a checker change,
    // and the standing class is the same: an `@throws`-annotated declaration with an
    // undeclared base-exception escape, arriving with new application code. The
    // proof layer stayed 0 across all of them.
    //   43964 → 44372  2026-07-24, live-tree drift (~210 files gained: 84,038 →
    //                  84,248). 3-sample verbatim triage, all TRUE.
    //   44372 → 44343  2026-08-01, the standing DOWNWARD drift, reseeded in its own
    //                  pass (never inside a fix commit). Observed unchanged across
    //                  every session since 2026-07-25 — cross-commit stability plus
    //                  the #63 determinism fix rule out a checker cause.
    //   44343 → 44374  2026-08-05, the corpus-state movement `PHPDOC_EXPECTED`
    //                  records in the same run, established the same two ways (every
    //                  public count exact; the gate at the 44343-seed commit
    //                  reproduces 44374 today). NOT triaged finding-by-finding — at
    //                  this volume the attribution is the only honest evidence, and a
    //                  finding-level diff needs a corpus state nobody retained.
    //   44374 → 43886  **Reseed 2026-08-09, downward**, with the corpus pin, and this
    //                  one has a direct attribution: the measured revision replaces a
    //                  bespoke exception class with concrete assertions across the
    //                  tree, and an assertion where a `throw` used to be is exactly
    //                  one `throw.undeclared` fewer. A baseline 488 above the truth
    //                  would swallow the next 488 real regressions in silence.
    ("pxxxx-monorepo", 43886),
];

/// The expected `throw.*` count for a package/local-project name (0 if untabled).
fn throw_expected(name: &str) -> usize {
    THROW_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for the `effect.*` **contract** ids
/// (`effect.envelope-exceeded`, `effect.liskov-widened`, and since issue #311
/// `effect.interop-unknown-label`), the ADR-0050 §9 recorded delta: these gate as
/// the same per-package **increase** tripwire as [`PHPDOC_EXPECTED`] /
/// [`THROW_EXPECTED`], matching their declared-contract semantics (a proven
/// behavior exceeds an envelope the code *declares* about itself; it still runs).
///
/// Seeded **empty** in 2026-08, on the reading that no ADR-0006 envelope exists in
/// the corpus. The reading was about *Steins* annotations: all three ids fire on
/// upstream's spellings too — `@phpstan-impure` since #311, and the envelope pair
/// since #303 taught the scanner `@phpstan-all-methods-pure` — so "no envelopes in
/// the corpus" stopped being true the day it was read for upstream's vocabulary.
const EFFECT_EXPECTED: &[(&str, usize)] = &[
    // **0 → 4442, 2026-08-15**, the first row in this table and it carries no Steins
    // annotation: the private monorepo declares `@phpstan-all-methods-pure` on three
    // classes, and #303 (master 7b3ecab, 2026-08-12) made that an operative bound.
    // Measured, not inferred — a binary at 93eff42 (the merge before #303) reports 0
    // over the 5041-file subtree holding all 4442; one at 7b3ecab reports 4442.
    //
    // NOT the resource-value run (#341), which the first reading blamed: findings
    // over that subtree are byte-identical on both sides of it (27,332 = 27,332),
    // and the gate re-run at 040658c — the commit whose record claims green at
    // 526/0 — reproduces 536/4442 today. `corpus.local.toml` is gitignored, so an
    // agent worktree measures public packages only; this was simply the first run
    // with the private corpus mounted since 2026-08-12.
    //
    // One root, one shape. Names below are **placeholders**, since the corpus's own
    // identifiers are not written into this repo — do not grep for them. Every
    // finding sits in one of the three declaring classes (`MyApp\Route\UrlBuilder`
    // 4376, `MyApp\Util\Validator` 54, `MyApp\Util\Filter` 12), across 795 call
    // sites in 555 declared-pure methods, and each reaches the same house logger
    // `MyApp\Log\Debug`: an assertion helper delegates to `Validator`, which
    // delegates to `Filter`, whose `true`-rejecting arm logs. One finding per
    // (label, origin) group, six for most sites — which is how 795 becomes 4442.
    //
    // TRUE, and the corpus says so in its own source: that logging call carries a
    // `@phpstan-ignore impure.methodCall`, so upstream reports the same violation at
    // the same line and suppresses it there. PHPStan then reads the callee as pure
    // for everyone above it; the proven lane does not (ADR-0067 — a declaration
    // neither manufactures nor erases a finding), so the effect travels to every
    // caller declaring purity over it.
    //
    // What the count is made of: 2148 `nondet.time` (a datetime utility wrapping
    // `\time`/`\date`), 716 `nondet.random` (`mt_rand`, the log's sampling gate),
    // 716 `global.read` (a `getenv`), 86 `io` (a `file_exists`), and 776
    // `io.output.buffer` — the one soft class, where `print_r($x, true)` /
    // `var_export($x, true)` are pure in return-mode and the source proves the
    // `true` at each of the three origins. That label is this catalog's
    // deliberately arg-blind row, which `effect_labels`' own doc already calls an
    // over-approximation; the fix is a sibling of `narrowed_stream_labels` and will
    // move this row DOWN, which never trips the tripwire.
    //
    // Seeding rather than fixing is the zero-FP posture, not a retreat from it: the
    // violations are real, and what makes 4442 of them is a house logger three hops
    // under an assertion helper — the shape ADR-0084's `[effects]` attribution
    // exists to discharge on the corpus owner's side. Until then this row's job is
    // to notice the 4443rd.
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
/// [`PHPDOC_EXPECTED`] / [`THROW_EXPECTED`] / [`EFFECT_EXPECTED`]; absent = zero.
///
/// A count and not per-finding pins, because a possibly-grade id claims only that
/// *a* path reaches the site — the shape defensive house styles produce on purpose,
/// and why the registry floors these at `strict`. Pinning each would have meant
/// hundreds of rows asserting that working code works. (A definite proof id claims
/// the program breaks, so its count belongs at zero with each exception triaged
/// into [`EXPECTED_PROOF_FINDINGS`].)
///
/// Seeded 2026-08-09 from the binding-presence run, at the revisions
/// `corpus.lock.toml` / `corpus.local.toml` record. The five classes that triage
/// found, so a reader knows what a count is made of:
///
/// 1. **Conditional binding** (`if`/`elseif` with no `else`) — TRUE, the id's point.
/// 2. **A loop variable read after its loop** — TRUE; the zero-iteration path
///    really does reach it unbound.
/// 3. **Correlated conditions** — bound under `if (count($c) > 1)` and read under a
///    textually identical one whose body reassigns `$c` between. TRUE under the
///    syntactic reading this id is defined over; proving the conditions agree is
///    path feasibility, which nothing here attempts.
/// 4. **A never-returning callee** (`$this->fail()`, `markTestSkipped()`) — FALSE.
///    `stmt_end` reads a statement-position call as falling through because
///    deciding otherwise needs the project index; ADR-0081 §9 defers the refinement
///    to the emitter side, where the index lives.
/// 5. **A binding in an argument of a throwing call inside `try`** — FALSE. PHP
///    evaluates arguments before entering the callee, so the binding is done before
///    anything can throw; the pass weakens at statement granularity.
/// 6. **A builtin's `T|false`/`T|null` into a native `T`**
///    (`type.maybe-argument-mismatch`, issue #391) — TRUE, and the first class in
///    this bucket that is not a binding claim: the argument's own declared type
///    has an arm the parameter rejects. Only the all-`Verified` half lands here;
///    an `Asserted` arm routes the same judgment to `phpdoc.maybe-argument-mismatch`
///    and thus to [`PHPDOC_EXPECTED`].
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
    // 2 → 3, 2026-08-17 with issue #423 (ADR-0056 §9): the wave's one proof-layer
    // row, and the one that pays for the whole slice. `CarbonInterval::
    // createFromFormat(string $format, ?string $interval)` calls
    // `explode($match[1], $interval)` at `CarbonInterval.php:739` — a NATIVE
    // `?string` straight into `explode`'s native `string $string`, so the premise
    // is all-`Verified` and the id is `type.maybe-argument-mismatch`. The
    // normalization that would have saved it (`$interval ??= '';`) sits four lines
    // BELOW the call, inside a `preg_match` branch the null value reaches, so
    // `createFromFormat('H:i:s.v', null)` fatals on a released, strict-types
    // library. TRUE, unguarded, and reachable through the public API — exactly the
    // shape a builtin parameter surface exists to see.
    ("briannesbitt/Carbon", 3),
    // 0 → 1, 2026-08-16 with issue #391 — and a sixth class, the first that is not
    // a binding claim at all: `type.maybe-argument-mismatch` on
    // `PrettyPrinter/Standard.php:1100`. `preg_replace()` declares `string|null`
    // (natively, so the premise is all-`Verified` and the finding is proof-layer),
    // and `$escaped` goes straight into `indentString(string $str)`. TRUE at the
    // possibly grade: PCRE answers `null` only on a pattern/backtrack failure, so
    // the arm is real and its inhabitation on a live path is exactly the part this
    // grade does not claim. The package's other issue #391 finding rides an
    // `Asserted` premise and is counted in PHPDOC_EXPECTED instead.
    ("nikic/PHP-Parser", 1),
    // 10 — 8 `variable.maybe-undefined` (classes 1 and 4) plus the 2
    // `type.return-maybe-missing` rows absorbed from `EXPECTED_PROOF_FINDINGS`.
    // 10 → 11, 2026-08-16 with issue #391: one `type.maybe-argument-mismatch` —
    // a `string|null` (a nullable native declaration) handed to a native `string`
    // parameter; the null arm is real, its inhabitation is what the grade does
    // not claim.
    ("phpstan/phpstan-src", 11),
    // 120 (111 `variable.maybe-undefined` over 85,282 files + 9 absorbed
    // `type.return-maybe-missing`) → 121, 2026-08-14 with issue #330 PR2:
    // `array_merge` joined the fold allowlist, so it stopped being an uncatalogued
    // name whose by-ref parameters might WRITE an argument, and an argument read
    // became a read (the issue #77 bind-free distinction). The row is a data
    // provider returning `array_merge($a, $b)` where `$b` is bound only inside a
    // `foreach` — the third sibling of two already-baselined rows of that shape in
    // the same function, not a new class of claim. TRUE at the possibly grade.
    // 121 → 131, 2026-08-16 with issue #391: ten `type.maybe-argument-mismatch`
    // rows on a Verified premise — a natively declared `string|null` / `int|null`
    // / `string|false` handed to a native `string`/`int` parameter under
    // `strict_types` (four sites are one helper called with the same nullable
    // local four times). Every pre-existing `variable.maybe-undefined` and
    // `type.return-maybe-missing` count is unchanged.
    ("pxxxx-monorepo", 131),
];

/// The expected possibly-grade count for a package/local-project name (0 if
/// untabled).
fn possibly_expected(name: &str) -> usize {
    POSSIBLY_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// A triaged TRUE proof-layer positive the corpus legitimately contains: real
/// broken code Steins correctly proves. Unlike measurement-mode `phpdoc.*`/
/// `throw.*`, this is runtime-layer (standing bar: zero, ADR-0013), so an
/// entry is a recorded exception matched at finding precision (package + id +
/// path + line + message fingerprint) — any drift re-reds the gate.
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

/// Triaged TRUE proof-layer positives (ADR-0043 §5). Each row's triage is in
/// the comment beside it; adding a row is a conscious, orchestrator-visible act.
const EXPECTED_PROOF_FINDINGS: &[ExpectedProofFinding] = &[
    // monolog's own test deliberately constructs `new MongoDBHandler(new
    // \stdClass, …)` and asserts the TypeError; `stdClass` is a mined root with
    // no supers (ADR-0043), so it is-a-NOs the `Client|Manager` union. TRUE.
    ExpectedProofFinding {
        package: "Seldaek/monolog",
        id: "type.argument-mismatch",
        path_suffix: "tests/Monolog/Handler/MongoDBHandlerTest.php",
        line: 27,
        // Source-cased since `TypeMember::Instance` grew its `display` field
        // (diagnostics render the declared casing; matching stays lowercased).
        message_contains: "cannot become MongoDB\\Client|MongoDB\\Driver\\Manager",
    },
    // ADR-0049 S2: `call.undefined-method` fired 10 times, all monorepo, all
    // static calls into a final/trait-free/fully-enumerated chain — genuine
    // `Error: Call to undefined method` fatals. TRUE, all 10. OSS packages:
    // zero. A DAO batch calls a legacy accessor removed/renamed from its
    // final DAO class.
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
    // `OAuth2Model::checkPassword()` is called statically, but exists only as an
    // instance method on the caller `OAuth2ClientModel`; `OAuth2Model` is final
    // with no such method — genuine undefined-static-method fatal.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "util/src/Model/Auth/OAuth2ClientModel.php",
        line: 106,
        message_contains: "OAuth2Model::checkPassword() — hierarchy fully enumerated",
    },
    // ADR-0049 S5: `call.too-few-arguments` fired twice on the monorepo, both
    // TRUE ArgumentCountErrors (two grouped-`use` `Query::__construct` FPs from
    // an unlowered import are fixed in the paired commit and no longer fire).
    // An admin mail-preview handler calls `getEmailExamples($lang)` with no args.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.too-few-arguments",
        path_suffix: "email_preview.php",
        line: 64,
        message_contains: "getEmailExamples(): 0 passed, 1 required",
    },
    // A test script calls `requestToAllAppApiEndpoints($host, $header, $token)`
    // with one argument — ArgumentCountError (1 passed, 3 required).
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.too-few-arguments",
        path_suffix: "test/testall.php",
        line: 14,
        message_contains: "AppApi_Testing::requestToAllAppApiEndpoints(): 1 passed, 3 required",
    },
    // ADR-0078 #190: `dataProviderThatTriggersPhpError` (`$foo = []; $foo->bar();`)
    // is issue 5451's reproduction fixture — genuinely fatals by design. TRUE.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "call.on-non-object",
        path_suffix: "regression/5451/Issue5451Test.php",
        line: 20,
        message_contains: "proven array on this path",
    },
    // ADR-0078 #184: issue 6294's reproduction fixture deliberately weakens a
    // parent's visibility to observe the engine fatal. TRUE, same class as above.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "override.visibility-weakened",
        path_suffix: "regression/6294/B.php",
        line: 17,
        message_contains: "weakens the visibility",
    },
    // ADR-0078 #187: `array.duplicate-key` fired 19 times, all monorepo, all
    // TRUE: 1 config key overwritten (dead value), 12 duplicate allowlist ids,
    // 1 series-options key, 1 analytics path key, 3 test-fixture keys, 1
    // view-parameter key. Mechanics/red-on-sight (ADR-0050 §1); a corpus
    // reseed may move these lines. A config accessor literal binds
    // 'x_restricts' twice below; the earlier value is dead.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Illust/Common.php",
        line: 2495,
        message_contains: "array key 'x_restricts' is declared twice",
    },
    // An append-grown integer allowlist literal: 12 ids duplicated across its
    // history, each overwriting an identical value — churn, no information.
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
    // A test fixture rebinds 'illust_sanity_level' 3x across 3 literals — copy-paste drift.
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
    // ADR-0078 #183: the declaration-fatal tracer's one corpus finding, TRUE.
    // A ClockMock test double extends a `final` class, relying on ext-uopz to
    // strip `final` at runtime; Steins's analyzed PHP has no uopz loaded, so the
    // fatal is real. Issue #205 tracks demoting this once the sidecar reports a
    // final-stripping extension.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "class.extends-final",
        path_suffix: "tests/lib/ExDateTimeImmutableMock.php",
        line: 14,
        message_contains: "cannot extend final class ExDateTimeImmutable",
    },
    // Docblock hygiene (ADR-0078 / issue #186), triaged 2026-08-08: six
    // mechanics ids, red-on-sight. All 11 public-corpus sites read TRUE at
    // source; the private corpus is measured only, not pinned here. The
    // fuzzing driver captures `$lexer` into a closure that never reads it
    // (works off the parser it also captures) — a dead, by-value capture.
    ExpectedProofFinding {
        package: "nikic/PHP-Parser",
        id: "closure.unused-use",
        path_suffix: "tools/fuzzing/target.php",
        line: 111,
        message_contains: "`use ($lexer)` is never read",
    },
    // The mock generator stacks three `@var` docblocks above one `return`.
    // ADR-0073: only the LAST of a run adopts, so `$className` and `$type`
    // (the first two) are inert — correctly silent on the third. TRUE.
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
    // The same virtual-parameter idiom as the symfony group below, in a
    // vendored `symfony/filesystem` copy inside a fixture tree:
    // `tempnam($dir, $prefix/*, $suffix=''*/)` documents `@param $suffix` for
    // an argument read via `func_get_arg(2)`. TRUE. Pinned, not
    // vendor-suppressed, because pinned packages are analyzed whole
    // (ADR-0015's vendor split is local-project only).
    ExpectedProofFinding {
        package: "composer/composer",
        id: "phpdoc.stale-param",
        path_suffix: "installed-versions2/vendor/symfony/filesystem/Filesystem.php",
        line: 586,
        message_contains: "`@param $suffix` names no parameter",
    },
    // Two deliberate idioms that still leave a tag declaring nothing:
    //   * `@return list<\SIG*>` wildcards the `SIG*` constant family — no
    //     PHPDoc grammar admits it (PHPStan's parser rejects it too). TRUE.
    //   * `SymfonyStyle`'s progress helpers document a virtual `$format` param
    //     (real signature comments it out for BC, read via `func_get_arg(1)`) —
    //     names no parameter of the declaration. TRUE; PHPStan reports
    //     `parameter.notFound` on the same shape.
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
    // Three inert `@var` casts in `MountManager`, two shapes: `move()`/`copy()`
    // (245, 262) follow a real `/** @var */` with a single-star `/* @var */`
    // (a plain comment), breaking ADR-0073 adjacency; `determineFilesystemAndPath()`
    // (358) stacks two docblocks, shadowing the first (phpunit shape above). TRUE.
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
    // The monorepo's five, verified at source 2026-08-08, all TRUE. Path
    // suffixes are cut to the shortest 1:1-keying fragment (private-corpus
    // naming rule) — a checkout re-cut can move a line and re-red the gate,
    // which is the pin working, not drift to paper over. A `@phpstan-param`
    // below names a parameter the signature lacks — refactor rot.
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
    // Two `@var` casts as the last statement of a branch, meant for the code
    // after `}` — but ADR-0073 next-statement adoption ends at the brace, so
    // both are inert as written.
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
    // A pseudo-tuple `@return [$total, $illust_ids]` — no PHPDoc grammar admits
    // it, so the tag declares nothing.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.unparsable",
        path_suffix: "Controller/V1SearchWorks.php",
        line: 91,
        message_contains: "does not parse",
    },
    // `type.return-missing` (ADR-0078 §5, issue #199), triaged 2026-08-08: every
    // row is a deliberately-stub test double — an empty/never-returning body
    // carrying a real return type, which would fatal if invoked. Not bugs, not
    // FPs — fixtures. Its `maybe-` sibling moved to `POSSIBLY_EXPECTED` under
    // ADR-0081 §8 (a possibly-grade id claims only that *a* path reaches the site).

    // Two macro bodies registered only so the PHPStan extension under test can
    // read their declared return type; the closure body is `{}`, never called.
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
    // Eight `FnStream`/`MockHandler` decorator closures whose body is
    // `self::fail(...)` — reaching one IS the test's failure condition.
    // `Assert::fail(): never` would silence these, but PHPUnit is outside
    // guzzle's analysed universe here. Expected to disappear once cross-package
    // callee resolution reaches them — that disappearance is a gate event.
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
    // A `tests/_files` fixture implementing `Event` with two empty method
    // bodies carrying the interface's declared return types.
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
    // `NonStringInput`: an `Input` subclass whose three overrides are empty
    // bodies carrying the parent's return types; the test drives the listener,
    // never these methods.
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
    // `variable.undefined` (ADR-0078, issue #194), triaged 2026-08-08. Eleven
    // TRUE positives — reads of a name bound nowhere in scope. Two FP-shaped
    // sites from the same run (a same-variable `empty($x)?:` ternary) are fixed
    // at source (both isset/empty arms now shield) and not pinned here. The ten
    // monorepo ones: 4 exception-message typos (incl. `$this->$mode` for
    // `->mode`, `$withd` for `$width`), 1 renamed logger field, 1 KVS setter
    // always storing 0 (live defect), 2 accidental-pass `$offset` reads
    // (`array_splice(…, null)` degrades gracefully), 1 deleted-parameter read,
    // 1 stale fixture. Path suffixes are kept short — full paths carry the
    // private project's name. `$a = $b;` below (no `$b` anywhere) is written
    // to make PHPUnit observe a PHP warning; the `@`-suppressed twin beneath
    // stays silent, correctly.
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
    // The two `$offset` reads below rely on that same accidental degrade.
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
    // Builtin parameter types (issue #423): a date helper forwards its
    // `$timestamp` to `\date()`, and its own test hands it a non-numeric string
    // on purpose — the test's `expectException(TypeError::class)` names exactly
    // this "must be of type ?int, string given". A non-numeric string is a
    // TypeError for an `int` parameter in BOTH modes, so the descent-bound
    // literal convicts the forwarding line. TRUE, and asserted by its own test.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "type.argument-mismatch",
        path_suffix: "Util/DateTime.php",
        line: 65,
        message_contains: "to date() cannot become ?int $timestamp",
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
    // optional (gitignored) `corpus.local.toml` is analyzed like a package;
    // vendor files are indexed but their findings don't count.
    let locals = corpus_local::read_local()?;
    let mut local_reports: Vec<PackageReport> =
        locals.par_iter().map(analyze_local).collect();
    local_reports.sort_by(|a, b| a.name.cmp(&b.name));

    // Measurement-mode regression tripwires (see `PHPDOC_EXPECTED` /
    // `THROW_EXPECTED`): a package regresses iff its count exceeds the baseline.
    let regressions = phpdoc_regressions(&reports, &local_reports);
    let throw_regressions = measurement_regressions(&reports, &local_reports, "throw", |r| r.throws.len(), throw_expected);
    // ADR-0050 §9 delta family: `effect.*`-contract findings gate as an increase
    // tripwire too, same shape as `phpdoc.*`/`throw.*`.
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

    // RED on any proof-layer finding (package + local non-vendor diagnostics;
    // vendor never gates, ADR-0015) OR any measurement-mode regression.
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
// packages it analyzes — which carries state between projects, so every use
// must go through [`check_under_target`] (see its doc for what forgetting cost).
thread_local! {
    static FOLDER: RefCell<SidecarFolder> = RefCell::new(SidecarFolder::enabled());
}

/// Check `project` on the resident folder, configured for **this** project's
/// declared PHP target (issue #28).
///
/// RULE: this is the only way to reach `FOLDER`. A resident folder reused
/// across projects keeps the previous project's `php_target`, which gates
/// ADR-0056 curated-fact admission and the absence family, so skipping this
/// call silently changes which facts get seeded. Issue #63 is exactly that:
/// `analyze_local` judged each local project under whichever leftover target
/// its rayon worker held, swinging the local corpus's `phpdoc.*` count
/// 536↔483 run to run (invisible under `RAYON_NUM_THREADS=1`), and cost two
/// sessions of triage before this was found to be the cause.
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

    // Identify parse-error files (their diagnostics are excluded from the
    // count). ADR-0079 (#180): mostly redundant now (a failed-parse file emits
    // only `syntax.unparsable`, nothing else to drop) but still a deliberate
    // blind spot for that id — a pre-existing unparsable file can't red the
    // gate on a remedy that lives elsewhere. What it does NOT drop: a
    // non-vendor unparsable file dams the existence family for that package,
    // only ever lowering counts.
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
    // family and curated-fact admission — the gate measures the analyzer as
    // the CLI ships it, so each package's own `require.php` applies. The
    // resident folder drops target-dependent memos on the change, keeping
    // cross-package reuse sound.
    let php_target = layout.php_target().cloned();
    let plugins = steins_db::PluginFacts::discover(&layout, None);
    let project = Project::new(&db, inputs, layout, plugins);
    let mut diags: Vec<Diagnostic> = check_under_target(&db, project, php_target);
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

    // Read the tree's state BEFORE walking it, so revision/cleanliness match
    // the count they're reported beside. Both degrade to "unknown" rather
    // than failing.
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
    // by fixtures alone (`crates/steins-infer/tests/parse_failure_dam.rs`)
    // until a NON-vendor break appears here, at which point these counts can
    // only fall, never rise.
    let mut parse_error_files = Vec::new();
    for &input in &inputs {
        if !parse(&db, input).parse_errors().is_empty() {
            parse_error_files.push(input.path(&db).to_owned());
        }
    }
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let layout = composer::discover(&[root.to_path_buf()], root);
    // A local project declares a target like any other (issue #63): this used
    // to go straight to the resident folder without setting it.
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

    // `throw.*` contract-layer ids (ADR-0040), counted against `THROW_EXPECTED`,
    // gating only on increase. Volume is far larger than `phpdoc.*` (checked-
    // exception saturation), so only counts and a small sample print.
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

    // `effect.*` contract ids (ADR-0050 §9 delta). Suppressed while dormant —
    // prints nothing unless a finding lands, the table is seeded, or a
    // regression trips — kept the report byte-identical pre-convergence. Off
    // since 2026-08-12, when #303's interop-envelope run made the private
    // monorepo's purity tags fire (see [`EFFECT_EXPECTED`]'s seeded row).
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

    // Possibly-grade proof ids (ADR-0081 §8), the `strict`-floored rows,
    // counted against `POSSIBLY_EXPECTED`, gating only on increase. Every
    // finding prints — the volume is triageable and there's no prefix to
    // search under.
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
