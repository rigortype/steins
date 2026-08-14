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
