//! `steins check` (ADR-0020): walk `.php` files, run the salsa pipeline, print
//! proof-layer diagnostics, exit 1 iff a fail-level finding is displayed
//! (ADR-0050 §7).
//!
//! The suppression channels run in ADR-0050 §6 order (vendor → surface →
//! policy → inline), then the baseline ratchet (ADR-0022, surface-aware per
//! ADR-0050 §8) partitions what survives. `--fix` applies fix payloads under
//! ADR-0034's transformed-or-refused discipline, gated by the post-check in
//! [`crate::transform`]. Rendering is the seam in [`crate::render`] (ADR-0054):
//! ONE report, whichever format was selected.
//!
//! The analysis itself comes through the frozen-generation lifecycle by
//! default (ADR-0092 §5, ADR-0020 amendment / issue #525) and through the
//! plain cold pipeline under `--no-cache` or on any degradation. The two
//! arms differ in cost and in nothing else: same findings, same channels, same
//! stderr — see [`crate::generation`] for why that silence is a property
//! rather than a preference. The config read and both arms are
//! [`analyze_check`], which the MCP `check` tool calls too; the baseline
//! channel, `--fix` and the render are this command's own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{Project, SteinsDatabase, parse as parse_tree};
use steins_edit::{ByteSpan, Edit, EditPlan};
use steins_infer::{
    Diagnostic, InlineOutcome, SOUND_SUBSET_NOTICE, SidecarFolder, apply_inline_ignores,
    check_project_under,
};
use steins_syntax::SourceTree;

use crate::config::{
    allow_list, effects_from_config, profiles_from_config, read_steins_config, runtime_from_config,
};
use crate::generation::{consume_cached_run, try_generation_check};
use crate::project::{LoadedProject, collect_files, load_project, reject_missing_paths};
use crate::transform::{PostCheckSurface, post_check};
use crate::{baseline, profile, render};

/// `steins check`'s command line, parsed. Every usage error is said on stderr as
/// it is found, and is exit 2.
#[derive(Default)]
struct CheckArgs {
    /// `None` until `--format` names one: absence is what auto-detection reads
    /// (ADR-0054 §6), so a default here would defeat GitHub Actions detection.
    format: Option<render::CheckFormat>,
    no_php: bool,
    no_cache: bool,
    no_tolerated_effects: bool,
    fix: bool,
    vendor_diagnostics: bool,
    profile: Option<String>,
    set_baseline: bool,
    ignore_baseline: bool,
    baseline: Option<String>,
    paths: Vec<String>,
}

impl CheckArgs {
    fn parse(args: &[String]) -> Result<Self, ExitCode> {
        let mut parsed = CheckArgs::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--no-php" => parsed.no_php = true,
                // The generation cache is on by default (ADR-0020 amendment,
                // issue #525); this is the opt-out, spelled like `--no-php`
                // because it switches off the same kind of thing — a capability
                // the run would otherwise use. Cost-only in both directions: a
                // run without the cache finds exactly what a run with it finds.
                "--no-cache" => parsed.no_cache = true,
                // ADR-0084 §1 audit switch: empties tolerance; attribution table unaffected.
                "--no-tolerated-effects" => parsed.no_tolerated_effects = true,
                "--fix" => parsed.fix = true,
                "--vendor-diagnostics" => parsed.vendor_diagnostics = true,
                "--profile" => {
                    parsed.profile = Some(flag_value(arg, args.next(), "a name argument")?);
                }
                "--set-baseline" => parsed.set_baseline = true,
                "--ignore-baseline" => parsed.ignore_baseline = true,
                "--baseline" => {
                    parsed.baseline = Some(flag_value(arg, args.next(), "a path argument")?);
                }
                "--format" => {
                    let value =
                        flag_value(arg, args.next(), "an argument (text|json|github|sarif)")?;
                    let Some(format) = render::CheckFormat::parse(&value) else {
                        errln!("steins: unknown format `{value}` (text|json|github|sarif)");
                        return Err(ExitCode::from(2));
                    };
                    parsed.format = Some(format);
                }
                other => parsed.paths.push(other.to_owned()),
            }
        }
        if parsed.paths.is_empty() {
            errln!("steins: no paths given");
            return Err(ExitCode::from(2));
        }
        // `--set-baseline` and `--fix` cannot combine (ambiguous which state the
        // baseline would capture) — usage error.
        if parsed.fix && parsed.set_baseline {
            errln!("steins: --fix cannot be combined with --set-baseline");
            return Err(ExitCode::from(2));
        }
        Ok(parsed)
    }

    /// The analysis this command line asks for over `files`. `check` says the
    /// `[runtime]` warnings on stderr, after the boundary notices.
    fn request<'a>(&'a self, files: &'a [PathBuf]) -> CheckRequest<'a> {
        CheckRequest {
            files,
            paths: &self.paths,
            profile: self.profile.as_deref(),
            no_tolerated_effects: self.no_tolerated_effects,
            no_php: self.no_php,
            no_cache: self.no_cache,
            vendor_diagnostics: self.vendor_diagnostics,
            runtime_warnings_on_stderr: true,
        }
    }

    /// Auto-detection (ADR-0054 §6): explicit `--format` wins, else env may
    /// name a consumer (GitHub Actions) — only the spelling changes.
    fn format(&self) -> render::CheckFormat {
        self.format.unwrap_or_else(render::detect_from_env)
    }

    /// The baseline file (ADR-0022): `--set-baseline`/`--baseline` name one
    /// explicitly, else the default auto-loads unless `--ignore-baseline`.
    fn baseline_file(&self) -> Option<PathBuf> {
        if self.set_baseline {
            Some(PathBuf::from(self.baseline.as_deref().unwrap_or(baseline::DEFAULT_FILE)))
        } else if self.ignore_baseline {
            None
        } else if let Some(p) = &self.baseline {
            Some(PathBuf::from(p))
        } else if Path::new(baseline::DEFAULT_FILE).exists() {
            Some(PathBuf::from(baseline::DEFAULT_FILE))
        } else {
            None
        }
    }
}

