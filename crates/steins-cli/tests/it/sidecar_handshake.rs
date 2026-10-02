//! End-to-end test for issue #110: the sidecar spawns but a request goes
//! unanswered — at the opening handshake, or mid-run.
//!
//! Three sidecar outcomes exist (ADR-0004/0024): `--no-php` and "no `php` on
//! PATH" print a stderr notice and stay exit-neutral; the third — `php` starts
//! but never replies (dead wrapper, hanging `php.ini`/`auto_prepend_file`, or
//! answers then hangs/dies) — must not degrade in silence: `check` names both
//! the handshake and mid-run cases.
//!
//! Two stub `php` scripts run on a PRIVATE `PATH` given only to the child
//! `steins` process (`Command::env`, never `std::env::set_var`, which would
//! race other tests in the binary). [`stub_php_dir`] never speaks the wire
//! format; [`stub_php_dir_mid_run`] passes the handshake and only then goes
//! silent (PR #134 review). Neither needs a real `php`, unlike
//! `crates/steins-sidecar/tests/it/protocol.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test scrubs `GITHUB_ACTIONS`: `check`'s format auto-detection (ADR-0054
/// §6) reads it, so CI would otherwise emit workflow commands instead of text.
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

/// Writes `script` as an executable `php` in a fresh directory and returns it
/// (a private `PATH`); shared by both stub variants so only the body differs.
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

/// A `php` that spawns fine and never answers: reads one line at a time
/// forever with POSIX builtins alone (`read`, `:`), needing no external `PATH`
/// entries (`sleep`/`cat` would fail once `PATH` is pinned here). Blocks like a
/// hung `php.ini`/`auto_prepend_file` — what ADR-0024's timeout catches.
/// Models a dead-from-the-start opening handshake.
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

/// A `php` that answers every `env()` request for real (well-formed `EnvInfo`
/// reply — the handshake genuinely succeeds) and silently drops every other
/// method (`fold`, `reflect`), forever. Models a wrapper that passes the
/// handshake then goes quiet on the request that drives analysis — missed by a
/// first cut of the issue #110 fix, which latched on "one success ever"
/// instead of "one failure this run" (PR #134 review).
///
/// `id` extraction is POSIX parameter expansion, not `sed`: `PATH` is narrowed
/// to this directory alone, so an external command would silently fail
/// (`Sidecar` discards the child's stderr) into a malformed reply that poisons
/// the first request — a false pass for the wrong reason (PR #134 review round 2).
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

/// Runs `steins` with `PATH` narrowed to `stub_dir` so the sidecar spawns the
/// stub, not the host's real `php`. The opening stub never answers its boot
/// handshake, which is charged `STEINS_SIDECAR_BOOT_TIMEOUT_MS` (issue #891,
/// 20 s by default); `boot_ms` sets it short for that stub so the test does not
/// wait that out. The mid-run stub passes its handshake at once and keeps the
/// default, so a starved boot could never make its timeout test pass for the
/// wrong reason.
/// Per-request ADR-0024 timeouts for one foldable argument stay under ten
/// seconds; the 30-second bound below only guards an actual hang.
///
/// A `check` runs `--no-cache`: these fixtures are checked in, and a cached run
/// would leave a `.steins/` in the repository for the next run of the suite to
/// start warm from (issue #525 — see `tests/it/cli.rs`'s `uncached`, which states
/// the rule in full). Nothing here is about the cache; it is about what a
/// silent sidecar does to a run.
fn run_against_stub(stub_dir: &Path, args: &[&str], boot_ms: Option<&str>) -> Run {
    let mut args: Vec<&str> = args.to_vec();
    if args.first() == Some(&"check") {
        args.insert(1, "--no-cache");
    }
    let mut cmd = steins_cmd();
    cmd.args(&args).env("PATH", stub_dir);
    if let Some(ms) = boot_ms {
        cmd.env("STEINS_SIDECAR_BOOT_TIMEOUT_MS", ms);
    }
    let out = cmd.output().expect("run steins");
    let _ = std::fs::remove_dir_all(stub_dir);
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// [`run_against_stub`] against the opening-handshake stub.
fn run_against_hung_sidecar(args: &[&str]) -> Run {
    run_against_stub(&stub_php_dir(), args, Some("1500"))
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

    // Same sound-subset shape as `--no-php` on this fixture (cli.rs's
    // no_php_omits_folded_but_keeps_direct_and_notes_posture): folded omitted, direct fires.
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

    // Issue #110 notice: prints once even though the run makes >1 sidecar request.
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
    // Distinct from --no-php / spawn-failure wording: `php` genuinely started here
    // (a failed boot, issue #891, is the degraded report and not the absent one).
    assert!(
        !r.stderr.contains("no PHP sidecar"),
        "a spawned-but-silent php must not be reported as absent, got:\n{}",
        r.stderr
    );
}

#[test]
fn annotate_surfaces_the_same_handshake_notice() {
    // annotate shares SidecarFolder::enabled() with check (main.rs) — same latch.
    let path = fixture("fold_mixed.php");
    let r = run_against_hung_sidecar(&["annotate", path.to_str().unwrap()]);
    let hits = r.stderr.matches("sound subset (degraded)").count();
    assert_eq!(hits, 1, "annotate must surface the same notice once, got stderr:\n{}", r.stderr);
}

#[test]
fn check_surfaces_the_notice_when_the_sidecar_stops_answering_mid_run() {
    // PR #134 review: a fix latching on "any success ever" must still notice a
    // mid-run failure after a genuinely successful handshake (stub_php_dir_mid_run).
    let path = fixture("fold_mixed.php");
    let start = std::time::Instant::now();
    let r = run_against_stub(&stub_php_dir_mid_run(), &["check", path.to_str().unwrap()], None);
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "a hung sidecar must still bound the run — this is not a real hang, got {elapsed:?}"
    );
    // Regression guard (PR #134 review round 2): a broken stub corrupts env()
    // and passes near-instantly for the wrong reason (stub_php_dir_mid_run).
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "a near-instant run means env() itself failed (e.g. the stub's id \
         extraction silently broke) rather than a genuine mid-run timeout \
         after a real handshake, got {elapsed:?}"
    );

    // Same sound-subset shape as the opening-handshake case: direct fires, fold omitted.
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

