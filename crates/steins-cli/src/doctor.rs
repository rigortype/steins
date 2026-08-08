//! `steins doctor` (ADR-0054 Part II).
//!
//! Doctor is the **index-bound posture mirror** (ADR-0054 §8): it reads
//! configuration, the environment (via the sidecar's `env()`), and index-level
//! facts (declared `@throws` envelopes, the diagnostic registry, the baseline
//! header) and renders a plain, quiet, sectioned report. It NEVER runs a
//! diagnostic emitter — "doctor asks what the world is; check asks what is
//! wrong". Its exit never depends on what `check` would find.
//!
//! # Exit semantics (ADR-0054 §10)
//!
//! * **0** — report produced, including *degraded* postures (no reachable PHP,
//!   monkey-patch extensions, dormant baseline entries, catalog skew). Degradation
//!   is surfaced loudly but exit-neutrally (ADR-0004 crying-wolf prohibition).
//! * **1** — a hard *configuration contradiction*: an unparseable `steins.toml`, a
//!   profile-resolution error, an unparseable baseline file, or a violated
//!   `[doctor] require` assertion (an unknown assertion NAME is also this lane —
//!   §14) — exactly the conditions under which `check` diverges from declared
//!   intent, or a strictness the project itself opted into.
//! * **2** — doctor's own usage errors: an unknown flag, a second path, a
//!   `--baseline` with no argument, an unrecognized `--format` value, and — §10
//!   amendment — a path argument that names nothing.
//!
//! # Scope
//!
//! Nine sections, in the ADR-0054 §9 numbered order plus C4's additions: Runtime
//! (sidecar/PHP health, SAPI/monkey-patch lines A6/A9), Config + active surface,
//! Layout (the ADR-0015 vendor resolution), Coverage posture (dam statistics, the
//! opaque-construct inventory, the vouch/vendor/sound-subset lines), Envelopes
//! (the G1-demote written-but-unchecked notice — ADR-0054 §9.4's Active-surface
//! content), Baseline, Catalog (A11 pin skew), Registry totality (the mechanics
//! self-check), and Require (`[doctor] require`, ADR-0054 §14). Two C4 lines the
//! ADR lists are not rendered, each for a documented reason rather than an
//! oversight: the **dump-site count** (ADR-0053 §13 / ADR-0054's own §9.2 text)
//! waits on the D3/D4 recognizer, which has not landed; `contract_touches_class`'s
//! project-wide count (ADR-0049 §11) needs the checker's whole-project symbol
//! index, which lives deep inside `steins-infer`'s private `Cx`/`Index` machinery
//! with no index-only surface doctor can reach without standing up a second
//! inference pipeline — exactly the "no new analysis passes" line issue #268
//! draws. Both land with the slice that lands their recognizer.
//!
//! `--format json` (ADR-0054 §14) renders the identical `Vec<Section>` this file
//! builds once, so `text`/`json` invariance is structural, not a second render
//! path to keep in sync (see [`render_json`]'s schema doc).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{PhpTarget, ProjectLayout};
use steins_infer::{
    ALL_EMITTABLE_IDS, DIAGNOSTIC_REGISTRY, DamKind, FileUnit, MONKEY_PATCH_EXTENSIONS,
    REGISTERED_NOT_YET_EMITTED, SAPI_PROVIDED_FUNCTIONS_EXACT, SAPI_PROVIDED_FUNCTION_PREFIXES,
    SOUND_SUBSET_NOTICE, THROW_UNDECLARED_ID, dam_facts,
};
use steins_phpdoc::{TagKind, scan_docblock};
use steins_sidecar::Sidecar;
use steins_syntax::{OpaqueConstruct, ReflectionKind, SourceTree};

use crate::baseline;
use crate::profile;

/// One parsed file of the scanned tree — doctor's whole index. Parsed once in
/// [`run_doctor`] and shared by every section that reads source-level facts, so the
/// report costs exactly one parse per file. This is the deepest doctor ever looks:
/// index-bound by construction (ADR-0054 §8), no emitter, no inference.
struct ParsedFile {
    path: String,
    tree: SourceTree,
}

