//! End-to-end CLI tests for `steins check --fix` (ADR-0010/0020, issue #114): the
//! first fix family — dump-statement removal for `debug.type`/`debug.phpdoc-type`
//! (ADR-0053). Writes are gated by the transform engine's dual-verification
//! post-check (ADR-0034): zero new diagnostics or nothing written, named refusal either way.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Spawns with `GITHUB_ACTIONS` scrubbed so `check`'s CI auto-detection
/// (ADR-0054 §6) doesn't emit workflow commands where a test expects text.
/// Detection itself is tested in `tests/format_github.rs`.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let out = steins_cmd().args(args).output().expect("run steins");
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
            "steins-checkfix-{}-{}-{}",
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
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
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

const DUMP_SRC: &str = "<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n";

#[test]
fn fix_removes_the_dump_statement_and_a_rerun_is_clean() {
    let proj = TempProject::new("applies");
    proj.write("app.php", DUMP_SRC);

    // Without --fix: dump reports, reds the run (ADR-0053 §3), file untouched.
    let plain = run(&["check", proj.path()]);
    assert_eq!(plain.code, 1, "stdout:\n{}", plain.stdout);
    assert!(plain.stdout.contains("error[debug.type]"), "plain run:\n{}", plain.stdout);
    assert!(!plain.stdout.contains("fixed["), "plain run must not report fixes:\n{}", plain.stdout);
    assert_eq!(proj.read("app.php"), DUMP_SRC);

    // --fix removes the statement; the fixed finding doesn't count toward exit (→ 0).
    let fixed = run(&["check", "--fix", proj.path()]);
    assert_eq!(fixed.code, 0, "stdout:\n{}\nstderr:\n{}", fixed.stdout, fixed.stderr);
    assert!(fixed.stdout.contains("fixed[debug.type]"), "fix report:\n{}", fixed.stdout);
    assert!(!fixed.stdout.contains("error["), "no surviving errors:\n{}", fixed.stdout);
    assert!(
        fixed.stderr.contains("steins: fixed 1 finding(s) (1 file(s) written)"),
        "stderr accounting:\n{}",
        fixed.stderr
    );
    assert_eq!(proj.read("app.php"), "<?php\n$x = 5;\n", "whole line removed");

    // A rerun of plain `check` on the result is clean.
    let rerun = run(&["check", proj.path()]);
    assert_eq!(rerun.code, 0, "rerun:\n{}", rerun.stdout);
    assert!(rerun.stdout.is_empty(), "rerun output:\n{}", rerun.stdout);
}

#[test]
fn the_gate_passes_a_removal_beside_an_unrelated_error() {
    // This family goes first because a recognized dump is transparent (ADR-0053
    // point 10 — reads facts, binds nothing), so removing it can't change what the
    // rest of the file proves; the unrelated `$x->m()` error already reports before
    // the edit, so the post-check's per-id count is unchanged and the gate passes.
    // The refusal side is exercised in `post_check_gate_refuses_a_regressing_fix`
    // (`crates/steins-cli/src/main.rs`), via a synthetic regressing payload.
    let proj = TempProject::new("gatepass");
    let src = "<?php\n$x = null;\n\\PHPStan\\dumpType($x);\n$x->m();\n";
    proj.write("app.php", src);

    let r = run(&["check", "--fix", proj.path()]);
    // The pre-existing error survives at fail level → exit 1.
    assert_eq!(r.code, 1, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("error[call.on-null]"), "unrelated error survives:\n{}", r.stdout);
    assert!(r.stdout.contains("fixed[debug.type]"), "the dump was fixed:\n{}", r.stdout);
    assert!(!r.stdout.contains("fix refused"), "the gate must not refuse:\n{}", r.stdout);
    assert!(
        r.stderr.contains("steins: fixed 1 finding(s) (1 file(s) written)"),
        "stderr accounting:\n{}",
        r.stderr
    );
    assert_eq!(proj.read("app.php"), "<?php\n$x = null;\n$x->m();\n", "only the dump line went");
}

