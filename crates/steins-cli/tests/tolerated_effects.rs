//! `annotate` under a tolerated-effects policy (ADR-0084 §4): the `~` marker in
//! the text margin and the `tolerated` array in the JSON document.
//!
//! Each test runs the real binary in a private temp dir that is both the project
//! root and the CWD, so the `[effects]` table under test is the only one loaded.
//! `--no-php` keeps the run independent of a `php` on PATH and touches no effect fact.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("steins-tolerated-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write file");
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

/// A logging facade and two callers: `f` reaches the stream and the clock only
/// through the facade, `g` also reads the clock in its own body. One policy, two
/// verdicts about `nondet.time`.
const SRC: &str = "<?php\nclass Logger {\n    public static function debug(string $m): void { fwrite(STDERR, $m . time()); }\n}\nfunction f(string $s): int { Logger::debug($s); return 1; }\nfunction g(string $s): int { Logger::debug($s); return time(); }\n";

const POLICY: &str = "[effects]\ntolerated = [\"telemetry\"]\n\n[effects.attribution]\n\"Logger\" = [\"telemetry\"]\n";

/// The project the two policy tests share: source plus a `steins.toml` attributing the facade.
fn project(tag: &str) -> PathBuf {
    let dir = workdir(tag);
    write(&dir, "app.php", SRC);
    write(&dir, "steins.toml", POLICY);
    dir
}

#[test]
fn the_margin_marks_a_wholly_discharged_label_and_leaves_a_surviving_one_plain() {
    let dir = project("margin");
    let r = run_in(&dir, &["annotate", "--no-php", "app.php"]);
    assert_eq!(r.code, 0, "annotate never fails on a readable file, got:\n{}", r.stderr);
    assert!(
        r.stdout.contains("//=> effects: {~io.output.stderr, ~nondet.time}"),
        "both labels reach `f` only through the facade, got:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("//=> effects: {~io.output.stderr, nondet.time}"),
        "`g` reads the clock itself, so that label is still judged, got:\n{}",
        r.stdout
    );
}

#[test]
fn the_json_document_lists_the_tolerated_subset_beside_the_unchanged_effects() {
    let dir = project("json");
    let r = run_in(&dir, &["annotate", "--no-php", "--format", "json", "app.php"]);
    assert_eq!(r.code, 0, "got:\n{}", r.stderr);
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json object");
    let functions = doc["functions"].as_array().expect("functions array");
    let by_name = |name: &str| -> &serde_json::Value {
        functions.iter().find(|f| f["name"] == name).unwrap_or_else(|| panic!("no `{name}` entry"))
    };

    // Proven lane is unchanged; `tolerated` names a subset, never a removal (ADR-0084 §4).
    let f = by_name("f");
    assert_eq!(f["effects"], serde_json::json!(["io.output.stderr", "nondet.time"]));
    assert_eq!(f["tolerated"], serde_json::json!(["io.output.stderr", "nondet.time"]));
    assert_eq!(f["declared"], serde_json::json!([]));
    assert_eq!(f["exhaustive"], serde_json::json!(true));

    let g = by_name("g");
    assert_eq!(g["effects"], serde_json::json!(["io.output.stderr", "nondet.time"]));
    assert_eq!(g["tolerated"], serde_json::json!(["io.output.stderr"]));

    // The facade's own set is judged where it stands: no attributed edge crossed to get there.
    assert_eq!(by_name("Logger::debug").get("tolerated"), None);
}

#[test]
fn a_project_with_no_policy_emits_the_document_and_the_margin_it_always_did() {
    let dir = workdir("no-policy");
    write(&dir, "app.php", SRC);
    let text = run_in(&dir, &["annotate", "--no-php", "app.php"]);
    assert_eq!(text.code, 0, "got:\n{}", text.stderr);
    assert!(!text.stdout.contains('~'), "no marker without a policy, got:\n{}", text.stdout);
    assert!(
        text.stdout.contains("//=> effects: {io.output.stderr, nondet.time}"),
        "got:\n{}",
        text.stdout
    );

    let json = run_in(&dir, &["annotate", "--no-php", "--format", "json", "app.php"]);
    let doc: serde_json::Value = serde_json::from_str(&json.stdout).expect("valid json object");
    for entry in doc["functions"].as_array().expect("functions array") {
        assert_eq!(entry.get("tolerated"), None, "the field joins only a policy, got:\n{entry}");
    }
}
