//! Regression test for issue #246 at the binary's own entry point.
//!
//! `SourceTree::parse`'s lowering walkers recurse one frame per CST node: on the
//! OS default ~8 MiB stack, `steins check` aborted with `fatal runtime error:
//! stack overflow` at ~520 `->next` levels in debug, ~2,700 in release.
//! phpstan-src's own 1,000-level fixture is past the first ceiling.
//!
//! PR #253 gave the nsrt harness a sized worker thread; `main` now does the same
//! for every subcommand (`WORKER_STACK_SIZE` in `crates/steins-cli/src/main.rs`),
//! driving the real binary past both ceilings in either build profile.
//!
//! A stack overflow isn't a catchable panic, so failure is asserted as a
//! subprocess signal death (no exit code) with the stderr message, below.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Scrubs `GITHUB_ACTIONS`: `check`'s format auto-detection (ADR-0054 §6)
/// reads it and would otherwise emit workflow commands instead of plain text.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

/// Past both measured ceilings (~520 debug, ~2,700 release), so `--release`
/// stays meaningful too.
const CHAIN_DEPTH: usize = 3_000;

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("steins-deepnest-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, contents).expect("write fixture");
    p
}

/// The shape of phpstan-src's `nullsafe-chain-walk.php`, parameterized by depth:
/// a declared-type property fetched `depth` times in one expression.
fn deep_chain_src(depth: usize) -> String {
    let mut src = String::from(
        "<?php declare(strict_types = 1);

namespace DeepNesting;

final class Node
{
    public Node $next;
}

function walk(Node $n): Node
{
    return $n",
    );
    for _ in 0..depth {
        src.push_str("->next");
    }
    src.push_str(";\n}\n");
    src
}

#[test]
fn a_deep_property_chain_does_not_overflow_the_stack() {
    let dir = workdir("chain");
    let file = write(&dir, "deep.php", &deep_chain_src(CHAIN_DEPTH));

    let out = steins_cmd()
        .args(["check", "--no-php"])
        .arg(&file)
        .output()
        .expect("run steins");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stderr.contains("overflowed its stack") && !stderr.contains("stack overflow"),
        "steins check overflowed its stack on a {CHAIN_DEPTH}-deep property chain:\n{stderr}"
    );
    assert!(
        out.status.code().is_some(),
        "steins check died by signal on a {CHAIN_DEPTH}-deep property chain (status {:?}):\n{stderr}",
        out.status
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A function with `depth` nested loops, `$x` read before the innermost `if` binds it:
/// every read of `$x` is a `variable.maybe-undefined`.
fn deep_loop_src(kind: &str, depth: usize) -> String {
    let mut src = String::from("<?php\n\nfunction walk(array $a, bool $c): void\n{\n");
    for i in 0..depth {
        match kind {
            "foreach" => src.push_str(&format!("foreach ($a as $v{i}) {{\n")),
            "while" => src.push_str("while ($c) {\n"),
            _ => src.push_str(&format!("for ($i{i} = 0; $i{i} < 10; $i{i}++) {{\n")),
        }
        if i % 8 == 0 {
            src.push_str("echo $x;\n");
        }
    }
    src.push_str("if ($c) { $x = 1; }\necho $x;\n");
    for _ in 0..depth {
        src.push_str("}\n");
    }
    src.push_str("}\n");
    src
}

/// Issue #793: the binding-presence pass walked a loop body up to three times per
/// enclosing loop, so `foreach` 20 deep took 11 s and 22 deep 43 s in a release build.
/// This is the whole `check` — lowering and analysis — on a nest that did not finish,
/// pinned to a wall-clock budget generous for a debug build (the cached run is
/// well under a second). The subprocess is killed at the deadline so a regression
/// fails the test instead of hanging the suite.
#[test]
fn deeply_nested_loops_check_in_bounded_time() {
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
    let dir = workdir("loops");
    for kind in ["foreach", "while", "for"] {
        let file = write(&dir, &format!("{kind}.php"), &deep_loop_src(kind, 24));
        let started = std::time::Instant::now();
        let mut child = steins_cmd()
            .args(["check", "--profile", "strict", "--no-php", "--no-cache", "--format", "json"])
            .arg(&file)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("run steins");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let reader = std::thread::spawn(move || {
            let mut out = String::new();
            std::io::Read::read_to_string(&mut stdout, &mut out).expect("read stdout");
            out
        });
        while child.try_wait().expect("poll steins").is_none() {
            if started.elapsed() > BUDGET {
                let _ = child.kill();
                panic!("`steins check` on 24 nested `{kind}` loops overran {BUDGET:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let out = reader.join().expect("reader thread");
        eprintln!("24 nested `{kind}`: checked in {:?}", started.elapsed());
        // Four reads: levels 0, 8 and 16, and the innermost.
        assert_eq!(
            out.matches("variable.maybe-undefined").count(),
            4,
            "the nest's reads of `$x` must still report:\n{out}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
