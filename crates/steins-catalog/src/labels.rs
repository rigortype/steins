//! The hierarchical label registry (ADR-0018): the builtin label table
//! ([`known_labels`]), prefix subsumption ([`subsumes`]), the core roots a
//! plugin may only refine ([`core_roots`], ADR-0068), the retired spellings
//! ([`retired_label`]), and [`LabelRegistry`] — the registry as one run sees
//! it, builtins plus whatever the plugin channel registered.
//!
//! The table is the union of ADR-0018's taxonomy and every label the effect
//! tables can color a builtin with; the tests at the bottom check that the
//! effect rows, the stream narrowing included, never produce a label the
//! registry does not know.

/// The hierarchical **label registry** (ADR-0018): the set of known effect
/// labels. A declared envelope label outside this set (and not an ancestor of
/// any entry — see [`is_known_label`]) earns an `effect.unknown-label`
/// diagnostic.
///
/// The union of every label [`effect_labels`] can color a builtin with and the
/// core taxonomy roots/parents of ADR-0018. Ecosystem/private labels
/// (`io.redis`, `email.send`) are **not** here — a plugin opens the registry
/// beside it; see [`LabelRegistry`], what inference actually asks.
///
/// [`effect_labels`]: crate::effect_labels
#[must_use]
pub fn known_labels() -> &'static [&'static str] {
    BUILTIN_LABELS
}

/// The **core taxonomy roots** of ADR-0018 — the label roots Steins itself owns.
///
/// A plugin may register *descendants* of these (`io.redis`, `io.db.dynamo`),
/// so subsumption works with no new machinery; a **new root** must instead
/// equal the plugin's composer vendor name (ADR-0068 §2), which is what the
/// vendor-root rule checks against this list.
///
/// `global` is a root even though only `global.read`/`global.write` are
/// registry entries — root ownership applies to the namespace.
#[must_use]
pub fn core_roots() -> &'static [&'static str] {
    &["exit", "failure", "ffi", "global", "io", "mutate", "nondet"]
}

/// Whether `label` lies under some [`core_roots`] entry — equal to a root, or a
/// dot-path descendant of one. The ADR-0068 §2 predicate a plugin registration
/// passes when it refines Steins' own taxonomy instead of opening a new root.
#[must_use]
pub fn is_core_label(label: &str) -> bool {
    core_roots().iter().any(|&r| r == label || subsumes(r, label))
}

/// The builtin label table [`known_labels`] returns, shared with [`LabelRegistry`]
/// so the builtin-only and extended views cannot drift.
const BUILTIN_LABELS: &[&str] = {
    // Sorted; ADR-0018's taxonomy plus every label `effect_labels` uses.
    &[
        "exit",
        // Failure-cause provenance family (ADR-0042): labels value provenance,
        // not an effect. See [`failure_arms`].
        "failure",
        "failure.environment",
        "failure.input",
        "failure.resource",
        // Opaque native boundary (FFI, effects_gaps.md §3): OO-only.
        "ffi",
        "global.read",
        "global.write",
        "io",
        "io.db",
        "io.fs",
        "io.fs.read",
        "io.fs.write",
        // Ambient input channel (ADR-0083, issue #318), from
        // [`narrowed_stream_labels`]; `$_GET` reads stay `global.read`.
        "io.input",
        "io.ipc", // System-V / shared-memory IPC (effects_gaps.md §4).
        "io.net",
        "io.net.http",
        // Ambient output channel (ADR-0083); children split on `ob_start()` capture.
        "io.output",
        "io.output.buffer", // OB-layer output — the only `ob_start()`-deductible.
        "io.output.header", // HTTP header mutation (effects_gaps.md §2), outside OB.
        "io.output.stderr", // Process-fd writes, which OB cannot touch.
        "io.output.stdout",
        "io.process",
        "io.signal", // Signal delivery/handling (pcntl/posix; effects_gaps.md §1).
        "mutate",
        // By-ref out-parameter write into the calling frame's own binding
        // (ADR-0063 §2.3); non-local targets stop at parent `mutate` (ADR-0055 §1).
        "mutate.local",
        "nondet",
        "nondet.random",
        "nondet.time",
    ]
};

