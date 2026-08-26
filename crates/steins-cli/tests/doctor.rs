//! End-to-end tests for `steins doctor` (ADR-0054 Part II, C3/C4 posture set).
//! Exit semantics per §10: environment degrades at 0, config contradicts at 1,
//! usage at 2. Each test runs the real binary in an isolated temp-dir CWD;
//! most use `--no-php` for determinism (Runtime still renders, exit-neutral).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Spawns with `GITHUB_ACTIONS` scrubbed so `check`'s CI auto-detection
/// (ADR-0054 §6) doesn't emit workflow commands where a test expects text.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
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
    let out = steins_cmd().args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Wall-clock ceiling: generous next to a no-op run, tight against the walk it guards.
const TIMEOUT: Duration = Duration::from_secs(30);

/// [`run_in`] with a deadline: kills the child and fails with a hang report
/// instead of an unbounded wait; output drains on a thread to avoid pipe deadlock.
fn run_in_within(dir: &Path, args: &[&str], timeout: Duration) -> Run {
    let mut child = steins_cmd()
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

// All sections render

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
        "Generation store",
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
    // Runtime renders and stays exit-neutral regardless of `php` (§10 sound subset).
    let dir = workdir("runtime");
    write(&dir, "a.php", THREE_THROWS);
    let r = run_in(&dir, &["doctor", "."]);
    assert_eq!(r.code, 0, "environment facts report at 0; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("Runtime"), "stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("PHP version:") || r.stdout.contains("sound subset"),
        "runtime posture must render; stdout:\n{}",
        r.stdout
    );
}

/// Integer width consequence (issue #64, ADR-0028's 2026-08-14 amendment); `env`
/// already carries `int_size`. 64-bit is pinned here, 32-bit in `steins-infer`.
#[test]
fn doctor_reports_the_engines_integer_width_and_its_fold_consequence() {
    let dir = workdir("int-width");
    write(&dir, "a.php", "<?php\n$x = 1;\n");
    let r = run_in(&dir, &["doctor", "."]);
    assert_eq!(r.code, 0, "stdout:\n{}", r.stdout);
    if !r.stdout.contains("PHP version:") {
        eprintln!("SKIP doctor_reports_the_engines_integer_width…: no `php` on PATH");
        return;
    }
    assert!(r.stdout.contains("integer width:"), "stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("integer width: 8 bytes — the whole foldable allowlist is admitted"),
        "a native run is 64-bit and says so; stdout:\n{}",
        r.stdout
    );
    // `--no-php` reports no width at all rather than guessing one.
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert!(!r.stdout.contains("integer width:"), "stdout:\n{}", r.stdout);
}

// Reflected class world

/// ext-random's `Random\Randomizer` (built-in 8.2) plus a class nothing provides (#269).
const EXTENSION_CLASS_REFS: &str =
    "<?php\nfunction f(\\Random\\Randomizer $r, \\Steins\\NoSuchClass269 $n): void {}\n";

/// Doctor names the classes it resolved off the project's own PHP, and origin extension.
#[test]
fn doctor_reports_the_reflected_class_world() {
    let dir = workdir("reflected");
    write(&dir, "a.php", EXTENSION_CLASS_REFS);
    let r = run_in(&dir, &["doctor", "."]);
    assert_eq!(r.code, 0, "stdout:\n{}", r.stdout);
    if !r.stdout.contains("PHP version:") {
        eprintln!("SKIP doctor_reports_the_reflected_class_world: no `php` on PATH");
        return;
    }
    assert!(r.stdout.contains("reflected class world:"), "stdout:\n{}", r.stdout);
    // ext-random is built in from 8.2; else the line renders but resolves nothing.
    if r.stdout.contains("Random\\Randomizer") {
        assert!(r.stdout.contains("Random\\Randomizer (random)"), "stdout:\n{}", r.stdout);
        assert!(
            r.stdout.contains("no absence finding is premised on it"),
            "the ruling is stated beside the fact; stdout:\n{}",
            r.stdout
        );
    }
}

/// `--no-php` has no engine to ask, so the line does not appear at all.
#[test]
fn doctor_no_php_says_nothing_about_a_reflected_class_world() {
    let dir = workdir("reflected-nophp");
    write(&dir, "a.php", EXTENSION_CLASS_REFS);
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 0, "stdout:\n{}", r.stdout);
    assert!(
        !r.stdout.contains("reflected class world"),
        "no engine, no line; stdout:\n{}",
        r.stdout
    );
}

