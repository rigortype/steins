//! End-to-end tests for `steins doctor` (ADR-0054 Part II, C3 plus C4's coverage
//! posture): the sections (Runtime, Config + active surface, Layout, Coverage
//! posture, Envelopes, Baseline) and the exit semantics (§10 — environment degrades
//! at 0, configuration contradicts at 1, usage at 2).
//!
//! Each test runs the real `steins` binary in a private temp dir (its own CWD) so
//! the auto-loaded `steins.toml` and `.steins-baseline.jsonl` are isolated. Most use
//! `--no-php` for determinism (no dependency on a `php` on PATH); the Runtime section
//! still renders (the sound-subset posture) and every run stays exit-neutral there.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-doctor-{}-{tag}-{n}", std::process::id()));
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
    let out = Command::new(bin()).args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Wall-clock ceiling for the usage-error runs below. Generous next to a doctor
/// run that does no work at all (it exits before the header), tight next to the
/// unbounded filesystem walk it guards against.
const TIMEOUT: Duration = Duration::from_secs(30);

/// [`run_in`] with a deadline: on expiry the child is killed and the test fails
/// with a hang report rather than blocking the suite.
///
/// `Command::output` waits forever, which is the wrong failure mode for a
/// regression whose symptom *is* not terminating — CI would stall on a red build
/// instead of reporting one. Piped output is drained on a thread so a child that
/// fills its pipe buffer cannot deadlock against the timer.
fn run_in_within(dir: &Path, args: &[&str], timeout: Duration) -> Run {
    let mut child = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn steins");
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let _ = out.read_to_end(&mut o);
        let _ = err.read_to_end(&mut e);
        (o, e)
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().expect("wait on steins") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                panic!("`steins {}` did not terminate within {timeout:?}", args.join(" "));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let (o, e) = reader.join().expect("output reader");
    Run {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o).into_owned(),
        stderr: String::from_utf8_lossy(&e).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

/// Two functions and one method, each carrying a written `@throws` (3 envelopes),
/// plus one function with no `@throws` (not counted).
const THREE_THROWS: &str = "<?php\n\
    /** @throws \\RuntimeException */\n\
    function a(): void { throw new \\RuntimeException(); }\n\
    /** @throws \\LogicException */\n\
    function b(): void {}\n\
    class C {\n\
    /** @throws \\JsonException */\n\
    public function m(): void {}\n\
    public function n(): void {}\n\
    }\n";

// ------------------------------------------------------- all sections render ---

#[test]
fn doctor_renders_all_sections_exit_zero() {
    let dir = workdir("sections");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "healthy/degraded world → exit 0; stdout:\n{}", r.stdout);
    for section in [
        "Runtime",
        "Config + active surface",
        "Layout",
        "Coverage posture",
        "Envelopes",
        "Baseline",
    ] {
        assert!(r.stdout.contains(section), "missing `{section}` section; stdout:\n{}", r.stdout);
    }
}

#[test]
fn doctor_default_path_is_dot() {
    // No path argument defaults to `.`.
    let dir = workdir("defaultpath");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("Envelopes"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_without_no_php_still_renders_runtime_and_exits_zero() {
    // With or without a `php` on PATH the Runtime section renders and the run is
    // exit-neutral: a missing sidecar is the sound subset, never a failure (§10).
    let dir = workdir("runtime");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "."]);
    assert_eq!(r.code, 0, "environment facts report at 0; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("Runtime"), "stdout:\n{}", r.stdout);
    // Either a live version line or the sound-subset posture — one must appear.
    assert!(
        r.stdout.contains("PHP version:") || r.stdout.contains("sound subset"),
        "runtime posture must render; stdout:\n{}",
        r.stdout
    );
}

// -------------------------------------------------------- active surface line ---

#[test]
fn doctor_reflects_configured_profile() {
    let dir = workdir("profile");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[check]\nprofile = \"contracts\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("active profile: `contracts`"),
        "active profile must reflect [check] profile; stdout:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("[check] profile"), "provenance named; stdout:\n{}", r.stdout);
}

#[test]
fn doctor_default_profile_provenance() {
    let dir = workdir("default-prof");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("active profile: `default`") && r.stdout.contains("built-in default"),
        "stdout:\n{}",
        r.stdout
    );
}