/// One report section (ADR-0054 §9): a title plus already-formatted body lines.
/// Both renderings (`text`/`json`) walk the same `Vec<Section>` — the point-9
/// numbered structure IS the schema (ADR-0054 §14's `--format json`).
struct Section {
    name: &'static str,
    lines: Vec<String>,
}

impl Section {
    fn new(name: &'static str) -> Self {
        Self { name, lines: Vec::new() }
    }
}

/// Push one formatted line onto a [`Section`]'s body — the doctor-local twin of
/// `outln!` that buffers instead of writing, so the same section data feeds
/// either renderer.
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
    // A path argument naming nothing is doctor's own usage error (ADR-0054 §10
    // amendment) — the third exit code, not the environment-degradation 0 and not
    // the config-contradiction 1. Checked before the header line: a report about a
    // tree that is not there is worse than no report.
    if let Err(code) = crate::reject_missing_paths(&paths) {
        return code;
    }

    // Environment facts report at exit 0 (ADR-0054 §10); a configuration the world
    // refutes flips this and exits 1.
    let mut contradiction = false;

    let banner = "steins doctor — posture report (index-bound; runs no checks)";

    // One parse of the tree, one layout discovery, shared by every section below.
    // Routed through the same `resolve_layout` every other surface uses (issue
    // #181), so doctor's Layout section reports exactly what `check` would
    // filter — including `steins.toml [paths] vendor-dirs` when set.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let layout = crate::resolve_layout(std::slice::from_ref(&root.to_string_lossy().into_owned()));
    let files = parse_project(&root);

    let mut sections: Vec<Section> = Vec::new();

    let (runtime_section, runtime_facts) = section_runtime(no_php, &layout);
    sections.push(runtime_section);

    let (config_section, config) = section_config(&mut contradiction);
    sections.push(config_section);

    sections.push(section_layout(&root, &cwd, &layout));
    sections.push(section_coverage(&root, &files, &layout, runtime_facts.sidecar_ok, config.vouch_sites));
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
        // ExitCode::FAILURE == 1: the doctor config-contradiction code (ADR-0054 §10).
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Print every section exactly as the pre-C4 doctor did: a blank line, the
/// section name, then its body lines verbatim (already carrying their own
/// leading-space indentation) — byte-identical to the historical output for the
/// sections that existed before this change.
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

/// Render the same `sections` as JSON (ADR-0054 §14): the schema IS the point-9
/// section list, one object per section with its name and body lines. Lines are
/// trimmed of the text renderer's leading-space indentation (a display nicety for
/// terminals, not a machine-readable fact); nesting is not otherwise represented
/// in this v1 shape.
///
/// ```json
/// {
///   "schema": "steins.doctor/v1",
///   "banner": "steins doctor — posture report (index-bound; runs no checks)",
///   "exit_code": 0,
///   "sections": [
///     {"name": "Runtime", "lines": ["PHP sidecar: spawned ok", "..."]},
///     {"name": "Config + active surface", "lines": ["..."]},
///     {"name": "Layout", "lines": ["..."]},
///     {"name": "Coverage posture", "lines": ["..."]},
///     {"name": "Envelopes", "lines": ["..."]},
///     {"name": "Baseline", "lines": ["..."]},
///     {"name": "Catalog", "lines": ["..."]},
///     {"name": "Registry totality", "lines": ["..."]},
///     {"name": "Require", "lines": ["..."]}
///   ]
/// }
/// ```
///
/// `sections` is always exactly this list, in this order, whether a given
/// section's `lines` are empty-of-content prose (e.g. "none") or numerous — the
/// *structure* is fixed, never the *content*, which is the same format-invariance
/// discipline Part I's four formats hold for `check` (ADR-0054 point 1).
/// `schema` is versioned so a future incompatible reshape bumps it rather than
/// breaking a consumer silently.
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

