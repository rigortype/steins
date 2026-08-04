//! The plugin channel (ADR-0012 / ADR-0039 / ADR-0068): Composer-distributed
//! packages that register effect labels and color functions with them.
//!
//! # What is read
//!
//! A **manifest**, directly from Rust — `vendor/<vendor>/<package>/steins-plugin.json`:
//!
//! ```json
//! { "steins-plugin-api": 1,
//!   "labels": ["acme.cache"],
//!   "effects": { "acme_cache_get": ["acme.cache"] } }
//! ```
//!
//! No PHP runs. ADR-0039's design has the sidecar boot the project's own autoload
//! and answer `plugin(id, "declare", …)` on demand, and that remains the plan for
//! synthetic declarations and for facts only the live framework knows. But the two
//! facts read here — *which labels exist* and *which plain functions carry them* —
//! are static per installed version, so reading them out of a JSON file keeps
//! discovery deterministic, keeps `steins check --no-php` a complete answer, and
//! keeps the tracer testable without a PHP toolchain.
//!
//! # Discovery
//!
//! `vendor/composer/installed.json` names the installed packages; those of `type:
//! steins-plugin` are the candidates. A `steins.toml` `[plugins] allow = […]` list,
//! when present, **replaces** discovery with exactly those package names (ADR-0039:
//! the explicit listing wins) — and, per ADR-0068 §2, an explicitly listed plugin
//! is exempt from the vendor-root rule, because the owner's listing is the vouching
//! act.
//!
//! # What is refused, and loudly
//!
//! - An unrecognized `steins-plugin-api` → the whole plugin is skipped.
//! - A label outside the plugin's own vendor root and outside the core taxonomy
//!   (ADR-0068 §2) → that registration is skipped; the rest of the plugin loads.
//! - A coloring naming a label the plugin never registered → that coloring is
//!   skipped.
//!
//! Every refusal becomes a [`PluginFacts::notices`] line the CLI prints to stderr
//! — never a diagnostic. Plugin-supplied facts must not be able to create or
//! influence a finding (ADR-0068 §1), and that includes a finding about the plugin
//! itself: silence names itself, on the error stream.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use steins_catalog::LabelRegistry;

use crate::layout::ProjectLayout;

/// The per-package manifest file name, at the package's own root.
pub const MANIFEST_FILE: &str = "steins-plugin.json";

/// The composer `type` that makes a package a discovery candidate (ADR-0039).
pub const PLUGIN_TYPE: &str = "steins-plugin";

/// The only `steins-plugin-api` value this build understands (ADR-0039). A newer
/// one is not loaded and is reported by name.
pub const SUPPORTED_API: u64 = 1;

/// Everything the plugin channel contributed to one project — carried as a
/// [`crate::Project`] input beside the layout, for the same reason the layout is:
/// it is resolved once at the IO boundary, and a replay from the same inputs must
/// reach the same verdict (ADR-0048).
///
/// [`PluginFacts::none`] is the empty channel, and it is the default: a project
/// with no plugin loads no registrations and infers exactly as if the channel
/// were absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginFacts {
    /// The label registry this project's inference asks — builtin labels plus the
    /// accepted registrations.
    registry: LabelRegistry,
    /// Lowercased global function name → the labels a plugin colors it with. A
    /// `BTreeMap` so iteration (and therefore every derived answer) is ordered.
    effects: BTreeMap<String, Vec<String>>,
    /// Load-time refusals, in discovery order, for the CLI to print to stderr.
    notices: Vec<String>,
}

impl PluginFacts {
    /// The empty channel: no plugin, builtin labels only.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The label registry inference asks (builtin + registered extensions).
    #[must_use]
    pub fn registry(&self) -> &LabelRegistry {
        &self.registry
    }

    /// The labels a plugin colors the global function `name` with, if any.
    /// Case-insensitive, as PHP function names are.
    ///
    /// A hit here is an **assertion**, never a proof: the caller puts it in the
    /// declared lane and keeps the exhaustiveness taint (ADR-0068 §1).
    #[must_use]
    pub fn effect_labels(&self, name: &str) -> Option<&[String]> {
        self.effects.get(&name.to_ascii_lowercase()).map(Vec::as_slice)
    }

