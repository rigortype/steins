//! The `steins` binary (ADR-0020). `check` walks `.php` files, runs the salsa
//! pipeline, prints proof-layer diagnostics, exits 1 if any finding was
//! reported. `annotate` reprints a file with a right-margin *proven*-fact
//! column. `transform`, `effect-diff`, `doctor`, `version`, `license` complete it.
//!
//! One file per subcommand (`check`, `annotate`, `transform`, `effect_diff`,
//! `doctor`, `mcp`); `config` parses `steins.toml` and `project` loads the
//! salsa project they share. Sibling modules (`doctor`, `mcp`, `render`) reach
//! the shared helpers as `crate::X` through the re-exports below.

// Output seam (issue #44), declared first: `outln!`/`out!`/`errln!` are
// textually-scoped macros, so every module using them must come after this.
#[macro_use]
mod out;

mod annotate;
mod baseline;
mod check;
mod config;
mod doctor;
mod effect_baseline;
mod effect_diff;
mod mcp;
mod project;
mod render;
mod sarif;
mod sha256;
mod transform;

// Shared with the wasm playground (no-second-relation discipline for surface selection).
pub(crate) use steins_infer::profile;

use std::io::Write as _;
use std::process::ExitCode;

pub(crate) use check::suppression_pipeline;
pub(crate) use config::{
    RuntimeConfig, allow_list, allow_list_from_disk, effects_from_config, effects_policy_from_disk,
    profiles_from_config, read_steins_config, runtime_from_config,
};
pub(crate) use project::{
    collect_files, collect_php_files, dedup_canonical, load_project, missing_paths,
    reject_missing_paths, resolve_layout,
};
pub(crate) use transform::{plan_transform_run, post_check, transform_json};
use annotate::run_annotate;
use check::run_check;
use effect_diff::run_effect_diff;
use transform::run_transform;

/// The `text|json` pair `annotate`, `transform` and `effect-diff` share.
/// `check` uses its own [`render::CheckFormat`] instead (CI renderings, ADR-0054).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

/// Headroom for the worker thread every subcommand runs on (issue #246).
/// CST-walking recursion costs one stack frame per nesting level; this
/// workspace overflowed the ~8 MiB OS default around 520 levels in debug,
/// ~2,700 in release — under phpstan-src's 1,000-level chain-walk fixture. A
/// stack-overflow abort loses the whole run (ADR-0009), so #246 chose
/// headroom over a depth cutoff. 256 MiB matches the nsrt harness's
/// `WORKER_STACK_SIZE` (`xtask/src/nsrt.rs`), costing nothing when unused.
/// Sizes the binary only — steins-syntax and the wasm playground aren't covered.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Output seam (issue #44): `out::finish` flushes stdout, maps write failure
    // to exit code. Worker thread sized per `WORKER_STACK_SIZE` (see its doc).
    let code = std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || dispatch(&args))
        .expect("failed to spawn the steins worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    out::finish(code)
}

/// Dispatch on the subcommand. Split out of `main` so there is exactly one exit
/// path through [`out::finish`] rather than one per `return`.
fn dispatch(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("check") => run_check(&args[1..]),
        Some("annotate") => run_annotate(&args[1..]),
        Some("transform") => run_transform(&args[1..]),
        Some("effect-diff") => run_effect_diff(&args[1..]),
        Some("doctor") => doctor::run_doctor(&args[1..]),
        Some("mcp") => mcp::run_mcp(&args[1..]),
        Some("version" | "--version" | "-v") => print_version(),
        Some("license" | "licenses") => print_license(),
        Some(other) => {
            errln!(
                "steins: unknown command `{other}` (available: check, annotate, transform, effect-diff, doctor, mcp, version, license)"
            );
            ExitCode::from(2)
        }
        None => {
            errln!(
                "usage: steins check [--format text|json|github|sarif] [--profile <name>] [--no-php] [--no-tolerated-effects] [--vendor-diagnostics] [--fix] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>"
            );
            errln!("       steins annotate [--no-php] [--format text|json] <file.php>");
            errln!(
                "       steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map> [--apply] [--asserted-subjects] [--format text|json] <paths...>"
            );
            errln!(
                "       steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json] <paths...>"
            );
            errln!("       steins doctor [--no-php] [--baseline <path>] [--format text|json] [path]");
            errln!("       steins mcp");
            errln!("       steins version | -v | --version");
            errln!("       steins license");
            ExitCode::from(2)
        }
    }
}

/// Steins' own terms, embedded (#43): Homebrew and `cargo install --git` both
/// produce a binary with no `LICENSE` file, and Apache-2.0 §4(a) requires one.
const LICENSE_APACHE: &str = include_str!("../../../LICENSE");

