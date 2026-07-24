//! The `steins` binary.
//!
//! Two commands exist for this milestone (ADR-0020 documents the eventual
//! six-command surface; the rest are deliberately NOT stubbed). `check` walks
//! `.php` files, runs the salsa pipeline, prints proof-layer diagnostics, and
//! exits 1 if any finding was reported. `annotate` reprints a single file with a
//! right-margin column of *proven* inferred facts (the Rigor-style display).

mod baseline;
mod doctor;
mod profile;
mod sha256;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{Project, SourceFile, SteinsDatabase, parse as parse_tree};
use steins_edit::{
    PartitionMap, TransformReport, VouchSet, plan_phpdoc_honesty, plan_phpdoc_to_native,
    unified_diff,
};
use steins_infer::{
    Diagnostic, LineFact, NoFold, SOUND_SUBSET_NOTICE, SidecarFolder, annotate_file,
    annotate_project, apply_inline_ignores, check_project, check_project_with_runtime,
    is_vendor_path,
};
use steins_syntax::SourceTree;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("check") => run_check(&args[1..]),
        Some("annotate") => run_annotate(&args[1..]),
        Some("transform") => run_transform(&args[1..]),
        Some("doctor") => doctor::run_doctor(&args[1..]),
        Some(other) => {
            eprintln!(
                "steins: unknown command `{other}` (available: check, annotate, transform, doctor)"
            );
            ExitCode::from(2)
        }
        None => {
            eprintln!(
                "usage: steins check [--format text|json] [--profile <name>] [--no-php] [--vendor-diagnostics] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>"
            );
            eprintln!("       steins annotate [--no-php] <file.php>");
            eprintln!(
                "       steins transform <phpdoc-to-native|phpdoc-honesty> [--apply] [--format text|json] <paths...>"
            );
            eprintln!("       steins doctor [--no-php] [--baseline <path>] [path]");
            ExitCode::from(2)
        }
    }
}

