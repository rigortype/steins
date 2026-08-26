//! `perf`: the ADR-0092 performance harness — cold baselines and the
//! **cold half of the warm ≡ cold oracle** (ROADMAP M5, issue #483).
//!
//! Measures the library path the way `fp-gate` drives it (load → parse →
//! `check_project`, never a shelled-out binary, so process startup is not in
//! the numbers), per target tree, reporting the median over N runs. Two
//! verdicts ride on the measurement:
//!
//! - **Determinism (the point of this slice).** Every invocation runs the full
//!   cold analysis at least twice on identical inputs and asserts the runs'
//!   findings serialize byte-identically (sorted the way `steins check` sorts
//!   its output). This is the cold half of ADR-0092 §5's warm ≡ cold oracle:
//!   "warm findings ≡ cold findings" is only meaningful once "cold ≡ cold"
//!   holds, and this harness pins that today. Issue #489 extends this same
//!   comparison to warm-vs-cold when the generation layer lands — extend the
//!   comparison here, do not reinvent it.
//! - **The blessed baseline.** `--bless` records, per target, the file count,
//!   findings count, a SHA-256 of the serialized findings, the median cold
//!   timings, and the engine posture into `perf.local.toml` (repo root,
//!   machine-local, untracked like `corpus.local.toml`). Without `--bless`, a
//!   findings-hash mismatch against the baseline is a hard failure; timing
//!   deltas are printed but never fail — machine variance, and the enforcement
//!   point is later slices' cold-vs-pre-persistence comparison.
//!
//! Provisional M5 targets live HERE, not in the roadmap (ROADMAP M5): cold
//! within [`COLD_BUDGET_FRACTION`] of the pre-persistence baseline this
//! harness pins today, and warm re-check ≤ 2s p95 at the ~30k-file first-party
//! scale (unenforceable until issue #489 builds the warm path).
//!
//! Each run is cold by construction: a fresh salsa DB and a fresh sidecar per
//! run. The OS file cache is the one warmth the harness does not control,
//! which is part of why timing never gates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use steins_db::{EffectsPolicy, PluginFacts, Project, SourceFile, SteinsDatabase, composer, parse};
use steins_infer::{
    Diagnostic, FinalKeyword, GenerationMode, GenerationParams, SidecarFolder, check_project,
    generation_check,
};
use steins_syntax::SourceTree;

use crate::corpus::{collect_php_files, repo_root};
use crate::sha256;

/// Median-of-N default. Three is enough for a median that shrugs off one
/// scheduler hiccup without making a corpus-sized target tedious.
const DEFAULT_RUNS: usize = 3;

/// Headroom for the worker thread each cold run executes on — same number and
/// reasoning as `xtask/src/nsrt.rs` and the CLI's `WORKER_STACK_SIZE` (issue
/// #246): CST recursion costs a frame per nesting level, and an overflow
/// aborts the whole process. Lazily committed, free when unused.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

/// The machine-local baseline file at the repo root. Untracked (.gitignore),
/// beside `corpus.local.toml` in spirit: timings are one machine's.
const BASELINE_FILE: &str = "perf.local.toml";

/// Provisional M5 cold budget (ROADMAP M5 keeps the number here): once the
/// generation layer lands, a cold build must stay within this fraction over
/// the pre-persistence baseline this harness blesses today. Advisory now — a
/// crossed budget prints a note, timing never fails.
const COLD_BUDGET_FRACTION: f64 = 0.10;

