//! `steins doctor` (ADR-0054 Part II, slice C3 — the v0.1.0 MINIMAL scope).
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
//! * **2** — doctor's own usage errors.
//!
//! # v0.1.0 minimal scope (owner-decided landing point)
//!
//! Four sections: Runtime (sidecar/PHP health + SAPI + extension count, the
//! monkey-patch line), Config + active surface, Envelopes (the G1-demote
//! written-but-unchecked notice), and Baseline. The full ADR-0054 §9 sections
//! (Coverage posture with dam statistics, Catalog skew, Registry totality, the
//! SAPI-undeclared A6 line, `[runtime]` pseudo-constant reporting) are **v0.1.x** —
//! deferred, not built here. `doctor --format json` is likewise deferred with design
//! (§14: the section structure is the schema; it ships when a consumer exists).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use steins_infer::{MONKEY_PATCH_EXTENSIONS, SOUND_SUBSET_NOTICE, THROW_UNDECLARED_ID};
use steins_phpdoc::{TagKind, scan_docblock};
use steins_sidecar::Sidecar;
use steins_syntax::SourceTree;

use crate::baseline;
use crate::profile;

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
                    eprintln!("steins: --baseline requires a path argument");
                    return ExitCode::from(2);
                };
                baseline_path = Some(value.clone());
                i += 2;
            }
            other if other.starts_with('-') => {
                eprintln!("steins: unknown flag `{other}` for doctor");
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
            eprintln!(
                "steins: doctor takes at most one path (usage: steins doctor [--no-php] [--baseline <path>] [path])"
            );
            return ExitCode::from(2);
        }
    };

    // Environment facts report at exit 0 (ADR-0054 §10); a configuration the world
    // refutes flips this and exits 1.
    let mut contradiction = false;

    println!("steins doctor — posture report (index-bound; runs no checks)");

    section_runtime(no_php);
    let surface = section_config(&mut contradiction);
    section_envelopes(&root, &surface);
    section_baseline(baseline_path.as_deref(), &surface, &mut contradiction);

    if contradiction {
        // ExitCode::FAILURE == 1: the doctor config-contradiction code (ADR-0054 §10).
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Section 1 — Runtime (ADR-0054 §9.1, minimal): sidecar spawn health, PHP version,
/// SAPI, loaded-extension count, and the monkey-patch line (ADR-0049 A9). No
/// reachable PHP is the sound-subset posture (ADR-0004): named loudly, exit 0.
fn section_runtime(no_php: bool) {
    println!();
    println!("Runtime");
    if no_php {
        println!("  PHP sidecar: disabled (--no-php)");
        println!("  posture: sound subset — findings that require executing PHP are omitted");
        println!("  (a degraded environment is not a failure — exit stays 0, ADR-0004)");
        return;
    }
    match Sidecar::spawn() {
        Ok(mut sc) => match sc.env() {
            Some(env) => {
                println!("  PHP sidecar: spawned ok");
                println!("  PHP version: {}", env.php_version);
                println!("  SAPI: {}", env.sapi);
                println!("  loaded extensions: {}", env.extensions.len());
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
                    println!(
                        "  monkey-patch extension(s) loaded: {} — the entire absence-proof family is Unknown-silent this run (ADR-0049 A9)",
                        present.join(", ")
                    );
                }
            }
            None => {
                println!("  PHP sidecar: spawned, but the env() query failed");
                println!(
                    "  posture: sound subset (degraded) — findings that require executing PHP are omitted (exit 0, ADR-0004)"
                );
            }
        },
        Err(_) => {
            println!("  PHP sidecar: not spawnable (no `php` on PATH)");
            println!("  {SOUND_SUBSET_NOTICE}");
            println!("  (a degraded environment is not a failure — exit stays 0, ADR-0004)");
        }
    }
}

