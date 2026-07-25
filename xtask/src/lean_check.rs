//! `lean-check`: verify the committed differential vectors are still what the
//! Lean 4 specification produces (ADR-0059).
//!
//! The loop has three legs, mirroring the phpdoc oracle's shape (ADR-0029):
//!
//!   1. the Lean spec proves the domain's soundness contract (`lake build`);
//!   2. it prints the vector file (`lake exe vectors`) — this command checks the
//!      committed fixture is byte-identical to that output;
//!   3. `cargo test -p steins-domain --test lean_vectors` regenerates the same
//!      vectors from the Rust implementation and diffs them.
//!
//! Leg 3 runs in the ordinary test suite and needs no Lean. Only this command
//! does, which is why it *skips* — never fails — when no toolchain is present:
//! the fixture is committed, so a machine without Lean still gets the full
//! Rust-side check.
//!
//! With `--bless`, the fixture is overwritten instead of compared. Use it after
//! deliberately changing the spec, and re-run the Rust test afterwards.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::corpus::repo_root;

/// Entry point for `cargo xtask lean-check [--bless]`.
pub fn run(bless: bool) -> Result<(), String> {
    let root = repo_root();
    let spike = root.join("spike").join("lean-domain");
    let fixture = root
        .join("crates")
        .join("steins-domain")
        .join("tests")
        .join("fixtures")
        .join("lean-vectors.expected");

    if !spike.join("lakefile.toml").is_file() {
        return Err(format!("Lean spec not found at {}", spike.display()));
    }

    let Some(runner) = Runner::detect() else {
        println!(
            "lean-check: no Lean toolchain and no nix — skipping.\n\
             \x20 The committed vectors are still checked against the Rust implementation by\n\
             \x20 `cargo test -p steins-domain --test lean_vectors`; this command only verifies\n\
             \x20 that the fixture still matches what the spec prints.\n\
             \x20 To get a toolchain: `nix develop` in {}, or install elan.",
            spike.display()
        );
        return Ok(());
    };
    println!("lean-check: spec = {}, runner = {}\n", spike.display(), runner.describe());

    // Leg 1: the proofs must still compile. `lake build` is the whole point —
    // a spec that does not build proves nothing.
    let build = runner.run(&spike, &["build"])?;
    if !build.status.success() {
        return Err(format!(
            "`lake build` failed — the specification does not compile:\n{}",
            String::from_utf8_lossy(&build.stderr).trim()
        ));
    }
    println!("lean-check: proofs compile.");

    // Leg 2: the vector file.
    let out = runner.run(&spike, &["exe", "vectors"])?;
    if !out.status.success() {
        return Err(format!(
            "`lake exe vectors` failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let produced = String::from_utf8(out.stdout)
        .map_err(|e| format!("the spec printed non-UTF-8 output: {e}"))?;

    if bless {
        std::fs::write(&fixture, &produced)
            .map_err(|e| format!("cannot write {}: {e}", fixture.display()))?;
        println!(
            "lean-check: blessed {} ({} lines).\n\
             Now run `cargo test -p steins-domain --test lean_vectors`.",
            fixture.display(),
            produced.lines().count()
        );
        return Ok(());
    }

    let committed = std::fs::read_to_string(&fixture)
        .map_err(|e| format!("cannot read {}: {e}", fixture.display()))?;
    if committed == produced {
        println!(
            "lean-check: {} lines agree with the specification.",
            produced.lines().count()
        );
        return Ok(());
    }

    Err(first_difference(&committed, &produced, &fixture))
}

/// Report the first differing line rather than a whole-file dump: the vectors
/// are four thousand lines, and the first divergence is the one that explains
/// the rest.
fn first_difference(committed: &str, produced: &str, fixture: &Path) -> String {
    let mut want = committed.lines();
    let mut got = produced.lines();
    let mut line = 1usize;
    loop {
        match (want.next(), got.next()) {
            (Some(w), Some(g)) if w == g => line += 1,
            (Some(w), Some(g)) => {
                return format!(
                    "{} is stale at line {line}:\n  committed: {w}\n  spec:      {g}\n\
                     Re-run with `--bless` if the spec change was intended.",
                    fixture.display()
                );
            }
            (None, Some(g)) => {
                return format!(
                    "{} is shorter than the spec's output: line {line} is `{g}`.\n\
                     Re-run with `--bless` if the spec change was intended.",
                    fixture.display()
                );
            }
            (Some(w), None) => {
                return format!(
                    "{} is longer than the spec's output: line {line} is `{w}`.\n\
                     Re-run with `--bless` if the spec change was intended.",
                    fixture.display()
                );
            }
            (None, None) => {
                // Byte-inequality with equal lines means a trailing-newline
                // difference; say so rather than reporting no difference.
                return format!(
                    "{} differs from the spec's output only in trailing whitespace.\n\
                     Re-run with `--bless`.",
                    fixture.display()
                );
            }
        }
    }
}

/// How to invoke `lake`: directly, or through the spike's own flake.
enum Runner {
    Lake(PathBuf),
    Nix(PathBuf),
}

impl Runner {
    fn detect() -> Option<Runner> {
        // A `lake` already on PATH wins: it is what an editor session uses, so
        // checking through it is checking what the author sees.
        if let Some(lake) = which("lake") {
            return Some(Runner::Lake(lake));
        }
        which("nix").map(Runner::Nix)
    }

    fn describe(&self) -> String {
        match self {
            Runner::Lake(p) => format!("{} (on PATH)", p.display()),
            Runner::Nix(p) => format!("{} develop (spike/lean-domain/flake.nix)", p.display()),
        }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
        let mut cmd = match self {
            Runner::Lake(lake) => {
                let mut c = Command::new(lake);
                c.args(args);
                c
            }
            Runner::Nix(nix) => {
                let mut c = Command::new(nix);
                c.arg("develop").arg("--command").arg("lake").args(args);
                c
            }
        };
        cmd.current_dir(dir);
        cmd.output().map_err(|e| format!("cannot run lake: {e}"))
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
