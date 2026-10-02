//! Per-file fault isolation (issue #895, owner decision D3): a panic in one
//! file's walk becomes that file's `internal.panic` finding, and the run goes on
//! with every other file.
//!
//! **What is isolated** is the per-file walk — the scope walk and the file's own
//! passes in [`crate::file_walk`] — wherever it runs: the cold pipeline, the
//! generation orchestrator's fan-out, and the single-file entry points, because
//! all of them walk through `WalkInputs::walk`. **What is not**: the parse and
//! the index (salsa queries on the cold path, the load on the generation path),
//! the whole-universe facts, the two fixpoints and the reporting passes over
//! them. None of those is one file's work, so a panic there has no file to name
//! and still ends the run as it always did.
//!
//! **Opt-in per process.** Isolation is off until [`isolate_file_panics`] turns
//! it on, which the `steins` binary does first thing. A library caller — every
//! in-process test among them — keeps the old behavior, where a panic unwinds
//! to the caller: a test that asserts the *absence* of a finding must not pass
//! because the walk that would have produced it panicked and was turned into a
//! finding the test does not look at.
//!
//! **The hook.** Turning isolation on installs a panic hook that, while a walk
//! is guarded on this thread, records the message and its source location for
//! the finding instead of printing them in the middle of the run. With
//! `RUST_BACKTRACE` set it prints the standard report as well, backtrace
//! included, so a developer chasing the panic loses nothing. Outside a guarded
//! walk it defers to whatever hook was installed before.
//!
//! **Salsa cancellation is not a fault.** Salsa cancels a query by unwinding
//! with a [`salsa::Cancelled`] payload; that is control flow, and is re-raised
//! rather than reported.
//!
//! **The test hook.** A debug build panics on purpose in the walk of any file
//! whose diagnostic path ends with the value of [`TEST_PANIC_ENV`]. Release
//! builds never read it. It is how the end-to-end tests provoke a panic
//! without a real analyzer bug to lean on.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::INTERNAL_PANIC_ID;
use crate::project::Diagnostic;

/// The debug-build environment variable naming the file a walk panics on: any
/// file whose diagnostic path ends with its value. Read in one place, the walk's
/// test hook, and never by a release build.
pub const TEST_PANIC_ENV: &str = "STEINS_TEST_PANIC_ON";

/// How much of a panic message the finding carries. An `assert_eq!` on a large
/// value can render pages of `Debug` output; the first part says what broke,
/// and the full text is one `RUST_BACKTRACE=1` away.
const MESSAGE_LIMIT: usize = 240;

/// Whether this process isolates file panics ([`isolate_file_panics`]).
static ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Whether this thread is inside a guarded walk right now.
    static GUARDED: Cell<bool> = const { Cell::new(false) };
    /// What the hook recorded about the panic being unwound, if anything.
    static CAUGHT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Turn per-file panic isolation on for this process and install the hook that
/// captures a guarded panic's message. Idempotent; the `steins` binary calls it
/// before it dispatches a command.
pub fn isolate_file_panics() {
    install_hook();
    ENABLED.store(true, Ordering::Relaxed);
}

/// Run one file's walk `f`, guarded when this process isolates file panics.
/// `Err` carries the panic's description; without isolation a panic unwinds to
/// the caller exactly as it always did.
pub(crate) fn guard<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    if ENABLED.load(Ordering::Relaxed) { catch(f) } else { Ok(f()) }
}

/// The isolation itself: run `f`, and turn a panic into its description.
/// Re-raises a [`salsa::Cancelled`] unwind untouched.
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    install_hook();
    // A message some other catch left behind must not be mistaken for ours.
    CAUGHT.take();
    let outer = GUARDED.replace(true);
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    GUARDED.set(outer);
    match result {
        Ok(value) => Ok(value),
        Err(payload) => {
            if payload.is::<salsa::Cancelled>() {
                panic::resume_unwind(payload);
            }
            let recorded = CAUGHT.take();
            // `resume_unwind` runs no hook, so a payload may arrive unrecorded.
            Err(recorded.unwrap_or_else(|| payload_text(payload.as_ref()).to_owned()))
        }
    }
}

/// Install the capturing hook over whatever hook is current, once per process.
fn install_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let guarded = GUARDED.try_with(Cell::get).unwrap_or(false);
            if !guarded {
                previous(info);
                return;
            }
            let described = describe(info);
            let _ = CAUGHT.try_with(|c| *c.borrow_mut() = Some(described));
            if backtrace_requested() {
                previous(info);
            }
        }));
    });
}

