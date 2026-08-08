//! `--format github` and CI auto-detection (ADR-0054 Part I, slice C1).
//!
//! Three things are pinned here, and they are the three the ADR argues for:
//!
//! 1. **The workflow command** (§4) — one `::error|::warning|::notice` line per
//!    displayed finding, `title` carrying the id, plus the same plain accounting
//!    `text` prints.
//! 2. **The level mapping** (§3) — the exit level decides, with the debug lane's
//!    carve-out: a warn-fixed dump is `::notice`, and neither dump is *omitted*
//!    (§13 refuses that outright — an invisible CI-red is a serializer lying
//!    about its own exit).
//! 3. **Detection** (§6) — `GITHUB_ACTIONS` selects `github`, an explicit
//!    `--format` always wins, a generic `CI=true` selects nothing, and detection
//!    changes *only the spelling*: same findings, same exit code.
//!
//! Plus §1's format invariance: the four spellings render one multiset and one
//! exit code.
//!
//! Each test runs the real binary in a private temp dir (its own CWD) so an
//! auto-loaded `steins.toml` / `.steins-baseline.jsonl` is isolated, and with
//! `--no-php` for determinism.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-ghfmt-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
}

/// Run `steins` with the CI environment stated explicitly. `env` is the
/// `(name, value)` pairs to SET; `GITHUB_ACTIONS` and `CI` are always scrubbed
/// first, so a test run on GitHub Actions sees exactly the environment it asked
/// for and nothing it inherited.
fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(dir).env_remove("GITHUB_ACTIONS").env_remove("CI");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn run_in(dir: &Path, args: &[&str]) -> Run {
    run_env(dir, args, &[])
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// One proof finding: `width("abc")` is a proven TypeError.
const PROOF: &str = "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n";

/// One direct undeclared throw — contract layer, reached via a named profile.
const THROW_ONLY: &str =
    "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\RuntimeException(); }\n";

/// A `var_dump` (warn-fixed debug lane) beside a `\PHPStan\dumpType` (fail-fixed).
const DUMPS: &str =
    "<?php\n$x = 5;\nvar_dump($x);\n\\PHPStan\\dumpType($x);\n";

// ------------------------------------------------------- the workflow command ---

#[test]
fn one_command_per_displayed_finding() {
    let dir = workdir("command");
    write(&dir, "a.php", PROOF);
    let r = run_in(&dir, &["check", "--no-php", "--format", "github", "a.php"]);
    assert_eq!(r.code, 1, "the proof finding fails; stdout:\n{}", r.stdout);
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one annotation, got:\n{}", r.stdout);
    // ADR-0054 §4's shape, verbatim: `::error file=…,line=…,col=…,title=…::message`.
    assert!(
        lines[0].starts_with("::error file=a.php,line=3,col="),
        "the §4 command shape, got:\n{}",
        lines[0]
    );
    assert!(
        lines[0].contains(",title=type.argument-mismatch::"),
        "the id rides in `title`, got:\n{}",
        lines[0]
    );
    // The message rides after the `::`, verbatim (wording is not a contract,
    // ADR-0023 — the id is).
    assert!(lines[0].contains("cannot become int $w"), "message carried, got:\n{}", lines[0]);
}

#[test]
fn warn_level_is_a_warning_command() {
    // ADR-0054 §3: the level keys on ADR-0050 §7's level. A profile `warn = [...]`
    // demotion prints `warning[…]` in text and must print `::warning` here.
    let dir = workdir("warn");
    write(&dir, "a.php", THROW_ONLY);
    write(
        &dir,
        "steins.toml",
        "[check]\nprofile = \"migration\"\n\n[profile.migration]\nextends = \"contracts\"\nwarn = [\"throw.*\"]\n",
    );
    let r = run_in(&dir, &["check", "--no-php", "--format", "github", "a.php"]);
    assert_eq!(r.code, 0, "warn-only run exits 0 in every format; stdout:\n{}", r.stdout);
    assert!(r.stdout.starts_with("::warning file=a.php,"), "warn → ::warning, got:\n{}", r.stdout);
    assert!(r.stdout.contains("title=throw.undeclared::"), "id in title, got:\n{}", r.stdout);
}

#[test]
fn the_debug_lane_is_carried_never_omitted() {
    // ADR-0054 §3 and §13's refusal, together: the explicit dump pair is
    // fail-fixed and reds CI, so it is annotated as `::error` — omitting it would
    // leave a red run with nothing explaining it. `var_dump` is warn-fixed and an
    // *answer to a question the code asked*, so it takes `::notice`, not
    // `::warning` and not silence.
    let dir = workdir("debug");
    write(&dir, "a.php", DUMPS);
    let r = run_in(&dir, &["check", "--no-php", "--format", "github", "a.php"]);
    assert_eq!(r.code, 1, "the fail-fixed dump reds the run; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("::notice file=a.php,line=3,") && r.stdout.contains("title=debug.var-dump::"),
        "var_dump → ::notice, got:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("::error file=a.php,line=4,") && r.stdout.contains("title=debug.type::"),
        "the explicit dump → ::error, got:\n{}",
        r.stdout
    );
}

#[test]
fn the_plain_accounting_follows_the_commands() {
    // ADR-0054 §4: after the commands, the same plain accounting lines `text`
    // prints — inert in a workflow log, and the accounting must not become
    // format-dependent.
    let dir = workdir("accounting");
    write(&dir, "a.php", PROOF);
    let set = run_in(&dir, &["check", "--no-php", "--set-baseline", "a.php"]);
    assert_eq!(set.code, 0, "baseline written");
    let r = run_in(&dir, &["check", "--no-php", "--format", "github", "a.php"]);
    assert_eq!(r.code, 0, "everything baselined → exit 0; stdout:\n{}", r.stdout);
    assert_eq!(r.stdout, "1 findings in baseline\n", "plain accounting, got:\n{}", r.stdout);
}

// ------------------------------------------------------------------ detection ---

#[test]
fn github_actions_selects_the_github_spelling() {
    let dir = workdir("detect");
    write(&dir, "a.php", PROOF);
    let r = run_env(&dir, &["check", "--no-php", "a.php"], &[("GITHUB_ACTIONS", "true")]);
    assert_eq!(r.code, 1);
    assert!(r.stdout.starts_with("::error file=a.php,"), "detected github, got:\n{}", r.stdout);
}

#[test]
fn an_explicit_format_always_wins() {
    let dir = workdir("explicit");
    write(&dir, "a.php", PROOF);
    for (flag, expect) in [("text", "a.php:3:"), ("json", "{\n  \"findings\"")] {
        let r = run_env(
            &dir,
            &["check", "--no-php", "--format", flag, "a.php"],
            &[("GITHUB_ACTIONS", "true")],
        );
        assert!(r.stdout.starts_with(expect), "--format {flag} wins, got:\n{}", r.stdout);
        assert!(!r.stdout.contains("::error"), "no workflow command, got:\n{}", r.stdout);
    }
    // Including the explicit `--format github` outside Actions, the other side of
    // the same rule.
    let r = run_in(&dir, &["check", "--no-php", "--format", "github", "a.php"]);
    assert!(r.stdout.starts_with("::error"), "explicit github off-CI, got:\n{}", r.stdout);
}

#[test]
fn a_generic_ci_signal_detects_nothing() {
    // ADR-0054 §13: no generic `CI=true` detection and no per-CI format zoo —
    // detection selects a rendering the environment can *consume*, and "some CI"
    // names none. `text` is already correct there.
    let dir = workdir("ci");
    write(&dir, "a.php", PROOF);
    for env in [
        &[("CI", "true")][..],
        &[("GITHUB_ACTIONS", "false")][..],
        &[("GITHUB_ACTIONS", "")][..],
    ] {
        let r = run_env(&dir, &["check", "--no-php", "a.php"], env);
        assert!(r.stdout.starts_with("a.php:3:"), "text stays, got:\n{}", r.stdout);
    }
}

#[test]
fn detection_changes_only_the_spelling() {
    // ADR-0054 §6: everything else about the run — surface, profile, pipeline,
    // exit code — is untouched by detection. Format invariance (§1) is what makes
    // that checkable, so it is checked rather than asserted.
    let dir = workdir("invariant-detect");
    write(&dir, "a.php", DUMPS);
    let plain = run_in(&dir, &["check", "--no-php", "a.php"]);
    let detected = run_env(&dir, &["check", "--no-php", "a.php"], &[("GITHUB_ACTIONS", "true")]);
    assert_eq!(plain.code, detected.code, "same exit code");
    assert_eq!(positions_text(&plain.stdout), positions_github(&detected.stdout));
}

// --------------------------------------------------------- format invariance ---

/// `(id, path, line, column)` for every `text` finding line.
fn positions_text(stdout: &str) -> BTreeSet<String> {
    stdout
        .lines()
        .filter_map(|l| {
            let (pos, rest) = l.split_once(": ")?;
            let (kind, rest) = rest.split_once('[')?;
            if kind != "error" && kind != "warning" {
                return None;
            }
            let (id, _) = rest.split_once(']')?;
            let mut it = pos.rsplitn(3, ':');
            let column = it.next()?;
            let line = it.next()?;
            let path = it.next()?;
            Some(format!("{id}|{path}|{line}|{column}"))
        })
        .collect()
}

/// The same, from workflow commands.
fn positions_github(stdout: &str) -> BTreeSet<String> {
    stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("::")?;
            let (head, _) = rest.split_once("::")?;
            let (_, props) = head.split_once(' ')?;
            let mut path = "";
            let mut line = "";
            let mut column = "";
            let mut id = "";
            for kv in props.split(',') {
                let (k, v) = kv.split_once('=')?;
                match k {
                    "file" => path = v,
                    "line" => line = v,
                    "col" => column = v,
                    "title" => id = v,
                    _ => return None,
                }
            }
            Some(format!("{id}|{path}|{line}|{column}"))
        })
        .collect()
}