/// Section 2 — Config + active surface (ADR-0054 §9.3/§9.4, minimal). Returns the
/// resolved display surface for the later sections. An unparseable `steins.toml` or a
/// profile-resolution error is a configuration contradiction (`*contradiction =
/// true`, exit 1); the section still renders on the built-in `default` surface so the
/// rest of the report is produced.
fn section_config(contradiction: &mut bool) -> profile::Surface {
    println!();
    println!("Config + active surface");

    let config = match crate::read_steins_config() {
        Ok(c) => c,
        Err(e) => {
            println!("  steins.toml: PARSE ERROR — {e}");
            println!("  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
            *contradiction = true;
            None
        }
    };
    let (check_cfg, profile_tbl) = match config {
        Some(c) => {
            println!("  steins.toml: found");
            (c.check, c.profile)
        }
        None => {
            // A genuine absence (not the parse-error fallback, which already printed).
            if !*contradiction {
                println!("  steins.toml: not found (built-in defaults govern)");
            }
            (None, None)
        }
    };

    let (config_profile, profile_configs) = crate::profiles_from_config(check_cfg, profile_tbl);
    let provenance = if config_profile.is_some() { "[check] profile" } else { "built-in default" };
    let surface = match profile_configs.resolve(config_profile.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            println!("  profile resolution: ERROR — {e}");
            println!("  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
            *contradiction = true;
            // Fall back to the built-in default surface so the remaining sections
            // render; the run already exits 1 on the contradiction.
            profile::ProfileConfigs::default()
                .resolve(None)
                .expect("the built-in default profile always resolves")
        }
    };
    println!("  active profile: `{}` (from {provenance})", surface.name);
    let layers = surface.layers_on();
    println!(
        "  surface: layers [{}], {} checked id(s)",
        layers.join(", "),
        surface.surface_ids().len()
    );
    surface
}

/// Section 3 — Envelopes (ADR-0054 §9.4, the G1-amendment written-but-unchecked
/// notice). An index scan (never the checker): count declarations carrying a written
/// `@throws` tag, then state whether the active surface checks them. This is the
/// designed answer to "wrote `@throws`, got silence".
fn section_envelopes(root: &Path, surface: &profile::Surface) {
    println!();
    println!("Envelopes");
    let n = count_throws_envelopes(root);
    let checked = surface.surfaces_id(THROW_UNDECLARED_ID);
    if checked {
        println!(
            "  {n} declaration(s) carry a written @throws — the active profile `{}` checks them (throw.undeclared on surface)",
            surface.name
        );
    } else {
        println!(
            "  {n} written throw envelope(s); the active profile `{}` does not check them — the `contracts` (or `throws-direct`) profile does",
            surface.name
        );
    }
}

/// Count declarations (functions + methods) that carry a written `@throws` tag, by
/// scanning parsed docblocks across every `.php` file under `root`. Index-bound: it
/// parses source and reads docblock trivia; it runs no inference.
fn count_throws_envelopes(root: &Path) -> usize {
    let mut files = Vec::new();
    crate::collect_php_files(root, &mut files);
    files.sort();
    files.dedup();

    let mut count = 0usize;
    for file in &files {
        let Ok(bytes) = std::fs::read(file) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let tree = SourceTree::parse(&text);
        for f in tree.functions() {
            if declares_throws(f.docblock.as_deref()) {
                count += 1;
            }
        }
        for c in tree.classes() {
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

/// Section 4 — Baseline (ADR-0054 §9.5, minimal): the capture surface (profile + id
/// count from the header) versus the active surface, and the dormant-entry count
/// (entries whose id is outside the active surface — kept, not stale). Doctor accepts
/// `--baseline <path>`; absent that it discovers the conventional default file, and
/// reports "none" when neither resolves. An unparseable baseline file is a
/// configuration contradiction (exit 1, ADR-0054 §10).
fn section_baseline(cli_path: Option<&str>, surface: &profile::Surface, contradiction: &mut bool) {
    println!();
    println!("Baseline");

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
        println!("  none (no baseline file; `check --set-baseline` writes one)");
        return;
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(_) => {
            // An explicit `--baseline` to a missing path is reported absent, not failed.
            println!("  none ({} not readable)", file.display());
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
        println!("  {}: UNPARSEABLE (header is not valid JSON)", file.display());
        println!("  (configuration contradiction — doctor exits 1, ADR-0054 §10)");
        *contradiction = true;
        return;
    }

    let entries = baseline::parse(&text);
    println!("  file: {} ({} entr{})", file.display(), entries.len(), plural(entries.len()));

    match baseline::parse_header(&text) {
        Some(capture) => {
            println!(
                "  capture surface: profile `{}`, {} id(s)",
                capture.profile,
                capture.ids.len()
            );
            println!(
                "  active surface: profile `{}`, {} id(s)",
                surface.name,
                surface.surface_ids().len()
            );
            // Dormant (ADR-0050 §8): an entry whose id is outside the ACTIVE surface —
            // kept, not stale, because this profile simply never looks for it.
            let dormant = entries.iter().filter(|e| !surface.surfaces_id(&e.id)).count();
            if dormant > 0 {
                println!(
                    "  {dormant} dormant entr{} (id outside the active surface — kept, not stale)",
                    plural(dormant)
                );
            }
        }
        None => {
            // A pre-ADR-0050 header (no capture surface) is reported as such, not failed.
            println!("  capture surface: none recorded (pre-capture-surface baseline header)");
        }
    }
}

/// `y`/`ies` suffix for "entr{}" — a tiny plain-text nicety.
fn plural(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}
