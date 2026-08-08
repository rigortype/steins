//! End-to-end tests for `steins doctor` (ADR-0054 Part II, C3 plus C4's full
//! posture set): the sections (Runtime, Config + active surface, Layout, Coverage
//! posture, Envelopes, Baseline, Catalog, Registry totality, Require) and the exit
//! semantics (§10 — environment degrades at 0, configuration contradicts at 1,
//! usage at 2). `--format json` and `[doctor] require` (§14) get their own test
//! groups near the bottom of this file.
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
        "Catalog",
        "Registry totality",
        "Require",
    ] {
        assert!(r.stdout.contains(section), "missing `{section}` section; stdout:\n{}", r.stdout);
    }
}

#[test]
fn doctor_default_path_is_dot() {
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

// ------------------------------------------ `[runtime]` pseudo-constant lines ---

#[test]
fn doctor_names_both_runtime_postures_on_a_bare_project() {
    // Named-silence discipline (ADR-0037 §2 family): a default posture is still a
    // posture, so both keys print with their provenance even when `steins.toml` does
    // not exist at all. Absence has to be legible from the report, not inferred from
    // a missing line.
    let dir = workdir("runtime-postures-default");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("[runtime] warning-handler: \"abort\" (default)"),
        "stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("[runtime] final-keyword: \"enforced\" (default)"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_reports_a_declared_final_keyword_posture() {
    // Issue #234: the posture changes what Steins will claim about an intersection
    // type, so it must be visible without reading the source — and the line must
    // also carry the boundary, since `readonly` and the `final` diagnostics are
    // exactly what a reader would otherwise assume it moved.
    let dir = workdir("runtime-postures-stripped");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[runtime]\nfinal-keyword = \"stripped\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "a declared posture is not a contradiction; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("[runtime] final-keyword: \"stripped\" (declared)"),
        "the declared posture and its provenance; stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("`readonly` and the `final` diagnostics are unaffected"),
        "the line names its own boundary; stdout:\n{}",
        r.stdout
    );
    // The sibling posture is untouched by declaring this one.
    assert!(
        r.stdout.contains("[runtime] warning-handler: \"abort\" (default)"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_distinguishes_a_declared_default_from_an_absent_key() {
    // `"enforced"` spelled out resolves to the same posture as absence, but the
    // report says which one the project wrote — the difference between "I never
    // declared that" and "I declared it and it did not take".
    let dir = workdir("runtime-postures-declared-default");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[runtime]\nfinal-keyword = \"enforced\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("[runtime] final-keyword: \"enforced\" (declared)"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_names_an_unrecognized_runtime_value() {
    // An unknown *value* on a known key is a warn-and-proceed in `check`; doctor
    // reports it as the config fact it is, at exit 0 — the value is legible, so this
    // is a degraded posture, not a contradiction.
    let dir = workdir("runtime-postures-bad-value");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[runtime]\nfinal-keyword = \"bypassed\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "an unrecognized value is not a contradiction; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("final-keyword: unknown value `bypassed`"),
        "stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("[runtime] final-keyword: \"enforced\" (declared)"),
        "and the posture actually in force; stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_names_the_contract_layer_under_throws_direct() {
    // Issue #108: `throws-direct` reaches contract-layer `throw.undeclared`
    // through `enable`, despite sharing `Floor::Default`. The printed layer list
    // must name every layer the surface admits.
    let dir = workdir("throws-direct-layers");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[check]\nprofile = \"throws-direct\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("surface: layers [contract, mechanics, proof]"),
        "the contract layer must be named under throws-direct; stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_layer_line_excludes_debug_even_under_an_explicit_enable() {
    // Review finding on issue #108 (PR #133): `enable = ["debug.type"]` is a
    // pattern the config layer accepts (debug ids are registered), but the debug
    // lane must not surface. `surfaces_id` excludes the debug lane before
    // `enable`/`disable` are consulted, per `layers_on`'s doc comment ("the debug
    // lane is display-only and never a surface layer").
    let dir = workdir("debug-enable-layers");
    write(&dir, "a.php", THREE_THROWS);
    write(
        &dir,
        "steins.toml",
        "[check]\nprofile = \"debug-enabled\"\n\n[profile.debug-enabled]\nenable = [\"debug.type\"]\n",
    );
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("surface: layers [mechanics, proof]"),
        "debug must not appear in the layer line even under an explicit enable; stdout:\n{}",
        r.stdout
    );
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
fn doctor_never_counts_a_leftover_debug_entry_as_dormant() {
    // `surfaces_id` excludes the debug lane unconditionally (issue #108). A
    // leftover `debug.type` baseline entry must not be counted "dormant" here:
    // `check` treats a debug entry as stale dead weight at every rung
    // (`match_baseline` in main.rs), and `doctor` must not contradict that.
    let dir = workdir("debug-not-dormant");
    write(&dir, "a.php", "<?php\n$x = 1;\n\\PHPStan\\dumpType($x);\n");
    assert_eq!(run_in(&dir, &["check", "--no-php", "--set-baseline", "a.php"]).code, 0);

    let header = std::fs::read_to_string(dir.join(".steins-baseline.jsonl")).unwrap();
    let header_line = header.lines().next().expect("header line");
    let forged =
        format!("{header_line}\n{{\"id\":\"debug.type\",\"path\":\"a.php\",\"hash\":\"deadbeefdeadbeef\"}}\n");
    std::fs::write(dir.join(".steins-baseline.jsonl"), forged).unwrap();

    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "stdout:\n{}", r.stdout);
    assert!(
        !r.stdout.contains("dormant entr"),
        "a leftover debug entry must not be reported as dormant; stdout:\n{}",
        r.stdout
    );
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

// ------------------------------------------------------------------- coverage: sound subset ---

#[test]
fn doctor_names_the_sound_subset_ids_when_no_sidecar() {
    // ADR-0054 §9.2 / A2(ii): the sound-subset line names exactly which absence
    // claims go silent, not just that "some" do.
    let dir = workdir("sound-subset");
    write(&dir, "a.php", "<?php\nfunction f(int $x): int { return $x + 1; }\nf(1);\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("call.undefined-function")
            && r.stdout.contains("class.undefined")
            && r.stdout.contains("call.undefined-method")
            && r.stdout.contains("silenced"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_reports_no_vouch_sites_by_default() {
    let dir = workdir("no-vouch");
    write(&dir, "a.php", "<?php\nfunction f(int $x): int { return $x + 1; }\nf(1);\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("vouched dynamic-code exemptions: none declared"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_counts_vouched_sites_and_names_the_checker_boundary() {
    let dir = workdir("vouch");
    write(&dir, "a.php", "<?php\nfunction f(int $x): int { return $x + 1; }\nf(1);\n");
    write(
        &dir,
        "steins.toml",
        "[transform.vouch]\nsites = [\"a.php:1\", \"a.php:2\"]\n",
    );
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "a declared vouch is not a contradiction; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("vouched dynamic-code exemptions: 2 site(s) declared")
            && r.stdout.contains("consulted by `transform` only"),
        "stdout:\n{}",
        r.stdout
    );
}

// --------------------------------------------------------------------------- Runtime: A6 SAPI ---

#[test]
fn doctor_names_the_sapi_undeclared_curated_set() {
    let dir = workdir("sapi");
    write(&dir, "a.php", "<?php\n$x = 1;\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("[runtime] sapi: undeclared")
            && r.stdout.contains("fastcgi_finish_request")
            && r.stdout.contains("apache_*"),
        "stdout:\n{}",
        r.stdout
    );
}

// ------------------------------------------------------------------------------------ Catalog ---

#[test]
fn doctor_catalog_reports_the_pin_and_freshness_context() {
    let dir = workdir("catalog");
    write(&dir, "a.php", "<?php\n$x = 1;\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("Catalog"), "stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("builtin catalog pinned to php-src PHP"),
        "stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("hierarchy table:") && r.stdout.contains("foldable allowlist:"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_catalog_says_skew_is_unconfirmed_with_no_target_and_no_sidecar() {
    let dir = workdir("catalog-unconfirmed");
    write(&dir, "a.php", "<?php\n$x = 1;\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(
        r.stdout.contains("skew is unconfirmed"),
        "with no target and --no-php, skew has no comparison basis; stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_catalog_flags_skew_against_a_declared_target() {
    // ADR-0052 A11: a target range that does not exactly match the catalog's pin
    // is skewed — demonstrated with a `require.php` far from `steins_catalog::PINNED_PHP`.
    let dir = workdir("catalog-skew");
    write(&dir, "a.php", "<?php\n$x = 1;\n");
    write(&dir, "composer.json", r#"{"require":{"php":"^7.4"}}"#);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "catalog skew is a degraded posture, not a contradiction; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("SKEWED against the pin") && r.stdout.contains("A11 consequence"),
        "stdout:\n{}",
        r.stdout
    );
}

// ----------------------------------------------------------------------- Registry totality ---

#[test]
fn doctor_registry_totality_is_consistent() {
    let dir = workdir("registry");
    write(&dir, "a.php", "<?php\n$x = 1;\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("Registry totality"), "stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("registered id(s):")
            && r.stdout.contains("emittable")
            && r.stdout.contains("registered-not-yet-emitted"),
        "stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("partition consistent"),
        "the shipped registry must itself be self-consistent; stdout:\n{}",
        r.stdout
    );
}

// ------------------------------------------------------------------------ `--format json` ---

#[test]
fn doctor_format_json_renders_the_same_section_structure_as_text() {
    let dir = workdir("json");
    write(&dir, "a.php", THREE_THROWS);
    let text = run_in(&dir, &["doctor", "--no-php", "."]);
    let json_run = run_in(&dir, &["doctor", "--no-php", "--format", "json", "."]);
    assert_eq!(text.code, 0);
    assert_eq!(json_run.code, 0);

    let doc: serde_json::Value =
        serde_json::from_str(&json_run.stdout).expect("doctor --format json must emit valid JSON");
    assert_eq!(doc["schema"], "steins.doctor/v1");
    assert_eq!(doc["exit_code"], 0);
    let sections = doc["sections"].as_array().expect("sections is an array");
    let names: Vec<&str> = sections.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "Runtime",
            "Config + active surface",
            "Layout",
            "Coverage posture",
            "Envelopes",
            "Baseline",
            "Catalog",
            "Registry totality",
            "Require",
        ],
        "the json section list must match the text rendering's structure exactly"
    );
    // Spot-check one section's content agrees between the two renderings (modulo
    // the text renderer's leading-space indentation, which json trims).
    let envelopes = sections.iter().find(|s| s["name"] == "Envelopes").expect("Envelopes section");
    let envelope_line = envelopes["lines"][0].as_str().expect("a line");
    assert!(
        text.stdout.contains(envelope_line),
        "json line `{envelope_line}` must appear (trimmed) in the text rendering:\n{}",
        text.stdout
    );
}

#[test]
fn doctor_format_json_exit_code_matches_the_process_exit_code() {
    let dir = workdir("json-exit");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "garbage = = [[[\n");
    let r = run_in(&dir, &["doctor", "--no-php", "--format", "json", "."]);
    assert_eq!(r.code, 1, "malformed toml is a contradiction even under json; stdout:\n{}", r.stdout);
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid JSON even on contradiction");
    assert_eq!(doc["exit_code"], 1);
}

#[test]
fn doctor_rejects_unknown_format_value() {
    let dir = workdir("bad-format");
    let r = run_in(&dir, &["doctor", "--format", "yaml"]);
    assert_eq!(r.code, 2, "unrecognized --format value → usage error; stderr:\n{}", r.stderr);
}

#[test]
fn doctor_format_requires_an_argument() {
    let dir = workdir("format-noarg");
    let r = run_in(&dir, &["doctor", "--format"]);
    assert_eq!(r.code, 2, "stderr:\n{}", r.stderr);
}

// -------------------------------------------------------------------- `[doctor] require` ---

#[test]
fn doctor_require_not_configured_by_default() {
    let dir = workdir("require-absent");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("Require\n  not configured"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_require_sidecar_fails_under_no_php() {
    let dir = workdir("require-sidecar");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[doctor]\nrequire = [\"sidecar\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "a declared `sidecar` requirement fails under --no-php; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("FAIL `sidecar`") && r.stdout.contains("doctor exits 1"),
        "the failing assertion must be named in the output; stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_require_no_monkey_patch_passes_by_default() {
    let dir = workdir("require-monkey-patch");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[doctor]\nrequire = [\"no-monkey-patch\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "no monkey-patch extension is loaded by default; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("PASS `no-monkey-patch`"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_require_no_dormant_baseline_passes_with_no_baseline() {
    let dir = workdir("require-dormant");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[doctor]\nrequire = [\"no-dormant-baseline\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "vacuously true with no baseline at all; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("PASS `no-dormant-baseline`"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_require_catalog_pin_match_unconfirmed_is_a_failure() {
    // Orchestrator ruling on issue #268: `[doctor] require` is the named
    // strictness opt-in (ADR-0054 §14) — the caller is asking doctor to
    // GUARANTEE the pin match, so a comparison doctor cannot even attempt (no
    // target, no sidecar) is a violation, not a free pass. This deliberately
    // disagrees with the Catalog section's own rendering, which still reports
    // "unskewed" by the default lenient-default posture (unaffected by this
    // test — see `doctor_catalog_says_skew_is_unconfirmed_with_no_target_and_no_sidecar`).
    let dir = workdir("require-catalog-unconfirmed");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[doctor]\nrequire = [\"catalog-pin-match\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(
        r.code, 1,
        "no target and no sidecar ⇒ unconfirmable, and require treats that as a failure; stdout:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("FAIL `catalog-pin-match`") && r.stdout.contains("unconfirmable"),
        "the failure message must distinguish unconfirmable from skewed; stdout:\n{}",
        r.stdout
    );
    // The Catalog section's own text is untouched by this stricter require verdict.
    assert!(r.stdout.contains("skew is unconfirmed"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_require_catalog_pin_match_passes_on_a_confirmed_match() {
    // A declared target that IS the catalog's pin (steins_catalog::PINNED_PHP is
    // 8.5 as of this writing) confirms the match, so the assertion passes.
    let dir = workdir("require-catalog-confirmed");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "composer.json", r#"{"require":{"php":"8.5.*"}}"#);
    write(&dir, "steins.toml", "[doctor]\nrequire = [\"catalog-pin-match\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "an exact pin match confirms and passes; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("PASS `catalog-pin-match`"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_require_catalog_pin_match_fails_on_a_confirmed_skew() {
    let dir = workdir("require-catalog-skewed");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "composer.json", r#"{"require":{"php":"^7.4"}}"#);
    write(&dir, "steins.toml", "[doctor]\nrequire = [\"catalog-pin-match\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "a confirmed skew fails the assertion; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("FAIL `catalog-pin-match`")
            && r.stdout.contains("skewed against the catalog's php-src pin")
            && !r.stdout.contains("unconfirmable"),
        "a confirmed skew must be distinguished from an unconfirmable comparison; stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_require_unknown_assertion_is_a_hard_config_error() {
    let dir = workdir("require-unknown");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[doctor]\nrequire = [\"not-a-real-assertion\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "an unknown assertion name is a configuration contradiction; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("FAIL `not-a-real-assertion`") && r.stdout.contains("unknown assertion"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_require_rejects_an_unknown_toml_key() {
    // `deny_unknown_fields` posture (issue #268 acceptance criteria): a misspelled
    // key under `[doctor]` is a hard parse error, same as `[runtime]`/`[plugins]`.
    let dir = workdir("doctor-config-typo");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[doctor]\nrequires = [\"sidecar\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unknown [doctor] key → parse error → contradiction; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("PARSE ERROR"), "stdout:\n{}", r.stdout);
}
