//! The tolerated-effects policy (ADR-0084): `steins.toml`'s `[effects]` table,
//! resolved once at the IO boundary and carried as project input state.
//!
//! Two halves that do very different jobs:
//!
//! * `tolerated` is **policy** — the labels the envelope judgment discharges. It
//!   is the only half that can change a verdict.
//! * `[effects.attribution]` is **fact** — what a symbol's effects are *for*. It
//!   changes no judgment on its own; it gives the policy something precise to
//!   grip, so that the same `time()` call is distinguishable by whether it
//!   arrived through a logging facade or through business logic.
//!
//! Neither half ever touches the proven lane. `time()` stays `nondet.time`, the
//! fixpoint is unchanged, and `annotate` shows every label — the tolerance lives
//! at the judgment, which is what makes it reversible per question (ADR-0084's
//! three invariants).
//!
//! # Key normalization
//!
//! Attribution keys name PHP symbols, and PHP folds the case of class, method and
//! function names, so every key is stored `trim_start_matches('\\')`-ed and
//! ASCII-lowercased — the same normalization [`crate::ProjectIndex`] uses for its
//! own FQN keys and [`crate::PluginFacts::effect_labels`] uses for global function
//! names. A key with `::` is one method; a key without is *either* a class (every
//! method of it) or a global function, and both lookups are tried, because the
//! two spellings are indistinguishable in the config and PHP lets a class and a
//! function share a name.

use std::collections::BTreeMap;

use steins_catalog::LabelRegistry;

/// One `[effects.attribution]` row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AttributionEntry {
    /// The key exactly as the config wrote it (`Monolog\Logger`), kept for
    /// messages: a notice that named the normalized `monolog\logger` would point
    /// at a line the reader never wrote.
    spelling: String,
    /// The semantic labels, sorted and deduplicated.
    labels: Vec<String>,
}

/// The `[effects]` policy of one project — carried as a [`crate::Project`] input
/// beside the layout and the plugin channel, for the same reason they are: it is
/// resolved once at the IO boundary, and a replay from the same inputs must reach
/// the same verdict (ADR-0048).
///
/// [`EffectsPolicy::none`] is the empty policy, and it is the default: a project
/// with no `[effects]` table judges exactly as it did before ADR-0084.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectsPolicy {
    /// The tolerated labels, sorted and deduplicated. Any label is admissible —
    /// transport labels included; the docs recommend tolerating attributed
    /// (semantic) labels, but bluntness is the project's right.
    tolerated: Vec<String>,
    /// Normalized symbol key → its attribution. A `BTreeMap` so iteration (and
    /// therefore every derived answer, including notice order) is deterministic.
    attribution: BTreeMap<String, AttributionEntry>,
}

