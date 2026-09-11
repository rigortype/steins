//! End-to-end tests for `steins triage` (issue #50, ADR-0095): golden text and
//! JSON over a synthetic `check --format json` stream, the run-over-paths
//! arm (exit `0` whatever `check` found), the strict what-if, the usage
//! errors, and one deletion test per recognizer and per guard-status bucket —
//! a stream that fires exactly that row, so removing the row fails a test.
//!
//! The synthetic streams live in `tests/fixtures/triage/`; the PHP project the
//! what-if tests analyze is written into a private temp dir, `--no-php` and
//! `--no-cache` for determinism.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triage")
}

/// Every run scrubs `GITHUB_ACTIONS`: the child `check` names `--format json`
/// explicitly, but a stray detection elsewhere must not drift a recording.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-triage-{}-{tag}-{n}", std::process::id()));
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
    let out = steins_cmd().args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Run with `stdin` fed from `input` (the `--input -` arm).
fn run_with_stdin(dir: &Path, args: &[&str], input: &str) -> Run {
    use std::io::Write as _;
    let mut child = steins_cmd()
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn steins");
    child.stdin.take().expect("stdin").write_all(input.as_bytes()).expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn json(run: &Run) -> serde_json::Value {
    serde_json::from_str(&run.stdout).unwrap_or_else(|e| panic!("json: {e}\n{}", run.stdout))
}

/// A one-file check document with `findings` and `profile`, for the deletion tests.
fn stream(profile: &str, findings: &[(&str, &str)], baselined: usize) -> String {
    let items: Vec<String> = findings
        .iter()
        .map(|(id, path)| {
            format!(
                r#"{{"id":"{id}","layer":"proof","level":"fail","path":"{path}","line":1,"column":1,"message":"m"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"findings":[{}],"profile":"{profile}","vendor_suppressed":0,"suppressed":0,"baselined":{baselined}}}"#,
        items.join(",")
    )
}

fn recognizers(run: &Run) -> Vec<String> {
    json(run)["hints"]
        .as_array()
        .expect("hints array")
        .iter()
        .map(|h| h["recognizer"].as_str().expect("recognizer").to_owned())
        .collect()
}

fn bucket<'a>(doc: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    doc["offset"]["buckets"]
        .as_array()
        .expect("buckets")
        .iter()
        .find(|b| b["name"] == name)
        .unwrap_or_else(|| panic!("bucket {name} missing"))
}

// ---- golden: every section, both formats -----------------------------------

const STRICT_TEXT: &str = "\
triage of surface `strict`: 12 finding(s) in 4 file(s)

Summary
  by level: fail 11, warn 1
  by layer: contract 3, debug 1, mechanics 1, proof 7
  held back: vendor 3, inline ignores 1, baseline 2

Distribution
       5  type.argument-mismatch  (2 file(s), proof/fail)
       3  offset.maybe-missing  (2 file(s), contract/fail)
       1  debug.var-dump  (1 file(s), debug/warn)
       1  offset.missing  (1 file(s), proof/fail)
       1  offset.on-unsupported  (1 file(s), proof/fail)
       1  suppress.unmatched  (1 file(s), mechanics/fail)

Hotspots (top 4 of 4 file(s))
       6  src/Http/A.php
          4 type.argument-mismatch, 2 offset.maybe-missing
       3  src/Cli/C.php
          1 offset.missing, 1 offset.on-unsupported, 1 type.argument-mismatch
       2  src/D.php
          1 debug.var-dump, 1 suppress.unmatched
       1  src/Http/B.php
          1 offset.maybe-missing

Offset guard status (5 offset.* finding(s))
  unguarded                    3  [offset.maybe-missing]
    optional-key reads no guard discharges on their path; each would be a finding on the `strict` surface
  guarded-and-discharged    not measured
    a discharged read leaves no finding, and the check stream carries findings only; measuring this bucket needs a check-side count of discharges
  provably-missing             1  [offset.missing]
    reads of a key provably absent from a proven container; on the default surface, so these are runtime warnings today

Hints (advice, not findings; the exit code is 0 either way)
  - [baseline-hides-debt] 2 finding(s) sit in the baseline and are not counted above; rerun with --ignore-baseline to measure the whole debt before judging a stage change
  - [localised-id] `type.argument-mismatch`: 4 of 5 findings are in src/Http/A.php — localised, not systemic; fixing that file clears most of the id before it is enabled or baselined
  - [optional-reads-concentrated] 3 of 3 `offset.maybe-missing` reads sit under `src/Http` — a shape-to-DTO transform (one typed object in place of the optional-key array) discharges them together; enabling `strict` before that lands would baseline them as debt
";

const STRICT_JSON: &str = r#"{
  "source": {
    "kind": "input",
    "profile": "strict",
    "input": "strict-stream.json",
    "paths": []
  },
  "summary": {
    "findings": 12,
    "files": 4,
    "by_level": {
      "fail": 11,
      "warn": 1
    },
    "by_layer": {
      "contract": 3,
      "debug": 1,
      "mechanics": 1,
      "proof": 7
    },
    "held_back": {
      "vendor_suppressed": 3,
      "suppressed": 1,
      "baselined": 2
    }
  },
  "distribution": [
    {
      "id": "type.argument-mismatch",
      "layer": "proof",
      "level": "fail",
      "count": 5,
      "files": 2
    },
    {
      "id": "offset.maybe-missing",
      "layer": "contract",
      "level": "fail",
      "count": 3,
      "files": 2
    },
    {
      "id": "debug.var-dump",
      "layer": "debug",
      "level": "warn",
      "count": 1,
      "files": 1
    },
    {
      "id": "offset.missing",
      "layer": "proof",
      "level": "fail",
      "count": 1,
      "files": 1
    },
    {
      "id": "offset.on-unsupported",
      "layer": "proof",
      "level": "fail",
      "count": 1,
      "files": 1
    },
    {
      "id": "suppress.unmatched",
      "layer": "mechanics",
      "level": "fail",
      "count": 1,
      "files": 1
    }
  ],
  "hotspots": {
    "files_with_findings": 4,
    "limit": 10,
    "entries": [
      {
        "path": "src/Http/A.php",
        "count": 6,
        "by_id": [
          {
            "id": "type.argument-mismatch",
            "count": 4
          },
          {
            "id": "offset.maybe-missing",
            "count": 2
          }
        ]
      },
      {
        "path": "src/Cli/C.php",
        "count": 3,
        "by_id": [
          {
            "id": "offset.missing",
            "count": 1
          },
          {
            "id": "offset.on-unsupported",
            "count": 1
          },
          {
            "id": "type.argument-mismatch",
            "count": 1
          }
        ]
      },
      {
        "path": "src/D.php",
        "count": 2,
        "by_id": [
          {
            "id": "debug.var-dump",
            "count": 1
          },
          {
            "id": "suppress.unmatched",
            "count": 1
          }
        ]
      },
      {
        "path": "src/Http/B.php",
        "count": 1,
        "by_id": [
          {
            "id": "offset.maybe-missing",
            "count": 1
          }
        ]
      }
    ]
  },
  "offset": {
    "family_total": 5,
    "buckets": [
      {
        "name": "unguarded",
        "status": "measured",
        "count": 3,
        "ids": [
          "offset.maybe-missing"
        ],
        "note": "optional-key reads no guard discharges on their path; each would be a finding on the `strict` surface"
      },
      {
        "name": "guarded-and-discharged",
        "status": "not-measured",
        "count": null,
        "ids": [],
        "note": "a discharged read leaves no finding, and the check stream carries findings only; measuring this bucket needs a check-side count of discharges"
      },
      {
        "name": "provably-missing",
        "status": "measured",
        "count": 1,
        "ids": [
          "offset.missing"
        ],
        "note": "reads of a key provably absent from a proven container; on the default surface, so these are runtime warnings today"
      }
    ]
  },
  "hints": [
    {
      "recognizer": "baseline-hides-debt",
      "advice": "2 finding(s) sit in the baseline and are not counted above; rerun with --ignore-baseline to measure the whole debt before judging a stage change"
    },
    {
      "recognizer": "localised-id",
      "advice": "`type.argument-mismatch`: 4 of 5 findings are in src/Http/A.php — localised, not systemic; fixing that file clears most of the id before it is enabled or baselined"
    },
    {
      "recognizer": "optional-reads-concentrated",
      "advice": "3 of 3 `offset.maybe-missing` reads sit under `src/Http` — a shape-to-DTO transform (one typed object in place of the optional-key array) discharges them together; enabling `strict` before that lands would baseline them as debt"
    }
  ]
}
"#;

const DEFAULT_TEXT: &str = "\
triage of surface `default`: 1 finding(s) in 1 file(s)

Summary
  by level: fail 1
  by layer: proof 1
  held back: vendor 0, inline ignores 0, baseline 0

Distribution
       1  offset.missing  (1 file(s), proof/fail)

Hotspots (top 1 of 1 file(s))
       1  src/Cli/C.php
          1 offset.missing

Offset guard status (1 offset.* finding(s))
  unguarded                 not measured
    the stream names surface `default` but not the id set it resolved to, and `offset.maybe-missing` did not fire: 0 if `default` extends `strict`, otherwise unmeasured; `steins triage --profile strict <paths>` measures the what-if either way
  guarded-and-discharged    not measured
    a discharged read leaves no finding, and the check stream carries findings only; measuring this bucket needs a check-side count of discharges
  provably-missing             1  [offset.missing]
    reads of a key provably absent from a proven container; on the default surface, so these are runtime warnings today

Hints (advice, not findings; the exit code is 0 either way)
  - [strict-what-if-unmeasured] this report measures surface `default`, whose id set the stream does not carry; the strict what-if is one run away: `steins triage --profile strict <paths>` counts the optional-key reads `strict` would report, with check's behavior and exit code unchanged
";

#[test]
fn golden_text_over_the_strict_stream() {
    let run = run_in(&fixtures(), &["triage", "--input", "strict-stream.json"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, STRICT_TEXT);
}

#[test]
fn golden_json_over_the_strict_stream_is_the_data_model_verbatim() {
    let run = run_in(&fixtures(), &["triage", "--format", "json", "--input", "strict-stream.json"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, STRICT_JSON);
    // Every section is a top-level key.
    let doc = json(&run);
    for key in ["source", "summary", "distribution", "hotspots", "offset", "hints"] {
        assert!(doc.get(key).is_some(), "section {key} missing");
    }
}

#[test]
fn golden_text_over_the_default_stream() {
    let run = run_in(&fixtures(), &["triage", "--input", "default-stream.json"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, DEFAULT_TEXT);
}

#[test]
fn stdin_is_the_same_stream_as_the_file() {
    let text = std::fs::read_to_string(fixtures().join("default-stream.json")).expect("fixture");
    let run = run_with_stdin(&fixtures(), &["triage", "--input", "-"], &text);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, DEFAULT_TEXT);
    let run = run_with_stdin(&fixtures(), &["triage", "--format", "json", "--input", "-"], &text);
    assert_eq!(json(&run)["source"]["input"], "-");
}

#[test]
fn top_caps_the_hotspots_and_the_json_says_so() {
    let run = run_in(
        &fixtures(),
        &["triage", "--format", "json", "--top", "1", "--input", "strict-stream.json"],
    );
    let doc = json(&run);
    assert_eq!(doc["hotspots"]["limit"], 1);
    assert_eq!(doc["hotspots"]["files_with_findings"], 4);
    assert_eq!(doc["hotspots"]["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(doc["hotspots"]["entries"][0]["path"], "src/Http/A.php");
}

#[test]
fn top_zero_hides_every_hotspot_but_does_not_call_them_absent() {
    let run = run_in(&fixtures(), &["triage", "--top", "0", "--input", "strict-stream.json"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("Hotspots (top 0 of 4 file(s))\n  (none shown: --top 0)\n"), "{}", run.stdout);
    assert!(!run.stdout.contains("(no findings)"), "{}", run.stdout);
}

// ---- the run-over-paths arm and the what-if --------------------------------

const SHAPE_PHP: &str = "<?php

declare(strict_types=1);

/** @param array{host: string, port?: int} $dsn */
function port(array $dsn): int
{
    return $dsn['port'];
}

/** @param array{host: string, port?: int} $dsn */
function guarded(array $dsn): int
{
    return isset($dsn['port']) ? $dsn['port'] : 0;
}
";

fn shape_project(tag: &str) -> PathBuf {
    let dir = workdir(tag);
    std::fs::create_dir_all(dir.join("src")).expect("src");
    std::fs::write(dir.join("src/Shape.php"), SHAPE_PHP).expect("write fixture");
    dir
}

/// The strict what-if: `check` on the project's own surface stays clean and
/// exits `0`; `triage --profile strict` counts what `strict` would report and
/// exits `0` too — even though the `check` it ran exited `1`.
#[test]
fn a_not_yet_enabled_surface_is_measured_without_changing_check() {
    let dir = shape_project("whatif");
    let check = run_in(&dir, &["check", "--no-php", "--no-cache", "src"]);
    assert_eq!(check.code, 0, "default check is clean: {}", check.stdout);
    assert_eq!(check.stdout, "");

    let strict = run_in(&dir, &["check", "--no-php", "--no-cache", "--profile", "strict", "src"]);
    assert_eq!(strict.code, 1, "strict check reports the unguarded read");
    assert!(strict.stdout.contains("offset.maybe-missing"), "{}", strict.stdout);

    let triage = run_in(
        &dir,
        &["triage", "--format", "json", "--no-php", "--no-cache", "--profile", "strict", "src"],
    );
    assert_eq!(triage.code, 0, "triage is a measurement, not a gate: {}", triage.stderr);
    let doc = json(&triage);
    assert_eq!(doc["source"]["kind"], "check-run");
    assert_eq!(doc["source"]["profile"], "strict");
    assert_eq!(doc["source"]["paths"], serde_json::json!(["src"]));
    assert_eq!(doc["summary"]["findings"], 1);
    let unguarded = bucket(&doc, "unguarded");
    assert_eq!(unguarded["status"], "measured");
    assert_eq!(unguarded["count"], 1);

    // The default run measures nothing strict and says which run would.
    let default = run_in(&dir, &["triage", "--format", "json", "--no-php", "--no-cache", "src"]);
    assert_eq!(default.code, 0);
    let doc = json(&default);
    assert_eq!(doc["summary"]["findings"], 0);
    assert_eq!(bucket(&doc, "unguarded")["status"], "not-measured");
    assert!(recognizers(&default).contains(&"strict-what-if-unmeasured".to_owned()));
}

/// Running over paths aggregates exactly the stream `check --format json`
/// prints — the same findings, once.
#[test]
fn a_check_run_and_its_piped_stream_agree() {
    let dir = shape_project("agree");
    let check = run_in(
        &dir,
        &["check", "--no-php", "--no-cache", "--profile", "strict", "--format", "json", "src"],
    );
    let piped = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &check.stdout);
    let ran = run_in(
        &dir,
        &["triage", "--format", "json", "--no-php", "--no-cache", "--profile", "strict", "src"],
    );
    let (piped, ran) = (json(&piped), json(&ran));
    for key in ["summary", "distribution", "hotspots", "offset", "hints"] {
        assert_eq!(piped[key], ran[key], "section {key} differs between the two arms");
    }
}

/// `check`'s stderr passes through (the sound-subset notice), and `check`'s
/// usage/config exit `2` is forwarded rather than read as an empty stream.
#[test]
fn check_errors_are_forwarded() {
    let dir = shape_project("forward");
    let run = run_in(&dir, &["triage", "--no-php", "--no-cache", "src"]);
    assert_eq!(run.code, 0);
    assert!(run.stderr.contains("sound subset"), "{}", run.stderr);

    let run = run_in(&dir, &["triage", "--no-php", "--no-cache", "--profile", "nope", "src"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("unknown profile `nope`"), "{}", run.stderr);
    // Forwarded, not swallowed: a swallowed exit 2 leaves an empty stdout that
    // the parser would then reject with its own exit 2 and a second message.
    assert!(!run.stderr.contains("not a `check --format json` document"), "{}", run.stderr);
    assert_eq!(run.stdout, "");

    let run = run_in(&dir, &["triage", "--no-php", "--no-cache", "nothere"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("path does not exist: nothere"), "{}", run.stderr);
    assert!(!run.stderr.contains("not a `check --format json` document"), "{}", run.stderr);
}

// ---- usage errors ----------------------------------------------------------

#[test]
fn usage_errors_exit_two() {
    let dir = workdir("usage");
    let cases: &[(&[&str], &str)] = &[
        (&["triage"], "no --input and no paths given"),
        (&["triage", "--input", "x.json", "src"], "--input and <paths> cannot combine"),
        (&["triage", "--fix", "src"], "unknown flag `--fix` for triage"),
        (&["triage", "--format", "sarif", "--input", "x.json"], "unknown format `sarif`"),
        (&["triage", "--top", "many", "--input", "x.json"], "--top requires a non-negative integer"),
        (&["triage", "--input", "missing.json"], "cannot read --input missing.json"),
        (&["triage", "--profile", "strict", "--input", "x.json"], "belongs to a check run"),
    ];
    for (args, needle) in cases {
        let run = run_in(&dir, args);
        assert_eq!(run.code, 2, "{args:?}: {}", run.stderr);
        assert!(run.stderr.contains(needle), "{args:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{args:?} wrote a document on a usage error");
    }
    let run = run_with_stdin(&dir, &["triage", "--input", "-"], "not json");
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("not a `check --format json` document"), "{}", run.stderr);
}

#[test]
fn the_no_argument_usage_and_the_unknown_command_list_name_triage() {
    let dir = workdir("dispatch");
    let run = run_in(&dir, &[]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("steins triage [--format text|json] [--input <file>|-]"), "{}", run.stderr);
    let run = run_in(&dir, &["nope"]);
    assert!(run.stderr.contains("mcp, triage, version"), "{}", run.stderr);
}

/// A stream without the additive `layer`/`level`/`profile` keys aggregates
/// under `unknown` — the command depends on the schema, not on one version of it.
#[test]
fn a_trimmed_stream_still_aggregates() {
    let dir = workdir("trimmed");
    let run = run_with_stdin(
        &dir,
        &["triage", "--format", "json", "--input", "-"],
        r#"{"findings":[{"id":"x.y","path":"a.php"}]}"#,
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    let doc = json(&run);
    assert_eq!(doc["source"]["profile"], "unknown");
    assert_eq!(doc["summary"]["by_layer"]["unknown"], 1);
    assert_eq!(doc["distribution"][0]["level"], "unknown");
}

// ---- deletion tests: one per recognizer -------------------------------------

/// The catalogue is exactly these four rows, and the JSON names them by
/// these spellings. Renaming or dropping a row fails here first.
const CATALOGUE: [&str; 4] = [
    "strict-what-if-unmeasured",
    "baseline-hides-debt",
    "localised-id",
    "optional-reads-concentrated",
];

#[test]
fn recognizer_strict_what_if_unmeasured_fires_below_strict_only() {
    let dir = workdir("rec-whatif");
    let below = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &stream("default", &[], 0));
    assert_eq!(recognizers(&below), vec![CATALOGUE[0]]);
    let strict = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &stream("strict", &[], 0));
    assert!(recognizers(&strict).is_empty(), "{:?}", recognizers(&strict));
}

#[test]
fn recognizer_baseline_hides_debt_fires_on_a_nonzero_baselined_count() {
    let dir = workdir("rec-baseline");
    let held = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &stream("strict", &[], 7));
    assert_eq!(recognizers(&held), vec![CATALOGUE[1]]);
    let advice = json(&held)["hints"][0]["advice"].as_str().expect("advice").to_owned();
    assert!(advice.starts_with("7 finding(s) sit in the baseline"), "{advice}");
    let none = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &stream("strict", &[], 0));
    assert!(recognizers(&none).is_empty());
}

#[test]
fn recognizer_localised_id_fires_at_eighty_percent_in_one_file() {
    let dir = workdir("rec-local");
    let a = "src/A.php";
    let fires = stream("strict", &[("x.y", a), ("x.y", a), ("x.y", a), ("x.y", a), ("x.y", "src/B.php")], 0);
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &fires);
    assert_eq!(recognizers(&run), vec![CATALOGUE[2]]);
    // 3 of 5 (60%) in one file: spread, not localised.
    let spread = stream(
        "strict",
        &[("x.y", a), ("x.y", a), ("x.y", a), ("x.y", "src/B.php"), ("x.y", "src/C.php")],
        0,
    );
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &spread);
    assert!(recognizers(&run).is_empty(), "{:?}", recognizers(&run));
}

#[test]
fn recognizer_optional_reads_concentrated_fires_at_sixty_percent_under_one_directory() {
    let dir = workdir("rec-optional");
    let id = "offset.maybe-missing";
    let fires = stream(
        "strict",
        &[(id, "src/Http/A.php"), (id, "src/Http/B.php"), (id, "src/Cli/C.php")],
        0,
    );
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &fires);
    assert_eq!(recognizers(&run), vec![CATALOGUE[3]]);
    assert!(json(&run)["hints"][0]["advice"].as_str().expect("advice").contains("under `src/Http`"));
    // Two reads: under the minimum, whatever the concentration.
    let few = stream("strict", &[(id, "src/Http/A.php"), (id, "src/Http/B.php")], 0);
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &few);
    assert!(recognizers(&run).is_empty());
}

