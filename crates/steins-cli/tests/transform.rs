//! End-to-end CLI tests for `steins transform phpdoc-to-native` (ADR-0020/0034).
//! Dry-run prints a diff + refusal report and never writes; `--apply` writes only
//! after the dual-verification post-check passes.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A throwaway project directory under the OS temp dir, cleaned on drop.
struct TempProject {
    dir: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-transform-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap()
    }
    fn path(&self) -> &str {
        self.dir.to_str().unwrap()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn dry_run_prints_diff_and_does_not_write() {
    let proj = TempProject::new("dryrun");
    let lib_before = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    proj.write("lib.php", lib_before);
    proj.write("main.php", "<?php\nf(1);\nf(2);\n");

    let r = run(&["transform", "phpdoc-to-native", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("+function f(int $x)"), "diff missing native hint:\n{}", r.stdout);
    assert!(r.stdout.contains("1 enumerated: 1 promoted"), "oracle line:\n{}", r.stdout);
    assert!(r.stdout.contains("Post-check OK"), "postcheck:\n{}", r.stdout);
    // The file on disk is unchanged by a dry run.
    assert_eq!(proj.read("lib.php"), lib_before);
}

#[test]
fn apply_writes_the_promotion() {
    let proj = TempProject::new("apply");
    proj.write("lib.php", "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n");
    proj.write("main.php", "<?php\nf(1);\n");

    let r = run(&["transform", "phpdoc-to-native", "--apply", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let after = proj.read("lib.php");
    assert!(after.contains("function f(int $x)"), "not promoted on disk:\n{after}");
    assert!(!after.contains("@param"), "tag not removed:\n{after}");
}

#[test]
fn refusal_is_reported_and_nothing_written() {
    let proj = TempProject::new("refuse");
    let before = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    proj.write("lib.php", before);
    // A wrong literal at a call site → argument-not-proven refusal.
    proj.write("main.php", "<?php\nf(\"nope\");\n");

    let r = run(&["transform", "phpdoc-to-native", "--apply", proj.path()]);
    assert_eq!(r.code, 0, "a refusal is not a failure; stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("argument-not-proven"), "refusal reason:\n{}", r.stdout);
    assert!(r.stdout.contains("0 promoted, 1 refused"), "oracle:\n{}", r.stdout);
    // Refused → the file is untouched.
    assert_eq!(proj.read("lib.php"), before);
}

#[test]
fn json_format_emits_report_and_postcheck() {
    let proj = TempProject::new("json");
    proj.write("lib.php", "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n");
    proj.write("main.php", "<?php\nf(1);\n");

    let r = run(&["transform", "phpdoc-to-native", "--format", "json", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert_eq!(v["report"]["oracle"]["enumerated"], 1);
    assert_eq!(v["report"]["oracle"]["transformed"], 1);
    assert_eq!(v["postcheck"]["ok"], true);
    assert_eq!(v["applied"], false);
    // The EditPlan is present and serialized.
    assert!(v["report"]["plan"]["edits"].as_array().unwrap().len() >= 2);
}

#[test]
fn missing_transform_name_is_usage_error() {
    let r = run(&["transform"]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("transform requires a name"), "stderr:\n{}", r.stderr);
}

#[test]
fn unknown_transform_name_is_usage_error() {
    let r = run(&["transform", "bogus-transform", "."]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("unknown transform"), "stderr:\n{}", r.stderr);
}

// ---- phpdoc-honesty (Transform #2) ----------------------------------------

#[test]
fn honesty_dry_run_prints_diff_and_does_not_write() {
    let proj = TempProject::new("honesty-dryrun");
    // `@param int $id` but callers pass an int and numeric strings — a lie.
    let lib_before = "<?php\n/** @param int $id */\nfunction f($id) { return $id; }\n";
    proj.write("lib.php", lib_before);
    proj.write("main.php", "<?php\nf(1);\nf(\"12\");\nf(\"34\");\n");

    let r = run(&["transform", "phpdoc-honesty", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(
        r.stdout.contains("+/** @param int|numeric-string $id */"),
        "diff missing widened tag:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("1 enumerated: 1 rewritten"), "oracle line:\n{}", r.stdout);
    assert!(r.stdout.contains("Post-check OK"), "postcheck:\n{}", r.stdout);
    // The dry run does not touch disk.
    assert_eq!(proj.read("lib.php"), lib_before);
}

#[test]
fn honesty_apply_writes_the_widened_tag() {
    let proj = TempProject::new("honesty-apply");
    proj.write("lib.php", "<?php\n/** @param int $id */\nfunction f($id) { return $id; }\n");
    proj.write("main.php", "<?php\nf(1);\nf(\"12\");\nf(\"34\");\n");

    let r = run(&["transform", "phpdoc-honesty", "--apply", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let after = proj.read("lib.php");
    assert!(after.contains("@param int|numeric-string $id"), "not widened on disk:\n{after}");
}

#[test]
fn honesty_refusal_is_reported_and_nothing_written() {
    let proj = TempProject::new("honesty-refuse");
    // A lying tag (array arg violates int) with no faithful scalar spelling.
    let before = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    proj.write("lib.php", before);
    proj.write("main.php", "<?php\nf([1, 2]);\n");

    let r = run(&["transform", "phpdoc-honesty", "--apply", proj.path()]);
    assert_eq!(r.code, 0, "a refusal is not a failure; stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("type-not-renderable"), "refusal reason:\n{}", r.stdout);
    assert!(r.stdout.contains("0 rewritten, 1 refused"), "oracle:\n{}", r.stdout);
    assert_eq!(proj.read("lib.php"), before);
}

#[test]
fn honesty_json_format_emits_report_and_postcheck() {
    let proj = TempProject::new("honesty-json");
    proj.write("lib.php", "<?php\n/** @param int $id */\nfunction f($id) { return $id; }\n");
    proj.write("main.php", "<?php\nf(1);\nf(\"12\");\nf(\"34\");\n");

    let r = run(&["transform", "phpdoc-honesty", "--format", "json", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert_eq!(v["report"]["oracle"]["enumerated"], 1);
    assert_eq!(v["report"]["oracle"]["transformed"], 1);
    assert_eq!(v["postcheck"]["ok"], true);
    assert_eq!(v["applied"], false);
    assert_eq!(v["report"]["plan"]["edits"].as_array().unwrap().len(), 1);
}

// ---- ADR-0046 §2: dynamic-code obstacles + the vouching valve --------------

/// An `eval` in a project file raises the `eval-present` obstacle (recorded once)
/// and blocks every promotion — the canonical `'foo(42)'` invisible-caller gap.
#[test]
fn eval_obstacle_fires_and_blocks_promotion() {
    let proj = TempProject::new("eval-obstacle");
    proj.write("lib.php", "<?php\n/** @param int $x */\nfunction foo($x) { return $x; }\n");
    proj.write("main.php", "<?php\nfoo(1);\n");
    proj.write("evil.php", "<?php\neval('foo(42)');\n");

    let r = run(&["transform", "phpdoc-to-native", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("Dynamic-code obstacles"), "obstacle block missing:\n{}", r.stdout);
    assert!(r.stdout.contains("eval-present"), "reason missing:\n{}", r.stdout);
    assert!(r.stdout.contains("0 promoted"), "promotion must be blocked:\n{}", r.stdout);
    // The source is untouched (dry run, and nothing promotable anyway).
    assert!(proj.read("lib.php").contains("@param"));
}

/// A dynamic `require $x` raises `dynamic-include-present`.
#[test]
fn dynamic_include_obstacle_fires() {
    let proj = TempProject::new("dyn-include");
    proj.write("lib.php", "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n");
    proj.write("main.php", "<?php\n$p = $_GET['p'];\nrequire $p;\nf(1);\n");

    let r = run(&["transform", "phpdoc-to-native", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("dynamic-include-present"), "reason missing:\n{}", r.stdout);
    assert!(r.stdout.contains("0 promoted"), "{}", r.stdout);
}

/// The vouching valve: a vouched eval site does not raise its obstacle, so the
/// promotion proceeds — but the report carries the prominent downgrade note.
#[test]
fn vouched_eval_proceeds_with_downgrade_note() {
    let proj = TempProject::new("vouch-text");
    proj.write("lib.php", "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n");
    proj.write("main.php", "<?php\nf(1);\n");
    proj.write("evil.php", "<?php\neval('legacy_boot();');\n"); // eval on line 2
    let cfg = proj.write("steins.toml", "[transform.vouch]\nsites = [\"evil.php:2\"]\n");

    let r = run(&[
        "transform",
        "phpdoc-to-native",
        "--config",
        cfg.to_str().unwrap(),
        proj.path(),
    ]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("1 promoted"), "vouched run must promote:\n{}", r.stdout);
    assert!(r.stdout.contains("DOWNGRADE"), "downgrade note missing:\n{}", r.stdout);
    assert!(r.stdout.contains("1 user-vouched dynamic-code exemption"), "{}", r.stdout);
}

/// The obstacle and downgrade note both appear in JSON (ADR-0046 §2: the claim
/// downgrade must be machine-visible).
#[test]
fn json_carries_obstacles_and_downgrade_note() {
    let proj = TempProject::new("vouch-json");
    proj.write("lib.php", "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n");
    proj.write("main.php", "<?php\nf(1);\n");
    proj.write("evil.php", "<?php\neval('legacy_boot();');\n");

    // Without a vouch: the obstacle is present in JSON and blocks.
    let r = run(&["transform", "phpdoc-to-native", "--format", "json", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert_eq!(v["report"]["obstacles"][0]["reason"], "eval-present");
    assert_eq!(v["report"]["oracle"]["transformed"], 0);
    assert!(v["downgrade_note"].is_null());

    // With a vouch: obstacle clears, downgrade note appears, promotion proceeds.
    let cfg = proj.write("steins.toml", "[transform.vouch]\nsites = [\"evil.php:2\"]\n");
    let r2 = run(&[
        "transform",
        "phpdoc-to-native",
        "--format",
        "json",
        "--config",
        cfg.to_str().unwrap(),
        proj.path(),
    ]);
    assert_eq!(r2.code, 0, "stderr:\n{}", r2.stderr);
    let v2: serde_json::Value = serde_json::from_str(&r2.stdout).expect("valid json");
    assert_eq!(v2["report"]["oracle"]["transformed"], 1);
    assert_eq!(v2["report"]["vouched_exemptions"].as_array().unwrap().len(), 1);
    assert!(
        v2["downgrade_note"].as_str().unwrap().contains("conditional on 1"),
        "downgrade note:\n{}",
        r2.stdout
    );
}

/// A proven literal include resolving inside the analyzed universe is benign — no
/// obstacle, and the promotion proceeds normally.
#[test]
fn in_universe_include_does_not_obstruct() {
    let proj = TempProject::new("in-universe");
    proj.write("lib.php", "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n");
    proj.write("main.php", "<?php\nrequire __DIR__ . '/lib.php';\nf(1);\n");

    let r = run(&["transform", "phpdoc-to-native", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(!r.stdout.contains("Dynamic-code obstacles"), "should be benign:\n{}", r.stdout);
    assert!(r.stdout.contains("1 promoted"), "{}", r.stdout);
}