impl EffectsPolicy {
    /// The empty policy: nothing tolerated, nothing attributed.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Build the policy from an already-parsed `[effects]` table: the `tolerated`
    /// list and the `attribution` key→labels rows, as written.
    ///
    /// Two rows whose keys normalize to the same symbol (`App\Log` and `app\log`)
    /// join rather than one winning — attribution is a set of claims about a
    /// symbol, and there is no reason to prefer either spelling.
    #[must_use]
    pub fn new<A>(tolerated: Vec<String>, attribution: A) -> Self
    where
        A: IntoIterator<Item = (String, Vec<String>)>,
    {
        let mut tolerated: Vec<String> =
            tolerated.into_iter().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
        tolerated.sort();
        tolerated.dedup();

        let mut rows: BTreeMap<String, AttributionEntry> = BTreeMap::new();
        for (key, labels) in attribution {
            let normalized = normalize_key(&key);
            if normalized.is_empty() {
                continue;
            }
            let entry = rows
                .entry(normalized)
                .or_insert_with(|| AttributionEntry { spelling: key.trim().to_owned(), labels: Vec::new() });
            entry.labels.extend(labels.into_iter().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()));
        }
        for entry in rows.values_mut() {
            entry.labels.sort();
            entry.labels.dedup();
        }
        Self { tolerated, attribution: rows }
    }

    /// This policy with the tolerance emptied — `steins check --no-tolerated-effects`
    /// (ADR-0084 §1's audit switch).
    ///
    /// The attribution survives, and deliberately: it is fact rather than policy,
    /// so dropping it would change what the findings *say* about themselves and
    /// not merely which of them are discharged.
    #[must_use]
    pub fn without_tolerance(&self) -> Self {
        Self { tolerated: Vec::new(), attribution: self.attribution.clone() }
    }

    /// Whether this policy could discharge anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tolerated.is_empty() && self.attribution.is_empty()
    }

    /// The tolerated labels, sorted.
    #[must_use]
    pub fn tolerated(&self) -> &[String] {
        &self.tolerated
    }

    /// Whether `label` is covered by the tolerance under ADR-0018 prefix
    /// subsumption — tolerating `io.fs` covers a proven `io.fs.write`.
    ///
    /// This is the policy half only. The built-in, unconditional `mutate.local`
    /// tolerance is not spelled here (ADR-0084 §2): it is a property of the
    /// language, not of any project's judgment call, and it lives with the
    /// envelope check that has always owned it.
    #[must_use]
    pub fn tolerates(&self, label: &str) -> bool {
        self.tolerated.iter().any(|t| steins_catalog::subsumes(t, label))
    }

    /// The attribution labels of a global/namespaced **function**, by its
    /// lowercase-normalized FQN. Empty when the function is not attributed.
    #[must_use]
    pub fn function_attribution(&self, fqn: &str) -> &[String] {
        self.labels_at(&normalize_key(fqn))
    }

    /// The attribution labels covering one **method**: the class-level row, which
    /// covers every method of the class, joined with the method's own row.
    #[must_use]
    pub fn method_attribution(&self, class_fqn: &str, method: &str) -> Vec<String> {
        let class = normalize_key(class_fqn);
        if self.attribution.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = self.labels_at(&class).to_vec();
        out.extend_from_slice(self.labels_at(&format!("{class}::{}", method.to_ascii_lowercase())));
        out.sort();
        out.dedup();
        out
    }

    /// Every label the attribution table introduces — the labels that are
    /// *project-declared* by virtue of appearing here (ADR-0084 §1), sorted and
    /// deduplicated.
    #[must_use]
    pub fn declared_labels(&self) -> Vec<String> {
        let mut out: Vec<String> =
            self.attribution.values().flat_map(|e| e.labels.iter().cloned()).collect();
        out.sort();
        out.dedup();
        out
    }

    /// The attribution keys as the config wrote them, in normalized-key order —
    /// what a caller with a symbol table checks for resolvability.
    pub fn attribution_keys(&self) -> impl Iterator<Item = &str> {
        self.attribution.values().map(|e| e.spelling.as_str())
    }

    /// The label registry this policy's own labels are judged against (ADR-0084
    /// §5): `base` — builtin plus whatever the plugin channel registered —
    /// extended with every label the attribution table introduces.
    ///
    /// The view is for **validating the policy**, and stops there. It is
    /// deliberately not the registry inference asks: a label a project attributes
    /// is not thereby a label its docblocks may declare, and letting `[effects]`
    /// register vocabulary would open a second door past ADR-0068 §2's root rule.
    #[must_use]
    pub fn registry_view(&self, base: &LabelRegistry) -> LabelRegistry {
        LabelRegistry::with_extensions(
            base.extensions().iter().cloned().chain(self.declared_labels()),
        )
    }

    /// Load-time complaints about the policy's own vocabulary (ADR-0084 §5), in
    /// deterministic order: one line per `tolerated` entry or attribution value
    /// the [`Self::registry_view`] does not know, carrying the same
    /// nearest-suggestion the declared-label check gives a typo'd envelope.
    ///
    /// Empty for a well-spelled policy — including one whose only labels are its
    /// own attribution values, which the view knows precisely because they are
    /// written there.
    ///
    /// An unknown label is reported and **kept**. It subsumes nothing in the
    /// taxonomy, so it discharges nothing, and the error is therefore in the
    /// direction that reports more findings rather than fewer: worth a word on
    /// stderr, never worth refusing to run.
    #[must_use]
    pub fn label_notices(&self, base: &LabelRegistry) -> Vec<String> {
        let view = self.registry_view(base);
        let mut out = Vec::new();
        let mut complain = |label: &str, where_: &str| {
            if view.is_known(label) {
                return;
            }
            let suggestion = steins_catalog::retired_label(label)
                .map(|r| format!(" — write {}", r.guidance))
                .or_else(|| view.nearest(label).map(|s| format!(" — did you mean '{s}'?")))
                .unwrap_or_default();
            out.push(format!("steins.toml {where_}: unknown effect label '{label}'{suggestion}"));
        };
        for label in &self.tolerated {
            complain(label, "[effects] tolerated");
        }
        for entry in self.attribution.values() {
            for label in &entry.labels {
                complain(label, &format!("[effects.attribution] \"{}\"", entry.spelling));
            }
        }
        out
    }

    /// The labels stored at an already-normalized key.
    fn labels_at(&self, key: &str) -> &[String] {
        self.attribution.get(key).map_or(&[], |e| e.labels.as_slice())
    }
}

