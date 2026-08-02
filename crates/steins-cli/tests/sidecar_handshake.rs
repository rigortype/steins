//! End-to-end test for issue #110: the sidecar spawns but a request goes
//! unanswered — at the opening handshake, or mid-run.
//!
//! Three sidecar outcomes exist (ADR-0004/0024). `--no-php` and "no `php` on
//! PATH" both already print a stderr notice and keep exit-neutral (ADR-0004's
//! "incompleteness is never silent"). The third — `php` resolves and starts,
//! but a request never gets a reply (a wrapper that never execs real PHP, a
//! `php.ini` that hangs on startup, an `auto_prepend_file` that never
//! returns, or a child that answers fine at first and then hangs or dies
//! partway through) — used to degrade in total silence: every fold widened,
//! the absence-proof family went quiet, and the run looked exactly like a
//! healthy one. `steins doctor` already named the opening-handshake case
//! ("PHP sidecar: spawned, but the env() query failed"); `check` now names
//! both.
//!
//! Two stub `php` scripts below, each put on a PRIVATE `PATH` passed only to
//! the child `steins` process (`Command::env`, never `std::env::set_var` on
//! this test process itself, which would race every other test in the
//! binary). [`stub_php_dir`] never speaks the wire format at all; the harder
//! case, [`stub_php_dir_mid_run`], answers a real `env()` request correctly —
//! so the opening handshake succeeds — and only goes silent on whatever comes
//! after (a `fold`), which is the shape a first cut of this fix missed
//! (review finding on PR #134): it latched on "any request has ever
//! succeeded" and never reported again for the rest of the run, even though
//! `Sidecar`'s own contract is that a lost reply is never retried — a mid-run
//! timeout loses that specific finding exactly as silently as an opening one.
//! Both stubs need no real `php` on the host, unlike the sidecar tests in
//! `crates/steins-sidecar/tests/protocol.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Write `script` as an executable `php` in a fresh, uniquely-named directory,
/// and return the directory (suitable as a private `PATH`). Shared by both
/// stub variants below so only the script body differs.
fn write_stub_php(tag: &str, script: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("steins-handshake-stub-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create stub PATH dir");
    let path = dir.join("php");
    std::fs::write(&path, script).expect("write stub php");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).expect("stat stub php").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod stub php");
    dir
}

/// A private `PATH` directory containing only a `php` that spawns fine and
/// then never answers anything: reading one line at a time forever, off
/// nothing but POSIX shell builtins (`read`, `:`) so it needs no `PATH` of its
/// own to resolve an external `sleep`/`cat` — the earlier draft of this stub
/// used `sleep` and failed instantly with "command not found" once `PATH` was
/// pinned to just this directory, which produced a false pass (the notice
/// fired, but for the wrong reason). `read` blocks exactly like a hung
/// `php.ini`/`auto_prepend_file` would: the process is alive and silent, the
/// two things the ADR-0024 timeout is built to catch. Models a dead-from-the-
/// start opening handshake.
fn stub_php_dir() -> PathBuf {
    write_stub_php(
        "opening",
        "#!/bin/sh\n\
         # issue #110 repro: spawns, never speaks the ADR-0024 JSON-RPC framing.\n\
         while :; do\n\
         \tread -r _line || exit 0\n\
         done\n",
    )
}

/// A private `PATH` directory containing a `php` that answers every `env()`
/// request FOR REAL — extracting the request's own `id` and replying with a
/// well-formed `EnvInfo` result, so the opening handshake genuinely succeeds —
/// and silently drops any other method (`fold`, `reflect`): no reply, ever,
/// exactly like [`stub_php_dir`]'s hang, just conditional on which request it
/// is. Models the harder case: a wrapper (or a `php.ini`/`auto_prepend_file`)
/// that speaks just enough of the framing to pass the handshake and then goes
/// quiet on the request that actually drives analysis — the case a first cut
/// of the issue #110 fix missed (review finding on PR #134) by latching on
/// "one success ever" instead of "one failure this run".
fn stub_php_dir_mid_run() -> PathBuf {
    write_stub_php(
        "midrun",
        "#!/bin/sh\n\
         # issue #110 repro (PR #134 review): answers env() for real, then goes\n\
         # silent on the first request that isn't env() — a mid-run hang, not an\n\
         # opening one.\n\
         while :; do\n\
         \tread -r _line || exit 0\n\
         \tcase \"$_line\" in\n\
         \t*'\"method\":\"env\"'*)\n\
         \t\tid=$(printf '%s' \"$_line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p')\n\
         \t\tprintf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"php_version\":\"8.5.0\",\"extensions\":[],\"sapi\":\"cli\",\"int_size\":8}}\\n' \"$id\"\n\
         \t\t;;\n\
         \t*) : ;;\n\
         \tesac\n\
         done\n",
    )
}

