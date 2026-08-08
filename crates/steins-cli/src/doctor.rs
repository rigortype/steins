//! `steins doctor` (ADR-0054 Part II).
//!
//! Doctor is the **index-bound posture mirror** (ADR-0054 §8): it reads
//! configuration, the environment (via the sidecar's `env()`), and index-level
//! facts (declared `@throws` envelopes, the baseline header) and renders a plain,
//! quiet, sectioned report. It NEVER runs a diagnostic emitter — "doctor asks what
//! the world is; check asks what is wrong". Its exit never depends on what `check`
//! would find.
//!
//! # Exit semantics (ADR-0054 §10)
//!
//! * **0** — report produced, including *degraded* postures (no reachable PHP,
//!   monkey-patch extensions, dormant baseline entries). Degradation is surfaced
//!   loudly but exit-neutrally (ADR-0004 crying-wolf prohibition).
//! * **1** — a hard *configuration contradiction*: an unparseable `steins.toml`, a
//!   profile-resolution error, or an unparseable baseline file — exactly the
//!   conditions under which `check` diverges from declared intent.
//! * **2** — doctor's own usage errors: an unknown flag, a second path, a
//!   `--baseline` with no argument, and — §10 amendment — a path argument that
//!   names nothing. The last one is argv, not environment: doctor reports the
//!   world at 0, but only about a tree the caller actually named. Reporting on
//!   `/typo`'s *parent* is not a degraded posture, it is an answer to a different
//!   question, so it does not belong to the exit-0 lane.
//!
//! # Scope
//!
//! Six sections: Runtime (sidecar/PHP health + SAPI + extension count, the
//! monkey-patch line), Config + active surface, Layout (the ADR-0015 vendor
//! resolution and the manifest that answered), Coverage posture (ADR-0054 §9.2 and
//! issue #30 — the dam statistics and the opaque-construct inventory), Envelopes
//! (the G1-demote written-but-unchecked notice), and Baseline. The Config section
//! also carries the `[runtime]` pseudo-constant lines (ADR-0037 §2), which the
//! posture family reaches through `steins.toml` rather than through the
//! environment. Not covered from the full ADR-0054 §9 list: Catalog skew, Registry
//! totality, the SAPI-undeclared A6 line, and `doctor --format json` (§14: the
//! section structure is the schema; it ships when a consumer exists).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_db::{PhpTarget, ProjectLayout};
use steins_infer::{
    DamKind, FileUnit, MONKEY_PATCH_EXTENSIONS, SOUND_SUBSET_NOTICE, THROW_UNDECLARED_ID,
    dam_facts,
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

/// `steins doctor [--no-php] [--baseline <path>] [path]` (default `path` = `.`).
pub fn run_doctor(args: &[String]) -> ExitCode {
    let mut no_php = false;
    let mut baseline_path: Option<String> = None;
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
                "steins: doctor takes at most one path (usage: steins doctor [--no-php] [--baseline <path>] [path])"
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

    outln!("steins doctor — posture report (index-bound; runs no checks)");

    // One parse of the tree, one layout discovery, shared by every section below.
    // Routed through the same `resolve_layout` every other surface uses (issue
    // #181), so doctor's Layout section reports exactly what `check` would
    // filter — including `steins.toml [paths] vendor-dirs` when set.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let layout = crate::resolve_layout(std::slice::from_ref(&root.to_string_lossy().into_owned()));
    let files = parse_project(&root);

    // The live child, when there is one, outlives its section: the Coverage
    // posture's reflected class world (issue #269) is the same engine's answer
    // about the same run, and spawning a second `php` to ask it would be a second
    // runtime the report could not honestly attribute the first one's numbers to.
    let mut sidecar = section_runtime(no_php, &layout);
    let surface = section_config(&mut contradiction);
    section_layout(&root, &cwd, &layout);
    section_coverage(&root, &files, &layout, sidecar.as_mut());
    section_envelopes(&files, &surface);
    section_baseline(baseline_path.as_deref(), &surface, &mut contradiction);

    if contradiction {
        // ExitCode::FAILURE == 1: the doctor config-contradiction code (ADR-0054 §10).
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Section 1 — Runtime (ADR-0054 §9.1, minimal): sidecar spawn health, PHP version,
/// SAPI, loaded-extension count, the monkey-patch line (ADR-0049 A9), and the
/// analysis TARGET version with its skew against the runtime (issue #28).
///
/// Returns the **live child** when one answered `env()`, so a later section can put
/// a further question to the same engine (issue #269). Every degraded posture —
/// `--no-php`, no `php` on PATH, a spawn that then failed to answer — returns
/// `None`, which is what keeps those runs byte-identical to the report they
/// produced before this surface existed.
fn section_runtime(no_php: bool, layout: &ProjectLayout) -> Option<Sidecar> {
    outln!();
    outln!("Runtime");
    let target = layout.php_target();
    if no_php {
        outln!("  PHP sidecar: disabled (--no-php)");
        section_target(target, None);
        outln!("  posture: sound subset — findings that require executing PHP are omitted");
        outln!("  (a degraded environment is not a failure — exit stays 0, ADR-0004)");
        return None;
    }
    match Sidecar::spawn() {
        Ok(mut sc) => match sc.env() {
            Some(env) => {
                outln!("  PHP sidecar: spawned ok");
                outln!("  PHP version: {}", env.php_version);
                outln!("  SAPI: {}", env.sapi);
                outln!("  loaded extensions: {}", env.extensions.len());
                // Monkey-patch presence (ADR-0049 A9): a loaded `uopz`/`runkit7`/
                // `Componere` silently voids the entire absence-proof family — the
                // exact incompleteness ADR-0004 forbids leaving unsaid, so name it.
                let present: Vec<&str> = env
                    .extensions
                    .iter()
                    .filter(|e| MONKEY_PATCH_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m)))
                    .map(String::as_str)
                    .collect();
                if !present.is_empty() {
                    outln!(
                        "  monkey-patch extension(s) loaded: {} — the entire absence-proof family is Unknown-silent this run (ADR-0049 A9)",
                        present.join(", ")
                    );
                }
                section_target(target, parse_env_minor(&env.php_version));
                Some(sc)
            }
            None => {
                outln!("  PHP sidecar: spawned, but the env() query failed");
                outln!(
                    "  posture: sound subset (degraded) — findings that require executing PHP are omitted (exit 0, ADR-0004)"
                );
                None
            }
        },
        Err(_) => {
            outln!("  PHP sidecar: not spawnable (no `php` on PATH)");
            outln!("  {SOUND_SUBSET_NOTICE}");
            outln!("  (a degraded environment is not a failure — exit stays 0, ADR-0004)");
            None
        }
    }
}

/// Section 2 — Config + active surface (ADR-0054 §9.3/§9.4, minimal). Returns the
/// resolved display surface for the later sections. An unparseable `steins.toml` or a
/// profile-resolution error is a configuration contradiction (`*contradiction =
/// true`, exit 1); the section still renders on the built-in `default` surface so the
/// rest of the report is produced.
fn section_config(contradiction: &mut bool) -> profile::Surface {
    outln!();
    outln!("Config + active surface");

    let config = match crate::read_steins_config() {
        Ok(c) => c,
        Err(e) => {
            outln!("  steins.toml: PARSE ERROR — {e}");
            outln!("  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
            *contradiction = true;
            None
        }
    };
    let (check_cfg, profile_tbl, runtime_cfg) = match config {
        Some(c) => {
            outln!("  steins.toml: found");
            (c.check, c.profile, c.runtime)
        }
        None => {
            // A genuine absence (not the parse-error fallback, which already printed).
            if !*contradiction {
                outln!("  steins.toml: not found (built-in defaults govern)");
            }
            (None, None, None)
        }
    };

    section_runtime_postures(runtime_cfg);

    let (config_profile, profile_configs) = crate::profiles_from_config(check_cfg, profile_tbl);
    let provenance = if config_profile.is_some() { "[check] profile" } else { "built-in default" };
    let surface = match profile_configs.resolve(config_profile.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            outln!("  profile resolution: ERROR — {e}");
            outln!("  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
            *contradiction = true;
            // Fall back to the built-in default surface so the remaining sections
            // render; the run already exits 1 on the contradiction.
            profile::ProfileConfigs::default()
                .resolve(None)
                .expect("the built-in default profile always resolves")
        }
    };
    outln!("  active profile: `{}` (from {provenance})", surface.name);
    let layers = surface.layers_on();
    outln!(
        "  surface: layers [{}], {} checked id(s)",
        layers.join(", "),
        surface.surface_ids().len()
    );
    surface
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
fn section_runtime_postures(runtime_cfg: Option<crate::RuntimeConfig>) {
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
    outln!("  [runtime] warning-handler: \"{wh}\" ({wh_src}) — {wh_note}");

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
    outln!("  [runtime] final-keyword: \"{fk}\" ({fk_src}) — {fk_note}");

    for w in warnings {
        outln!("  {w}");
    }
}

/// The Runtime section's TARGET lines (issue #28): what version range the
/// analysis is about, where that came from, and — when a runtime answered —
/// the skew between the two, named in the direction it degrades.
fn section_target(target: Option<&PhpTarget>, runtime_minor: Option<(u16, u16)>) {
    match target {
        None => {
            outln!("  analysis target: none declared — the runtime PHP is the target");
        }
        Some(t) => {
            outln!(
                "  analysis target: PHP {} (from {} \"{}\")",
                t.render(),
                t.source.as_str(),
                t.raw
            );
            if let Some(m) = runtime_minor {
                if !t.contains(m) {
                    outln!(
                        "  version skew: runtime {}.{} is OUTSIDE the declared range — the absence family and reflection-seeded facts are disabled this run (the boot surface is not a version this project ships on)",
                        m.0, m.1
                    );
                } else if t.floor < m {
                    outln!(
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
fn section_layout(root: &Path, cwd: &Path, layout: &ProjectLayout) {
    outln!();
    outln!("Layout");
    if layout.is_fallback() {
        outln!(
            "  no composer.json governs {} — vendor is the `vendor` directory-name floor, not a declared fact",
            root.display()
        );
        return;
    }
    outln!("  {} manifest(s) govern this tree:", layout.roots().len());
    for r in layout.roots() {
        outln!("    {}", display_path(cwd, r.manifest()));
        outln!("      vendor: {}", join_paths(cwd, r.vendor_roots()));
        outln!("      ours:   {}", join_paths(cwd, r.first_party_roots()));
    }
}

/// Section 4 — Coverage posture (ADR-0054 §9.2; issue #30): what this run parsed
/// and then declined to reason about.
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
/// Three facts, each from an existing surface and none of them a diagnostic (no
/// registry id, no baseline entry, no fp-gate counter):
///
/// 1. **Poisoned scopes** as a share of all scopes, with the sites broken down by
///    construct kind. Both come from `Scope::opaque`, which the poison predicate
///    itself populates — one walk decides poisoning and enumerates the reasons, so
///    the inventory cannot drift from the behaviour it describes.
/// 2. **Dam sites** (ADR-0049 §2 / ADR-0054 §9.2's designed line), broken down by
///    eval / unproven-or-out-of-universe include / non-literal `class_alias`. These
///    are the *existence*-claim conditionals, a different soundness hole from scope
///    havoc: they answer "could a name exist that the reference scan never saw".
/// 3. **Reflection-driven invocation** sites — inventoried even though they poison
///    no scope and dam no claim, and labelled as the guess they are.
fn section_coverage(
    root: &Path,
    files: &[ParsedFile],
    layout: &ProjectLayout,
    sidecar: Option<&mut Sidecar>,
) {
    outln!();
    outln!("Coverage posture");
    if files.is_empty() {
        outln!("  no .php files under {} — nothing to inventory", root.display());
        return;
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

    outln!(
        "  {} file(s), {scopes} scope(s), {poisoned} poisoned ({}) — a poisoned scope knows no local's value (ADR-0001, ADR-0046 §1)",
        files.len(),
        share(poisoned, scopes)
    );
    let construct_total: usize = constructs.iter().sum();
    if construct_total == 0 {
        outln!("  opaque constructs: none — no scope is on the give-up list");
    } else {
        outln!(
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
        outln!(
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
        outln!(
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
            outln!(
                "    existence-absence claims (undefined function/class) still stand — no site here can mint a function or class name"
            );
        } else {
            outln!(
                "    existence-absence claims (undefined function/class) stay silent where these stand (ADR-0049 §2)"
            );
        }
        // global constants (ADR-0078, issue #198): every kind closes the constant
        // valve, so the operator is told that separately.
        outln!(
            "    `constant.undefined` stays silent where any of these stand — a runtime-name define is a constant-only dam"
        );
    }

    let reflection_total: usize = reflection.iter().sum();
    if reflection_total == 0 {
        outln!("  reflection-driven invocation: none recognized");
    } else {
        outln!(
            "  reflection-driven invocation: {reflection_total} site(s) — {}",
            breakdown(&reflection, ReflectionKind::ALL.map(ReflectionKind::label))
        );
    }
    // Stated on every run, not only a non-zero one: the honest reading of a `0` here
    // is "the recognizer saw nothing", not "the code reflects nowhere".
    outln!(
        "    (this list is a guess until measured: the recognizer is syntactic, it names no receiver type, and it is not exhaustive)"
    );

    section_reflected_classes(files, sidecar);
}

/// How many distinct unanswered class names doctor will put to the engine.
///
/// A round trip apiece, on a report that must stay quick; a tree with thousands of
/// unresolved names is answered honestly by a sample plus the count it came from,
/// not by a minute of IPC. `check` has no such cap — it asks about the names its
/// walk actually reaches, when it reaches them.
const REFLECT_QUERY_CAP: usize = 200;

/// How many resolved names are printed before the line summarizes the rest.
const REFLECT_DISPLAY_CAP: usize = 8;

/// The **reflected class world** (issue #269), inside Coverage posture: the class
/// names this tree references that neither a source declaration nor a builtin-catalog
/// row answers, but the *project's own PHP* does.
///
/// This is the origin surface for the reflect slice. A class an installed extension
/// provides (`Redis`, `Random\Randomizer`, `Dom\Element`) is invisible to both of
/// Steins' static class worlds, and the engine running the project is the only
/// honest source for it (ADR-0049 §1 — ask the real thing, never a curated stub
/// list). Naming the resolved ones with the extension that declares them is what
/// makes "where did this fact come from" answerable.
///
/// The line is printed **only when a live engine answered**. Under `--no-php`, with
/// no `php` on PATH, or after a failed handshake there is no question to report the
/// answer to, and the section stays exactly what it was before this surface existed.
///
/// Doctor stays index-bound (ADR-0054 §8): this reads declarations and asks the
/// environment, and runs no emitter. The reflected declarations it finds convict
/// nothing — see `steins_infer::Folder::reflected_class`.
fn section_reflected_classes(files: &[ParsedFile], sidecar: Option<&mut Sidecar>) {
    let Some(sc) = sidecar else {
        return;
    };

    // Every class-like the project itself declares, by the same lowercased key the
    // index uses. Declared here means answered here — the engine is never asked.
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in files {
        for cd in f.tree.classes() {
            declared.insert(cd.fqn.clone());
        }
    }

    // The unanswered names, deduped, in first-encounter order so a report is stable
    // and its sample is the reader's own first few. Lowercased throughout: PHP class
    // names are case-insensitive, and it is the key both the index and the catalog
    // are written in. The engine's reply carries the declaration's own casing back.
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
        outln!(
            "  reflected class world: no unanswered class-like referenced — the project index and the builtin catalog cover this tree"
        );
        return;
    }

    let asked = unanswered.len().min(REFLECT_QUERY_CAP);
    let mut resolved: Vec<String> = Vec::new();
    for fqn in unanswered.iter().take(asked) {
        // A decline (`None`) and a not-found are both "not resolved here". Only a
        // declaration counts, and it is counted with the origin it arrived with.
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
        outln!(
            "  reflected class world: none of {} unanswered class-like name(s) is resident on this PHP{truncated}",
            unanswered.len()
        );
        return;
    }
    let shown = resolved.len().min(REFLECT_DISPLAY_CAP);
    let more = resolved.len() - shown;
    let tail = if more == 0 { String::new() } else { format!(", +{more} more") };
    outln!(
        "  reflected class world: {} of {} unanswered class-like name(s) resolved off the project's own PHP{truncated} — {}{tail}",
        resolved.len(),
        unanswered.len(),
        resolved[..shown].join(", ")
    );
    outln!(
        "    (a reflected declaration restores coverage only: it is the runtime's own claim, and no absence finding is premised on it — issue #269)"
    );
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
/// notice). An index scan (never the checker): count declarations carrying a written
/// `@throws` tag, then state whether the active surface checks them. This is the
/// designed answer to "wrote `@throws`, got silence".
fn section_envelopes(files: &[ParsedFile], surface: &profile::Surface) {
    outln!();
    outln!("Envelopes");
    let n = count_throws_envelopes(files);
    let checked = surface.surfaces_id(THROW_UNDECLARED_ID);
    if checked {
        outln!(
            "  {n} declaration(s) carry a written @throws — the active profile `{}` checks them (throw.undeclared on surface)",
            surface.name
        );
    } else {
        outln!(
            "  {n} written throw envelope(s); the active profile `{}` does not check them — the `contracts` (or `throws-direct`) profile does",
            surface.name
        );
    }
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

/// Section 6 — Baseline (ADR-0054 §9.5, minimal): the capture surface (profile + id
/// count from the header) versus the active surface, and the dormant-entry count
/// (entries whose id is outside the active surface — kept, not stale). Doctor accepts
/// `--baseline <path>`; absent that it discovers the conventional default file, and
/// reports "none" when neither resolves. An unparseable baseline file is a
/// configuration contradiction (exit 1, ADR-0054 §10).
fn section_baseline(cli_path: Option<&str>, surface: &profile::Surface, contradiction: &mut bool) {
    outln!();
    outln!("Baseline");

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
        outln!("  none (no baseline file; `check --set-baseline` writes one)");
        return;
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(_) => {
            // An explicit `--baseline` to a missing path is reported absent, not failed.
            outln!("  none ({} not readable)", file.display());
            return;
        }
    };

    // Unparseable = the header line is not even valid JSON (ADR-0054 §10 contradiction).
    // Entry lines stay hand-edit-tolerant (baseline::parse ignores unparsable ones).
    let header_ok = text
        .lines()
        .next()
        .is_some_and(|first| serde_json::from_str::<serde_json::Value>(first).is_ok());
    if !header_ok {
        outln!("  {}: UNPARSEABLE (header is not valid JSON)", file.display());
        outln!("  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
        *contradiction = true;
        return;
    }

    let entries = baseline::parse(&text);
    outln!("  file: {} ({} entr{})", file.display(), entries.len(), plural(entries.len()));

    match baseline::parse_header(&text) {
        Some(capture) => {
            outln!(
                "  capture surface: profile `{}`, {} id(s)",
                capture.profile,
                capture.ids.len()
            );
            outln!(
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
                outln!(
                    "  {dormant} dormant entr{} (id outside the active surface — kept, not stale)",
                    plural(dormant)
                );
            }
        }
        None => {
            // A pre-ADR-0050 header (no capture surface) is reported as such, not failed.
            outln!("  capture surface: none recorded (pre-capture-surface baseline header)");
        }
    }
}

/// `y`/`ies` suffix for "entr{}" — a tiny plain-text nicety.
fn plural(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}
