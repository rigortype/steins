//! `steins doctor` (ADR-0054 Part II).
//!
//! Index-bound posture mirror (ADR-0054 §8): reads config, the environment
//! (sidecar `env()`), and index-level facts into a plain sectioned report.
//! Never runs a diagnostic emitter — "doctor asks what the world is; check
//! asks what is wrong" — so its exit never depends on what `check` finds.
//!
//! # Exit semantics (ADR-0054 §10)
//!
//! * **0** — degraded postures included (no reachable PHP, monkey-patch
//!   extensions, dormant baseline, catalog skew) — loud but exit-neutral
//!   (ADR-0004 crying-wolf prohibition).
//! * **1** — configuration contradiction: unparseable `steins.toml`, a
//!   profile-resolution error, unparseable baseline, or a violated/unknown
//!   `[doctor] require` assertion (§14).
//! * **2** — usage errors: unknown flag, second path, bad `--baseline`/
//!   `--format` argument, or (§10 amendment) a missing path.
//!
//! # Scope
//!
//! Nine sections, ADR-0054 §9 order plus C4's additions: Runtime, Config +
//! active surface, Layout (ADR-0015), Coverage posture (dam stats, opaque
//! constructs, reflected class world — issue #269), Envelopes (G1-demote
//! notice, §9.4), Baseline, Catalog (A11 pin skew), Registry totality,
//! Require (§14). Two C4 lines are not rendered: dump-site count (ADR-0053
//! §13, unlanded D3/D4 recognizer) and `contract_touches_class`'s project
//! count (ADR-0049 §11, blocked by issue #268's ban on a second inference
//! pipeline); both land with their recognizer.
//!
//! `--format json` (§14) renders the identical `Vec<Section>` built once, so
//! text/json invariance is structural (see [`render_json`]).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::walk::{LinkSkip, SkippedLink};
use steins_db::{PhpTarget, ProjectLayout};
use steins_infer::{
    ALL_EMITTABLE_IDS, DIAGNOSTIC_REGISTRY, DamKind, FileUnit, LazyTree, MONKEY_PATCH_EXTENSIONS,
    REGISTERED_NOT_YET_EMITTED, SAPI_PROVIDED_FUNCTIONS_EXACT, SAPI_PROVIDED_FUNCTION_PREFIXES,
    SOUND_SUBSET_NOTICE, THROW_UNDECLARED_ID, dam_facts,
};
use steins_phpdoc::{TagKind, scan_docblock};
use steins_sidecar::Sidecar;
use steins_syntax::{OpaqueConstruct, ReflectionKind, SourceTree};

use crate::baseline;
use crate::profile;

/// One parsed file of the scanned tree — doctor's whole index (ADR-0054 §8: no
/// emitter, no inference). Parsed once in [`run_doctor`], shared by every section.
struct ParsedFile {
    path: String,
    tree: SourceTree,
}

/// One report section (ADR-0054 §9): a title plus formatted body lines. Both
/// renderers walk the same `Vec<Section>`, which is the JSON schema too (§14).
struct Section {
    name: &'static str,
    lines: Vec<String>,
}

impl Section {
    fn new(name: &'static str) -> Self {
        Self { name, lines: Vec::new() }
    }
}

/// Push one formatted line onto a [`Section`]'s body.
macro_rules! line {
    ($sec:expr, $($arg:tt)*) => {
        $sec.lines.push(format!($($arg)*))
    };
}

