//! The `steins` binary (ADR-0020). `check` walks `.php` files, runs the salsa
//! pipeline, prints proof-layer diagnostics, exits 1 if any finding was
//! reported. `annotate` reprints a file with a right-margin *proven*-fact
//! column. `transform`, `effect-diff`, `doctor`, `version`, `license` complete it.

// Output seam (issue #44), declared first: `outln!`/`out!`/`errln!` are
// textually-scoped macros, so every module using them must come after this.
#[macro_use]
mod out;

mod baseline;
mod doctor;
mod effect_baseline;
mod mcp;
mod render;
mod sarif;
mod sha256;

// Shared with the wasm playground (no-second-relation discipline for surface selection).
pub(crate) use steins_infer::profile;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{
    EffectsPolicy, PluginFacts, Project, ProjectLayout, Resolve, SourceFile, SteinsDatabase,
    composer, parse as parse_tree, project_index,
};
use steins_edit::{
    ByteSpan, Edit, EditPlan, LoopToArrayMapOptions, PartitionMap, TransformReport, VouchSet,
    plan_effects_envelope, plan_loop_to_array_map, plan_phpdoc_honesty, plan_phpdoc_to_native,
    plan_throws_envelope, unified_diff,
};
use steins_infer::{
    Diagnostic, EffectSummary, FinalKeyword, LineFact, NoFold, SOUND_SUBSET_NOTICE, SidecarFolder,
    annotate_file, annotate_project, apply_inline_ignores, check_project,
    check_project_with_postures, effect_summaries_file, effect_summaries_project,
};
use steins_syntax::SourceTree;

/// The `text|json` pair `annotate`, `transform` and `effect-diff` share.
/// `check` uses its own [`render::CheckFormat`] instead (CI renderings, ADR-0054).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

/// Headroom for the worker thread every subcommand runs on (issue #246).
/// CST-walking recursion costs one stack frame per nesting level; this
/// workspace overflowed the ~8 MiB OS default around 520 levels in debug,
/// ~2,700 in release — under phpstan-src's 1,000-level chain-walk fixture. A
/// stack-overflow abort loses the whole run (ADR-0009), so #246 chose
/// headroom over a depth cutoff. 256 MiB matches the nsrt harness's
/// `WORKER_STACK_SIZE` (`xtask/src/nsrt.rs`), costing nothing when unused.
/// Sizes the binary only — steins-syntax and the wasm playground aren't covered.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Output seam (issue #44): `out::finish` flushes stdout, maps write failure
    // to exit code. Worker thread sized per `WORKER_STACK_SIZE` (see its doc).
    let code = std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || dispatch(&args))
        .expect("failed to spawn the steins worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    out::finish(code)
}

/// Dispatch on the subcommand. Split out of `main` so there is exactly one exit
/// path through [`out::finish`] rather than one per `return`.
fn dispatch(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("check") => run_check(&args[1..]),
        Some("annotate") => run_annotate(&args[1..]),
        Some("transform") => run_transform(&args[1..]),
        Some("effect-diff") => run_effect_diff(&args[1..]),
        Some("doctor") => doctor::run_doctor(&args[1..]),
        Some("mcp") => mcp::run_mcp(&args[1..]),
        Some("version" | "--version" | "-v") => print_version(),
        Some("license" | "licenses") => print_license(),
        Some(other) => {
            errln!(
                "steins: unknown command `{other}` (available: check, annotate, transform, effect-diff, doctor, mcp, version, license)"
            );
            ExitCode::from(2)
        }
        None => {
            errln!(
                "usage: steins check [--format text|json|github|sarif] [--profile <name>] [--no-php] [--no-tolerated-effects] [--vendor-diagnostics] [--fix] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>"
            );
            errln!("       steins annotate [--no-php] [--format text|json] <file.php>");
            errln!(
                "       steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map> [--apply] [--asserted-subjects] [--format text|json] <paths...>"
            );
            errln!(
                "       steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json] <paths...>"
            );
            errln!("       steins doctor [--no-php] [--baseline <path>] [--format text|json] [path]");
            errln!("       steins mcp");
            errln!("       steins version | -v | --version");
            errln!("       steins license");
            ExitCode::from(2)
        }
    }
}

/// Steins' own terms, embedded (#43): Homebrew and `cargo install --git` both
/// produce a binary with no `LICENSE` file, and Apache-2.0 §4(a) requires one.
const LICENSE_APACHE: &str = include_str!("../../../LICENSE");

/// Bundled dependencies' notices (their MIT/BSD/ISC terms require it).
/// Generated by `cargo xtask licenses`; a CI drift guard keeps it current.
const THIRD_PARTY_LICENSES: &str = include_str!("../../../THIRD-PARTY-LICENSES.md");

/// PHPStan's own MIT notice — not a Rust dependency, so the generator can't
/// discover it; embedded by hand to credit Steins' direct model (README).
const LICENSE_PHPSTAN: &str = include_str!("../../../LICENSE-PHPSTAN");

/// The `version` banner. Build date/revision come from `build.rs`, degrading
/// to `unknown` outside a git working tree.
fn version_text() -> String {
    format!(
        concat!(
            "steins {} ({} revision {}) - {}\n",
            // `authors` carries only the individual; README names `TypedDuck`.
            "Copyright (c) TypedDuck, {}\n",
            "    Built with the help of many third-party libraries.\n",
            "    Run `steins license` to see all dependencies and their licenses.",
        ),
        env!("CARGO_PKG_VERSION"),
        env!("STEINS_BUILD_DATE"),
        env!("STEINS_GIT_REV"),
        env!("CARGO_PKG_REPOSITORY"),
        env!("CARGO_PKG_AUTHORS"),
    )
}

/// The `license` output: Steins' Apache-2.0 terms, PHPStan's MIT notice, then
/// every dependency notice. Apache-2.0 appears twice deliberately (standalone
/// `THIRD-PARTY-LICENSES.md` ships without `LICENSE`, Homebrew).
fn license_text() -> String {
    format!(
        "steins {} — open source licenses\n{}\n\n\
         Steins is licensed under the Apache License 2.0:\n\n\
         {}\n\n\
         Steins draws on a number of PHP type checkers, but PHPStan is its direct \
         model, and many of its rules are borrowed straight from PHPStan. PHPStan is \
         licensed under the MIT License:\n\n\
         {}\n\n{THIRD_PARTY_LICENSES}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_REPOSITORY"),
        LICENSE_APACHE.trim_start_matches('\n'),
        LICENSE_PHPSTAN.trim_start_matches('\n'),
    )
}

/// `version` / `-v` / `--version`.
fn print_version() -> ExitCode {
    outln!("{}", version_text());
    ExitCode::SUCCESS
}

/// `license` / `licenses`. Thousands of lines, so an interactive terminal
/// pages it; piped/redirected output goes straight to stdout (EPIPE-safe
/// seam, see [`mod@out`]).
fn print_license() -> ExitCode {
    let text = license_text();
    let pager = std::env::var("PAGER").ok();
    if should_page(out::stdout_is_terminal(), pager.as_deref()) {
        // `should_page` confirmed `pager` is non-blank.
        return page_through(pager.as_deref().expect("should_page confirmed a pager"), &text);
    }
    out!("{text}");
    ExitCode::SUCCESS
}

/// Whether to page: an interactive terminal AND a non-blank `$PAGER`. A pure
/// function so the decision (not the untestable terminal/process) is covered
/// by `tests::pager_policy`.
fn should_page(stdout_is_terminal: bool, pager: Option<&str>) -> bool {
    stdout_is_terminal && pager.is_some_and(|p| !p.trim().is_empty())
}

