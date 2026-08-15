//! `nsrt`: the assertType harness (oracle idea B).
//!
//! Consumes phpstan-src's `PHPStan\Testing\assertType('Type', $expr)` corpus
//! (`tests/PHPStan/Analyser/nsrt/`) as an inference oracle: PHPStan asserts the
//! type it infers for `$expr`; this harness compares Steins' rendering against
//! it and ranks the resulting gaps.
//!
//! Recognition extends the D3 dump-family seam (`steins_infer::collect_assert_types`):
//! `assertType` matched by resolved FQN, `$expr` rendered through
//! `PHPStan\dumpType`. Harness-only — a normal `check` never recognizes it.
//!
//! Each nsrt file is a standalone single-file universe, analyzed as a separate
//! project sharing one resident sidecar folder.
//!
//! Five-verdict taxonomy (see [`classify`]):
//!
//! - `match` — equal after normalization (case, `|` order, nullable forms,
//!   int-range spelling).
//! - `unsupported` — expected uses vocabulary Steins deliberately does not model
//!   (`*ERROR*`/`*NEVER*`, non-array generics, intersections, arbitrary
//!   subtraction, `object`, …), named by pattern. Since S1.5 (ADR-0062) the array
//!   vocabulary is no longer here; nor is `mixed` (issue #239).
//! - `equal` — proven-equal-but-differently-spelled (issue #172): the relation
//!   proves both `expected ⊇ got` and `got ⊇ expected` (`Certainty::Yes`) while
//!   strings differ. Canonical case: D4-native spelling pairs (`array{X}` vs
//!   `list{X}`).
//! - `subsumed` — Steins strictly more precise: a proper subtype of the assertion
//!   (issue #47). `equal` is claimed first.
//! - `differ` — semantically different (the gap inventory), including `unknown`
//!   against a concrete assertion (a reach gap). An asymmetric relation answer
//!   stays here.
//!
//! ## `subsumed` vs `differ`/`match`
//!
//! PHPStan asserts `bool` for `in_array('foo', ['foo','bar'])` (declines to
//! fold); Steins proves `true`, admissible under `bool`. Scoring it `differ`
//! would penalize precision gains as folding widens (#39, ADR-0061).
//! [`subsumption_directions`] asks `normalize::subsumes` both ways via
//! `steins_contract::lower_str` — the same relation used for param
//! contravariance/return covariance and ADR-0056's envelope check. Only the
//! strict covering direction earns `subsumed`; laundering the reverse would
//! turn widening regressions into false precision (pinned by
//! `reverse_direction_is_never_subsumption`).
//!
//! ## `mixed`: measured, never `subsumed` (issue #239)
//!
//! `ContractTy::Mixed` spells `mixed`; `MixedMinus` spells its two cuts, so the
//! old `unsupported` listing was stale. Measured (phpstan-src `55a7732`, 329
//! `mixed` rows): 324 render `unknown`, five a concrete type, zero `mixed` —
//! almost entirely reach, now `differ`. [`expected_is_top_type`] vetoes an
//! expected `mixed`: as the top type, `expected ⊇ got` is unconditionally
//! `Yes`, so `subsumed` there would just name the oracle's silence (`match`
//! still claims genuine `mixed`/`mixed` pairs first). Of the five: one
//! (`bug-14333.php:167`, missed by-ref invalidation) is held out by
//! [`crosses_int_float`]; three (`unresolvable-types.php:17,18`,
//! `invalid-type-aliases.php:13`) are unresolvable phpdoc rendered as a class
//! name; one (`bug-13282.php:40`) is a genuine precision claim lost to the
//! veto. Pinned by `mixed_is_measured_never_subsumed`.
//!
//! ## `mixed~…`: reach gap in vocabulary's clothes (issue #237)
//!
//! `subtraction` (158 rows at #237, 133 `mixed~…`): 44 are exact re-spellings
//! of a cut Steins already holds (`mixed~null` = `MixedMinus(Null)`, 33;
//! `mixed~(0|0.0|''|'0'|array{}|false|null)` = `MixedMinus(Falsy)`, 11; 8 more
//! have a plain-union complement). 154/158 render `unknown` — closing the
//! spelling would move `unsupported` → `differ` and award nothing (a sentinel
//! asks no direction). The four remaining rows cap the slice at +1: three are
//! class/enum subtractions rendering the wider un-narrowed base; one
//! (`bug-8249.php:19`) would earn `subsumed` from body-return inference, not
//! subtraction. Ceiling: one row, needing a cut (`Int`, asked for 16 times)
//! `ContractTy::MixedMinus` can't construct outside `lower_str`'s two literal
//! keywords — recorded in ADR-0030's registry (entry 6) rather than built. The
//! #239 veto never reaches these rows: `unsupported_pattern` claims every `~`
//! first. Pinned by `subtraction_is_gated_before_the_top_type_veto_is_reached`
//! and `the_two_cuts_stay_spellable_and_judged`.
//!
//! ## Version-gated fixtures: not analyzed at all (issue #356)
//!
//! 448 of the 1,617 nsrt fixtures open with a `// lint <op> <version>` gate *on
//! the `<?php` line*, naming the PHP range under which PHPStan's assertions in
//! that file hold. Steins folds through a sidecar running whatever `php` is on
//! `PATH`, so outside that range the assertions are not an oracle: agreement
//! would be luck (an assertion that happens not to be version-sensitive), not
//! confirmation. [`lint_gate`] reads the marker, [`running_php_version`] asks
//! the interpreter, and an excluded fixture is **skipped before analysis** and
//! counted on its own line — never folded into a verdict, so no bucket can
//! absorb it silently. Measured at PHP 8.5 (phpstan-src `55a7732`): 59 files /
//! 619 observations excluded, of which 81 were `match` and 20
//! `equal`/`subsumed` — i.e. the pre-#356 headline was carrying 81 rows it had
//! not earned. Pinned by `lint_gates_parse_off_the_open_tag` and
//! `gates_admit_only_the_versions_they_name`.
//!
//! The gate is per *file* while only some assertions in it are
//! version-sensitive; the honest denominator is still the file, because the
//! marker is the only statement anyone makes about which rows those are.
//! Owner ruling (2026-08-15, #356): file-level exclusion stands — settled, do
//! not re-argue per slice.
//!
//! ## Headline decision (settled here; do not re-argue per slice)
//!
//! `subsumed` does NOT count toward headline `match`: `match` is oracle-confirmed
//! agreement, `subsumed` only unfalsified (a fold bug producing `'bar'` for truth
//! `'foo'` under PHPStan's `string` would land here too). Issue #47 is fixed by
//! rows leaving `differ`, not joining `match`. Reported as
//! `match + equal + subsumed`, a secondary **admissible** figure. `equal` stays
//! out of the headline too — it proves agreement both ways but reproduces the
//! denotation, not the spelling.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use steins_contract::{CKey, ContractTy};
use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_domain::Base;
use steins_infer::{AssertObservation, SidecarFolder, collect_assert_types};

use crate::corpus::{collect_php_files, repo_root};

/// Headroom for [`run`]'s worker thread (issue #246). phpstan-src's own
/// benchmark fixture nests `Node` property-fetch chains 250–1,000 `->next`
/// deep (finite, not a cycle); walking it recurses ~2,500 frames through
/// steins-syntax's `scan_effect_origins`, blowing a debug build's ~8 MiB
/// default stack (release's optimized frames fit fine — previously the only
/// workaround).
///
/// The harness sizes this because the library can't (steins-infer serves an
/// LSP, no worker-thread-per-keystroke); the recursion terminates on its own,
/// so a depth cutoff would manufacture unneeded silence. 256 MiB is ~100x the
/// needed depth — lazily-committed address space, free when unused.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

/// Entry point for `cargo xtask nsrt [DIR]`. `DIR` overrides the default nsrt
/// path. Runs the analysis on a worker thread sized per [`WORKER_STACK_SIZE`]
/// rather than on `main`'s default-sized stack — see that constant's doc.
pub fn run(dir_arg: Option<&str>) -> Result<(), String> {
    let dir_arg = dir_arg.map(str::to_owned);
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || run_on_worker(dir_arg.as_deref()))
        .expect("failed to spawn the nsrt worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn run_on_worker(dir_arg: Option<&str>) -> Result<(), String> {
    let dir = match dir_arg {
        Some(d) => PathBuf::from(d),
        None => default_nsrt_dir(),
    };
    if !dir.is_dir() {
        return Err(format!(
            "nsrt directory not found: {}\n  pass the path explicitly: `cargo xtask nsrt <DIR>`",
            dir.display()
        ));
    }

    let mut files = Vec::new();
    collect_php_files(&dir, &mut files);
    files.sort();
    if files.is_empty() {
        return Err(format!("no .php files under {}", dir.display()));
    }
    // What the sidecar will actually run: the fixtures' `// lint` gates are
    // claims about *that* engine, not about Steins (issue #356).
    let php = running_php_version();
    match php {
        Some((maj, min)) => {
            println!("nsrt: analyzing {} files under {}", files.len(), dir.display());
            println!("nsrt: sidecar PHP {maj}.{min}\n");
        }
        None => {
            println!("nsrt: analyzing {} files under {}", files.len(), dir.display());
            println!(
                "nsrt: WARNING — could not determine the PHP version; `// lint` gates \
                 not applied, so counts include fixtures asserting another engine's answers\n"
            );
        }
    }

    let start = Instant::now();

    // One resident sidecar folder reused across every project (ADR-0004's fold
    // posture); analysis is single-threaded and finishes in seconds.
    let mut folder = SidecarFolder::enabled();

    let mut records: Vec<Record> = Vec::new();
    let mut version_skipped = 0usize;
    for f in &files {
        let name = f.strip_prefix(&dir).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => continue, // unreadable → contributes nothing
        };

        // A gated fixture's assertions are only PHPStan's answer *for that
        // version range*. Run against another engine they are not an oracle at
        // all, so agreement there would be luck, not confirmation (issue #356).
        if let Some(v) = php
            && lint_gate(&text).is_some_and(|g| !g.admits(v))
        {
            version_skipped += 1;
            continue;
        }

        // Each file is its own single-file project (a standalone universe).
        let db = SteinsDatabase::default();
        let input = SourceFile::new(&db, name.clone(), text);
        let project = Project::new(&db, vec![input], steins_db::ProjectLayout::fallback(), steins_db::PluginFacts::none());
        let observations = collect_assert_types(&db, project, &mut folder);
        for obs in observations {
            records.push(Record::classify(&name, obs));
        }
    }

    let elapsed = start.elapsed();

    report(&records, elapsed.as_secs_f64(), folder.posture(), version_skipped);
    write_json(&records)?;
    Ok(())
}

