//! Steins dev tooling (the cargo-xtask pattern; ADR-0013/0021).
//!
//! `cargo xtask <command>`:
//!   corpus-sync [--update]   materialize the pinned FP-gate corpus into `corpus/`
//!   fp-gate                  run the proof-layer pipeline over the corpus (gate)
//!   freq                     builtin-call frequency, written to docs/notes/
//!   gen-catalog              regenerate the builtin hierarchy table from mining TOML
//!   phpdoc-oracle [--check]  diff steins-phpdoc against the real phpstan/phpdoc-parser
//!
//! It links the analysis crates directly (never shells out to the `steins`
//! binary) so it reads parse errors and call data straight off `SourceTree`.

mod corpus;
mod corpus_local;
mod freq;
mod gate;
mod gen_catalog;
mod phpdoc_oracle;
mod sync;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("corpus-sync") => {
            let update = args[1..].iter().any(|a| a == "--update");
            match sync::run(update) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some("fp-gate") => match gate::run() {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE, // ADR-0013: any diagnostic on clean code blocks release.
            Err(e) => fail(&e),
        },
        Some("freq") => match freq::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Some("gen-catalog") => match gen_catalog::run() {
            Ok(()) => ExitCode::SUCCESS,
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
            "unknown command `{other}` (corpus-sync | fp-gate | freq | gen-catalog | phpdoc-oracle)"
        )),
        None => {
            eprintln!(
                "usage: cargo xtask <corpus-sync [--update] | fp-gate | freq | gen-catalog | phpdoc-oracle [--check]>"
            );
            ExitCode::from(2)
        }
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("xtask: {msg}");
    ExitCode::from(2)
}
