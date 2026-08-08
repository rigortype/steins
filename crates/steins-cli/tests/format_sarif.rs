//! `--format sarif` (ADR-0054 Part I §2, slice C2).
//!
//! The committed schema shape is pinned as a **snapshot fixture**
//! (`tests/fixtures/sarif/basic.sarif.json`) rather than as a pile of field
//! assertions: SARIF's value to a user is that an ingestion service reads it, and
//! what breaks such a consumer is a field that quietly changed shape, not one a
//! test forgot to look at. The snapshot sees everything. Structural properties
//! that a snapshot cannot state — the rule table is deduped and sorted, every
//! `ruleIndex` points at its own rule, the ADR's refusals hold — are asserted
//! beside it.
//!
//! `semanticVersion` is the one field that legitimately changes without anyone
//! deciding anything, so the fixture carries `{{VERSION}}` and the test
//! substitutes the crate version. Everything else is verbatim: if it moves,
//! somebody moved it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-sarif-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
}

fn run_in(dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sarif").join(name);
    std::fs::read_to_string(p).expect("read snapshot fixture")
}

/// A proof finding (fail → `error`) and a `var_dump` (warn-fixed debug lane →
/// `note`), so one snapshot covers both ends of the §3 mapping.
const MIXED: &str = "<?php\n\
    function width(int $w): int { return $w; }\n\
    width(\"abc\");\n\
    var_dump(1);\n";

/// A direct undeclared throw, which carries the ADR-0050 §4 `origin` facet.
const THROW_ONLY: &str =
    "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\RuntimeException(); }\n";

fn sarif_of(dir: &Path, args: &[&str]) -> serde_json::Value {
    let r = run_in(dir, args);
    serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("valid SARIF json: {e}\n{}", r.stdout))
}

// ------------------------------------------------------------ the snapshot ---