// ----------------------------------------------------------------------------
// version gating (issue #356)
// ----------------------------------------------------------------------------

/// A `(major, minor)` PHP version. Patch is never gated on.
type PhpVersion = (u32, u32);

/// A fixture's `// lint <op> <version>` gate: the PHP range under which
/// PHPStan's assertions in that file are claimed to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LintGate {
    op: GateOp,
    version: PhpVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateOp {
    Lt,
    Le,
    Gt,
    Ge,
}

impl LintGate {
    /// Whether the running engine falls inside the gate.
    fn admits(self, running: PhpVersion) -> bool {
        match self.op {
            GateOp::Lt => running < self.version,
            GateOp::Le => running <= self.version,
            GateOp::Gt => running > self.version,
            GateOp::Ge => running >= self.version,
        }
    }
}

/// The gate on a fixture's first line, if any.
///
/// phpstan-src writes it *on the open tag* (`<?php // lint >= 8.1`), not as a
/// standalone comment line — 448 of the 1,617 nsrt fixtures carry one. Only the
/// first line is consulted, matching phpstan-src's own reader.
fn lint_gate(text: &str) -> Option<LintGate> {
    let first = text.lines().next()?;
    let rest = first.split("// lint").nth(1).or_else(|| first.split("//lint").nth(1))?;
    let rest = rest.trim_start();
    // Longest operator first: `<=` must not read as `<`.
    let (op, rest) = ["<=", ">=", "<", ">"]
        .into_iter()
        .find_map(|o| rest.strip_prefix(o).map(|r| (o, r)))?;
    let op = match op {
        "<=" => GateOp::Le,
        ">=" => GateOp::Ge,
        "<" => GateOp::Lt,
        _ => GateOp::Gt,
    };
    let token: String =
        rest.trim_start().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let (maj, min) = token.split_once('.')?;
    Some(LintGate { op, version: (maj.parse().ok()?, min.parse().ok()?) })
}

/// The PHP the sidecar will run, asked of the interpreter itself.
///
/// `steins-sidecar` spawns a bare `Command::new("php")`, so resolving the same
/// way off `PATH` is what keeps this honest: a gate is only meaningful against
/// the engine that actually answers the folds.
fn running_php_version() -> Option<PhpVersion> {
    let out = std::process::Command::new("php")
        .args(["-r", "echo PHP_MAJOR_VERSION, '.', PHP_MINOR_VERSION;"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let (maj, min) = s.trim().split_once('.')?;
    Some((maj.parse().ok()?, min.parse().ok()?))
}

// ----------------------------------------------------------------------------
// coverage posture (issue #245)
// ----------------------------------------------------------------------------

/// The run's fold-surface posture, printed under the headline (issue #245):
/// absolute counts are only comparable between runs that folded the same way.
/// Trigger: phpstan-src's `data/bug-6866.php`
/// (`str_repeat('abcdefghij', 1000000000)`) exhausts `memory_limit`, a PHP
/// fatal no `catch` sees — the child dies, gets replaced, and the run finishes
/// one answer poorer with only a stderr notice.
///
/// Three postures: **backed throughout** — every request reached a live
/// engine, the only shape comparable with a sidecar-backed baseline;
/// **degraded, recovered** — the child died and was replaced, lost replies
/// are never retried (ADR-0024), counts are a FLOOR; **abandoned** — the
/// respawn budget is spent, counts are the sound subset from that point.
///
/// Never engaging an engine is the plain sound subset (already on stderr via
/// [`steins_infer::SOUND_SUBSET_NOTICE`]); named here too so a reader doesn't
/// have to go find that stderr.
fn posture_line(p: steins_infer::FoldPosture) -> String {
    if !p.engaged {
        return "  fold surface: SOUND SUBSET — no PHP sidecar was engaged; counts are not \
                comparable with a sidecar-backed baseline"
            .to_owned();
    }
    if p.sidecar_backed_throughout() {
        return "  fold surface: sidecar-backed throughout".to_owned();
    }
    let answers = if p.losses == 1 { "answer was" } else { "answers were" };
    if p.abandoned {
        format!(
            "  fold surface: ABANDONED — the PHP sidecar died {} time(s), was replaced {} time(s), \
             and its respawn budget is spent; the run finished as the sound subset, so these \
             counts are NOT comparable with a sidecar-backed baseline",
            p.losses, p.restarts
        )
    } else {
        format!(
            "  fold surface: DEGRADED — the PHP sidecar died {} time(s) and was restarted {} \
             time(s); {} {} lost and never retried (ADR-0024), so these counts are a FLOOR and \
             NOT comparable with a sidecar-backed baseline",
            p.losses, p.restarts, p.losses, answers
        )
    }
}

/// The default nsrt directory: a sibling phpstan-src checkout, relative to the repo.
fn default_nsrt_dir() -> PathBuf {
    // repo_root = …/repo/rust/steins ; php sibling = …/repo/php/phpstan-src.
    repo_root()
        .join("../../php/phpstan-src/tests/PHPStan/Analyser/nsrt")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("../../php/phpstan-src/tests/PHPStan/Analyser/nsrt"))
}

// ----------------------------------------------------------------------------
// classification
// ----------------------------------------------------------------------------

/// The five-verdict taxonomy. Observations whose expected slot could not be
/// resolved to a plain string (`::class`/concat) never reach [`classify`] — they are
/// recorded as the `"skipped"` housekeeping bucket directly in [`Record::classify`]
/// and kept out of the measurement denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Match,
    Unsupported,
    /// Proven-equal-but-differently-spelled (issue #172): [`is_subsumption`]'s
    /// relation answers `Yes` both ways while the normalized strings differ.
    /// Exists so the D4-native spelling class (ADR-0062 §6) is listable, not
    /// buried in `differ`.
    Equal,
    /// A proper subtype of the assertion (issue #47; see module docs). Names a
    /// type relation, not a quality — narrower isn't always better (a fold bug
    /// under a correct base type lands here too) — why it excludes the headline.
    Subsumed,
    Differ,
}

/// One classified assertType observation.
#[derive(Debug, Clone, serde::Serialize)]
struct Record {
    file: String,
    line: u32,
    verdict: &'static str,
    /// The raw PHPStan expected string (or `<unresolved>` when skipped).
    expected: String,
    /// Steins' rendering.
    got: String,
    asserted: bool,
    /// For `unsupported`: the named vocabulary pattern. For
    /// `differ`/`subsumed`/`equal`: the coarse gap-class key (for `equal` it names
    /// the spelling pair). Empty for `match`/`skipped`.
    class: String,
}

impl Record {
    fn classify(file: &str, obs: AssertObservation) -> Record {
        let AssertObservation { line, expected, got, asserted, .. } = obs;
        let Some(expected) = expected else {
            return Record {
                file: file.to_owned(),
                line,
                verdict: "skipped",
                expected: "<unresolved>".to_owned(),
                got,
                asserted,
                class: String::new(),
            };
        };

        let (verdict, class) = classify(&expected, &got);
        Record {
            file: file.to_owned(),
            line,
            verdict: verdict_name(verdict),
            expected,
            got,
            asserted,
            class,
        }
    }
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Match => "match",
        Verdict::Unsupported => "unsupported",
        Verdict::Equal => "equal",
        Verdict::Subsumed => "subsumed",
        Verdict::Differ => "differ",
    }
}

/// Classify one (expected, got) pair: unsupported vocabulary first, then
/// normalized equivalence, then the acceptance relation both ways — mutual
/// `Yes` is `equal` (#172), the strict covering direction alone is `subsumed`
/// (#47), checked in that order so `equal` claims mutual subsumption first.
fn classify(expected: &str, got: &str) -> (Verdict, String) {
    if let Some(pattern) = unsupported_pattern(expected) {
        return (Verdict::Unsupported, pattern.to_owned());
    }
    if normalize(expected) == normalize(got) {
        return (Verdict::Match, String::new());
    }
    if is_proven_equal(expected, got) {
        return (Verdict::Equal, gap_class(expected, got));
    }
    if is_subsumption(expected, got) {
        return (Verdict::Subsumed, gap_class(expected, got));
    }
    (Verdict::Differ, gap_class(expected, got))
}

// ----------------------------------------------------------------------------
// subsumption (issue #47) — the checker's own acceptance relation
// ----------------------------------------------------------------------------

/// Steins' own sentinel renderings — not type strings. `unknown` is the reach
/// gap this harness exists to inventory; lowering it would parse as a class
/// named `unknown`, which the guard exists to stop from becoming "more precise".
const STEINS_SENTINELS: &[&str] = &["unknown", "no declared contract"];

/// Both directions of the acceptance question for one pair, asked once. Named so
/// the two verdicts built on it ([`Verdict::Equal`], [`Verdict::Subsumed`]) read
/// off the same evidence instead of re-deriving it.
#[derive(Debug, Clone, Copy)]
struct SubsumptionDirections {
    /// `expected ⊇ got` answered `Certainty::Yes`.
    covers: bool,
    /// `got ⊇ expected` answered `Certainty::Yes`.
    covered: bool,
}