/// Spawn `pager` (e.g. `less -R`) via `sh -c` and write `text` to its stdin.
/// If it cannot even be spawned (a typo'd `$PAGER`), print `text` directly
/// instead of losing the output.
fn page_through(pager: &str, text: &str) -> ExitCode {
    let mut child = match std::process::Command::new("sh")
        .arg("-c")
        .arg(pager)
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            errln!("steins: cannot start pager `{pager}` ({e}); printing directly");
            out!("{text}");
            return ExitCode::SUCCESS;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A pager quitting early (`q`) closes stdin first, same EPIPE policy as `out`.
        let _ = stdin.write_all(text.as_bytes());
    }
    match child.wait() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            errln!("steins: pager `{pager}` exited with {status}");
            ExitCode::FAILURE
        }
        Err(e) => {
            errln!("steins: pager `{pager}` failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_check(args: &[String]) -> ExitCode {
    // `None` until `--format` names one: absence is what auto-detection reads
    // (ADR-0054 §6), so a default here would defeat GitHub Actions detection.
    let mut format: Option<render::CheckFormat> = None;
    let mut no_php = false;
    let mut no_tolerated_effects = false;
    let mut fix_requested = false;
    let mut set_baseline = false;
    let mut ignore_baseline = false;
    let mut vendor_diagnostics = false;
    let mut baseline_path: Option<String> = None;
    let mut profile_flag: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-php" => {
                no_php = true;
                i += 1;
            }
            // ADR-0084 §1 audit switch: empties tolerance; attribution table unaffected.
            "--no-tolerated-effects" => {
                no_tolerated_effects = true;
                i += 1;
            }
            "--fix" => {
                fix_requested = true;
                i += 1;
            }
            "--vendor-diagnostics" => {
                vendor_diagnostics = true;
                i += 1;
            }
            "--profile" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --profile requires a name argument");
                    return ExitCode::from(2);
                };
                profile_flag = Some(value.clone());
                i += 2;
            }
            "--set-baseline" => {
                set_baseline = true;
                i += 1;
            }
            "--ignore-baseline" => {
                ignore_baseline = true;
                i += 1;
            }
            "--baseline" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --baseline requires a path argument");
                    return ExitCode::from(2);
                };
                baseline_path = Some(value.clone());
                i += 2;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --format requires an argument (text|json|github|sarif)");
                    return ExitCode::from(2);
                };
                let Some(parsed) = render::CheckFormat::parse(value) else {
                    errln!("steins: unknown format `{value}` (text|json|github|sarif)");
                    return ExitCode::from(2);
                };
                format = Some(parsed);
                i += 2;
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    // Auto-detection (ADR-0054 §6): explicit `--format` wins, else env may
    // name a consumer (GitHub Actions) — only the spelling changes.
    let format = format.unwrap_or_else(render::detect_from_env);

    if paths.is_empty() {
        errln!("steins: no paths given");
        return ExitCode::from(2);
    }
    // `--set-baseline` and `--fix` cannot combine (ambiguous which state the
    // baseline would capture) — usage error.
    if fix_requested && set_baseline {
        errln!("steins: --fix cannot be combined with --set-baseline");
        return ExitCode::from(2);
    }
    if let Err(code) = reject_missing_paths(&paths) {
        return code;
    }

    let files = collect_files(&paths);

    // Coverage posture (ADR-0004): `--no-php` runs the sound subset (notice up
    // front); otherwise folds via a lazily-spawned sidecar.
    if no_php {
        errln!("{SOUND_SUBSET_NOTICE}");
    }

    // Parse `./steins.toml` once, up front (ADR-0050 §7/ADR-0052 §5 N2): a
    // malformed file (incl. an unknown `[runtime]` key) is exit 2, never warn-and-proceed.
    let config = match read_steins_config() {
        Ok(c) => c,
        Err(e) => {
            errln!("steins: {e}");
            return ExitCode::from(2);
        }
    };
    let (check_cfg, profile_tbl, runtime_cfg, plugin_allow, effects_cfg) = match config {
        Some(c) => (c.check, c.profile, c.runtime, allow_list(c.plugins), c.effects),
        None => (None, None, None, None, None),
    };
    let effects_policy = effects_from_config(effects_cfg, no_tolerated_effects);

    // Active display surface (ADR-0050 §5), resolved before analysis (config
    // error fails fast, exit 2). Precedence: `--profile` > `[check] profile` > `default`.
    let (config_profile, profile_configs) = profiles_from_config(check_cfg, profile_tbl);
    let selected = profile_flag.as_deref().or(config_profile.as_deref());
    let surface = match profile_configs.resolve(selected) {
        Ok(s) => s,
        Err(e) => {
            errln!("steins: {e}");
            return ExitCode::from(2);
        }
    };

    // One folder for the whole run: owns the sidecar + fold memo, so repeated
    // calls across files never re-spawn or re-fold.
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

    // Project mode (ADR-0009/0015): all `.php` files form ONE project (one salsa
    // DB) so cross-file calls, class chains, effects resolve.
    let loaded = load_project(&files, &paths, plugin_allow.as_deref(), effects_policy);
    let (db, project, texts) = (&loaded.db, loaded.project, &loaded.texts);
    // Target PHP range (issue #28) gates the folder's absence family and curated facts.
    folder.set_php_target(loaded.layout.php_target().cloned());
    // `[runtime]` pseudo-constants (ADR-0037 §2): an unknown value on a known
    // key warns and keeps the safe default (a parse error already exited 2).
    let (postures, runtime_warnings) = runtime_from_config(runtime_cfg);
    for w in &runtime_warnings {
        errln!("steins: {w}");
    }
    let findings: Vec<Diagnostic> = check_project_with_postures(
        db,
        project,
        &mut folder,
        postures.warning_handler_abort,
        postures.final_keyword,
    );

    // Suppression channels, ADR-0050 §6 order (vendor → surface → policy →
    // inline). Baseline stays here: it's the CI ratchet, this command's own argument.
    let (inline, vendor_suppressed) =
        suppression_pipeline(&loaded, findings, &surface, vendor_diagnostics);

    // Baseline file (ADR-0022): `--set-baseline`/`--baseline` name one
    // explicitly, else the default auto-loads unless `--ignore-baseline`.
    let baseline_file: Option<PathBuf> = if set_baseline {
        Some(PathBuf::from(baseline_path.as_deref().unwrap_or(baseline::DEFAULT_FILE)))
    } else if ignore_baseline {
        None
    } else if let Some(p) = &baseline_path {
        Some(PathBuf::from(p))
    } else if Path::new(baseline::DEFAULT_FILE).exists() {
        Some(PathBuf::from(baseline::DEFAULT_FILE))
    } else {
        None
    };

    if set_baseline {
        let file = baseline_file.expect("set-baseline names a file");
        return write_baseline(&file, &inline.kept, texts, &surface);
    }

    // Baseline channel: partitions survivors into baselined (excluded) and
    // reported; no file → all report. Staleness is surface-aware (ADR-0050 §8).
    let (reported, baselined, stale, surface_notice) = match &baseline_file {
        Some(file) => match std::fs::read_to_string(file) {
            Ok(text) => match_baseline(file, &text, inline.kept, texts, &surface),
            Err(_) => (inline.kept, 0, 0, None),
        },
        None => (inline.kept, 0, 0, None),
    };

    // Displayed = survivors + meta-diagnostics (exempt from both channels), sorted.
    let mut displayed = reported;
    displayed.extend(inline.meta);
    displayed.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column, a.id).cmp(&(b.path.as_str(), b.line, b.column, b.id))
    });

    // `check --fix` (ADR-0010): applies fix payloads under ADR-0034's
    // transformed-or-refused discipline. Without the flag, `None` — unchanged.
    let fix_run = fix_requested.then(|| apply_fixes(db, project, &displayed, texts));

    // A fixed finding leaves both display and exit; the plan is atomic, so
    // payload presence is the partition key.
    let (displayed, fixed): (Vec<Diagnostic>, Vec<Diagnostic>) = match &fix_run {
        Some(run) if run.applied => displayed.into_iter().partition(|d| d.fix.is_none()),
        _ => (displayed, Vec::new()),
    };

    // Render seam (ADR-0054 C1): ONE report handed to whichever format was
    // selected — format invariance (§1) is a property of this shape.
    let report = render::CheckReport {
        displayed: &displayed,
        fixed: &fixed,
        fix_run: fix_run.as_ref(),
        surface: &surface,
        accounting: render::Accounting {
            vendor_suppressed,
            suppressed: inline.suppressed,
            baselined,
            stale,
            surface_notice: surface_notice.as_deref(),
        },
        texts,
    };
    out!("{}", render::render(&report, format));

    // Fix-run accounting, after the report like other maintenance confirmations.
    if let Some(run) = &fix_run {
        if run.applied {
            errln!(
                "steins: fixed {} finding(s) ({} file(s) written)",
                fixed.len(),
                run.files_written
            );
        } else if let Some(r) = &run.refusal {
            errln!("steins: fix refused ({}): {}", r.reason, r.detail);
        } else {
            errln!("steins: no fixable findings");
        }
    }

    // Exit level (ADR-0050 §7): 1 iff any fail-level finding is displayed, else
    // 0 (warn-only); fixed findings are already gone from `displayed`.
    let any_fail = displayed.iter().any(|d| surface.level(d.id) == profile::Level::Fail);
    if any_fail { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// Outcome of a `check --fix` run. `applied` is true iff edits were written; a
/// refusal (four named reasons) leaves findings as a plain run reports them.
struct FixRun {
    applied: bool,
    files_written: usize,
    refusal: Option<FixRefusal>,
}

/// A named fix refusal (ADR-0034 Refusal discipline): machine `reason`, human
/// `detail`, and the diagnostics the edits would have surfaced.
struct FixRefusal {
    reason: &'static str,
    detail: String,
    new_diagnostics: Vec<Diagnostic>,
}

/// Apply fix payloads (ADR-0010): pour every edit into ONE atomic
/// [`EditPlan`], run the post-check (ADR-0034 point 3a), then write.
fn apply_fixes(
    db: &SteinsDatabase,
    project: Project,
    displayed: &[Diagnostic],
    texts: &HashMap<String, String>,
) -> FixRun {
    let none = FixRun { applied: false, files_written: 0, refusal: None };
    let fixes: Vec<&steins_infer::Fix> = displayed.iter().filter_map(|d| d.fix.as_ref()).collect();
    if fixes.is_empty() {
        return none;
    }
    let mut plan = EditPlan::new();
    for fix in &fixes {
        for e in &fix.edits {
            let edit = Edit {
                path: e.path.clone(),
                span: ByteSpan::new(e.start, e.end),
                replacement: e.replacement.clone(),
            };
            // Findings may share an edit; dedupe rather than collide as overlaps.
            if plan.edits.contains(&edit) {
                continue;
            }
            if let Err(err) = plan.add_edit(edit) {
                // Overlapping edits can't be one atomic transaction; refuse rather than guess.
                return FixRun {
                    applied: false,
                    files_written: 0,
                    refusal: Some(FixRefusal {
                        reason: "overlapping-fix-edits",
                        detail: format!("cannot combine this run's fixes into one plan: {err}"),
                        new_diagnostics: Vec::new(),
                    }),
                };
            }
        }
    }

    // Post-check gate (ADR-0034 point 3a): refuses the write if any id's count
    // rises. Broad surface — a fix-it must not move the contract layer.
    let postcheck = post_check(db, project, &plan, texts, PostCheckSurface::Everything);
    if !postcheck.ok {
        let n = postcheck.new_diagnostics.len();
        return FixRun {
            applied: false,
            files_written: 0,
            refusal: Some(FixRefusal {
                reason: "postcheck-new-diagnostics",
                detail: format!("applying the fixes would surface {n} new diagnostic(s)"),
                new_diagnostics: postcheck.new_diagnostics,
            }),
        };
    }

    let mut written = 0usize;
    for path in plan.edited_paths() {
        // Guard, not a reachable path: `texts` holds every analyzed file. If
        // this invariant broke, skipping would silently report `applied`.
        let Some(original) = texts.get(path) else {
            return FixRun {
                applied: false,
                files_written: written,
                refusal: Some(FixRefusal {
                    reason: "fix-target-unread",
                    detail: format!(
                        "no analyzed source text for {path} ({written} file(s) already written)"
                    ),
                    new_diagnostics: Vec::new(),
                }),
            };
        };
        let updated = plan.apply_file(path, original);
        if let Err(e) = std::fs::write(path, &updated) {
            return FixRun {
                applied: false,
                files_written: written,
                refusal: Some(FixRefusal {
                    reason: "write-failed",
                    detail: format!("cannot write {path}: {e} ({written} file(s) already written)"),
                    new_diagnostics: Vec::new(),
                }),
            };
        }
        written += 1;
    }
    FixRun { applied: true, files_written: written, refusal: None }
}

/// The `[[policy]]` scoped enable/disable stage (ADR-0050 §6): currently an
/// identity, keeping the vendor→surface→policy→inline→baseline order real.
fn apply_policy_stage(findings: Vec<Diagnostic>) -> Vec<Diagnostic> {
    findings
}

/// Write a baseline file from inline-surviving findings (ADR-0022
/// `--set-baseline`); never affects exit code. Header records capture surface (ADR-0050 §8).
fn write_baseline(
    file: &Path,
    findings: &[Diagnostic],
    texts: &HashMap<String, String>,
    surface: &profile::Surface,
) -> ExitCode {
    let dir = baseline::base_dir(file);
    let entries: Vec<baseline::Entry> = findings
        .iter()
        // Debug lane (ADR-0053 §4/§8) must NEVER be captured — else a committed
        // dump baselines and later reports suppressed at exit 0 (issue #108).
        .filter(|d| !matches!(steins_infer::layer(d.id), Some(steins_infer::Layer::Debug)))
        .map(|d| {
            let rel = baseline::relativize(&dir, &d.path);
            let hash = texts
                .get(&d.path)
                .map_or_else(String::new, |t| baseline::entry_hash(d.id, &rel, t, d.line));
            // Capture rung (ADR-0062 A-G10): `None` at `default` writes pre-S6 bytes.
            baseline::Entry {
                id: d.id.to_owned(),
                path: rel,
                hash,
                surface: baseline::Entry::tag_for(surface.rung()),
            }
        })
        .collect();
    let n = entries.len();
    let capture = baseline::CaptureSurface { profile: surface.name.clone(), ids: surface.surface_ids() };
    match std::fs::write(file, baseline::render(entries, &capture)) {
        Ok(()) => {
            errln!(
                "steins: wrote {n} baseline entries to {} (profile `{}`)",
                file.display(),
                surface.name
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            errln!("steins: cannot write baseline {}: {e}", file.display());
            ExitCode::from(2)
        }
    }
}

/// Match inline-surviving `findings` against a baseline's entries. Returns
/// `(reported, baselined, stale, surface_notice)`; surface-aware (ADR-0050 §8).
fn match_baseline(
    file: &Path,
    text: &str,
    findings: Vec<Diagnostic>,
    texts: &HashMap<String, String>,
    surface: &profile::Surface,
) -> (Vec<Diagnostic>, usize, usize, Option<String>) {
    let entries = baseline::parse(text);
    let dir = baseline::base_dir(file);
    let mut matcher = baseline::Matcher::new(&entries);
    let mut reported = Vec::new();
    let mut baselined = 0usize;
    for d in findings {
        // Debug lane exempt on MATCH too (ADR-0053 §4/§8), symmetric with write_baseline.
        if matches!(steins_infer::layer(d.id), Some(steins_infer::Layer::Debug)) {
            reported.push(d);
            continue;
        }
        let rel = baseline::relativize(&dir, &d.path);
        let hash = texts
            .get(&d.path)
            .map_or_else(String::new, |t| baseline::entry_hash(d.id, &rel, t, d.line));
        if matcher.take(d.id, &rel, &hash) {
            baselined += 1;
        } else {
            reported.push(d);
        }
    }
    // Debug carve-out (§8): a leftover debug entry surfaces stale on EVERY run,
    // ignoring `captured` (#108), since `surfaces_id` excludes debug ids.
    let stale = matcher.stale_count_within(|id, captured| {
        if matches!(steins_infer::layer(id), Some(steins_infer::Layer::Debug)) {
            true
        } else {
            captured <= surface.rung() && surface.surfaces_id(id)
        }
    });

    // Drowns-loudly notice (ADR-0050 §8): ids the surface admits the header didn't.
    let surface_notice = baseline::parse_header(text).and_then(|captured| {
        let captured_ids: std::collections::HashSet<&str> =
            captured.ids.iter().map(String::as_str).collect();
        let extra = surface
            .surface_ids()
            .into_iter()
            .filter(|id| !captured_ids.contains(id.as_str()))
            .count();
        (extra > 0).then(|| {
            format!(
                "active profile `{}` surfaces {extra} id(s) the baseline (captured under `{}`) did not — \
                 those findings are unbaselined (rerun --set-baseline to capture them)",
                surface.name, captured.profile
            )
        })
    });

    (reported, baselined, stale, surface_notice)
}

/// `steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map>
/// [--apply] [--asserted-subjects] [--format text|json] <paths...>` (ADR-0020/0034).
/// Dry-run by default: diff + refusal report + post-check (ADR-0034 point 3a,
/// zero new diagnostics; see [`PostCheckSurface`]). `--apply` writes only
/// after post-check passes. Exit 2 usage error, 1 post-check fail, 0 else.
fn run_transform(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut apply = false;
    let mut asserted_subjects = false;
    let mut subcommand: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut config_path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--apply" => {
                apply = true;
                i += 1;
            }
            "--asserted-subjects" => {
                asserted_subjects = true;
                i += 1;
            }
            "--config" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --config requires a path argument");
                    return ExitCode::from(2);
                };
                config_path = Some(value.clone());
                i += 2;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                match value.as_str() {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    other => {
                        errln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other if subcommand.is_none() && !other.starts_with('-') => {
                subcommand = Some(other.to_owned());
                i += 1;
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    // Select the transform by subcommand; planner/verb/surface all live on
    // [`TransformKind`], shared by the CLI and MCP surface.
    let kind = match subcommand.as_deref() {
        Some(name) => match TransformKind::from_id(name) {
            Some(k) => k,
            None => {
                errln!(
                    "steins: unknown transform `{name}` (available: phpdoc-to-native, phpdoc-honesty, throws-envelope, effects-envelope, loop-to-array-map)"
                );
                return ExitCode::from(2);
            }
        },
        None => {
            errln!(
                "steins: transform requires a name (usage: steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map> [--apply] [--asserted-subjects] [--config steins.toml] [--format text|json] <paths...>)"
            );
            return ExitCode::from(2);
        }
    };
    // Opt-in belongs to exactly one transform (ADR-0076 issue-#175); on any
    // other it's a usage error, not a silent no-op.
    if asserted_subjects && !kind.supports_asserted_subjects() {
        errln!("steins: --asserted-subjects applies only to loop-to-array-map");
        return ExitCode::from(2);
    }
    if paths.is_empty() {
        errln!("steins: no paths given");
        return ExitCode::from(2);
    }
    if let Err(code) = reject_missing_paths(&paths) {
        return code;
    }

    let run = match plan_transform_run(kind, &paths, config_path.as_deref(), asserted_subjects) {
        Ok(run) => run,
        Err(e) => {
            errln!("steins: {e}");
            return ExitCode::from(2);
        }
    };
    for notice in &run.notices {
        errln!("steins: {notice}");
    }
    let TransformRun { report, postcheck, texts, .. } = &run;

    match format {
        Format::Json => print_transform_json(report, postcheck, apply && postcheck.ok),
        Format::Text => print_transform_text(report, postcheck, texts, kind.action()),
    }

    if !postcheck.ok {
        if apply {
            errln!(
                "steins: post-check found {} new diagnostic(s); refusing to write (ADR-0034)",
                postcheck.new_diagnostics.len()
            );
        }
        return ExitCode::FAILURE;
    }

    if apply {
        let mut written = 0usize;
        for path in report.plan.edited_paths() {
            let Some(original) = texts.get(path) else { continue };
            let updated = report.plan.apply_file(path, original);
            if let Err(e) = std::fs::write(path, &updated) {
                errln!("steins: cannot write {path}: {e}");
                return ExitCode::FAILURE;
            }
            written += 1;
        }
        for nf in &report.plan.new_files {
            if let Err(e) = std::fs::write(&nf.path, &nf.contents) {
                errln!("steins: cannot create {}: {e}", nf.path);
                return ExitCode::FAILURE;
            }
            written += 1;
        }
        errln!("steins: applied {written} file edit(s)");
    }

    ExitCode::SUCCESS
}

/// Which transform a run drives (ADR-0034). Shared by the command line and MCP
/// surface (issue #117) so both agree on what `throws-envelope` means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TransformKind {
    Promote,
    Honesty,
    ThrowsEnvelope,
    EffectsEnvelope,
    LoopToArrayMap,
}

impl TransformKind {
    /// Every transform, in the order the usage line lists them.
    const ALL: [TransformKind; 5] = [
        TransformKind::Promote,
        TransformKind::Honesty,
        TransformKind::ThrowsEnvelope,
        TransformKind::EffectsEnvelope,
        TransformKind::LoopToArrayMap,
    ];

    /// The stable command id: the subcommand word and MCP `plan_transform` argument.
    fn id(self) -> &'static str {
        match self {
            TransformKind::Promote => "phpdoc-to-native",
            TransformKind::Honesty => "phpdoc-honesty",
            TransformKind::ThrowsEnvelope => "throws-envelope",
            TransformKind::EffectsEnvelope => "effects-envelope",
            TransformKind::LoopToArrayMap => "loop-to-array-map",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        TransformKind::ALL.into_iter().find(|k| k.id() == id)
    }

    /// The verb the completeness-oracle summary uses for an edited site.
    fn action(self) -> &'static str {
        match self {
            TransformKind::Promote => "promoted",
            TransformKind::Honesty | TransformKind::LoopToArrayMap => "rewritten",
            TransformKind::ThrowsEnvelope | TransformKind::EffectsEnvelope => "seeded",
        }
    }

    /// One sentence describing what the transform rewrites, for an agent choosing one.
    fn summary(self) -> &'static str {
        match self {
            TransformKind::Promote => {
                "promote a `@param`/`@return` tag to a native declaration where call-site propagation proves every project call site flows the type"
            }
            TransformKind::Honesty => {
                "rewrite a docblock type that the engine proves wrong to the type it proves"
            }
            TransformKind::ThrowsEnvelope => {
                "seed `@throws` tags from proven escapes; a declaration whose escapes are only `Maybe` is refused"
            }
            TransformKind::EffectsEnvelope => {
                "seed the interop envelopes of ADR-0082 from proven effects: `@phpstan-impure <labels>` where inference is exhaustive, `@phpstan-all-methods-pure` where every declared method is provenly pure; a non-exhaustive declaration is refused and no bare or per-declaration pure tag is ever written"
            }
            TransformKind::LoopToArrayMap => {
                "rewrite an append `foreach` to `array_map` where the body is proven to have no effects and no throws"
            }
        }
    }

    /// Whether this transform consumes `--asserted-subjects` (ADR-0076
    /// issue-#175). Exactly one does; both entry points read it from here.
    fn supports_asserted_subjects(self) -> bool {
        matches!(self, TransformKind::LoopToArrayMap)
    }

    /// The surface this transform's post-check is measured against (ADR-0034
    /// point 3a, issue #115), named once so the CLI and MCP agree.
    fn post_check_surface(self) -> PostCheckSurface {
        match self {
            // Docblock/loop rewrites aren't meant to change the contract — a new
            // contract-layer finding is a regression, so both take the broad net.
            TransformKind::Promote | TransformKind::Honesty | TransformKind::LoopToArrayMap => {
                PostCheckSurface::Everything
            }
            // Seeding an envelope IS a contract change (ADR-0082): measuring
            // against the contract layer would let it veto its own success.
            TransformKind::ThrowsEnvelope | TransformKind::EffectsEnvelope => {
                PostCheckSurface::DefaultOnly
            }
        }
    }
}