#[test]
fn the_committed_schema_shape() {
    let dir = workdir("snapshot");
    write(&dir, "a.php", MIXED);
    let r = run_in(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    let expected = fixture("basic.sarif.json").replace("{{VERSION}}", env!("CARGO_PKG_VERSION"));
    assert_eq!(r.stdout, expected, "the committed SARIF shape drifted");
    // ADR-0050 §7 is identity, not a per-format decision: one fail-level finding
    // displays, so `sarif` exits 1 exactly as `text` does. ADR-0054 §13 refuses
    // `--exit-zero` — "upload anyway" is the workflow's `continue-on-error`.
    assert_eq!(r.code, 1, "surfaced means fail, in every format");
}

// ------------------------------------------------------------ the rule table ---

#[test]
fn rules_are_the_displayed_ids_deduped_and_sorted() {
    // ADR-0054 §2: one `reportingDescriptor` per id present in the displayed
    // results — not the full registry, not the surface's capture set (which has
    // exactly one carrier already, the baseline capture header).
    let dir = workdir("rules");
    write(&dir, "a.php", "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\nwidth(\"def\");\nvar_dump(1);\n");
    let v = sarif_of(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    let run = &v["runs"][0];
    let rules = run["rules"].as_array().or_else(|| run["tool"]["driver"]["rules"].as_array());
    let rules = rules.expect("rules array");
    let ids: Vec<&str> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["debug.var-dump", "type.argument-mismatch"], "deduped and sorted");
    // Three results, two rules: the duplicate id is one rule.
    let results = run["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3, "every displayed finding is a result");
    // Every `ruleIndex` points at its own `ruleId`.
    for res in results {
        let idx = res["ruleIndex"].as_u64().expect("ruleIndex") as usize;
        assert_eq!(rules[idx]["id"], res["ruleId"], "ruleIndex agrees with ruleId");
    }
    // The registry is much larger than two ids; the log carries only what it
    // referenced.
    assert!(rules.len() < 5, "not the whole registry: {ids:?}");
}

#[test]
fn the_run_carries_its_profile_and_the_accounting_envelope() {
    let dir = workdir("envelope");
    write(&dir, "a.php", MIXED);
    let set = run_in(&dir, &["check", "--no-php", "--set-baseline", "a.php"]);
    assert_eq!(set.code, 0, "baseline written");
    let v = sarif_of(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    let run = &v["runs"][0];
    // Parallel uploads under different profiles must not clobber each other's
    // alert categories (§2).
    assert_eq!(run["automationDetails"]["id"], "steins/default");
    assert_eq!(run["properties"]["profile"], "default");
    assert_eq!(run["properties"]["baselined"], 1, "counts, from the same envelope json carries");
    assert_eq!(run["properties"]["vendorSuppressed"], 0);
    assert_eq!(run["properties"]["suppressed"], 0);
    // §7/§13: the suppression machinery stays unused. A baselined finding is a
    // count, never a `suppressions` entry — a format that re-surfaced suppressed
    // findings would be a fourth suppression channel, and would leak the
    // baseline's contents into every upload.
    assert!(run.get("suppressions").is_none(), "no run-level suppressions");
    for res in run["results"].as_array().expect("results") {
        assert!(res.get("suppressions").is_none(), "no per-result suppressions");
    }
    // The `var_dump` is exempt from the baseline (ADR-0053 §8), so it still
    // reports — and the run is exit-neutral because it is warn-fixed.
    let ids: Vec<&str> =
        run["results"].as_array().unwrap().iter().map(|r| r["ruleId"].as_str().unwrap()).collect();
    assert_eq!(ids, ["debug.var-dump"], "the baselined proof finding is gone, the dump is not");
}

// ------------------------------------------------------------- the mapping ---

#[test]
fn the_debug_lane_is_carried_at_its_own_level() {
    // ADR-0054 §3 and §13: `debug.var-dump` is `note` — not `warning`, which
    // would misstate an answered question as a softly-surfaced claim, and not
    // absent, which would hide the cause of a red run. The fail-fixed pair is
    // `error`.
    let dir = workdir("debug");
    write(&dir, "a.php", "<?php\n$x = 5;\nvar_dump($x);\n\\PHPStan\\dumpType($x);\n");
    let v = sarif_of(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    let results = v["runs"][0]["results"].as_array().expect("results");
    let level = |id: &str| {
        results
            .iter()
            .find(|r| r["ruleId"] == id)
            .unwrap_or_else(|| panic!("{id} carried, never omitted"))["level"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(level("debug.var-dump"), "note");
    assert_eq!(level("debug.type"), "error");
}

#[test]
fn a_warn_demoted_id_is_a_warning() {
    let dir = workdir("warn");
    write(&dir, "a.php", THROW_ONLY);
    write(
        &dir,
        "steins.toml",
        "[check]\nprofile = \"migration\"\n\n[profile.migration]\nextends = \"contracts\"\nwarn = [\"throw.*\"]\n",
    );
    let r = run_in(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    assert_eq!(r.code, 0, "warn-only run exits 0 in every format");
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid SARIF");
    let run = &v["runs"][0];
    assert_eq!(run["automationDetails"]["id"], "steins/migration");
    assert_eq!(run["results"][0]["level"], "warning");
    assert_eq!(run["tool"]["driver"]["rules"][0]["defaultConfiguration"]["level"], "warning");
}

#[test]
fn a_registry_declared_facet_rides_properties() {
    // ADR-0050 §4, mirroring `json`: `"origin": "direct"` on the ids that
    // declare a facet, absent on the ids that do not.
    let dir = workdir("facet");
    write(&dir, "a.php", THROW_ONLY);
    let v = sarif_of(&dir, &["check", "--no-php", "--profile", "contracts", "--format", "sarif", "a.php"]);
    let res = &v["runs"][0]["results"][0];
    assert_eq!(res["ruleId"], "throw.undeclared");
    assert_eq!(res["properties"]["origin"], "direct");
}

// -------------------------------------------------------- fingerprints ---

#[test]
fn fingerprints_survive_an_unrelated_edit() {
    // ADR-0054 §2: `partialFingerprints` reuses the ADR-0022 baseline hash. The
    // hash exists precisely so identity survives unrelated edits — which is what
    // alert tracking across runs needs, so the two consumers share one identity
    // function. Adding a line far from the finding must not move it.
    let dir = workdir("fingerprint");
    write(&dir, "a.php", MIXED);
    let before = sarif_of(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    let fp = |v: &serde_json::Value| {
        v["runs"][0]["results"][0]["partialFingerprints"]["steinsFindingHash/v1"]
            .as_str()
            .expect("fingerprint")
            .to_owned()
    };
    let original = fp(&before);
    assert_eq!(original.len(), 16, "the ADR-0022 hash, verbatim");

    write(&dir, "a.php", &format!("{MIXED}function unrelated(): void {{}}\n"));
    let after = sarif_of(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    assert_eq!(fp(&after), original, "an unrelated edit does not move the fingerprint");

    // And it *does* move when the flagged line's own neighborhood changes — that
    // is the same intentional break the baseline has.
    write(&dir, "a.php", &MIXED.replace("width(\"abc\");", "width(\"abcd\");"));
    let changed = sarif_of(&dir, &["check", "--no-php", "--format", "sarif", "a.php"]);
    assert_ne!(fp(&changed), original, "the flagged line changed");
}

// ------------------------------------------------------------------ usage ---

#[test]
fn sarif_is_never_auto_selected() {
    // ADR-0054 §6: `sarif` is a file artifact chosen deliberately for an upload
    // step, not a log rendering — detection never picks it.
    let dir = workdir("no-autodetect");
    write(&dir, "a.php", MIXED);
    let out = Command::new(bin())
        .args(["check", "--no-php", "a.php"])
        .current_dir(&dir)
        .env("GITHUB_ACTIONS", "true")
        .output()
        .expect("run steins");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("::error"), "Actions gets github, not sarif:\n{stdout}");
}
