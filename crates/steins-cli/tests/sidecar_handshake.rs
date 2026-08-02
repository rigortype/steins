//! End-to-end test for issue #110: the sidecar spawns but never answers.
//!
//! Three sidecar outcomes exist (ADR-0004/0024). `--no-php` and "no `php` on
//! PATH" both already print a stderr notice and keep exit-neutral (ADR-0004's
//! "incompleteness is never silent"). The third — `php` resolves and starts,
//! but never speaks the JSON-RPC framing (a wrapper that never execs real PHP,
//! a `php.ini` that hangs on startup, an `auto_prepend_file` that never
//! returns) — used to degrade in total silence: every fold widened, the
//! absence-proof family went quiet, and the run looked exactly like a healthy
//! one. `steins doctor` already named this case ("PHP sidecar: spawned, but
//! the env() query failed"); `check` now does too.
//!
//! The stub `php` below is put on a PRIVATE `PATH` passed only to the child
//! `steins` process (`Command::env`, never `std::env::set_var` on this test
//! process itself, which would race every other test in the binary). It never
//! speaks the wire format at all — it just blocks reading lines forever — so
//! the failure is deterministic and needs no real `php` on the host, unlike
//! the sidecar tests in `crates/steins-sidecar/tests/protocol.rs`.

use std::path::PathBuf;
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

/// A private `PATH` directory containing only a `php` that spawns fine and
/// then never answers: reading one line at a time forever, off nothing but
/// POSIX shell builtins (`read`, `:`) so it needs no `PATH` of its own to
/// resolve an external `sleep`/`cat` — the earlier draft of this stub used
/// `sleep` and failed instantly with "command not found" once `PATH` was
/// pinned to just this directory, which produced a false pass (the notice
/// fired, but for the wrong reason). `read` blocks exactly like a hung
/// `php.ini`/`auto_prepend_file` would: the process is alive and silent, the
/// two things the ADR-0024 timeout is built to catch.
fn stub_php_dir() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("steins-handshake-stub-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create stub PATH dir");
    let script = dir.join("php");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         # issue #110 repro: spawns, never speaks the ADR-0024 JSON-RPC framing.\n\
         while :; do\n\
         \tread -r _line || exit 0\n\
         done\n",
    )
    .expect("write stub php");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&script).expect("stat stub php").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod stub php");
    dir
}

/// Run `steins` with `args`, `PATH` narrowed to [`stub_php_dir`] so the
/// sidecar spawns the hung stub instead of whatever real `php` the host may or
/// may not have. The two per-request ADR-0024 timeouts a `check` run on a file
/// with one foldable argument pays (the `env()` handshake, then one fold
/// attempt with its one respawn) put this comfortably under ten seconds; a
/// 30-second bound below only guards against an actual hang regression.
fn run_against_hung_sidecar(args: &[&str]) -> Run {
    let stub_dir = stub_php_dir();
    let out = Command::new(bin()).args(args).env("PATH", &stub_dir).output().expect("run steins");
    let _ = std::fs::remove_dir_all(&stub_dir);
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
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