/// One planned transform run: [`TransformReport`], post-check, file texts.
/// Producing writes nothing; applying is a separate step (ADR-0010) —
/// `transform --apply`/MCP `apply_plan` are the only code reaching disk.
struct TransformRun {
    report: TransformReport,
    postcheck: PostCheck,
    texts: HashMap<String, String>,
    /// Human notices to report on the way out: vouch-file problems and
    /// no-op vouch entries (ADR-0046 §2).
    notices: Vec<String>,
}

/// Plan `kind` over `paths` and run its post-check (ADR-0010's dry-run half),
/// shared by `steins transform` and MCP `plan_transform`. `Err` is a config
/// error (exit 2): overlapping partition path-sets. A vouch typo is a notice.
fn plan_transform_run(
    kind: TransformKind,
    paths: &[String],
    config_path: Option<&str>,
    asserted_subjects: bool,
) -> Result<TransformRun, String> {
    // Vouching valve (ADR-0046 §2): `[transform.vouch]`. A malformed entry warns.
    let (vouches, mut notices) = load_vouches(config_path);

    // Region map (ADR-0047 §7). No section → `None` (single-region identity);
    // an *overlap* in partition path-sets is a hard error (unlike a vouch typo).
    let partitions = load_partitions(config_path)?;

    let files = collect_files(paths);
    let loaded =
        load_project(&files, paths, allow_list_from_disk().as_deref(), effects_policy_from_disk());
    let (db, project) = (&loaded.db, loaded.project);

    // Plan the transform (pure — no writes, no re-check).
    let report = match kind {
        TransformKind::Promote => plan_phpdoc_to_native(db, project, &vouches, partitions.as_ref()),
        TransformKind::Honesty => plan_phpdoc_honesty(db, project, &vouches, partitions.as_ref()),
        // No vouch set: proven escapes are forward facts (ADR-0046 §2 doesn't apply).
        TransformKind::ThrowsEnvelope => plan_throws_envelope(db, project, partitions.as_ref()),
        TransformKind::EffectsEnvelope => plan_effects_envelope(db, project, partitions.as_ref()),
        TransformKind::LoopToArrayMap => plan_loop_to_array_map(
            db,
            project,
            &vouches,
            partitions.as_ref(),
            LoopToArrayMapOptions { asserted_subjects },
        ),
    };

    // A benign/nonexistent vouched site is a no-op worth reporting (ADR-0046 §2).
    if !matches!(kind, TransformKind::ThrowsEnvelope | TransformKind::EffectsEnvelope) {
        for entry in vouches.unused() {
            notices.push(format!("vouched site `{entry}` matched no dynamic-code obstacle (no-op)"));
        }
    }

    // Dual verification (ADR-0034 point 3a): zero NEW diagnostics, both dry-run and `--apply`.
    let postcheck =
        post_check(db, project, &report.plan, &loaded.texts, kind.post_check_surface());

    Ok(TransformRun { report, postcheck, texts: loaded.texts, notices })
}