/// The same, from the `json` document.
fn positions_json(stdout: &str) -> BTreeSet<String> {
    let v: serde_json::Value = serde_json::from_str(stdout).expect("valid json document");
    v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|d| {
            format!(
                "{}|{}|{}|{}",
                d["id"].as_str().unwrap(),
                d["path"].as_str().unwrap(),
                d["line"],
                d["column"]
            )
        })
        .collect()
}

#[test]
fn every_format_renders_one_multiset_and_one_exit_code() {
    // ADR-0054 §1, the binding invariant: for a fixed invocation every format
    // renders the same displayed finding multiset and produces the same exit
    // code. The fixture deliberately spans three levels — a fail-level proof
    // finding, a fail-fixed dump and a warn-fixed dump — so a format that dropped
    // the "merely informational" one would be caught.
    let dir = workdir("invariance");
    write(&dir, "a.php", &format!("{PROOF}{}", &DUMPS[6..]));
    let text = run_in(&dir, &["check", "--no-php", "--format", "text", "a.php"]);
    let json = run_in(&dir, &["check", "--no-php", "--format", "json", "a.php"]);
    let github = run_in(&dir, &["check", "--no-php", "--format", "github", "a.php"]);
    assert_eq!(text.code, 1, "stdout:\n{}", text.stdout);
    assert_eq!(text.code, json.code, "json exit");
    assert_eq!(text.code, github.code, "github exit");
    let expected = positions_text(&text.stdout);
    assert_eq!(expected.len(), 3, "three findings span the levels: {expected:?}");
    assert_eq!(expected, positions_json(&json.stdout), "json multiset");
    assert_eq!(expected, positions_github(&github.stdout), "github multiset");
}

// ----------------------------------------------------------------- usage ---

#[test]
fn an_unknown_format_is_a_usage_error() {
    let dir = workdir("usage");
    write(&dir, "a.php", PROOF);
    let out = Command::new(bin())
        .args(["check", "--no-php", "--format", "gitlab", "a.php"])
        .current_dir(&dir)
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run steins");
    assert_eq!(out.status.code(), Some(2), "unknown format → exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown format `gitlab`"), "names it, got:\n{stderr}");
    assert!(stderr.contains("text|json|github"), "lists the formats, got:\n{stderr}");
}
