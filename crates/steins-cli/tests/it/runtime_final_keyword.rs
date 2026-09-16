//! End-to-end tests for `steins.toml [runtime] final-keyword` (issue #234): the
//! posture declaring that the runtime this project is analyzed for strips the
//! `final` keyword, the way `dg/bypass-finals` does under a test harness.
//!
//! The rule the posture governs — whether an intersection carrying a final
//! class arm is inhabited — is pinned in
//! `steins_contract::normalize::provably_uninhabited`'s own unit tests, which
//! need no consumer. Pinned here is everything the CLI owns: the key
//! round-trips, absence means the safe posture, a declared posture changes
//! nothing in the finding stream today, and it never reaches `readonly`.
//!
//! Each test runs the real `steins` binary in a private temp dir (its own
//! CWD), mirroring `tests/profile.rs`: `steins.toml` reads from the process's
//! working directory, not the analyzed path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test scrubs `GITHUB_ACTIONS`: `check`'s format auto-detection
/// (ADR-0054 §6) reads it, so CI would otherwise get workflow commands where
/// text was asserted (detection itself is tested in `tests/format_github.rs`).
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

/// A `final` service class, a mock marker interface, a parameter typed as
/// their intersection, a member access on that receiver, and a call passing a
/// plain instance — every position a consumer of intersections would judge.
const INTERSECTION: &str = "<?php\n\
interface Mock {}\n\
final class Svc { public function run(): int { return 1; } }\n\
function drive(Svc&Mock $m): int { return $m->run(); }\n\
function plain(Svc $s): int { return drive($s); }\n";

/// A proven `readonly` reassignment: promoted ctor param is the first write,
/// the method body the second. Reported as `readonly.reassigned` by default.
const READONLY: &str = "<?php\n\
class Acct {\n\
  public function __construct(public readonly int $balance) {}\n\
  public function reset(): void { $this->balance = 0; }\n\
}\n";

// Round-tripping

#[test]
fn both_postures_round_trip_through_the_config() {
    // The key parses at either value and the run proceeds. `[runtime]` uses
    // `deny_unknown_fields`, so this also proves the key is registered, not tolerated.
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
    // `deny_unknown_fields` extended to the new key: a typo must never leave the
    // safe posture in force while the user believes they overrode it. Exit 2,
    // like `warning-hadler`.
    let dir = workdir("typo-key");
    write(&dir, "a.php", INTERSECTION);
    write(&dir, "steins.toml", "[runtime]\nfinal-keywrd = \"stripped\"\n");
    let r = run_in(&dir, &["check", "--no-php", "a.php"]);
    assert_eq!(r.code, 2, "unknown [runtime] key → exit 2; stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("parse error"), "names the parse failure, got:\n{}", r.stderr);
}

#[test]
fn an_unknown_value_warns_and_keeps_the_safe_posture() {
    // Mirrors the `[runtime]` convention: an unrecognized value on a known key is
    // warn-and-proceed (key spelled right, intent legible), proceeding with `"enforced"`.
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

// Off is a no-op

#[test]
fn no_declaration_and_an_enforced_declaration_are_byte_identical() {
    // Default off, and off is byte-identical to today: neither spelling of the
    // safe posture may diverge, from each other or from absent config.
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
    // Issue #234's measured state: intersections are consumed nowhere, so this
    // slice changes no observable behaviour either way. Fails loudly if the
    // posture ever quietly grows a finding leg of its own.
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
    // Byte-identity holds for the whole surface, not just the default floor: a
    // stricter profile sees the same stream under both postures.
    let off = workdir("strict-off");
    write(&off, "a.php", INTERSECTION);
    let a = run_in(&off, &["check", "--no-php", "--profile", "strict", "a.php"]);

    let on = workdir("strict-on");
    write(&on, "a.php", INTERSECTION);
    write(&on, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let b = run_in(&on, &["check", "--no-php", "--profile", "strict", "a.php"]);

    assert_eq!((a.code, a.stdout), (b.code, b.stdout));
}

// The readonly cut

#[test]
fn the_stripped_posture_does_not_reach_readonly() {
    // `dg/bypass-finals` strips `readonly` only when explicitly asked
    // (`enable(bypassReadOnly: true)`); the motivating project passes `false`.
    // Separate knobs, kept separate here: `final-keyword` must never silence
    // `readonly.reassigned`, whose proof rests on the property modifier, not class finality.
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
    // Stronger form of the cut: not merely "still fires" but "identical", so a
    // future demotion or message change on the readonly family cannot slip in behind it.
    let off = workdir("readonly-off");
    write(&off, "a.php", READONLY);
    let a = run_in(&off, &["check", "--no-php", "a.php"]);

    let on = workdir("readonly-on");
    write(&on, "a.php", READONLY);
    write(&on, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let b = run_in(&on, &["check", "--no-php", "a.php"]);

    assert_eq!((a.code, a.stdout), (b.code, b.stdout));
}

// The other `final` surfaces

#[test]
fn class_extends_final_is_unmoved_by_the_posture() {
    // Issue #234's "out of scope": a declaration extending a final class is
    // broken under whatever runtime a test harness rewrites at load time, so the
    // posture doesn't demote it — only intersection inhabitance was ever at stake.
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