/// Ask the checker's own acceptance relation both ways for one pair (issues
/// #47, #172): both strings lower via `steins_contract::lower_str`, judged by
/// `normalize::subsumes` — the same relation the contract layer uses for
/// param contravariance/return covariance and ADR-0056's envelope check. Only
/// `Certainty::Yes` counts (undecided `Maybe` yields `false`, the FP-safe
/// `differ` side); three guards veto both directions: a sentinel, a
/// coercion-crossing pair ([`crosses_int_float`]), or a top-type expectation
/// ([`expected_is_top_type`]).
fn subsumption_directions(expected: &str, got: &str) -> SubsumptionDirections {
    const NEITHER: SubsumptionDirections = SubsumptionDirections { covers: false, covered: false };
    if STEINS_SENTINELS.contains(&got.trim()) {
        return NEITHER;
    }
    if crosses_int_float(expected, got) {
        return NEITHER;
    }
    if expected_is_top_type(expected) {
        return NEITHER;
    }
    let (Some(exp_ty), Some(got_ty)) =
        (steins_contract::lower_str(expected), steins_contract::lower_str(got))
    else {
        return NEITHER; // one side does not parse as a type — not a comparison at all
    };
    // The same coercion veto, now at every *nested* position (issue #356): the
    // string scan above only splits top-level `|`, so `array{2.0, …}` vs
    // `list{2, …}` reads as two array atoms and slips past it.
    if crosses_int_float_nested(&exp_ty, &got_ty) {
        return NEITHER;
    }
    use steins_contract::normalize::subsumes;
    SubsumptionDirections {
        covers: subsumes(&exp_ty, &got_ty).is_yes(),
        covered: subsumes(&got_ty, &exp_ty).is_yes(),
    }
}

/// Whether the relation proves the pair equal in both directions (issue #172):
/// `expected ⊇ got` and `got ⊇ expected`, each `Certainty::Yes`. This — and only
/// this — awards [`Verdict::Equal`]; no string comparison is consulted.
fn is_proven_equal(expected: &str, got: &str) -> bool {
    let dirs = subsumption_directions(expected, got);
    dirs.covers && dirs.covered
}

/// Whether `got` is strictly narrower than `expected` — Steins answering a
/// question the oracle left open (issue #47). Strict: covering direction `Yes`,
/// reverse not; mutual `Yes` is [`Verdict::Equal`] instead (pinned by
/// `mutual_subsumption_is_not_strict`).
fn is_subsumption(expected: &str, got: &str) -> bool {
    let dirs = subsumption_directions(expected, got);
    dirs.covers && !dirs.covered
}

/// Whether the oracle asserted the top type (issue #239): `mixed` denotes every
/// value, so `subsumes(mixed, got)` answers `Yes` unconditionally — a `subsumed`
/// verdict there would just re-report the oracle's silence, not a type relation
/// (measurement in the module docs, §`mixed`). A veto on *asking*, same shape as
/// [`crosses_int_float`]; `match` still claims a genuine `mixed`/`mixed` pair
/// first, so the veto costs no agreement.
fn expected_is_top_type(expected: &str) -> bool {
    normalize(expected) == "mixed"
}

/// Whether the pair straddles the int/float boundary in the widening
/// direction. `admits_val(float, Int) = Yes` is PHP's coercion rule for a
/// value entering a declared `float` slot, not a claim an int *is* a float —
/// PHPStan's hierarchy says `No`. So oracle `float` vs Steins `int` is a
/// contradiction, not an open question (`bug-12393.php:40`, a missing
/// typed-property coercion) — booking it as precision would launder a bug.
/// [`subsumes`] stays the only relation consulted; this only declines to ask
/// it across the coercion boundary.
///
/// [`subsumes`]: steins_contract::normalize::subsumes
fn crosses_int_float(expected: &str, got: &str) -> bool {
    fn int_flavored(s: &str) -> bool {
        normalize(s)
            .split('|')
            .any(|a| matches!(atom_kind(a), "int" | "int-literal" | "int-range"))
    }
    // Only the widening direction is blocked: an int-flavored `got` needs an
    // int-flavored arm in `expected` to be a *member*, not merely coercible.
    int_flavored(got) && !int_flavored(expected)
}

/// [`crosses_int_float`]'s judgment carried to every **nested** position
/// (issue #356).
///
/// The string scan splits only top-level `|`, so a crossing buried in an array
/// element reads as one opaque `array-shape` atom and escapes the veto. The
/// live case: `range(2, 5, 1.0)` is asserted `array{2.0, 3.0, 4.0, 5.0}` behind
/// a `// lint < 8.3` gate, PHP ≥ 8.3 returns ints, and the fold answers
/// `list{2, 3, 4, 5}`. `admits_val(LitFloat(2.0), Val::Int(2))` is `Yes` by
/// design (PHP value equality, `admit.rs`), so `subsumes` covers it and the
/// pair booked `subsumed` — a real disagreement laundered into *admissible*.
///
/// Judged on the lowered types with **aligned** positions, so a genuine int arm
/// elsewhere in the shape cannot excuse a crossing at the position that has
/// one. Undecidable alignments simply yield no pair: this only ever declines to
/// ask [`subsumes`], never manufactures a verdict.
///
/// [`subsumes`]: steins_contract::normalize::subsumes
fn crosses_int_float_nested(expected: &ContractTy, got: &ContractTy) -> bool {
    if float_only(expected) && int_flavored_ty(got) {
        return true;
    }
    aligned_value_positions(expected, got)
        .iter()
        .any(|(e, g)| crosses_int_float_nested(e, g))
}

/// Whether the contract is float-flavored with no int arm — the only shape
/// `admits_val` widens an int into. A union carrying a real int arm admits the
/// int as a *member*, so it is not a coercion and earns no veto.
fn float_only(t: &ContractTy) -> bool {
    match t {
        ContractTy::Base(Base::Float) | ContractTy::LitFloat(_) => true,
        ContractTy::Union(m) => m.iter().any(float_only) && !m.iter().any(int_flavored_ty),
        _ => false,
    }
}

/// Whether the contract has an int arm — the `got` side of the widening.
fn int_flavored_ty(t: &ContractTy) -> bool {
    match t {
        ContractTy::Base(Base::Int) | ContractTy::LitInt(_) | ContractTy::IntIn(_) => true,
        ContractTy::Union(m) => m.iter().any(int_flavored_ty),
        _ => false,
    }
}

/// The value-contract pairs `expected` and `got` align on, one level down.
///
/// Keyed shapes pair by key; the generic forms contribute their single element
/// contract. A position `expected` cannot answer for (an unkeyed field of a
/// sealed shape, a vocabulary outside the array forms) contributes no pair —
/// silence, not a guess.
fn aligned_value_positions<'a>(
    expected: &'a ContractTy,
    got: &'a ContractTy,
) -> Vec<(&'a ContractTy, &'a ContractTy)> {
    let mut out = Vec::new();
    match got {
        ContractTy::Shape { fields, .. } => {
            for f in fields {
                if let Some(e) = expected_value_for(expected, Some(&f.key)) {
                    out.push((e, &f.ty));
                }
            }
        }
        ContractTy::ListOf { elem, .. } => {
            if let Some(e) = expected_value_for(expected, None) {
                out.push((e, elem.as_ref()));
            }
        }
        ContractTy::MapOf { val, .. } | ContractTy::IterableOf { val, .. } => {
            if let Some(e) = expected_value_for(expected, None) {
                out.push((e, val.as_ref()));
            }
        }
        // A `got` union crosses if any realization does (the FP-safe side: the
        // veto costs a `subsumed`, never a `match`).
        ContractTy::Union(members) => {
            for m in members {
                out.extend(aligned_value_positions(expected, m));
            }
        }
        _ => {}
    }
    out
}

/// `expected`'s value contract at `key` (or its element contract, for `None`).
fn expected_value_for<'a>(
    expected: &'a ContractTy,
    key: Option<&CKey>,
) -> Option<&'a ContractTy> {
    match expected {
        ContractTy::Shape { fields, unsealed, .. } => key
            .and_then(|k| fields.iter().find(|f| &f.key == k))
            .map(|f| &f.ty)
            .or_else(|| unsealed.as_ref().map(|(_, v)| v.as_ref())),
        ContractTy::ListOf { elem, .. } => Some(elem.as_ref()),
        ContractTy::MapOf { val, .. } | ContractTy::IterableOf { val, .. } => Some(val.as_ref()),
        _ => None,
    }
}

// ----------------------------------------------------------------------------
// unsupported-vocabulary detection (named patterns)
// ----------------------------------------------------------------------------

/// If `expected` uses vocabulary Steins deliberately does not model, return the
/// named pattern; else `None` (it is a supported comparison). An expected string is
/// unsupported iff ANY of its top-level union atoms is unsupported; the returned
/// name is the category of the first such atom (priority order below).
fn unsupported_pattern(expected: &str) -> Option<&'static str> {
    let s = strip_outer_parens(expected.trim());
    let s = s.strip_prefix('?').map(str::trim).unwrap_or(s); // `?X` is supported; rest is atoms
    for atom in split_union(s) {
        if let Some(cat) = atom_unsupported_category(atom.trim()) {
            return Some(cat);
        }
    }
    None
}

