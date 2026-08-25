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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{Project, SteinsDatabase, parse as parse_tree};
use steins_edit::{ByteSpan, Edit, EditPlan};
use steins_infer::{
    Diagnostic, SOUND_SUBSET_NOTICE, SidecarFolder, apply_inline_ignores,
    check_project_with_postures,
};
use steins_syntax::SourceTree;

use crate::config::{
    allow_list, effects_from_config, profiles_from_config, read_steins_config, runtime_from_config,
};
use crate::project::{LoadedProject, collect_files, load_project, reject_missing_paths};
use crate::transform::{PostCheckSurface, post_check};
use crate::{baseline, profile, render};

pub(crate) fn run_check(args: &[String]) -> ExitCode {
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

    // `[runtime]` pseudo-constants (ADR-0037 §2), resolved up front (pure;
    // an unknown value on a known key warns — printed in each arm below at
    // the same point it always was — and keeps the safe default).
    let (postures, runtime_warnings) = runtime_from_config(runtime_cfg);

    // The experimental frozen-generation lifecycle (ADR-0092 §5, issue #489
    // slice A): activated only by `STEINS_EXPERIMENTAL_GENERATIONS=1` in the
    // environment — read once here, plumbed as a bool, deliberately no CLI
    // flag (flag promotion is an ADR-0020 owner decision). With the variable
    // unset — every CI run — this function is byte-identical to before the
    // gate existed; any gated failure degrades to the ordinary arm below.
    let experimental_generations =
        std::env::var("STEINS_EXPERIMENTAL_GENERATIONS").is_ok_and(|v| v == "1");
    let gated = if experimental_generations {
        crate::generation::try_generation_check(
            &files,
            &paths,
            plugin_allow.as_deref(),
            &effects_policy,
            &postures,
            no_php,
            &runtime_warnings,
        )
    } else {
        None
    };

    let (loaded, findings, gated_trees) = match gated {
        Some(run) => (run.loaded, run.findings, Some(run.trees)),
        None => {
            // One folder for the whole run: owns the sidecar + fold memo, so repeated
            // calls across files never re-spawn or re-fold.
            let mut folder =
                if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

            // Project mode (ADR-0009/0015): all `.php` files form ONE project (one salsa
            // DB) so cross-file calls, class chains, effects resolve.
            let loaded = load_project(&files, &paths, plugin_allow.as_deref(), effects_policy);
            // Target PHP range (issue #28) gates the folder's absence family and curated facts.
            folder.set_php_target(loaded.layout.php_target().cloned());
            for w in &runtime_warnings {
                errln!("steins: {w}");
            }
            let findings: Vec<Diagnostic> = check_project_with_postures(
                &loaded.db,
                loaded.project,
                &mut folder,
                postures.warning_handler_abort,
                postures.final_keyword,
            );
            (loaded, findings, None)
        }
    };
    let (db, project, texts) = (&loaded.db, loaded.project, &loaded.texts);

    // Suppression channels, ADR-0050 §6 order (vendor → surface → policy →
    // inline). Baseline stays here: it's the CI ratchet, this command's own
    // argument. The gated arm supplies the orchestrator's own trees so the
    // inline scan re-parses nothing; the cold arm reads the salsa parse memo.
    let (inline, vendor_suppressed) = match &gated_trees {
        Some(trees) => {
            let pairs: Vec<(String, &SourceTree)> =
                trees.iter().map(|(path, tree)| (path.clone(), tree)).collect();
            suppression_over(&loaded.layout, pairs, findings, &surface, vendor_diagnostics)
        }
        None => suppression_pipeline(&loaded, findings, &surface, vendor_diagnostics),
    };

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
/// experimental generation path (issue #489) comes through with the
/// orchestrator's owned trees, so a warm run's inline scan re-parses nothing.
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