// Active surface line

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

// `[runtime]` pseudo-constant lines

#[test]
fn doctor_names_both_runtime_postures_on_a_bare_project() {
    // Named-silence (ADR-0037 §2): both keys print with provenance even with no config.
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
    // Issue #234: affects intersection types; line must carry its readonly/final boundary.
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
    assert!(
        r.stdout.contains("[runtime] warning-handler: \"abort\" (default)"),
        "stdout:\n{}",
        r.stdout
    );
}

#[test]
fn doctor_distinguishes_a_declared_default_from_an_absent_key() {
    // Spelled `"enforced"` resolves like absence but reports "declared" not "default".
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
    // Unknown value: check warns-and-proceeds; doctor reports it degraded, not contradictory.
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
    // Issue #108: `throws-direct` reaches throw.undeclared via `enable`; list names it.
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
    // Issue #108/PR #133: `enable = ["debug.type"]` registers the debug id but must not
    // surface — `surfaces_id` excludes it before `enable`/`disable` are consulted.
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

// Envelope scan

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

// Baseline

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
    // A proof finding plus a direct throw, captured under CONTRACTS so both land.
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
    // `surfaces_id` excludes the debug lane unconditionally (issue #108); a leftover
    // `debug.type` entry must not be reported dormant (`check` treats it as stale, main.rs).
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

// Config contradiction exits

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
    // Unknown `[runtime]` key: exit 2 for check, exit 1 (contradiction) for doctor (§10).
    let dir = workdir("bad-runtime");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[runtime]\nzend-asertions = \"enabled\"\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unknown [runtime] key → doctor exit 1; stdout:\n{}", r.stdout);
}

// Usage errors

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

/// ADR-0054 §10 amendment: a path naming nothing is doctor's own usage error
/// (2), completing ADR-0050 §7. Timeout-guarded: the bug closed here was a
/// hang (`composer::discover` walked all of `/`), not a wrong code.
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
    // No report at all — checked ahead of the header line.
    assert!(r.stdout.is_empty(), "no report emitted; stdout:\n{}", r.stdout);
}