/// `steins doctor [--no-php] [--baseline <path>] [--format text|json] [path]`
/// (default `path` = `.`, default format `text`).
pub fn run_doctor(args: &[String]) -> ExitCode {
    let mut no_php = false;
    let mut baseline_path: Option<String> = None;
    let mut format_json = false;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-php" => {
                no_php = true;
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
                    "text" => format_json = false,
                    "json" => format_json = true,
                    other => {
                        errln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other if other.starts_with('-') => {
                errln!("steins: unknown flag `{other}` for doctor");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }
    let root = match paths.as_slice() {
        [] => PathBuf::from("."),
        [p] => PathBuf::from(p),
        _ => {
            errln!(
                "steins: doctor takes at most one path (usage: steins doctor [--no-php] [--baseline <path>] [--format text|json] [path])"
            );
            return ExitCode::from(2);
        }
    };
    // Missing path = usage error, exit 2 (§10 amendment).
    if let Err(code) = crate::reject_missing_paths(&paths) {
        return code;
    }

    let mut contradiction = false; // flips to exit 1 on a config contradiction (§10)

    let banner = "steins doctor — posture report (index-bound; runs no checks)";

    // One parse, one layout discovery; same `resolve_layout` every surface uses (#181).
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let layout = crate::resolve_layout(std::slice::from_ref(&root.to_string_lossy().into_owned()));
    let (files, skipped_links) = parse_project(&root);

    let mut sections: Vec<Section> = Vec::new();

    // The live child outlives its section, reused by Coverage posture (#269).
    let (runtime_section, mut runtime_facts) = section_runtime(no_php, &layout);
    sections.push(runtime_section);

    let (config_section, config) = section_config(&mut contradiction);
    sections.push(config_section);

    sections.push(section_layout(&root, &cwd, &layout, &skipped_links));
    sections.push(section_coverage(
        &root,
        &files,
        &layout,
        runtime_facts.sidecar_ok,
        config.vouch_sites,
        runtime_facts.sidecar.as_mut(),
    ));
    sections.push(section_envelopes(&files, &config.surface));

    let (baseline_section, dormant_count) =
        section_baseline(baseline_path.as_deref(), &config.surface, &mut contradiction);
    sections.push(baseline_section);

    let (catalog_section, catalog_skew) =
        section_catalog(layout.php_target(), runtime_facts.runtime_minor);
    sections.push(catalog_section);

    sections.push(section_registry());

    let require_facts = RequireFacts {
        sidecar_ok: runtime_facts.sidecar_ok,
        catalog_skew,
        monkey_patch_present: runtime_facts.monkey_patch_present,
        dormant_count,
    };
    sections.push(section_require(config.require, &require_facts, &mut contradiction));

    let exit_code: u8 = u8::from(contradiction);
    if format_json {
        render_json(banner, &sections, exit_code);
    } else {
        render_text(banner, &sections);
    }

    if contradiction {
        // FAILURE == 1: the config-contradiction code (ADR-0054 §10).
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Print each section: blank line, name, body lines verbatim.
fn render_text(banner: &str, sections: &[Section]) {
    outln!("{banner}");
    for s in sections {
        outln!();
        outln!("{}", s.name);
        for l in &s.lines {
            outln!("{l}");
        }
    }
}

/// Render `sections` as JSON (ADR-0054 §14): one object per section, lines
/// trimmed of the text renderer's leading-space indentation.
///
/// ```json
/// {
///   "schema": "steins.doctor/v1",
///   "banner": "...",
///   "exit_code": 0,
///   "sections": [{"name": "Runtime", "lines": ["..."]}, ...]
/// }
/// ```
///
/// Structure is fixed, content is not (same format-invariance discipline as
/// `check`, ADR-0054 point 1). `schema` is versioned for future reshapes.
fn render_json(banner: &str, sections: &[Section], exit_code: u8) {
    let doc = serde_json::json!({
        "schema": "steins.doctor/v1",
        "banner": banner,
        "exit_code": exit_code,
        "sections": sections
            .iter()
            .map(|s| serde_json::json!({
                "name": s.name,
                "lines": s.lines.iter().map(|l| l.trim().to_owned()).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });
    outln!("{}", serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_owned()));
}

/// Facts the Runtime section computes that later sections (Catalog, Require)
/// also need; doctor spawns the sidecar at most once per run.
struct RuntimeFacts {
    /// The sidecar spawned AND `env()` succeeded — the `"sidecar"` [`RequireFacts`] leg.
    sidecar_ok: bool,
    /// The sidecar-reported `(major, minor)`, when the round-trip succeeded.
    runtime_minor: Option<(u16, u16)>,
    /// Whether a monkey-patch extension (ADR-0049 A9) is loaded.
    monkey_patch_present: bool,
    /// The live child, reused by Coverage posture (issue #269). `None` when degraded.
    sidecar: Option<Sidecar>,
}

/// Section 1 — Runtime (ADR-0054 §9.1): sidecar health, PHP version, SAPI,
/// extension count, monkey-patch line (A9), A6 SAPI-undeclared line, and
/// analysis TARGET skew against the runtime (issue #28).
fn section_runtime(no_php: bool, layout: &ProjectLayout) -> (Section, RuntimeFacts) {
    let mut sec = Section::new("Runtime");
    let target = layout.php_target();
    if no_php {
        line!(sec, "  PHP sidecar: disabled (--no-php)");
        section_target(&mut sec, target, None);
        line!(sec, "  posture: sound subset — findings that require executing PHP are omitted");
        line!(sec, "  (a degraded environment is not a failure — exit stays 0, ADR-0004)");
        section_sapi_notice(&mut sec);
        return (
            sec,
            RuntimeFacts {
                sidecar_ok: false,
                runtime_minor: None,
                monkey_patch_present: false,
                sidecar: None,
            },
        );
    }
    let facts = match Sidecar::spawn() {
        Ok(mut sc) => match sc.env() {
            Some(env) => {
                line!(sec, "  PHP sidecar: spawned ok");
                line!(sec, "  PHP version: {}", env.php_version);
                line!(sec, "  SAPI: {}", env.sapi);
                line!(sec, "  loaded extensions: {}", env.extensions.len());
                // Fold lane's integer-width gate (#64): width decides allowlist size;
                // refused/unverified counts differ only in Catalog.
                match env.int_size {
                    Some(8) => line!(
                        sec,
                        "  integer width: 8 bytes — the whole foldable allowlist is admitted"
                    ),
                    Some(4) => line!(
                        sec,
                        "  integer width: 4 bytes — only the {} portable name(s) of the foldable allowlist fold; the other {} decline (issue #64)",
                        steins_catalog::portable_names().len(),
                        steins_catalog::foldable_entry_count()
                            - steins_catalog::portable_names().len()
                    ),
                    Some(n) => line!(
                        sec,
                        "  integer width: {n} bytes — no fold lane is verified against this machine, so nothing folds (default-deny)"
                    ),
                    None => line!(
                        sec,
                        "  integer width: unreported — not provably 64-bit, so nothing folds (default-deny; a runner predating the field)"
                    ),
                }
                // Monkey-patch presence (ADR-0049 A9): a loaded `uopz`/`runkit7`/
                // `Componere` silently voids the entire absence-proof family.
                let present: Vec<&str> = env
                    .extensions
                    .iter()
                    .filter(|e| MONKEY_PATCH_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m)))
                    .map(String::as_str)
                    .collect();
                let monkey_patch_present = !present.is_empty();
                if monkey_patch_present {
                    line!(
                        sec,
                        "  monkey-patch extension(s) loaded: {} — the entire absence-proof family is Unknown-silent this run (ADR-0049 A9)",
                        present.join(", ")
                    );
                }
                let runtime_minor = parse_env_minor(&env.php_version);
                section_target(&mut sec, target, runtime_minor);
                RuntimeFacts {
                    sidecar_ok: true,
                    runtime_minor,
                    monkey_patch_present,
                    sidecar: Some(sc),
                }
            }
            None => {
                line!(sec, "  PHP sidecar: spawned, but the env() query failed");
                line!(
                    sec,
                    "  posture: sound subset (degraded) — findings that require executing PHP are omitted (exit 0, ADR-0004)"
                );
                RuntimeFacts {
                    sidecar_ok: false,
                    runtime_minor: None,
                    monkey_patch_present: false,
                    sidecar: None,
                }
            }
        },
        Err(_) => {
            line!(sec, "  PHP sidecar: not spawnable (no `php` on PATH)");
            line!(sec, "  {SOUND_SUBSET_NOTICE}");
            line!(sec, "  (a degraded environment is not a failure — exit stays 0, ADR-0004)");
            RuntimeFacts {
                sidecar_ok: false,
                runtime_minor: None,
                monkey_patch_present: false,
                sidecar: None,
            }
        }
    };
    section_sapi_notice(&mut sec);
    (sec, facts)
}

/// The A6 SAPI-existence-oracle line (ADR-0049 A6, ADR-0054 §9.1): `[runtime]
/// sapi` is deferred-with-design and undeclared every run. Printed even under
/// `--no-php`: depends on the project's SAPI declaration, not on the sidecar.
fn section_sapi_notice(sec: &mut Section) {
    let mut names: Vec<String> = SAPI_PROVIDED_FUNCTIONS_EXACT.iter().map(|s| (*s).to_owned()).collect();
    names.extend(SAPI_PROVIDED_FUNCTION_PREFIXES.iter().map(|p| format!("{p}*")));
    names.sort();
    line!(
        sec,
        "  [runtime] sapi: undeclared (deferred-with-design, ADR-0049 A6) — {} are never reported Absent this run",
        names.join(", ")
    );
}

/// Section 2 — Config + active surface (ADR-0054 §9.3/§9.4): resolved surface
/// plus config-derived facts other sections need. Unparseable `steins.toml`
/// or profile-resolution error is a contradiction (exit 1); still renders default.
struct ConfigOutcome {
    surface: profile::Surface,
    /// `[transform.vouch] sites` count, read here to avoid a second toml parse.
    vouch_sites: usize,
    /// `[doctor] require` as declared; validated later in [`section_require`].
    require: Vec<String>,
}

fn section_config(contradiction: &mut bool) -> (Section, ConfigOutcome) {
    let mut sec = Section::new("Config + active surface");

    let config = match crate::read_steins_config() {
        Ok(c) => c,
        Err(e) => {
            line!(sec, "  steins.toml: PARSE ERROR — {e}");
            line!(sec, "  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
            *contradiction = true;
            None
        }
    };
    let (check_cfg, profile_tbl, runtime_cfg, vouch_sites, require) = match config {
        Some(c) => {
            line!(sec, "  steins.toml: found");
            let vouch_sites = c.transform.as_ref().and_then(|t| t.vouch.as_ref()).map_or(0, |v| v.sites.len());
            let require = c.doctor.map(|d| d.require).unwrap_or_default();
            (c.check, c.profile, c.runtime, vouch_sites, require)
        }
        None => {
            // Genuine absence, not the parse-error fallback (already printed).
            if !*contradiction {
                line!(sec, "  steins.toml: not found (built-in defaults govern)");
            }
            (None, None, None, 0, Vec::new())
        }
    };

    section_runtime_postures(&mut sec, runtime_cfg);

    let (config_profile, profile_configs) = crate::profiles_from_config(check_cfg, profile_tbl);
    let provenance = if config_profile.is_some() { "[check] profile" } else { "built-in default" };
    let surface = match profile_configs.resolve(config_profile.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            line!(sec, "  profile resolution: ERROR — {e}");
            line!(sec, "  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
            *contradiction = true;
            // Fall back to the default surface so remaining sections still render.
            profile::ProfileConfigs::default()
                .resolve(None)
                .expect("the built-in default profile always resolves")
        }
    };
    line!(sec, "  active profile: `{}` (from {provenance})", surface.name);
    let layers = surface.layers_on();
    line!(
        sec,
        "  surface: layers [{}], {} checked id(s)",
        layers.join(", "),
        surface.surface_ids().len()
    );
    (sec, ConfigOutcome { surface, vouch_sites, require })
}

/// The `[runtime]` pseudo-constant lines of the Config section (ADR-0037 §2).
/// Both keys print every run tagged with their source (declared/default), so
/// "never declared" and "declared but didn't take" stay distinguishable.
fn section_runtime_postures(sec: &mut Section, runtime_cfg: Option<crate::RuntimeConfig>) {
    // Read before consuming config; resolved value alone can't tell absent from default.
    let declared = |v: &Option<String>| if v.is_some() { "declared" } else { "default" };
    let (wh_src, fk_src) = match &runtime_cfg {
        Some(r) => (declared(&r.warning_handler), declared(&r.final_keyword)),
        None => ("default", "default"),
    };
    let (postures, warnings) = crate::runtime_from_config(runtime_cfg);

    let (wh, wh_note) = if postures.warning_handler_abort {
        ("abort", "a proven E_WARNING is a proven break, so warning-grade ids fire")
    } else {
        ("null", "the app tolerates the warning, so warning-grade ids leave the proof surface")
    };
    line!(sec, "  [runtime] warning-handler: \"{wh}\" ({wh_src}) — {wh_note}");

    let (fk, fk_note) = match postures.final_keyword {
        steins_infer::FinalKeyword::Enforced => (
            "enforced",
            "`final` seals a class, so an intersection carrying a final arm is uninhabited",
        ),
        steins_infer::FinalKeyword::Stripped => (
            "stripped",
            "the analyzed runtime rewrites `final` away, so `FinalClass&MockObject` stays inhabited; `readonly` and the `final` diagnostics are unaffected",
        ),
    };
    line!(sec, "  [runtime] final-keyword: \"{fk}\" ({fk_src}) — {fk_note}");

    for w in warnings {
        line!(sec, "  {w}");
    }
}

/// Runtime section's TARGET lines (issue #28): declared range, its source, and
/// the skew against the runtime when one answered.
fn section_target(sec: &mut Section, target: Option<&PhpTarget>, runtime_minor: Option<(u16, u16)>) {
    match target {
        None => {
            line!(sec, "  analysis target: none declared — the runtime PHP is the target");
        }
        Some(t) => {
            line!(
                sec,
                "  analysis target: PHP {} (from {} \"{}\")",
                t.render(),
                t.source.as_str(),
                t.raw
            );
            if let Some(m) = runtime_minor {
                if !t.contains(m) {
                    line!(
                        sec,
                        "  version skew: runtime {}.{} is OUTSIDE the declared range — the absence family and reflection-seeded facts are disabled this run (the boot surface is not a version this project ships on)",
                        m.0, m.1
                    );
                } else if t.floor < m {
                    line!(
                        sec,
                        "  version skew: runtime {}.{} sits above the {}.{} floor — reflection describes the runtime, so symbols newer than the floor are not proven absent for it (silence, never a false claim)",
                        m.0, m.1, t.floor.0, t.floor.1
                    );
                }
            }
        }
    }
}

/// `(major, minor)` from the sidecar's version report, for the skew line.
fn parse_env_minor(v: &str) -> Option<(u16, u16)> {
    let mut it = v.split('.');
    let major = it.next()?.parse().ok()?;
    let minor: u16 =
        it.next()?.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?;
    Some((major, minor))
}

/// Section 3 — Layout (ADR-0015): which trees this run treats as vendor,
/// deciding whether a finding is reported or a declaration is a transform
/// candidate. Resolved from `composer.json`, falling back to the `vendor`
/// directory-name floor when no manifest governs.
fn section_layout(
    root: &Path,
    cwd: &Path,
    layout: &ProjectLayout,
    skipped: &[SkippedLink],
) -> Section {
    let mut sec = Section::new("Layout");
    if layout.is_fallback() {
        line!(
            sec,
            "  no composer.json governs {} — vendor is the `vendor` directory-name floor, not a declared fact",
            root.display()
        );
    } else {
        line!(sec, "  {} manifest(s) govern this tree:", layout.roots().len());
        for r in layout.roots() {
            line!(sec, "    {}", display_path(cwd, r.manifest()));
            line!(sec, "      vendor: {}", join_paths(cwd, r.vendor_roots()));
            line!(sec, "      ours:   {}", join_paths(cwd, r.first_party_roots()));
        }
    }
    skipped_links_lines(&mut sec, cwd, skipped);
    sec
}

/// Where the walk stopped (issue #524). A directory symlink is not followed —
/// one out of the tree names code this run was never asked about, one back into
/// it counts the same files twice — so what it would have reached is *not*
/// analyzed, and an unreported omission is how that goes unnoticed for a
/// release series. Silent when there is nothing to report: a project with one
/// symlinked vendor directory should not read as if something were wrong.
fn skipped_links_lines(sec: &mut Section, cwd: &Path, skipped: &[SkippedLink]) {
    if skipped.is_empty() {
        return;
    }
    /// Named individually; the rest are counted. Enough to recognize the tree.
    const SHOWN: usize = 5;
    let escaping = skipped.iter().filter(|s| s.reason == LinkSkip::Escapes).count();
    line!(
        sec,
        "  {} path(s) skipped as symlinks ({} leaving the analyzed tree, {} re-entering it) — nothing under them was analyzed:",
        skipped.len(),
        escaping,
        skipped.len() - escaping
    );
    for s in skipped.iter().take(SHOWN) {
        line!(sec, "    {} — {}", display_path(cwd, &s.path), s.reason.reason());
    }
    if skipped.len() > SHOWN {
        line!(sec, "    … and {} more", skipped.len() - SHOWN);
    }
}

/// Section 4 — Coverage posture (ADR-0054 §9.2): what this run parsed and then
/// declined to reason about — the crying-wolf-required measurement of a quiet
/// analyzer (`Scope::poisoned` marks eval-affected locals unknown, ADR-0046
/// §1). None of these is a diagnostic (no registry id, baseline entry, or
/// fp-gate counter):
///
/// 1. Poisoned scopes, share of all, by construct kind (`Scope::opaque`).
/// 2. Dam sites (ADR-0049 §2) — could a name exist the reference scan never saw.
/// 3. Reflection-driven invocation sites — a labelled guess.
/// 4. The reflected class world (issue #269), off the project's own PHP via
///    the engine Runtime spawned ([`section_reflected_classes`]).
/// 5. The sound-subset id list (A2(ii)) — only when no sidecar answered.
/// 6. The vendor posture (ADR-0015) — static, printed always.
/// 7. The `[transform.vouch]` count (ADR-0046 §2) — not yet dam-consulted (`dam.rs`).
fn section_coverage(
    root: &Path,
    files: &[ParsedFile],
    layout: &ProjectLayout,
    sidecar_ok: bool,
    vouch_sites: usize,
    sidecar: Option<&mut Sidecar>,
) -> Section {
    let mut sec = Section::new("Coverage posture");
    if files.is_empty() {
        line!(sec, "  no .php files under {} — nothing to inventory", root.display());
        return sec;
    }

    let mut scopes = 0usize;
    let mut poisoned = 0usize;
    let mut constructs = [0usize; OpaqueConstruct::ALL.len()];
    let mut reflection = [0usize; ReflectionKind::ALL.len()];
    for f in files {
        // Counted per construct, not per affected scope: `use (&$x)` sits on both
        // scopes (ADR-0033); double-counting misanswers "grep for this".
        let mut seen = std::collections::HashSet::new();
        for scope in f.tree.scopes() {
            scopes += 1;
            if scope.poisoned {
                poisoned += 1;
            }
            for site in &scope.opaque {
                if !seen.insert(*site) {
                    continue;
                }
                if let Some(i) = OpaqueConstruct::ALL.iter().position(|c| *c == site.construct) {
                    constructs[i] += 1;
                }
            }
        }
        for site in f.tree.reflection_sites() {
            if let Some(i) = ReflectionKind::ALL.iter().position(|k| *k == site.kind) {
                reflection[i] += 1;
            }
        }
    }

    line!(
        sec,
        "  {} file(s), {scopes} scope(s), {poisoned} poisoned ({}) — a poisoned scope knows no local's value (ADR-0001, ADR-0046 §1)",
        files.len(),
        share(poisoned, scopes)
    );
    let construct_total: usize = constructs.iter().sum();
    if construct_total == 0 {
        line!(sec, "  opaque constructs: none — no scope is on the give-up list");
    } else {
        line!(
            sec,
            "  opaque constructs: {construct_total} site(s) — {}",
            breakdown(&constructs, OpaqueConstruct::ALL.map(OpaqueConstruct::label))
        );
    }

    // The dam (ADR-0049 §2): same answer `check` computes, recomputed here,
    // independent of the counts above (`class_alias` dams without poisoning a scope).
    let lazy: Vec<LazyTree<'_>> = files.iter().map(|f| LazyTree::borrowed(&f.tree)).collect();
    let units: Vec<FileUnit<'_>> =
        files.iter().zip(&lazy).map(|(f, tree)| FileUnit { path: &f.path, tree }).collect();
    let dam = dam_facts(&units, layout);
    if dam.is_empty() {
        line!(
            sec,
            "  dam sites: none — no runtime-definition construct stands, so existence-absence claims are undammed (ADR-0049 §2)"
        );
    } else {
        let mut dam_counts = [0usize; 5];
        for site in dam.sites() {
            let i = match site.kind {
                DamKind::Eval => 0,
                DamKind::Include => 1,
                DamKind::ClassAlias => 2,
                DamKind::Unparsable => 3, // parse failure (ADR-0079, issue #180)
                DamKind::DefineDynamic => 4, // global constants (ADR-0078, issue #198)
            };
            dam_counts[i] += 1;
        }
        line!(
            sec,
            "  dam sites: {} — {}",
            dam.len(),
            breakdown(
                &dam_counts,
                [
                    "eval",
                    "unproven/out-of-universe include",
                    "runtime-name class_alias",
                    "unparsable file",
                    "runtime-name define",
                ]
            )
        );
        // Name valve closes only for a kind that can mint a name (ADR-0078, #198).
        if dam.is_clear() {
            line!(
                sec,
                "    existence-absence claims (undefined function/class) still stand — no site here can mint a function or class name"
            );
        } else {
            line!(
                sec,
                "    existence-absence claims (undefined function/class) stay silent where these stand (ADR-0049 §2)"
            );
        }
        // Every dam kind closes the constant valve too (ADR-0078, #198).
        line!(
            sec,
            "    `constant.undefined` stays silent where any of these stand — a runtime-name define is a constant-only dam"
        );
    }

    let reflection_total: usize = reflection.iter().sum();
    if reflection_total == 0 {
        line!(sec, "  reflection-driven invocation: none recognized");
    } else {
        line!(
            sec,
            "  reflection-driven invocation: {reflection_total} site(s) — {}",
            breakdown(&reflection, ReflectionKind::ALL.map(ReflectionKind::label))
        );
    }
    // Stated even on 0: reads as "recognizer saw nothing", not "code doesn't reflect".
    line!(
        sec,
        "    (this list is a guess until measured: the recognizer is syntactic, it names no receiver type, and it is not exhaustive)"
    );

    section_reflected_classes(&mut sec, files, sidecar);

    // Sound-subset id list (ADR-0054 §9.2, A2(ii)): only when no sidecar answered.
    if !sidecar_ok {
        line!(
            sec,
            "  sound subset: no PHP sidecar this run — `call.undefined-function`, `class.undefined`, and `call.undefined-method` (homonym leg) are silenced (ADR-0004, A2(ii))"
        );
    }

    // Vendor posture (ADR-0015): a policy fact, not a per-project measurement.
    line!(sec, "  vendor posture: findings under a vendor root are suppressed by default (ADR-0015)");

    // `[transform.vouch]` count (ADR-0046 §2 / ADR-0049 §4); dam.rs's dam does
    // not consult it yet.
    if vouch_sites == 0 {
        line!(
            sec,
            "  vouched dynamic-code exemptions: none declared ([transform.vouch] in steins.toml)"
        );
    } else {
        line!(
            sec,
            "  vouched dynamic-code exemptions: {vouch_sites} site(s) declared ([transform.vouch]) — consulted by `transform` only; the checker's dam does not yet honor vouches (ADR-0046 §2's checker-side vouch valve is deferred, dam.rs)"
        );
    }

    sec
}

/// Cap on distinct unanswered class names put to the engine (one round trip
/// apiece); `check` has no such cap.
const REFLECT_QUERY_CAP: usize = 200;

/// How many resolved names print before the line summarizes the rest.
const REFLECT_DISPLAY_CAP: usize = 8;

/// The reflected class world (issue #269): class names this tree references
/// that neither a declaration nor the builtin catalog answers, but the
/// project's own PHP does (e.g. `Redis` — ADR-0049 §1, ask the real thing).
/// Printed only when a live engine answered; convicts nothing (ADR-0054 §8;
/// see `steins_infer::Folder::reflected_class`).
fn section_reflected_classes(sec: &mut Section, files: &[ParsedFile], sidecar: Option<&mut Sidecar>) {
    let Some(sc) = sidecar else {
        return;
    };

    // Every class-like the project declares, keyed like the index; never asked.
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in files {
        for cd in f.tree.classes() {
            declared.insert(cd.fqn.clone());
        }
    }

    // Unanswered names, deduped in first-encounter order; lowercased (case-insensitive).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unanswered: Vec<String> = Vec::new();
    for f in files {
        for r in f.tree.hard_class_refs() {
            let fqn = f.tree.resolve_class_fqn(r).to_ascii_lowercase();
            if declared.contains(&fqn) || steins_catalog::builtin_class_supers(&fqn).is_some() {
                continue;
            }
            if seen.insert(fqn.clone()) {
                unanswered.push(fqn);
            }
        }
    }
    if unanswered.is_empty() {
        line!(
            sec,
            "  reflected class world: no unanswered class-like referenced — the project index and the builtin catalog cover this tree"
        );
        return;
    }

    let asked = unanswered.len().min(REFLECT_QUERY_CAP);
    let mut resolved: Vec<String> = Vec::new();
    for fqn in unanswered.iter().take(asked) {
        // A decline and a not-found are both "not resolved here".
        let Some(reflection) = sc.reflect_class(fqn) else {
            continue;
        };
        if let Some(decl) = reflection.declaration {
            let origin = decl.extension.unwrap_or_else(|| {
                if decl.internal { "engine".to_owned() } else { "runtime".to_owned() }
            });
            resolved.push(format!("{} ({origin})", decl.name));
        }
    }

    let truncated = if asked < unanswered.len() {
        format!(" (asked {asked} of {} distinct names)", unanswered.len())
    } else {
        String::new()
    };
    if resolved.is_empty() {
        line!(
            sec,
            "  reflected class world: none of {} unanswered class-like name(s) is resident on this PHP{truncated}",
            unanswered.len()
        );
        return;
    }
    let shown = resolved.len().min(REFLECT_DISPLAY_CAP);
    let more = resolved.len() - shown;
    let tail = if more == 0 { String::new() } else { format!(", +{more} more") };
    line!(
        sec,
        "  reflected class world: {} of {} unanswered class-like name(s) resolved off the project's own PHP{truncated} — {}{tail}",
        resolved.len(),
        unanswered.len(),
        resolved[..shown].join(", ")
    );
    line!(
        sec,
        "    (a reflected declaration restores coverage only: it is the runtime's own claim, and no absence finding is premised on it — issue #269)"
    );
}

/// `poisoned/total` as a percentage, or `n/a` for an empty denominator.
fn share(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "n/a".to_owned();
    }
    #[expect(clippy::cast_precision_loss, reason = "a display percentage; counts are small")]
    let pct = part as f64 * 100.0 / whole as f64;
    format!("{pct:.1}%")
}

/// `label N` pairs for non-zero counts, comma-joined. Zero counts are dropped
/// as noise; the total is already on the line.
fn breakdown<const N: usize>(counts: &[usize; N], labels: [&str; N]) -> String {
    counts
        .iter()
        .zip(labels)
        .filter(|(n, _)| **n > 0)
        .map(|(n, label)| format!("{label} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse every `.php` file under `root` once, and hand back what the walk
/// refused (issue #524) for the Layout section to report. Unreadable files are
/// skipped silently; a recovered parse tree still carries scopes and sites
/// (ADR-0003).
fn parse_project(root: &Path) -> (Vec<ParsedFile>, Vec<SkippedLink>) {
    let sources = crate::collect_sources(std::slice::from_ref(&root.to_path_buf()));
    let parsed = sources
        .files
        .iter()
        .filter_map(|file| {
            let bytes = std::fs::read(file).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            Some(ParsedFile {
                path: file.to_string_lossy().into_owned(),
                tree: SourceTree::parse(&text),
            })
        })
        .collect();
    (parsed, sources.skipped_links)
}

/// Path relative to `cwd` when underneath it, else absolute.
fn display_path(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd).unwrap_or(p).display().to_string()
}

/// Comma-joined root list, or `none declared` for an empty one.
fn join_paths(cwd: &Path, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "none declared".to_owned();
    }
    paths.iter().map(|p| display_path(cwd, p)).collect::<Vec<_>>().join(", ")
}

/// Section 5 — Envelopes (ADR-0054 §9.4, G1-amendment written-but-unchecked
/// notice). Index scan, never the checker: count declarations with a written
/// `@throws`, state whether the active surface checks them.
fn section_envelopes(files: &[ParsedFile], surface: &profile::Surface) -> Section {
    let mut sec = Section::new("Envelopes");
    let n = count_throws_envelopes(files);
    let checked = surface.surfaces_id(THROW_UNDECLARED_ID);
    if checked {
        line!(
            sec,
            "  {n} declaration(s) carry a written @throws — the active profile `{}` checks them (throw.undeclared on surface)",
            surface.name
        );
    } else {
        line!(
            sec,
            "  {n} written throw envelope(s); the active profile `{}` does not check them — the `contracts` (or `throws-direct`) profile does",
            surface.name
        );
    }
    sec
}

/// Count declarations (functions + methods) carrying a written `@throws` tag.
/// Index-bound: reads parsed source, runs no inference.
fn count_throws_envelopes(files: &[ParsedFile]) -> usize {
    let mut count = 0usize;
    for file in files {
        for f in file.tree.functions() {
            if declares_throws(f.docblock.as_deref()) {
                count += 1;
            }
        }
        for c in file.tree.classes() {
            for m in &c.methods {
                if declares_throws(m.docblock.as_deref()) {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Whether a docblock carries at least one `@throws` tag (ADR-0040 written envelope).
fn declares_throws(docblock: Option<&str>) -> bool {
    docblock.is_some_and(|d| scan_docblock(d).iter().any(|t| t.kind == TagKind::Throws))
}

/// Section 6 — Baseline (ADR-0054 §9.5): capture surface versus active
/// surface, and dormant-entry count (id outside active — kept, not stale).
/// Accepts `--baseline <path>`, else the default file, else "none";
/// unparseable = configuration contradiction (exit 1, §10). Returns the
/// dormant count (`0` absent) for `"no-dormant-baseline"` in [`RequireFacts`].
fn section_baseline(
    cli_path: Option<&str>,
    surface: &profile::Surface,
    contradiction: &mut bool,
) -> (Section, usize) {
    let mut sec = Section::new("Baseline");

    // Explicit `--baseline` wins; else the same default file `check` auto-loads.
    let file: Option<PathBuf> = match cli_path {
        Some(p) => Some(PathBuf::from(p)),
        None => {
            let default = PathBuf::from(baseline::DEFAULT_FILE);
            default.exists().then_some(default)
        }
    };
    let Some(file) = file else {
        line!(sec, "  none (no baseline file; `check --set-baseline` writes one)");
        return (sec, 0);
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(_) => {
            // Missing `--baseline` path is reported absent, not failed.
            line!(sec, "  none ({} not readable)", file.display());
            return (sec, 0);
        }
    };

    // Unparseable = header not valid JSON (§10 contradiction); entries stay tolerant.
    let header_ok = text
        .lines()
        .next()
        .is_some_and(|first| serde_json::from_str::<serde_json::Value>(first).is_ok());
    if !header_ok {
        line!(sec, "  {}: UNPARSEABLE (header is not valid JSON)", file.display());
        line!(sec, "  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
        *contradiction = true;
        return (sec, 0);
    }

    let entries = baseline::parse(&text);
    line!(sec, "  file: {} ({} entr{})", file.display(), entries.len(), plural(entries.len()));

    let dormant = match baseline::parse_header(&text) {
        Some(capture) => {
            line!(
                sec,
                "  capture surface: profile `{}`, {} id(s)",
                capture.profile,
                capture.ids.len()
            );
            line!(
                sec,
                "  active surface: profile `{}`, {} id(s)",
                surface.name,
                surface.surface_ids().len()
            );
            // Dormant (ADR-0050 §8): outside active, kept not stale. Debug-lane entries
            // (ADR-0053 §4/§8, #108) are excluded — `match_baseline` calls those stale.
            let dormant = entries
                .iter()
                .filter(|e| !surface.surfaces_id(&e.id))
                .filter(|e| !matches!(steins_infer::layer(&e.id), Some(steins_infer::Layer::Debug)))
                .count();
            if dormant > 0 {
                line!(
                    sec,
                    "  {dormant} dormant entr{} (id outside the active surface — kept, not stale)",
                    plural(dormant)
                );
            }
            dormant
        }
        None => {
            // Pre-ADR-0050 header (no capture surface).
            line!(sec, "  capture surface: none recorded (pre-capture-surface baseline header)");
            0
        }
    };
    (sec, dormant)
}

/// `y`/`ies` suffix for "entr{}" — a tiny plain-text nicety.
fn plural(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}

/// The Catalog section's version-pin verdict (ADR-0052 amendment A11). Three
/// states: "not skewed" and "cannot say" have different fixes. Rendering
/// treats [`Self::Unconfirmed`] like [`Self::Confirmed`] (ADR-0004); the
/// `"catalog-pin-match"` require assertion does not ([`evaluate_assertion`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogSkew {
    /// A target or the runtime minor is known and matches the pin exactly.
    Confirmed,
    /// A target or the runtime minor is known and does NOT match the pin.
    Skewed,
    /// Neither a target nor a sidecar answered — no comparison basis at all.
    Unconfirmed,
}

/// Section 7 — Catalog (ADR-0052 amendment A11): pinned php-src minor versus
/// this run's version, plus hierarchy/foldable counts as freshness context.
/// Mirrors `steins-infer`'s private skew rule: a declared TARGET is skewed
/// unless exactly the pin; else the sidecar runtime minor; else "unconfirmed"
/// (not asserted unskewed). Returns [`CatalogSkew`] for [`evaluate_assertion`].
fn section_catalog(target: Option<&PhpTarget>, runtime_minor: Option<(u16, u16)>) -> (Section, CatalogSkew) {
    let mut sec = Section::new("Catalog");
    let pin = steins_catalog::PINNED_PHP;
    line!(sec, "  builtin catalog pinned to php-src PHP {}.{}", pin.0, pin.1);

    let skew = match target {
        Some(t) => {
            let skewed = !t.is_exactly(pin);
            line!(
                sec,
                "  analysis target: PHP {} — {}",
                t.render(),
                if skewed { "SKEWED against the pin" } else { "matches the pin exactly" }
            );
            if skewed { CatalogSkew::Skewed } else { CatalogSkew::Confirmed }
        }
        None => match runtime_minor {
            Some(m) => {
                let skewed = m != pin;
                line!(
                    sec,
                    "  no target declared; runtime PHP {}.{} — {}",
                    m.0,
                    m.1,
                    if skewed { "SKEWED against the pin" } else { "matches the pin exactly" }
                );
                if skewed { CatalogSkew::Skewed } else { CatalogSkew::Confirmed }
            }
            None => {
                line!(
                    sec,
                    "  no target declared and no PHP sidecar this run — skew is unconfirmed (no comparison basis); the checker treats this as unskewed, the same silence-over-absence default as elsewhere in this report"
                );
                CatalogSkew::Unconfirmed
            }
        },
    };
    if skew == CatalogSkew::Skewed {
        line!(
            sec,
            "  A11 consequence: catalog-backed is-a demoted to Unknown for arm deletion and descendant closure (ADR-0052 amendment A11)"
        );
    }
    // The portability classification is three-valued (ADR-0028, 2026-08-14);
    // describes the CATALOG, not the project. Refused (a divergence on record,
    // and `refusal()` says on which axis) vs unverified (awaiting probes).
    // Reported as "portability", not "width": one refused row — `preg_split` —
    // is refused for a PCRE build option, and calling that a width verdict would
    // tell the reader something untrue about it.
    line!(
        sec,
        "  hierarchy table: {} row(s); foldable allowlist: {} name(s) (portability: {} portable / {} refused / {} unverified) (freshness context, not a per-project fact)",
        steins_catalog::hierarchy_entry_count(),
        steins_catalog::foldable_entry_count(),
        steins_catalog::portable_names().len(),
        steins_catalog::refused_names().len(),
        steins_catalog::unverified_names().len()
    );
    (sec, skew)
}

/// Section 8 — Registry totality (ADR-0054 §9.7): mechanics self-check.
/// Registry ids must partition exactly into `ALL_EMITTABLE_IDS` and
/// `REGISTERED_NOT_YET_EMITTED`. Redundant with `tests/registry.rs` today,
/// until plugin registration adds ids at runtime.
fn section_registry() -> Section {
    let mut sec = Section::new("Registry totality");
    let emittable: std::collections::HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    let pending: std::collections::HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();

    let mut anomalies: Vec<String> = Vec::new();
    for &(id, ..) in DIAGNOSTIC_REGISTRY {
        let in_emittable = emittable.contains(id);
        let in_pending = pending.contains(id);
        if in_emittable && in_pending {
            anomalies.push(format!("`{id}` is both emittable and registered-not-yet-emitted"));
        } else if !in_emittable && !in_pending {
            anomalies.push(format!("`{id}` is registered but neither emittable nor pending"));
        }
    }
    for &id in REGISTERED_NOT_YET_EMITTED {
        if !DIAGNOSTIC_REGISTRY.iter().any(|&(rid, ..)| rid == id) {
            anomalies.push(format!("`{id}` is pending but not registered"));
        }
    }

    line!(
        sec,
        "  {} registered id(s): {} emittable, {} registered-not-yet-emitted",
        DIAGNOSTIC_REGISTRY.len(),
        emittable.len(),
        pending.len()
    );
    if anomalies.is_empty() {
        line!(
            sec,
            "  partition consistent — every registered id is emittable XOR pending (ADR-0050 §2 totality)"
        );
    } else {
        for a in &anomalies {
            line!(sec, "  INCONSISTENT: {a}");
        }
    }
    sec
}

/// The facts [`section_require`] evaluates named assertions against, gathered
/// from earlier sections in [`run_doctor`] so this section recomputes nothing.
struct RequireFacts {
    sidecar_ok: bool,
    catalog_skew: CatalogSkew,
    monkey_patch_present: bool,
    dormant_count: usize,
}

/// Known `[doctor] require` assertion names (ADR-0054 §14). A name outside this
/// list is a hard config error (serde can't gate a string value).
const KNOWN_ASSERTIONS: &[&str] = &["sidecar", "catalog-pin-match", "no-monkey-patch", "no-dormant-baseline"];

/// Section 9 — Require (ADR-0054 §14): `[doctor] require = [...]` turns a
/// posture line into an exit-1 assertion. Empty list renders "not configured",
/// no contradiction. An unknown name is a config contradiction.
fn section_require(names: Vec<String>, facts: &RequireFacts, contradiction: &mut bool) -> Section {
    let mut sec = Section::new("Require");
    if names.is_empty() {
        line!(
            sec,
            "  not configured — no posture assertions declared ([doctor] require = [...] opts in, ADR-0054 §14)"
        );
        return sec;
    }

    let mut failed: Vec<&str> = Vec::new();
    for name in &names {
        let Some((ok, detail)) = evaluate_assertion(name, facts) else {
            line!(
                sec,
                "  FAIL `{name}` — unknown assertion (known: {}); configuration contradiction",
                KNOWN_ASSERTIONS.join(", ")
            );
            *contradiction = true;
            failed.push(name.as_str());
            continue;
        };
        if ok {
            line!(sec, "  PASS `{name}` — {detail}");
        } else {
            line!(sec, "  FAIL `{name}` — {detail}");
            failed.push(name.as_str());
            *contradiction = true;
        }
    }
    if failed.is_empty() {
        line!(sec, "  all {} declared assertion(s) satisfied", names.len());
    } else {
        line!(
            sec,
            "  {} of {} declared assertion(s) FAILED ({}) — doctor exits 1, ADR-0054 §14",
            failed.len(),
            names.len(),
            failed.join(", ")
        );
    }
    sec
}

/// Evaluate one `[doctor] require` assertion against [`RequireFacts`]. `None`
/// for an unrecognized name, so a config typo can't print as a violated
/// posture; the caller turns `None` into the contradiction line.
fn evaluate_assertion(name: &str, facts: &RequireFacts) -> Option<(bool, &'static str)> {
    match name {
        "sidecar" => Some((
            facts.sidecar_ok,
            if facts.sidecar_ok {
                "the PHP sidecar spawned and answered env()"
            } else {
                "no PHP sidecar answered this run (Runtime section)"
            },
        )),
        // Disagrees with the section's rendering on `Unconfirmed` (#268): `require`
        // demands a guarantee, so only `Confirmed` passes; both others fail.
        "catalog-pin-match" => Some(match facts.catalog_skew {
            CatalogSkew::Confirmed => {
                (true, "the analysis version matches the catalog's php-src pin (Catalog section)")
            }
            CatalogSkew::Skewed => {
                (false, "the analysis version is skewed against the catalog's php-src pin (Catalog section)")
            }
            CatalogSkew::Unconfirmed => (
                false,
                "unconfirmable — no target declared and no PHP sidecar this run, so the pin match cannot be guaranteed (Catalog section); declare a PHP target or make a sidecar available",
            ),
        }),
        "no-monkey-patch" => Some((
            !facts.monkey_patch_present,
            if facts.monkey_patch_present {
                "a monkey-patch extension is loaded (Runtime section, ADR-0049 A9)"
            } else {
                "no monkey-patch extension is loaded"
            },
        )),
        "no-dormant-baseline" => Some((
            facts.dormant_count == 0,
            if facts.dormant_count == 0 {
                "the baseline carries no dormant entries"
            } else {
                "the baseline carries dormant entries (Baseline section)"
            },
        )),
        _ => None,
    }
}
