//! End-to-end tests for `steins.toml [paths] vendor-dirs` (issue #181): the
//! no-manifest config channel for a project that predates or ignores Composer.
//!
//! Each test runs the real binary in a private temp dir, mirroring
//! `tests/profile.rs`'s isolation discipline: `steins.toml` reads from the CWD,
//! not the analyzed path, so these can't reuse the checked-in `tests/fixtures`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Spawns the binary with `GITHUB_ACTIONS` scrubbed, since `check`'s format
/// auto-detection (ADR-0054 §6) reads it and would else emit workflow commands.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-vendor-paths-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
}

fn run_in(dir: &Path, args: &[&str]) -> Run {
    let out = steins_cmd().args(args).current_dir(dir).output().expect("run steins");
    Run { code: out.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&out.stdout).into_owned() }
}

fn write(dir: &Path, name: &str, contents: &str) {
    let target = dir.join(name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(target, contents).expect("write fixture");
}

/// First-party code: `width()` is defined and called badly, on every fixture.
const APP: &str = "<?php\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n";

/// A third-party-shaped file defining and badly calling `{name}()`, so two
/// candidate directories in one project never collide on a declaration.
fn lib(name: &str) -> String {
    format!("<?php\nfunction {name}(int $h): int {{ return $h; }}\n{name}(\"xyz\");\n")
}

// No config: the literal `vendor` floor only.

#[test]
fn no_manifest_no_key_only_the_literal_vendor_directory_is_suppressed() {
    let dir = workdir("literal-only");
    write(&dir, "app.php", APP);
    write(&dir, "vendor/acme/lib.php", &lib("heightVendor"));
    write(&dir, "3rdparty/acme/lib.php", &lib("heightThirdparty"));

    let r = run_in(&dir, &["check", "."]);
    assert_eq!(r.code, 1, "got:\n{}", r.stdout);
    assert!(r.stdout.contains("to width() cannot become int $w"), "first-party shown, got:\n{}", r.stdout);
    assert!(!r.stdout.contains("to heightVendor()"), "vendor/ suppressed, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("to heightThirdparty() cannot become int $h"),
        "3rdparty/ NOT suppressed with no config, got:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("1 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "exactly one suppression (vendor/ only), got:\n{}",
        r.stdout
    );
}

// `[paths] vendor-dirs` fills the no-manifest gap.

#[test]
fn steins_toml_paths_vendor_dirs_suppresses_the_declared_directory() {
    let dir = workdir("declared");
    write(&dir, "steins.toml", "[paths]\nvendor-dirs = [\"3rdparty\"]\n");
    write(&dir, "app.php", APP);
    write(&dir, "3rdparty/acme/lib.php", &lib("height"));

    let def = run_in(&dir, &["check", "."]);
    assert_eq!(def.code, 1, "got:\n{}", def.stdout);
    assert!(def.stdout.contains("to width() cannot become int $w"), "first-party shown, got:\n{}", def.stdout);
    assert!(!def.stdout.contains("to height()"), "3rdparty/ suppressed, got:\n{}", def.stdout);
    assert!(
        def.stdout.contains("1 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "got:\n{}",
        def.stdout
    );

    let show = run_in(&dir, &["check", "--vendor-diagnostics", "."]);
    assert_eq!(show.code, 1);
    assert!(show.stdout.contains("to width() cannot become int $w"));
    assert!(show.stdout.contains("to height() cannot become int $h"), "shown under the flag, got:\n{}", show.stdout);
}

#[test]
fn steins_toml_paths_vendor_dirs_still_honours_the_vendor_literal_too() {
    // Additive: declaring `3rdparty` does not withdraw the `vendor` literal too.
    let dir = workdir("additive");
    write(&dir, "steins.toml", "[paths]\nvendor-dirs = [\"3rdparty\"]\n");
    write(&dir, "app.php", APP);
    write(&dir, "vendor/acme/lib.php", &lib("heightVendor"));
    write(&dir, "3rdparty/acme/lib.php", &lib("heightThirdparty"));

    let r = run_in(&dir, &["check", "."]);
    assert_eq!(r.code, 1, "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("to heightVendor()"), "vendor/ still suppressed, got:\n{}", r.stdout);
    assert!(!r.stdout.contains("to heightThirdparty()"), "3rdparty/ suppressed by config, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("2 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "both suppressed, got:\n{}",
        r.stdout
    );
}

#[test]
fn a_present_composer_manifest_needs_no_paths_section_at_all() {
    // Acceptance criterion: a `composer.json` beside a `steins.toml` with no
    // `[paths]` section still resolves via Composer's own `vendor-dir` (zero-config).
    let dir = workdir("zero-config-with-manifest");
    write(
        &dir,
        "composer.json",
        r#"{"config":{"vendor-dir":"3rdparty"},"autoload":{"psr-4":{"App\\":"src/"}}}"#,
    );
    write(&dir, "steins.toml", "[check]\nprofile = \"default\"\n");
    write(&dir, "src/app.php", APP);
    write(&dir, "3rdparty/acme/lib.php", &lib("height"));

    let r = run_in(&dir, &["check", "."]);
    assert_eq!(r.code, 1, "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("to height()"), "3rdparty/ suppressed via the manifest, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("1 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "got:\n{}",
        r.stdout
    );
}

// Whole-path-component matching stays, for the config channel too.

#[test]
fn a_declared_vendor_dir_never_matches_a_component_prefix_or_suffix() {
    let dir = workdir("no-prefix-suffix");
    write(&dir, "steins.toml", "[paths]\nvendor-dirs = [\"3rdparty\"]\n");
    write(&dir, "app.php", APP);
    // Shares characters with the declared name but is not the whole component.
    write(&dir, "3rdparty_extra/acme/lib.php", &lib("heightExtra"));
    write(&dir, "my3rdparty.php", &lib("heightSuffix"));

    let r = run_in(&dir, &["check", "."]);
    assert_eq!(r.code, 1, "got:\n{}", r.stdout);
    assert!(r.stdout.contains("to heightExtra() cannot become int $h"), "not vendor, got:\n{}", r.stdout);
    assert!(r.stdout.contains("to heightSuffix() cannot become int $h"), "not vendor, got:\n{}", r.stdout);
    assert!(!r.stdout.contains("in vendor suppressed"), "nothing suppressed, got:\n{}", r.stdout);
}

#[test]
fn a_multi_component_declared_entry_matches_only_the_whole_contiguous_run() {
    let dir = workdir("multi-component");
    write(&dir, "steins.toml", "[paths]\nvendor-dirs = [\"lib/deps\"]\n");
    write(&dir, "app.php", APP);
    // The whole `lib/deps` run is vendor; `lib` and `deps` split by `other/` are not.
    write(&dir, "lib/deps/acme/lib.php", &lib("heightDeps"));
    write(&dir, "lib/other/deps/acme/lib.php", &lib("heightSplit"));

    let r = run_in(&dir, &["check", "."]);
    assert_eq!(r.code, 1, "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("to heightDeps()"), "lib/deps/ suppressed, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("to heightSplit() cannot become int $h"),
        "the split path is not the declared sequence, got:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("1 findings in vendor suppressed (--vendor-diagnostics to show)"),
        "got:\n{}",
        r.stdout
    );
}
