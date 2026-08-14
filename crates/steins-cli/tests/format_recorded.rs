//! Recorded-output regression for the render seam (ADR-0054 slice C1): a
//! format is a serialization of the displayed surface, so moving where that
//! lives (into [`render`](../src/render.rs)) must not move a byte — recorded
//! here since drift (a stray newline, a lost separator) is easy to miss.
//!
//! The fixture mixes a fail-level proof finding and a warn-level debug dump, so
//! both level spellings, `layer`/`level` JSON fields, and the exit code are
//! covered in one recording; `github` is recorded alongside for the same reason.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-recfmt-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

/// `(exit code, stdout)` for a run in `dir`, CI scrubbed so recordings don't drift on Actions.
fn run_in(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run steins");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
}

const FIXTURE: &str = "<?php\n\
    function width(int $w): int { return $w; }\n\
    width(\"abc\");\n\
    var_dump(1);\n";

const TEXT: &str = "\
a.php:3:7: error[type.argument-mismatch]: argument \"abc\" to width() cannot become int $w — proven TypeError (coercive mode)
a.php:4:10: warning[debug.var-dump]: dumped type: 1
";

const JSON: &str = r#"{
  "findings": [
    {
      "id": "type.argument-mismatch",
      "layer": "proof",
      "level": "fail",
      "path": "a.php",
      "line": 3,
      "column": 7,
      "message": "argument \"abc\" to width() cannot become int $w — proven TypeError (coercive mode)"
    },
    {
      "id": "debug.var-dump",
      "layer": "debug",
      "level": "warn",
      "path": "a.php",
      "line": 4,
      "column": 10,
      "message": "dumped type: 1"
    }
  ],
  "profile": "default",
  "vendor_suppressed": 0,
  "suppressed": 0,
  "baselined": 0
}
"#;

const GITHUB: &str = "\
::error file=a.php,line=3,col=7,title=type.argument-mismatch::argument \"abc\" to width() cannot become int $w — proven TypeError (coercive mode)
::notice file=a.php,line=4,col=10,title=debug.var-dump::dumped type: 1
";

fn recorded(tag: &str, format: &str, expected: &str) {
    let dir = workdir(tag);
    std::fs::write(dir.join("a.php"), FIXTURE).expect("write fixture");
    let (code, stdout) = run_in(&dir, &["check", "--no-php", "--format", format, "a.php"]);
    assert_eq!(stdout, expected, "`--format {format}` output drifted");
    // ADR-0050 §7 identity, not per-format (ADR-0054 §13 refuses `--exit-zero`):
    // one fail-level finding → every format exits 1.
    assert_eq!(code, 1, "`--format {format}` exit code");
}

#[test]
fn text_is_byte_identical() {
    recorded("text", "text", TEXT);
}

#[test]
fn json_is_byte_identical() {
    recorded("json", "json", JSON);
}

#[test]
fn github_matches_its_committed_shape() {
    recorded("github", "github", GITHUB);
}