/// The value `flag` takes, or the usage error saying it `requires` one.
fn flag_value(flag: &str, value: Option<&String>, requires: &str) -> Result<String, ExitCode> {
    value.cloned().ok_or_else(|| {
        errln!("steins: {flag} requires {requires}");
        ExitCode::from(2)
    })
}

pub(crate) fn run_check(args: &[String]) -> ExitCode {
    let args = match CheckArgs::parse(args) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if let Err(code) = reject_missing_paths(&args.paths) {
        return code;
    }

    let files = collect_files(&args.paths);

    // Coverage posture (ADR-0004): `--no-php` runs the sound subset (notice up
    // front); otherwise folds via a lazily-spawned sidecar.
    if args.no_php {
        errln!("{SOUND_SUBSET_NOTICE}");
    }

    let CheckOutcome { surface, loaded, inline, vendor_suppressed, .. } =
        match analyze_check(&args.request(&files)) {
            Ok(outcome) => outcome,
            Err(e) => {
                errln!("steins: {e}");
                return ExitCode::from(2);
            }
        };
    let (db, project, texts) = (&loaded.db, loaded.project, &loaded.texts);

    let baseline_file = args.baseline_file();
    if args.set_baseline {
        let file = baseline_file.expect("set-baseline names a file");
        return write_baseline(&file, &inline.kept, texts, &surface);
    }

    let (reported, baselined, stale, surface_notice) =
        baseline_channel(baseline_file.as_deref(), inline.kept, texts, &surface);

    // Displayed = survivors + meta-diagnostics (exempt from both channels), sorted.
    let mut displayed = reported;
    displayed.extend(inline.meta);
    sort_displayed(&mut displayed);

    // `check --fix` (ADR-0010): applies fix payloads under ADR-0034's
    // transformed-or-refused discipline. Without the flag, `None` — unchanged.
    let fix_run = args.fix.then(|| apply_fixes(db, project, &displayed, texts));

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
    out!("{}", render::render(&report, args.format()));

    if let Some(run) = &fix_run {
        report_fix_run(run, fixed.len());
    }

    // Exit level (ADR-0050 §7): 1 iff any fail-level finding is displayed, else
    // 0 (warn-only); fixed findings are already gone from `displayed`.
    let any_fail = displayed.iter().any(|d| surface.level(d.id) == profile::Level::Fail);
    if any_fail { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// One check as its caller spells it: what to analyze, the caller's own
/// selections, and where the `[runtime]` warnings go.
pub(crate) struct CheckRequest<'a> {
    pub(crate) files: &'a [PathBuf],
    pub(crate) paths: &'a [String],
    /// The caller's profile selection, which beats `[check] profile`.
    pub(crate) profile: Option<&'a str>,
    /// `--no-tolerated-effects` (ADR-0084 §1).
    pub(crate) no_tolerated_effects: bool,
    pub(crate) no_php: bool,
    /// Skip the generation lifecycle and run the cold pipeline.
    pub(crate) no_cache: bool,
    pub(crate) vendor_diagnostics: bool,
    /// Whether the `[runtime]` warnings are said on stderr, after the boundary
    /// notices. `check` says them there; the MCP tool carries them in its reply
    /// document instead, and saying them on stderr too would say them twice.
    pub(crate) runtime_warnings_on_stderr: bool,
}

/// What a check hands its report: the surface it displays under, the
/// `[runtime]` warnings, the salsa view (for `--fix` and the baseline
/// machinery), the inline outcome and the vendor count.
pub(crate) struct CheckOutcome {
    pub(crate) surface: profile::Surface,
    pub(crate) runtime_warnings: Vec<String>,
    pub(crate) loaded: LoadedProject,
    pub(crate) inline: InlineOutcome,
    pub(crate) vendor_suppressed: usize,
}

/// Why a check could not start: exit 2 on the command line, a named refusal
/// over MCP.
pub(crate) enum SetupError {
    /// `steins.toml` does not parse, an unknown `[runtime]` key included.
    Config(String),
    /// The selected profile does not resolve.
    Profile(profile::ConfigError),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Config(e) => write!(f, "{e}"),
            SetupError::Profile(e) => write!(f, "{e}"),
        }
    }
}