/// Facts the Runtime section computes that later sections (Catalog, Require) also
/// need — carried out rather than recomputed, since doctor spawns the sidecar at
/// most once per run.
struct RuntimeFacts {
    /// Whether the sidecar spawned AND its `env()` round-trip succeeded — the
    /// `"sidecar"` [`RequireFacts`] leg.
    sidecar_ok: bool,
    /// The sidecar-reported `(major, minor)`, when the round-trip succeeded.
    runtime_minor: Option<(u16, u16)>,
    /// Whether a monkey-patch extension (ADR-0049 A9) is loaded.
    monkey_patch_present: bool,
}

/// Section 1 — Runtime (ADR-0054 §9.1): sidecar spawn health, PHP version, SAPI,
/// loaded-extension count, the monkey-patch line (A9), the A6 SAPI-undeclared
/// line, and the analysis TARGET version with its skew against the runtime
/// (issue #28).
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
            RuntimeFacts { sidecar_ok: false, runtime_minor: None, monkey_patch_present: false },
        );
    }
    let facts = match Sidecar::spawn() {
        Ok(mut sc) => match sc.env() {
            Some(env) => {
                line!(sec, "  PHP sidecar: spawned ok");
                line!(sec, "  PHP version: {}", env.php_version);
                line!(sec, "  SAPI: {}", env.sapi);
                line!(sec, "  loaded extensions: {}", env.extensions.len());
                // Monkey-patch presence (ADR-0049 A9): a loaded `uopz`/`runkit7`/
                // `Componere` silently voids the entire absence-proof family — the
                // exact incompleteness ADR-0004 forbids leaving unsaid, so name it.
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
                RuntimeFacts { sidecar_ok: true, runtime_minor, monkey_patch_present }
            }
            None => {
                line!(sec, "  PHP sidecar: spawned, but the env() query failed");
                line!(
                    sec,
                    "  posture: sound subset (degraded) — findings that require executing PHP are omitted (exit 0, ADR-0004)"
                );
                RuntimeFacts { sidecar_ok: false, runtime_minor: None, monkey_patch_present: false }
            }
        },
        Err(_) => {
            line!(sec, "  PHP sidecar: not spawnable (no `php` on PATH)");
            line!(sec, "  {SOUND_SUBSET_NOTICE}");
            line!(sec, "  (a degraded environment is not a failure — exit stays 0, ADR-0004)");
            RuntimeFacts { sidecar_ok: false, runtime_minor: None, monkey_patch_present: false }
        }
    };
    section_sapi_notice(&mut sec);
    (sec, facts)
}

/// The A6 SAPI-existence-oracle line (ADR-0049 A6, ADR-0054 §9.1): `[runtime]
/// sapi` names the serving surface and is what would unlock a firing claim
/// against the curated SAPI-provided names below — that key is itself
/// deferred-with-design (`steins_infer::is_sapi_provided_function`'s doc), so it
/// is undeclared on every run today, and this line says so rather than leaving
/// the standing of `fastcgi_finish_request()` and friends unstated. Printed on
/// every run (`--no-php` included): the standing does not depend on whether a
/// sidecar answered this run, only on whether the project declared its SAPI.
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