/// `steins.toml` — `[transform.vouch]` (ADR-0046 §2) and
/// `[transform.partitions]` (ADR-0047 §7). Unknown keys ignored.
#[derive(serde::Deserialize, Default)]
struct SteinsConfig {
    transform: Option<TransformConfig>,
    runtime: Option<RuntimeConfig>,
    /// The `[check]` section (ADR-0050 §5): the repo's default profile selection.
    check: Option<CheckConfig>,
    /// The `[profile.<name>]` table (ADR-0050 §5): user-defined profiles.
    profile: Option<std::collections::BTreeMap<String, ProfileEntryConfig>>,
    /// The `[plugins]` section (ADR-0039/0068): the explicit plugin listing.
    plugins: Option<PluginsConfig>,
    /// The `[paths]` section (issue #181): the no-manifest vendor-dir config
    /// channel.
    paths: Option<PathsConfig>,
    /// The `[doctor]` section (ADR-0054 §14 deferred-with-design, issue #268):
    /// `require`'s named posture-to-failure assertions.
    doctor: Option<DoctorConfig>,
    /// The `[effects]` section (ADR-0084 §1): the tolerated-effects policy and
    /// the attribution table it grips.
    effects: Option<EffectsConfig>,
}

/// The `[effects]` section (ADR-0084 §1) — tolerated-effects policy. NOT a
/// `[profile.*]` field (ADR-0050 §10): this changes which findings exist.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct EffectsConfig {
    /// The labels the envelope judgment discharges — policy.
    #[serde(default)]
    tolerated: Vec<String>,
    /// `[effects.attribution]`: symbol → labels its effects are *for*. Fact,
    /// not policy — inert until `tolerated` names its label.
    #[serde(default)]
    attribution: std::collections::BTreeMap<String, Vec<String>>,
}

/// The `[doctor]` section (ADR-0054 §14, issue #268): `require = [...]` turns
/// posture facts doctor otherwise only reports into its exit 1. An unknown
/// name inside `require` is validated by `doctor::known_assertions`.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DoctorConfig {
    #[serde(default)]
    require: Vec<String>,
}

/// The `[paths]` section (issue #181): config channel for a project with no
/// `composer.json`, consulted only at the vendor floor.
#[derive(serde::Deserialize, Default)]
struct PathsConfig {
    /// Extra vendor directory-name sequences, `/`-separated, matched
    /// whole-component like `vendor` (`vendor_proj/` never matches).
    #[serde(rename = "vendor-dirs", default)]
    vendor_dirs: Vec<String>,
}

/// The `[plugins]` section (ADR-0039/ADR-0068 §2). `allow = [...]`
/// **replaces** `installed.json` discovery, so `allow = []` loads nothing.
/// Listing a plugin also vouches for it, lifting the vendor-root label rule.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PluginsConfig {
    #[serde(default)]
    allow: Vec<String>,
}

/// The `[check]` section (ADR-0050 §5): default profile name; `--profile` beats it.
#[derive(serde::Deserialize, Default)]
struct CheckConfig {
    profile: Option<String>,
}

/// A `[profile.<name>]` entry (ADR-0050 §5): `extends` a base, refines with
/// ADR-0022 prefix id-arrays. Facet tokens error as unknown id patterns (v1).
#[derive(serde::Deserialize, Default)]
struct ProfileEntryConfig {
    extends: Option<String>,
    #[serde(default)]
    enable: Vec<String>,
    #[serde(default)]
    disable: Vec<String>,
    #[serde(default)]
    warn: Vec<String>,
}

/// The `[runtime]` section (ADR-0037 §2): boot-truth pseudo-constants the
/// checker can't observe from source. `deny_unknown_fields` so a typo can't
/// silently keep the safe default. Abolished `zend-assertions` also lands
/// here as an unknown-key error (2026-07-25: `assert()` is a throw-guard).
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RuntimeConfig {
    /// `warning-handler = "abort" | "null"` (ADR-0049 §7): what a proven
    /// `E_WARNING` does at runtime. Default `"abort"` emits proven
    /// warning-grade findings; `"null"` silences them (app tolerates it).
    #[serde(rename = "warning-handler", default)]
    warning_handler: Option<String>,
    /// `final-keyword = "enforced" | "stripped"` (issue #234): default
    /// `"enforced"` is PHP's own rule; `"stripped"` declares a loader that
    /// strips it (e.g. `dg/bypass-finals`), making `FinalClass&MockObject`
    /// real under test. See [`steins_infer::FinalKeyword`].
    #[serde(rename = "final-keyword", default)]
    final_keyword: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct TransformConfig {
    vouch: Option<VouchConfig>,
    partitions: Option<PartitionsConfig>,
}

#[derive(serde::Deserialize, Default)]
struct VouchConfig {
    /// User-vouched dynamic-code sites as `file:line` entries.
    #[serde(default)]
    sites: Vec<String>,
}

/// The `[transform.partitions]` section (ADR-0047 §7): observer globs and the
/// name→glob-list `sets` table. Region assignment is a pure function of these
/// plus the file path ([`PartitionMap`]).
#[derive(serde::Deserialize, Default)]
struct PartitionsConfig {
    /// Observer path-sets (tests, dev-scripts; ADR-0047 §1).
    #[serde(default)]
    observers: Vec<String>,
    /// Partition name → glob list; `BTreeMap` for deterministic iteration.
    #[serde(default)]
    sets: std::collections::BTreeMap<String, Vec<String>>,
}

/// Load the vouching valve from `steins.toml` (ADR-0046 §2): `--config` else
/// `./steins.toml`. Returns [`VouchSet`] plus warnings for a missing explicit
/// `--config`, parse error, or malformed `file:line` entry.
fn load_vouches(config_path: Option<&str>) -> (VouchSet, Vec<String>) {
    let mut warnings = Vec::new();
    let (path, explicit) = match config_path {
        Some(p) => (PathBuf::from(p), true),
        None => (PathBuf::from("steins.toml"), false),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            if explicit {
                warnings.push(format!("--config {}: cannot read; proceeding with no vouches", path.display()));
            }
            return (VouchSet::empty(), warnings);
        }
    };
    let config: SteinsConfig = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("{}: parse error ({e}); proceeding with no vouches", path.display()));
            return (VouchSet::empty(), warnings);
        }
    };
    let sites = config.transform.and_then(|t| t.vouch).map(|v| v.sites).unwrap_or_default();
    let mut entries: Vec<(String, u32)> = Vec::new();
    for raw in sites {
        // `file:line` — split on the LAST colon so Windows drive letters survive.
        match raw.rsplit_once(':').and_then(|(f, l)| {
            let line = l.trim().parse::<u32>().ok()?;
            (!f.trim().is_empty()).then(|| (f.trim().to_owned(), line))
        }) {
            Some(entry) => entries.push(entry),
            None => warnings.push(format!(
                "steins.toml [transform.vouch]: malformed site `{raw}` (want `file:line`); skipped"
            )),
        }
    }
    (VouchSet::from_entries(entries), warnings)
}

