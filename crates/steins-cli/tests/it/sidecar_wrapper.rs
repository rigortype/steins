//! End-to-end test for issue #894: a `php` on `PATH` that is a shell wrapper
//! running the real interpreter without `exec`.
//!
//! Version managers and container shims are often written this way. The
//! sidecar's child is then the wrapper and the interpreter is its child, and
//! closing the sidecar at the end of the run waited on an interpreter the kill
//! never reached, so `check` never exited. The wrapper here stands in front of
//! the host's real `php` on a private `PATH` (`Command::env`, as in
//! `sidecar_handshake.rs`), and the run is polled against a deadline so a hang
//! fails the test instead of stalling the suite.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// A fresh directory holding a `php` that runs the real one without `exec`, or
/// `None` (printing a skip marker) when there is no `php` on `PATH` to wrap.
fn wrapper_dir() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let Some(real) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join("php")).find(|php| php.is_file())
    }) else {
        eprintln!("SKIP check_finishes_behind_a_php_wrapper: no `php` on PATH to wrap");
        return None;
    };
    let dir = std::env::temp_dir().join(format!("steins-wrapper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the wrapper dir");
    let php = dir.join("php");
    std::fs::write(&php, format!("#!/bin/sh\n'{}' \"$@\"\n", real.display()))
        .expect("write the wrapper");
    std::fs::set_permissions(&php, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    Some(dir)
}

/// `check` folds through the wrapper's interpreter and then exits: the folded
/// finding proves the sidecar ran behind the wrapper, and the deadline that the
/// run finished at all.
#[test]
fn check_finishes_behind_a_php_wrapper_that_does_not_exec() {
    let Some(dir) = wrapper_dir() else { return };
    let path = fixture("fold_mixed.php");
    let mut child = Command::new(env!("CARGO_BIN_EXE_steins"))
        .env_remove("GITHUB_ACTIONS")
        .args(["check", "--no-cache", path.to_str().unwrap()])
        .env("PATH", &dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run steins");
    let limit = Duration::from_secs(60);
    let deadline = Instant::now() + limit;
    while child.try_wait().expect("poll steins").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            panic!("check did not exit within {limit:?} behind a wrapper that does not exec");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("collect steins");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "both findings, got:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("folded from strtolower(\"XYZ\")"),
        "the sidecar answered through the wrapper, got:\n{stdout}\n{stderr}"
    );
    assert!(!stderr.contains("sound subset"), "the sidecar did not degrade, got:\n{stderr}");
}
