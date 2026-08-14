//! End-to-end plugin-channel contracts (issue #68, ADR-0068).
//!
//! Each test stages a mutable Composer fixture. Its plugin registers
//! `acme.cache` and assigns it to `acme_cache_get`; the application exercises
//! propagation, declaration, and typo handling.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Spawns with `GITHUB_ACTIONS` scrubbed so `check`'s CI auto-detection
/// (ADR-0054 §6) doesn't emit workflow commands where a test expects text.
/// Detection itself is tested in `tests/format_github.rs`.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("plugin_proj")
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

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

    fn manifest(&self, contents: &str) {
        self.write("vendor/acme/steins-plugin/steins-plugin.json", contents);
    }

    fn run(&self, args: &[&str]) -> Run {
        let mut argv: Vec<&str> = args.to_vec();
        argv.push("src");
        let out =
            steins_cmd().args(&argv).current_dir(&self.0).output().expect("run steins");
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn check(&self) -> Run {
        self.run(&["check", "--no-php"])
    }

    fn annotate_json(&self) -> serde_json::Value {
        let out = steins_cmd()
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
    // Suggestions include plugin-registered labels, not only built-ins.
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

#[test]
fn a_plugin_coloring_reaches_the_callers_declared_lane_without_discharging_taint() {
    let p = Staged::new("declared");
    let doc = p.annotate_json();
    assert_eq!(declared_of(&doc, "read_cached"), ["acme.cache"]);
    assert_eq!(declared_of(&doc, "warm_cache"), ["acme.cache"]);
    for name in ["read_cached", "warm_cache"] {
        let f = doc["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap();
        // Unchecked plugin assertions remain declared and non-exhaustive (ADR-0068 §1).
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

    let doc = without.annotate_json();
    assert!(declared_of(&doc, "read_cached").is_empty());
    assert!(declared_of(&doc, "warm_cache").is_empty());

    assert_eq!(with.check().code, r.code, "both are findings runs");
}

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
    // Rejected labels are notices, not diagnostics.
    assert!(!r.stdout.contains("email.send"), "no finding was manufactured:\n{}", r.stdout);
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

#[test]
fn an_allow_list_replaces_discovery_rather_than_extending_it() {
    let p = Staged::new("allow-replaces");
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

#[test]
fn a_project_function_of_the_same_name_shadows_the_plugin_coloring() {
    let p = Staged::new("shadowing");
    // Project definitions override plugin colorings (ADR-0068).
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
    let own = doc["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "read_cached")
        .unwrap()
        .clone();
    assert_eq!(own["exhaustive"], true, "a resolved callee leaves no taint");
    // Shadowing a coloring does not unregister its label.
    let r = p.check();
    assert_eq!(r.stdout.matches(UNKNOWN_LABEL).count(), 1, "only the typo:\n{}", r.stdout);
}