/// Bundled dependencies' notices (their MIT/BSD/ISC terms require it).
/// Generated by `cargo xtask licenses`; a CI drift guard keeps it current.
const THIRD_PARTY_LICENSES: &str = include_str!("../../../THIRD-PARTY-LICENSES.md");

/// PHPStan's own MIT notice — not a Rust dependency, so the generator can't
/// discover it; embedded by hand to credit Steins' direct model (README).
const LICENSE_PHPSTAN: &str = include_str!("../../../LICENSE-PHPSTAN");

/// The `version` banner. Build date/revision come from `build.rs`, degrading
/// to `unknown` outside a git working tree.
fn version_text() -> String {
    format!(
        concat!(
            "steins {} ({} revision {}) - {}\n",
            // `authors` carries only the individual; README names `TypedDuck`.
            "Copyright (c) TypedDuck, {}\n",
            "    Built with the help of many third-party libraries.\n",
            "    Run `steins license` to see all dependencies and their licenses.",
        ),
        env!("CARGO_PKG_VERSION"),
        env!("STEINS_BUILD_DATE"),
        env!("STEINS_GIT_REV"),
        env!("CARGO_PKG_REPOSITORY"),
        env!("CARGO_PKG_AUTHORS"),
    )
}

/// The `license` output: Steins' Apache-2.0 terms, PHPStan's MIT notice, then
/// every dependency notice. Apache-2.0 appears twice deliberately (standalone
/// `THIRD-PARTY-LICENSES.md` ships without `LICENSE`, Homebrew).
fn license_text() -> String {
    format!(
        "steins {} — open source licenses\n{}\n\n\
         Steins is licensed under the Apache License 2.0:\n\n\
         {}\n\n\
         Steins draws on a number of PHP type checkers, but PHPStan is its direct \
         model, and many of its rules are borrowed straight from PHPStan. PHPStan is \
         licensed under the MIT License:\n\n\
         {}\n\n{THIRD_PARTY_LICENSES}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_REPOSITORY"),
        LICENSE_APACHE.trim_start_matches('\n'),
        LICENSE_PHPSTAN.trim_start_matches('\n'),
    )
}

/// `version` / `-v` / `--version`.
fn print_version() -> ExitCode {
    outln!("{}", version_text());
    ExitCode::SUCCESS
}

/// `license` / `licenses`. Thousands of lines, so an interactive terminal
/// pages it; piped/redirected output goes straight to stdout (EPIPE-safe
/// seam, see [`mod@out`]).
fn print_license() -> ExitCode {
    let text = license_text();
    let pager = std::env::var("PAGER").ok();
    if should_page(out::stdout_is_terminal(), pager.as_deref()) {
        // `should_page` confirmed `pager` is non-blank.
        return page_through(pager.as_deref().expect("should_page confirmed a pager"), &text);
    }
    out!("{text}");
    ExitCode::SUCCESS
}

/// Whether to page: an interactive terminal AND a non-blank `$PAGER`. A pure
/// function so the decision (not the untestable terminal/process) is covered
/// by `tests::pager_policy`.
fn should_page(stdout_is_terminal: bool, pager: Option<&str>) -> bool {
    stdout_is_terminal && pager.is_some_and(|p| !p.trim().is_empty())
}

/// Spawn `pager` (e.g. `less -R`) via `sh -c` and write `text` to its stdin.
/// If it cannot even be spawned (a typo'd `$PAGER`), print `text` directly
/// instead of losing the output.
fn page_through(pager: &str, text: &str) -> ExitCode {
    let mut child = match std::process::Command::new("sh")
        .arg("-c")
        .arg(pager)
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            errln!("steins: cannot start pager `{pager}` ({e}); printing directly");
            out!("{text}");
            return ExitCode::SUCCESS;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A pager quitting early (`q`) closes stdin first, same EPIPE policy as `out`.
        let _ = stdin.write_all(text.as_bytes());
    }
    match child.wait() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            errln!("steins: pager `{pager}` exited with {status}");
            ExitCode::FAILURE
        }
        Err(e) => {
            errln!("steins: pager `{pager}` failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- should_page --------------------------------------------------------

    /// Paging requires both an interactive terminal AND a non-blank `$PAGER`;
    /// piped output (`tests/license.rs`) must never page even with `$PAGER` set.
    #[test]
    fn pager_policy() {
        assert!(should_page(true, Some("less")));
        assert!(!should_page(false, Some("less")), "a pipe or redirect must never page");
        assert!(!should_page(true, None), "no PAGER set must not page");
        assert!(!should_page(true, Some("")), "an empty PAGER must not page");
        assert!(!should_page(true, Some("   ")), "a blank PAGER must not page");
    }
}