    /// The load-time refusals, in discovery order. The CLI prints one stderr line
    /// each; nothing here is ever a diagnostic.
    #[must_use]
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Whether the channel contributed nothing at all (no labels, no colorings).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registry.is_builtin_only() && self.effects.is_empty()
    }

    /// Discover and load the project's plugins.
    ///
    /// `allow`, when `Some`, is the `steins.toml` `[plugins] allow` list: it
    /// **replaces** `installed.json` discovery with exactly those package names,
    /// and marks each of them explicitly vouched (ADR-0068 §2 exemption).
    ///
    /// Runs once, at the boundary, before any salsa input is set. Anything it
    /// cannot read is skipped and reported; nothing here is fatal.
    #[must_use]
    pub fn discover(layout: &ProjectLayout, allow: Option<&[String]>) -> Self {
        let mut loaded: Vec<String> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        let mut effects: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut notices: Vec<String> = Vec::new();

        for root in layout.roots() {
            for vendor in root.vendor_roots() {
                for cand in candidates(vendor, allow) {
                    // One package speaks once, however many vendor trees carry it.
                    if loaded.contains(&cand.name) {
                        continue;
                    }
                    loaded.push(cand.name.clone());
                    load(&cand, &mut labels, &mut effects, &mut notices);
                }
            }
        }

        Self { registry: LabelRegistry::with_extensions(labels), effects, notices }
    }
}

/// One package the discovery step is willing to ask.
struct Candidate {
    /// The composer package name, `vendor/package`.
    name: String,
    /// Its install directory, `<vendor-dir>/<name>`.
    dir: PathBuf,
    /// Whether the owner listed it explicitly (`steins.toml [plugins] allow`),
    /// which vouches for its identity and lifts the vendor-root rule.
    explicit: bool,
}

impl Candidate {
    /// The composer **vendor name** — the label root this plugin may open
    /// (ADR-0068 §2: packagist already adjudicates who is `acme`).
    fn vendor(&self) -> &str {
        self.name.split('/').next().unwrap_or(&self.name)
    }
}

