//! End-to-end tests for `steins.toml [runtime] final-keyword` (issue #234): the
//! posture declaring that the runtime this project is analyzed for strips the
//! `final` keyword, the way `dg/bypass-finals` does under a test harness.
//!
//! The *rule* the posture governs — whether an intersection carrying a final class
//! arm is inhabited — is pinned where it lives, in
//! `steins_contract::normalize::provably_uninhabited`'s own unit tests. Those tests
//! deliberately do not need a consumer to exist. What is pinned here is everything
//! the CLI owns: that the key round-trips, that the safe posture is what absence
//! means, that a declared posture changes **nothing** in the finding stream today,
//! and — the boundary the issue draws — that it never reaches `readonly`.
//!
//! Each test runs the real `steins` binary in a private temp dir (its own CWD),
//! mirroring `tests/profile.rs`'s isolation discipline: `steins.toml` is read from
//! the process's working directory, not from the analyzed path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test in this file spawns the binary with `GITHUB_ACTIONS` scrubbed.
/// `check`'s format auto-detection (ADR-0054 §6) reads that variable, so a test
/// run *on* GitHub Actions would otherwise get workflow commands where it
/// asserted text. No test's expected output may depend on the ambient CI
/// environment; detection itself is tested in `tests/format_github.rs`, which
/// sets the variable deliberately.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("steins-final-keyword-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_in(dir: &Path, args: &[&str]) -> Run {
    let out = steins_cmd().args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// A project that spells the very type the issue is about — a `final` service class,
/// the mock marker interface, and a parameter typed as their intersection — plus a
/// member access on that receiver and a call passing a plain instance into it.
/// Every position a consumer of intersections would eventually judge.
const INTERSECTION: &str = "<?php\n\
interface Mock {}\n\
final class Svc { public function run(): int { return 1; } }\n\
function drive(Svc&Mock $m): int { return $m->run(); }\n\
function plain(Svc $s): int { return drive($s); }\n";

/// A proven `readonly` reassignment: the promoted constructor parameter is the first
/// write, the method body the second. Reported as `readonly.reassigned` on the
/// default surface.
const READONLY: &str = "<?php\n\
class Acct {\n\
  public function __construct(public readonly int $balance) {}\n\
  public function reset(): void { $this->balance = 0; }\n\
}\n";

// ------------------------------------------------------------ round-tripping ---

#[test]
fn both_postures_round_trip_through_the_config() {
    // The key parses at either value and the run proceeds. `[runtime]` uses
    // `deny_unknown_fields`, so this is also the proof that `final-keyword` is a
    // *registered* key and not silently tolerated.
    for value in ["enforced", "stripped"] {
        let dir = workdir(value);
        write(&dir, "a.php", INTERSECTION);
        write(&dir, "steins.toml", &format!("[runtime]\nfinal-keyword = \"{value}\"\n"));
        let r = run_in(&dir, &["check", "--no-php", "a.php"]);
        assert_eq!(r.code, 0, "final-keyword = \"{value}\" runs clean; stderr:\n{}", r.stderr);
        assert!(
            !r.stderr.contains("final-keyword:"),
            "a recognized value warns about nothing, got:\n{}",
            r.stderr
        );
    }
}

#[test]
fn a_misspelled_key_is_a_hard_config_error() {
    // The `[runtime]` `deny_unknown_fields` rule, extended to the new key: a typo
    // must never leave the safe posture in force while the user believes they
    // overrode it. Exit 2, like `warning-hadler`.
    let dir = workdir("typo-key");
    write(&dir, "a.php", INTERSECTION);
    write(&dir, "steins.toml", "[runtime]\nfinal-keywrd = \"stripped\"\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 2, "unknown [runtime] key → exit 2; stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("parse error"), "names the parse failure, got:\n{}", r.stderr);
}