/// The tolerated-effects policy from an already-parsed config (ADR-0084 §1).
/// `no_tolerated` (`--no-tolerated-effects`) empties tolerance while leaving
/// attribution in place — attribution is fact, tolerance is policy.
fn effects_from_config(effects: Option<EffectsConfig>, no_tolerated: bool) -> EffectsPolicy {
    let effects = effects.unwrap_or_default();
    let policy = EffectsPolicy::new(effects.tolerated, effects.attribution);
    if no_tolerated { policy.without_tolerance() } else { policy }
}

/// [`effects_from_config`] for surfaces without an already-parsed config, as
/// leniently as [`allow_list_from_disk`] reads the plugin allow-list.
fn effects_policy_from_disk() -> EffectsPolicy {
    effects_from_config(read_steins_config().ok().flatten().and_then(|c| c.effects), false)
}

/// The `[plugins] allow` list: `Some(names)` when present (`[]` deliberately
/// loads nothing), `None` when absent (`installed.json` discovery in charge).
fn allow_list(plugins: Option<PluginsConfig>) -> Option<Vec<String>> {
    plugins.map(|p| p.allow)
}

/// [`allow_list`] for surfaces without an already-parsed config. Lenient: an
/// unparseable `steins.toml` leaves discovery in charge.
fn allow_list_from_disk() -> Option<Vec<String>> {
    allow_list(read_steins_config().ok().flatten().and_then(|c| c.plugins))
}

/// `[paths] vendor-dirs` (issue #181), read as leniently as
/// [`allow_list_from_disk`]: missing/unparseable → no extra dirs.
fn vendor_dirs_from_disk() -> Vec<String> {
    read_steins_config().ok().flatten().and_then(|c| c.paths).map(|p| p.vendor_dirs).unwrap_or_default()
}

/// Load the plugin channel (ADR-0068) for `layout`, reporting every load-time
/// refusal on stderr. Never a diagnostic — the zero-FP banner covers the
/// user's code, not a third party's packaging mistake.
fn load_plugins(layout: &ProjectLayout, allow: Option<&[String]>) -> PluginFacts {
    let facts = PluginFacts::discover(layout, allow);
    for notice in facts.notices() {
        errln!("steins: {notice}");
    }
    facts
}

/// Read and parse `./steins.toml` once for `check`/`doctor` (ADR-0050 §7 /
/// ADR-0052 §5 N2). `Ok(None)`: no file. `Err`: doesn't parse, INCLUDING an
/// unknown `[runtime]` key — a hard error (exit 2), never warn-and-proceed.
/// Transform's `--config` keeps its own lenient loaders (ADR-0046 §2).
fn read_steins_config() -> Result<Option<SteinsConfig>, String> {
    let path = PathBuf::from("steins.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    toml::from_str::<SteinsConfig>(&text)
        .map(Some)
        .map_err(|e| format!("{}: parse error ({e})", path.display()))
}

/// The `[runtime]` pseudo-constants a run analyzes under (ADR-0037 §2). Every
/// slot has a safe default, resolved by [`runtime_from_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimePostures {
    /// `warning-handler` (ADR-0049 §7 amendment): `true` for `"abort"`.
    warning_handler_abort: bool,
    /// `final-keyword` (issue #234), consumed by steins-contract's inhabitance judgment.
    final_keyword: FinalKeyword,
}

/// Derive the `[runtime]` pseudo-constants from the already-parsed config.
/// Returns [`RuntimePostures`] plus warnings for an unrecognized value on a
/// known key. Absence defaults to `"abort"`/`"enforced"`.
fn runtime_from_config(runtime: Option<RuntimeConfig>) -> (RuntimePostures, Vec<String>) {
    let mut warnings = Vec::new();
    let runtime = runtime.unwrap_or_default();
    // Default "abort" (ADR-0049 §7): a proven E_WARNING is a runtime break.
    let warning_handler_abort = match runtime.warning_handler.as_deref() {
        None | Some("abort") => true,
        Some("null") => false,
        Some(other) => {
            warnings.push(format!(
                "steins.toml [runtime] warning-handler: unknown value `{other}` (want \"abort\"|\"null\"); using abort"
            ));
            true
        }
    };
    // Default "enforced" is PHP's own rule (issue #234).
    let final_keyword = match runtime.final_keyword.as_deref() {
        None | Some("enforced") => FinalKeyword::Enforced,
        Some("stripped") => FinalKeyword::Stripped,
        Some(other) => {
            warnings.push(format!(
                "steins.toml [runtime] final-keyword: unknown value `{other}` (want \"enforced\"|\"stripped\"); using enforced"
            ));
            FinalKeyword::Enforced
        }
    };
    (RuntimePostures { warning_handler_abort, final_keyword }, warnings)
}

/// Derive the profile selection and user-profile table from the already-parsed
/// config (ADR-0050 §5). Pure decomposition — parsing already happened in
/// [`read_steins_config`].
fn profiles_from_config(
    check: Option<CheckConfig>,
    profile: Option<std::collections::BTreeMap<String, ProfileEntryConfig>>,
) -> (Option<String>, profile::ProfileConfigs) {
    let selected = check.and_then(|c| c.profile);
    let map = profile
        .unwrap_or_default()
        .into_iter()
        .map(|(name, e)| {
            (
                name,
                profile::UserProfile {
                    extends: e.extends,
                    enable: e.enable,
                    disable: e.disable,
                    warn: e.warn,
                },
            )
        })
        .collect();
    (selected, profile::ProfileConfigs(map))
}

/// Load the region map from `[transform.partitions]` (ADR-0047 §7). `Ok(None)`
/// (single-region identity) when missing/unparseable/no section. `Err` only
/// for overlapping partition path-sets (no defined assignment).
fn load_partitions(config_path: Option<&str>) -> Result<Option<PartitionMap>, String> {
    let path = match config_path {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("steins.toml"),
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    // Parse error already reported by `load_vouches`; proceed as identity.
    let Ok(config) = toml::from_str::<SteinsConfig>(&text) else {
        return Ok(None);
    };
    let Some(partitions) = config.transform.and_then(|t| t.partitions) else {
        return Ok(None);
    };
    // Empty section is still the identity.
    if partitions.sets.is_empty() && partitions.observers.is_empty() {
        return Ok(None);
    }
    PartitionMap::build(partitions.sets, partitions.observers)
        .map(Some)
        .map_err(|e| format!("steins.toml [transform.partitions]: {e}"))
}

/// Outcome of the dual-verification post-check: whether the edit is clean,
/// plus diagnostics whose per-id count increased.
struct PostCheck {
    ok: bool,
    new_diagnostics: Vec<Diagnostic>,
}

/// Which diagnostics a post-check counts as "new" (ADR-0034 point 3a). Not a
/// global default — each call site names its own (issue #115).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PostCheckSurface {
    /// Every diagnostic, vendor-filtered only (ADR-0015): proof + mechanics +
    /// contract layer, for transforms not meant to change what docs promise.
    Everything,
    /// The default display surface alone (proof + mechanics). Reserved for
    /// transforms whose *product is a contract* (seeding `@throws`/effects
    /// envelopes, ADR-0082 §7): the contract layer would let them veto their
    /// own success — deliberate asymmetry with [`PostCheckSurface::Everything`].
    DefaultOnly,
}

impl PostCheckSurface {
    /// The stable name an agent reads (MCP `list_transforms`).
    fn name(self) -> &'static str {
        match self {
            PostCheckSurface::Everything => "everything",
            PostCheckSurface::DefaultOnly => "default-only",
        }
    }

    /// The display surface to filter through, or `None` to count every layer.
    fn display(self) -> Option<profile::Surface> {
        match self {
            PostCheckSurface::Everything => None,
            PostCheckSurface::DefaultOnly => Some(
                profile::ProfileConfigs::default()
                    .resolve(None)
                    .expect("the built-in default profile always resolves"),
            ),
        }
    }
}

/// Re-analyze the edited project, report any diagnostic id whose count
/// increased (ADR-0034 point 3a); vendor-filtered (ADR-0015). Shared by
/// `transform` and `check --fix`. `surface` — see [`PostCheckSurface`].
fn post_check(
    db: &SteinsDatabase,
    project: Project,
    plan: &EditPlan,
    texts: &HashMap<String, String>,
    surface: PostCheckSurface,
) -> PostCheck {
    if plan.is_empty() {
        return PostCheck { ok: true, new_diagnostics: Vec::new() };
    }
    let display = surface.display();
    let before = filtered_diagnostics(
        project.layout(db),
        display.as_ref(),
        check_project(db, project, &mut NoFold),
    );

    // Fresh database avoids salsa mutation subtlety and keeps `before` intact.
    let edb = SteinsDatabase::default();
    let mut einputs: Vec<SourceFile> = Vec::new();
    for (path, original) in texts {
        let updated = plan.apply_file(path, original);
        einputs.push(SourceFile::new(&edb, path.clone(), updated));
    }
    // Must classify vendor the same way, or before/after measures layout, not the edit.
    let eproject =
        Project::new(&edb, einputs, project.layout(db).clone(), project.plugins(db).clone());
    let after = filtered_diagnostics(
        eproject.layout(&edb),
        display.as_ref(),
        check_project(&edb, eproject, &mut NoFold),
    );

    let mut before_counts: HashMap<&str, usize> = HashMap::new();
    for d in &before {
        *before_counts.entry(d.id).or_default() += 1;
    }
    let mut after_counts: HashMap<&str, usize> = HashMap::new();
    for d in &after {
        *after_counts.entry(d.id).or_default() += 1;
    }
    let regressed_ids: Vec<&str> = after_counts
        .iter()
        .filter(|(id, n)| **n > before_counts.get(**id).copied().unwrap_or(0))
        .map(|(id, _)| *id)
        .collect();

    let new_diagnostics: Vec<Diagnostic> =
        after.into_iter().filter(|d| regressed_ids.contains(&d.id)).collect();
    PostCheck { ok: new_diagnostics.is_empty(), new_diagnostics }
}

/// The post-check's view of a diagnostic run: always vendor-filtered (ADR-0015),
/// plus restricted to `surface` when the transform asked. `None` keeps everything.
fn filtered_diagnostics(
    layout: &ProjectLayout,
    surface: Option<&profile::Surface>,
    mut ds: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    ds.retain(|d| !layout.is_vendor(&d.path) && surface.is_none_or(|s| s.is_surfaced(d)));
    ds
}

