//! The output seam (issue #44): a reader that closes early must not crash a
//! command, and must not invent a failure.
//!
//! `println!` panics on `EPIPE`, so `| head`, `| grep -m1` and quitting `less`
//! early — the normal ways to read a long report — turned correct usage into
//!
//! ```text
//! thread 'main' panicked at library/std/src/io/stdio.rs: failed printing to
//! stdout: Broken pipe (os error 32)
//! ```
//!
//! Two kinds of test live here. The end-to-end ones run the real binary against a
//! reader that goes away, one per long-output command (`annotate`'s reprint,
//! `check`'s finding list, `transform`'s diff, `doctor`'s report). The structural
//! one is the anti-regression device: it reads the crates' own source and fails if
//! a `println!` reappears anywhere outside `steins-cli/src/out.rs`, because a fix
//! that lives in a seam is only as good as the guarantee that nothing writes
//! around it.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// A fresh, unique working directory under the system temp dir.
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

/// Run `steins <args>` in `dir` with a reader that closes early.
///
/// With `read_first_line`, one line is consumed before the read end is dropped —
/// the `| head -1` shape, and the one that *guarantees* the writer meets `EPIPE`
/// when the report is larger than a pipe buffer. Without it the read end closes
/// before the child has written anything, which is the `less`-quit shape.
fn closed_reader(dir: &Path, args: &[&str], read_first_line: bool) -> Closed {
    let mut child = Command::new(bin())
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

/// A file long enough that its report cannot fit in a pipe buffer (64 KiB on
/// Linux and macOS): `annotate` reprints every line, so `n` assignments give `n`
/// output lines. 1,500 of them is roughly 100 KB.
fn long_php(n: usize) -> String {
    let mut s = String::from("<?php\n");
    for i in 0..n {
        s.push_str(&format!("$value_number_{i} = \"a string long enough to matter\";\n"));
    }
    s
}

/// A definition plus `n` coercive calls — `n` findings, so `check`'s report is
/// long *and* the run's own verdict is exit 1 (ADR-0050 §7).
fn many_findings(n: usize) -> String {
    let mut s = String::from("<?php\nfunction width(int $w): int { return $w; }\n");
    for _ in 0..n {
        s.push_str("width(\"abc\");\n");
    }
    s
}

// ------------------------------------------------ the reader goes away first ---

#[test]
fn annotate_survives_a_reader_that_reads_one_line() {
    // The strongest form: the report exceeds a pipe buffer, so the writer is
    // still going when the reader leaves and `EPIPE` is certain rather than racy.
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
    // Exit 1 because the tree has findings, NOT because the pipe closed. The seam
    // never turns a success into a failure; it equally never rewrites a real
    // verdict into 0 (`out.rs`, "The policy").
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
    // The diff is the long output here; the dry run writes nothing to disk.
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
    // stderr carries the sound-subset notice (ADR-0004), and `2>&1 | head` closes
    // it like any other pipe. The seam's stderr policy is to drop the write and
    // leave the verdict alone — losing a notice is not a reason to fail a run.
    let dir = workdir("stderr");
    write(&dir, "a.php", &long_php(20));
    let mut child = Command::new(bin())
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

// -------------------------------------------------- the anti-regression scan ---

/// Every `.rs` file under `crates/*/src/`.
fn crate_sources() -> Vec<PathBuf> {
    // `<root>/crates/steins-cli` → `<root>`.
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
    // The whole point of a seam: a future `println!` must not be able to
    // reintroduce the panic quietly. `out.rs` is the one file allowed to hold a
    // raw handle, and comments may still *name* the macros (this is how the
    // module documents what it replaces), so only code lines are considered.
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