/// Hints never leave the `hints` section: no finding, no exit contribution.
#[test]
fn hints_are_advice_and_never_findings() {
    let run = run_in(&fixtures(), &["triage", "--format", "json", "--input", "strict-stream.json"]);
    assert_eq!(run.code, 0);
    let doc = json(&run);
    let names: Vec<&str> =
        doc["hints"].as_array().expect("hints").iter().map(|h| h["recognizer"].as_str().expect("name")).collect();
    for n in &names {
        assert!(CATALOGUE.contains(n), "unknown recognizer {n}");
    }
    for h in doc["hints"].as_array().expect("hints") {
        assert!(h.get("id").is_none() && h.get("level").is_none() && h.get("line").is_none(), "{h}");
    }
}

// ---- deletion tests: one per bucket -----------------------------------------

#[test]
fn bucket_unguarded_counts_offset_maybe_missing_on_strict() {
    let dir = workdir("bucket-unguarded");
    let s = stream("strict", &[("offset.maybe-missing", "a.php"), ("offset.maybe-missing", "b.php")], 0);
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &s);
    let doc = json(&run);
    let b = bucket(&doc, "unguarded");
    assert_eq!(b["status"], "measured");
    assert_eq!(b["count"], 2);
    assert_eq!(b["ids"], serde_json::json!(["offset.maybe-missing"]));
    // Below strict, not measured — and the count is null, not zero.
    let s = stream("contracts", &[], 0);
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &s);
    let doc = json(&run);
    assert_eq!(bucket(&doc, "unguarded")["status"], "not-measured");
    assert!(bucket(&doc, "unguarded")["count"].is_null());
}

