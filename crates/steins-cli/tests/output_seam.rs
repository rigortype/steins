//! Output-seam contracts (issue #44): a closed stdout or stderr must neither
//! panic nor alter the command's verdict.
//!
//! End-to-end tests cover long-output commands under `EPIPE`; a structural test
//! prevents raw output calls from bypassing `steins-cli/src/out.rs`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test in this file spawns the binary with `GITHUB_ACTIONS` scrubbed.
/// `check`'s format auto-detection (ADR-0054 §6) reads that variable, so a test
/// run *on* GitHub Actions would otherwise get workflow commands where it
/// asserted text. No test's expected output may depend on the ambient CI
/// environment; detection itself is tested in `tests/format_github.rs`, which
/// sets the variable deliberately.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-outseam-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

/// The outcome of a run whose reader went away: the exit code and stderr.
struct Closed {
    code: i32,
    stderr: String,
}

/// Run with a reader that closes immediately or, when `read_first_line`, after
/// one line. A report larger than the pipe buffer then guarantees `EPIPE`.
fn closed_reader(dir: &Path, args: &[&str], read_first_line: bool) -> Closed {
    let mut child = steins_cmd()
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn steins");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    if read_first_line {
        let mut line = String::new();
        let _ = BufReader::new(&mut stdout).read_line(&mut line);
    }
    drop(stdout);
    let out = child.wait_with_output().expect("wait for steins");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!stderr.contains("panicked"), "a closed reader must not panic:\n{stderr}");
    assert_ne!(out.status.code(), Some(101), "101 is the panic exit code:\n{stderr}");
    Closed { code: out.status.code().unwrap_or(-1), stderr }
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// Input whose annotated output can exceed a 64 KiB pipe buffer.
fn long_php(n: usize) -> String {
    let mut s = String::from("<?php\n");
    for i in 0..n {
        s.push_str(&format!("$value_number_{i} = \"a string long enough to matter\";\n"));
    }
    s
}

/// Produce `n` findings and an intrinsic exit status of 1 (ADR-0050 §7).
fn many_findings(n: usize) -> String {
    let mut s = String::from("<?php\nfunction width(int $w): int { return $w; }\n");
    for _ in 0..n {
        s.push_str("width(\"abc\");\n");
    }
    s
}

#[test]
fn annotate_survives_a_reader_that_reads_one_line() {
    // Exceeding the pipe buffer makes `EPIPE` deterministic.
    let dir = workdir("annotate");
    write(&dir, "long.php", &long_php(1500));
    let r = closed_reader(&dir, &["annotate", "--no-php", "long.php"], true);
    assert_eq!(r.code, 0, "annotate has no failure verdict; stderr:\n{}", r.stderr);
}

#[test]
fn check_survives_a_reader_that_reads_one_line() {
    let dir = workdir("check-head");
    write(&dir, "a.php", &many_findings(800));
    let r = closed_reader(&dir, &["check", "--no-php", "a.php"], true);
    // Findings, not the closed pipe, determine exit 1.
    assert_eq!(r.code, 1, "findings still decide the exit; stderr:\n{}", r.stderr);
}

#[test]
fn a_clean_check_with_a_closed_reader_exits_zero() {
    let dir = workdir("check-clean");
    write(&dir, "a.php", &long_php(200));
    let r = closed_reader(&dir, &["check", "--no-php", "a.php"], false);
    assert_eq!(r.code, 0, "no findings and a closed pipe → 0; stderr:\n{}", r.stderr);
}

#[test]
fn doctor_survives_a_closed_reader() {
    let dir = workdir("doctor");
    write(&dir, "a.php", &long_php(50));
    let r = closed_reader(&dir, &["doctor", "--no-php", "."], false);
    assert_eq!(r.code, 0, "a posture report is exit-neutral; stderr:\n{}", r.stderr);
}

#[test]
fn transform_survives_a_closed_reader() {
    let dir = workdir("transform");
    write(
        &dir,
        "a.php",
        "<?php\n/** @param int $a */\nfunction f($a) { return $a; }\n",
    );
    let r = closed_reader(&dir, &["transform", "phpdoc-to-native", "a.php"], false);
    assert_eq!(r.code, 0, "a clean dry run is exit 0; stderr:\n{}", r.stderr);
}

#[test]
fn a_closed_stderr_does_not_crash_a_run() {
    // A dropped stderr notice must not change the verdict (ADR-0004).
    let dir = workdir("stderr");
    write(&dir, "a.php", &long_php(20));
    let mut child = steins_cmd()
        .args(["check", "--no-php", "a.php"])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn steins");
    drop(child.stderr.take());
    let out = child.wait_with_output().expect("wait for steins");
    assert_eq!(out.status.code(), Some(0), "a closed stderr changes nothing");
}

fn crate_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();
    let mut out = Vec::new();
    let crates = std::fs::read_dir(root.join("crates")).expect("read crates/");
    for entry in crates.flatten() {
        collect_rs(&entry.path().join("src"), &mut out);
    }
    out.sort();
    assert!(out.len() > 20, "the source scan found almost nothing — did the layout move?");
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn nothing_writes_around_the_output_seam() {
    // Only the seam may use raw output; mentions in comments are ignored.
    let seam = Path::new("steins-cli").join("src").join("out.rs");
    let mut offenders: Vec<String> = Vec::new();
    for file in crate_sources() {
        if file.ends_with(&seam) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read a source file");
        for (i, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for needle in ["print!(", "println!(", "io::stdout("] {
                if code.contains(needle) {
                    offenders.push(format!("{}:{}: {}", file.display(), i + 1, code.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these write around the output seam and will panic on a closed pipe \
         (use outln!/out!/errln! from steins-cli's `out` module):\n{}",
        offenders.join("\n")
    );
}
