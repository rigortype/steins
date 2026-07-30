//! End-to-end tests for the plugin channel tracer (issue #68, ADR-0068).
//!
//! Every test stages `tests/fixtures/plugin_proj` — a Composer project whose
//! `vendor/` carries one `type: steins-plugin` package — into a private temp
//! directory, so a test may edit the plugin's manifest, drop the vendor tree, or
//! write a `steins.toml` without touching the checked-in fixture. The staged
//! directory is also the working directory, which is where `steins.toml` is read
//! from.
//!
//! The fixture in one breath: `acme/steins-plugin` registers the label
//! `acme.cache` and colors the plain function `acme_cache_get` with it;
//! `src/app.php` declares `#[\Steins\Effect('acme.cache')]`, calls that function,
//! and — one function further down — misspells the label.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// The checked-in fixture project.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("plugin_proj")
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// A staged, mutable copy of the fixture project, removed on drop.
struct Staged(PathBuf);

impl Staged {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("steins-plugin-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_tree(&fixture(), &dir);
        Self(dir)
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).expect("create parent");
        std::fs::write(path, contents).expect("write fixture file");
    }

    fn remove(&self, rel: &str) {
        let _ = std::fs::remove_dir_all(self.0.join(rel));
    }

    /// The plugin's manifest, rewritten.
    fn manifest(&self, contents: &str) {
        self.write("vendor/acme/steins-plugin/steins-plugin.json", contents);
    }

    /// Run `steins <args> src` with the staged project as the working directory.
    fn run(&self, args: &[&str]) -> Run {
        let mut argv: Vec<&str> = args.to_vec();
        argv.push("src");
        let out =
            Command::new(bin()).args(&argv).current_dir(&self.0).output().expect("run steins");
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn check(&self) -> Run {
        self.run(&["check", "--no-php"])
    }

    /// `annotate --format json` over the one analyzed file.
    fn annotate_json(&self) -> serde_json::Value {
        let out = Command::new(bin())
            .args(["annotate", "--format", "json", "src/app.php"])
            .current_dir(&self.0)
            .output()
            .expect("run steins annotate");
        serde_json::from_slice(&out.stdout).expect("annotate emitted json")
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create staging dir");
    for entry in std::fs::read_dir(from).expect("read fixture dir").flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("copy fixture file");
        }
    }
}

/// The `declared` array `annotate --format json` reports for one function.
fn declared_of(doc: &serde_json::Value, name: &str) -> Vec<String> {
    doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .find(|f| f["name"] == name)
        .unwrap_or_else(|| panic!("no summary for {name}"))
        ["declared"]
        .as_array()
        .expect("declared array")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect()
}

const UNKNOWN_LABEL: &str = "effect.unknown-label";

// ---------------------------------------------------------------------------
// GOAL 1 — the registry opens for a registered label, and only for it.
// ---------------------------------------------------------------------------