/// Resolve `./steins.toml` and analyze — the one path `steins check` and the
/// MCP `check` tool share, so the two cannot answer differently.
///
/// The analysis comes through the generation lifecycle unless the request says
/// `no_cache`, and falls back to the cold pipeline in silence. Either arm
/// prints the same boundary notices in the same order — plugin refusals, the
/// effect label vocabulary, attribution hygiene, then the `[runtime]` warnings
/// when the request wants them on stderr — and runs the `[runtime]` postures
/// whole: every posture is part of the generation's identity, so declaring
/// some of them on one arm and all of them on the other would key two stores
/// over one tree.
pub(crate) fn analyze_check(req: &CheckRequest<'_>) -> Result<CheckOutcome, SetupError> {
    // Parse `./steins.toml` once, up front (ADR-0050 §7/ADR-0052 §5 N2): a
    // malformed file (incl. an unknown `[runtime]` key) is exit 2, never warn-and-proceed.
    let config = read_steins_config().map_err(SetupError::Config)?;
    let (check_cfg, profile_tbl, runtime_cfg, plugin_allow, effects_cfg) = match config {
        Some(c) => (c.check, c.profile, c.runtime, allow_list(c.plugins), c.effects),
        None => (None, None, None, None, None),
    };
    let effects_policy = effects_from_config(effects_cfg, req.no_tolerated_effects);

    // Active display surface (ADR-0050 §5), resolved before analysis (config
    // error fails fast, exit 2). Precedence: `--profile` > `[check] profile` > `default`.
    let (config_profile, profile_configs) = profiles_from_config(check_cfg, profile_tbl);
    let selected = req.profile.or(config_profile.as_deref());
    let surface = profile_configs.resolve(selected).map_err(SetupError::Profile)?;

    // `[runtime]` pseudo-constants (ADR-0037 §2), resolved up front (pure;
    // an unknown value on a known key warns — printed in each arm below at
    // the same point it always was — and keeps the safe default).
    let (postures, runtime_warnings) = runtime_from_config(runtime_cfg);
    let said: &[String] = if req.runtime_warnings_on_stderr { &runtime_warnings } else { &[] };

    // The frozen-generation lifecycle (ADR-0092 §5): how a check runs unless
    // `--no-cache` says otherwise (ADR-0020 amendment, issue #525). Silent in
    // both directions — the cached arm prints the boundary notices the cold
    // arm prints and nothing more, and any degradation falls through to the
    // cold arm below with stderr still untouched.
    let cached = if req.no_cache {
        None
    } else {
        try_generation_check(
            req.files,
            req.paths,
            plugin_allow.as_deref(),
            &effects_policy,
            &postures,
            req.no_php,
            said,
        )
    };

    // Suppression channels, ADR-0050 §6 order (vendor → surface → policy →
    // inline). Baseline stays out: it's the CI ratchet, this command's own
    // argument. The cached arm supplies the orchestrator's own trees so the
    // inline scan re-parses nothing; the cold arm reads the salsa parse memo.
    let (loaded, inline, vendor_suppressed) = match cached {
        Some(run) => consume_cached_run(run, &surface, req.vendor_diagnostics),
        None => {
            // One folder for the whole run: owns the sidecar + fold memo, so repeated
            // calls across files never re-spawn or re-fold.
            let mut folder =
                if req.no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

            // Project mode (ADR-0009/0015): all `.php` files form ONE project (one salsa
            // DB) so cross-file calls, class chains, effects resolve.
            let loaded =
                load_project(req.files, req.paths, plugin_allow.as_deref(), effects_policy);
            // Target PHP range (issue #28) gates the folder's absence family and curated facts.
            folder.set_php_target(loaded.layout.php_target().cloned());
            for w in said {
                errln!("steins: {w}");
            }
            let findings: Vec<Diagnostic> =
                check_project_under(&loaded.db, loaded.project, &mut folder, postures);
            let (inline, vendor_suppressed) =
                suppression_pipeline(&loaded, findings, &surface, req.vendor_diagnostics);
            (loaded, inline, vendor_suppressed)
        }
    };
    Ok(CheckOutcome { surface, runtime_warnings, loaded, inline, vendor_suppressed })
}