/// Entry point for `cargo xtask perf <target-dir>... [--runs N] [--bless]
/// [--no-php] [--warm] [--paranoid]`. Returns `Ok(true)` when green;
/// `Ok(false)` on a determinism, baseline-hash, warm ≡ cold, or paranoid
/// divergence failure; `Err` for operator mistakes (bad args, unreadable
/// baseline, posture mismatch — a compare across postures is an error, not a
/// number).
///
/// `--warm` (issue #489, the warm half of ADR-0092 §5's oracle): per target,
/// cold-build + publish a generation into a scratch store, then measure warm
/// rebuilds (median over the same N) and assert in-process that every warm
/// run's findings hash equals the cold hash this invocation measured. The
/// lines are additive to the existing output; `--bless` still records the
/// cold numbers only — warm baselines become meaningful in slice B, so warm
/// timings are printed, never persisted.
///
/// `--paranoid` (issue #489 slice B) turns the walk verifier on for every
/// generation run this invocation makes: each warm rebuild walks *every* file
/// regardless of the affected set and asserts that each file it would have
/// skipped replays byte-identically. Any divergence fails the invocation.
/// Implies `--warm` — a cold-only run has nothing to skip — and costs the
/// skipping back, which is the price of the measurement.
pub fn run(args: &[String]) -> Result<bool, String> {
    let parsed = parse_args(args)?;
    if parsed.paranoid {
        println!(
            "perf: paranoid walk verification ON — every file is walked and every would-be skip is graded"
        );
    }
    let runs = if parsed.runs < 2 {
        // The determinism oracle needs two runs to compare; a single run
        // measures nothing this slice is for.
        println!("perf: --runs {} raised to 2 (the determinism oracle compares two runs)", parsed.runs);
        2
    } else {
        parsed.runs
    };
    let posture = if parsed.no_php { Posture::NoPhp } else { Posture::Php };

    let baseline_path = repo_root().join(BASELINE_FILE);
    let mut baseline = read_baseline(&baseline_path)?;

    let mut green = true;
    let mut blessed: Vec<BaselineEntry> = Vec::new();
    for target in &parsed.targets {
        let m = measure_target(Path::new(target), runs, posture)?;
        print_measurement(target, posture, &m);

        match &m.determinism {
            Determinism::Ok => println!(
                "    determinism: OK — {runs}/{runs} cold runs serialize byte-identically (the cold half of warm ≡ cold, ADR-0092 §5)"
            ),
            Determinism::Mismatch { run_index, diff } => {
                green = false;
                println!(
                    "    determinism: FAILED — run {run_index} serializes differently from run 1 on identical inputs"
                );
                for line in diff.lines() {
                    println!("      {line}");
                }
            }
        }

        if parsed.warm {
            match measure_warm(Path::new(target), runs, posture, parsed.paranoid, &m) {
                Ok(w) => {
                    print_warm(&w, &m);
                    if !w.matches || w.divergence_count > 0 {
                        green = false;
                    }
                }
                Err(e) => return Err(format!("target `{target}`: --warm failed: {e}")),
            }
        }

        if parsed.edits {
            match measure_edits(Path::new(target), posture) {
                Ok(rows) => {
                    if !print_edits(&rows) {
                        green = false;
                    }
                }
                Err(e) => return Err(format!("target `{target}`: --edits failed: {e}")),
            }
        }

        if parsed.bless {
            blessed.push(entry_from(target, posture, &m));
        } else {
            match verdict(baseline.get(target), &m, posture) {
                BaselineVerdict::NoBaseline => println!(
                    "    baseline: none recorded for this target on this machine — `--bless` to pin one"
                ),
                BaselineVerdict::PostureMismatch { recorded } => {
                    return Err(format!(
                        "target `{target}`: baseline was blessed under posture `{recorded}` but this run is `{}` — a cross-posture compare is an error, not a number; re-run under the blessed posture or re-bless",
                        posture.as_str()
                    ));
                }
                BaselineVerdict::HashMismatch { recorded } => {
                    green = false;
                    println!(
                        "    baseline: FINDINGS HASH MISMATCH — recorded {} findings over {} files (sha256 {}…), measured {} findings over {} files (sha256 {}…). The target tree moved or the analyzer changed what it finds; triage, then re-bless consciously.",
                        recorded.findings,
                        recorded.files,
                        &recorded.findings_sha256[..12.min(recorded.findings_sha256.len())],
                        m.findings,
                        m.files,
                        &m.findings_sha256[..12]
                    );
                }
                BaselineVerdict::Match { recorded } => {
                    println!("    baseline: findings hash matches ({BASELINE_FILE})");
                    print_timing_delta(&recorded, &m.median);
                }
            }
        }
    }

    if parsed.bless {
        if !green {
            println!("perf: refusing to bless — the determinism oracle failed; a baseline must pin a deterministic analysis");
            return Ok(false);
        }
        for e in blessed {
            baseline.upsert(e);
        }
        write_baseline(&baseline_path, &baseline)?;
        println!("perf: blessed {} target(s) → {}", parsed.targets.len(), baseline_path.display());
    }

    Ok(green)
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// Parsed `perf` arguments. `runs` is as requested — `run` raises it to the
/// oracle's floor of 2 with a printed note.
struct PerfArgs {
    targets: Vec<String>,
    runs: usize,
    bless: bool,
    no_php: bool,
    warm: bool,
    paranoid: bool,
    edits: bool,
}

fn parse_args(args: &[String]) -> Result<PerfArgs, String> {
    let mut targets = Vec::new();
    let mut runs = DEFAULT_RUNS;
    let mut bless = false;
    let mut no_php = false;
    let mut warm = false;
    let mut paranoid = false;
    let mut edits = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => {
                let v = it.next().ok_or("--runs needs a number")?;
                runs = v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| format!("--runs {v}: expected a positive integer"))?;
            }
            "--bless" => bless = true,
            "--no-php" => no_php = true,
            "--warm" => warm = true,
            // Verifying skips only means something over a published
            // generation, so the flag implies the warm half rather than
            // erroring on an operator who asked for one and not the other.
            "--paranoid" => {
                paranoid = true;
                warm = true;
            }
            // Seeded edits need something published to rebuild from and the
            // verifier to grade the rebuild, so the flag implies both.
            "--edits" => {
                edits = true;
                paranoid = true;
                warm = true;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}` (perf <target-dir>... [--runs N] [--bless] [--no-php] [--warm] [--paranoid] [--edits])"));
            }
            dir => targets.push(dir.to_owned()),
        }
    }
    if targets.is_empty() {
        return Err("usage: cargo xtask perf <target-dir>... [--runs N] [--bless] [--no-php] [--warm] [--paranoid] [--edits]".to_owned());
    }
    Ok(PerfArgs { targets, runs, bless, no_php, warm, paranoid, edits })
}

/// The engine posture a measurement ran under (recorded in the baseline; the
/// full ADR-0092 §2 identity — PHP version, width, extension set — arrives
/// with the generation fingerprint, not here). Cross-posture findings differ
/// legitimately (fold answers, curated-fact admission), so a baseline is only
/// comparable under the posture that blessed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// The default: fold via the resident PHP sidecar (ADR-0004).
    Php,
    /// `--no-php`: the sidecar disabled, folds decline.
    NoPhp,
}

impl Posture {
    fn as_str(self) -> &'static str {
        match self {
            Posture::Php => "php",
            Posture::NoPhp => "no-php",
        }
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// One cold run's phase timings, milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct RunTiming {
    /// Collect + read + `SourceFile` + parse of every file + layout/plugin
    /// discovery + `Project` construction.
    pub load_ms: f64,
    /// `check_project` on a fresh folder under the target's declared PHP range.
    pub analyze_ms: f64,
    /// The two phases' wall clock together.
    pub total_ms: f64,
}

/// What one invocation measured for one target: the canonical findings (count,
/// hash), per-run and median timings, and the determinism verdict.
pub struct Measurement {
    pub files: usize,
    pub findings: usize,
    pub findings_sha256: String,
    pub timings: Vec<RunTiming>,
    pub median: RunTiming,
    pub determinism: Determinism,
}

/// The within-invocation determinism verdict — the cold half of ADR-0092 §5's
/// warm ≡ cold oracle (see the module doc; issue #489 grows the warm half onto
/// this same comparison).
pub enum Determinism {
    /// Every run's canonical serialization is byte-identical.
    Ok,
    /// Run `run_index` (1-based) differs from run 1; `diff` is the printable
    /// per-diagnostic-id count diff (or the first differing line, when the
    /// counts agree and only bytes moved).
    Mismatch { run_index: usize, diff: String },
}

/// One cold run's full result, kept only long enough to compare runs.
struct ColdRun {
    files: usize,
    timing: RunTiming,
    serialized: String,
    id_counts: BTreeMap<&'static str, usize>,
}

/// Measure `dir` over `runs` cold runs on a worker thread sized per
/// [`WORKER_STACK_SIZE`] (the analysis recursion must not overflow `main`'s
/// default stack — same shape as `nsrt`).
pub fn measure_target(dir: &Path, runs: usize, posture: Posture) -> Result<Measurement, String> {
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || measure_on_worker(&dir, runs, posture))
        .expect("failed to spawn the perf worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn measure_on_worker(dir: &Path, runs: usize, posture: Posture) -> Result<Measurement, String> {
    if !dir.is_dir() {
        return Err(format!("target `{}` is not a directory", dir.display()));
    }
    let mut cold: Vec<ColdRun> = Vec::with_capacity(runs);
    for _ in 0..runs {
        cold.push(cold_run(dir, posture)?);
    }

    // Inputs must hold still for the oracle to mean anything: a tree that
    // changes mid-invocation is an operator problem, not nondeterminism.
    if let Some(moved) = cold.iter().position(|r| r.files != cold[0].files) {
        return Err(format!(
            "target `{}` changed while measuring: run 1 saw {} files, run {} saw {}",
            dir.display(),
            cold[0].files,
            moved + 1,
            cold[moved].files
        ));
    }

    let determinism = match cold.iter().position(|r| r.serialized != cold[0].serialized) {
        None => Determinism::Ok,
        Some(i) => Determinism::Mismatch {
            run_index: i + 1,
            diff: determinism_diff(&cold[0], &cold[i]),
        },
    };

    let timings: Vec<RunTiming> = cold.iter().map(|r| r.timing).collect();
    let median = RunTiming {
        load_ms: median(timings.iter().map(|t| t.load_ms)),
        analyze_ms: median(timings.iter().map(|t| t.analyze_ms)),
        total_ms: median(timings.iter().map(|t| t.total_ms)),
    };
    let findings = cold[0].id_counts.values().sum();
    let findings_sha256 = sha256::hex(cold[0].serialized.as_bytes());
    Ok(Measurement {
        files: cold[0].files,
        findings,
        findings_sha256,
        timings,
        median,
        determinism,
    })
}

/// One cold run: fresh DB, fresh folder, the `fp-gate` load path
/// (`xtask/src/gate.rs::analyze_local`) minus its gate bookkeeping — every
/// diagnostic `check_project` emits counts here, vendor and parse-error files
/// included, because the harness measures the analysis, not the gate policy.
fn cold_run(dir: &Path, posture: Posture) -> Result<ColdRun, String> {
    let t_load = Instant::now();
    let mut files = Vec::new();
    collect_php_files(dir, &mut files);
    files.sort();
    if files.is_empty() {
        return Err(format!("target `{}` holds no .php files", dir.display()));
    }

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        // Target-relative paths, like the gate's local-project path: keeps the
        // serialized findings (and so the hash) independent of where the
        // target happens to be mounted.
        let rel = f.strip_prefix(dir).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(), // unreadable → empty, as the gate reads it
        };
        inputs.push(SourceFile::new(&db, rel, text));
    }
    // Force the parse memo in the load phase, so the analyze phase times
    // inference rather than a parse it happens to trigger first.
    for &input in &inputs {
        let _ = parse(&db, input).parse_errors();
    }
    let layout = composer::discover(&[dir.to_path_buf()], dir);
    let php_target = layout.php_target().cloned();
    let plugins = steins_db::PluginFacts::discover(&layout, None);
    let project = Project::new(&db, inputs, layout, plugins);
    let load_ms = ms(t_load.elapsed());

    let t_analyze = Instant::now();
    // A fresh folder per run keeps the fold memo from warming run 2 — the run
    // is cold by construction. The target's declared PHP range applies, as in
    // the gate (issue #28/#63: an unset target silently changes fact seeding).
    let mut folder = match posture {
        Posture::Php => SidecarFolder::enabled(),
        Posture::NoPhp => SidecarFolder::new(true),
    };
    folder.set_php_target(php_target);
    let diags = check_project(&db, project, &mut folder);
    let analyze_ms = ms(t_analyze.elapsed());

    let mut id_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for d in &diags {
        *id_counts.entry(d.id).or_insert(0) += 1;
    }
    Ok(ColdRun {
        files: files.len(),
        timing: RunTiming { load_ms, analyze_ms, total_ms: load_ms + analyze_ms },
        serialized: canonical_serialization(diags),
        id_counts,
    })
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

// ---------------------------------------------------------------------------
// The warm half (issue #489): the generation lifecycle, measured
// ---------------------------------------------------------------------------

/// One warm rebuild's numbers, straight off the orchestrator's own report,
/// plus the loaded/parsed file split the no-change oracle reads.
struct WarmRun {
    capture_ms: f64,
    trees_ms: f64,
    analyze_ms: f64,
    /// The analyze split issue #516 asks for: merge, whole-universe facts,
    /// each fixpoint, the walk loop, the reporting passes.
    merge_ms: f64,
    facts_ms: f64,
    effects_ms: f64,
    throws_ms: f64,
    walk_ms: f64,
    report_ms: f64,
    persist_ms: f64,
    loaded: usize,
    parsed: usize,
    /// Files whose tree was actually decoded — issue #516's counter, and zero
    /// is what a no-change warm run must report.
    decoded: usize,
    /// Packages that both loaded and parsed — the shape of an edit inside a
    /// package under the per-file provenance gate (issue #512).
    mixed: usize,
    /// The walk split of issue #489 slice B: files walked vs files that
    /// replayed a persisted block instead.
    walked: usize,
    replayed: usize,
    /// Files the affected set would have skipped — under `--paranoid` these
    /// were walked anyway and graded, so this is the verified population.
    would_skip: usize,
}

/// What `--warm` measured for one target.
struct WarmMeasurement {
    /// The cold generation build + publish that seeded the store.
    cold_build_ms: f64,
    warm: Vec<WarmRun>,
    /// Whether every generation run (cold and warm alike) hashed identically
    /// to this invocation's measured cold baseline — the ADR-0092 §5 oracle,
    /// asserted in-process.
    matches: bool,
    /// The first mismatch, when there was one.
    mismatch: Option<String>,
    /// Whether the paranoid verifier ran, the divergence samples it kept over
    /// every generation run of this target, and how many there were in all
    /// (zero is the pass).
    paranoid: bool,
    divergences: Vec<String>,
    divergence_count: usize,
}

/// Cold-build + publish a generation into a scratch store, then measure `runs`
/// warm rebuilds over the untouched target. Runs on the same
/// [`WORKER_STACK_SIZE`] worker as the cold measurement.
fn measure_warm(
    dir: &Path,
    runs: usize,
    posture: Posture,
    paranoid: bool,
    cold: &Measurement,
) -> Result<WarmMeasurement, String> {
    let dir = dir.to_path_buf();
    let cold_hash = cold.findings_sha256.clone();
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || measure_warm_on_worker(&dir, runs, posture, paranoid, &cold_hash))
        .expect("failed to spawn the perf warm worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn measure_warm_on_worker(
    dir: &Path,
    runs: usize,
    posture: Posture,
    paranoid: bool,
    cold_hash: &str,
) -> Result<WarmMeasurement, String> {
    // The store lives in a scratch directory, never in the measured tree.
    let store = std::env::temp_dir().join(format!(
        "steins-perf-warm-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&store).map_err(|e| format!("create scratch store: {e}"))?;
    let result = measure_warm_in_store(dir, &store, runs, posture, paranoid, cold_hash);
    let _ = std::fs::remove_dir_all(&store);
    result
}

#[allow(clippy::too_many_arguments)]
fn measure_warm_in_store(
    dir: &Path,
    store: &Path,
    runs: usize,
    posture: Posture,
    paranoid: bool,
    cold_hash: &str,
) -> Result<WarmMeasurement, String> {
    // The same file list and target-relative diagnostic paths as `cold_run`,
    // so the canonical serialization (and therefore the hash) is comparable.
    let mut files = Vec::new();
    collect_php_files(dir, &mut files);
    files.sort();
    let rel: Vec<PathBuf> =
        files.iter().map(|f| f.strip_prefix(dir).unwrap_or(f).to_path_buf()).collect();
    let layout = composer::discover(&[dir.to_path_buf()], dir);
    let partition = steins_db::partition::discover(&layout);
    let plugins = PluginFacts::discover(&layout, None);
    let effects = EffectsPolicy::none();
    let params = GenerationParams {
        store_root: store,
        capture_root: dir,
        files: &rel,
        layout: &layout,
        partition: &partition,
        plugins: &plugins,
        effects: &effects,
        // `check_project`'s own defaults — what the cold measurement ran under.
        warning_handler_abort: true,
        final_keyword: FinalKeyword::Enforced,
        php: matches!(posture, Posture::Php),
        paranoid,
    };

    let check = |tag: &str, findings: Vec<Diagnostic>| -> Option<String> {
        let hash = sha256::hex(canonical_serialization(findings).as_bytes());
        (hash != cold_hash).then(|| {
            format!("{tag} findings hash {hash} != measured cold hash {cold_hash}")
        })
    };

    // Seed: one cold generation build + publish.
    let t = Instant::now();
    let outcome = generation_check(&params).map_err(|e| format!("cold generation build: {e}"))?;
    let cold_build_ms = ms(t.elapsed());
    if outcome.report.mode != GenerationMode::Cold {
        return Err("the scratch store unexpectedly held a generation".to_owned());
    }
    let paranoid = outcome.report.walk.paranoid;
    let mut divergences: Vec<String> =
        outcome.report.walk.divergences.iter().map(ToString::to_string).collect();
    let mut divergence_count = outcome.report.walk.divergence_count;
    let mut mismatch = check("cold generation build", outcome.findings);

    let mut warm: Vec<WarmRun> = Vec::with_capacity(runs);
    for i in 0..runs {
        let outcome =
            generation_check(&params).map_err(|e| format!("warm rebuild {}: {e}", i + 1))?;
        if outcome.report.mode != GenerationMode::Warm {
            return Err(format!("warm rebuild {} ran cold — no published generation", i + 1));
        }
        let (loaded, parsed, decoded) = outcome.report.packages.iter().fold(
            (0usize, 0usize, 0usize),
            |(l, p, d), pkg| (l + pkg.loaded, p + pkg.parsed, d + pkg.decoded),
        );
        let mixed = outcome.report.packages.iter().filter(|pkg| pkg.is_mixed()).count();
        let t = outcome.report.timings;
        let w = &outcome.report.walk;
        divergences.extend(w.divergences.iter().map(ToString::to_string));
        divergence_count += w.divergence_count;
        warm.push(WarmRun {
            capture_ms: t.capture_ms,
            trees_ms: t.trees_ms,
            analyze_ms: t.analyze_ms,
            merge_ms: t.merge_ms,
            facts_ms: t.facts_ms,
            effects_ms: t.effects_ms,
            throws_ms: t.throws_ms,
            walk_ms: t.walk_ms,
            report_ms: t.report_ms,
            persist_ms: t.persist_ms,
            loaded,
            parsed,
            decoded,
            mixed,
            walked: w.walked,
            replayed: w.replayed,
            would_skip: w.would_skip,
        });
        if mismatch.is_none() {
            mismatch = check(&format!("warm rebuild {}", i + 1), outcome.findings);
        }
    }

    Ok(WarmMeasurement {
        cold_build_ms,
        warm,
        matches: mismatch.is_none(),
        mismatch,
        paranoid,
        divergences,
        divergence_count,
    })
}

/// Print the warm measurement under the cold block. Additive lines only; warm
/// timings are never persisted (`--bless` records the cold numbers — warm
/// baselines become meaningful in slice B).
fn print_warm(w: &WarmMeasurement, cold: &Measurement) {
    println!("    warm (experimental generations, ADR-0092 §5):");
    println!("      cold build+publish into a scratch store: {:.1} ms", w.cold_build_ms);
    for (i, run) in w.warm.iter().enumerate() {
        println!(
            "      warm run {}: capture {:.1} ms, trees {:.1} ms ({} loaded, {} parsed{}, {} tree(s) decoded), analyze {:.1} ms ({} walked, {} replayed), persist {:.1} ms, total {:.1} ms",
            i + 1,
            run.capture_ms,
            run.trees_ms,
            run.loaded,
            run.parsed,
            if run.mixed == 0 {
                String::new()
            } else {
                format!(", {} package(s) partly reused", run.mixed)
            },
            run.decoded,
            run.analyze_ms,
            run.walked,
            run.replayed,
            run.persist_ms,
            run.capture_ms + run.trees_ms + run.analyze_ms + run.persist_ms,
        );
        println!(
            "        analyze split: merge {:.1} ms, facts {:.1} ms, effects {:.1} ms, throws {:.1} ms, walk {:.1} ms, report {:.1} ms",
            run.merge_ms,
            run.facts_ms,
            run.effects_ms,
            run.throws_ms,
            run.walk_ms,
            run.report_ms,
        );
    }
    let total =
        |r: &WarmRun| r.capture_ms + r.trees_ms + r.analyze_ms + r.persist_ms;
    println!(
        "      warm median: total {:.1} ms (cold median load+parse {:.1} ms + analyze {:.1} ms = {:.1} ms)",
        median(w.warm.iter().map(total)),
        cold.median.load_ms,
        cold.median.analyze_ms,
        cold.median.total_ms,
    );
    match &w.mismatch {
        None => println!(
            "      warm ≡ cold: OK — every generation run's findings hash equals the measured cold hash ({}…)",
            &cold.findings_sha256[..12]
        ),
        Some(detail) => println!("      warm ≡ cold: FAILED — {detail}"),
    }
    if w.paranoid {
        let graded: usize = w.warm.iter().map(|r| r.would_skip).sum();
        if w.divergence_count == 0 {
            println!(
                "      paranoid: OK — {graded} would-be skip(s) graded across {} run(s), every one byte-identical to its fresh walk",
                w.warm.len()
            );
        } else {
            println!(
                "      paranoid: FAILED — {} divergence(s) over {graded} graded would-be skip(s); first {} shown",
                w.divergence_count,
                w.divergences.len()
            );
            for line in &w.divergences {
                println!("        {line}");
            }
        }
    }
}

/// The median of an iterator of milliseconds; the mean of the two middles on
/// an even count.
fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

// ---------------------------------------------------------------------------
// Canonical serialization — what the determinism oracle and the hash pin
// ---------------------------------------------------------------------------

/// Serialize findings the way the CLI's output path does: sorted by
/// `(path, line, column, id)` (`crates/steins-cli/src/check.rs`), then one
/// compact JSON object per line in the `--format json` field order (`id`,
/// `path`, `line`, `column`, `message`, plus the additive facet and fix keys —
/// `serde_json`'s `preserve_order` keeps insertion order). `layer` and `level`
/// are omitted: both are functions of the id and the active surface, not of
/// the analysis run, so they cannot carry nondeterminism the id itself would
/// not.
pub fn canonical_serialization(mut diags: Vec<Diagnostic>) -> String {
    diags.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column, a.id).cmp(&(b.path.as_str(), b.line, b.column, b.id))
    });
    let mut out = String::new();
    for d in &diags {
        let mut obj = serde_json::json!({
            "id": d.id,
            "path": d.path,
            "line": d.line,
            "column": d.column,
            "message": d.message,
        });
        if let Some(facet) = d.facet {
            obj[facet.key()] = serde_json::Value::String(facet.value().to_owned());
        }
        if let Some(fix) = &d.fix {
            let edits: Vec<serde_json::Value> = fix
                .edits
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "path": e.path,
                        "span": { "start": e.start, "end": e.end },
                        "replacement": e.replacement,
                    })
                })
                .collect();
            obj["fix"] = serde_json::json!({ "title": fix.title, "edits": edits });
        }
        out.push_str(&obj.to_string());
        out.push('\n');
    }
    out
}

/// The printable diff between two runs that failed the oracle: per-id count
/// movements when any id's count moved, else the first byte-differing line
/// (same counts, different rendering — a message or ordering instability).
fn determinism_diff(first: &ColdRun, other: &ColdRun) -> String {
    let moved = id_count_diff(&first.id_counts, &other.id_counts);
    if !moved.is_empty() {
        let mut out = String::from("per-diagnostic-id count diff (run 1 → mismatching run):\n");
        for (id, a, b) in moved {
            out.push_str(&format!("  {id}: {a} → {b}\n"));
        }
        return out;
    }
    let mut out = String::from("per-id counts agree; the serialization differs in rendering:\n");
    for (a, b) in first.serialized.lines().zip(other.serialized.lines()) {
        if a != b {
            out.push_str(&format!("  run 1: {a}\n  other: {b}\n"));
            return out;
        }
    }
    // Same lines, different length (one is a prefix of the other).
    out.push_str(&format!(
        "  run 1 has {} finding(s), the other {}\n",
        first.serialized.lines().count(),
        other.serialized.lines().count()
    ));
    out
}

/// The ids whose counts differ between two runs, as `(id, count_a, count_b)`,
/// sorted by id. An id absent from a run counts 0.
fn id_count_diff(
    a: &BTreeMap<&'static str, usize>,
    b: &BTreeMap<&'static str, usize>,
) -> Vec<(&'static str, usize, usize)> {
    let mut ids: Vec<&'static str> = a.keys().chain(b.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .filter_map(|id| {
            let ca = a.get(id).copied().unwrap_or(0);
            let cb = b.get(id).copied().unwrap_or(0);
            (ca != cb).then_some((id, ca, cb))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The baseline file
// ---------------------------------------------------------------------------

/// The machine-local baseline document (`perf.local.toml`).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Baseline {
    #[serde(default, rename = "target")]
    pub targets: Vec<BaselineEntry>,
}

/// One blessed target. `path` is the target argument as given on the command
/// line — the file is machine-local, so a spelling is an identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub path: String,
    /// `"php"` or `"no-php"` — see [`Posture`].
    pub posture: String,
    pub files: usize,
    pub findings: usize,
    pub findings_sha256: String,
    pub load_ms: f64,
    pub analyze_ms: f64,
    pub total_ms: f64,
}

impl Baseline {
    /// The entry blessed for `path`, if any.
    pub fn get(&self, path: &str) -> Option<&BaselineEntry> {
        self.targets.iter().find(|e| e.path == path)
    }

    /// Insert or replace the entry for `entry.path`, keeping the file sorted
    /// by path for a stable diff (the `corpus.lock.toml` discipline).
    pub fn upsert(&mut self, entry: BaselineEntry) {
        if let Some(slot) = self.targets.iter_mut().find(|e| e.path == entry.path) {
            *slot = entry;
        } else {
            self.targets.push(entry);
        }
        self.targets.sort_by(|a, b| a.path.cmp(&b.path));
    }
}

/// Read the baseline, or an empty one if the file does not exist yet. A
/// malformed file is an error naming the remedy, not a silent fresh start —
/// silently dropping baselines is how a regression sails through.
fn read_baseline(path: &Path) -> Result<Baseline, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|e| format!("{} is malformed ({e}); fix or delete it, then re-bless", path.display())),
        Err(_) => Ok(Baseline::default()),
    }
}

/// Write the baseline with a short provenance header (`corpus.lock.toml`'s shape).
fn write_baseline(path: &Path, baseline: &Baseline) -> Result<(), String> {
    let body = toml::to_string_pretty(baseline).expect("baseline serializes");
    let text = format!(
        "# Machine-local perf baselines (ADR-0092 §5 / ROADMAP M5). Generated by\n\
         # `cargo xtask perf <target>... --bless`. Untracked: the timings are this\n\
         # machine's; the findings hash pins the cold analysis of each target tree.\n\n{body}"
    );
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// A [`BaselineEntry`] from a finished measurement, timings rounded to 0.1 ms
/// so the file diffs readably.
fn entry_from(target: &str, posture: Posture, m: &Measurement) -> BaselineEntry {
    let round = |x: f64| (x * 10.0).round() / 10.0;
    BaselineEntry {
        path: target.to_owned(),
        posture: posture.as_str().to_owned(),
        files: m.files,
        findings: m.findings,
        findings_sha256: m.findings_sha256.clone(),
        load_ms: round(m.median.load_ms),
        analyze_ms: round(m.median.analyze_ms),
        total_ms: round(m.median.total_ms),
    }
}

/// How a measurement relates to its blessed baseline.
pub enum BaselineVerdict {
    /// Nothing blessed for this target on this machine.
    NoBaseline,
    /// Blessed under the other engine posture — refuse to compare (an error,
    /// not a number: cross-posture findings differ legitimately).
    PostureMismatch { recorded: String },
    /// The findings hash moved: hard failure, triage then re-bless.
    HashMismatch { recorded: BaselineEntry },
    /// The findings hash matches; timing deltas are advisory.
    Match { recorded: BaselineEntry },
}

/// Judge `m` against the blessed entry, posture first: a hash comparison
/// across postures would attribute a legitimate posture difference to drift.
pub fn verdict(entry: Option<&BaselineEntry>, m: &Measurement, posture: Posture) -> BaselineVerdict {
    let Some(e) = entry else { return BaselineVerdict::NoBaseline };
    if e.posture != posture.as_str() {
        return BaselineVerdict::PostureMismatch { recorded: e.posture.clone() };
    }
    if e.findings_sha256 != m.findings_sha256 {
        return BaselineVerdict::HashMismatch { recorded: e.clone() };
    }
    BaselineVerdict::Match { recorded: e.clone() }
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn print_measurement(target: &str, posture: Posture, m: &Measurement) {
    println!(
        "\nperf: {target} — {} files, {} findings, posture {}, {} run(s)",
        m.files,
        m.findings,
        posture.as_str(),
        m.timings.len()
    );
    for (i, t) in m.timings.iter().enumerate() {
        println!(
            "    run {}: load+parse {:.1} ms, analyze {:.1} ms, total {:.1} ms",
            i + 1,
            t.load_ms,
            t.analyze_ms,
            t.total_ms
        );
    }
    println!(
        "    median: load+parse {:.1} ms, analyze {:.1} ms, total {:.1} ms  (findings sha256 {}…)",
        m.median.load_ms,
        m.median.analyze_ms,
        m.median.total_ms,
        &m.findings_sha256[..12]
    );
}

/// Print the timing movement against the blessed medians. Never gates: this
/// is machine variance until later slices measure cold-vs-pre-persistence.
/// The provisional M5 budget prints as an advisory when crossed.
fn print_timing_delta(recorded: &BaselineEntry, median: &RunTiming) {
    let pct = |old: f64, new: f64| if old > 0.0 { (new - old) / old * 100.0 } else { 0.0 };
    let total_pct = pct(recorded.total_ms, median.total_ms);
    println!(
        "    timing vs baseline (never gates): load+parse {:.1} → {:.1} ms ({:+.1}%), analyze {:.1} → {:.1} ms ({:+.1}%), total {:.1} → {:.1} ms ({:+.1}%)",
        recorded.load_ms,
        median.load_ms,
        pct(recorded.load_ms, median.load_ms),
        recorded.analyze_ms,
        median.analyze_ms,
        pct(recorded.analyze_ms, median.analyze_ms),
        recorded.total_ms,
        median.total_ms,
        total_pct
    );
    if total_pct > COLD_BUDGET_FRACTION * 100.0 {
        println!(
            "    note: total is {total_pct:+.1}% over the blessed baseline — past the provisional M5 cold budget ({:.0}%); advisory only until the generation layer lands",
            COLD_BUDGET_FRACTION * 100.0
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Seeded edits (issue #516): the warm path graded against a fresh cold run,
// one edit shape at a time.
// ---------------------------------------------------------------------------

/// One seeded edit's verdict: what the warm rebuild did, whether the verifier
/// found a divergence, and whether the findings equal a fresh cold run's over
/// the same edited tree.
struct EditRow {
    shape: &'static str,
    file: String,
    walked: usize,
    replayed: usize,
    would_skip: usize,
    parsed: usize,
    decoded: usize,
    warm_ms: f64,
    capture_ms: f64,
    trees_ms: f64,
    analyze_ms: f64,
    persist_ms: f64,
    /// Artifacts the publish shared with the previous generation rather than
    /// rewriting (issue #519) — the multi-package half of the persist story.
    shared: usize,
    divergences: usize,
    /// `None` when warm equalled the fresh cold run; the mismatch otherwise.
    mismatch: Option<String>,
}

/// Copy `dir` into a scratch tree, publish a cold generation over it, then
/// seed one edit shape at a time — each graded warm-with-paranoid against a
/// fresh cold run of the *edited* tree.
///
/// The tree is copied because every shape mutates it; the corpus checkouts are
/// never written to. Edits accumulate rather than reverting, which is both
/// cheaper and closer to how a warm store is actually used: each rebuild
/// publishes, and the next shape starts from that generation.
fn measure_edits(dir: &Path, posture: Posture) -> Result<Vec<EditRow>, String> {
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || measure_edits_on_worker(&dir, posture))
        .expect("failed to spawn the perf edits worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn measure_edits_on_worker(dir: &Path, posture: Posture) -> Result<Vec<EditRow>, String> {
    let scratch = std::env::temp_dir().join(format!(
        "steins-perf-edits-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create scratch: {e}"))?;
    let tree = scratch.join("tree");
    // Two stores, fed the same edits: one measures what a warm rebuild costs,
    // the other runs the verifier — which walks everything and so measures
    // nothing but its own thoroughness.
    let cost = scratch.join("cost");
    let grade = scratch.join("grade");
    std::fs::create_dir_all(&cost).map_err(|e| format!("create store: {e}"))?;
    std::fs::create_dir_all(&grade).map_err(|e| format!("create store: {e}"))?;
    let result = copy_tree(dir, &tree)
        .map_err(|e| format!("copy the target: {e}"))
        .and_then(|()| edit_scenarios(&tree, &cost, &grade, posture));
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            // `.git` is large and irrelevant to the analysis.
            if entry.file_name() == ".git" {
                continue;
            }
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The five edit shapes, seeded in order over one accumulating tree.
fn edit_scenarios(
    tree: &Path,
    cost: &Path,
    grade_store: &Path,
    posture: Posture,
) -> Result<Vec<EditRow>, String> {
    let mut files = Vec::new();
    collect_php_files(tree, &mut files);
    files.sort();
    if files.len() < 3 {
        return Err("a seeded-edit run needs at least three files".to_owned());
    }
    // Shape selection by a property of the *universe*, not by a file name, so
    // the same five shapes land on any target.
    //
    // A leaf is a file nothing else names. Measured rather than guessed: one
    // pass over every file's text builds an identifier frequency table, and a
    // file's inbound weight is the frequency of the names it declares. A file
    // that declares nothing, or whose declarations nobody spells, scores zero
    // and is the leaf. (Prose counts as a mention here, which is the right
    // side to err on for picking a fixture — a file the corpus never even
    // *says* is a leaf by any reading.)
    let texts: Vec<String> =
        files.iter().map(|p| std::fs::read_to_string(p).unwrap_or_default()).collect();
    let mut frequency: BTreeMap<String, usize> = BTreeMap::new();
    for text in &texts {
        for token in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if token.is_empty() {
                continue;
            }
            *frequency.entry(token.to_ascii_lowercase()).or_default() += 1;
        }
    }
    let parsed: Vec<SourceTree> = texts.iter().map(|t| SourceTree::parse(t)).collect();
    let inbound = |i: usize| -> usize {
        let tree = &parsed[i];
        let names = tree
            .classes()
            .iter()
            .map(|c| c.name.to_ascii_lowercase())
            .chain(tree.functions().iter().map(|f| f.name.to_ascii_lowercase()));
        names.map(|n| frequency.get(&n).copied().unwrap_or(0)).sum()
    };
    let pick = |by: &dyn Fn(usize) -> usize, most: bool| -> PathBuf {
        let mut best = 0usize;
        for i in 1..files.len() {
            let better = if most { by(i) > by(best) } else { by(i) < by(best) };
            if better {
                best = i;
            }
        }
        files[best].clone()
    };
    let leaf = pick(&inbound, false);
    let core = pick(&inbound, true);
    let declarer = pick(&|i| parsed[i].classes().len(), true);
    let added = tree.join("steins_perf_added.php");
    drop(parsed);

    // Seed both stores with a cold build over the untouched copy.
    let mut rows = Vec::new();
    let _ = run_generation(tree, cost, posture, false)?;
    let _ = run_generation(tree, grade_store, posture, true)?;

    let comment = "\n// steins perf: seeded edit\n";
    let append = |path: &Path| -> Result<(), String> {
        let mut text = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
        text.push_str(comment);
        std::fs::write(path, text).map_err(|e| format!("write: {e}"))
    };

    // 1. A leaf edit.
    append(&leaf)?;
    rows.push(grade(tree, cost, grade_store, posture, "leaf (least-named file)", &leaf)?);

    // 2. A core edit.
    append(&core)?;
    rows.push(grade(tree, cost, grade_store, posture, "core (most-named file)", &core)?);

    // 3. The declarer most classes live in.
    append(&declarer)?;
    rows.push(grade(
        tree,
        cost,
        grade_store,
        posture,
        "declarer (most class-likes)",
        &declarer,
    )?);

    // 4. A file addition.
    std::fs::write(
        &added,
        "<?php\nnamespace SteinsPerf;\nclass Added {}\nfunction addedFn(): int { return 1; }\n",
    )
    .map_err(|e| format!("write the added file: {e}"))?;
    rows.push(grade(tree, cost, grade_store, posture, "addition (a new file)", &added)?);

    // 5. A file removal — the file just added, so the universe returns to the
    //    shape the other four shapes left it in.
    std::fs::remove_file(&added).map_err(|e| format!("remove: {e}"))?;
    rows.push(grade(tree, cost, grade_store, posture, "removal (that file again)", &added)?);

    Ok(rows)
}

/// One graded scenario: a warm rebuild with the verifier on, then a fresh cold
/// run of the same tree in its own store, compared finding for finding.
fn grade(
    tree: &Path,
    cost: &Path,
    grade_store: &Path,
    posture: Posture,
    shape: &'static str,
    file: &Path,
) -> Result<EditRow, String> {
    // The cost run: what a warm rebuild of this edit actually does.
    let warm = run_generation(tree, cost, posture, false)?;
    if warm.0 != GenerationMode::Warm {
        return Err(format!("{shape}: the rebuild ran cold"));
    }
    // The graded run, in its own store: the verifier walks every file anyway
    // and compares each would-be skip against its fresh walk.
    let verified = run_generation(tree, grade_store, posture, true)?;
    // And the oracle: a cold build of the same tree, in a store of its own.
    let fresh_store = cost.with_extension("fresh");
    let _ = std::fs::remove_dir_all(&fresh_store);
    std::fs::create_dir_all(&fresh_store).map_err(|e| format!("create fresh store: {e}"))?;
    let cold = run_generation(tree, &fresh_store, posture, false)?;
    let _ = std::fs::remove_dir_all(&fresh_store);
    let cold_hash = sha256::hex(canonical_serialization(cold.1).as_bytes());
    let warm_hash = sha256::hex(canonical_serialization(warm.1).as_bytes());
    let verified_hash = sha256::hex(canonical_serialization(verified.1).as_bytes());
    let mismatch = if warm_hash != cold_hash {
        Some(format!("warm {warm_hash} != fresh cold {cold_hash}"))
    } else if verified_hash != cold_hash {
        Some(format!("paranoid warm {verified_hash} != fresh cold {cold_hash}"))
    } else {
        None
    };
    let t = warm.2.timings;
    Ok(EditRow {
        shape,
        file: file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        walked: warm.2.walk.walked,
        replayed: warm.2.walk.replayed,
        would_skip: verified.2.walk.would_skip,
        parsed: warm.2.packages.iter().map(|p| p.parsed).sum(),
        decoded: warm.2.packages.iter().map(|p| p.decoded).sum(),
        warm_ms: t.total_ms(),
        capture_ms: t.capture_ms,
        trees_ms: t.trees_ms,
        analyze_ms: t.analyze_ms,
        persist_ms: t.persist_ms,
        shared: warm.2.shared_artifacts,
        divergences: verified.2.walk.divergence_count,
        mismatch,
    })
}

/// One generation run over `tree` against `store`, with the caller's own
/// boundary resolution — the same one `measure_warm_in_store` performs.
fn run_generation(
    tree: &Path,
    store: &Path,
    posture: Posture,
    paranoid: bool,
) -> Result<(GenerationMode, Vec<Diagnostic>, steins_infer::GenerationReport), String> {
    let mut files = Vec::new();
    collect_php_files(tree, &mut files);
    files.sort();
    let rel: Vec<PathBuf> =
        files.iter().map(|f| f.strip_prefix(tree).unwrap_or(f).to_path_buf()).collect();
    let layout = composer::discover(&[tree.to_path_buf()], tree);
    let partition = steins_db::partition::discover(&layout);
    let plugins = PluginFacts::discover(&layout, None);
    let effects = EffectsPolicy::none();
    let params = GenerationParams {
        store_root: store,
        capture_root: tree,
        files: &rel,
        layout: &layout,
        partition: &partition,
        plugins: &plugins,
        effects: &effects,
        warning_handler_abort: true,
        final_keyword: FinalKeyword::Enforced,
        php: matches!(posture, Posture::Php),
        paranoid,
    };
    let outcome = generation_check(&params).map_err(|e| e.to_string())?;
    Ok((outcome.report.mode, outcome.findings, outcome.report))
}

/// Print the seeded-edit table; `false` when any shape diverged or disagreed
/// with its fresh cold run.
fn print_edits(rows: &[EditRow]) -> bool {
    println!("    seeded edits (warm + paranoid, each graded against a fresh cold run):");
    let mut green = true;
    for row in rows {
        println!(
            "      {:<28} {:<26} {} walked / {} replayed, {} parsed, {} tree(s) decoded — capture {:.1} + trees {:.1} + analyze {:.1} + persist {:.1}{} = {:.1} ms (verifier graded {})",
            row.shape,
            row.file,
            row.walked,
            row.replayed,
            row.parsed,
            row.decoded,
            row.capture_ms,
            row.trees_ms,
            row.analyze_ms,
            row.persist_ms,
            if row.shared == 0 {
                String::new()
            } else {
                format!(" ({} artifact(s) shared)", row.shared)
            },
            row.warm_ms,
            row.would_skip,
        );
        if row.divergences > 0 {
            green = false;
            println!("        paranoid: FAILED — {} divergence(s)", row.divergences);
        }
        if let Some(detail) = &row.mismatch {
            green = false;
            println!("        warm ≡ cold: FAILED — {detail}");
        }
    }
    if green {
        let graded: usize = rows.iter().map(|r| r.would_skip).sum();
        println!(
            "      OK — {graded} would-be skip(s) graded byte-identical across {} shape(s), every shape's findings equal to a fresh cold run",
            rows.len()
        );
    }
    green
}

#[cfg(test)]
mod tests {
    use super::*;
    use steins_infer::{Facet, Origin};

    fn diag(id: &'static str, path: &str, line: u32, column: u32, message: &str) -> Diagnostic {
        Diagnostic {
            id,
            path: path.to_owned(),
            line,
            column,
            message: message.to_owned(),
            facet: None,
            fix: None,
        }
    }

    #[test]
    fn canonical_serialization_sorts_like_the_cli_and_is_input_order_independent() {
        let a = diag("type.mismatch", "src/a.php", 3, 1, "a");
        let b = diag("variable.undefined", "src/a.php", 3, 1, "b");
        let c = diag("type.mismatch", "src/b.php", 1, 9, "c");
        // Two presentation orders of the same set serialize identically…
        let one = canonical_serialization(vec![c.clone(), b.clone(), a.clone()]);
        let two = canonical_serialization(vec![a.clone(), c.clone(), b.clone()]);
        assert_eq!(one, two);
        // …in the CLI's (path, line, column, id) order.
        let lines: Vec<&str> = one.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"type.mismatch\""), "{one}");
        assert!(lines[1].contains("\"variable.undefined\""), "{one}");
        assert!(lines[2].contains("src/b.php"), "{one}");
    }

    #[test]
    fn canonical_serialization_keeps_the_additive_facet_key() {
        let mut d = diag("throw.undeclared", "src/a.php", 1, 1, "escapes");
        d.facet = Some(Facet::Origin(Origin::Direct));
        let s = canonical_serialization(vec![d]);
        assert!(s.contains("\"origin\":\"direct\""), "{s}");
    }

    #[test]
    fn id_count_diff_reports_only_moved_ids() {
        let mut a = BTreeMap::new();
        a.insert("type.mismatch", 5);
        a.insert("variable.undefined", 2);
        let mut b = BTreeMap::new();
        b.insert("type.mismatch", 6);
        b.insert("variable.undefined", 2);
        b.insert("throw.undeclared", 1);
        let diff = id_count_diff(&a, &b);
        assert_eq!(diff, vec![("throw.undeclared", 0, 1), ("type.mismatch", 5, 6)]);
    }

    #[test]
    fn median_takes_the_middle_and_averages_an_even_pair() {
        assert_eq!(median([3.0, 1.0, 2.0].into_iter()), 2.0);
        assert_eq!(median([4.0, 2.0].into_iter()), 3.0);
        assert_eq!(median(std::iter::empty()), 0.0);
    }

    #[test]
    fn baseline_round_trips_through_toml_and_upsert_replaces() {
        let mut baseline = Baseline::default();
        baseline.upsert(BaselineEntry {
            path: "corpus/b".to_owned(),
            posture: "php".to_owned(),
            files: 10,
            findings: 2,
            findings_sha256: "aa".repeat(32),
            load_ms: 12.5,
            analyze_ms: 100.0,
            total_ms: 112.5,
        });
        baseline.upsert(BaselineEntry {
            path: "corpus/a".to_owned(),
            posture: "no-php".to_owned(),
            files: 3,
            findings: 0,
            findings_sha256: "bb".repeat(32),
            load_ms: 1.0,
            analyze_ms: 2.0,
            total_ms: 3.0,
        });
        // Sorted by path for a stable file.
        assert_eq!(baseline.targets[0].path, "corpus/a");
        // Replacing keeps one entry per path.
        let mut replaced = baseline.targets[1].clone();
        replaced.findings = 7;
        baseline.upsert(replaced);
        assert_eq!(baseline.targets.len(), 2);
        assert_eq!(baseline.get("corpus/b").unwrap().findings, 7);

        let text = toml::to_string_pretty(&baseline).expect("serializes");
        let back: Baseline = toml::from_str(&text).expect("parses");
        assert_eq!(back.targets, baseline.targets);
    }

    #[test]
    fn verdict_orders_posture_before_hash() {
        let entry = BaselineEntry {
            path: "t".to_owned(),
            posture: "php".to_owned(),
            files: 1,
            findings: 1,
            findings_sha256: "cc".repeat(32),
            load_ms: 1.0,
            analyze_ms: 1.0,
            total_ms: 2.0,
        };
        let m = Measurement {
            files: 1,
            findings: 1,
            findings_sha256: "dd".repeat(32),
            timings: vec![],
            median: RunTiming { load_ms: 1.0, analyze_ms: 1.0, total_ms: 2.0 },
            determinism: Determinism::Ok,
        };
        // Same posture, different hash → the hash mismatch reds.
        assert!(matches!(
            verdict(Some(&entry), &m, Posture::Php),
            BaselineVerdict::HashMismatch { .. }
        ));
        // Different posture → refused before any hash comparison.
        assert!(matches!(
            verdict(Some(&entry), &m, Posture::NoPhp),
            BaselineVerdict::PostureMismatch { .. }
        ));
        // No entry → no baseline, never a failure.
        assert!(matches!(verdict(None, &m, Posture::Php), BaselineVerdict::NoBaseline));
    }

    /// Integration smoke: a tiny self-contained PHP tree (the temp-dir fixture
    /// shape `corpus_local.rs`/`nsrt.rs` tests use — never the corpus, which
    /// worktrees do not have). Runs the full measure path under `--no-php`
    /// (the test environment need not ship a PHP binary) and holds the
    /// determinism oracle plus cross-invocation hash stability — the property
    /// the blessed baseline relies on.
    #[test]
    fn smoke_a_tiny_fixture_tree_measures_deterministically() {
        let dir = std::env::temp_dir().join(format!("steins-perf-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir fixture tree");
        std::fs::write(
            dir.join("src/add.php"),
            "<?php\nfunction add(int $x, int $y): int {\n    return $x + $y;\n}\n",
        )
        .expect("write add.php");
        std::fs::write(
            dir.join("src/use_add.php"),
            "<?php\nfunction use_add(): int {\n    return add(1, 2);\n}\n",
        )
        .expect("write use_add.php");
        std::fs::write(
            dir.join("src/broken.php"),
            "<?php\nfunction broken(): int {\n    return $undef;\n}\n",
        )
        .expect("write broken.php");

        let m = measure_target(&dir, 2, Posture::NoPhp).expect("measure the fixture tree");
        assert_eq!(m.files, 3);
        assert!(matches!(m.determinism, Determinism::Ok), "the oracle must hold on a fixed tree");
        assert!(m.findings >= 1, "the unbound read in broken.php should be found");
        assert_eq!(m.timings.len(), 2);

        // A second, independent invocation reproduces the hash — what makes a
        // blessed baseline comparable at all.
        let again = measure_target(&dir, 2, Posture::NoPhp).expect("measure again");
        assert_eq!(again.findings_sha256, m.findings_sha256);
        assert_eq!(again.findings, m.findings);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