#[test]
fn a_registered_label_is_accepted_and_a_typo_of_it_is_not() {
    let p = Staged::new("registry");
    let r = p.check();
    assert_eq!(
        r.stdout.matches(UNKNOWN_LABEL).count(),
        1,
        "exactly the typo, not the registered label; stdout:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("'acme.cach'"), "the typo is the one reported:\n{}", r.stdout);
    assert!(
        !r.stdout.contains("on read_cached()"),
        "the registered label is legal where it is declared:\n{}",
        r.stdout
    );
    // The suggestion searches the EXTENDED registry, so it can name a label no
    // builtin table contains.
    assert!(
        r.stdout.contains("did you mean 'acme.cache'?"),
        "the plugin's label is the suggestion:\n{}",
        r.stdout
    );
    assert!(
        r.stderr.lines().all(|l| !l.contains("plugin")),
        "a clean load is silent:\n{}",
        r.stderr
    );
}

// ---------------------------------------------------------------------------
// GOAL 2 — a plugin coloring reaches caller summaries, in the declared lane.
// ---------------------------------------------------------------------------

#[test]
fn a_plugin_coloring_reaches_the_callers_declared_lane_without_discharging_taint() {
    let p = Staged::new("declared");
    let doc = p.annotate_json();
    // The direct caller of the colored function...
    assert_eq!(declared_of(&doc, "read_cached"), ["acme.cache"]);
    // ...and its own caller, by ordinary propagation along the call edge.
    assert_eq!(declared_of(&doc, "warm_cache"), ["acme.cache"]);
    for name in ["read_cached", "warm_cache"] {
        let f = doc["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap();
        // ADR-0068 §1: an unchecked assertion bounds nothing. The proven lane
        // stays empty and the exhaustiveness taint survives — "declared
        // acme.cache, and possibly more".
        assert_eq!(f["effects"].as_array().unwrap().len(), 0, "{name} proves nothing");
        assert_eq!(f["exhaustive"], false, "{name} keeps its taint");
    }
}

#[test]
fn the_declared_label_shows_up_in_the_effect_diff_surface() {
    let p = Staged::new("effect-diff");
    let capture = p.run(&["effect-diff", "--set-baseline"]);
    assert_eq!(capture.code, 0, "capture failed:\n{}{}", capture.stdout, capture.stderr);
    let text = std::fs::read_to_string(p.0.join("steins-effects-baseline.json")).expect("baseline");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("baseline json");
    let warm = doc["functions"]
        .as_array()
        .expect("functions")
        .iter()
        .find(|e| e["symbol"].as_str().unwrap_or_default().ends_with("warm_cache"))
        .expect("warm_cache captured");
    assert_eq!(warm["declared"], serde_json::json!(["acme.cache"]));
    assert_eq!(warm["proven"], serde_json::json!([]));
}

// ---------------------------------------------------------------------------
// GOAL 3 — no plugin, no change.
// ---------------------------------------------------------------------------

#[test]
fn a_project_without_the_plugin_behaves_exactly_as_before() {
    let with = Staged::new("with-plugin");
    let without = Staged::new("without-plugin");
    without.remove("vendor");

    let r = without.check();
    assert_eq!(
        r.stdout.matches(UNKNOWN_LABEL).count(),
        2,
        "both labels are correctly unknown with no channel to open the registry:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("'acme.cache'") && r.stdout.contains("'acme.cach'"), "{}", r.stdout);
    assert!(r.stderr.lines().all(|l| !l.contains("plugin")), "no vendor tree, nothing to say");

    // And nothing the plugin would have contributed leaks into the summaries.
    let doc = without.annotate_json();
    assert!(declared_of(&doc, "read_cached").is_empty());
    assert!(declared_of(&doc, "warm_cache").is_empty());

    // The two runs differ ONLY in the plugin-derived lines.
    assert_eq!(with.check().code, r.code, "both are findings runs");
}

// ---------------------------------------------------------------------------
// ADR-0068 §2 — label-root ownership.
// ---------------------------------------------------------------------------

#[test]
fn a_label_outside_the_vendor_root_is_rejected_on_stderr_and_the_rest_still_loads() {
    let p = Staged::new("root-rule");
    p.manifest(
        r#"{ "steins-plugin-api": 1,
             "labels": ["acme.cache", "io.acmecache", "email.send"],
             "effects": { "acme_cache_get": ["acme.cache"] } }"#,
    );
    let r = p.check();
    assert!(
        r.stderr.contains(
            "steins: plugin acme/steins-plugin: label 'email.send' rejected (outside vendor root)"
        ),
        "the refusal names itself on stderr:\n{}",
        r.stderr
    );
    // A refusal is a notice, never a diagnostic.
    assert!(!r.stdout.contains("email.send"), "no finding was manufactured:\n{}", r.stdout);
    // The rest of the plugin loaded: the vendor-root label and the coloring stand.
    assert_eq!(r.stdout.matches(UNKNOWN_LABEL).count(), 1, "{}", r.stdout);
    assert_eq!(declared_of(&p.annotate_json(), "warm_cache"), ["acme.cache"]);
}

#[test]
fn an_explicitly_listed_plugin_may_register_a_cross_vendor_label() {
    let p = Staged::new("allow-vouch");
    p.manifest(
        r#"{ "steins-plugin-api": 1,
             "labels": ["acme.cache", "email.send"],
             "effects": { "acme_cache_get": ["email.send"] } }"#,
    );
    p.write("steins.toml", "[plugins]\nallow = [\"acme/steins-plugin\"]\n");
    let r = p.check();
    assert!(
        !r.stderr.contains("rejected"),
        "the owner's listing is the vouching act:\n{}",
        r.stderr
    );
    assert_eq!(declared_of(&p.annotate_json(), "warm_cache"), ["email.send"]);
}

// ---------------------------------------------------------------------------
// Discovery controls.
// ---------------------------------------------------------------------------

#[test]
fn an_allow_list_replaces_discovery_rather_than_extending_it() {
    let p = Staged::new("allow-replaces");
    // The installed plugin is real and of the right type; the list names another.
    p.write("steins.toml", "[plugins]\nallow = [\"other/steins-plugin\"]\n");
    let r = p.check();
    assert_eq!(
        r.stdout.matches(UNKNOWN_LABEL).count(),
        2,
        "the unlisted plugin is not loaded, so both labels are unknown:\n{}",
        r.stdout
    );
    assert!(declared_of(&p.annotate_json(), "warm_cache").is_empty());
}

#[test]
fn an_unsupported_api_version_skips_the_plugin_and_says_so() {
    let p = Staged::new("api-version");
    p.manifest(
        r#"{ "steins-plugin-api": 2,
             "labels": ["acme.cache"],
             "effects": { "acme_cache_get": ["acme.cache"] } }"#,
    );
    let r = p.check();
    assert!(
        r.stderr
            .contains("plugin acme/steins-plugin: unsupported steins-plugin-api 2, skipped"),
        "an unrecognized bundle is reported by name:\n{}",
        r.stderr
    );
    assert_eq!(
        r.stdout.matches(UNKNOWN_LABEL).count(),
        2,
        "nothing of a skipped plugin is believed:\n{}",
        r.stdout
    );
}

// ---------------------------------------------------------------------------
// Precedence — a plugin recolors nothing that is already spoken for.
// ---------------------------------------------------------------------------

#[test]
fn a_project_function_of_the_same_name_shadows_the_plugin_coloring() {
    let p = Staged::new("shadowing");
    // The project defines the very function the plugin colors. Its own body is
    // the truth; the plugin's assertion is never reached (ADR-0068 precedence).
    p.write(
        "src/cache.php",
        concat!(
            "<?php\n\ndeclare(strict_types=1);\n\n",
            "function acme_cache_get(string $key): string\n{\n    return $key;\n}\n"
        ),
    );
    let doc = p.annotate_json();
    assert!(
        declared_of(&doc, "read_cached").is_empty(),
        "the project body answers, so no plugin label enters the lane"
    );
    // And the project's own function is proven pure and exhaustive — the plugin
    // could neither color it nor taint it.
    let own = doc["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "read_cached")
        .unwrap()
        .clone();
    assert_eq!(own["exhaustive"], true, "a resolved callee leaves no taint");
    // The envelope label itself is still registered, so it stays legal.
    let r = p.check();
    assert_eq!(r.stdout.matches(UNKNOWN_LABEL).count(), 1, "only the typo:\n{}", r.stdout);
}
