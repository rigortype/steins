//! Steins dev tooling (the cargo-xtask pattern; ADR-0013/0021).
//!
//! ```text
//! cargo xtask <command>
//!
//!   artifact-bytes <DIR>… [--no-php]
//!                            where a package artifact's bytes go (issue #504)
//!   corpus-sync [--update]   materialize the pinned FP-gate corpus into corpus/
//!   fold-probe [--names …]   differential 32/64-bit width probe over the fold allowlist
//!   fp-gate                  run the proof-layer pipeline over the corpus (gate)
//!   freq                     builtin-call frequency, written to docs/notes/
//!   gen-catalog [--check]    regenerate the builtin tables from mining TOML (--check: verify only)
//!   lean-check [--bless]     check the committed Lean 4 vectors against the spec
//!   licenses                 regenerate THIRD-PARTY-LICENSES.md from cargo-about
//!   mine-constants [--php PATH]…
//!                            mine the engines' constants and their minor ranges into the constants TOML
//!   mine-function-map [DIR] [--functions] [--methods] [--php PATH]…
//!                            mine phpstan-src's functionMap into the declared-return TOMLs
//!   mine-function-map [DIR] --migrated [--php PATH]…
//!                            derive the ADR-0097 §2.6 migrated-class table at declared_returns.toml's pin
//!   mine-param-facts [--php PATH]… [--merge TOML]…
//!                            union the engines' own arginfo into the parameter-facts TOML
//!   mine-resource-params [--php-src DIR]
//!                            scan php-src's stubs for `@param resource` positions into the resource-params TOML
//!   nsrt [DIR]               assertType harness (oracle idea B) over phpstan-src nsrt
//!   perf <DIR>… [--runs N] [--bless] [--no-php] [--warm] [--paranoid]
//!                            cold perf baseline + the determinism half of warm ≡ cold (ADR-0092 §5)
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
mod mine_resource_params;
mod gen_catalog;
mod lean_check;
mod nsrt;
mod perf;
mod phpdoc_oracle;
mod sha256;
mod sync;
mod test_layout;

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

/// One `cargo xtask` command.
///
/// The dispatch, the unknown-command list and the usage text are all read off
/// [`COMMANDS`], so a command is listed wherever it is dispatched (issue #776).
struct Command {
    name: &'static str,
    /// The arguments as the usage text spells them after the name; empty for none.
    usage: &'static str,
    /// Runs the command on the arguments after its name, which it parses itself.
    run: fn(&[String]) -> ExitCode,
}

/// Every command, in the order the usage text lists them.
const COMMANDS: &[Command] = &[
    Command {
        name: "artifact-bytes",
        usage: "<DIR>… [--no-php]",
        run: |args| outcome(artifact_bytes::run(args)),
    },
    Command {
        name: "corpus-sync",
        usage: "[--update]",
        run: |args| outcome(sync::run(args.iter().any(|a| a == "--update"))),
    },
    Command {
        name: "fold-probe",
        usage: "[--names a,b,c] [--strict] [--json OUT] [--unsafe]",
        run: |args| outcome(fold_probe::run(args)),
    },
    // ADR-0013: any diagnostic on clean code blocks release.
    Command { name: "fp-gate", usage: "", run: |_| verdict(gate::run()) },
    Command { name: "freq", usage: "", run: |_| outcome(freq::run()) },
    Command {
        name: "gen-catalog",
        usage: "[--check]",
        run: |args| outcome(gen_catalog::run(args.iter().any(|a| a == "--check"))),
    },
    Command {
        name: "lean-check",
        usage: "[--bless]",
        run: |args| outcome(lean_check::run(args.iter().any(|a| a == "--bless"))),
    },
    Command { name: "licenses", usage: "", run: |_| outcome(licenses::run()) },
    Command {
        name: "mine-constants",
        usage: "[--php PATH]…",
        run: |args| {
            // `--php PATH`, repeatable: the engines the run diffs, one per minor
            // (ADR-0094 §2 — the generator runs over the minors the corpus
            // harness scopes, and one engine cannot disagree with itself). Since
            // issue #718 these engines also answer the RANGE question, so a
            // single-engine run is rangeless as well as undiffed.
            let php: Vec<String> = args
                .windows(2)
                .filter(|w| w[0] == "--php")
                .map(|w| w[1].clone())
                .collect();
            outcome(mine_constants::run(&php))
        },
    },
    Command {
        name: "mine-function-map",
        usage: "[DIR] [--functions] [--methods] [--migrated] [--php PATH]…",
        run: |args| {
            let dir = args.first().filter(|a| !a.starts_with("--")).map(String::as_str);
            // `--php PATH`, repeatable: the countersigning engines (issue #714).
            // The top minor decides each row's bucket; the rest are vetoes.
            let php: Vec<String> = args
                .windows(2)
                .filter(|w| w[0] == "--php")
                .map(|w| w[1].clone())
                .collect();
            // `--migrated` writes the ADR-0097 §2.6 migrated-class table and
            // nothing else: it reads functionMap at `declared_returns.toml`'s own
            // pin, so the two declared-return TOMLs stay byte-identical.
            if args.iter().any(|a| a == "--migrated") {
                return outcome(mine_function_map::run_migrated(dir, &php));
            }
            // Neither flag means both halves; either one alone narrows the run
            // (see `mine_function_map::Halves`).
            let functions = args.iter().any(|a| a == "--functions");
            let methods = args.iter().any(|a| a == "--methods");
            let halves = mine_function_map::Halves {
                functions: functions || !methods,
                methods: methods || !functions,
            };
            outcome(mine_function_map::run(dir, halves, &php))
        },
    },
    Command {
        name: "mine-param-facts",
        usage: "[--php PATH]… [--merge TOML]…",
        run: |args| {
            // `--php PATH`, repeatable: the engines whose rows the run unions
            // (issue #703 — a build is one operating system, and `chroot` is a
            // Linux builtin). `--merge TOML` takes a previously-mined table as one
            // more source, which is how a Linux engine reaches this table from a
            // machine that has none.
            let php: Vec<String> = args
                .windows(2)
                .filter(|w| w[0] == "--php")
                .map(|w| w[1].clone())
                .collect();
            let merges: Vec<String> = args
                .windows(2)
                .filter(|w| w[0] == "--merge")
                .map(|w| w[1].clone())
                .collect();
            outcome(mine_param_facts::run(&php, &merges))
        },
    },
    Command {
        name: "mine-resource-params",
        usage: "[--php-src DIR]",
        run: |args| {
            // `--php-src DIR`: the pinned php-src checkout whose stubs are scanned
            // (ADR-0097 §2.5); the default is the checkout `resource_returns.toml`
            // was read from.
            let php_src = args
                .windows(2)
                .find(|w| w[0] == "--php-src")
                .map(|w| w[1].as_str());
            outcome(mine_resource_params::run(php_src))
        },
    },
    Command {
        name: "nsrt",
        usage: "[DIR]",
        run: |args| {
            let dir = args.first().filter(|a| !a.starts_with("--")).map(String::as_str);
            outcome(nsrt::run(dir))
        },
    },
    // ADR-0092 §5: a determinism or blessed-findings break blocks; timing never does.
    Command {
        name: "perf",
        usage: "<DIR>… [--runs N] [--bless] [--no-php] [--warm] [--paranoid]",
        run: |args| verdict(perf::run(args)),
    },
    Command {
        name: "phpdoc-oracle",
        usage: "[--check]",
        run: |args| outcome(phpdoc_oracle::run(args.iter().any(|a| a == "--check"))),
    },
];

