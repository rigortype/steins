//! End-to-end tests for the profile engine (ADR-0050 slice 2): the proof-only
//! default surface, the named opt-up stages (`contracts`, `throws-direct`), the
//! `origin` facet selector, exit levels (`fail`/`warn`), config errors, and
//! baseline capture-surface awareness.
//!
//! Each test runs the real `steins` binary in a private temp dir (its own CWD) so
//! the auto-loaded `steins.toml` and `.steins-baseline.jsonl` are isolated. Runs
//! use `--no-php` for determinism (the fixtures need no runtime folding).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-profile-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_in(dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// A fixture with **one proof** finding (`width("abc")` — type.argument-mismatch)
/// and **two contract** findings on `f()`: a DIRECT undeclared throw (RangeException
/// in its own body) and a PROPAGATED one (RuntimeException up the call to `g()`).
const MIXED: &str = "<?php\n\
    function width(int $w): int { return $w; }\n\
    width(\"abc\");\n\
    function g(): void { throw new \\RuntimeException(); }\n\
    /** @throws \\JsonException */\n\
    function f(): void { g(); throw new \\RangeException(); }\n";

/// A fixture with only a single DIRECT undeclared throw and no proof finding.
const THROW_ONLY: &str =
    "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\RuntimeException(); }\n";

// ------------------------------------------------------- default = proof only ---

#[test]
fn default_surface_is_proof_plus_mechanics_only() {
    let dir = workdir("default");
    write(&dir, "a.php", MIXED);
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 1, "the proof finding fails; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("type.argument-mismatch"), "proof shown, got:\n{}", r.stdout);
    assert!(
        !r.stdout.contains("throw.undeclared"),
        "contract layer must be OFF by default, got:\n{}",
        r.stdout
    );
}

#[test]
fn contracts_profile_opts_up_the_whole_contract_layer() {
    let dir = workdir("contracts");
    write(&dir, "a.php", MIXED);
    let r = run_in(&dir, &["check", "--no-php", "--profile", "contracts", "a.php"]);
    assert_eq!(r.code, 1);
    assert!(r.stdout.contains("type.argument-mismatch"), "proof still on");
    // Both throw findings present (direct RangeException + propagated RuntimeException).
    let n = r.stdout.matches("throw.undeclared").count();
    assert_eq!(n, 2, "both direct and propagated throws shown, got:\n{}", r.stdout);
    assert!(r.stdout.contains("RangeException can escape"));
    assert!(r.stdout.contains("RuntimeException can escape"));
}

#[test]
fn throws_direct_profile_selects_the_origin_facet() {
    let dir = workdir("throws-direct");
    write(&dir, "a.php", MIXED);
    let r = run_in(&dir, &["check", "--no-php", "--profile", "throws-direct", "a.php"]);
    assert_eq!(r.code, 1);
    // Proof on; exactly the DIRECT throw shown, the propagated one hidden.
    assert!(r.stdout.contains("type.argument-mismatch"), "proof still on");
    let n = r.stdout.matches("throw.undeclared").count();
    assert_eq!(n, 1, "only the direct throw shown, got:\n{}", r.stdout);
    assert!(r.stdout.contains("RangeException can escape"), "direct throw shown");
    assert!(!r.stdout.contains("RuntimeException can escape"), "propagated throw hidden");
}

// ---------------------------------------------------------- config selection ---

#[test]
fn config_selects_profile_and_flag_beats_config() {
    let dir = workdir("precedence");
    write(&dir, "a.php", MIXED);
    write(&dir, "steins.toml", "[check]\nprofile = \"contracts\"\n");

    // Config selects contracts → throws shown.
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert!(r.stdout.contains("throw.undeclared"), "config profile applied, got:\n{}", r.stdout);

    // Flag beats config → back to default, throws hidden.
    let r = run_in(&dir, &["check", "--no-php", "--profile", "default", "a.php"]);
    assert!(!r.stdout.contains("throw.undeclared"), "flag beats config, got:\n{}", r.stdout);
}

#[test]
fn unknown_profile_is_a_config_error() {
    let dir = workdir("unknown");
    write(&dir, "a.php", THROW_ONLY);
    let r = run_in(&dir, &["check", "--no-php", "--profile", "nope", "a.php"]);
    assert_eq!(r.code, 2, "unknown profile → exit 2");
    assert!(r.stderr.contains("unknown profile `nope`"), "got:\n{}", r.stderr);
}

#[test]
fn reserved_profile_name_is_a_config_error() {
    let dir = workdir("reserved");
    write(&dir, "a.php", THROW_ONLY);
    for name in ["strict", "boundary"] {
        let r = run_in(&dir, &["check", "--no-php", "--profile", name, "a.php"]);
        assert_eq!(r.code, 2, "reserved `{name}` → exit 2");
        assert!(r.stderr.contains("reserved name"), "got:\n{}", r.stderr);
    }
}