/// Render the transform dry-run/apply report as text: diff, refusals, oracle
/// summary, post-check verdict.
fn print_transform_text(
    report: &TransformReport,
    postcheck: &PostCheck,
    texts: &HashMap<String, String>,
    action: &str,
) {
    for path in report.plan.edited_paths() {
        if let Some(original) = texts.get(path) {
            let updated = report.plan.apply_file(path, original);
            out!("{}", unified_diff(path, original, &updated, 3));
        }
    }

    // Asserted-subject admissions (ADR-0076 issue-#175): absent when none opted in.
    if !report.asserted_admissions.is_empty() {
        outln!("\nAsserted-subject admissions ({}):", report.asserted_admissions.len());
        for a in &report.asserted_admissions {
            outln!("  {}:{}:{}: {} — {}", a.site.path, a.site.line, a.site.column, a.site.label, a.detail);
        }
    }

    // Dynamic-code obstacles (ADR-0046 §2): site list capped in text (JSON has all).
    const OBSTACLE_SITE_CAP: usize = 5;
    if !report.obstacles.is_empty() {
        outln!("\nDynamic-code obstacles ({}):", report.obstacles.len());
        for ob in &report.obstacles {
            outln!("  [{}] {} — {} site(s):", ob.reason, ob.detail, ob.sites.len());
            for s in ob.sites.iter().take(OBSTACLE_SITE_CAP) {
                outln!("    {}:{}:{}: {}", s.path, s.line, s.column, s.label);
            }
            if ob.sites.len() > OBSTACLE_SITE_CAP {
                outln!("    … and {} more (see --format json)", ob.sites.len() - OBSTACLE_SITE_CAP);
            }
        }
    }

    if !report.refusals.is_empty() {
        outln!("\nRefusals ({}):", report.refusals.len());
        for r in &report.refusals {
            outln!(
                "  {}:{}:{}: {} [{}] — {}",
                r.site.path, r.site.line, r.site.column, r.site.label, r.reason, r.detail
            );
        }
    }

    let o = &report.oracle;
    if o.transformed_asserted > 0 {
        // Lane split (ADR-0076): proven and opted-in yield are different numbers.
        outln!(
            "\n{} enumerated: {} {action} ({} on asserted evidence), {} refused",
            o.enumerated, o.transformed, o.transformed_asserted, o.refused
        );
    } else {
        outln!("\n{} enumerated: {} {action}, {} refused", o.enumerated, o.transformed, o.refused);
    }

    // Vouching downgrade (ADR-0046 §2/ADR-0037): completeness claim is conditional.
    if !report.vouched_exemptions.is_empty() {
        outln!(
            "\nDOWNGRADE: completeness claim is conditional on {} user-vouched dynamic-code exemption(s):",
            report.vouched_exemptions.len()
        );
        for s in &report.vouched_exemptions {
            outln!("    vouched {}:{}:{}: {}", s.path, s.line, s.column, s.label);
        }
    }

    if !postcheck.ok {
        outln!("\nPost-check FAILED — {} new diagnostic(s):", postcheck.new_diagnostics.len());
        for d in &postcheck.new_diagnostics {
            outln!("  {}:{}:{}: [{}] {}", d.path, d.line, d.column, d.id, d.message);
        }
    } else if !report.plan.is_empty() {
        outln!("Post-check OK — no new diagnostics.");
    }
}

/// Render the transform report as JSON: [`TransformReport`] plus post-check
/// verdict and write status. Shared with MCP (issue #117, plan handle added).
fn transform_json(
    report: &TransformReport,
    postcheck: &PostCheck,
    applied: bool,
) -> serde_json::Value {
    let new_ds: Vec<serde_json::Value> = postcheck
        .new_diagnostics
        .iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id, "path": d.path, "line": d.line,
                "column": d.column, "message": d.message,
            })
        })
        .collect();
    // Vouching downgrade (ADR-0046 §2): surfaced as a top-level note whenever
    // any site was vouched.
    let downgrade_note = (!report.vouched_exemptions.is_empty()).then(|| {
        format!(
            "completeness claim is conditional on {} user-vouched dynamic-code exemption(s)",
            report.vouched_exemptions.len()
        )
    });
    serde_json::json!({
        "report": report,
        "postcheck": { "ok": postcheck.ok, "new_diagnostics": new_ds },
        "applied": applied,
        "downgrade_note": downgrade_note,
    })
}

/// Print [`transform_json`] to stdout — the `transform --format json` surface.
fn print_transform_json(report: &TransformReport, postcheck: &PostCheck, applied: bool) {
    match serde_json::to_string_pretty(&transform_json(report, postcheck, applied)) {
        Ok(s) => outln!("{s}"),
        Err(e) => errln!("steins: failed to serialize json: {e}"),
    }
}