#[test]
fn json_findings_carry_the_fix_payload_without_the_flag() {
    let proj = TempProject::new("payload");
    proj.write("app.php", DUMP_SRC);
    // Second file with a fix-less finding, to pin the negative: no `fix` key when none exists.
    proj.write("other.php", "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n");

    let r = run(&["check", "--format", "json", proj.path()]);
    assert_eq!(r.code, 1);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert!(v.get("fix").is_none(), "no top-level fix key without --fix:\n{}", r.stdout);
    let findings = v["findings"].as_array().expect("findings array");
    let dump = findings
        .iter()
        .find(|d| d["id"] == "debug.type")
        .expect("debug.type finding present");
    // Payload mirrors steins-edit's `Edit` shape. The statement occupies bytes
    // 14..36 alone on its line, so deletion swallows the whole line (14..37).
    assert_eq!(dump["fix"]["title"], "remove the dump statement");
    let edits = dump["fix"]["edits"].as_array().expect("edits array");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["span"]["start"], 14);
    assert_eq!(edits[0]["span"]["end"], 37);
    assert_eq!(edits[0]["replacement"], "");
    assert!(edits[0]["path"].as_str().unwrap().ends_with("app.php"));
    let mismatch = findings
        .iter()
        .find(|d| d["id"] == "type.argument-mismatch")
        .expect("type.argument-mismatch present");
    assert!(mismatch.get("fix").is_none(), "no fix key on a fix-less finding");
}

#[test]
fn fix_with_json_reports_the_fixed_array() {
    let proj = TempProject::new("jsonfix");
    proj.write("app.php", DUMP_SRC);

    let r = run(&["check", "--fix", "--format", "json", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert_eq!(v["fix"]["applied"], true);
    assert_eq!(v["fix"]["refusal"], serde_json::Value::Null);
    let fixed = v["fix"]["fixed"].as_array().expect("fixed array");
    assert_eq!(fixed.len(), 1);
    assert_eq!(fixed[0]["id"], "debug.type");
    // The fixed finding left the findings array: it cannot be double-counted.
    assert_eq!(v["findings"].as_array().unwrap().len(), 0);
}

#[test]
fn var_dump_is_not_fixed() {
    // Scope guard (issue #114): `debug.var-dump` has no fix — `var_dump()` is legal PHP.
    let proj = TempProject::new("vardump");
    let src = "<?php\n$x = 1;\nvar_dump($x);\n";
    proj.write("app.php", src);

    let r = run(&["check", "--fix", proj.path()]);
    // Warn-level, exit-neutral (ADR-0053 §3) — and nothing fixable.
    assert_eq!(r.code, 0, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("steins: no fixable findings"), "stderr:\n{}", r.stderr);
    assert_eq!(proj.read("app.php"), src, "file untouched");
}

#[test]
fn embedded_dump_is_not_fixed_and_still_reds_the_run() {
    // `$y = dumpType($x);`: deleting the statement would delete the binding too,
    // so no fix rides along; the finding survives at fail level.
    let proj = TempProject::new("embedded");
    let src = "<?php\n$x = 1;\n$y = \\PHPStan\\dumpType($x);\n";
    proj.write("app.php", src);

    let r = run(&["check", "--fix", proj.path()]);
    assert_eq!(r.code, 1, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("error[debug.type]"), "finding survives:\n{}", r.stdout);
    assert!(r.stderr.contains("steins: no fixable findings"), "stderr:\n{}", r.stderr);
    assert_eq!(proj.read("app.php"), src, "file untouched");
}

#[test]
fn fix_and_set_baseline_is_a_usage_error() {
    let proj = TempProject::new("usage");
    proj.write("app.php", DUMP_SRC);

    let r = run(&["check", "--fix", "--set-baseline", proj.path()]);
    assert_eq!(r.code, 2);
    assert!(
        r.stderr.contains("--fix cannot be combined with --set-baseline"),
        "stderr:\n{}",
        r.stderr
    );
    assert_eq!(proj.read("app.php"), DUMP_SRC, "file untouched");
}

#[test]
fn multi_argument_dump_is_fixed_once() {
    // Two findings (one per arg), ONE deletion: identical edits dedupe into a single splice.
    let proj = TempProject::new("multiarg");
    proj.write("app.php", "<?php\n$x = 1;\n$y = 2;\n\\PHPStan\\dumpType($x, $y);\n");

    let r = run(&["check", "--fix", proj.path()]);
    assert_eq!(r.code, 0, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert_eq!(r.stdout.matches("fixed[debug.type]").count(), 2, "both report:\n{}", r.stdout);
    assert!(
        r.stderr.contains("steins: fixed 2 finding(s) (1 file(s) written)"),
        "stderr:\n{}",
        r.stderr
    );
    assert_eq!(proj.read("app.php"), "<?php\n$x = 1;\n$y = 2;\n");
}
