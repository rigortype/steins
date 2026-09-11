//! Steins dev tooling (the cargo-xtask pattern; ADR-0013/0021).
//!
//! ```text
//! cargo xtask <command>
//!
//!   artifact-bytes <DIR>…   where a package artifact's bytes go (issue #504)
//!   corpus-sync [--update]   materialize the pinned FP-gate corpus into corpus/
//!   fold-probe [--names …]   differential 32/64-bit width probe over the fold allowlist
//!   fp-gate                  run the proof-layer pipeline over the corpus (gate)
//!   freq                     builtin-call frequency, written to docs/notes/
//!   gen-catalog [--check]    regenerate the builtin tables from mining TOML (--check: verify only)
//!   lean-check [--bless]     check the committed Lean 4 vectors against the spec
//!   licenses                 regenerate THIRD-PARTY-LICENSES.md from cargo-about
//!   mine-constants [DIR] [--php PATH]…
//!                            mine the engines' constants (+ php-src ranges) into the constants TOML
//!   mine-function-map [DIR] [--functions] [--methods]
//!                            mine phpstan-src's functionMap into the declared-return TOMLs
//!   mine-param-facts         mine the engine's own arginfo into the parameter-facts TOML
//!   nsrt [DIR]               assertType harness (oracle idea B) over phpstan-src nsrt
//!   perf <DIR>… [--bless]    cold perf baseline + the determinism half of warm ≡ cold (ADR-0092 §5)
//!   phpdoc-oracle [--check]  diff steins-phpdoc against the real phpstan/phpdoc-parser
//! ```
//!
//! It links the analysis crates directly (never shells out to the `steins`
//! binary) so it reads parse errors and call data straight off `SourceTree`.

mod artifact_bytes;
mod corpus;
mod corpus_local;
mod fold_probe;
mod freq;
mod licenses;
mod gate;
mod mine_constants;
mod mine_function_map;
mod mine_param_facts;
mod gen_catalog;
mod lean_check;
mod nsrt;
mod perf;
mod phpdoc_oracle;
mod sha256;
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
        Some("artifact-bytes") => match artifact_bytes::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
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
        Some("mine-constants") => {
            let dir = args.get(1).filter(|a| !a.starts_with("--")).map(String::as_str);
            // `--php PATH`, repeatable: the engines whose values the run diffs
            // (ADR-0094 §2 — the generator runs over the minors the corpus
            // harness scopes, and one engine cannot disagree with itself).
            let php: Vec<String> = args
                .windows(2)
                .filter(|w| w[0] == "--php")
                .map(|w| w[1].clone())
                .collect();
            match mine_constants::run(dir, &php) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some("mine-function-map") => {
            let dir = args.get(1).filter(|a| !a.starts_with("--")).map(String::as_str);
            // Neither flag means both halves; either one alone narrows the run
            // (see `mine_function_map::Halves`).
            let functions = args.iter().any(|a| a == "--functions");
            let methods = args.iter().any(|a| a == "--methods");
            let halves = mine_function_map::Halves {
                functions: functions || !methods,
                methods: methods || !functions,
            };
            match mine_function_map::run(dir, halves) {
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
        Some("perf") => match perf::run(&args[1..]) {
            Ok(true) => ExitCode::SUCCESS,
            // ADR-0092 §5: a determinism or blessed-findings break blocks; timing never does.
            Ok(false) => ExitCode::FAILURE,
            Err(e) => fail(&e),
        },
        Some("phpdoc-oracle") => {
            let check = args[1..].iter().any(|a| a == "--check");
            match phpdoc_oracle::run(check) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some(other) => fail(&format!(
            "unknown command `{other}` (artifact-bytes | corpus-sync | fp-gate | freq | gen-catalog | lean-check | licenses | mine-constants | mine-function-map | nsrt | perf | phpdoc-oracle)"
        )),
        None => {
            eprintln!(
                "usage: cargo xtask <artifact-bytes <DIR>… [--no-php] | corpus-sync [--update] | fp-gate | freq | gen-catalog | lean-check [--bless] | licenses | mine-constants [DIR] [--php PATH]… | mine-function-map [DIR] [--functions] [--methods] | nsrt [DIR] | perf <DIR>… [--runs N] [--bless] [--no-php] | phpdoc-oracle [--check]>"
            );
            ExitCode::from(2)
        }
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("xtask: {msg}");
    ExitCode::from(2)
}