/// The one order a check displays findings in: path, line, column, id.
pub(crate) fn sort_displayed(displayed: &mut [Diagnostic]) {
    displayed.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column, a.id).cmp(&(b.path.as_str(), b.line, b.column, b.id))
    });
}

/// Outcome of a `check --fix` run. `applied` is true iff edits were written; a
/// refusal (four named reasons) leaves findings as a plain run reports them.
pub(crate) struct FixRun {
    pub(crate) applied: bool,
    files_written: usize,
    pub(crate) refusal: Option<FixRefusal>,
}

/// A named fix refusal (ADR-0034 Refusal discipline): machine `reason`, human
/// `detail`, and the diagnostics the edits would have surfaced.
pub(crate) struct FixRefusal {
    pub(crate) reason: &'static str,
    pub(crate) detail: String,
    pub(crate) new_diagnostics: Vec<Diagnostic>,
}

/// Fix-run accounting, after the report like other maintenance confirmations.
fn report_fix_run(run: &FixRun, fixed: usize) {
    if run.applied {
        errln!("steins: fixed {fixed} finding(s) ({} file(s) written)", run.files_written);
    } else if let Some(r) = &run.refusal {
        errln!("steins: fix refused ({}): {}", r.reason, r.detail);
    } else {
        errln!("steins: no fixable findings");
    }
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

/// Baseline channel: partitions survivors into baselined (excluded) and
/// reported; no file (or an unreadable one) → all report. Staleness is
/// surface-aware (ADR-0050 §8). Returns what [`match_baseline`] returns.
fn baseline_channel(
    file: Option<&Path>,
    kept: Vec<Diagnostic>,
    texts: &HashMap<String, String>,
    surface: &profile::Surface,
) -> (Vec<Diagnostic>, usize, usize, Option<String>) {
    match file {
        Some(file) => match std::fs::read_to_string(file) {
            Ok(text) => match_baseline(file, &text, kept, texts, surface),
            Err(_) => (kept, 0, 0, None),
        },
        None => (kept, 0, 0, None),
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

/// Suppression channels, ADR-0050 §6 order: vendor → surface → policy →
/// inline. Baseline deliberately NOT here — a per-invocation argument.
pub(crate) fn suppression_pipeline(
    loaded: &LoadedProject,
    findings: Vec<Diagnostic>,
    surface: &profile::Surface,
    vendor_diagnostics: bool,
) -> (steins_infer::InlineOutcome, usize) {
    let db = &loaded.db;
    let trees: Vec<&SourceTree> = loaded.inputs.iter().map(|&sf| parse_tree(db, sf)).collect();
    let file_pairs: Vec<(String, &SourceTree)> = loaded
        .inputs
        .iter()
        .zip(trees.iter())
        .map(|(&sf, &t)| (sf.path(db).to_owned(), t))
        .collect();
    suppression_over(&loaded.layout, file_pairs, findings, surface, vendor_diagnostics)
}

/// The pipeline proper, over trees the caller already holds — the seam the
/// generation path (issue #489) comes through with the orchestrator's owned
/// trees, so a warm run's inline scan re-parses nothing.
pub(crate) fn suppression_over(
    layout: &steins_db::ProjectLayout,
    file_pairs: Vec<(String, &SourceTree)>,
    mut findings: Vec<Diagnostic>,
    surface: &profile::Surface,
    vendor_diagnostics: bool,
) -> (steins_infer::InlineOutcome, usize) {
    // Vendor filtering FIRST (ADR-0015): suppressed by default, must not eat a
    // baseline entry. `--vendor-diagnostics` opts back in.
    let mut vendor_suppressed = 0usize;
    if !vendor_diagnostics {
        let before = findings.len();
        findings.retain(|d| !layout.is_vendor(&d.path));
        vendor_suppressed = before - findings.len();
    }

    // Profile surface (ADR-0050 §6): bare `check` shows proof + mechanics;
    // named profiles opt into contracts. Mechanics ids stay on always (§1).
    findings.retain(|d| surface.is_surfaced(d));

    // Scoped policy, third stage (ADR-0050 §6); currently an identity.
    let findings = apply_policy_stage(findings);

    // Inline `@steins-ignore` next (ADR-0023): suppressed findings skip the baseline.
    (apply_inline_ignores(findings, &file_pairs), vendor_suppressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use steins_db::{ProjectLayout, SourceFile};

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
}