#[test]
fn user_profile_facet_token_is_rejected() {
    // The deferred-with-design decision (§4/§11): user profiles do not accept facet
    // selectors; a facet-shaped token is an unknown id pattern.
    let dir = workdir("facet-token");
    write(&dir, "a.php", THROW_ONLY);
    write(
        &dir,
        "steins.toml",
        "[check]\nprofile = \"p\"\n\n[profile.p]\nextends = \"default\"\nenable = [\"throw.undeclared@direct\"]\n",
    );
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 2, "facet token → config error");
    assert!(r.stderr.contains("throw.undeclared@direct"), "got:\n{}", r.stderr);
}

// --------------------------------------------------- [runtime] config errors ---

#[test]
fn unknown_runtime_key_is_a_hard_config_error() {
    // ADR-0050 §7 / ADR-0052 §5 N2: `[runtime]` uses `deny_unknown_fields`, so a
    // misspelled key fails the parse — a HARD config error (exit 2), not a
    // warn-and-proceed. The typo can never silently leave the safe default in force.
    let dir = workdir("runtime-typo");
    write(&dir, "a.php", THROW_ONLY);
    write(&dir, "steins.toml", "[runtime]\nzend-asertions = \"enabled\"\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 2, "unknown [runtime] key → exit 2; stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("parse error"), "names the parse failure, got:\n{}", r.stderr);
}