/// Relative spelling bites: pre-fix, a typo'd subdirectory resolved against the CWD.
#[test]
fn doctor_rejects_a_relative_path_that_names_nothing() {
    let dir = workdir("missing-relative");
    write(&dir, "composer.json", r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#);
    let r = run_in_within(&dir, &["doctor", "--no-php", "src-typo"], TIMEOUT);
    assert_eq!(r.code, 2, "nonexistent relative path → exit 2; stdout:\n{}", r.stdout);
    assert!(r.stderr.contains("path does not exist: src-typo"), "stderr:\n{}", r.stderr);
    assert!(r.stdout.is_empty(), "stdout:\n{}", r.stdout);
}

/// Existence, not emptiness, is the discriminator (§10's exit-0 environment lane).
#[test]
fn doctor_reports_on_an_existing_empty_path_at_zero() {
    let dir = workdir("existing-empty");
    std::fs::create_dir_all(dir.join("src")).expect("create empty subdir");
    let r = run_in_within(&dir, &["doctor", "--no-php", "src"], TIMEOUT);
    assert_eq!(r.code, 0, "an existing empty dir still reports at 0; stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("posture report"), "stdout:\n{}", r.stdout);
}

// Coverage posture (issue #30)

/// 11/17 scopes poisoned across give-up-list constructs (by-ref capture counts once).
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
    assert!(r.stdout.contains("a guess until measured"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_reports_dam_sites_broken_down_by_kind() {
    let dir = workdir("dam");
    write(&dir, "a.php", OPAQUE);
    write(&dir, "b.php", "<?php\nclass_alias($src, 'B');\n");
    // Issue #36: `X::class` is compile-time, so this adds an index edge, not a 4th dam site.
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
    // States what it looked at: clean-code silence differs from opaque-code silence.
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

// Coverage: sound subset

#[test]
fn doctor_names_the_sound_subset_ids_when_no_sidecar() {
    // ADR-0054 §9.2/A2(ii): names which absence claims go silent, not just "some".
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

// Runtime: A6 SAPI

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

// Catalog

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
    // Portability breakdown (ADR-0028's 2026-08-14 amendment §4, #330): from the
    // catalog's own accessors, so refused stays distinguished from unverified.
    // The word is "portability" and not "width" because one refused row is not
    // about the width at all, and the user-facing line must not say it is.
    let expected = format!(
        "(portability: {} portable / {} refused / {} unverified)",
        steins_catalog::portable_names().len(),
        steins_catalog::refused_names().len(),
        steins_catalog::unverified_names().len()
    );
    assert!(r.stdout.contains(&expected), "expected `{expected}`; stdout:\n{}", r.stdout);
    // The guard this replaces required a NON-EMPTY unverified list, on the
    // ground that a zero would make the line above vacuous. The class is empty
    // since issue #382 measured its last two rows, and the concern it names is
    // real: a hardcoded `0 unverified` would satisfy the format assertion. So
    // the zero is asserted as a RENDERED fact — the doctor says "nothing here is
    // unmeasured" rather than falling silent about the class — and the other two
    // counts, which are non-empty, are what pin that the numbers come from the
    // accessors at all.
    assert!(
        r.stdout.contains("0 unverified"),
        "an empty class is still reported, not omitted; stdout:\n{}",
        r.stdout
    );
    assert!(
        !steins_catalog::portable_names().is_empty()
            && !steins_catalog::refused_names().is_empty(),
        "two non-empty counts are what make the format assertion above bite"
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
    // ADR-0052 A11: a target range not exactly matching the catalog's pin is skewed.
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

// Registry totality

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

// `--format json`

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
            "Generation store",
            "Coverage posture",
            "Envelopes",
            "Baseline",
            "Catalog",
            "Registry totality",
            "Require",
        ],
        "the json section list must match the text rendering's structure exactly"
    );
    // Spot-check: content agrees between renderings (json trims leading-space indentation).
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

// `[doctor] require`

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
    // Orchestrator ruling, issue #268 (ADR-0054 §14): an unconfirmable comparison
    // is a violation here, unlike the Catalog section's lenient default.
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
    assert!(r.stdout.contains("skew is unconfirmed"), "stdout:\n{}", r.stdout);
}

#[test]
fn doctor_require_catalog_pin_match_passes_on_a_confirmed_match() {
    // steins_catalog::PINNED_PHP is 8.5 as of this writing — an exact match confirms.
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
    // `deny_unknown_fields` (issue #268): a misspelled `[doctor]` key is a hard parse error.
    let dir = workdir("doctor-config-typo");
    write(&dir, "a.php", THREE_THROWS);
    write(&dir, "steins.toml", "[doctor]\nrequires = [\"sidecar\"]\n");
    let r = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(r.code, 1, "unknown [doctor] key → parse error → contradiction; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("PARSE ERROR"), "stdout:\n{}", r.stdout);
}

// Generation store (ADR-0092 §2, issue #525)

/// Where the cache's disposition went when `check` went silent: doctor reports
/// an absent store as an absence, and a published one by its own manifest.
#[test]
fn doctor_reports_the_generation_store() {
    let dir = workdir("gen-store");
    write(&dir, "a.php", THREE_THROWS);

    let before = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(before.code, 0, "an absent store is a posture, not a contradiction");
    assert!(
        before.stdout.contains("store: absent"),
        "no store yet → absent; stdout:\n{}",
        before.stdout
    );
    // Doctor reads; it never creates what it reports on.
    assert!(!dir.join(".steins").exists(), "doctor must not create a store");

    // The cached run adds nothing to what the uncached run said — the whole
    // point of the silence (ADR-0020 amendment): here that is the sound-subset
    // notice `--no-php` prints, and nothing else.
    let uncached = run_in(&dir, &["check", "--no-php", "--no-cache", "."]);
    let check = run_in(&dir, &["check", "--no-php", "."]);
    assert_eq!(check.stderr, uncached.stderr, "a cached check says nothing extra");
    let current = std::fs::read_to_string(dir.join(".steins/gen/CURRENT"))
        .expect("the check published a generation");

    let after = run_in(&dir, &["doctor", "--no-php", "."]);
    assert_eq!(after.code, 0);
    assert!(
        after.stdout.contains(current.trim()),
        "the store section names the published generation; stdout:\n{}",
        after.stdout
    );
    assert!(
        after.stdout.contains("packages: ") && after.stdout.contains("on disk: "),
        "the store section reports its package count and size; stdout:\n{}",
        after.stdout
    );
}