#[test]
fn bucket_guarded_and_discharged_is_reported_as_not_measured() {
    let dir = workdir("bucket-discharged");
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &stream("strict", &[], 0));
    let doc = json(&run);
    let b = bucket(&doc, "guarded-and-discharged");
    assert_eq!(b["status"], "not-measured");
    assert!(b["count"].is_null());
    assert!(b["note"].as_str().expect("note").contains("findings only"));
    // And the text rendering says so in words, never with a zero.
    let text = run_with_stdin(&dir, &["triage", "--input", "-"], &stream("strict", &[], 0));
    assert!(text.stdout.contains("guarded-and-discharged    not measured"), "{}", text.stdout);
}

#[test]
fn bucket_provably_missing_counts_offset_missing_on_every_surface() {
    let dir = workdir("bucket-missing");
    let s = stream("default", &[("offset.missing", "a.php"), ("offset.on-unsupported", "a.php")], 0);
    let run = run_with_stdin(&dir, &["triage", "--format", "json", "--input", "-"], &s);
    let doc = json(&run);
    let b = bucket(&doc, "provably-missing");
    assert_eq!(b["status"], "measured");
    assert_eq!(b["count"], 1);
    assert_eq!(b["ids"], serde_json::json!(["offset.missing"]));
    // The family total keeps the id no bucket names.
    assert_eq!(doc["offset"]["family_total"], 2);
    // Exactly the three buckets, in report order.
    let names: Vec<&str> = doc["offset"]["buckets"]
        .as_array()
        .expect("buckets")
        .iter()
        .map(|b| b["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["unguarded", "guarded-and-discharged", "provably-missing"]);
}
