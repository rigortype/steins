//! The output seam: every user-facing byte this binary writes goes through here
//! (issue #44).
//!
//! # Why a seam rather than a per-call-site fix
//!
//! `println!` panics when its write fails, and the one failure a correctly-used
//! CLI hits constantly is `EPIPE`: `| head`, `| grep -m1`, and quitting `less`
//! early all close the read end while the writer is still going. `steins license`
//! emits over two thousand lines and `check` on a large tree emits one line per
//! finding, so the panic
//!
//! ```text
//! thread 'main' panicked at library/std/src/io/stdio.rs: failed printing to
//! stdout: Broken pipe (os error 32)
//! ```
//!
//! was a crash report for ordinary use. Fixing it call site by call site leaves
//! the next `println!` free to reintroduce it silently, so the rule lives in one
//! place instead: `outln!` / `out!` / `errln!` are the only way this crate
//! writes, and `crates/steins-cli/tests/output_seam.rs` fails the build if a
//! `println!` reappears anywhere in the CLI's source.
//!
//! # The policy
//!
//! * **stdout, closed reader** — stop writing (the rest of the report has no
//!   destination, and one skipped branch beats thousands of failing syscalls) and
//!   leave the exit code alone. A reader that quit early has not caused a
//!   failure, so nothing here turns a successful run into `1` — and nothing here
//!   turns a *failing* run into `0` either: `check` exits 1 because the tree has
//!   findings (ADR-0050 §7), which stays true whether or not the pager was still
//!   listening. The pipe never *introduces* a nonzero exit; that is the property
//!   the tests pin.
//! * **stdout, any other error** — a genuine write failure (a full disk, an I/O
//!   error) is reported once on stderr and makes the process exit 1 via
//!   [`finish`]. That is the pre-existing behaviour of the `license`/`version`
//!   writer this module generalizes.
//! * **stderr, any error** — ignored. There is nowhere to report a broken stderr,
//!   and losing the diagnostic channel does not change what the analysis found,
//!   so it must not change the exit code either. This is the deliberate asymmetry
//!   the issue asks to record.
//!
//! # Buffering
//!
//! Writes go straight to `std::io::stdout()`, which is line-buffered, so this
//! seam issues exactly the syscalls `println!` did. Wrapping it in a `BufWriter`
//! would be faster for a long report but would reorder stdout against the stderr
//! notices, and correctness of the interleaving is worth more than the syscalls.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

// The macros come first: `macro_rules!` is textually scoped, so a definition has
// to precede its use — including `record`'s use of `errln!` below.

/// `println!` for this crate: one line to stdout, never panicking on a closed
/// reader. See the module docs for the policy.
macro_rules! outln {
    () => { $crate::out::stdout_str("\n") };
    ($($arg:tt)*) => { $crate::out::stdout_line(format_args!($($arg)*)) };
}

/// `print!` for this crate: stdout with no trailing newline.
macro_rules! out {
    ($($arg:tt)*) => { $crate::out::stdout_fmt(format_args!($($arg)*)) };
}

/// `eprintln!` for this crate: one line to stderr, never panicking.
macro_rules! errln {
    ($($arg:tt)*) => { $crate::out::stderr_line(format_args!($($arg)*)) };
}

/// Set once stdout has refused a write — further writes are skipped. Covers both
/// the closed-reader case and a hard error; in either case there is nothing more
/// this process can put on stdout.
static STDOUT_STOPPED: AtomicBool = AtomicBool::new(false);

/// Set when stdout failed for a reason that is *not* a closed reader. Only this
/// flag reaches the exit code (see [`finish`]).
static STDOUT_FAILED: AtomicBool = AtomicBool::new(false);

/// Write `text` to stdout verbatim (no trailing newline).
pub fn stdout_str(text: &str) {
    if STDOUT_STOPPED.load(Ordering::Relaxed) {
        return;
    }
    let mut handle = std::io::stdout().lock();
    record(handle.write_all(text.as_bytes()));
}

/// Write formatted output to stdout with no trailing newline — the `out!` macro.
pub fn stdout_fmt(args: std::fmt::Arguments<'_>) {
    if STDOUT_STOPPED.load(Ordering::Relaxed) {
        return;
    }
    let mut handle = std::io::stdout().lock();
    record(handle.write_fmt(args));
}

/// Write one formatted line to stdout — the `outln!` macro. The newline is
/// written under the same lock as the body, so a line is never split.
pub fn stdout_line(args: std::fmt::Arguments<'_>) {
    if STDOUT_STOPPED.load(Ordering::Relaxed) {
        return;
    }
    let mut handle = std::io::stdout().lock();
    record(handle.write_fmt(args).and_then(|()| handle.write_all(b"\n")));
}

/// Whether stdout is an interactive terminal — `license` reads this to decide
/// whether to page (see `main::should_page`). Lives in the seam, rather than
/// as a bare `std::io::stdout().is_terminal()` at the call site, so the
/// `nothing_writes_around_the_output_seam` guard test (which forbids raw
/// `io::stdout()` elsewhere, on the theory that any of it might panic-write on
/// a closed pipe) does not have to special-case a read that never writes.
pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// Write one formatted line to stderr — the `errln!` macro. Errors are dropped
/// on the floor: a broken stderr has nowhere to be reported and says nothing
/// about whether the analysis succeeded.
pub fn stderr_line(args: std::fmt::Arguments<'_>) {
    let mut handle = std::io::stderr().lock();
    let _ = handle.write_fmt(args).and_then(|()| handle.write_all(b"\n"));
}

/// Record a stdout write result: stop writing on any error, and remember whether
/// it was a closed reader (exit-neutral) or a real failure (exit 1).
fn record(result: std::io::Result<()>) {
    let Err(e) = result else { return };
    STDOUT_STOPPED.store(true, Ordering::Relaxed);
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        return;
    }
    STDOUT_FAILED.store(true, Ordering::Relaxed);
    // Reported once — `STDOUT_STOPPED` keeps every later write from repeating it.
    errln!("steins: cannot write to stdout: {e}");
}

/// Flush stdout and settle the exit code — the last thing `main` does.
///
/// `code` is the command's own verdict (ADR-0050 §7) and survives a closed
/// reader untouched. Only a hard write failure overrides it, because in that case
/// the report the exit code describes was never delivered *and* the delivery
/// itself failed.
pub fn finish(code: ExitCode) -> ExitCode {
    if !STDOUT_STOPPED.load(Ordering::Relaxed) {
        let mut handle = std::io::stdout().lock();
        record(handle.flush());
    }
    if STDOUT_FAILED.load(Ordering::Relaxed) { ExitCode::FAILURE } else { code }
}

#[cfg(test)]
mod tests {
    //! The seam's own behaviour is observed end-to-end (a real closed pipe, a
    //! real process exit) in `tests/output_seam.rs`; a unit test cannot close the
    //! read end of its own stdout without breaking the test harness's. What is
    //! testable here is that the macros compile and route through the module.

    #[test]
    fn the_macros_write_through_the_seam() {
        outln!();
        outln!("seam {}", 1);
        out!("");
        errln!("seam stderr");
    }
}
