//! End-to-end tests for `steins annotate` under `steins.toml [runtime]` (issue
//! #787): the margin resolves the postures the way `check` does, so a declared
//! posture moves its findings and its value facts as it moves `check`'s.
//!
//! Each test runs the real `steins` binary in a private temp dir (its own
//! CWD), mirroring `tests/it/profile.rs`: `steins.toml` reads from the process's
//! working directory, not the analyzed path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test scrubs `GITHUB_ACTIONS`: `check`'s format auto-detection
/// (ADR-0054 §6) reads it, so CI would otherwise get workflow commands where
/// text was asserted (detection itself is tested in `tests/it/format_github.rs`).
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("steins-annotate-runtime-{}-{tag}-{n}", std::process::id()));
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

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// A proven-missing offset read: warning-grade (`Undefined array key`), so it
/// is a finding under the default `"abort"` handler and none under `"null"`.
/// The offset family needs the sidecar (ADR-0049 A9), so these runs keep PHP.
const MISSING_OFFSET: &str = "<?php\n\
function f(): void {\n\
    $a = ['x' => 1];\n\
    $v = $a['y'];\n\
}\n";

/// The host-dependent constants an `os` pin fixes (ADR-0094 §3).
const HOST_CONSTANTS: &str = "<?php\n\
function g(): void {\n\
    $eol = PHP_EOL;\n\
    $sep = DIRECTORY_SEPARATOR;\n\
}\n";

#[test]
fn a_null_warning_handler_silences_the_margin_as_it_silences_check() {
    // Control: under the default handler both surfaces report the read.
    let dir = workdir("abort");
    write(&dir, "a.php", MISSING_OFFSET);
    let check = run_in(&dir, &["check", "--no-cache", "a.php"]);
    assert!(check.stdout.contains("offset.missing"), "check reports it, got:\n{}", check.stdout);
    let annotate = run_in(&dir, &["annotate", "a.php"]);
    assert!(
        annotate.stdout.contains("//=> ✗ offset.missing"),
        "annotate reports it, got:\n{}",
        annotate.stdout
    );

    let dir = workdir("null");
    write(&dir, "a.php", MISSING_OFFSET);
    write(&dir, "steins.toml", "[runtime]\nwarning-handler = \"null\"\n");
    let check = run_in(&dir, &["check", "--no-cache", "a.php"]);
    assert_eq!(check.code, 0, "check is silent under \"null\"; stdout:\n{}", check.stdout);
    let annotate = run_in(&dir, &["annotate", "a.php"]);
    assert_eq!(annotate.code, 0, "stderr:\n{}", annotate.stderr);
    assert!(
        !annotate.stdout.contains("offset.missing"),
        "annotate drops what check drops, got:\n{}",
        annotate.stdout
    );
}

#[test]
fn an_os_pin_narrows_the_margins_host_constants() {
    // Absent a pin each constant is the union of its hosts' values, which the
    // margin, printing literals only, leaves unannotated.
    let dir = workdir("no-pin");
    write(&dir, "a.php", HOST_CONSTANTS);
    let r = run_in(&dir, &["annotate", "--no-php", "a.php"]);
    assert!(!r.stdout.contains("//=> $eol"), "no pin, no literal, got:\n{}", r.stdout);

    let dir = workdir("linux");
    write(&dir, "a.php", HOST_CONSTANTS);
    write(&dir, "steins.toml", "[runtime]\nos = \"linux\"\n");
    let r = run_in(&dir, &["annotate", "--no-php", "a.php"]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(r.stdout.contains(r#"//=> $eol = "\n""#), "PHP_EOL pinned, got:\n{}", r.stdout);
    assert!(r.stdout.contains(r#"//=> $sep = "/""#), "separator pinned, got:\n{}", r.stdout);
}

#[test]
fn a_misspelled_runtime_key_is_a_hard_config_error_for_annotate_too() {
    // Read leniently, the whole file would drop to the defaults in silence,
    // and the margin would disagree with a `check` that refuses to run.
    let dir = workdir("typo-key");
    write(&dir, "a.php", MISSING_OFFSET);
    write(&dir, "steins.toml", "[runtime]\nwarning-hadler = \"null\"\n");
    let r = run_in(&dir, &["annotate", "--no-php", "a.php"]);
    assert_eq!(r.code, 2, "unknown [runtime] key → exit 2; stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("parse error"), "names the parse failure, got:\n{}", r.stderr);
}

#[test]
fn an_unknown_runtime_value_warns_on_stderr_as_check_does() {
    let dir = workdir("typo-value");
    write(&dir, "a.php", HOST_CONSTANTS);
    write(&dir, "steins.toml", "[runtime]\nos = \"linus\"\n");
    let r = run_in(&dir, &["annotate", "--no-php", "a.php"]);
    assert_eq!(r.code, 0, "warn-and-proceed, not exit 2; stderr:\n{}", r.stderr);
    assert!(
        r.stderr.contains("steins: steins.toml [runtime] os: unknown value `linus`"),
        "names the value, got:\n{}",
        r.stderr
    );
    assert!(!r.stdout.contains("//=> $eol"), "a typo is never a pin, got:\n{}", r.stdout);
}
