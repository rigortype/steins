//! Regression test for issue #246 at the binary's own entry point.
//!
//! `SourceTree::parse`'s lowering walkers (`scan_effect_origins` and its siblings
//! in `crates/steins-syntax/src/lib.rs`) recurse one frame per CST node. A
//! property-fetch chain is a finite tree and the walk terminates, but the descent
//! costs stack in proportion to depth: measured on the OS default ~8 MiB stack,
//! `steins check` aborted with `fatal runtime error: stack overflow` at roughly
//! 520 `->next` levels in a debug build and roughly 2,700 in a release one.
//! phpstan-src's own `tests/bench/data/nullsafe-chain-walk.php` is 1,000 levels
//! deep, which is past the first number and 40% of the way to the second.
//!
//! PR #253 gave the nsrt harness a sized worker thread; `main` now does the same
//! for every subcommand (`WORKER_STACK_SIZE` in `crates/steins-cli/src/main.rs`).
//! This drives the real binary over a chain past BOTH ceilings, so it is a
//! meaningful check in either build profile.
//!
//! A stack overflow is not a catchable panic — there is no assertion to make on
//! the failure side from inside the process. Running the binary as a subprocess
//! is what makes the failure observable: an overflow kills it by signal (no exit
//! code) after printing to stderr, and both are asserted below.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Past the release ceiling (~2,700) as well as the debug one (~520), so the
/// test does not quietly stop testing anything under `--release`.
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