#[test]
fn an_unknown_value_warns_and_keeps_the_safe_posture() {
    // The existing `[runtime]` convention, mirrored: an unrecognized *value* on a
    // *known* key is a warn-and-proceed (the key is spelled right, so the intent is
    // legible), and what it proceeds with is the safe `"enforced"`.
    let dir = workdir("typo-value");
    write(&dir, "a.php", INTERSECTION);
    write(&dir, "steins.toml", "[runtime]\nfinal-keyword = \"bypassed\"\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 0, "warn-and-proceed, not exit 2; stderr:\n{}", r.stderr);
    assert!(
        r.stderr.contains("final-keyword: unknown value `bypassed`"),
        "names the offending value, got:\n{}",
        r.stderr
    );
    assert!(r.stderr.contains("using enforced"), "names the fallback, got:\n{}", r.stderr);
}

// -------------------------------------------------------- off is a no-op ---

#[test]
fn no_declaration_and_an_enforced_declaration_are_byte_identical() {
    // "Default off, and off is byte-identical to today" — the two spellings of the
    // safe posture cannot diverge from each other or from an absent config.
    let bare = workdir("bare");
    write(&bare, "a.php", INTERSECTION);
    let a = run_in(&bare, &["check", "--no-php", "a.php"]);

    let declared = workdir("declared-enforced");
    write(&declared, "a.php", INTERSECTION);
    write(&declared, "steins.toml", "[runtime]\nfinal-keyword = \"enforced\"\n");
    let b = run_in(&declared, &["check", "--no-php", "a.php"]);

    assert_eq!(a.code, b.code);
    assert_eq!(a.stdout, b.stdout, "stdout differs");
    assert_eq!(a.stderr, b.stderr, "stderr differs");
}

#[test]
fn the_stripped_posture_adds_and_removes_no_finding_today() {
    // The measured state issue #234 records: intersections are consumed nowhere, so
    // this slice changes no observable behaviour in either direction. The guard is
    // planted for the consumer, not for a finding — and this is the test that fails
    // loudly if the posture ever quietly grows a finding leg of its own.
    let off = workdir("off");
    write(&off, "a.php", INTERSECTION);
    let a = run_in(&off, &["check", "--no-php", "a.php"]);

    let on = workdir("on");
    write(&on, "a.php", INTERSECTION);
    write(&on, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let b = run_in(&on, &["check", "--no-php", "a.php"]);

    assert_eq!(a.code, b.code, "exit code differs");
    assert_eq!(a.stdout, b.stdout, "the finding stream differs under the declared posture");
    assert_eq!(a.stderr, b.stderr, "stderr differs under the declared posture");
}

#[test]
fn the_strict_surface_is_unmoved_too() {
    // The byte-identity claim is about the whole surface, not just the default floor:
    // a stricter profile sees the same stream under both postures.
    let off = workdir("strict-off");
    write(&off, "a.php", INTERSECTION);
    let a = run_in(&off, &["check", "--no-php", "--profile", "strict", "a.php"]);

    let on = workdir("strict-on");
    write(&on, "a.php", INTERSECTION);
    write(&on, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let b = run_in(&on, &["check", "--no-php", "--profile", "strict", "a.php"]);

    assert_eq!((a.code, a.stdout), (b.code, b.stdout));
}

// ----------------------------------------------------------- the readonly cut ---

#[test]
fn the_stripped_posture_does_not_reach_readonly() {
    // `dg/bypass-finals` strips `readonly` only when explicitly asked —
    // `enable(bypassReadOnly: true)` — and the project that motivated this issue
    // passes `false`. The two are separate knobs in the library, so they stay
    // separate here: declaring the `final` posture must never silence
    // `readonly.reassigned`, whose proof rests on the property modifier and not on
    // class finality at all.
    let dir = workdir("readonly");
    write(&dir, "a.php", READONLY);
    write(&dir, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 1, "the readonly finding still fails the run; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("error[readonly.reassigned]"),
        "readonly.reassigned still fires under the declared posture, got:\n{}",
        r.stdout
    );
}

#[test]
fn the_readonly_stream_is_byte_identical_across_postures() {
    // The stronger form of the cut: not merely "still fires", but "identical", so a
    // future demotion or message change on the readonly family cannot slip in behind
    // this posture either.
    let off = workdir("readonly-off");
    write(&off, "a.php", READONLY);
    let a = run_in(&off, &["check", "--no-php", "a.php"]);

    let on = workdir("readonly-on");
    write(&on, "a.php", READONLY);
    write(&on, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let b = run_in(&on, &["check", "--no-php", "a.php"]);

    assert_eq!((a.code, a.stdout), (b.code, b.stdout));
}

// ------------------------------------------------- the other `final` surfaces ---

#[test]
fn class_extends_final_is_unmoved_by_the_posture() {
    // Issue #234's "out of scope", pinned end to end: a declaration that extends a
    // final class is broken under a plain runtime whatever a test harness rewrites
    // at load time, so the posture does not demote it. Only the *inhabitance* of an
    // intersection type was ever at stake.
    let src = "<?php\nfinal class Sealed {}\nclass Sub extends Sealed {}\n";
    let off = workdir("extends-final-off");
    write(&off, "a.php", src);
    let a = run_in(&off, &["check", "--no-php", "--profile", "strict", "a.php"]);

    let on = workdir("extends-final-on");
    write(&on, "a.php", src);
    write(&on, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let b = run_in(&on, &["check", "--no-php", "--profile", "strict", "a.php"]);

    assert_eq!((a.code, &a.stdout), (b.code, &b.stdout), "the posture moved a `final` diagnostic");
    assert!(a.stdout.contains("class.extends-final"), "the control fires at all, got:\n{}", a.stdout);
}