/// The unsupported category of a single atom, or `None` if the atom is one Steins
/// can model (a scalar/refined/int-range keyword, a literal, or a plain class name).
fn atom_unsupported_category(atom: &str) -> Option<&'static str> {
    let a = strip_outer_parens(atom).trim();
    let a = a.strip_prefix('?').map(str::trim).unwrap_or(a);

    // Supported shapes first — a positive test keeps the negative list honest.
    if is_supported_atom(a) {
        return None;
    }

    // PHPStan sentinels and set-algebra vocabulary.
    if a.contains('*') {
        return Some("phpstan-special"); // *ERROR*, *NEVER*
    }
    if a.contains('~') {
        return Some("subtraction"); // e.g. mixed~null
    }
    if a.contains('&') {
        // A string-refinement conjunction is claimed by `is_supported_atom` first
        // (issue #240); what reaches here is the object half — `int&object`,
        // `T&hasMethod(...)`, a `literal-string` arm — no `StrPreds` set denotes it.
        return Some("intersection");
    }
    // Well-formed array/list shapes and generics are claimed by `is_supported_atom`
    // (S1.5); anything else with `{`/`<` is unnamed shape vocabulary or a true
    // non-array generic.
    if a.contains('{') {
        return Some("shape-other");
    }
    if a.contains('<') {
        if a.contains("class-string") {
            return Some("class-string");
        }
        return Some("generic-other");
    }
    if a.starts_with("callable") || a.contains("Closure") || a.contains("\\Closure") {
        return Some("callable");
    }
    if a.contains("key-of") || a.contains("value-of") {
        return Some("key-of-value-of");
    }
    if a.contains("class-string") {
        return Some("class-string");
    }

    // Bare keyword atoms Steins does not render.
    let low = a.to_ascii_lowercase();
    match low.as_str() {
        "object" => Some("object"),
        "void" | "never" | "resource" | "scalar" | "empty" | "iterable" => Some("other-keyword"),
        "static" | "self" | "parent" | "$this" => Some("self-static"),
        "callable" => Some("callable"),
        // Bare `class-string` is claimed by `is_supported_atom` first (issue
        // #236); only `class-string-`prefixed leftovers reach here.
        "" => Some("empty-atom"),
        _ => {
            // A leftover token that is not a plain class name.
            if a.chars().any(|c| c.is_whitespace()) {
                Some("compound")
            } else {
                Some("other")
            }
        }
    }
}

/// Whether a single atom is one Steins can render (fair to *compare*, not
/// classify unsupported). Scalar/refined/int-range keywords, literals, plain
/// class names, and — as of S1.5 (ADR-0062) — the array vocabulary the speller
/// spells (`array`/`list`, `non-empty-` forms, bare or shaped/generic) all
/// qualify, as does a conjunction of string-refinement keywords (issue #240;
/// see [`is_str_preds_conjunction`]).
fn is_supported_atom(a: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "int",
        "float",
        "string",
        "bool",
        "true",
        "false",
        "null",
        "non-empty-string",
        "non-falsy-string",
        "numeric-string",
        // Casing pair (issue #77): `preds_keyword` spells both; their
        // `non-empty-` intersection form stays unsupported (it's an intersection).
        "lowercase-string",
        "uppercase-string",
        // Bare class-string only (issue #236); `class-string<T>` stays gated
        // (the `<` branch of `atom_unsupported_category`) pending the generics
        // carry (issue #10).
        "class-string",
        "positive-int",
        "negative-int",
        "non-negative-int",
        // Bare array/list keywords (S1.5): the speller spells the full vocabulary.
        "array",
        "non-empty-array",
        "list",
        "non-empty-list",
        // Top type (issue #239): `ContractTy::Mixed` spells `mixed`. The cut forms
        // are PHPStan's subtraction spelling (`mixed~null` etc.) and stay gated.
        "mixed",
    ];
    let low = a.to_ascii_lowercase();
    if KEYWORDS.contains(&low.as_str()) {
        return true;
    }
    if is_str_preds_conjunction(a) {
        return true;
    }
    if is_array_accessory_conjunction(a) || is_class_intersection(a) {
        return true;
    }
    if is_int_range(&low) {
        return true;
    }
    if is_int_literal(a) || is_float_literal(a) {
        return true;
    }
    if a.starts_with('\'') && a.ends_with('\'') && a.len() >= 2 {
        return true; // a string literal
    }
    if is_array_shape_atom(a) || is_array_generic_atom(a) {
        return true;
    }
    // Reserved lowercase keywords look class-like but name vocabulary Steins does
    // not render as a class — they must NOT pass as a plain class name.
    if RESERVED_UNSUPPORTED_KEYWORDS.contains(&low.as_str()) {
        return false;
    }
    // A plain class name: `Foo`, `\Foo\Bar`, `Foo\Bar` — letters/digits/underscore
    // and namespace separators only, starting class-like.
    is_plain_class_name(a)
}

/// `A&B` where every arm is a string-refinement keyword the value domain holds as
/// a bit — one closed `StrPreds` set the relation can judge (issue #240). Gating
/// on `&` measured the harness's vocabulary, not the analyzer's (same defect as
/// S1.5 array atoms, #77 casing, #236 `class-string`; the #235 probe found
/// 263/273 accessory rows misfiled this way). The arm test is the lowering
/// itself (`lower_str` → `ContractTy::Inter`, judged arm-wise), not a hand list.
/// Excluded by construction: `literal-string` (→ `StrOpaque`, provenance,
/// ADR-0038) and `class-string` (contextual `CLASS_STRING` bit, issue #236,
/// which `is_extensional` refuses).
fn is_str_preds_conjunction(a: &str) -> bool {
    a.contains('&')
        && a.split('&').all(|arm| {
            matches!(
                steins_contract::lower_identifier(arm.trim()),
                steins_contract::ContractTy::StrWith(p) if p.is_extensional()
            )
        })
}

/// `<array>&hasOffset(K)` / `<array>&hasOffsetValue(K, V)` — PHPStan's accessory-
/// predicate spelling for facts ADR-0062's array vocabulary already carries
/// (issue #238). Same #240-style defect: e.g. `array-flip.php:74` renders
/// `non-empty-array{foo: int, ...<string, int>}` for PHPStan's
/// `non-empty-array<string, int>&hasOffset('foo')`, and the relation proves them
/// equal — a lowering was missing, not a domain. The test is the fold itself (an
/// atom qualifies iff `lower_str` yields the array vocabulary), so a predicate
/// on a class base still lowers to `Inter` and stays `unsupported`.
fn is_array_accessory_conjunction(a: &str) -> bool {
    a.contains('&')
        && a.contains("hasOffset")
        && matches!(
            steins_contract::lower_str(a),
            Some(
                steins_contract::ContractTy::Shape { .. }
                    | steins_contract::ContractTy::ArrayAny { .. }
                    | steins_contract::ContractTy::ListOf { .. }
                    | steins_contract::ContractTy::MapOf { .. }
            )
        )
}

/// `A&B` where every arm is a plain class/interface name (issue #238) —
/// `ArrayAccess&stdClass`. Same #240-style re-filing: `lower_str` → `Inter` of
/// `Class` arms, `spell_nested` round-trips, `subsumes` judges arm-wise.
/// Measured before the change: 35 rows, 34 rendering `unknown` — reach rows in
/// a vocabulary costume, moved to `differ`. The arm test is the lowering: a
/// keyword, template, callable, or accessory-predicate arm doesn't lower to
/// `Class` and stays `unsupported`.
fn is_class_intersection(a: &str) -> bool {
    a.contains('&')
        && a.split('&').all(|arm| {
            let arm = arm.trim();
            is_plain_class_name(arm)
                && !RESERVED_UNSUPPORTED_KEYWORDS.contains(&arm.to_ascii_lowercase().as_str())
                && matches!(
                    steins_contract::lower_identifier(arm),
                    steins_contract::ContractTy::Class(_)
                )
        })
}

/// `array{…}` / `list{…}` / `non-empty-array{…}` / `non-empty-list{…}` — the full
/// shape vocabulary the speller renders (S1, ADR-0062). Structural only: a
/// recognized prefix plus a matching closing brace; `split_union` upstream hands
/// out brace-balanced atoms, so this never mis-detects a malformed shape. What's
/// unrepresentable inside a shape (a conditional type, a template field) lowers
/// to `Opaque` rather than erroring, so the mismatch just lands in `differ`.
fn is_array_shape_atom(a: &str) -> bool {
    const PREFIXES: &[&str] = &["array{", "list{", "non-empty-array{", "non-empty-list{"];
    let low = a.to_ascii_lowercase();
    PREFIXES.iter().any(|p| low.starts_with(p)) && a.ends_with('}')
}

/// `array<...>` / `list<...>` / `non-empty-array<...>` / `non-empty-list<...>` —
/// the generic-array/list vocabulary the speller now renders (S1). Same
/// structural-only caveat as [`is_array_shape_atom`].
fn is_array_generic_atom(a: &str) -> bool {
    const PREFIXES: &[&str] = &["array<", "list<", "non-empty-array<", "non-empty-list<"];
    let low = a.to_ascii_lowercase();
    PREFIXES.iter().any(|p| low.starts_with(p)) && a.ends_with('>')
}

/// Bare lowercase keywords that look class-like but denote vocabulary Steins does
/// not model — never a plain class name. `array`/`list` (+ `non-empty-` forms) and
/// `mixed` are NOT here as of S1.5/#239: they're recognized earlier in
/// [`is_supported_atom`]'s `KEYWORDS` list.
const RESERVED_UNSUPPORTED_KEYWORDS: &[&str] = &[
    "object", "void", "never", "resource", "scalar", "empty", "iterable",
    "callable", "static", "self", "parent",
];

/// `int<lo, hi>` where lo/hi are `min`/`max`/signed integers (whitespace-tolerant).
fn is_int_range(low: &str) -> bool {
    let Some(inner) = low.strip_prefix("int<").and_then(|s| s.strip_suffix('>')) else {
        return false;
    };
    let mut parts = inner.split(',');
    let (Some(lo), Some(hi), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    is_range_bound(lo.trim()) && is_range_bound(hi.trim())
}

fn is_range_bound(b: &str) -> bool {
    b == "min" || b == "max" || is_int_literal(b)
}

fn is_int_literal(a: &str) -> bool {
    let t = a.strip_prefix('-').unwrap_or(a);
    !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit())
}

fn is_float_literal(a: &str) -> bool {
    let t = a.strip_prefix('-').unwrap_or(a);
    let mut dot = false;
    let mut digit = false;
    for c in t.chars() {
        match c {
            '0'..='9' => digit = true,
            '.' if !dot => dot = true,
            _ => return false,
        }
    }
    dot && digit
}