fn run_check(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut no_php = false;
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
            "--vendor-diagnostics" => {
                vendor_diagnostics = true;
                i += 1;
            }
            "--profile" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("steins: --profile requires a name argument");
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
                    eprintln!("steins: --baseline requires a path argument");
                    return ExitCode::from(2);
                };
                baseline_path = Some(value.clone());
                i += 2;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                match value.as_str() {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    other => {
                        eprintln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    if paths.is_empty() {
        eprintln!("steins: no paths given");
        return ExitCode::from(2);
    }

    let mut files = Vec::new();
    for p in &paths {
        collect_php_files(Path::new(p), &mut files);
    }
    files.sort();
    files.dedup();

    // Coverage posture (ADR-0004): with `--no-php` the run is the sound subset,
    // surfaced up front as a startup notice. Without the flag we fold via a
    // lazily-spawned sidecar (spawned only on the first foldable call); if `php`
    // turns out to be unavailable, the folder emits the same notice itself.
    if no_php {
        eprintln!("{SOUND_SUBSET_NOTICE}");
    }

    // Parse `./steins.toml` ONCE, up front, before any analysis (ADR-0050 §7 /
    // ADR-0052 §5 N2): a malformed file — including an unknown key in `[runtime]`,
    // which `deny_unknown_fields` rejects — is a HARD config error (exit 2), never
    // a warn-and-proceed. Silently proceeding on defaults would let a `zend-asertions`
    // typo leave the safe default in force while the user believed they had overridden
    // it. A missing file is `None` (the built-in defaults govern).
    let config = match read_steins_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("steins: {e}");
            return ExitCode::from(2);
        }
    };
    let (check_cfg, profile_tbl, runtime_cfg) = match config {
        Some(c) => (c.check, c.profile, c.runtime),
        None => (None, None, None),
    };

    // The active display surface (ADR-0050 §5): resolve the selected profile before
    // any analysis so a config error fails fast (exit 2). Precedence: the
    // `--profile` flag beats `[check] profile` beats the built-in `default`.
    let (config_profile, profile_configs) = profiles_from_config(check_cfg, profile_tbl);
    let selected = profile_flag.as_deref().or(config_profile.as_deref());
    let surface = match profile_configs.resolve(selected) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("steins: {e}");
            return ExitCode::from(2);
        }
    };

    let db = SteinsDatabase::default();
    // One folder for the whole run: it owns the resident sidecar and the fold
    // memo, so a repeated call across files never re-spawns or re-folds.
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

    // Project mode (ADR-0009/0015): all `.php` files across the given paths form
    // ONE project (one salsa DB), so cross-file calls, class chains, and effects
    // resolve. `texts` keeps each file's contents by diagnostic path so the
    // baseline hash can read the flagged line's neighborhood (ADR-0022).
    let mut inputs: Vec<SourceFile> = Vec::new();
    let mut texts: HashMap<String, String> = HashMap::new();
    for file_path in &files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                eprintln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        let path = file_path.to_string_lossy().into_owned();
        texts.insert(path.clone(), text.clone());
        inputs.push(SourceFile::new(&db, path, text));
    }
    let project = Project::new(&db, inputs.clone());
    // `[runtime]` pseudo-constants (ADR-0052 §5): the boot truth the checker cannot
    // observe from source (e.g. `zend-assertions = "enabled"`). Parsed above with the
    // rest of the config; an unknown *value* on a known key still warns and keeps the
    // safe default (a parse error already exited 2).
    let (zend_assertions, warning_handler_abort, runtime_warnings) =
        runtime_from_config(runtime_cfg);
    for w in &runtime_warnings {
        eprintln!("steins: {w}");
    }
    let mut findings: Vec<Diagnostic> = check_project_with_runtime(
        &db,
        project,
        &mut folder,
        zend_assertions,
        warning_handler_abort,
    );

    // Vendor filtering applies FIRST (ADR-0015), before inline ignores and the
    // baseline: vendor code is fully indexed and inferred, but a finding whose
    // path is inside a `vendor/` directory is suppressed by default and never
    // reaches — nor consumes — a later channel (a vendor finding must not eat a
    // baseline entry). `--vendor-diagnostics` opts back in, sending vendor
    // findings through the normal channels like any first-party finding.
    let mut vendor_suppressed = 0usize;
    if !vendor_diagnostics {
        let before = findings.len();
        findings.retain(|d| !is_vendor_path(&d.path));
        vendor_suppressed = before - findings.len();
    }

    // Profile surface (ADR-0050 §6, second in the pipeline): a finding off the
    // active surface does not exist — it never reaches, nor consumes, a later
    // channel (inline ignore, baseline). This is the M2 adoption-surface core: a
    // bare `check` shows proof + mechanics only; the contract layer opts up via a
    // named profile. Mechanics ids stay on in every profile (anti-rot, §1).
    findings.retain(|d| surface.is_surfaced(d));

    // Scoped policy (ADR-0050 §6, third in the pipeline): `[[policy]]` per-path
    // enable/disable is issue #15 / slice 3. This slice ships the seam as a no-op
    // stage so the composition order is real and slice 3 slots in without moving
    // the earlier stages.
    let findings = apply_policy_stage(findings);

    // Inline `@steins-ignore` applies next (ADR-0023): a finding suppressed
    // inline never reaches — nor consumes — the baseline channel.
    let trees: Vec<&SourceTree> = inputs.iter().map(|&sf| parse_tree(&db, sf)).collect();
    let file_pairs: Vec<(String, &SourceTree)> =
        inputs.iter().zip(trees.iter()).map(|(&sf, &t)| (sf.path(&db).to_owned(), t)).collect();
    let inline = apply_inline_ignores(findings, &file_pairs);

    // Which baseline file to consult (ADR-0022): `--set-baseline` and an explicit
    // `--baseline` both name a file; otherwise the default is auto-loaded when it
    // exists, unless `--ignore-baseline` bypasses it.
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
        return write_baseline(&file, &inline.kept, &texts, &surface);
    }

    // Baseline channel: partition the inline survivors into baselined (suppressed,
    // excluded from exit) and reported. `--ignore-baseline` / no file → all report.
    // Staleness is surface-aware (ADR-0050 §8): an unconsumed entry outside the
    // current surface is dormant, not stale. `surface_notice` fires when the active
    // surface exceeds the captured one (the drowns-loudly rule).
    let (reported, baselined, stale, surface_notice) = match &baseline_file {
        Some(file) => match std::fs::read_to_string(file) {
            Ok(text) => match_baseline(file, &text, inline.kept, &texts, &surface),
            Err(_) => (inline.kept, 0, 0, None),
        },
        None => (inline.kept, 0, 0, None),
    };

    // Displayed = object-level survivors + meta-diagnostics (which are exempt from
    // both channels). Sorted for deterministic output.
    let mut displayed = reported;
    displayed.extend(inline.meta);
    displayed.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column, a.id).cmp(&(b.path.as_str(), b.line, b.column, b.id))
    });

    match format {
        Format::Text => print_text(
            &displayed,
            &surface,
            vendor_suppressed,
            inline.suppressed,
            baselined,
            stale,
            surface_notice.as_deref(),
        ),
        Format::Json => {
            print_json(&displayed, &surface, vendor_suppressed, inline.suppressed, baselined)
        }
    }

    // Exit level (ADR-0050 §7): 1 iff any *fail*-level finding is displayed; a
    // warn-only run exits 0 (that is what `warn` means). Config/usage errors already
    // exited 2 above.
    let any_fail = displayed.iter().any(|d| surface.level(d.id) == profile::Level::Fail);
    if any_fail { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// The `[[policy]]` scoped enable/disable stage (ADR-0050 §6): issue #15 / slice 3.
/// A no-op identity today — the structural seam that keeps the composition order
/// (vendor → surface → policy → inline → baseline) real so slice 3 slots in without
/// disturbing the surrounding stages.
fn apply_policy_stage(findings: Vec<Diagnostic>) -> Vec<Diagnostic> {
    findings
}

/// Write a baseline file from the inline-surviving findings (ADR-0022
/// `--set-baseline`). Never affects exit code: writing is a maintenance action.
/// The header records the capture surface (ADR-0050 §8): the active `surface`'s
/// profile name and resolved id-set, so a later run can tell dormant from stale and
/// notice when its surface exceeds this one.
fn write_baseline(
    file: &Path,
    findings: &[Diagnostic],
    texts: &HashMap<String, String>,
    surface: &profile::Surface,
) -> ExitCode {
    let dir = baseline::base_dir(file);
    let entries: Vec<baseline::Entry> = findings
        .iter()
        .map(|d| {
            let rel = baseline::relativize(&dir, &d.path);
            let hash = texts
                .get(&d.path)
                .map_or_else(String::new, |t| baseline::entry_hash(d.id, &rel, t, d.line));
            baseline::Entry { id: d.id.to_owned(), path: rel, hash }
        })
        .collect();
    let n = entries.len();
    let capture = baseline::CaptureSurface { profile: surface.name.clone(), ids: surface.surface_ids() };
    match std::fs::write(file, baseline::render(entries, &capture)) {
        Ok(()) => {
            eprintln!(
                "steins: wrote {n} baseline entries to {} (profile `{}`)",
                file.display(),
                surface.name
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("steins: cannot write baseline {}: {e}", file.display());
            ExitCode::from(2)
        }
    }
}

/// Match inline-surviving `findings` against a baseline file's entries. Returns
/// `(reported, baselined_count, stale_count, surface_notice)`.
///
/// Surface-aware (ADR-0050 §8): staleness counts only unconsumed entries whose id
/// is inside the current `surface` (others are dormant — kept silently). The
/// `surface_notice` is `Some` when the active surface admits an id the captured
/// surface (from the header) did not — the "drowns loudly" rule so a
/// `--profile contracts` run over a default-captured baseline says so.
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
    let stale = matcher.stale_count_within(|id| surface.surfaces_id(id));

    // The drowns-loudly notice (ADR-0050 §8): ids the current surface admits that
    // the captured surface did not. A pre-ADR-0050 header (no capture surface) can
    // not be compared, so no notice fires.
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

/// `steins transform <phpdoc-to-native|phpdoc-honesty> [--apply] [--format
/// text|json] <paths...>` (ADR-0020/0034). Dry-run by default: prints a unified
/// diff and a refusal report, and runs the dual-verification post-check (ADR-0034
/// point 3a — the edited project must produce *zero new diagnostics*). `--apply`
/// writes the edited files only after the post-check passes. Exits 2 on usage
/// error, 1 when the post-check fails, 0 otherwise.
fn run_transform(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut apply = false;
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
            "--config" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("steins: --config requires a path argument");
                    return ExitCode::from(2);
                };
                config_path = Some(value.clone());
                i += 2;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                match value.as_str() {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    other => {
                        eprintln!("steins: unknown format `{other}` (text|json)");
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

    // Select the transform planner by subcommand. `action` is the verb the oracle
    // summary uses for an edited site.
    #[derive(Clone, Copy)]
    enum Kind {
        Promote,
        Honesty,
    }
    let (kind, action) = match subcommand.as_deref() {
        Some("phpdoc-to-native") => (Kind::Promote, "promoted"),
        Some("phpdoc-honesty") => (Kind::Honesty, "rewritten"),
        Some(other) => {
            eprintln!(
                "steins: unknown transform `{other}` (available: phpdoc-to-native, phpdoc-honesty)"
            );
            return ExitCode::from(2);
        }
        None => {
            eprintln!(
                "steins: transform requires a name (usage: steins transform <phpdoc-to-native|phpdoc-honesty> [--apply] [--config steins.toml] [--format text|json] <paths...>)"
            );
            return ExitCode::from(2);
        }
    };
    if paths.is_empty() {
        eprintln!("steins: no paths given");
        return ExitCode::from(2);
    }

    // Load the vouching valve (ADR-0046 §2): `steins.toml [transform.vouch]` from
    // `--config`, else `./steins.toml` if present. A malformed entry is a warning,
    // never a hard error (the run proceeds with the well-formed entries).
    let (vouches, vouch_warnings) = load_vouches(config_path.as_deref());
    for w in &vouch_warnings {
        eprintln!("steins: {w}");
    }

    // Load the region map (ADR-0047 §7): `steins.toml [transform.partitions]`. With
    // no such section this is `None` — the single-region identity, so the transform
    // is byte-identical to a run with no partition config. An *overlap* in the
    // declared partition path-sets is a hard config error (unlike a vouch typo),
    // because a non-disjoint partition claim has no defined region assignment.
    let partitions = match load_partitions(config_path.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("steins: {e}");
            return ExitCode::from(2);
        }
    };

    let mut files = Vec::new();
    for p in &paths {
        collect_php_files(Path::new(p), &mut files);
    }
    files.sort();
    files.dedup();

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::new();
    let mut texts: HashMap<String, String> = HashMap::new();
    for file_path in &files {
        let text = match std::fs::read(file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                eprintln!("steins: cannot read {}: {e}", file_path.display());
                continue;
            }
        };
        let path = file_path.to_string_lossy().into_owned();
        texts.insert(path.clone(), text.clone());
        inputs.push(SourceFile::new(&db, path, text));
    }
    let project = Project::new(&db, inputs.clone());

    // Plan the transform (pure — no writes, no re-check).
    // ADR-0047 Slice A: the region map is threaded into the planners but not yet
    // consumed by any decision — the plan is identical with or without it.
    let report = match kind {
        Kind::Promote => plan_phpdoc_to_native(&db, project, &vouches, partitions.as_ref()),
        Kind::Honesty => plan_phpdoc_honesty(&db, project, &vouches, partitions.as_ref()),
    };

    // Vouching an already-benign (or nonexistent) site is a no-op the user should
    // know about (ADR-0046 §2).
    for entry in vouches.unused() {
        eprintln!("steins: vouched site `{entry}` matched no dynamic-code obstacle (no-op)");
    }

    // Dual verification (ADR-0034 point 3a): re-analyze the edited project and
    // require zero NEW diagnostics vs. the pre-edit baseline. Run in both dry-run
    // and `--apply`, so a violation is visible before anything is written.
    let postcheck = post_check(&db, project, &report, &texts);

    match format {
        Format::Json => print_transform_json(&report, &postcheck, apply && postcheck.ok),
        Format::Text => print_transform_text(&report, &postcheck, &texts, action),
    }

    if !postcheck.ok {
        if apply {
            eprintln!(
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
                eprintln!("steins: cannot write {path}: {e}");
                return ExitCode::FAILURE;
            }
            written += 1;
        }
        for nf in &report.plan.new_files {
            if let Err(e) = std::fs::write(&nf.path, &nf.contents) {
                eprintln!("steins: cannot create {}: {e}", nf.path);
                return ExitCode::FAILURE;
            }
            written += 1;
        }
        eprintln!("steins: applied {written} file edit(s)");
    }

    ExitCode::SUCCESS
}

/// `steins.toml` — the `[transform.vouch]` (ADR-0046 §2) and
/// `[transform.partitions]` (ADR-0047 §7) sections are read this slice. Unknown
/// keys are ignored so the file can carry future config.
#[derive(serde::Deserialize, Default)]
struct SteinsConfig {
    transform: Option<TransformConfig>,
    runtime: Option<RuntimeConfig>,
    /// The `[check]` section (ADR-0050 §5): the repo's default profile selection.
    check: Option<CheckConfig>,
    /// The `[profile.<name>]` table (ADR-0050 §5): user-defined profiles.
    profile: Option<std::collections::BTreeMap<String, ProfileEntryConfig>>,
}

/// The `[check]` section (ADR-0050 §5): repo defaults for `steins check`. Today it
/// carries the default profile name; the `--profile` flag beats it (§5).
#[derive(serde::Deserialize, Default)]
struct CheckConfig {
    profile: Option<String>,
}

/// A `[profile.<name>]` entry (ADR-0050 §5): `extends` a base and refines it with
/// ADR-0022 prefix id-arrays. Facet selection is not accepted here in v1 (the
/// deferred-with-design lenient path, §4/§11): a facet-shaped token in any array is
/// an unknown id pattern and errors clearly.
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

/// The `[runtime]` section (ADR-0052 §5 / ADR-0037 §2): boot-truth pseudo-constants
/// the checker cannot observe from source. `deny_unknown_fields` makes a misspelled
/// key a hard parse error — a security-relevant knob (a silently-ignored
/// `zend-asertions` typo would leave the safe default in force while the user
/// believed otherwise). Reserved keys for future runtime pseudo-constants
/// (ADR-0049's `warning-handler`, `include-path`, `sapi`) join here as they land.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RuntimeConfig {
    /// `zend-assertions = "enabled"` promotes `assert($expr)` narrowing to the
    /// `Verified` stratum. Any other value (or absence) keeps the safe production
    /// default (`zend.assertions=-1`, narrowing stays `Asserted`).
    #[serde(rename = "zend-assertions", default)]
    zend_assertions: Option<String>,
    /// `warning-handler = "abort" | "null"` (ADR-0049 §7 amendment): what a proven
    /// `E_WARNING` *does* at runtime. Default (or absence) is `"abort"` — the
    /// owner-confirmed realistic-app assumption that a handler converts the warning to
    /// an exception / halts, so proven warning-grade offset findings emit. `"null"`
    /// declares the app tolerates the warning: those findings leave the proof surface
    /// and stay silent.
    #[serde(rename = "warning-handler", default)]
    warning_handler: Option<String>,
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

/// The `[transform.partitions]` section (ADR-0047 §7): declared observer globs and
/// the `[transform.partitions.sets]` name→glob-list table. Region assignment is a
/// pure function of these plus the file path ([`PartitionMap`]).
#[derive(serde::Deserialize, Default)]
struct PartitionsConfig {
    /// Observer path-sets (tests, dev-scripts; ADR-0047 §1).
    #[serde(default)]
    observers: Vec<String>,
    /// Partition name → glob list. A `BTreeMap` gives deterministic iteration;
    /// partitions are disjoint (validated), so order never affects assignment.
    #[serde(default)]
    sets: std::collections::BTreeMap<String, Vec<String>>,
}

/// Load the vouching valve from `steins.toml` (ADR-0046 §2). Reads `--config` when
/// given, else `./steins.toml` if it exists (a missing default file is silently
/// no vouches). Returns the [`VouchSet`] plus human warnings for a missing
/// explicit `--config`, a parse error, or a malformed `file:line` entry.
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

/// Read and parse `./steins.toml` once for `check`/`doctor` (ADR-0050 §7 /
/// ADR-0052 §5 N2). Returns:
///
/// * `Ok(None)` — no `steins.toml` (the built-in defaults govern);
/// * `Ok(Some(config))` — the parsed config;
/// * `Err(message)` — the file exists but does not parse. A malformed file,
///   INCLUDING an unknown key in `[runtime]` (`RuntimeConfig` uses
///   `deny_unknown_fields`), is a hard config error the caller maps to exit 2 —
///   never a warn-and-proceed. This is the one place `steins.toml` leniency is the
///   wrong default: a silently-ignored `zend-asertions` typo would leave the safe
///   runtime default in force while the user believed they had overridden it.
///
/// Transform's `--config` path keeps its own lenient loaders (`load_vouches` /
/// `load_partitions`, ADR-0046 §2): a vouch typo must not stop a transform run.
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

/// Derive the `[runtime]` pseudo-constants (ADR-0052 §5 / ADR-0049 §7) from the
/// already-parsed config. Returns `(zend_assertions, warning_handler_abort)` plus
/// human warnings for an unrecognized *value* on a known key (a parse error / unknown
/// key already exited 2 in [`read_steins_config`]). Absence is the safe default —
/// assertions off, `warning-handler = "abort"`.
fn runtime_from_config(runtime: Option<RuntimeConfig>) -> (bool, bool, Vec<String>) {
    let mut warnings = Vec::new();
    let runtime = runtime.unwrap_or_default();
    let zend_assertions = match runtime.zend_assertions.as_deref() {
        None | Some("disabled") => false,
        Some("enabled") => true,
        Some(other) => {
            warnings.push(format!(
                "steins.toml [runtime] zend-assertions: unknown value `{other}` (want \"enabled\"|\"disabled\"); using disabled"
            ));
            false
        }
    };
    // Default "abort": a proven E_WARNING is a proven runtime break (ADR-0049 §7).
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
    (zend_assertions, warning_handler_abort, warnings)
}

/// Derive the profile selection and user-profile table from the already-parsed
/// config (ADR-0050 §5). Returns the `[check] profile` selection (the repo default;
/// the `--profile` flag beats it) and the resolved [`profile::ProfileConfigs`] table.
/// The parse itself already happened in [`read_steins_config`] (a parse error exited
/// 2 there), so this is a pure decomposition.
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

/// Load the region map from `steins.toml [transform.partitions]` (ADR-0047 §7).
/// Reads `--config` when given, else `./steins.toml` if present.
///
/// Returns `Ok(None)` — the single-region identity — when the file is missing,
/// unparseable (the vouch loader already surfaces the parse warning), or carries
/// no `[transform.partitions]` section; a run with no partition config is then
/// byte-identical to today's behavior. Returns `Err` only for a genuine config
/// error: overlapping partition path-sets (which have no defined assignment).
fn load_partitions(config_path: Option<&str>) -> Result<Option<PartitionMap>, String> {
    let path = match config_path {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("steins.toml"),
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    // A parse error is already reported by `load_vouches`; proceed as identity here
    // rather than double-warning.
    let Ok(config) = toml::from_str::<SteinsConfig>(&text) else {
        return Ok(None);
    };
    let Some(partitions) = config.transform.and_then(|t| t.partitions) else {
        return Ok(None);
    };
    // An empty section (no sets, no observers) is still the identity.
    if partitions.sets.is_empty() && partitions.observers.is_empty() {
        return Ok(None);
    }
    PartitionMap::build(partitions.sets, partitions.observers)
        .map(Some)
        .map_err(|e| format!("steins.toml [transform.partitions]: {e}"))
}

/// The outcome of the dual-verification post-check: whether the edited project is
/// clean, plus any diagnostics whose per-id count *increased* after the edits.
struct PostCheck {
    ok: bool,
    new_diagnostics: Vec<Diagnostic>,
}

/// Re-analyze the project with the plan's edits applied and report any diagnostic
/// id whose count increased (ADR-0034 point 3a). Comparison is by per-id count so
/// it is robust to the line-number shifts a tag deletion causes; vendor findings
/// are filtered from both sides, matching `check`'s default (ADR-0015).
fn post_check(
    db: &SteinsDatabase,
    project: Project,
    report: &TransformReport,
    texts: &HashMap<String, String>,
) -> PostCheck {
    if report.plan.is_empty() {
        return PostCheck { ok: true, new_diagnostics: Vec::new() };
    }
    let before = filtered_diagnostics(check_project(db, project, &mut NoFold));

    // Build the edited project in a fresh database (avoids salsa mutation subtlety
    // and keeps the pre-edit query results intact for `before`).
    let edb = SteinsDatabase::default();
    let mut einputs: Vec<SourceFile> = Vec::new();
    for (path, original) in texts {
        let updated = report.plan.apply_file(path, original);
        einputs.push(SourceFile::new(&edb, path.clone(), updated));
    }
    let eproject = Project::new(&edb, einputs);
    let after = filtered_diagnostics(check_project(&edb, eproject, &mut NoFold));

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

fn filtered_diagnostics(mut ds: Vec<Diagnostic>) -> Vec<Diagnostic> {
    ds.retain(|d| !is_vendor_path(&d.path));
    ds
}

/// Render the transform dry-run/apply report as text: a unified diff per edited
/// file, then the refusals, the completeness-oracle summary, and the post-check
/// verdict.
fn print_transform_text(
    report: &TransformReport,
    postcheck: &PostCheck,
    texts: &HashMap<String, String>,
    action: &str,
) {
    for path in report.plan.edited_paths() {
        if let Some(original) = texts.get(path) {
            let updated = report.plan.apply_file(path, original);
            print!("{}", unified_diff(path, original, &updated, 3));
        }
    }

    // Project-global dynamic-code obstacles (ADR-0046 §2): recorded once, with the
    // site list capped in text output (the JSON carries every site).
    const OBSTACLE_SITE_CAP: usize = 5;
    if !report.obstacles.is_empty() {
        println!("\nDynamic-code obstacles ({}):", report.obstacles.len());
        for ob in &report.obstacles {
            println!("  [{}] {} — {} site(s):", ob.reason, ob.detail, ob.sites.len());
            for s in ob.sites.iter().take(OBSTACLE_SITE_CAP) {
                println!("    {}:{}:{}: {}", s.path, s.line, s.column, s.label);
            }
            if ob.sites.len() > OBSTACLE_SITE_CAP {
                println!("    … and {} more (see --format json)", ob.sites.len() - OBSTACLE_SITE_CAP);
            }
        }
    }

    if !report.refusals.is_empty() {
        println!("\nRefusals ({}):", report.refusals.len());
        for r in &report.refusals {
            println!(
                "  {}:{}:{}: {} [{}] — {}",
                r.site.path, r.site.line, r.site.column, r.site.label, r.reason, r.detail
            );
        }
    }

    let o = &report.oracle;
    println!("\n{} enumerated: {} {action}, {} refused", o.enumerated, o.transformed, o.refused);

    // The vouching downgrade (ADR-0046 §2 / ADR-0037): a run that vouched sites
    // does not silently pass — its completeness claim is conditional on those
    // user assertions, and the report says so prominently.
    if !report.vouched_exemptions.is_empty() {
        println!(
            "\nDOWNGRADE: completeness claim is conditional on {} user-vouched dynamic-code exemption(s):",
            report.vouched_exemptions.len()
        );
        for s in &report.vouched_exemptions {
            println!("    vouched {}:{}:{}: {}", s.path, s.line, s.column, s.label);
        }
    }

    if !postcheck.ok {
        println!("\nPost-check FAILED — {} new diagnostic(s):", postcheck.new_diagnostics.len());
        for d in &postcheck.new_diagnostics {
            println!("  {}:{}:{}: [{}] {}", d.path, d.line, d.column, d.id, d.message);
        }
    } else if !report.plan.is_empty() {
        println!("Post-check OK — no new diagnostics.");
    }
}

/// Render the transform report as JSON: the serializable [`TransformReport`]
/// (plan + refusals + oracle) plus the post-check verdict and whether the edits
/// were written.
fn print_transform_json(report: &TransformReport, postcheck: &PostCheck, applied: bool) {
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
    // The vouching downgrade (ADR-0046 §2): the `report` already serializes the
    // `obstacles` and `vouched_exemptions` arrays; surface the claim downgrade as a
    // prominent top-level note whenever any site was vouched.
    let downgrade_note = (!report.vouched_exemptions.is_empty()).then(|| {
        format!(
            "completeness claim is conditional on {} user-vouched dynamic-code exemption(s)",
            report.vouched_exemptions.len()
        )
    });
    let doc = serde_json::json!({
        "report": report,
        "postcheck": { "ok": postcheck.ok, "new_diagnostics": new_ds },
        "applied": applied,
        "downgrade_note": downgrade_note,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("steins: failed to serialize json: {e}"),
    }
}

/// `steins annotate [--no-php] <file.php>` — reprint one file with a right-margin
/// column of proven inferred facts (ADR-0020). Never modifies the file; output
/// goes to stdout. Exits 2 on a usage error (directory, missing/extra args).
fn run_annotate(args: &[String]) -> ExitCode {
    let mut no_php = false;
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
                    eprintln!("steins: --project requires a directory argument");
                    return ExitCode::from(2);
                };
                project_dir = Some(dir.clone());
                i += 2;
            }
            other if other.starts_with('-') => {
                eprintln!("steins: unknown flag `{other}` for annotate");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    let [path] = paths.as_slice() else {
        eprintln!(
            "steins: annotate takes exactly one file (usage: steins annotate [--no-php] [--project <dir>] <file.php>)"
        );
        return ExitCode::from(2);
    };
    let path = Path::new(path);
    if path.is_dir() {
        eprintln!("steins: annotate expects a single file, not a directory: {}", path.display());
        return ExitCode::from(2);
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            eprintln!("steins: cannot read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };

    // Same coverage posture as `check` (ADR-0004).
    if no_php {
        eprintln!("{SOUND_SUBSET_NOTICE}");
    }
    let db = SteinsDatabase::default();
    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };

    // The project context for cross-file facts (ADR-0015): the `--project`
    // directory, else the file's own directory. Every `.php` file under it is
    // parsed into one project so `annotate` sees cross-file resolution.
    let root = project_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")));

    let canon_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut project_files = Vec::new();
    collect_php_files(&root, &mut project_files);
    project_files.sort();
    project_files.dedup();

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

    // If the target file was not found under the root (e.g. an explicit path
    // outside the project dir), fall back to a one-file project.
    let facts = match target {
        Some(target_file) => {
            let project = Project::new(&db, inputs);
            annotate_project(&db, project, target_file, &mut folder)
        }
        None => {
            let input = SourceFile::new(&db, path.to_string_lossy().into_owned(), text.clone());
            annotate_file(&db, input, &mut folder)
        }
    };

    print!("{}", render_annotation(&text, &facts));
    ExitCode::SUCCESS
}

/// Render the annotated file: each source line reprinted verbatim, and lines
/// with a proven fact padded (to the longest line, capped at column 88) and
/// given a `//=>` margin. Multiple facts on one line join with `; `.
fn render_annotation(text: &str, facts: &[LineFact]) -> String {
    /// The column source lines are padded to before the margin (cap: longer
    /// lines simply get a single separating space).
    const CAP: usize = 88;
    const PREFIX: &str = "//=> ";

    let lines: Vec<&str> = text.lines().collect();

    // Group fact bodies by line, de-duplicating identical bodies, order-stable.
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
            // Pad up to `target`, then always exactly one separating space — so
            // margins align at column `target + 1`, and an over-long line (width
            // >= target) simply gets that single space.
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

#[allow(clippy::too_many_arguments)]
fn print_text(
    findings: &[Diagnostic],
    surface: &profile::Surface,
    vendor_suppressed: usize,
    suppressed: usize,
    baselined: usize,
    stale: usize,
    surface_notice: Option<&str>,
) {
    for d in findings {
        // The level distinction (ADR-0050 §7): fail-level prints `error[…]`,
        // warn-level (a profile `warn = [...]` demotion) prints `warning[…]`.
        let kind = match surface.level(d.id) {
            profile::Level::Fail => "error",
            profile::Level::Warn => "warning",
        };
        println!("{}:{}:{}: {kind}[{}]: {}", d.path, d.line, d.column, d.id, d.message);
    }
    // Suppression accounting (ADR-0022/0023/0015), each line printed only when
    // nonzero. Vendor is the first channel (ADR-0015), so it prints first.
    if vendor_suppressed > 0 {
        println!("{vendor_suppressed} findings in vendor suppressed (--vendor-diagnostics to show)");
    }
    if suppressed > 0 {
        println!("{suppressed} diagnostics suppressed by inline ignores");
    }
    if baselined > 0 {
        println!("{baselined} findings in baseline");
    }
    if stale > 0 {
        println!("{stale} baseline entries no longer match (stale — rerun --set-baseline)");
    }
    // The drowns-loudly notice (ADR-0050 §8), printed after the accounting.
    if let Some(notice) = surface_notice {
        println!("{notice}");
    }
}

fn print_json(
    findings: &[Diagnostic],
    surface: &profile::Surface,
    vendor_suppressed: usize,
    suppressed: usize,
    baselined: usize,
) {
    let array: Vec<serde_json::Value> = findings
        .iter()
        .map(|d| {
            let mut obj = serde_json::json!({
                "id": d.id,
                // ADR-0050 §2: the diagnostic layer, additive. Every emitted id is
                // registered (totality test), so this is always present.
                "layer": steins_infer::layer(d.id).map(steins_infer::Layer::as_str),
                // ADR-0050 §7: the exit level (`fail|warn`), additive.
                "level": surface.level(d.id).as_str(),
                "path": d.path,
                "line": d.line,
                "column": d.column,
                "message": d.message,
            });
            // ADR-0050 §4: the registry-declared facet, additive — present as its own
            // key (`"origin": "direct"|"propagated"`) only on ids that declare one.
            if let Some(facet) = d.facet {
                obj[facet.key()] = serde_json::Value::String(facet.value().to_owned());
            }
            obj
        })
        .collect();
    let doc = serde_json::json!({
        "findings": array,
        "profile": surface.name,
        "vendor_suppressed": vendor_suppressed,
        "suppressed": suppressed,
        "baselined": baselined,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("steins: failed to serialize json: {e}"),
    }
}

/// Recursively collect `.php` files under `path` (or `path` itself if it is a
/// `.php` file).
fn collect_php_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else { return };
        for entry in entries.flatten() {
            collect_php_files(&entry.path(), out);
        }
    } else if path.extension().is_some_and(|e| e == "php") {
        out.push(path.to_path_buf());
    }
}