/// The packages under one vendor directory this run will ask, sorted by name so
/// the notice order (and every derived answer) is deterministic.
fn candidates(vendor_dir: &Path, allow: Option<&[String]>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = match allow {
        // The explicit listing replaces discovery entirely: a package of type
        // `steins-plugin` that is not listed is not loaded, which is what makes
        // the list a control rather than an addition.
        Some(names) => names
            .iter()
            .map(|n| Candidate {
                name: n.clone(),
                dir: vendor_dir.join(n),
                explicit: true,
            })
            .filter(|c| c.dir.is_dir())
            .collect(),
        None => installed_plugins(vendor_dir)
            .into_iter()
            .map(|n| Candidate { dir: vendor_dir.join(&n), name: n, explicit: false })
            .collect(),
    };
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The `type: steins-plugin` package names in `vendor/composer/installed.json`.
///
/// Both shapes are read: Composer 2's `{"packages": [...]}` object and Composer 1's
/// bare array. An unreadable or unparseable file yields no candidates — a project
/// whose vendor tree cannot be introspected has no plugins, which is the pre-plugin
/// behavior.
fn installed_plugins(vendor_dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(vendor_dir.join("composer").join("installed.json"))
    else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    let packages = match json.get("packages") {
        Some(v) => v.as_array(),
        None => json.as_array(),
    };
    packages
        .into_iter()
        .flatten()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some(PLUGIN_TYPE))
        .filter_map(|p| p.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// Read one candidate's manifest and fold its accepted facts in.
fn load(
    cand: &Candidate,
    labels: &mut Vec<String>,
    effects: &mut BTreeMap<String, Vec<String>>,
    notices: &mut Vec<String>,
) {
    let mut refuse = |reason: String| notices.push(format!("plugin {}: {reason}", cand.name));

    let path = cand.dir.join(MANIFEST_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        refuse(format!("no {MANIFEST_FILE} at {}; skipped", path.display()));
        return;
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        refuse(format!("{MANIFEST_FILE} is not valid JSON; skipped"));
        return;
    };
    // The version gate first: an unrecognized bundle is not partially believed.
    match json.get("steins-plugin-api").and_then(Value::as_u64) {
        Some(SUPPORTED_API) => {}
        Some(other) => {
            refuse(format!("unsupported steins-plugin-api {other}, skipped"));
            return;
        }
        None => {
            refuse(format!("{MANIFEST_FILE} declares no steins-plugin-api, skipped"));
            return;
        }
    }

    // Label registrations, each judged against the ADR-0068 §2 root rule.
    let mut accepted: Vec<String> = Vec::new();
    for label in json.get("labels").and_then(Value::as_array).into_iter().flatten() {
        let Some(label) = label.as_str() else {
            refuse("a `labels` entry is not a string; skipped".to_owned());
            continue;
        };
        if !well_formed(label) {
            refuse(format!("label '{label}' rejected (malformed)"));
            continue;
        }
        if !root_allows(label, cand) {
            refuse(format!("label '{label}' rejected (outside vendor root)"));
            continue;
        }
        accepted.push(label.to_owned());
    }

    // Function colorings. A coloring may only name a label this plugin actually
    // registered (or refined from it), or a core taxonomy label — otherwise the
    // declared lane would carry a label no registry knows.
    for (name, value) in json.get("effects").and_then(Value::as_object).into_iter().flatten() {
        let mut colors: Vec<String> = Vec::new();
        for label in value.as_array().into_iter().flatten() {
            let Some(label) = label.as_str() else {
                refuse(format!("a color of '{name}' is not a string; skipped"));
                continue;
            };
            if steins_catalog::is_core_label(label)
                || accepted.iter().any(|a| a == label || steins_catalog::subsumes(a, label))
            {
                colors.push(label.to_owned());
            } else {
                refuse(format!(
                    "color '{label}' on '{name}' rejected (not a label this plugin registers)"
                ));
            }
        }
        if colors.is_empty() {
            continue;
        }
        colors.sort();
        colors.dedup();
        // Two plugins coloring one name: the registry is a set union (ADR-0068 §2),
        // so the colors join rather than one winning.
        effects.entry(name.to_ascii_lowercase()).or_default().extend(colors);
    }
    for colors in effects.values_mut() {
        colors.sort();
        colors.dedup();
    }
    labels.extend(accepted);
}

/// Whether `cand` may register `label` (ADR-0068 §2): a descendant of a core
/// taxonomy root, a new root equal to the plugin's composer vendor name, or
/// anything at all when the owner listed the plugin explicitly.
fn root_allows(label: &str, cand: &Candidate) -> bool {
    if cand.explicit {
        return true;
    }
    if steins_catalog::is_core_label(label) {
        return true;
    }
    let root = label.split('.').next().unwrap_or(label);
    root.eq_ignore_ascii_case(cand.vendor())
}

/// Whether `label` is a well-formed ADR-0018 dot path: non-empty segments of
/// ASCII alphanumerics, `-` or `_`. Nothing downstream parses labels beyond
/// segment splitting, so this only refuses spellings that would render as noise.
fn well_formed(label: &str) -> bool {
    !label.is_empty()
        && label.split('.').all(|seg| {
            !seg.is_empty()
                && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::GoverningRoot;

    /// A scratch project tree, removed on drop.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("steins-plugins-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// A layout whose single governing root is this tree, vendoring to `vendor/`.
        fn layout(&self) -> ProjectLayout {
            let root = GoverningRoot::new(
                self.0.join("composer.json"),
                self.0.clone(),
                vec![self.0.join("vendor")],
                Vec::new(),
            );
            ProjectLayout::new(self.0.clone(), vec![root])
        }

        /// Install one package with the given composer type and manifest text.
        fn install(&self, name: &str, ctype: &str, manifest: Option<&str>) {
            self.write(
                "vendor/composer/installed.json",
                &format!(r#"{{"packages":[{{"name":"{name}","type":"{ctype}"}}]}}"#),
            );
            if let Some(m) = manifest {
                self.write(&format!("vendor/{name}/{MANIFEST_FILE}"), m);
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const ACME: &str = r#"{"steins-plugin-api":1,"labels":["acme.cache"],
        "effects":{"acme_cache_get":["acme.cache"]}}"#;

    #[test]
    fn a_plugin_package_registers_its_label_and_coloring() {
        let t = Tree::new("basic");
        t.install("acme/steins-plugin", PLUGIN_TYPE, Some(ACME));
        let f = PluginFacts::discover(&t.layout(), None);
        assert!(f.registry().is_known("acme.cache"));
        assert!(!f.registry().is_known("acme.cach"), "the typo stays unknown");
        assert_eq!(f.effect_labels("acme_cache_get"), Some(["acme.cache".to_owned()].as_slice()));
        assert_eq!(f.effect_labels("ACME_CACHE_GET"), f.effect_labels("acme_cache_get"));
        assert!(f.notices().is_empty(), "{:?}", f.notices());
    }

    #[test]
    fn a_package_of_another_type_is_not_a_candidate() {
        let t = Tree::new("other-type");
        t.install("acme/steins-plugin", "library", Some(ACME));
        let f = PluginFacts::discover(&t.layout(), None);
        assert!(f.is_empty());
        assert!(f.notices().is_empty(), "a plain library is not a refusal");
    }

    #[test]
    fn no_vendor_tree_at_all_is_the_empty_channel() {
        let t = Tree::new("bare");
        let f = PluginFacts::discover(&t.layout(), None);
        assert!(f.is_empty());
        assert_eq!(f.registry(), &LabelRegistry::builtin());
    }

    #[test]
    fn a_label_outside_the_vendor_root_is_rejected_and_named() {
        let t = Tree::new("root-rule");
        t.install(
            "acme/steins-plugin",
            PLUGIN_TYPE,
            Some(
                r#"{"steins-plugin-api":1,"labels":["acme.cache","io.acmecache","email.send"],
                    "effects":{"acme_cache_get":["acme.cache"]}}"#,
            ),
        );
        let f = PluginFacts::discover(&t.layout(), None);
        assert!(f.registry().is_known("acme.cache"), "own vendor root");
        assert!(f.registry().is_known("io.acmecache"), "core-root descendant");
        assert!(!f.registry().is_known("email.send"), "someone else's root");
        assert_eq!(
            f.notices(),
            ["plugin acme/steins-plugin: label 'email.send' rejected (outside vendor root)"]
        );
        assert!(f.effect_labels("acme_cache_get").is_some(), "the rest of the plugin still loads");
    }

    #[test]
    fn an_explicit_allow_list_replaces_discovery_and_vouches() {
        let t = Tree::new("allow");
        // Installed and of the right type, but the list does not name it.
        t.write(
            "vendor/composer/installed.json",
            r#"{"packages":[{"name":"acme/steins-plugin","type":"steins-plugin"},
                            {"name":"other/steins-plugin","type":"steins-plugin"}]}"#,
        );
        t.write(&format!("vendor/acme/steins-plugin/{MANIFEST_FILE}"), ACME);
        t.write(
            &format!("vendor/other/steins-plugin/{MANIFEST_FILE}"),
            r#"{"steins-plugin-api":1,"labels":["email.send"],"effects":{}}"#,
        );
        let allow = ["other/steins-plugin".to_owned()];
        let f = PluginFacts::discover(&t.layout(), Some(&allow));
        assert!(f.registry().is_known("email.send"), "explicit listing vouches for a new root");
        assert!(!f.registry().is_known("acme.cache"), "the list replaces discovery");
        assert!(f.effect_labels("acme_cache_get").is_none());
        assert!(f.notices().is_empty(), "{:?}", f.notices());
    }

    #[test]
    fn an_unsupported_api_version_skips_the_whole_plugin() {
        let t = Tree::new("api");
        t.install(
            "acme/steins-plugin",
            PLUGIN_TYPE,
            Some(
                r#"{"steins-plugin-api":2,"labels":["acme.cache"],
                    "effects":{"acme_cache_get":["acme.cache"]}}"#,
            ),
        );
        let f = PluginFacts::discover(&t.layout(), None);
        assert!(f.is_empty());
        assert_eq!(
            f.notices(),
            ["plugin acme/steins-plugin: unsupported steins-plugin-api 2, skipped"]
        );
    }

    #[test]
    fn a_candidate_without_a_manifest_is_named() {
        let t = Tree::new("no-manifest");
        t.install("acme/steins-plugin", PLUGIN_TYPE, None);
        let f = PluginFacts::discover(&t.layout(), None);
        assert!(f.is_empty());
        assert_eq!(f.notices().len(), 1);
        assert!(f.notices()[0].contains("no steins-plugin.json"), "{:?}", f.notices());
    }

    #[test]
    fn a_coloring_with_an_unregistered_label_is_dropped() {
        let t = Tree::new("stray-color");
        t.install(
            "acme/steins-plugin",
            PLUGIN_TYPE,
            Some(
                r#"{"steins-plugin-api":1,"labels":["acme.cache"],
                    "effects":{"acme_send":["email.send"],"acme_cache_get":["acme.cache"]}}"#,
            ),
        );
        let f = PluginFacts::discover(&t.layout(), None);
        assert!(f.effect_labels("acme_send").is_none());
        assert!(f.effect_labels("acme_cache_get").is_some());
        assert_eq!(f.notices().len(), 1);
        assert!(f.notices()[0].contains("not a label this plugin registers"), "{:?}", f.notices());
    }
}