/// A cached `check` of a private copy of `fold_mixed.php`, `PATH` narrowed to
/// `path_dir`: returns the run and whether it published a generation.
///
/// The cache is on here, unlike every other run in this file, because what is
/// under test is the publish gate (ADR-0092, #784): a run that lost a sidecar
/// answer publishes nothing, a run that never had a sidecar still does.
fn cached_run_against(tag: &str, path_dir: &Path) -> (Run, bool) {
    let tree =
        std::env::temp_dir().join(format!("steins-handshake-tree-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tree);
    std::fs::create_dir_all(&tree).expect("create the tree");
    std::fs::copy(fixture("fold_mixed.php"), tree.join("fold_mixed.php")).expect("copy fixture");
    let out = steins_cmd()
        .current_dir(&tree)
        .args(["check", "."])
        .env("PATH", path_dir)
        .env("STEINS_SIDECAR_BOOT_TIMEOUT_MS", "1500")
        .output()
        .expect("run steins");
    let published = tree.join(".steins/gen/CURRENT").is_file();
    let _ = std::fs::remove_dir_all(&tree);
    let _ = std::fs::remove_dir_all(path_dir);
    let run = Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    (run, published)
}

/// A `php` that started and never booted is a lost answer: the degraded notice,
/// and no generation for a later run to replay (issue #891).
#[test]
fn a_php_that_never_boots_publishes_nothing() {
    let (r, published) = cached_run_against("hung", &stub_php_dir());
    assert!(r.stderr.contains("sound subset (degraded)"), "got stderr:\n{}", r.stderr);
    assert!(!r.stderr.contains("no PHP sidecar"), "got stderr:\n{}", r.stderr);
    assert!(!published, "a run that lost the engine published a generation:\n{}", r.stderr);
    assert_eq!(r.code, 1, "the direct finding still fires, got:\n{}", r.stdout);
}

/// No `php` at all is the plain sound subset, which publishes under its
/// engine-off stamp as it always did.
#[test]
fn an_absent_php_still_publishes() {
    let empty = std::env::temp_dir().join(format!("steins-handshake-empty-{}", std::process::id()));
    std::fs::create_dir_all(&empty).expect("create an empty PATH dir");
    let (r, published) = cached_run_against("absent", &empty);
    assert!(r.stderr.contains("no PHP sidecar"), "got stderr:\n{}", r.stderr);
    assert!(!r.stderr.contains("sound subset (degraded)"), "got stderr:\n{}", r.stderr);
    assert!(published, "an absent php is the engine-off posture and publishes:\n{}", r.stderr);
}

/// `doctor` says what happened to a `php` that started and never booted: not
/// "no `php` on PATH", which is the other report (issue #891).
#[test]
fn doctor_reports_a_boot_failure_as_one_not_as_an_absent_php() {
    let r = run_against_hung_sidecar(&["doctor"]);
    assert!(
        r.stdout.contains("PHP sidecar: spawned, but did not complete its boot handshake"),
        "got stdout:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("posture: sound subset (degraded)"), "got stdout:\n{}", r.stdout);
    assert!(!r.stdout.contains("not spawnable"), "got stdout:\n{}", r.stdout);
    assert_eq!(r.code, 0, "a degraded environment is not a failure, got:\n{}", r.stdout);
}