fn is_plain_class_name(a: &str) -> bool {
    let t = a.strip_prefix('\\').unwrap_or(a);
    if t.is_empty() {
        return false;
    }
    let first = t.as_bytes()[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'\\')
}

// ----------------------------------------------------------------------------
// normalization (certain equivalences only)
// ----------------------------------------------------------------------------

/// Canonicalize a supported type string for equivalence comparison: strip enclosing
/// parens, expand a leading `?` nullable, normalize int-range spelling and each
/// atom, then sort the union (so `|` order and duplicate atoms do not matter).
fn normalize(s: &str) -> String {
    let s = strip_outer_parens(s.trim());
    // `?X` ⇒ `X|null`. The union split below then carries the `null` atom.
    let expanded: String = if let Some(rest) = s.strip_prefix('?') {
        format!("{}|null", rest.trim())
    } else {
        s.to_owned()
    };
    let mut atoms: Vec<String> =
        split_union(&expanded).into_iter().map(|a| normalize_atom(a.trim())).collect();
    atoms.sort();
    atoms.dedup();
    atoms.join("|")
}

/// Canonicalize a single atom. String literals are kept verbatim (case-sensitive);
/// int-range spellings collapse to the named keyword form; everything else lowercases
/// (PHP scalar keywords and class names are case-insensitive).
fn normalize_atom(a: &str) -> String {
    let a = strip_outer_parens(a).trim();
    // A string literal: keep exactly (case & quoting are semantic).
    if a.starts_with('\'') {
        return a.to_owned();
    }
    let a = a.strip_prefix('\\').unwrap_or(a); // drop a leading namespace slash
    let low = a.to_ascii_lowercase();
    // Collapse the three int-range spellings onto one canonical keyword, and vice
    // versa, so `positive-int` == `int<1, max>` etc.
    match canonical_int_range(&low) {
        Some(canon) => canon,
        None => low,
    }
}

/// Map an int-range atom (either the named keyword or the `int<lo, hi>` interval)
/// to one canonical spelling, so the two forms compare equal. Returns `None` for a
/// non-int-range atom.
fn canonical_int_range(low: &str) -> Option<String> {
    match low {
        "positive-int" => return Some("int<1,max>".to_owned()),
        "non-negative-int" => return Some("int<0,max>".to_owned()),
        "negative-int" => return Some("int<min,-1>".to_owned()),
        _ => {}
    }
    let inner = low.strip_prefix("int<")?.strip_suffix('>')?;
    let mut parts = inner.split(',');
    let lo = parts.next()?.trim();
    let hi = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }
    if !is_range_bound(lo) || !is_range_bound(hi) {
        return None;
    }
    Some(format!("int<{lo},{hi}>"))
}