// -------------------------------------------------------------- envelope scan ---

#[test]
fn doctor_counts_written_throws_envelopes() {
    let dir = workdir("envcount");
    write(&dir, "a.php", THREE_THROWS);
    // Default profile does not check throw.undeclared → the G1-demote notice fires.
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("3 written throw envelope"),
        "expected 3 envelopes counted; stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("does not check them") && r.stdout.contains("contracts"),
        "the G1-demote notice must name the checking profile; stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_envelope_notice_flips_under_contracts() {
    let dir = workdir("envcontracts");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[check]\nprofile = \"contracts\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("3 declaration(s) carry a written @throws")
            && r.stdout.contains("checks them"),
        "under contracts the envelopes are checked; stdout:\n{}",
        r.stdout
    );
}

// ------------------------------------------------------------------- baseline ---

#[test]
fn doctor_reports_no_baseline_when_absent() {
    let dir = workdir("nobaseline");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(r.stdout.contains("Baseline\n  none"), "stdout:\n{}", r.stdout);
    assert_eq!(r.code, 0);
}

#[test]
fn doctor_reports_baseline_capture_surface_and_dormant() {
    let dir = workdir("baseline");
    // A proof finding (width("abc")) plus a direct throw; capture under CONTRACTS so
    // the baseline holds both the proof and the throw entry.
    let mixed = "<?php\n\
        function width(int $w): int { return $w; }\n\
        width(\"abc\");\n\
        /** @throws \\JsonException */\n\
        function f(): void { throw new \\RangeException(); }\n";
    write(&dir, "a.php", mixed);
    let r = run_in(&dir, &["check", "--no-php", "--profile", "contracts", "--set-baseline", "a.php"]);
    assert_eq!(r.code, 0, "set-baseline exits 0; stderr:\n{}", r.stderr);

    // Doctor under the DEFAULT surface: the throw entry's id is outside it → dormant.
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "dormant entries are not a failure; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("capture surface: profile `contracts`"), "stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("active surface: profile `default`"), "stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("dormant entr"), "the out-of-surface throw entry is dormant; stdout:\n{}", r.stdout);
}

