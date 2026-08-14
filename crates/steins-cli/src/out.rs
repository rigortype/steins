//! The output seam: every user-facing byte this binary writes goes through here
//! (issue #44).
//!
//! `println!` panics on `EPIPE` (`| head`, `| grep -m1`, an early-quit `less`),
//! which crashed ordinary use of `license` (2000+ lines) and `check` (one line
//! per finding). Rather than fix each call site, `outln!` / `out!` / `errln!` are
//! the only way this crate writes; `crates/steins-cli/tests/output_seam.rs` fails
//! the build if a raw `println!` reappears in the CLI's source.
//!
//! # The policy
//!
//! * **stdout, closed reader** — stop writing, leave the exit code alone. A
//!   reader quitting early is not a failure: `check`'s exit 1 (ADR-0050 §7)
//!   reflects findings, not whether the pager was still listening.
//! * **stdout, any other error** — a genuine write failure (full disk, I/O
//!   error) is reported once on stderr and exits 1 via [`finish`].
//! * **stderr, any error** — ignored; there is nowhere to report a broken
//!   stderr, and it must not affect the exit code (deliberate asymmetry).
//!
//! # Buffering
//!
//! Writes go straight to `std::io::stdout()` (line-buffered), matching
//! `println!`'s syscalls. A `BufWriter` would reorder stdout against stderr,
//! and interleaving correctness matters more than the syscall count.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

// Macros come first: `macro_rules!` is textually scoped, and `record` below uses `errln!`.

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

/// Set once stdout has refused a write (closed reader or hard error) — further
/// writes are skipped.
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
/// whether to page (see `main::should_page`). Lives here (not a bare call at the
/// site) so the `nothing_writes_around_the_output_seam` guard test doesn't need
/// to special-case a read that never writes.
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
    // Reported once: STDOUT_STOPPED keeps later writes from repeating it.
    errln!("steins: cannot write to stdout: {e}");
}

/// Flush stdout and settle the exit code — the last thing `main` does.
///
/// `code` is the command's own verdict (ADR-0050 §7) and survives a closed
/// reader untouched; only a hard write failure overrides it to `FAILURE`.
pub fn finish(code: ExitCode) -> ExitCode {
    if !STDOUT_STOPPED.load(Ordering::Relaxed) {
        let mut handle = std::io::stdout().lock();
        record(handle.flush());
    }
    if STDOUT_FAILED.load(Ordering::Relaxed) { ExitCode::FAILURE } else { code }
}

#[cfg(test)]
mod tests {
    //! End-to-end behavior (a real closed pipe, a real process exit) is covered
    //! by `tests/output_seam.rs`; this just checks the macros compile and route.

    #[test]
    fn the_macros_write_through_the_seam() {
        outln!();
        outln!("seam {}", 1);
        out!("");
        errln!("seam stderr");
    }
}