/// Section 2 — Config + active surface (ADR-0054 §9.3/§9.4). Returns the resolved
/// display surface plus the config-derived facts other sections need (the
/// `[transform.vouch]` site count for Coverage posture, `[doctor] require` for the
/// Require section). An unparseable `steins.toml` or a profile-resolution error is
/// a configuration contradiction (`*contradiction = true`, exit 1); the section
/// still renders on the built-in `default` surface so the rest of the report is
/// produced.
struct ConfigOutcome {
    surface: profile::Surface,
    /// `[transform.vouch] sites` count — read alongside the rest of the config so
    /// Coverage posture's vouch line needs no second `steins.toml` parse.
    vouch_sites: usize,
    /// `[doctor] require` as declared (validated later, in [`section_require`],
    /// since its facts are not all known yet at config-parse time).
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
            // A genuine absence (not the parse-error fallback, which already printed).
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
            // Fall back to the built-in default surface so the remaining sections
            // render; the run already exits 1 on the contradiction.
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

/// The `[runtime]` pseudo-constant lines of the Config section (ADR-0037 §2; the
/// ADR-0054 §9 item that had been listed as not-covered).
///
/// Named-silence discipline: a posture that changes what Steins will and will not
/// claim must be visible without reading the source, and a *default* posture is
/// still a posture — both keys print on every run, tagged with where the value came
/// from, so "I never declared that" and "I declared it and it did not take" are
/// distinguishable from the report alone. An unrecognized *value* is a
/// warn-and-proceed in `check`; doctor names it here as the environment fact it is.
fn section_runtime_postures(sec: &mut Section, runtime_cfg: Option<crate::RuntimeConfig>) {
    // Declared-ness is read before the config is consumed: the resolved value alone
    // cannot distinguish an absent key from one spelled at its default.
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

/// The Runtime section's TARGET lines (issue #28): what version range the
/// analysis is about, where that came from, and — when a runtime answered —
/// the skew between the two, named in the direction it degrades.
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

/// The `(major, minor)` of the sidecar's version report, for the skew line.
fn parse_env_minor(v: &str) -> Option<(u16, u16)> {
    let mut it = v.split('.');
    let major = it.next()?.parse().ok()?;
    let minor: u16 =
        it.next()?.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?;
    Some((major, minor))
}

/// Section 3 — Layout (ADR-0015): which trees this run treats as somebody else's.
///
/// Vendor classification decides whether a finding is reported at all and whether
/// a declaration is a transform candidate, and a wrong answer moves findings
/// between "ours" and "theirs" without saying so. It is resolved from the
/// project's own `composer.json` — `config.vendor-dir` plus the autoload roots —
/// so the report names the manifest that answered, and says plainly when nothing
/// did and the directory-name floor is carrying the whole decision.
fn section_layout(root: &Path, cwd: &Path, layout: &ProjectLayout) -> Section {
    let mut sec = Section::new("Layout");
    if layout.is_fallback() {
        line!(
            sec,
            "  no composer.json governs {} — vendor is the `vendor` directory-name floor, not a declared fact",
            root.display()
        );
        return sec;
    }
    line!(sec, "  {} manifest(s) govern this tree:", layout.roots().len());
    for r in layout.roots() {
        line!(sec, "    {}", display_path(cwd, r.manifest()));
        line!(sec, "      vendor: {}", join_paths(cwd, r.vendor_roots()));
        line!(sec, "      ours:   {}", join_paths(cwd, r.first_party_roots()));
    }
    sec
}

/// Section 4 — Coverage posture (ADR-0054 §9.2): what this run parsed and then
/// declined to reason about.
///
/// A diagnostic-driven pipeline cannot see what produces no diagnostics: a scope
/// full of `extract()` and a scope proven clean print the same nothing. Steins is
/// *correct* on those scopes — `Scope::poisoned` makes every local unknown, so the
/// "eval rewrote my local" false-positive class is structurally impossible
/// (ADR-0046 §1). Under the
/// crying-wolf prohibition the risk is symmetrical: a quiet analyzer that cannot say
/// *why* it is quiet is asking to be trusted on nothing. This section is the
/// measurement, so a silent run is a claim with numbers behind it.
///
/// Facts, each from an existing surface and none of them a diagnostic (no
/// registry id, no baseline entry, no fp-gate counter):
///
/// 1. **Poisoned scopes** as a share of all scopes, with the sites broken down by
///    construct kind.
/// 2. **Dam sites** (ADR-0049 §2 / ADR-0054 §9.2), broken down by eval / unproven
///    include / runtime-name `class_alias` / … .
/// 3. **Reflection-driven invocation** sites — a labelled guess.
/// 4. **The sound-subset id list** (ADR-0054 §9.2 / A2(ii)) — printed only when no
///    sidecar answered this run, naming exactly which absence claims it silences.
/// 5. **The vendor posture** (ADR-0015) — a static policy fact, not a per-project
///    measurement, printed every run so the report is self-contained.
/// 6. **The `[transform.vouch]` count** (ADR-0046 §2) — read from config, not
///    consulted by the checker's own dam yet (`dam.rs`'s own doc: "the vouch
///    valve … [is] deferred; v1 is whole-universe"), so the line says both the
///    count and that boundary honestly rather than implying the dam already
///    reads it.
fn section_coverage(
    root: &Path,
    files: &[ParsedFile],
    layout: &ProjectLayout,
    sidecar_ok: bool,
    vouch_sites: usize,
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
        // Sites are counted per *construct in the source*, not per affected scope: a
        // `use (&$x)` capture is recorded on the enclosing scope AND on the closure's
        // own (one aliasing fact, two silenced scopes — ADR-0033), and both carry the
        // captured variable's span. The scope count above already reports the two;
        // counting the construct twice would answer "grep your source for this" wrong.
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

    // The dam (ADR-0049 §2): the same query answer `check` computes, recomputed here
    // from the same lowered universe. It does not track the construct counts above in
    // either direction — vendor `eval`/dynamic-include is presumed universe-internal
    // and a proven in-universe include is benign (both drop out), while a
    // runtime-name `class_alias` dams without poisoning any scope (it appears only
    // here).
    let units: Vec<FileUnit<'_>> =
        files.iter().map(|f| FileUnit { path: &f.path, tree: &f.tree }).collect();
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
                // parse failure (ADR-0079, issue #180)
                DamKind::Unparsable => 3,
                // end parse failure (ADR-0079, issue #180)
                // global constants (ADR-0078, issue #198)
                DamKind::DefineDynamic => 4,
                // end global constants (ADR-0078, issue #198)
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
        // The name valve is only closed by a kind that can mint a name — a universe
        // whose only sites are runtime-name `define`s keeps its function/class
        // existence claims (ADR-0078, issue #198).
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
        // global constants (ADR-0078, issue #198): every kind closes the constant
        // valve, so the operator is told that separately.
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
    // Stated on every run, not only a non-zero one: the honest reading of a `0` here
    // is "the recognizer saw nothing", not "the code reflects nowhere".
    line!(
        sec,
        "    (this list is a guess until measured: the recognizer is syntactic, it names no receiver type, and it is not exhaustive)"
    );

    // The sound-subset id list (ADR-0054 §9.2, A2(ii)): named only when no sidecar
    // answered — the same condition the Runtime section's degraded-posture lines
    // key on, but with the specific ids that go silent rather than a general note.
    if !sidecar_ok {
        line!(
            sec,
            "  sound subset: no PHP sidecar this run — `call.undefined-function`, `class.undefined`, and `call.undefined-method` (homonym leg) are silenced (ADR-0004, A2(ii))"
        );
    }

    // Vendor posture (ADR-0015): a policy fact, not a per-project measurement.
    line!(sec, "  vendor posture: findings under a vendor root are suppressed by default (ADR-0015)");

    // The `[transform.vouch]` count (ADR-0046 §2 / ADR-0049 §4). The checker's own
    // whole-universe dam does not consult it yet (`dam.rs`'s doc comment records
    // the vouch valve as deferred), so the line says the count AND that boundary —
    // an existing config surface rendered honestly, not a new analysis pass.
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

/// `poisoned/total` as a percentage, or `n/a` for an empty denominator. One decimal:
/// the number is a posture, not a metric anyone should diff on.
fn share(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "n/a".to_owned();
    }
    #[expect(clippy::cast_precision_loss, reason = "a display percentage; counts are small")]
    let pct = part as f64 * 100.0 / whole as f64;
    format!("{pct:.1}%")
}

/// `label N` pairs for the non-zero counts, comma-joined in the array's order. Zero
/// counts are dropped: a list of nine kinds where one fired reads as noise, and the
/// total is already on the line.
fn breakdown<const N: usize>(counts: &[usize; N], labels: [&str; N]) -> String {
    counts
        .iter()
        .zip(labels)
        .filter(|(n, _)| **n > 0)
        .map(|(n, label)| format!("{label} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse every `.php` file under `root` once. Unreadable files are skipped silently
/// (the same tolerance `check`'s collection has); parse errors are not: a recovered
/// tree still carries scopes and sites, which is exactly the ADR-0003 posture.
fn parse_project(root: &Path) -> Vec<ParsedFile> {
    let mut files = Vec::new();
    crate::collect_php_files(root, &mut files);
    let files = crate::dedup_canonical(files);
    files
        .iter()
        .filter_map(|file| {
            let bytes = std::fs::read(file).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            Some(ParsedFile {
                path: file.to_string_lossy().into_owned(),
                tree: SourceTree::parse(&text),
            })
        })
        .collect()
}

/// Render a path relative to `cwd` when it sits underneath it, else absolute.
/// Doctor's output is read next to the shell it was run from.
fn display_path(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd).unwrap_or(p).display().to_string()
}

/// A comma-joined root list, or `none declared` for an empty one — an autoload
/// block a project simply does not have.
fn join_paths(cwd: &Path, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "none declared".to_owned();
    }
    paths.iter().map(|p| display_path(cwd, p)).collect::<Vec<_>>().join(", ")
}

/// Section 5 — Envelopes (ADR-0054 §9.4, the G1-amendment written-but-unchecked
/// notice — the "Active surface" content the ADR's own numbered list gives its own
/// entry). An index scan (never the checker): count declarations carrying a written
/// `@throws` tag, then state whether the active surface checks them. This is the
/// designed answer to "wrote `@throws`, got silence".
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

/// Count declarations (functions + methods) that carry a written `@throws` tag,
/// reading the docblock trivia of the already-parsed tree. Index-bound: it reads
/// parsed source; it runs no inference.
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

/// Section 6 — Baseline (ADR-0054 §9.5): the capture surface (profile + id count
/// from the header) versus the active surface, and the dormant-entry count
/// (entries whose id is outside the active surface — kept, not stale). Doctor
/// accepts `--baseline <path>`; absent that it discovers the conventional default
/// file, and reports "none" when neither resolves. An unparseable baseline file is
/// a configuration contradiction (exit 1, ADR-0054 §10). Returns the dormant count
/// (`0` when there is no baseline, or no capture-surface header, to compare
/// against) for the `"no-dormant-baseline"` [`RequireFacts`] leg.
fn section_baseline(
    cli_path: Option<&str>,
    surface: &profile::Surface,
    contradiction: &mut bool,
) -> (Section, usize) {
    let mut sec = Section::new("Baseline");

    // Resolve the file: an explicit `--baseline` wins; else the conventional default
    // (the same file `check` auto-loads) when it exists.
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
            // An explicit `--baseline` to a missing path is reported absent, not failed.
            line!(sec, "  none ({} not readable)", file.display());
            return (sec, 0);
        }
    };

    // Unparseable = the header line is not even valid JSON (ADR-0054 §10 contradiction).
    // Entry lines stay hand-edit-tolerant (baseline::parse ignores unparsable ones).
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
            // Dormant (ADR-0050 §8): an entry whose id is outside the ACTIVE surface —
            // kept, not stale, because this profile simply never looks for it. The
            // debug lane (ADR-0053 §4/§8, issue #108) never reads "outside the
            // surface" — `surfaces_id` excludes it unconditionally, so it would
            // otherwise always count here — but a debug finding is checked on
            // every profile and a debug baseline entry can never be matched again,
            // so "kept, not stale" is the wrong story for it; `check` (main.rs's
            // `match_baseline`) reports the same entry as stale, and this line must
            // not contradict that by calling it dormant.
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
            // A pre-ADR-0050 header (no capture surface) is reported as such, not failed.
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

/// Section 7 — Catalog (ADR-0052 amendment A11): the builtin catalog's pinned
/// php-src minor versus this run's version answer, and the hierarchy/foldable
/// entry counts as freshness context.
///
/// Mirrors `steins-infer`'s own (private) `effective_php_view` skew rule exactly:
/// a declared TARGET is skewed unless it is *exactly* the pin (a range, not a
/// point, so even a range containing the pin is not the same claim as being it);
/// with no target, the sidecar-reported runtime minor is compared instead; with
/// neither, the checker treats the run as unskewed (no comparison basis, and
/// silence-over-absence is the same crying-wolf discipline as everywhere else in
/// this report) — recorded here as "unconfirmed", not asserted as a fact nobody
/// measured. Returns the skew flag for the `"catalog-pin-match"` [`RequireFacts`]
/// leg.
fn section_catalog(target: Option<&PhpTarget>, runtime_minor: Option<(u16, u16)>) -> (Section, bool) {
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
            skewed
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
                skewed
            }
            None => {
                line!(
                    sec,
                    "  no target declared and no PHP sidecar this run — skew is unconfirmed (no comparison basis); the checker treats this as unskewed, the same silence-over-absence default as elsewhere in this report"
                );
                false
            }
        },
    };
    if skew {
        line!(
            sec,
            "  A11 consequence: catalog-backed is-a demoted to Unknown for arm deletion and descendant closure (ADR-0052 amendment A11)"
        );
    }
    line!(
        sec,
        "  hierarchy table: {} row(s); foldable allowlist: {} name(s) (freshness context, not a per-project fact)",
        steins_catalog::hierarchy_entry_count(),
        steins_catalog::foldable_entry_count()
    );
    (sec, skew)
}

