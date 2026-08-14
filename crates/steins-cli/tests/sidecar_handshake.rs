//! End-to-end test for issue #110: the sidecar spawns but a request goes
//! unanswered — at the opening handshake, or mid-run.
//!
//! Three sidecar outcomes exist (ADR-0004/0024): `--no-php` and "no `php` on
//! PATH" print a stderr notice and stay exit-neutral ("incompleteness is
//! never silent"); the third — `php` starts but never replies (dead wrapper,
//! hanging `php.ini`/`auto_prepend_file`, or answers then hangs/dies) — must
//! not degrade in silence: `check` names both the handshake and mid-run cases.
//!
//! Two stub `php` scripts below run on a PRIVATE `PATH` given only to the
//! child `steins` process (`Command::env`, never `std::env::set_var`, which
//! would race other tests in the binary). [`stub_php_dir`] never speaks the
//! wire format; [`stub_php_dir_mid_run`] passes the handshake and only then
//! goes silent (see each stub's doc, PR #134 review). Neither needs a real
//! `php`, unlike `crates/steins-sidecar/tests/protocol.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test spawns the binary with `GITHUB_ACTIONS` scrubbed: `check`'s format
/// auto-detection (ADR-0054 §6) reads it, so a run on CI would otherwise emit workflow
/// commands instead of the asserted text (detection is tested in `tests/format_github.rs`).
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
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

/// A private `PATH` directory with only a `php` that spawns fine and then never
/// answers: it reads one line at a time forever using POSIX builtins (`read`,
/// `:`) alone, needing no external `PATH` entries (unlike `sleep`/`cat`, which
/// fail once `PATH` is pinned here). `read` blocks exactly like a hung
/// `php.ini`/`auto_prepend_file` — alive and silent, what ADR-0024's timeout
/// exists to catch. Models a dead-from-the-start opening handshake.
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

/// A private `PATH` directory with a `php` that answers every `env()` request
/// FOR REAL (extracting the request's `id`, replying with a well-formed
/// `EnvInfo` — the handshake genuinely succeeds) and silently drops every
/// other method (`fold`, `reflect`), forever. Models a wrapper that passes the
/// handshake and then goes quiet on the request that drives analysis — missed
/// by a first cut of the issue #110 fix (PR #134 review) that latched on "one
/// success ever" instead of "one failure this run".
///
/// The `id` extraction is POSIX parameter expansion, not `sed`: `PATH` is
/// narrowed to this directory ALONE, so an external command would silently
/// fail (`Sidecar` discards the child's stderr) into a malformed reply that
/// poisons the first request — a false pass for the wrong reason (PR #134
/// review, round 2).
fn stub_php_dir_mid_run() -> PathBuf {
    write_stub_php(
        "midrun",
        "#!/bin/sh\n\
         # issue #110 repro (PR #134 review): answers env() for real, then goes\n\
         # silent on the first request that isn't env() — a mid-run hang, not an\n\
         # opening one. No external commands (PATH is narrowed to this directory\n\
         # alone) — id extraction is POSIX parameter expansion only.\n\
         while :; do\n\
         \tread -r _line || exit 0\n\
         \tcase \"$_line\" in\n\
         \t*'\"method\":\"env\"'*)\n\
         \t\tid=${_line#*\\\"id\\\":}\n\
         \t\tid=${id%%,*}\n\
         \t\tprintf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"php_version\":\"8.5.0\",\"extensions\":[],\"sapi\":\"cli\",\"int_size\":8}}\\n' \"$id\"\n\
         \t\t;;\n\
         \t*) : ;;\n\
         \tesac\n\
         done\n",
    )
}

/// Run `steins` with `args`, `PATH` narrowed to `stub_dir` so the sidecar spawns
/// the stub instead of whatever real `php` the host has. The per-request
/// ADR-0024 timeouts a `check` run on one foldable argument pays (`env()` plus
/// one fold attempt, each with its own respawn) stay comfortably under ten
/// seconds; the 30-second bound below only guards against an actual hang.
fn run_against_stub(stub_dir: &Path, args: &[&str]) -> Run {
    let out = steins_cmd().args(args).env("PATH", stub_dir).output().expect("run steins");
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

    // The sound-subset shape as `--no-php` on the same fixture (cli.rs's
    // no_php_omits_folded_but_keeps_direct_and_notes_posture): the folded finding
    // is silently omitted (legitimate widen), the direct one still exits 1.
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

    // The issue #110 notice: printed exactly once even though the run makes
    // more than one sidecar request — the latch, not the request count, decides.
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
    // PR #134 review finding: a fix latching on "any success ever" must still
    // notice a mid-run failure after a genuinely successful handshake, exactly
    // as loudly as the opening-handshake case (see stub_php_dir_mid_run's docs).
    let path = fixture("fold_mixed.php");
    let start = std::time::Instant::now();
    let r = run_against_stub(&stub_php_dir_mid_run(), &["check", path.to_str().unwrap()]);
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "a hung sidecar must still bound the run — this is not a real hang, got {elapsed:?}"
    );
    // Regression guard (PR #134 review, round 2): a broken stub would corrupt
    // env() and pass near-instantly for the wrong reason — see stub_php_dir_mid_run.
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "a near-instant run means env() itself failed (e.g. the stub's id \
         extraction silently broke) rather than a genuine mid-run timeout \
         after a real handshake, got {elapsed:?}"
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