#[test]
fn doctor_accepts_explicit_baseline_flag() {
    let dir = workdir("baseline-flag");
    write(&dir, "a.php", "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n");
    let r = run_in(&dir, &["check", "--no-php", "--set-baseline", "--baseline", "custom.jsonl", "a.php"]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let r = run_in(&dir, &["doctor", "--no-php", "--baseline", "custom.jsonl", "."]);
    assert!(r.stdout.contains("custom.jsonl"), "explicit baseline reported; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("capture surface: profile `default`"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_unparseable_baseline_is_a_contradiction() {
    let dir = workdir("bad-baseline");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, ".steins-baseline.jsonl", "not json at all\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unparseable baseline → config contradiction exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("UNPARSEABLE"), "stdout:\n{}", r.stdout);
}

// ------------------------------------------------- config contradiction exits ---

#[test]
fn doctor_malformed_toml_exits_one() {
    // ADR-0054 §10: for DOCTOR a config contradiction is exit 1 (check's is exit 2).
    let dir = workdir("bad-toml");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "garbage = = [[[\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "malformed steins.toml → doctor exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("PARSE ERROR"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_unknown_profile_exits_one() {
    let dir = workdir("unknown-prof");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[check]\nprofile = \"nope\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unknown profile → doctor config contradiction exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("profile resolution: ERROR"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_unknown_runtime_key_exits_one() {
    // The unknown-[runtime]-key parse failure is exit 2 for check, but a config
    // contradiction (exit 1) for doctor (§10).
    let dir = workdir("bad-runtime");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[runtime]\nzend-asertions = \"enabled\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unknown [runtime] key → doctor exit 1; stdout:\n{}", r.stdout);
}

// ----------------------------------------------------------------- usage errors ---

#[test]
fn doctor_rejects_extra_paths() {
    let dir = workdir("usage");
    let r = run_in(&dir, &["doctor", "a", "b"]);
    assert_eq!(r.code, 2, "too many paths → usage error exit 2; stderr:\n{}", r.stderr);
}

#[test]
fn doctor_rejects_unknown_flag() {
    let dir = workdir("badflag");
    let r = run_in(&dir, &["doctor", "--nope"]);
    assert_eq!(r.code, 2, "unknown flag → usage error exit 2; stderr:\n{}", r.stderr);
}

/// ADR-0054 §10 amendment: a path argument that names nothing is doctor's own
/// usage error (2) — the open half of the ADR-0050 §7 amendment, which settled
/// the same question for `check`/`transform`/`effect-diff` and left doctor out.
///
/// Run under a timeout because the bug this closes was a *hang*, not a wrong
/// code: `composer::discover` substituted the parent of a nonexistent path and,
/// for a top-level `/typo`, walked all of `/`. A regression must red this test,
/// not wedge CI until the job's own timeout kills it.
#[test]
fn doctor_rejects_a_path_that_names_nothing() {
    let dir = workdir("missing-path");
    let r = run_in_within(&dir, &["doctor", "--no-php", "/definitely-not-a-real-path-9x8"], TIMEOUT);
    assert_eq!(r.code, 2, "nonexistent path → usage error exit 2; stderr:\n{}", r.stderr);
    assert!(
        r.stderr.contains("path does not exist: /definitely-not-a-real-path-9x8"),
        "the missing path is named, with the same message the path-walking commands use; stderr:\n{}",
        r.stderr
    );
    // Checked ahead of the header line: no posture report about a tree that is
    // not there, not even the banner.
    assert!(r.stdout.is_empty(), "no report emitted; stdout:\n{}", r.stdout);
}

/// The relative spelling is the same usage error, and is the one that actually
/// bites: a typo'd subdirectory resolves against the CWD, so before the fix its
/// seed was the *real* project root and doctor produced a plausible report about
/// the wrong tree.
#[test]
fn doctor_rejects_a_relative_path_that_names_nothing() {
    let dir = workdir("missing-relative");
    write(&dir, "composer.json", r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#);
    let r = run_in_within(&dir, &["doctor", "--no-php", "src-typo"], TIMEOUT);
    assert_eq!(r.code, 2, "nonexistent relative path → exit 2; stdout:\n{}", r.stdout);
    assert!(r.stderr.contains("path does not exist: src-typo"), "stderr:\n{}", r.stderr);
    assert!(r.stdout.is_empty(), "stdout:\n{}", r.stdout);
}

/// The discriminator is existence, not emptiness — §10's exit-0 environment lane
/// is untouched. A directory that exists and holds nothing is a real place doctor
/// genuinely has a posture to report about.
#[test]
fn doctor_reports_on_an_existing_empty_path_at_zero() {
    let dir = workdir("existing-empty");
    std::fs::create_dir_all(dir.join("src")).expect("create empty subdir");
    let r = run_in_within(&dir, &["doctor", "--no-php", "src"], TIMEOUT);
    assert_eq!(r.code, 0, "an existing empty dir still reports at 0; stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("posture report"), "stdout:\n{}", r.stdout);
}

// ------------------------------------------------ coverage posture (issue #30) ---

/// One scope per give-up-list construct, plus a clean function and the four
/// reflection shapes. 11 poisoned scopes over 17: the by-ref capture poisons the
/// enclosing scope AND the closure's own — one aliasing fact, two silenced scopes,
/// but one construct in the source (so it counts once in the construct breakdown).
const OPAQUE: &str = "<?php\n\
    function with_eval(string $c): void { eval($c); }\n\
    function with_include(string $p): void { include $p; }\n\
    function with_require(string $p): void { require $p; }\n\
    function with_extract(array $r): void { extract($r); }\n\
    function with_compact(int $a): array { return compact('a'); }\n\
    function with_varvar(string $n): void { $$n = 1; }\n\
    function with_ref(array $r): void { $x = &$r[0]; }\n\
    function with_global(): void { global $config; }\n\
    function with_static(): int { static $n = 0; return ++$n; }\n\
    function with_capture(): callable { $t = 0; return function (int $n) use (&$t): void { $t += $n; }; }\n\
    function clean(int $a, int $b): int { return $a + $b; }\n\
    class R {\n\
    public function a(\\ReflectionMethod $m, object $o): mixed { return $m->invoke($o); }\n\
    public function b(\\ReflectionClass $c): object { return $c->newInstance(); }\n\
    public function c(\\Closure $f, object $o, string $s): \\Closure { return \\Closure::bind($f, $o, $s); }\n\
    public function d(int $first): array { return func_get_args(); }\n\
    }\n";

#[test]
fn doctor_inventories_the_opaque_constructs() {
    let dir = workdir("coverage");
    write(&dir, "a.php", OPAQUE);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "an inventory is a report, never a failure; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("Coverage posture"), "stdout:\n{}", r.stdout);
    // The share, not a bare scalar: poisoned scopes against ALL scopes.
    assert!(
        r.stdout.contains("17 scope(s), 11 poisoned (64.7%)"),
        "expected the poisoned-scope share; stdout:\n{}",
        r.stdout
    );
    // Every construct kind named, with its own count.
    for kind in [
        "eval 1",
        "include/require 2",
        "extract 1",
        "compact 1",
        "variable variable 1",
        "reference assignment 1",
        "global 1",
        "static variable 1",
        "by-ref capture 1",
    ] {
        assert!(r.stdout.contains(kind), "missing `{kind}`; stdout:\n{}", r.stdout);
    }
}

#[test]
fn doctor_inventories_reflection_sites_and_labels_them_a_guess() {
    let dir = workdir("reflection");
    write(&dir, "a.php", OPAQUE);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("reflection-driven invocation: 4 site(s)")
            && r.stdout.contains("->invoke*() 1")
            && r.stdout.contains("->newInstance*() 1")
            && r.stdout.contains("Closure::bind (computed scope) 1")
            && r.stdout.contains("func_get_args() in a typed signature 1"),
        "stdout:\n{}",
        r.stdout
    );
    // The list is a guess until measured, and the report says so on every run.
    assert!(r.stdout.contains("a guess until measured"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_reports_dam_sites_broken_down_by_kind() {
    let dir = workdir("dam");
    write(&dir, "a.php", OPAQUE);
    write(&dir, "b.php", "<?php\nclass_alias($src, 'B');\n");
    // Issue #36: a `X::class` argument is compile-time, so this third file adds an
    // index edge and NOT a fourth dam site — the count below is the assertion.
    write(&dir, "c.php", "<?php\nclass Thing {}\nclass_alias(Thing::class, 'Legacy_Thing');\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("dam sites: 4")
            && r.stdout.contains("eval 1")
            && r.stdout.contains("unproven/out-of-universe include 2")
            && r.stdout.contains("runtime-name class_alias 1"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_says_a_clean_tree_is_clean() {
    // The point of the section: a quiet run states what it looked at, so silence
    // over clean code reads differently from silence over opaque code.
    let dir = workdir("clean");
    write(&dir, "a.php", "<?php\nfunction f(int $x): int { return $x + 1; }\nf(1);\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("2 scope(s), 0 poisoned (0.0%)")
            && r.stdout.contains("opaque constructs: none")
            && r.stdout.contains("dam sites: none")
            && r.stdout.contains("reflection-driven invocation: none recognized"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_coverage_survives_an_empty_tree() {
    let dir = workdir("empty");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("no .php files under"), "stdout:\n{}", r.stdout);
}