/// Section 8 — Registry totality (ADR-0054 §9.7): the mechanics self-check. Every
/// emittable id must be registered with a layer, and the registry's ids must
/// partition exactly into `ALL_EMITTABLE_IDS` and `REGISTERED_NOT_YET_EMITTED`
/// with no overlap and no phantom entries. Redundant with `tests/registry.rs`
/// today — the ADR names this explicitly ("exactly the check that stops being
/// redundant the day plugin registration puts ids into the registry at
/// runtime") — so this section runs the identical partition test over the
/// registry doctor actually links against, in the actual binary a user runs.
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

/// The facts [`section_require`] evaluates named assertions against — one field
/// per known assertion name, gathered from the sections computed earlier in
/// [`run_doctor`] so this section recomputes nothing.
struct RequireFacts {
    sidecar_ok: bool,
    catalog_skew: bool,
    monkey_patch_present: bool,
    dormant_count: usize,
}

/// The known `[doctor] require` assertion names (ADR-0054 §14), each paired with
/// the sentence printed on a PASS/FAIL line and the fact it reads from
/// [`RequireFacts`]. A name outside this list is a hard config error (the
/// `deny_unknown_fields` posture generalized to a value rather than a struct key,
/// since the string names data `[doctor]`'s own serde shape cannot gate).
const KNOWN_ASSERTIONS: &[&str] = &["sidecar", "catalog-pin-match", "no-monkey-patch", "no-dormant-baseline"];

/// Section 9 — Require (ADR-0054 §14): `[doctor] require = [...]` turns a named
/// posture line from a report-only fact into an exit-1 assertion — the
/// lenient-default opt-in point 10 promised. Not configured is not a failure:
/// this section always renders, but says "not configured" and leaves
/// `*contradiction` untouched when the list is empty. An unknown name is a
/// configuration contradiction, same lane as an unparseable `steins.toml`.
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

/// Evaluate one `[doctor] require` assertion name against the already-gathered
/// [`RequireFacts`]. `None` for an unrecognized name — the caller turns that into
/// the configuration-contradiction line, keeping the "unknown name" case out of
/// the PASS/FAIL bool space entirely (a `false` here would print as a violation
/// of a real posture, not the config typo it actually is).
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
        "catalog-pin-match" => Some((
            !facts.catalog_skew,
            if facts.catalog_skew {
                "the analysis version is skewed against the catalog's php-src pin (Catalog section)"
            } else {
                "the analysis version matches the catalog's php-src pin (or is unconfirmed, treated as a match)"
            },
        )),
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
