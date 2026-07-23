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
