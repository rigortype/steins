//! Steins dev tooling (the cargo-xtask pattern; ADR-0013/0021).
//!
//! ```text
//! cargo xtask <command>
//!
//!   corpus-sync [--update]   materialize the pinned FP-gate corpus into corpus/
//!   fold-probe [--names …]   differential 32/64-bit width probe over the fold allowlist
//!   fp-gate                  run the proof-layer pipeline over the corpus (gate)
//!   freq                     builtin-call frequency, written to docs/notes/
//!   gen-catalog [--check]    regenerate the builtin tables from mining TOML (--check: verify only)
//!   lean-check [--bless]     check the committed Lean 4 vectors against the spec
//!   licenses                 regenerate THIRD-PARTY-LICENSES.md from cargo-about
//!   mine-function-map [DIR]  mine phpstan-src's functionMap into the declared-envelope TOML
//!   mine-param-facts         mine the engine's own arginfo into the parameter-facts TOML
//!   nsrt [DIR]               assertType harness (oracle idea B) over phpstan-src nsrt
//!   phpdoc-oracle [--check]  diff steins-phpdoc against the real phpstan/phpdoc-parser
//! ```
//!
//! It links the analysis crates directly (never shells out to the `steins`
//! binary) so it reads parse errors and call data straight off `SourceTree`.

mod corpus;
mod corpus_local;
mod fold_probe;
mod freq;
mod licenses;
mod gate;
mod mine_function_map;
mod mine_param_facts;
mod gen_catalog;
mod lean_check;
mod nsrt;
mod phpdoc_oracle;
mod sync;

use std::process::ExitCode;

/// Headroom for the rayon workers `fp-gate` and `freq` analyze packages on
/// (issue #246).
///
/// Both fan out with `PACKAGES.par_iter()`, so parsing happens on rayon's pool,
/// not `main`. Rayon defaults to std's 2 MiB thread stack — a quarter of the
/// ~8 MiB issue #246 found too small for a deeply nested expression under
/// debug's large, uninlined frames. `fp-gate` is a debug-built CI job reading a
/// corpus that includes an unpinned local checkout, so "no package has a deep
/// chain today" is not a property anything holds.
///
/// Same reasoning and number as `WORKER_STACK_SIZE`
/// (`crates/steins-cli/src/main.rs`): buy headroom for a finite walk. Lazily
/// committed, so the reservation costs nothing until frames are touched.
const RAYON_STACK_SIZE: usize = 256 * 1024 * 1024;

fn main() -> ExitCode {
    // Sized before any `par_iter` runs — `build_global` refuses once the default
    // pool exists.
    if let Err(e) = rayon::ThreadPoolBuilder::new().stack_size(RAYON_STACK_SIZE).build_global() {
        return fail(&format!("failed to size the rayon worker stacks: {e}"));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("corpus-sync") => {
            let update = args[1..].iter().any(|a| a == "--update");
            match sync::run(update) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some("fold-probe") => match fold_probe::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Some("fp-gate") => match gate::run() {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE, // ADR-0013: any diagnostic on clean code blocks release.
            Err(e) => fail(&e),
        },
        Some("freq") => match freq::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Some("gen-catalog") => match gen_catalog::run(args[1..].iter().any(|a| a == "--check")) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Some("lean-check") => {
            let bless = args[1..].iter().any(|a| a == "--bless");
            match lean_check::run(bless) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some("licenses") => match licenses::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Some("mine-function-map") => {
            let dir = args.get(1).filter(|a| !a.starts_with("--")).map(String::as_str);
            match mine_function_map::run(dir) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some("mine-param-facts") => match mine_param_facts::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Some("nsrt") => {
            let dir = args.get(1).filter(|a| !a.starts_with("--")).map(String::as_str);
            match nsrt::run(dir) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some("phpdoc-oracle") => {
            let check = args[1..].iter().any(|a| a == "--check");
            match phpdoc_oracle::run(check) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some(other) => fail(&format!(
            "unknown command `{other}` (corpus-sync | fp-gate | freq | gen-catalog | lean-check | licenses | mine-function-map | nsrt | phpdoc-oracle)"
        )),
        None => {
            eprintln!(
                "usage: cargo xtask <corpus-sync [--update] | fp-gate | freq | gen-catalog | lean-check [--bless] | licenses | mine-function-map [DIR] | nsrt [DIR] | phpdoc-oracle [--check]>"
            );
            ExitCode::from(2)
        }
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("xtask: {msg}");
    ExitCode::from(2)
}