fn main() -> ExitCode {
    // Sized before any `par_iter` runs — `build_global` refuses once the default
    // pool exists.
    if let Err(e) = rayon::ThreadPoolBuilder::new().stack_size(RAYON_STACK_SIZE).build_global() {
        return fail(&format!("failed to size the rayon worker stacks: {e}"));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(name) = args.first().map(String::as_str) else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    match COMMANDS.iter().find(|c| c.name == name) {
        Some(command) => (command.run)(&args[1..]),
        None => {
            let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
            fail(&format!("unknown command `{name}` ({})", names.join(" | ")))
        }
    }
}

/// The usage text: every command with its arguments, one per line.
fn usage() -> String {
    let lines: Vec<String> = COMMANDS
        .iter()
        .map(|c| format!("{} {}", c.name, c.usage).trim_end().to_owned())
        .collect();
    format!("usage: cargo xtask <{}>", lines.join("\n                  | "))
}

/// A command that did its work, or could not (exit 2).
fn outcome(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

/// A command that passes a verdict: `Ok(false)` is a red one, exit 1, which CI
/// reads apart from a command that could not run (exit 2).
fn verdict(result: Result<bool, String>) -> ExitCode {
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => fail(&e),
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("xtask: {msg}");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name listed twice would dispatch to its first entry only.
    #[test]
    fn command_names_are_unique_and_sorted() {
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
    }

    /// Issue #776: `fold-probe` was dispatched but missing from the usage text.
    #[test]
    fn every_command_heads_a_usage_line() {
        let usage = usage();
        let heads: Vec<&str> = usage
            .lines()
            .map(|l| l.trim_start_matches("usage: cargo xtask <").trim_start_matches([' ', '|']))
            .map(|l| l.split(' ').next().unwrap_or_default().trim_end_matches('>'))
            .collect();
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(heads, names, "{usage}");
    }

    /// The module doc's command list is the one list still written by hand.
    #[test]
    fn the_module_doc_lists_every_command() {
        let mut listed: Vec<&str> = include_str!("main.rs")
            .lines()
            .filter_map(|l| l.strip_prefix("//!   "))
            .filter(|l| !l.starts_with(' '))
            .map(|l| l.split(' ').next().unwrap_or_default())
            .collect();
        listed.dedup();
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(listed, names);
    }

    /// CI reads 1 as a red gate and 2 as a command that could not run.
    #[test]
    fn exit_codes() {
        assert_eq!(outcome(Ok(())), ExitCode::SUCCESS);
        assert_eq!(outcome(Err("e".to_owned())), ExitCode::from(2));
        assert_eq!(verdict(Ok(true)), ExitCode::SUCCESS);
        assert_eq!(verdict(Ok(false)), ExitCode::FAILURE);
        assert_eq!(verdict(Err("e".to_owned())), ExitCode::from(2));
    }
}