/// One attribution key as it is stored and looked up: no leading `\`, ASCII
/// lowercase, surrounding space trimmed.
fn normalize_key(key: &str) -> String {
    key.trim().trim_start_matches('\\').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> EffectsPolicy {
        EffectsPolicy::new(
            vec!["telemetry".to_owned()],
            vec![
                ("Monolog\\Logger".to_owned(), vec!["telemetry".to_owned()]),
                ("App\\Log::debug".to_owned(), vec!["telemetry".to_owned()]),
                ("app_trace".to_owned(), vec!["telemetry".to_owned()]),
            ],
        )
    }

    #[test]
    fn keys_fold_case_and_a_leading_backslash() {
        let p = policy();
        assert_eq!(p.method_attribution("monolog\\logger", "warning"), ["telemetry"]);
        assert_eq!(p.method_attribution("\\Monolog\\Logger", "WARNING"), ["telemetry"]);
        assert_eq!(p.function_attribution("app_trace"), ["telemetry"]);
        assert_eq!(p.function_attribution("APP_TRACE"), ["telemetry"]);
    }

    #[test]
    fn a_method_key_covers_only_that_method() {
        let p = policy();
        assert_eq!(p.method_attribution("app\\log", "debug"), ["telemetry"]);
        assert!(p.method_attribution("app\\log", "audit").is_empty());
    }

    #[test]
    fn tolerance_follows_prefix_subsumption() {
        let p = EffectsPolicy::new(vec!["io.fs".to_owned()], Vec::new());
        assert!(p.tolerates("io.fs"));
        assert!(p.tolerates("io.fs.write"));
        assert!(!p.tolerates("io"), "a child does not tolerate its parent");
        assert!(!p.tolerates("io.fsx"), "subsumption is segment-aware");
    }

    #[test]
    fn dropping_the_tolerance_keeps_the_attribution() {
        let p = policy().without_tolerance();
        assert!(p.tolerated().is_empty());
        assert!(!p.tolerates("telemetry"));
        assert_eq!(p.function_attribution("app_trace"), ["telemetry"]);
    }

    #[test]
    fn an_attributed_label_validates_by_being_attributed() {
        let base = LabelRegistry::builtin();
        assert!(policy().label_notices(&base).is_empty(), "{:?}", policy().label_notices(&base));
    }

    #[test]
    fn an_unknown_tolerated_label_is_named_with_a_suggestion() {
        let p = EffectsPolicy::new(vec!["io.netw".to_owned()], Vec::new());
        let notices = p.label_notices(&LabelRegistry::builtin());
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("'io.netw'"), "{}", notices[0]);
        assert!(notices[0].contains("did you mean 'io.net'"), "{}", notices[0]);
    }

    #[test]
    fn a_retired_spelling_is_pointed_at_its_replacement() {
        let p = EffectsPolicy::new(vec!["output.header".to_owned()], Vec::new());
        let notices = p.label_notices(&LabelRegistry::builtin());
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("io.output.header"), "{}", notices[0]);
    }

    #[test]
    fn the_empty_policy_is_the_pre_adr_world() {
        let p = EffectsPolicy::none();
        assert!(p.is_empty());
        assert!(!p.tolerates("nondet.time"));
        assert!(p.function_attribution("anything").is_empty());
        assert!(p.label_notices(&LabelRegistry::builtin()).is_empty());
    }
}