/// Run `steins` with `args`, `PATH` narrowed to `stub_dir` so the sidecar
/// spawns the stub instead of whatever real `php` the host may or may not
/// have. The per-request ADR-0024 timeouts a `check` run on a file with one
/// foldable argument pays (the `env()` handshake and/or one fold attempt,
/// each with its own respawn) put this comfortably under ten seconds; a
/// 30-second bound below only guards against an actual hang regression.
fn run_against_stub(stub_dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).env("PATH", stub_dir).output().expect("run steins");
    let _ = std::fs::remove_dir_all(stub_dir);
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// [`run_against_stub`] against the opening-handshake stub — the shape the
/// pre-existing tests below exercise.
fn run_against_hung_sidecar(args: &[&str]) -> Run {
    run_against_stub(&stub_php_dir(), args)
}

#[test]
fn check_surfaces_the_handshake_notice_when_php_never_answers() {
    let path = fixture("fold_mixed.php");
    let start = std::time::Instant::now();
    let r = run_against_hung_sidecar(&["check", path.to_str().unwrap()]);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(30),
        "a hung sidecar must still bound the run — this is not a real hang, got {:?}",
        start.elapsed()
    );

    // Exactly the sound-subset shape `--no-php` produces on the same fixture
    // (see no_php_omits_folded_but_keeps_direct_and_notes_posture in cli.rs):
    // the direct literal finding fires, the finding that needs a live fold is
    // silently omitted (a legitimate widen, not a bug), and the run stays at
    // exit 1 on the finding that IS provable without PHP.
    assert_eq!(r.code, 1, "the direct (unfolded) finding still fires, got:\n{}", r.stdout);
    assert_eq!(
        r.stdout.lines().count(),
        1,
        "the folded finding is silently omitted (sound, not a false negative), got:\n{}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("folded from"),
        "no folded finding should reach stdout, got:\n{}",
        r.stdout
    );

    // The notice issue #110 is about. Printed exactly once even though the run
    // makes more than one sidecar request (the env() handshake, then the fold
    // itself) — the latch, not the request count, decides how many lines print.
    let hits = r.stderr.matches("sound subset (degraded)").count();
    assert_eq!(
        hits, 1,
        "the handshake notice must fire exactly once regardless of how many requests failed, got stderr:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("steins doctor"),
        "the notice should point the reader at doctor for detail, got:\n{}",
        r.stderr
    );
    // Distinct from the --no-php / spawn-failure wording: `php` genuinely
    // started here, so "no PHP sidecar" would misdiagnose the cause.
    assert!(
        !r.stderr.contains("no PHP sidecar"),
        "a spawned-but-silent php must not be reported as absent, got:\n{}",
        r.stderr
    );
}

#[test]
fn annotate_surfaces_the_same_handshake_notice() {
    // annotate shares SidecarFolder::enabled() with check (main.rs), so the
    // same ProcessEngine latch should fire here too.
    let path = fixture("fold_mixed.php");
    let r = run_against_hung_sidecar(&["annotate", path.to_str().unwrap()]);
    let hits = r.stderr.matches("sound subset (degraded)").count();
    assert_eq!(hits, 1, "annotate must surface the same notice once, got stderr:\n{}", r.stderr);
}

#[test]
fn check_surfaces_the_notice_when_the_sidecar_stops_answering_mid_run() {
    // Review finding on issue #110 (PR #134): a first cut of this fix latched
    // permanently on "any request has ever succeeded" and never printed again
    // for the rest of the run — so a `php` that answers the opening `env()`
    // handshake for real and only THEN stops (the `stub_php_dir_mid_run`
    // shape) produced zero notice lines, contradicting the issue's own
    // acceptance criterion ("the handshake fails OR times out mid-run").
    // `Sidecar`'s own contract is that a lost reply is never retried, so this
    // must be exactly as loud as the opening-handshake case, and the latch
    // must arm on the first poisoning event regardless of what happened
    // before it.
    let path = fixture("fold_mixed.php");
    let start = std::time::Instant::now();
    let r = run_against_stub(&stub_php_dir_mid_run(), &["check", path.to_str().unwrap()]);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(30),
        "a hung sidecar must still bound the run — this is not a real hang, got {:?}",
        start.elapsed()
    );

    // Same sound-subset shape as the opening-handshake case: the direct
    // finding fires, the finding needing a live fold is silently omitted.
    assert_eq!(r.code, 1, "the direct (unfolded) finding still fires, got:\n{}", r.stdout);
    assert_eq!(
        r.stdout.lines().count(),
        1,
        "the folded finding is silently omitted (sound, not a false negative), got:\n{}",
        r.stdout
    );

    let hits = r.stderr.matches("sound subset (degraded)").count();
    assert_eq!(
        hits, 1,
        "a mid-run failure after a genuinely successful handshake must still notice, got stderr:\n{}",
        r.stderr
    );
    assert!(r.stderr.contains("steins doctor"), "got:\n{}", r.stderr);
    assert!(
        !r.stderr.contains("no PHP sidecar"),
        "a spawned-and-partly-responsive php must not be reported as absent, got:\n{}",
        r.stderr
    );
}