/// Whether the developer asked for backtraces, by the standard library's own
/// switch: set, and not `0`.
fn backtrace_requested() -> bool {
    std::env::var_os("RUST_BACKTRACE").is_some_and(|v| v != "0")
}

/// The panic's message and where in the analyzer it was raised.
fn describe(info: &PanicHookInfo<'_>) -> String {
    let message = info.payload_as_str().unwrap_or(NON_STRING_PAYLOAD);
    match info.location() {
        Some(at) => format!("{message} (at {}:{})", at.file(), at.line()),
        None => message.to_owned(),
    }
}

const NON_STRING_PAYLOAD: &str = "a panic with a non-string payload";

/// The text of a panic payload that reached us without passing the hook.
fn payload_text(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or(NON_STRING_PAYLOAD)
}

/// The finding a panicked walk leaves in place of the file's own: positioned at
/// the top of the file, since the panic names no line of it.
pub(crate) fn internal_panic(path: &str, described: &str) -> Diagnostic {
    Diagnostic {
        id: INTERNAL_PANIC_ID,
        path: path.to_owned(),
        line: 1,
        column: 1,
        message: format!(
            "the analyzer panicked on this file: {}; its findings are missing from this run — \
             this is a bug in Steins, please report it",
            one_line(described)
        ),
        facet: None,
        fix: None,
    }
}

/// `text` on one line — every renderer prints a finding as one — and at most
/// [`MESSAGE_LIMIT`] characters.
fn one_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match joined.char_indices().nth(MESSAGE_LIMIT) {
        Some((cut, _)) => format!("{}…", &joined[..cut]),
        None => joined,
    }
}

/// Panic on purpose when [`TEST_PANIC_ENV`] names `path` (debug builds only).
#[cfg(debug_assertions)]
pub(crate) fn provoke_for_test(path: &str) {
    if let Some(suffix) = std::env::var_os(TEST_PANIC_ENV)
        && !suffix.is_empty()
        && path.ends_with(&*suffix.to_string_lossy())
    {
        panic!("{TEST_PANIC_ENV} names this file");
    }
}

/// Release builds never provoke anything.
#[cfg(not(debug_assertions))]
pub(crate) fn provoke_for_test(_path: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_walk_that_returns_is_untouched() {
        assert_eq!(catch(|| 7), Ok(7));
    }

    /// The description carries the message and where the analyzer raised it,
    /// and the guard is down again afterwards.
    #[test]
    fn a_panic_becomes_its_description() {
        let caught = catch(|| -> u8 { panic!("the walk broke on {}", "purpose") });
        let described = caught.expect_err("the panic is caught");
        assert!(described.starts_with("the walk broke on purpose (at "), "{described}");
        assert!(described.contains("panic_guard.rs:"), "{described}");
        assert!(!GUARDED.get(), "the guard is lowered after the catch");
    }

    /// A payload that never passed the hook still has its text read.
    #[test]
    fn an_unhooked_payload_is_read_directly() {
        let caught = catch(|| -> u8 { panic::resume_unwind(Box::new("raw payload")) });
        assert_eq!(caught, Err("raw payload".to_owned()));
    }

    /// Cancellation is salsa's control flow, not a fault: it unwinds on through.
    #[test]
    fn salsa_cancellation_is_reraised() {
        let outer = panic::catch_unwind(|| {
            catch(|| -> u8 { panic::resume_unwind(Box::new(salsa::Cancelled::PendingWrite)) })
        });
        let payload = outer.expect_err("the cancellation is not swallowed");
        assert!(payload.is::<salsa::Cancelled>());
    }

    #[test]
    fn the_message_is_one_bounded_line() {
        assert_eq!(one_line("assertion failed\n  left: 1\n right: 2"), "assertion failed left: 1 right: 2");
        let long = "x".repeat(MESSAGE_LIMIT + 10);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), MESSAGE_LIMIT + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_finding_names_the_file_at_its_top() {
        let d = internal_panic("src/Broken.php", "boom (at src/x.rs:1)");
        assert_eq!((d.id, d.path.as_str(), d.line, d.column), (INTERNAL_PANIC_ID, "src/Broken.php", 1, 1));
        assert!(d.message.contains("boom (at src/x.rs:1)"), "{}", d.message);
    }
}
