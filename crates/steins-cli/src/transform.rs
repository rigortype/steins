//! `steins transform` (ADR-0020/0034) and the dual-verification post-check it
//! shares with `check --fix` and the MCP surface (issue #117).
//!
//! Dry-run by default: diff + refusal report + post-check (ADR-0034 point 3a,
//! zero new diagnostics). [`plan_transform_run`] produces a [`TransformRun`]
//! without writing anything; `transform --apply` and MCP `apply_plan` are the
//! only code reaching disk, and only after [`post_check`] passes on the surface
//! the transform names for itself ([`PostCheckSurface`], issue #115).

use std::collections::HashMap;
use std::process::ExitCode;

use steins_db::{Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_edit::{
    EditPlan, LoopToArrayMapOptions, TransformReport, plan_effects_envelope,
    plan_loop_to_array_map, plan_phpdoc_honesty, plan_phpdoc_to_native, plan_throws_envelope,
    unified_diff,
};
use steins_infer::{Diagnostic, NoFold, check_project};

use crate::config::{allow_list_from_disk, effects_policy_from_disk, load_partitions, load_vouches};
use crate::project::{collect_files, load_project, reject_missing_paths};
use crate::{Format, profile};

/// `steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map>
/// [--apply] [--asserted-subjects] [--format text|json] <paths...>` (ADR-0020/0034).
/// Dry-run by default: diff + refusal report + post-check (ADR-0034 point 3a,
/// zero new diagnostics; see [`PostCheckSurface`]). `--apply` writes only
/// after post-check passes. Exit 2 usage error, 1 post-check fail, 0 else.
pub(crate) fn run_transform(args: &[String]) -> ExitCode {
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
pub(crate) enum TransformKind {
    Promote,
    Honesty,
    ThrowsEnvelope,
    EffectsEnvelope,
    LoopToArrayMap,
}

impl TransformKind {
    /// Every transform, in the order the usage line lists them.
    pub(crate) const ALL: [TransformKind; 5] = [
        TransformKind::Promote,
        TransformKind::Honesty,
        TransformKind::ThrowsEnvelope,
        TransformKind::EffectsEnvelope,
        TransformKind::LoopToArrayMap,
    ];

    /// The stable command id: the subcommand word and MCP `plan_transform` argument.
    pub(crate) fn id(self) -> &'static str {
        match self {
            TransformKind::Promote => "phpdoc-to-native",
            TransformKind::Honesty => "phpdoc-honesty",
            TransformKind::ThrowsEnvelope => "throws-envelope",
            TransformKind::EffectsEnvelope => "effects-envelope",
            TransformKind::LoopToArrayMap => "loop-to-array-map",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        TransformKind::ALL.into_iter().find(|k| k.id() == id)
    }

    /// The verb the completeness-oracle summary uses for an edited site.
    pub(crate) fn action(self) -> &'static str {
        match self {
            TransformKind::Promote => "promoted",
            TransformKind::Honesty | TransformKind::LoopToArrayMap => "rewritten",
            TransformKind::ThrowsEnvelope | TransformKind::EffectsEnvelope => "seeded",
        }
    }

    /// One sentence describing what the transform rewrites, for an agent choosing one.
    pub(crate) fn summary(self) -> &'static str {
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
    pub(crate) fn supports_asserted_subjects(self) -> bool {
        matches!(self, TransformKind::LoopToArrayMap)
    }

    /// The surface this transform's post-check is measured against (ADR-0034
    /// point 3a, issue #115), named once so the CLI and MCP agree.
    pub(crate) fn post_check_surface(self) -> PostCheckSurface {
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
pub(crate) struct TransformRun {
    pub(crate) report: TransformReport,
    pub(crate) postcheck: PostCheck,
    pub(crate) texts: HashMap<String, String>,
    /// Human notices to report on the way out: vouch-file problems and
    /// no-op vouch entries (ADR-0046 §2).
    pub(crate) notices: Vec<String>,
}

/// Plan `kind` over `paths` and run its post-check (ADR-0010's dry-run half),
/// shared by `steins transform` and MCP `plan_transform`. `Err` is a config
/// error (exit 2): overlapping partition path-sets. A vouch typo is a notice.
pub(crate) fn plan_transform_run(
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

/// Outcome of the dual-verification post-check: whether the edit is clean,
/// plus diagnostics whose per-id count increased.
pub(crate) struct PostCheck {
    pub(crate) ok: bool,
    pub(crate) new_diagnostics: Vec<Diagnostic>,
}

/// Which diagnostics a post-check counts as "new" (ADR-0034 point 3a). Not a
/// global default — each call site names its own (issue #115).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostCheckSurface {
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
    pub(crate) fn name(self) -> &'static str {
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
/// Deliberately cold on both sides: `before` and `after` must share one
/// analysis posture or a regression here measures the posture gap, not the
/// edit (why that rules out a warm `before` — see the `mcp` module docs,
/// issue #491).
pub(crate) fn post_check(
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
pub(crate) fn transform_json(
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

#[cfg(test)]
mod tests {
    use super::*;
    use steins_db::PluginFacts;

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
}