/// Whether `envelope_label` **subsumes** `effect_label` under ADR-0018 prefix
/// subsumption: true iff they are equal, or `effect_label` extends
/// `envelope_label` by a dot-path segment (a declared `io` admits an inferred
/// `io.net.http`). Segment-aware, so `io` does **not** subsume `iota`.
#[must_use]
pub fn subsumes(envelope_label: &str, effect_label: &str) -> bool {
    effect_label == envelope_label
        || effect_label
            .strip_prefix(envelope_label)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether a declared envelope `label` is **known** to the registry: it is a
/// registry entry, or an ancestor of one (an internal taxonomy path). Since the
/// registry already lists every internal node, the ancestor clause matters only
/// for labels finer than the registry taxonomy — `io.netw` is neither a node nor
/// an ancestor of one, so it stays unknown (→ `effect.unknown-label`), while
/// every registry root is accepted.
#[must_use]
pub fn is_known_label(label: &str) -> bool {
    known_labels().iter().any(|&k| admits(label, k))
}

/// The registry label nearest to an unknown `label`, for a typo suggestion
/// (`io.netw` → `io.net`). Returns `None` when nothing is close. The metric is a
/// simple Levenshtein distance capped so only genuinely near names suggest.
#[must_use]
pub fn nearest_label(label: &str) -> Option<&'static str> {
    nearest_of(label, known_labels().iter().copied())
}

/// Whether a registry entry `entry` makes declared `label` known: `label` is the
/// entry itself, or an ancestor path of it. The one rule [`is_known_label`] and
/// [`LabelRegistry::is_known`] share, so the builtin-only and extended views
/// cannot answer differently for the same entry.
fn admits(label: &str, entry: &str) -> bool {
    entry == label || subsumes(label, entry)
}

/// The nearest of `entries` to `label` under the capped Levenshtein metric.
fn nearest_of<'a>(label: &str, entries: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    entries
        .map(|k| (levenshtein(label, k), k))
        .filter(|&(d, _)| d <= 2)
        .min_by_key(|&(d, _)| d)
        .map(|(_, k)| k)
}

/// A label spelling this project has **retired**, paired with what to write in
/// its place. Lives beside the registry, read by both the attribute check and
/// the interop one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredLabel {
    /// The retired spelling, as unmigrated code still writes it.
    pub spelling: &'static str,
    /// What to write instead — prose, since one retirement can fan out to
    /// several new labels.
    pub guidance: &'static str,
}

/// Every label spelling Steins has retired, with replacement guidance — the
/// table [`retired_label`] looks up.
///
/// **A row is appended whenever a taxonomy node moves or is renamed**: the
/// Levenshtein suggestion of [`nearest_label`] cannot reach a rename more than
/// two edits away. The first two rows are ADR-0083's move of the ambient
/// output channel under `io` (`output` → `io.output.*`, distance 3).
const RETIRED_LABELS: &[RetiredLabel] = &[
    // ADR-0083 split `output` over three children on one question (can
    // `ob_start()` capture this?), so there is no single replacement to name.
    RetiredLabel {
        spelling: "output",
        guidance: "io.output.buffer for echo-shaped code, io.output.header for \
                   header()/setcookie(), or the umbrella io.output",
    },
    RetiredLabel { spelling: "output.header", guidance: "io.output.header" },
];

/// The retirement row for `label`, if this project retired that spelling.
#[must_use]
pub fn retired_label(label: &str) -> Option<&'static RetiredLabel> {
    RETIRED_LABELS.iter().find(|r| r.spelling == label)
}

/// Why an unrecognized label reads as an **attempt at a label** rather than as
/// human prose ([`LabelRegistry::label_intent`]). Variants are the evidence, in
/// weighing order: the first two carry something to suggest, the last two are
/// evidence of intent with nothing to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelIntent<'a> {
    /// A spelling this project retired — the strongest signal, since Steins
    /// itself once printed that name.
    Retired(&'static RetiredLabel),
    /// Within [`LabelRegistry::nearest`]'s edit cap of a known label, which is
    /// also the suggestion to print.
    Near(&'a str),
    /// Some *other* member of the same tag's label list is a recognized label —
    /// prose does not usually sit in a comma list beside a real effect label.
    KnownSibling,
    /// Two or more dot-path segments, a shape a one-word English note can't take.
    DotPath,
}

/// The label registry **as one run sees it**: the builtin table ([`known_labels`])
/// plus whatever the ADR-0012/0039 plugin channel registered (ADR-0068).
/// Inference asks this, not the free functions, so a plugin-registered label
/// stops earning `effect.unknown-label` without the builtin table growing.
///
/// [`LabelRegistry::builtin`] is the closed default view for a caller with no
/// project in hand. Extension labels are validated *before* they arrive here —
/// the ADR-0068 §2 vendor-root rule is a load-time gate in the discovery layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelRegistry {
    /// Registered extension labels, sorted and deduplicated so two runs that
    /// discovered the same plugins compare equal (a salsa input requirement).
    extensions: Vec<String>,
}