#[test]
fn valid_runtime_section_proceeds() {
    // The control: a well-formed `[runtime]` parses and the run proceeds normally.
    let dir = workdir("runtime-ok");
    write(&dir, "a.php", THROW_ONLY);
    write(&dir, "steins.toml", "[runtime]\nzend-assertions = \"enabled\"\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 0, "valid runtime + throw-only default surface → exit 0; stdout:\n{}", r.stdout);
}

#[test]
fn malformed_toml_is_a_hard_config_error() {
    // Any unparseable steins.toml the CLI reads is exit 2 (not silently ignored).
    let dir = workdir("toml-garbage");
    write(&dir, "a.php", THROW_ONLY);
    write(&dir, "steins.toml", "this is not = valid = toml [[[\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 2, "malformed steins.toml → exit 2; stderr:\n{}", r.stderr);
}

// -------------------------------------------------------------- exit levels ---

#[test]
fn warn_demotion_reports_without_failing() {
    let dir = workdir("warn");
    write(&dir, "a.php", THROW_ONLY);
    write(
        &dir,
        "steins.toml",
        "[check]\nprofile = \"migration\"\n\n[profile.migration]\nextends = \"contracts\"\nwarn = [\"throw.*\"]\n",
    );
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 0, "warn-only run exits 0; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("warning[throw.undeclared]"), "warn level printed, got:\n{}", r.stdout);
    assert!(!r.stdout.contains("error[throw.undeclared]"), "not error-level");
}

// -------------------------------------------------------------- json output ---

#[test]
fn json_carries_level_and_origin_facet() {
    let dir = workdir("json");
    write(&dir, "a.php", MIXED);
    let r = run_in(&dir, &["check", "--no-php", "--profile", "contracts", "--format", "json", "a.php"]);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert_eq!(v["profile"], "contracts");
    let arr = v["findings"].as_array().expect("findings array");
    // Every finding carries an additive level; proof findings carry no facet key.
    for d in arr {
        assert_eq!(d["level"], "fail", "fail by default (§7)");
    }
    // The throw findings carry the additive `origin` facet; the proof one does not.
    let throws: Vec<&serde_json::Value> =
        arr.iter().filter(|d| d["id"] == "throw.undeclared").collect();
    assert_eq!(throws.len(), 2);
    let origins: std::collections::HashSet<&str> =
        throws.iter().filter_map(|d| d["origin"].as_str()).collect();
    assert!(origins.contains("direct"), "direct facet present, got:\n{}", r.stdout);
    assert!(origins.contains("propagated"), "propagated facet present");
    // A proof finding carries no facet key.
    let proof = arr.iter().find(|d| d["id"] == "type.argument-mismatch").expect("proof finding");
    assert!(proof.get("origin").is_none(), "proof finding has no facet key");
}

// ------------------------------------------------- baseline capture surface ---

#[test]
fn baseline_captured_under_default_drowns_loudly_under_contracts() {
    let dir = workdir("baseline-notice");
    write(&dir, "a.php", MIXED);
    // Capture under the default surface (proof + mechanics only).
    let r = run_in(&dir, &["check", "--no-php", "--set-baseline", "a.php"]);
    assert_eq!(r.code, 0, "set-baseline exits 0");
    // Now run under contracts: the throw findings are unbaselined → drowns-loudly.
    let r = run_in(&dir, &["check", "--no-php", "--profile", "contracts", "a.php"]);
    assert!(
        r.stdout.contains("active profile `contracts`") && r.stdout.contains("did not"),
        "surface-exceeds notice printed, got:\n{}",
        r.stdout
    );
}

#[test]
fn out_of_surface_baseline_entries_are_dormant_not_stale() {
    let dir = workdir("dormant");
    write(&dir, "a.php", MIXED);
    // Capture under contracts: the baseline holds proof + throw entries.
    let r = run_in(&dir, &["check", "--no-php", "--profile", "contracts", "--set-baseline", "a.php"]);
    assert_eq!(r.code, 0);
    // Run under default: the throw entries are outside the surface → dormant, NOT
    // stale. The proof entry still matches, so there is nothing to report.
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert!(
        !r.stdout.contains("no longer match"),
        "dormant entries must not be reported stale, got:\n{}",
        r.stdout
    );
    assert_eq!(r.code, 0, "proof finding is baselined; exit 0");
}

// -------------------------------------------------- ADR-0053 D4: var_dump lane ---

/// A fixture whose ONLY finding is a default-on `var_dump` dump.
const VAR_DUMP_ONLY: &str = "<?php\n$x = 5;\nvar_dump($x);\n";

#[test]
fn var_dump_dumps_by_default_and_is_exit_neutral() {
    // ADR-0053 §3/§4: `var_dump` reports on a bare `check` (default-ON, every
    // profile), at warn level — so a run whose only findings are var_dump dumps
    // exits 0 (exit-neutral forever). The dump is warn, never a lint red.
    let dir = workdir("vardump");
    write(&dir, "a.php", VAR_DUMP_ONLY);
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert!(r.stdout.contains("warning[debug.var-dump]"), "var_dump dumps by default:\n{}", r.stdout);
    assert!(r.stdout.contains("dumped type: int"), "renders the fact:\n{}", r.stdout);
    assert_eq!(r.code, 0, "a var_dump-only run is exit-neutral; stdout:\n{}", r.stdout);
}

#[test]
fn var_dump_json_carries_debug_layer_and_warn_level() {
    let dir = workdir("vardump-json");
    write(&dir, "a.php", VAR_DUMP_ONLY);
    let r = run_in(&dir, &["check", "--no-php", "--format", "json", "a.php"]);
    assert!(r.stdout.contains("\"id\": \"debug.var-dump\""), "{}", r.stdout);
    assert!(r.stdout.contains("\"layer\": \"debug\""), "{}", r.stdout);
    assert!(r.stdout.contains("\"level\": \"warn\""), "{}", r.stdout);
    assert_eq!(r.code, 0);
}

#[test]
fn var_dump_is_profile_disableable() {
    // ADR-0053 §4: a named profile `disable = ["debug.var-dump"]` turns the incidental
    // dump off — the relief valve for a team drowning in legacy sites.
    let dir = workdir("vardump-off");
    write(&dir, "a.php", VAR_DUMP_ONLY);
    write(
        &dir,
        "steins.toml",
        "[profile.quiet]\nextends = \"default\"\ndisable = [\"debug.var-dump\"]\n",
    );
    let r = run_in(&dir, &["check", "--no-php", "--profile", "quiet", "a.php"]);
    assert!(!r.stdout.contains("debug.var-dump"), "disabled var_dump must not display:\n{}", r.stdout);
    assert_eq!(r.code, 0);
}

#[test]
fn an_inline_ignore_never_suppresses_a_dump() {
    // ADR-0053 §4: the debug lane is exempt from all three suppression channels. An
    // `@steins-ignore debug.var-dump` does NOT mute the dump (it still displays) and,
    // matching nothing suppressible, earns `suppress.unmatched` — the anti-rot channel
    // doing its normal job.
    let dir = workdir("vardump-ignore");
    write(&dir, "a.php", "<?php\n$x = 5;\nvar_dump($x); // @steins-ignore debug.var-dump\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert!(r.stdout.contains("debug.var-dump"), "the dump is NOT suppressed:\n{}", r.stdout);
    assert!(
        r.stdout.contains("suppress.unmatched"),
        "an ignore naming a dump earns suppress.unmatched:\n{}",
        r.stdout
    );
    // suppress.unmatched is mechanics (fail-level), so this run exits 1 — on the
    // meta-diagnostic, not the dump.
    assert_eq!(r.code, 1, "the unmatched-ignore mechanics finding fails; stdout:\n{}", r.stdout);
}