/// Strip fully-enclosing `(...)` pairs (repeatedly). A pair only encloses when its
/// opening paren matches the final closing paren at depth zero.
fn strip_outer_parens(s: &str) -> &str {
    let mut s = s.trim();
    loop {
        let bytes = s.as_bytes();
        if bytes.first() != Some(&b'(') || bytes.last() != Some(&b')') {
            return s;
        }
        // Verify the opening paren closes exactly at the end (not two siblings).
        let mut depth = 0i32;
        let mut encloses = true;
        for (i, ch) in s.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && i != s.len() - 1 {
                        encloses = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !encloses {
            return s;
        }
        s = s[1..s.len() - 1].trim();
    }
}

/// Split a type string on top-level `|`, respecting `'...'` string literals and the
/// nesting depth of `<>`, `{}`, and `()`. (Supported comparison strings carry no
/// brackets, but the splitter stays correct for the unsupported-detector's pass.)
fn split_union(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '\'' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_str = true,
            '<' | '{' | '(' => depth += 1,
            '>' | '}' | ')' => depth -= 1,
            '|' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

// ----------------------------------------------------------------------------
// gap-class heuristic (differ grouping)
// ----------------------------------------------------------------------------

/// A coarse gap-class key for a `differ`, keyed by the expected string's shape and
/// Steins' rendering kind — the ranking axis that drives the fix hunt.
fn gap_class(expected: &str, got: &str) -> String {
    format!("expected:{} | steins:{}", shape_of(expected), kind_of(got))
}

/// The coarse shape of the expected type: single-atom category, or a union label.
fn shape_of(s: &str) -> String {
    let norm = normalize(s);
    let atoms: Vec<&str> = norm.split('|').collect();
    if atoms.len() == 1 {
        return atom_kind(atoms[0]).to_owned();
    }
    let has_null = atoms.contains(&"null");
    let non_null: Vec<&str> = atoms.iter().copied().filter(|a| *a != "null").collect();
    if has_null && non_null.len() == 1 {
        return format!("nullable-{}", atom_kind(non_null[0]));
    }
    if non_null.iter().all(|a| is_scalarish(a)) {
        return if has_null { "scalar-union-null".to_owned() } else { "scalar-union".to_owned() };
    }
    "union".to_owned()
}

/// The coarse kind of Steins' rendering (its own vocabulary).
fn kind_of(got: &str) -> String {
    if got == "unknown" {
        return "unknown".to_owned();
    }
    if got == "no declared contract" {
        return "no-contract".to_owned();
    }
    let norm = normalize(got);
    let atoms: Vec<&str> = norm.split('|').collect();
    if atoms.len() == 1 {
        return atom_kind(atoms[0]).to_owned();
    }
    if atoms.iter().all(|a| is_scalarish(a)) {
        "scalar-union".to_owned()
    } else {
        "union".to_owned()
    }
}

/// The category of one normalized atom.
fn atom_kind(a: &str) -> &'static str {
    if a == "null" {
        return "null";
    }
    if a == "true" || a == "false" {
        return "bool-literal";
    }
    if a == "bool" {
        return "bool";
    }
    if a.starts_with('\'') {
        return "string-literal";
    }
    if is_int_literal(a) {
        return "int-literal";
    }
    if is_float_literal(a) {
        return "float-literal";
    }
    if a.starts_with("int<") {
        return "int-range";
    }
    // Array vocabulary (S1.5): shapes/generics get their own gap-class label
    // instead of the catch-all `other`, keeping the differ ranking legible.
    if is_array_shape_atom(a) {
        return "array-shape";
    }
    if is_array_generic_atom(a) {
        return "array-generic";
    }
    match a {
        "int" => "int",
        "float" => "float",
        "string" => "string",
        // Top type + cuts (issue #239): without this arm `mixed` lowercases into
        // a valid class name and the 329-row class would misrank as `class`.
        "mixed" | "non-null-mixed" | "non-empty-mixed" => "mixed",
        "non-empty-string" | "non-falsy-string" | "numeric-string" => "refined-string",
        "array" | "non-empty-array" => "array-bare",
        "list" | "non-empty-list" => "list-bare",
        _ => {
            if is_plain_class_name(a) {
                "class"
            } else {
                "other"
            }
        }
    }
}

fn is_scalarish(a: &str) -> bool {
    !matches!(
        atom_kind(a),
        "class"
            | "other"
            | "mixed"
            | "array-shape"
            | "array-generic"
            | "array-bare"
            | "list-bare"
    )
}

// ----------------------------------------------------------------------------
// reporting
// ----------------------------------------------------------------------------

fn report(
    records: &[Record],
    elapsed: f64,
    posture: steins_infer::FoldPosture,
    version_skipped: usize,
) {
    let total = records.len();
    let count = |v: &str| records.iter().filter(|r| r.verdict == v).count();
    let (m, u, d, s) = (count("match"), count("unsupported"), count("differ"), count("skipped"));
    let sub = count("subsumed");
    let eq = count("equal");
    // The measurement denominator excludes skipped (unresolvable expected slots).
    let measured = m + u + eq + sub + d;
    let pct = |n: usize| if measured == 0 { 0.0 } else { 100.0 * n as f64 / measured as f64 };

    println!("=== nsrt assertType harness — verdict summary ===\n");
    println!("total assertType observations: {total}");
    println!("  skipped (expected unresolvable ::class/concat): {s}");
    // Never folded into a verdict: these fixtures were not analyzed at all,
    // because their assertions are not this engine's oracle (issue #356).
    println!("  files skipped (`// lint` gate excludes the sidecar): {version_skipped}");
    println!("measured (match + unsupported + equal + subsumed + differ): {measured}\n");
    println!("  {:<13} {:>6}   {:>6}", "verdict", "count", "% meas");
    println!("  {}", "-".repeat(30));
    println!("  {:<13} {:>6}   {:>5.1}%", "match", m, pct(m));
    println!("  {:<13} {:>6}   {:>5.1}%", "unsupported", u, pct(u));
    println!("  {:<13} {:>6}   {:>5.1}%", "equal", eq, pct(eq));
    println!("  {:<13} {:>6}   {:>5.1}%", "subsumed", sub, pct(sub));
    println!("  {:<13} {:>6}   {:>5.1}%", "differ", d, pct(d));
    println!("  {}", "-".repeat(30));
    println!("  {:<13} {:>6}   ({:.2}s)", "TOTAL meas", measured, elapsed);
    // Headline stays `match` — oracle-confirmed agreement at the string level.
    // `equal`/`subsumed` are reported beside it, never inside it (issues #47/#172;
    // see module docs).
    println!("\n  HEADLINE (match, oracle-confirmed):   {m}");
    println!("  admissible (match + equal + subsumed): {}", m + eq + sub);
    // Coverage posture belongs with the headline: the two numbers above are only
    // comparable between runs whose fold surface held the same way (ADR-0004, #245).
    println!("{}\n", posture_line(posture));

    // Unsupported pattern breakdown.
    let mut unsup: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records.iter().filter(|r| r.verdict == "unsupported") {
        *unsup.entry(r.class.as_str()).or_insert(0) += 1;
    }
    let mut unsup_sorted: Vec<(&&str, &usize)> = unsup.iter().collect();
    unsup_sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("=== unsupported-vocabulary patterns ({u} total) ===\n");
    for (pat, n) in &unsup_sorted {
        println!("  {:<20} {:>6}", pat, n);
    }

    // Equal listing: the whole point of the bucket (issue #172) is that it's
    // countable and listable — print it whole.
    let equals: Vec<&Record> = records.iter().filter(|r| r.verdict == "equal").collect();
    println!("\n=== equal: proven equal, differently spelled ({eq} total) ===\n");
    for r in &equals {
        let mark = if r.asserted { " (asserted)" } else { "" };
        println!(
            "  {}:{}\n      phpstan: {}\n      steins:  {}{}",
            r.file, r.line, r.expected, r.got, mark
        );
    }

    // Subsumption listing: small enough to print whole, worth reading row by row.
    let subsumed: Vec<&Record> = records.iter().filter(|r| r.verdict == "subsumed").collect();
    println!("\n=== subsumed: Steins narrower than the assertion ({sub} total) ===\n");
    for r in &subsumed {
        let mark = if r.asserted { " (asserted)" } else { "" };
        println!(
            "  {}:{}\n      phpstan: {}\n      steins:  {}{}",
            r.file, r.line, r.expected, r.got, mark
        );
    }

    // Differ gap-class ranking.
    let mut gaps: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records.iter().filter(|r| r.verdict == "differ") {
        *gaps.entry(r.class.as_str()).or_insert(0) += 1;
    }
    let mut gaps_sorted: Vec<(&&str, &usize)> = gaps.iter().collect();
    gaps_sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\n=== differ gap-class ranking ({d} total) ===\n");
    for (gc, n) in &gaps_sorted {
        println!("  {:>6}  {}", n, gc);
    }

    // Top-30 differ listing (file:line, expected vs got).
    let differs: Vec<&Record> = records.iter().filter(|r| r.verdict == "differ").collect();
    println!("\n=== top-30 differs (expected vs got) ===\n");
    for r in differs.iter().take(30) {
        let mark = if r.asserted { " (asserted)" } else { "" };
        println!(
            "  {}:{}\n      expected: {}\n      got:      {}{}",
            r.file, r.line, r.expected, r.got, mark
        );
    }
    if differs.len() > 30 {
        println!("\n  … and {} more differs (see the JSON dump).", differs.len() - 30);
    }
}

/// Write the full machine-readable record set for the follow-up fix slices.
fn write_json(records: &[Record]) -> Result<(), String> {
    let scratch = scratch_dir();
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("cannot create scratch dir {}: {e}", scratch.display()))?;
    let path = scratch.join("nsrt-asserttype.json");
    let json = serde_json::to_string_pretty(records)
        .map_err(|e| format!("serializing records: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("writing {}: {e}", path.display()))?;
    println!("\nnsrt: wrote {} records to {}", records.len(), path.display());
    Ok(())
}

/// The session scratchpad directory (falls back to the repo `target/` if unset).
fn scratch_dir() -> PathBuf {
    std::env::var_os("CLAUDE_SCRATCHPAD")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target").join("nsrt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------------
    // coverage posture (issue #245)
    // ------------------------------------------------------------------------

    /// The one posture under which a headline may be compared with another run's.
    #[test]
    fn a_clean_run_says_it_was_sidecar_backed_throughout() {
        let p = steins_infer::FoldPosture { engaged: true, ..Default::default() };
        assert!(p.sidecar_backed_throughout());
        let line = posture_line(p);
        assert!(line.contains("sidecar-backed throughout"), "got: {line}");
        assert!(!line.contains("NOT comparable"), "got: {line}"); // no caveat on a clean run
    }

    /// The issue's own shape: the child died mid-run, was replaced, and the run
    /// finished on a live child. The count is a floor, and the line says so.
    #[test]
    fn a_recovered_death_is_named_and_the_count_is_called_a_floor() {
        let p =
            steins_infer::FoldPosture { engaged: true, losses: 1, restarts: 1, abandoned: false };
        assert!(!p.sidecar_backed_throughout());
        let line = posture_line(p);
        assert!(line.contains("DEGRADED"), "got: {line}");
        assert!(line.contains("restarted"), "the recovery is part of the story, got: {line}");
        assert!(line.contains("FLOOR"), "got: {line}");
        assert!(line.contains("NOT comparable"), "got: {line}");
    }

    /// Past the respawn cap the fold surface is gone, which is a different claim
    /// from "a few answers are missing" and must read differently.
    #[test]
    fn an_abandoned_transport_is_reported_as_the_sound_subset_from_that_point() {
        let p = steins_infer::FoldPosture { engaged: true, losses: 4, restarts: 3, abandoned: true };
        let line = posture_line(p);
        assert!(line.contains("ABANDONED"), "got: {line}");
        assert!(line.contains("sound subset"), "got: {line}");
        assert!(!line.contains("FLOOR"), "an abandoned run is not merely a floor, got: {line}");
    }

    /// Never engaging an engine is the plain sound subset — not a clean run.
    #[test]
    fn a_run_with_no_engine_is_not_reported_as_backed_throughout() {
        let p = steins_infer::FoldPosture::default();
        assert!(!p.sidecar_backed_throughout());
        let line = posture_line(p);
        assert!(line.contains("SOUND SUBSET"), "got: {line}");
        assert!(!line.contains("throughout"), "got: {line}");
    }

    #[test]
    fn nullable_forms_are_equivalent() {
        assert_eq!(normalize("int|null"), normalize("?int"));
        assert_eq!(normalize("?int"), normalize("null|int"));
    }

    #[test]
    fn union_order_and_dedup() {
        assert_eq!(normalize("string|int"), normalize("int|string"));
        assert_eq!(normalize("int|int|string"), normalize("string|int"));
    }

    #[test]
    fn int_range_spellings_collapse() {
        assert_eq!(normalize("positive-int"), normalize("int<1, max>"));
        assert_eq!(normalize("non-negative-int"), normalize("int<0, max>"));
        assert_eq!(normalize("negative-int"), normalize("int<min, -1>"));
        assert_ne!(normalize("positive-int"), normalize("int<2, max>")); // different interval
    }

    #[test]
    fn case_insensitivity_for_keywords_and_classes() {
        assert_eq!(normalize("INT"), normalize("int"));
        assert_eq!(normalize("\\Foo\\Bar"), normalize("Foo\\Bar"));
        assert_eq!(normalize("STDCLASS"), normalize("stdClass"));
    }

    #[test]
    fn string_literals_keep_case_and_order() {
        assert_ne!(normalize("'A'|'B'"), normalize("'a'|'b'")); // case is semantic
        assert_eq!(normalize("'a'|'b'"), normalize("'b'|'a'")); // but order doesn't matter
    }

    #[test]
    fn parenthesized_union_strips() {
        assert_eq!(normalize("(float|int)"), normalize("int|float"));
        assert_eq!(normalize("(DOMAttr|false)"), normalize("false|DOMAttr"));
    }

    #[test]
    fn classify_match_and_differ() {
        assert_eq!(classify("int", "int").0, Verdict::Match);
        assert_eq!(classify("positive-int", "int<1, max>").0, Verdict::Match);
        assert_eq!(classify("int", "unknown").0, Verdict::Differ);
        assert_eq!(classify("int", "string").0, Verdict::Differ);
    }

    // ---- subsumption (issue #47) --------------------------------------------

    #[test]
    fn strictly_narrower_is_subsumed_not_differ() {
        // Motivating row: binary.php:547, in_array('foo', ['foo','bar']) — PHPStan
        // asserts `bool` (declines to fold), Steins proves `true`, admissible under it.
        assert_eq!(classify("bool", "true").0, Verdict::Subsumed);
        assert_eq!(classify("int", "5").0, Verdict::Subsumed);
        assert_eq!(classify("string", "'foo'").0, Verdict::Subsumed);
        assert_eq!(classify("int|null", "int").0, Verdict::Subsumed);
        assert_eq!(classify("int", "int<1, max>").0, Verdict::Subsumed);
        assert_eq!(classify("string", "non-empty-string").0, Verdict::Subsumed);
    }

    /// The asymmetry is the point: Steins *wider* than the assertion is a real gap.
    /// If flipped, every widening regression would launder into "more precise".
    #[test]
    fn reverse_direction_is_never_subsumption() {
        assert_eq!(classify("true", "bool").0, Verdict::Differ);
        assert_eq!(classify("5", "int").0, Verdict::Differ);
        assert_eq!(classify("'foo'", "string").0, Verdict::Differ);
        assert_eq!(classify("int", "int|null").0, Verdict::Differ);
        assert_eq!(classify("int<1, max>", "int").0, Verdict::Differ);
        assert_eq!(classify("non-empty-string", "string").0, Verdict::Differ);
        assert!(!is_subsumption("true", "bool"));
    }

    #[test]
    fn unrelated_types_and_sentinels_stay_differ() {
        assert_eq!(classify("int", "string").0, Verdict::Differ);
        assert_eq!(classify("'a'", "'b'").0, Verdict::Differ);
        assert_eq!(classify("int", "unknown").0, Verdict::Differ); // reach, never precision
        assert_eq!(classify("stdClass", "unknown").0, Verdict::Differ);
        assert_eq!(classify("int", "no declared contract").0, Verdict::Differ);
        for s in STEINS_SENTINELS {
            assert!(!is_subsumption("bool", s), "{s} must not be a subsumption");
        }
    }

    /// An equal-but-differently-spelled pair is mutual subsumption, not a strict
    /// subtype — must not enter `subsumed` through the back door. Since issue #172,
    /// `equal` is claimed before `subsumed`, so this exclusion is load-bearing.
    #[test]
    fn mutual_subsumption_is_not_strict() {
        assert!(!is_subsumption("int", "int"));
        assert!(!is_subsumption("positive-int", "int<1, max>"));
    }

    /// Coercion boundary: `float ⊇ int` is `Yes` in the acceptance relation (PHP
    /// coerces at a declared `float` slot) but `No` in PHPStan's hierarchy.
    /// `bug-12393.php:40/56` stay `differ` — precision must never come from coercion.
    #[test]
    fn int_where_float_is_asserted_is_a_gap_not_precision() {
        assert_eq!(classify("float", "int").0, Verdict::Differ);
        assert_eq!(classify("1.0", "1").0, Verdict::Differ);
        assert_eq!(classify("float|null", "int").0, Verdict::Differ);
        // An int-flavored expected arm makes it a genuine membership question.
        assert_eq!(classify("float|int|string", "int").0, Verdict::Subsumed);
        assert_eq!(classify("int|float", "1").0, Verdict::Subsumed);
        assert_eq!(classify("float|int|string", "string").0, Verdict::Subsumed); // float too
    }

    /// Issue #356: the same boundary **inside** the array vocabulary. The string
    /// scan splits only top-level `|`, so these pairs read as two opaque
    /// `array-shape` atoms and used to reach `subsumes` — which answers `Yes`,
    /// because `admits_val(LitFloat(2.0), Val::Int(2))` is `Yes` by design. The
    /// live row is `range-function-php82.php:5`: `range(2, 5, 1.0)` asserted
    /// `array{2.0, 3.0, 4.0, 5.0}` under `// lint < 8.3`, folded to
    /// `list{2, 3, 4, 5}` on a ≥ 8.3 engine. That is a disagreement about which
    /// PHP is running, and booking it `subsumed` would launder it into
    /// *admissible*.
    #[test]
    fn nested_int_where_float_is_asserted_is_not_precision() {
        assert_eq!(
            classify("array{2.0, 3.0, 4.0, 5.0}", "list{2, 3, 4, 5}").0,
            Verdict::Differ
        );
        assert_eq!(classify("array<float>", "list{1, 2}").0, Verdict::Differ);
        assert_eq!(classify("list<float>", "list<int>").0, Verdict::Differ);
        assert_eq!(classify("array{1.0}", "array{1}").0, Verdict::Differ);
        // Depth is not the trigger — the crossing is.
        assert_eq!(classify("list<list<float>>", "list<list{1}>").0, Verdict::Differ);
    }

    /// The veto is *aligned*, so it neither over- nor under-fires: an int arm at
    /// another position cannot excuse a crossing, and a non-float expectation is
    /// left alone to be judged on the merits.
    #[test]
    fn the_nested_veto_is_positional() {
        // Position 0 crosses even though position 1 is honestly int-flavored.
        assert_eq!(classify("array{float, int}", "array{1, 2}").0, Verdict::Differ);
        // No crossing anywhere: a genuine precision win survives.
        assert_eq!(classify("array{float, int}", "array{1.0, 2}").0, Verdict::Subsumed);
        assert_eq!(classify("list<mixed>", "list{1}").0, Verdict::Subsumed);
        assert_eq!(classify("array{int}", "array{1}").0, Verdict::Subsumed);
        // An int-flavored arm at the crossing position is membership, not coercion.
        assert_eq!(classify("array{int|float}", "array{1}").0, Verdict::Subsumed);
    }

    // ---- version gating (issue #356) ----------------------------------------

    /// phpstan-src writes the gate on the open tag, not a standalone comment.
    #[test]
    fn lint_gates_parse_off_the_open_tag() {
        let g = |s: &str| lint_gate(s).expect("a gate");
        assert_eq!(g("<?php // lint < 8.3").version, (8, 3));
        assert_eq!(g("<?php // lint < 8.3").op, GateOp::Lt);
        assert_eq!(g("<?php // lint >= 8.1\n\nfoo();").op, GateOp::Ge);
        assert_eq!(g("<?php  // lint < 8.0").op, GateOp::Lt); // array-search-php7.php
        assert_eq!(g("<?php // lint > 7.4").op, GateOp::Gt); // bug-2600-php-version-scope.php
        // `<=` must not read as `<` — the ordering of the operator table.
        assert_eq!(g("<?php // lint <= 8.0"), LintGate { op: GateOp::Le, version: (8, 0) });
        // Only the first line carries a gate.
        assert!(lint_gate("<?php\n// lint < 8.3").is_none());
        assert!(lint_gate("<?php declare(strict_types=1);").is_none());
    }

    /// A gate excludes a run when the sidecar's minor falls outside it. The
    /// motivating file is `range-function-php82.php` (`< 8.3`) on an 8.5 engine.
    #[test]
    fn gates_admit_only_the_versions_they_name() {
        let lt83 = LintGate { op: GateOp::Lt, version: (8, 3) };
        assert!(!lt83.admits((8, 5)));
        assert!(!lt83.admits((8, 3))); // the boundary is exclusive
        assert!(lt83.admits((8, 2)));

        let ge81 = LintGate { op: GateOp::Ge, version: (8, 1) };
        assert!(ge81.admits((8, 5)));
        assert!(ge81.admits((8, 1)));
        assert!(!ge81.admits((8, 0)));
        // Minor is compared numerically, not lexically: 8.10 > 8.9.
        assert!(LintGate { op: GateOp::Ge, version: (8, 9) }.admits((8, 10)));
    }

    #[test]
    fn unsupported_expected_wins_over_subsumption() {
        // `*NEVER*` is the bottom type (relation would cover it too), but the
        // vocabulary verdict is decided first regardless.
        assert_eq!(classify("*NEVER*", "int").0, Verdict::Unsupported);
        assert_eq!(classify("int&object", "int").0, Verdict::Unsupported);
    }

    /// Issue #239: `mixed` is measured but never earns `equal`/`subsumed` — the
    /// covering direction is free for every rendering. See module docs (§`mixed`).
    #[test]
    fn mixed_is_measured_never_subsumed() {
        assert_eq!(unsupported_pattern("mixed"), None);
        // A concrete rendering under `mixed` is a gap, not precision — the corpus's
        // own shapes: `unknown` (324/329), a folded literal, an unresolvable phpdoc.
        assert_eq!(classify("mixed", "unknown").0, Verdict::Differ);
        assert_eq!(classify("mixed", "'name'").0, Verdict::Differ);
        assert_eq!(classify("mixed", "1").0, Verdict::Differ);
        assert_eq!(classify("mixed", "UnresolvableTypes\\array").0, Verdict::Differ);
        assert!(!is_subsumption("mixed", "int"));
        assert!(!is_proven_equal("mixed", "int"));
        assert_eq!(classify("mixed", "mixed").0, Verdict::Match); // normalization claims first
        assert_eq!(classify("MIXED", "mixed").0, Verdict::Match);
        assert!(!expected_is_top_type("non-null-mixed")); // veto is the top type only
        assert_eq!(classify("int", "mixed").0, Verdict::Differ); // got `mixed` uncovered too
    }

    /// Issue #239: the 329-row class must rank as `mixed`, not as a class name —
    /// `mixed` lowercases into a syntactically valid class identifier.
    #[test]
    fn mixed_has_its_own_gap_class() {
        assert_eq!(gap_class("mixed", "unknown"), "expected:mixed | steins:unknown");
        assert_eq!(atom_kind("non-null-mixed"), "mixed");
    }

    #[test]
    fn unsupported_patterns_are_named() {
        assert_eq!(unsupported_pattern("*ERROR*"), Some("phpstan-special"));
        assert_eq!(unsupported_pattern("*NEVER*"), Some("phpstan-special"));
        assert_eq!(unsupported_pattern("object"), Some("object"));
        assert_eq!(unsupported_pattern("int&object"), Some("intersection"));
        assert_eq!(unsupported_pattern("mixed~null"), Some("subtraction"));
        assert_eq!(unsupported_pattern("class-string<T>"), Some("class-string"));
        // Still-gated: non-array generics (S1.5 only opened the array vocabulary).
        assert_eq!(unsupported_pattern("Traversable<int, string>"), Some("generic-other"));
        // Supported vocab returns None below.
        assert_eq!(unsupported_pattern("int|null"), None);
        assert_eq!(unsupported_pattern("positive-int"), None);
        assert_eq!(unsupported_pattern("stdClass"), None);
        assert_eq!(unsupported_pattern("'foo'|'bar'"), None);
        // Casing pair (issue #77): spelled by `preds_keyword`, so measured.
        assert_eq!(unsupported_pattern("lowercase-string"), None);
        assert_eq!(unsupported_pattern("uppercase-string"), None);
        // Top type (issue #239): must stay ungated inside a union too.
        assert_eq!(unsupported_pattern("mixed"), None);
        assert_eq!(unsupported_pattern("int|mixed"), None);
        // Their PHPStan intersection spelling is measured too (issue #240): every
        // arm is a `StrPreds` bit, one closed predicate set the relation judges.
        assert_eq!(unsupported_pattern("lowercase-string&non-empty-string"), None);
        assert_eq!(
            unsupported_pattern("lowercase-string&non-falsy-string&uppercase-string"),
            None
        );
        assert_eq!(unsupported_pattern("(lowercase-string&non-falsy-string)|false"), None);
    }

    /// The #240 conjunction gate, from both sides: an atom is measured when EVERY
    /// arm is an extensional string refinement, and stays `intersection` otherwise.
    #[test]
    fn only_all_string_refinement_conjunctions_are_measured() {
        for a in [
            "numeric-string&uppercase-string",
            "non-empty-string&numeric-string",
            "decimal-int-string&non-falsy-string",
            "non-empty-lowercase-string&numeric-string", // compound cells are arms too
        ] {
            assert_eq!(unsupported_pattern(a), None, "{a} should be measured");
        }
        for a in [
            "literal-string&non-falsy-string", // provenance (`StrOpaque`), no predicate set
            // `class-string`'s bit is CONTEXTUAL (issue #236): measures class-table
            // reach, not the conjunction.
            "class-string&literal-string",
            "class-string&non-empty-string",
            "int&object", // `object` is reserved, never a plain class name (#238 gate too)
            "non-empty-string&hasOffset('a')", // base isn't array vocab, nothing to fold
        ] {
            assert_eq!(unsupported_pattern(a), Some("intersection"), "{a} should stay gated");
        }
    }

    /// Issue #238's two gates, from both sides: the object half splits into rows
    /// Steins can now be asked about (plain-class conjunctions; PHPStan's array
    /// accessory predicates folded into ADR-0062) and rows it still can't say.
    #[test]
    fn object_and_array_accessory_intersections_are_measured() {
        for a in [
            // Plain class conjunctions: lowered, spelled and judged arm-wise.
            "ArrayAccess&stdClass",
            "Countable&Traversable",
            "Bug14545\\ObjectClass&Bug14545\\SomeInterface",
            // Accessory predicates over an array base (ADR-0062 carries key
            // presence, and with the shape lane, the value at a key).
            "non-empty-array&hasOffset('foo')",
            "non-empty-array<string, int>&hasOffset('foo')",
            "non-empty-array<string, int>&hasOffsetValue('foo', 17)",
            "non-empty-list<int>&hasOffsetValue(0, 17)&hasOffsetValue(1, 19)",
        ] {
            assert_eq!(unsupported_pattern(a), None, "{a} should be measured");
        }
        for a in [
            "ArrayObject<int, array<string, mixed>>&hasOffset(1)", // class base, no fold
            "object&hasProperty(foo)", // object accessories: deliberately out of #238
            "object&hasMethod(doFoo)",
            // Template arm waits on #10.
            "list<int>&T (method Bug14631\\Foo::sortList(), argument)",
            "non-empty-list&callable(): mixed", // a callable arm is not a class
        ] {
            assert_eq!(unsupported_pattern(a), Some("intersection"), "{a} should stay gated");
        }
    }

    /// S1.5 (ADR-0062): the array vocabulary itself is no longer gated — the
    /// speller spells it (S1), so it is a fair comparison now.
    #[test]
    fn array_vocabulary_is_no_longer_unsupported() {
        for a in [
            "array{}",
            "array{a: int}",
            "list{}",
            "list{int, string}",
            "non-empty-array{a: int}",
            "non-empty-list{0: int}",
            "array<string>",
            "array<string, int>",
            "list<int>",
            "non-empty-array<int>",
            "non-empty-list<int>",
            "array",
            "non-empty-array",
            "list",
            "non-empty-list",
        ] {
            assert_eq!(unsupported_pattern(a), None, "{a} should be a supported comparison now");
        }
    }

    #[test]
    fn supported_atoms_are_not_flagged_unsupported() {
        for a in ["int", "float", "string", "bool", "true", "false", "null",
                  "non-empty-string", "numeric-string", "int<0, 5>", "-3", "1.5",
                  "'x'", "stdClass", "\\Foo\\Bar",
                  "array", "list", "non-empty-array", "non-empty-list",
                  "array{a: int}", "list<int>"] {
            assert!(is_supported_atom(a), "{a} should be supported");
        }
    }

    // ---- array vocabulary now classifiable (S1.5, ADR-0062) ----------------

    /// An array expectation now flows into the normal classify path instead of
    /// being gated Unsupported before `got` is ever read.
    #[test]
    fn array_expectation_is_classifiable() {
        assert_eq!(classify("array{a: int}", "array{a: int}").0, Verdict::Match);
        assert_eq!(classify("list<int>", "list<int>").0, Verdict::Match);
        assert_eq!(classify("array<string, int>", "array<string, int>").0, Verdict::Match);
    }

    /// A genuine D4-native divergence (Steins spells an empty/sequential array as
    /// `list{…}` where PHPStan asserts `array{…}`) lands in `equal`, never
    /// normalized away (ADR-0062 §6 as amended 2026-08-07, issue #172) — the
    /// award is the relation's proof, not a spelling rule.
    #[test]
    fn d4_native_list_vs_array_divergence_is_equal() {
        assert_eq!(classify("array{}", "list{}").0, Verdict::Equal);
        assert_ne!(normalize("array{}"), normalize("list{}"));
        // The proof, spelled out: mutual Yes through the checker's own relation.
        let dirs = subsumption_directions("array{}", "list{}");
        assert!(dirs.covers && dirs.covered);
    }

    /// The boundary of `equal` (issue #172): proven equality both ways, never a
    /// string trick. A pair the relation answers asymmetrically stays `differ`
    /// (a relation gap to file), and sentinels never qualify.
    #[test]
    fn equal_requires_mutual_proof_never_spelling() {
        assert_eq!(classify("bool", "true").0, Verdict::Subsumed); // narrowing, not equal
        assert_eq!(classify("true", "bool").0, Verdict::Differ); // widening stays differ
        assert_eq!(classify("array{}", "unknown").0, Verdict::Differ); // sentinel, no proof
        assert_eq!(classify("array{}", "array{}").0, Verdict::Match); // identical stays match
    }

    /// A pattern the speller still cannot spell (a non-array generic class, here
    /// with a type argument Steins' `lower_generic` would drop) stays Unsupported
    /// — S1.5 narrowed the gate, it did not remove it.
    #[test]
    fn still_gated_pattern_stays_unsupported() {
        assert_eq!(classify("Traversable<int, string>", "unknown").0, Verdict::Unsupported);
    }

    /// The `subtraction` bucket keeps its gate; the #239 top-type veto has nothing
    /// to do with it (issue #237). `unsupported_pattern` claims every `~` before
    /// [`classify`] reaches the relation, so [`expected_is_top_type`] is never
    /// reached (and would answer `false` anyway — a cut is its own atom). Lifting
    /// the gate would route these into `differ` with no direction asked at all,
    /// since the oracle's `~` spelling doesn't lower.
    #[test]
    fn subtraction_is_gated_before_the_top_type_veto_is_reached() {
        for s in ["mixed~null", "mixed~int", "mixed~(array|object|resource)"] {
            assert_eq!(unsupported_pattern(s), Some("subtraction"), "{s}");
            assert_eq!(classify(s, "unknown").0, Verdict::Unsupported, "{s}");
            assert!(!expected_is_top_type(s), "{s}"); // not the top type, veto is moot
            // Expected side doesn't parse — ungating would buy a `differ` row where
            // the relation is silent in both directions.
            assert!(steins_contract::lower_str(s).is_none(), "{s}");
            assert!(!subsumption_directions(s, "null").covers, "{s}");
            assert!(!subsumption_directions(s, "null").covered, "{s}");
        }
    }

    /// Steins' own spellings of the two cuts stay lowerable and judged in both
    /// directions, with `Maybe` staying silence (issue #237: nothing regresses).
    /// `mixed~null` denotes exactly `non-null-mixed` — a spelling miss over a set
    /// the engine already holds, which is why closing it moves nothing.
    #[test]
    fn the_two_cuts_stay_spellable_and_judged() {
        use steins_contract::normalize::subsumes;
        let nn = steins_contract::lower_str("non-null-mixed").expect("non-null-mixed lowers");
        let ne = steins_contract::lower_str("non-empty-mixed").expect("non-empty-mixed lowers");
        let nul = steins_contract::lower_str("null").expect("null lowers");
        // The cut excludes null outright (`No`); the reverse is undecided `Maybe`,
        // which stays silence — neither `equal` nor `subsumed` on a `Maybe`.
        assert!(subsumes(&nn, &nul).is_no());
        assert!(subsumes(&ne, &nul).is_no());
        assert!(!subsumes(&nul, &nn).is_yes());
        assert!(!subsumes(&nul, &ne).is_yes());
        // A concrete non-null value is covered — it's the `got` side that's empty
        // in the corpus, not the relation.
        let int = steins_contract::lower_str("int").expect("int lowers");
        assert!(subsumes(&nn, &int).is_yes());
        // Between two cuts the relation is silent by design (no scalar-fact
        // denotation to ask about) — `Maybe` both ways, never an award.
        assert!(!subsumes(&nn, &ne).is_yes());
        assert!(!subsumes(&ne, &nn).is_yes());
    }

    #[test]
    fn skipped_is_kept_out_of_measurement() {
        let obs = AssertObservation {
            path: "f.php".into(),
            line: 3,
            column: 1,
            expected: None,
            got: "unknown".into(),
            asserted: false,
        };
        let rec = Record::classify("f.php", obs);
        assert_eq!(rec.verdict, "skipped");
    }

    /// Regression for issue #246: phpstan-src's `tests/bench/data/nullsafe-chain-
    /// walk.php` nests a `->next` chain 1,000 deep; `scan_effect_origins` recurses
    /// ~2,500 frames and overflowed a debug build's default ~8 MiB stack, while
    /// fitting release's optimized frames. Reproduce the shape and drive it
    /// through `run`, whose worker thread is sized per [`WORKER_STACK_SIZE`].
    ///
    /// A stack overflow is not a catchable panic. If the worker-thread sizing in
    /// `run` regresses, this test aborts the whole process instead of failing
    /// cleanly — that abort is exactly the signal a regression here leaves.
    #[test]
    fn deep_property_chain_does_not_overflow_the_worker_stack() {
        let dir = std::env::temp_dir().join(format!("steins-nsrt-deep-chain-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir for the deep-chain fixture");

        let chain = "->next".repeat(1_500);
        let php = format!(
            "<?php\nclass Node {{ public Node $next; }}\nfunction f(Node $n): Node {{ return $n{chain}; }}\n"
        );
        std::fs::write(dir.join("deep_chain.php"), php).expect("write the deep-chain fixture");

        let result = run(Some(dir.to_str().expect("scratch path is valid UTF-8")));
        let _ = std::fs::remove_dir_all(&dir); // best-effort; a leftover temp dir is not a test failure
        assert!(result.is_ok(), "nsrt::run overflowed or errored on a deep-but-finite chain: {result:?}");
    }
}