impl LabelRegistry {
    /// The builtin-only registry — what every caller without a plugin channel
    /// wants.
    #[must_use]
    pub fn builtin() -> Self {
        Self { extensions: Vec::new() }
    }

    /// The builtin registry extended with `labels` (already vendor-root checked).
    #[must_use]
    pub fn with_extensions<I: IntoIterator<Item = String>>(labels: I) -> Self {
        let mut extensions: Vec<String> = labels.into_iter().collect();
        extensions.sort();
        extensions.dedup();
        Self { extensions }
    }

    /// The registered extension labels, sorted.
    #[must_use]
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// Whether this registry has no extensions.
    #[must_use]
    pub fn is_builtin_only(&self) -> bool {
        self.extensions.is_empty()
    }

    /// [`is_known_label`] over builtins **and** extensions.
    #[must_use]
    pub fn is_known(&self, label: &str) -> bool {
        is_known_label(label) || self.extensions.iter().any(|k| admits(label, k))
    }

    /// [`nearest_label`] over builtins **and** extensions — so a typo of a
    /// registered ecosystem label suggests that label, not a core one.
    #[must_use]
    pub fn nearest(&self, label: &str) -> Option<&str> {
        let builtin = known_labels().iter().copied();
        nearest_of(label, builtin.chain(self.extensions.iter().map(String::as_str)))
    }

    /// Whether an unrecognized `label`, written in a tag whose whole label list
    /// is `siblings`, carries evidence of **label intent** — and if so, which
    /// (issue #311).
    ///
    /// `None` matters: a bare word far from every known label, alone in its
    /// list, is indistinguishable from the one-word note PHPStan lets a
    /// docblock carry, and guessing "it is a label" is what ADR-0082 refuses —
    /// `None` means *stay silent*, permanently.
    ///
    /// Callers filter out already-known labels first; this checks siblings
    /// only, not `label` itself.
    #[must_use]
    pub fn label_intent<'a>(&'a self, label: &str, siblings: &[String]) -> Option<LabelIntent<'a>> {
        if let Some(r) = retired_label(label) {
            return Some(LabelIntent::Retired(r));
        }
        if let Some(near) = self.nearest(label) {
            return Some(LabelIntent::Near(near));
        }
        if siblings.iter().any(|s| s != label && self.is_known(s)) {
            return Some(LabelIntent::KnownSibling);
        }
        let mut segments = label.split('.');
        if segments.clone().count() >= 2 && segments.all(|s| !s.is_empty()) {
            return Some(LabelIntent::DotPath);
        }
        None
    }
}