/// `steins annotate [--no-php] [--format text|json] <file.php>` — reprint one
/// file with a right-margin column of proven facts (ADR-0020), or (JSON) the
/// same effect summaries (issue #65). Never modifies the file; exit 2 on usage error.
fn run_annotate(args: &[String]) -> ExitCode {
    let mut no_php = false;
    let mut format = Format::Text;
    let mut project_dir: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-php" => {
                no_php = true;
                i += 1;
            }
            "--project" => {
                let Some(dir) = args.get(i + 1) else {
                    errln!("steins: --project requires a directory argument");
                    return ExitCode::from(2);
                };
                project_dir = Some(dir.clone());
                i += 2;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                match value.as_str() {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    other => {
                        errln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other if other.starts_with('-') => {
                errln!("steins: unknown flag `{other}` for annotate");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    let [path] = paths.as_slice() else {
        errln!(
            "steins: annotate takes exactly one file (usage: steins annotate [--no-php] [--format text|json] [--project <dir>] <file.php>)"
        );
        return ExitCode::from(2);
    };
    let path = Path::new(path);
    if path.is_dir() {
        errln!("steins: annotate expects a single file, not a directory: {}", path.display());
        return ExitCode::from(2);
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            errln!("steins: cannot read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };

    // Same coverage posture as `check` (ADR-0004).
    if no_php {
        errln!("{SOUND_SUBSET_NOTICE}");
    }
    let db = SteinsDatabase::default();
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

    // Project context (ADR-0015): `--project` dir, else the file's dir. A bare
    // relative filename has an empty (unopenable) parent — else falls back silently.
    let root = project_dir.map(PathBuf::from).unwrap_or_else(|| {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    let canon_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut project_files = Vec::new();
    collect_php_files(&root, &mut project_files);
    let project_files = dedup_canonical(project_files);

    let mut inputs: Vec<SourceFile> = Vec::new();
    let mut target: Option<SourceFile> = None;
    for fp in &project_files {
        let content = if fp.canonicalize().map(|c| c == canon_target).unwrap_or(false) {
            text.clone()
        } else {
            match std::fs::read(fp) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(_) => continue,
            }
        };
        let input = SourceFile::new(&db, fp.to_string_lossy().into_owned(), content);
        if fp.canonicalize().map(|c| c == canon_target).unwrap_or(false) {
            target = Some(input);
        }
        inputs.push(input);
    }

    // Fall back to a one-file project if the target isn't under root. `--format
    // json` reads the same [`EffectSummary`]s as the text margin (issue #65).
    match format {
        Format::Text => {
            let facts = match target {
                Some(target_file) => {
                    let layout = resolve_layout(&[root.to_string_lossy().into_owned()]);
                    folder.set_php_target(layout.php_target().cloned());
                    let plugins = load_plugins(&layout, allow_list_from_disk().as_deref());
                    let project = Project::builder(inputs, layout, plugins)
                        .effects(effects_policy_from_disk())
                        .new(&db);
                    annotate_project(&db, project, target_file, &mut folder)
                }
                None => {
                    let input =
                        SourceFile::new(&db, path.to_string_lossy().into_owned(), text.clone());
                    annotate_file(&db, input, &mut folder)
                }
            };
            out!("{}", render_annotation(&text, &facts));
        }
        Format::Json => {
            let summaries = match target {
                Some(target_file) => {
                    let layout = resolve_layout(&[root.to_string_lossy().into_owned()]);
                    let plugins = load_plugins(&layout, allow_list_from_disk().as_deref());
                    let project = Project::builder(inputs, layout, plugins)
                        .effects(effects_policy_from_disk())
                        .new(&db);
                    effect_summaries_project(&db, project, target_file)
                }
                None => {
                    let input =
                        SourceFile::new(&db, path.to_string_lossy().into_owned(), text.clone());
                    effect_summaries_file(&db, input)
                }
            };
            print_annotate_json(&summaries);
        }
    }
    ExitCode::SUCCESS
}

/// `annotate --format json`'s document (issue #65): sorted proven labels,
/// sorted declared bounds (ADR-0067), exhaustiveness. `tolerated` (ADR-0084 §4)
/// joins only where discharged, as a subset of `effects`, never a removal.
fn print_annotate_json(summaries: &[EffectSummary]) {
    let functions: Vec<serde_json::Value> = summaries
        .iter()
        .map(|s| {
            let mut entry = serde_json::Map::new();
            entry.insert("name".to_owned(), serde_json::json!(s.symbol));
            entry.insert("line".to_owned(), serde_json::json!(s.line));
            entry.insert("effects".to_owned(), serde_json::json!(s.labels));
            if !s.tolerated.is_empty() {
                entry.insert("tolerated".to_owned(), serde_json::json!(s.tolerated));
            }
            entry.insert("declared".to_owned(), serde_json::json!(s.declared));
            entry.insert("exhaustive".to_owned(), serde_json::json!(s.exhaustive));
            serde_json::Value::Object(entry)
        })
        .collect();
    let doc = serde_json::json!({ "functions": functions });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => outln!("{s}"),
        Err(e) => errln!("steins: failed to serialize json: {e}"),
    }
}

/// `steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json]
/// <paths...>` (issue #69) — capture per-function effect summaries, or diff
/// against a captured past. Own sidecar file, untouched by `check`.
/// Informational: exits 0 even with changes; only usage/file errors exit 2.
fn run_effect_diff(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut set_baseline = false;
    let mut baseline_path: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--set-baseline" => {
                set_baseline = true;
                i += 1;
            }
            "--baseline" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --baseline requires a path argument");
                    return ExitCode::from(2);
                };
                baseline_path = Some(value.clone());
                i += 2;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                match value.as_str() {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    other => {
                        errln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other if other.starts_with('-') => {
                errln!("steins: unknown flag `{other}` for effect-diff");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    if paths.is_empty() {
        errln!("steins: no paths given");
        return ExitCode::from(2);
    }
    if let Err(code) = reject_missing_paths(&paths) {
        return code;
    }

    let mut files = Vec::new();
    for p in &paths {
        collect_php_files(Path::new(p), &mut files);
    }
    let files = dedup_canonical(files);

    // No sidecar, no folder: effect summaries are a pure static fixpoint, so this
    // command never needs `php` and takes no `--no-php`.
    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::new();
    for file_path in &files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                errln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        inputs.push(SourceFile::new(&db, file_path.to_string_lossy().into_owned(), text));
    }
    let layout = resolve_layout(&paths);
    let plugins = load_plugins(&layout, allow_list_from_disk().as_deref());
    let project = Project::new(&db, inputs.clone(), layout.clone(), plugins);

    // Sidecar file, resolved like the diagnostic baseline's: `--baseline` path
    // else the default name. Entry paths relative to its directory (ADR-0022).
    let file = PathBuf::from(baseline_path.as_deref().unwrap_or(effect_baseline::DEFAULT_FILE));
    let dir = baseline::base_dir(&file);
    let current = capture_effect_entries(&db, project, &inputs, &layout, &dir);

    if set_baseline {
        let n = current.len();
        return match std::fs::write(&file, effect_baseline::render(current)) {
            Ok(()) => {
                errln!("steins: wrote {n} effect summaries to {}", file.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                errln!("steins: cannot write effect baseline {}: {e}", file.display());
                ExitCode::from(2)
            }
        };
    }

    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            errln!(
                "steins: cannot read effect baseline {}: {e} (run --set-baseline to capture one)",
                file.display()
            );
            return ExitCode::from(2);
        }
    };
    let doc = match effect_baseline::parse(&text) {
        Ok(d) => d,
        Err(e) => {
            errln!("steins: {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };

    let report = effect_baseline::diff(&doc.functions, &current);
    match format {
        Format::Text => {
            for event in &report.events {
                outln!("{}", event.line());
            }
            if let Some(footer) = effect_baseline::footer(&report) {
                outln!("{footer}");
            }
        }
        Format::Json => print_effect_diff_json(&report),
    }
    ExitCode::SUCCESS
}

/// The current run's effect summaries as baseline entries, one per non-vendor
/// function/method (vendor excluded, ADR-0015).
fn capture_effect_entries(
    db: &SteinsDatabase,
    project: Project,
    inputs: &[SourceFile],
    layout: &ProjectLayout,
    dir: &Path,
) -> Vec<effect_baseline::Entry> {
    let mut entries = Vec::new();
    for &input in inputs {
        let path = input.path(db).to_owned();
        if layout.is_vendor(&path) {
            continue;
        }
        let rel = baseline::relativize(dir, &path);
        // One whole-project fixpoint per file (same cost `annotate` pays per call).
        for s in effect_summaries_project(db, project, input) {
            entries.push(effect_baseline::Entry {
                file: rel.clone(),
                symbol: s.qualified,
                proven: s.labels,
                declared: s.declared,
                exhaustive: s.exhaustive,
            });
        }
    }
    entries
}

/// `effect-diff --format json`: `events` array plus footer counts, for CI.
fn print_effect_diff_json(report: &effect_baseline::Diff) {
    let events: Vec<serde_json::Value> = report
        .events
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.file,
                "symbol": e.symbol,
                "category": e.category.as_str(),
                "label": e.label,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "events": events,
        "compared": report.compared,
        "not_in_baseline": report.not_in_baseline,
        "no_longer_present": report.no_longer_present,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => outln!("{s}"),
        Err(e) => errln!("steins: failed to serialize json: {e}"),
    }
}

/// Render the annotated file: lines with a proven fact padded (longest line,
/// capped at column 88) and given a `//=>` margin; facts join with `; `.
fn render_annotation(text: &str, facts: &[LineFact]) -> String {
    /// The column source lines are padded to before the margin.
    const CAP: usize = 88;
    const PREFIX: &str = "//=> ";

    let lines: Vec<&str> = text.lines().collect();

    // Group fact bodies by line, de-duplicating, order-stable.
    let mut by_line: std::collections::BTreeMap<u32, Vec<String>> = std::collections::BTreeMap::new();
    for f in facts {
        let bodies = by_line.entry(f.line).or_default();
        let body = f.body();
        if !bodies.contains(&body) {
            bodies.push(body);
        }
    }

    let target = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).min(CAP);

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let line_no = i as u32 + 1;
        out.push_str(line);
        if let Some(bodies) = by_line.get(&line_no) {
            let width = line.chars().count();
            // Pad to `target` plus one space, so margins align at `target + 1`.
            let pad = target.saturating_sub(width) + 1;
            for _ in 0..pad {
                out.push(' ');
            }
            out.push_str(PREFIX);
            out.push_str(&bodies.join("; "));
        }
        out.push('\n');
    }
    out
}

/// Reject explicitly-passed paths that name nothing (ADR-0050 §7 amendment):
/// previously `steins check /typo` reported an empty findings set at exit 0,
/// a false all-clear (a renamed directory kept CI green). A path that exists
/// but yields zero `.php` files still stays exit 0.
fn reject_missing_paths(paths: &[String]) -> Result<(), ExitCode> {
    let missing = missing_paths(paths);
    if missing.is_empty() {
        return Ok(());
    }
    for p in &missing {
        errln!("steins: path does not exist: {p}");
    }
    Err(ExitCode::from(2))
}

/// Resolve the run's [`ProjectLayout`] (ADR-0015): each governing
/// `composer.json` names its vendor dir; no manifest → [`ProjectLayout::fallback`].
/// `[paths] vendor-dirs` (issue #181) only supplies a floor a manifest beats.
fn resolve_layout(paths: &[String]) -> ProjectLayout {
    let Ok(cwd) = std::env::current_dir() else { return ProjectLayout::fallback() };
    let roots: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    composer::discover(&roots, &cwd).with_extra_vendor_dirs(vendor_dirs_from_disk())
}

/// The path arguments that name nothing on disk: exit 2 on the command line
/// ([`reject_missing_paths`]), a named tool error over MCP — same rule.
fn missing_paths(paths: &[String]) -> Vec<&String> {
    paths.iter().filter(|p| !Path::new(p.as_str()).exists()).collect()
}

/// The `.php` files `paths` names, deduplicated to real identity (issue #179)
/// — see [`dedup_canonical`] for the dedup key and surviving spelling.
fn collect_files(paths: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for p in paths {
        collect_php_files(Path::new(p), &mut files);
    }
    dedup_canonical(files)
}

/// Deduplicate `files` by real identity, first spelling wins (push order).
/// Issue #179: a symlinked dir made one tree reachable two ways; deduping by
/// path STRING double-declared classes (ADR-0049 existence guard). Dedup KEY
/// is [`Path::canonicalize`]; uncanonicalizable paths key on themselves.
fn dedup_canonical(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(files.len());
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let key = file.canonicalize().unwrap_or_else(|_| file.clone());
        if seen.insert(key) {
            out.push(file);
        }
    }
    // Re-sort for determinism (read_dir order is fs-dependent); selection already done.
    out.sort();
    out
}

/// One analyzed project: salsa database, [`Project`] input, parsed file
/// handles, each file's text keyed by diagnostic path. `db` owns everything
/// salsa ids point into — hand out `&loaded.db`, not moved out.
struct LoadedProject {
    db: SteinsDatabase,
    project: Project,
    inputs: Vec<SourceFile>,
    /// Each file's contents by diagnostic path (ADR-0022 baseline hash, splices).
    texts: HashMap<String, String>,
    layout: ProjectLayout,
}

/// Load `files` as ONE project (ADR-0009/0015): one salsa DB, so cross-file
/// calls resolve. Single door: `check`, `transform`, MCP (issue #117) all come
/// through here. An unreadable file is reported on stderr and left out.
fn load_project(
    files: &[PathBuf],
    paths: &[String],
    allow: Option<&[String]>,
    effects: EffectsPolicy,
) -> LoadedProject {
    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::new();
    let mut texts: HashMap<String, String> = HashMap::new();
    for file_path in files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                errln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        let path = file_path.to_string_lossy().into_owned();
        texts.insert(path.clone(), text.clone());
        inputs.push(SourceFile::new(&db, path, text));
    }
    let layout = resolve_layout(paths);
    // The plugin channel (ADR-0068), read once at the boundary like the layout.
    let plugins = load_plugins(&layout, allow);
    // Tolerated-effects vocabulary judged against this run's registry (ADR-0084 §5).
    for notice in effects.label_notices(plugins.registry()) {
        errln!("steins: {notice}");
    }
    let project =
        Project::builder(inputs.clone(), layout.clone(), plugins).effects(effects).new(&db);
    // Attribution keys checked against the symbol table. Never a diagnostic:
    // a key naming an unvendored class is a stale config line.
    for notice in attribution_notices(&db, project) {
        errln!("steins: {notice}");
    }
    LoadedProject { db, project, inputs, texts, layout }
}

/// `[effects.attribution]` keys naming no symbol (ADR-0084 §5). Tried against
/// all four symbol kinds; for `Class::method` only the class resolves.
fn attribution_notices(db: &SteinsDatabase, project: Project) -> Vec<String> {
    let policy = project.effects(db);
    if policy.is_empty() {
        return Vec::new();
    }
    let index = project_index(db, project);
    let known = |name: &str| {
        !matches!(index.resolve_class(name), Resolve::Absent)
            || !matches!(index.resolve_function(name), Resolve::Absent)
            // Same test the checker uses to decide builtin vs. unresolved userland call.
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

/// Suppression channels, ADR-0050 §6 order: vendor → surface → policy →
/// inline. Baseline deliberately NOT here — a per-invocation argument.
fn suppression_pipeline(
    loaded: &LoadedProject,
    mut findings: Vec<Diagnostic>,
    surface: &profile::Surface,
    vendor_diagnostics: bool,
) -> (steins_infer::InlineOutcome, usize) {
    // Vendor filtering FIRST (ADR-0015): suppressed by default, must not eat a
    // baseline entry. `--vendor-diagnostics` opts back in.
    let mut vendor_suppressed = 0usize;
    if !vendor_diagnostics {
        let before = findings.len();
        findings.retain(|d| !loaded.layout.is_vendor(&d.path));
        vendor_suppressed = before - findings.len();
    }

    // Profile surface (ADR-0050 §6): bare `check` shows proof + mechanics;
    // named profiles opt into contracts. Mechanics ids stay on always (§1).
    findings.retain(|d| surface.is_surfaced(d));

    // Scoped policy, third stage (ADR-0050 §6); currently an identity.
    let findings = apply_policy_stage(findings);

    // Inline `@steins-ignore` next (ADR-0023): suppressed findings skip the baseline.
    let db = &loaded.db;
    let trees: Vec<&SourceTree> = loaded.inputs.iter().map(|&sf| parse_tree(db, sf)).collect();
    let file_pairs: Vec<(String, &SourceTree)> = loaded
        .inputs
        .iter()
        .zip(trees.iter())
        .map(|(&sf, &t)| (sf.path(db).to_owned(), t))
        .collect();
    (apply_inline_ignores(findings, &file_pairs), vendor_suppressed)
}

fn collect_php_files(path: &Path, out: &mut Vec<PathBuf>) {
    collect_php_files_inner(path, out, &mut HashSet::new());
}

/// The walk `collect_php_files` fronts, plus a symlink cycle guard (issue
/// #179): `visited_dirs` resets per top-level call, so it stops loops but is
/// NOT the file-level dedup — [`dedup_canonical`] collapses cross-argument duplicates.
fn collect_php_files_inner(path: &Path, out: &mut Vec<PathBuf>, visited_dirs: &mut HashSet<PathBuf>) {
    if path.is_dir() {
        // Already-entered directory is a symlink cycle: stop. canonicalize()
        // failure is walked uncached; read_dir fails harmlessly if unreadable.
        if let Ok(canon) = path.canonicalize()
            && !visited_dirs.insert(canon)
        {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else { return };
        for entry in entries.flatten() {
            collect_php_files_inner(&entry.path(), out, visited_dirs);
        }
    } else if path.extension().is_some_and(|e| e == "php") {
        out.push(path.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `check --fix` post-check gate refuses a regressing fix by name,
    /// writing nothing (ADR-0034 point 3a); SYNTHETIC since a real dump is
    /// transparent (ADR-0053 §10) and can't trip it.
    #[test]
    fn post_check_gate_refuses_a_regressing_fix() {
        let db = SteinsDatabase::default();
        let src = "<?php\nfunction width(int $w): int { return $w; }\nwidth(5);\n";
        let path = "steins-checkfix-gate-unit.php".to_owned();
        let input = SourceFile::new(&db, path.clone(), src.to_owned());
        let project = Project::new(
            &db,
            vec![input],
            ProjectLayout::fallback(),
            steins_db::PluginFacts::default(),
        );
        let mut texts: HashMap<String, String> = HashMap::new();
        texts.insert(path.clone(), src.to_owned());

        // `width(5);` → `width("abc");`: byte 55..56 holds the `5`.
        let displayed = vec![Diagnostic {
            id: steins_infer::DEBUG_TYPE_ID,
            path: path.clone(),
            line: 3,
            column: 7,
            message: "synthetic fix carrier".to_owned(),
            facet: None,
            fix: Some(steins_infer::Fix {
                title: "synthetic regressing edit",
                edits: vec![steins_infer::FixEdit {
                    path: path.clone(),
                    start: 55,
                    end: 56,
                    replacement: "\"abc\"".to_owned(),
                }],
            }),
        }];

        let run = apply_fixes(&db, project, &displayed, &texts);
        assert!(!run.applied, "a regressing fix must not apply");
        assert_eq!(run.files_written, 0);
        let refusal = run.refusal.expect("the gate names its refusal");
        assert_eq!(refusal.reason, "postcheck-new-diagnostics");
        assert!(
            refusal.new_diagnostics.iter().any(|d| d.id == "type.argument-mismatch"),
            "the would-be diagnostics are attached, got {:?}",
            refusal.new_diagnostics
        );
        // Refusal returns before any write.
        assert!(!Path::new(&path).exists(), "nothing written on refusal");
    }

    /// A fix targeting no analyzed source text is refused by name, not
    /// skipped — skipping would leave `applied` true with the edit undone.
    #[test]
    fn a_fix_whose_target_was_never_read_is_refused_by_name() {
        let db = SteinsDatabase::default();
        let src = "<?php\n$x = 1;\n";
        let read = "steins-checkfix-unread-unit-read.php".to_owned();
        let input = SourceFile::new(&db, read.clone(), src.to_owned());
        let project = Project::new(
            &db,
            vec![input],
            ProjectLayout::fallback(),
            steins_db::PluginFacts::default(),
        );
        let mut texts: HashMap<String, String> = HashMap::new();
        texts.insert(read, src.to_owned());

        // Targets a path the project never read; the guard, not the post-check, stops it.
        let unread = "steins-checkfix-unread-unit-missing.php".to_owned();
        let displayed = vec![Diagnostic {
            id: steins_infer::DEBUG_TYPE_ID,
            path: unread.clone(),
            line: 1,
            column: 1,
            message: "synthetic fix carrier".to_owned(),
            facet: None,
            fix: Some(steins_infer::Fix {
                title: "synthetic edit on an unread file",
                edits: vec![steins_infer::FixEdit {
                    path: unread.clone(),
                    start: 0,
                    end: 1,
                    replacement: String::new(),
                }],
            }),
        }];

        let run = apply_fixes(&db, project, &displayed, &texts);
        assert!(!run.applied, "an unwritable target must not report as applied");
        assert_eq!(run.files_written, 0);
        let refusal = run.refusal.expect("the guard names its refusal");
        assert_eq!(refusal.reason, "fix-target-unread");
        assert!(refusal.detail.contains(&unread), "the detail names the path: {}", refusal.detail);
        assert!(!Path::new(&unread).exists(), "nothing written on refusal");
    }

    /// The case that forced the post-check surface asymmetry (issue #115): a
    /// correct seed vetoes itself against [`PostCheckSurface::Everything`] but
    /// passes [`PostCheckSurface::DefaultOnly`].
    #[test]
    fn the_broad_surface_would_veto_a_legitimate_throws_seed() {
        const LIB: &str = "<?php\nclass P {\n    /**\n     * @throws \\JsonException\n     */\n    public function m(): void {}\n}\nclass C extends P {\n    public function m(): void { throw new \\RuntimeException(\"x\"); }\n}\n";

        let db = SteinsDatabase::default();
        let input = SourceFile::new(&db, "lib.php".to_owned(), LIB.to_owned());
        let project =
            Project::new(&db, vec![input], ProjectLayout::fallback(), PluginFacts::none());
        let report = plan_throws_envelope(&db, project, None);
        assert_eq!(
            report.oracle.transformed, 1,
            "the seed itself must be planned, or the test proves nothing: {:#?}",
            report.refusals
        );
        let texts: HashMap<String, String> =
            [("lib.php".to_owned(), LIB.to_owned())].into_iter().collect();

        let broad = post_check(&db, project, &report.plan, &texts, PostCheckSurface::Everything);
        assert!(
            !broad.ok,
            "the broad surface must veto this seed — if it no longer does, the asymmetry has lost its justification and should be revisited"
        );
        assert!(
            broad.new_diagnostics.iter().any(|d| d.id == steins_infer::THROW_LISKOV_ID),
            "the veto must be the contract-layer interaction, not an unrelated regression: {:#?}",
            broad.new_diagnostics
        );

        let narrow = post_check(&db, project, &report.plan, &texts, PostCheckSurface::DefaultOnly);
        assert!(
            narrow.ok,
            "the same seed must pass the surface it is measured against: {:#?}",
            narrow.new_diagnostics
        );
    }

    /// The other half: phpdoc transforms measure against everything, contract
    /// included — a `phpdoc.*` finding is invisible on the default surface.
    #[test]
    fn the_phpdoc_transforms_are_measured_against_the_contract_layer_too() {
        assert!(
            PostCheckSurface::Everything.display().is_none(),
            "Everything must not filter by display surface"
        );
        let default_only =
            PostCheckSurface::DefaultOnly.display().expect("DefaultOnly resolves a surface");

        let contract_finding = Diagnostic {
            id: steins_infer::THROW_LISKOV_ID,
            path: "lib.php".to_owned(),
            line: 1,
            column: 1,
            message: String::new(),
            facet: None,
            fix: None,
        };
        let layout = ProjectLayout::fallback();
        assert_eq!(
            filtered_diagnostics(&layout, None, vec![contract_finding.clone()]).len(),
            1,
            "a contract finding must survive the broad surface"
        );
        assert!(
            filtered_diagnostics(&layout, Some(&default_only), vec![contract_finding]).is_empty(),
            "the same finding must be invisible on the default surface"
        );
    }

    // ---- dedup_canonical (issue #179) --------------------------------------

    /// Two spellings of one file collapse to one entry, first-pushed surviving.
    /// E2e repro: `tests/symlink_dedup.rs`.
    #[test]
    fn dedup_canonical_collapses_two_spellings_keeping_the_first() {
        let dir = std::env::temp_dir()
            .join(format!("steins-dedup-canonical-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/a.php"), "<?php\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();

        let first = dir.join("real/a.php");
        let second = dir.join("link/a.php"); // same file, symlinked spelling
        let out = dedup_canonical(vec![first.clone(), second]);
        assert_eq!(out, vec![first], "one real file survives, spelled as first pushed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path whose `canonicalize()` fails is never dropped: keyed on its own
    /// literal path (pre-#179 behavior).
    #[test]
    fn dedup_canonical_keeps_uncanonicalizable_paths() {
        let a = PathBuf::from("/steins-dedup-canonical-unit-does-not-exist-a.php");
        let b = PathBuf::from("/steins-dedup-canonical-unit-does-not-exist-b.php");
        let out = dedup_canonical(vec![a.clone(), b.clone(), a.clone()]);
        // `a` dedups against its own repeat but not against unrelated `b`.
        assert_eq!(out, vec![a, b]);
    }

    // ---- should_page --------------------------------------------------------

    /// Paging requires both an interactive terminal AND a non-blank `$PAGER`;
    /// piped output (`tests/license.rs`) must never page even with `$PAGER` set.
    #[test]
    fn pager_policy() {
        assert!(should_page(true, Some("less")));
        assert!(!should_page(false, Some("less")), "a pipe or redirect must never page");
        assert!(!should_page(true, None), "no PAGER set must not page");
        assert!(!should_page(true, Some("")), "an empty PAGER must not page");
        assert!(!should_page(true, Some("   ")), "a blank PAGER must not page");
    }
}