/// Plain Levenshtein edit distance (small strings, so the quadratic DP is fine).
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use crate::StreamTarget::{Constant, Literal};
    use crate::{effect_labels, narrowed_stream_labels as narrowed};
    use super::{
        LabelIntent, is_core_label, is_known_label, nearest_label, retired_label, subsumes,
    };

    #[test]
    fn io_db_is_a_registered_label() {
        assert!(is_known_label("io.db"));
        assert!(subsumes("io", "io.db"), "coarse io admits io.db");
        assert!(!subsumes("io.db", "io"), "and not the other way round");
        assert!(!subsumes("io.fs", "io.db"), "siblings do not subsume");
    }

    #[test]
    fn subsumption_is_prefix_and_segment_aware() {
        assert!(subsumes("io", "io"), "equal labels subsume");
        assert!(subsumes("io", "io.fs.write"), "coarse admits fine");
        assert!(subsumes("nondet", "nondet.random"));
        assert!(subsumes("io.fs.read", "io.fs.read"));
        assert!(!subsumes("io.fs.read", "io.fs.write"), "siblings do not subsume");
        assert!(!subsumes("io.net", "io"), "fine does not admit coarse");
        assert!(!subsumes("io", "iota"), "non-segment prefix is not subsumption");
        assert!(!subsumes("io.net", "io.netw"), "io.net does not subsume io.netw");
    }

    #[test]
    fn registry_roots_are_known() {
        for label in [
            "io.output", "io", "io.fs", "io.fs.read", "io.fs.write", "io.net", "io.net.http",
            "io.db", "io.process", "global.read", "global.write", "nondet", "nondet.random",
            "nondet.time", "exit", "mutate",
        ] {
            assert!(is_known_label(label), "{label} should be a known registry label");
        }
    }

    #[test]
    fn typos_and_private_labels_are_unknown() {
        assert!(!is_known_label("io.netw"), "typo is unknown");
        assert!(!is_known_label("email.send"), "private/plugin label is unknown for now");
        assert!(!is_known_label("nondet.rand"), "close typo still unknown");
    }

    #[test]
    fn nearest_label_suggests_the_obvious_typo() {
        assert_eq!(nearest_label("io.netw"), Some("io.net"));
        assert_eq!(nearest_label("io.outpt"), Some("io.output"));
        assert_eq!(nearest_label("completely-different"), None);
        // The retired ADR-0083 spelling is NOT a near miss (distance 3, past
        // the cap) — why `RETIRED_LABELS` exists (issue #311).
        assert_eq!(nearest_label("output"), None);
        assert_eq!(nearest_label("output.header"), None);
    }

    #[test]
    fn the_retired_table_carries_the_adr_0083_migration() {
        let out = retired_label("output").expect("the retired output root");
        assert_eq!(out.spelling, "output");
        assert_eq!(
            out.guidance,
            "io.output.buffer for echo-shaped code, io.output.header for \
             header()/setcookie(), or the umbrella io.output"
        );
        assert_eq!(retired_label("output.header").map(|r| r.guidance), Some("io.output.header"));
        assert_eq!(retired_label("io.output"), None);
        assert_eq!(retired_label("io.netw"), None);
        assert_eq!(retired_label("database"), None);
        for label in ["io.output", "io.output.buffer", "io.output.header"] {
            assert!(is_known_label(label), "{label} is named as a replacement");
        }
    }

    #[test]
    fn label_intent_tells_a_typo_from_a_humans_prose() {
        let r = super::LabelRegistry::builtin();
        let alone: Vec<String> = Vec::new();

        // THE GUARANTEE (issue #311): a bare word, far from everything, alone in
        // its list, is prose as far as this predicate is concerned — forever.
        assert_eq!(r.label_intent("database", &alone), None);
        assert_eq!(r.label_intent("todo", &alone), None);
        // (a) near a known label.
        assert_eq!(r.label_intent("io.netw", &alone), Some(LabelIntent::Near("io.net")));
        assert_eq!(r.label_intent("nondet.tyme", &alone), Some(LabelIntent::Near("nondet.time")));
        // (b) a recognized sibling in the same list turns even prose-shaped
        // `database` into evidence — the deliberately aggressive signal.
        let beside = vec!["io.db".to_owned(), "database".to_owned()];
        assert_eq!(r.label_intent("database", &beside), Some(LabelIntent::KnownSibling));
        // The sibling must be a *different* token: a list of one unknown label
        // repeated is not evidence of anything.
        let itself = vec!["database".to_owned(), "database".to_owned()];
        assert_eq!(r.label_intent("database", &itself), None);
        // (c) dot-path shape, with nothing near and no known sibling.
        assert_eq!(r.label_intent("cache.warmup", &alone), Some(LabelIntent::DotPath));
        // A trailing dot is not a second segment.
        assert_eq!(r.label_intent("database.", &alone), None);
        // (d) a retirement outranks the rest, and reaches where the metric cannot.
        let retired = r.label_intent("output", &alone).expect("the retired spelling reports");
        assert!(matches!(retired, LabelIntent::Retired(row) if row.spelling == "output"));
        // An extension label a plugin registered makes its own typos near misses.
        let plugged = super::LabelRegistry::with_extensions(["acme.cache".to_owned()]);
        assert_eq!(plugged.label_intent("acme.cach", &alone), Some(LabelIntent::Near("acme.cache")));
    }

    #[test]
    fn the_builtin_registry_answers_exactly_as_the_free_functions_do() {
        let r = super::LabelRegistry::builtin();
        assert!(r.is_builtin_only());
        for label in ["io", "io.db", "nondet.time", "exit", "mutate.local"] {
            assert_eq!(r.is_known(label), is_known_label(label), "{label}");
        }
        for label in ["io.netw", "email.send", "acme.cache"] {
            assert!(!r.is_known(label), "{label} is not in the closed set");
        }
        assert_eq!(r.nearest("io.netw"), Some("io.net"));
    }

    #[test]
    fn an_extension_label_becomes_known_without_the_builtin_table_growing() {
        let r = super::LabelRegistry::with_extensions(["acme.cache".to_owned()]);
        assert!(r.is_known("acme.cache"));
        assert!(!is_known_label("acme.cache"));
        assert!(!r.is_known("acme.cach"));
        assert_eq!(r.nearest("acme.cach"), Some("acme.cache"));
        assert!(r.is_known("acme"));
        assert!(!r.is_known("acme.cache.hit"));
    }

    #[test]
    fn core_roots_are_the_ones_a_plugin_may_only_refine() {
        // ADR-0068 §2: descendants of these are open to any plugin.
        assert!(is_core_label("io.redis"));
        assert!(is_core_label("io"));
        assert!(is_core_label("global.write"));
        assert!(!is_core_label("acme.cache"));
        assert!(!is_core_label("output"));
        assert!(is_core_label("io.output.buffer"));
        assert!(!is_core_label("email.send"));
        assert!(!is_core_label("iota.thing"));
    }

    #[test]
    fn new_effect_labels_are_registered_and_subsume() {
        for label in ["ffi", "io.signal", "io.ipc", "io.output.header", "io.input"] {
            assert!(is_known_label(label), "{label} should be a known registry label");
        }
        assert!(subsumes("io", "io.signal"), "coarse io admits io.signal");
        assert!(subsumes("io", "io.ipc"), "coarse io admits io.ipc");
        assert!(
            subsumes("io.output", "io.output.buffer"),
            "coarse io.output admits io.output.buffer"
        );
        // ADR-0083: bare `io` is the ambient channels' ancestor too.
        assert!(subsumes("io", "io.output.buffer"), "io admits the ambient output channel");
        assert!(subsumes("io", "io.input"));
        assert!(
            !subsumes("io.output.buffer", "io.output.header"),
            "headers are outside the OB-capturable family"
        );
        assert!(!subsumes("io.signal", "io.ipc"), "siblings do not subsume");
        assert!(!subsumes("io", "ffi"));
    }

    #[test]
    fn mutate_local_is_registered_under_mutate() {
        assert!(is_known_label("mutate.local"));
        assert!(subsumes("mutate", "mutate.local"), "a coarse `mutate` admits it");
        assert!(!subsumes("mutate.local", "mutate"), "and not the other way round");
    }

    #[test]
    fn every_narrowed_label_is_a_registry_entry() {
        let targets = [
            Literal("/tmp/x"), Literal("https://h/x"), Literal("ftp://h/x"), Literal("ssh2.exec://h/x"),
            Literal("unix:///s"), Literal("expect://ls"), Literal("data://text/plain,x"),
            Literal("php://output"), Literal("php://stdout"), Literal("php://stderr"),
            Literal("php://input"), Literal("php://memory"), Literal("php://temp"),
            Literal("phar:///a.phar/x"), Literal("php://filter/resource=https://h/x"),
            Constant("STDIN"), Constant("STDOUT"), Constant("STDERR"),
        ];
        let modes = [None, Some(Literal("r")), Some(Literal("w")), Some(Literal("r+"))];
        for name in ["file_get_contents", "file_put_contents", "fopen", "copy", "rename", "readfile",
                     "fpassthru", "fread", "fgets", "fwrite", "fputs", "unlink", "mkdir", "rmdir",
                     "touch", "scandir", "file_exists", "is_file", "is_dir"] {
            assert!(effect_labels(name).is_some(), "{name} must be catalogued");
            for &t in &targets {
                for &m in &modes {
                    let Some(labels) = narrowed(name, Some(t), m.or(Some(t))) else { continue };
                    for label in labels {
                        assert!(
                            super::is_known_label(label),
                            "{name}({t:?}) narrowed to unregistered label {label}"
                        );
                    }
                }
            }
        }
    }
}
