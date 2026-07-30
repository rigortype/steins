//! The `steins-effects-baseline.json` channel (issue #69, ADR-0067): the effect
//! summaries a project had at capture time, and the delta a later run reports
//! against them.
//!
//! # Why a second file
//!
//! This is deliberately NOT the `.steins-baseline.jsonl` diagnostic channel
//! (ADR-0022) and shares nothing with it but its file-IO idioms. That file records
//! *findings to suppress*; this one records *facts to compare*, produces no
//! suppression, and gates no exit code. Folding them together would make one file
//! answer two questions with two lifecycles — the diagnostic baseline is rewritten
//! when debt is paid down, the effect baseline whenever the review story wants a
//! new "before".
//!
//! # Format
//!
//! One JSON object: a `version` field, then a `functions` array sorted by
//! `(file, symbol)` for diff stability. Each entry carries the sorted `proven`
//! labels, the sorted `declared` bounds (the normalized display set — a bound the
//! proven lane already subsumes is not stored, exactly as it is not rendered), and
//! the `exhaustive` bit. `file` is relative to the baseline file's directory,
//! forward slashes; `symbol` is the namespace-qualified name
//! (`App\Checkout::confirm`).
//!
//! # The comparison universe
//!
//! Only a function present on **both** sides is compared. A key on one side alone
//! is silent — a renamed or deleted function must never manufacture a "removed
//! effect" claim, since its effects did not go anywhere, its name did (issue #69's
//! acceptance criterion). Those keys are counted in a one-line footer and never
//! itemized.
//!
//! # What the categories mean (ADR-0067 §2.6)
//!
//! * A **proven** label that appeared is always confident: an occurrence exists
//!   now that did not exist before. This is the headline event.
//! * A **proven** label that vanished is only confident when the *current* summary
//!   is exhaustive. A non-exhaustive summary says "these effects, and possibly
//!   more" — it cannot prove an absence, so the candidate is reported hedged.
//! * A label that left the **declared** lane and appeared in the **proven** one is
//!   a *materialization*: one event, not a removal plus an addition. The bound was
//!   always there; the code now demonstrably does it.
//! * Declared-lane additions and removals are bound changes and say so.
//! * Exhaustiveness transitions are their own category, never folded into a label
//!   event.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The default effect-baseline filename, resolved like the diagnostic baseline's
/// (relative to the working directory unless `--baseline` names another path).
pub const DEFAULT_FILE: &str = "steins-effects-baseline.json";

/// The on-disk format version. Bumped when the entry shape changes; a file whose
/// version this build does not know is refused rather than misread.
pub const VERSION: u32 = 1;

/// One captured function summary. Field order is the on-disk key order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the baseline file's directory, forward slashes.
    pub file: String,
    /// The namespace-qualified symbol: `App\Checkout::confirm`, `App\render_page`.
    pub symbol: String,
    /// Sorted proven labels — occurrences.
    pub proven: Vec<String>,
    /// Sorted declared labels — bounds, already normalized against `proven`.
    pub declared: Vec<String>,
    pub exhaustive: bool,
}

/// The whole file: a version and the sorted entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    #[serde(rename = "steins-effects-baseline")]
    pub version: u32,
    pub functions: Vec<Entry>,
}

impl Document {
    #[must_use]
    pub fn new(mut functions: Vec<Entry>) -> Self {
        functions.sort_by(|a, b| {
            (a.file.as_str(), a.symbol.as_str()).cmp(&(b.file.as_str(), b.symbol.as_str()))
        });
        Self { version: VERSION, functions }
    }
}

/// Serialize a capture to the file's text (pretty-printed, trailing newline): a
/// machine-managed file that still reviews cleanly in a pull request.
#[must_use]
pub fn render(functions: Vec<Entry>) -> String {
    let doc = Document::new(functions);
    // A derived-struct serialize never fails.
    let mut text = serde_json::to_string_pretty(&doc).expect("serialize effect baseline");
    text.push('\n');
    text
}

/// Parse an effect-baseline file. `Err` carries a human message for an unreadable
/// shape or an unknown version — unlike the diagnostic baseline's hand-edit
/// tolerance, a malformed file here has no partial reading that is honest: a
/// dropped entry would silently shrink the comparison universe and read as
/// "unchanged".
pub fn parse(text: &str) -> Result<Document, String> {
    let doc: Document =
        serde_json::from_str(text).map_err(|e| format!("cannot parse effect baseline: {e}"))?;
    if doc.version != VERSION {
        return Err(format!(
            "effect baseline version {} is not readable by this build (expected {VERSION})",
            doc.version
        ));
    }
    Ok(doc)
}

/// What kind of change one event records. The order is the report's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// A proven label appeared: an occurrence that was not there before.
    ProvenAdded,
    /// A label left the declared lane and entered the proven one (ADR-0067 §2.6).
    Materialized,
    /// A proven label vanished, with an exhaustive current summary to prove it.
    ProvenRemoved,
    /// A proven label vanished, but the current summary is non-exhaustive.
    ProvenRemovedMaybe,
    /// A declared bound appeared.
    DeclaredAdded,
    /// A declared bound vanished.
    DeclaredRemoved,
    /// Exhaustive → non-exhaustive.
    CoverageNarrowed,
    /// Non-exhaustive → exhaustive.
    CoverageCompleted,
}

impl Category {
    /// The stable JSON spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProvenAdded => "proven-added",
            Self::Materialized => "declared-materialized",
            Self::ProvenRemoved => "proven-removed",
            Self::ProvenRemovedMaybe => "proven-removed-maybe",
            Self::DeclaredAdded => "declared-added",
            Self::DeclaredRemoved => "declared-removed",
            Self::CoverageNarrowed => "coverage-narrowed",
            Self::CoverageCompleted => "coverage-completed",
        }
    }
}

/// One reported change to one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub file: String,
    pub symbol: String,
    pub category: Category,
    /// The label the event is about; `None` for an exhaustiveness transition.
    pub label: Option<String>,
}

impl Event {
    /// The text-format line: `<file> <symbol>: <marker> <label>`. The marker set
    /// keeps the headline literal — "this refactor added `io.net.http` to
    /// `Checkout::confirm`" reads off the `+` line directly.
    #[must_use]
    pub fn line(&self) -> String {
        let label = self.label.as_deref().unwrap_or("");
        let body = match self.category {
            Category::ProvenAdded => format!("+ {label}"),
            Category::Materialized => format!("≤→ {label} (declared bound now proven)"),
            Category::ProvenRemoved => format!("- {label}"),
            Category::ProvenRemovedMaybe => {
                format!("? - {label} (possibly removed; current summary non-exhaustive)")
            }
            Category::DeclaredAdded => format!("+ ≤{label} (declared)"),
            Category::DeclaredRemoved => format!("- ≤{label} (declared)"),
            Category::CoverageNarrowed => "coverage narrowed (exhaustive → non-exhaustive)".to_owned(),
            Category::CoverageCompleted => {
                "coverage completed (non-exhaustive → exhaustive)".to_owned()
            }
        };
        format!("{} {}: {body}", self.file, self.symbol)
    }
}

/// The whole comparison: the itemized events plus the footer counts for the keys
/// that were compared by nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub events: Vec<Event>,
    /// Functions the current run found that the baseline does not carry.
    pub not_in_baseline: usize,
    /// Baseline functions the current run did not find.
    pub no_longer_present: usize,
    /// Functions present on both sides — the comparison universe.
    pub compared: usize,
}

/// Compare a captured baseline against the current run's entries.
///
/// # Subsumption is deliberately not collapsed
///
/// Baseline proven `io` against current proven `io.net.http` is reported as an
/// **add** of `io.net.http` and a remove-candidate of `io` (subject to the
/// exhaustive gate), not as a single "refined" event. Two reasons to keep it
/// literal in this slice: a hierarchy-aware diff has to decide whether a coarse
/// label narrowing is even the same fact — it usually is not, since `io` most
/// often came from a different call site than `io.net.http` — and a wrong
/// collapse *hides* an addition, which is the one event this surface exists to
/// show. Smarter matching can land later; it cannot land by accident.
#[must_use]
pub fn diff(baseline: &[Entry], current: &[Entry]) -> Diff {
    let index = |entries: &[Entry]| -> BTreeMap<(String, String), Entry> {
        let mut map = BTreeMap::new();
        for e in entries {
            // First writing wins: a duplicate key (the same qualified symbol twice
            // in one file) has no honest second reading, and dropping the later one
            // keeps capture and comparison agreeing.
            map.entry((e.file.clone(), e.symbol.clone())).or_insert_with(|| e.clone());
        }
        map
    };
    let base = index(baseline);
    let cur = index(current);

    let mut events = Vec::new();
    let mut compared = 0usize;
    for (key, b) in &base {
        // The rename/delete silence rule: a key on one side only is not an effect
        // change at all, so it never reaches a category.
        let Some(c) = cur.get(key) else { continue };
        compared += 1;
        events.extend(compare_one(b, c));
    }
    let not_in_baseline = cur.keys().filter(|k| !base.contains_key(*k)).count();
    let no_longer_present = base.keys().filter(|k| !cur.contains_key(*k)).count();

    events.sort_by(|a, b| {
        (a.file.as_str(), a.symbol.as_str(), a.category, a.label.as_deref()).cmp(&(
            b.file.as_str(),
            b.symbol.as_str(),
            b.category,
            b.label.as_deref(),
        ))
    });
    Diff { events, not_in_baseline, no_longer_present, compared }
}

/// Every event for one function compared against its own past.
fn compare_one(b: &Entry, c: &Entry) -> Vec<Event> {
    let bp: BTreeSet<&str> = b.proven.iter().map(String::as_str).collect();
    let cp: BTreeSet<&str> = c.proven.iter().map(String::as_str).collect();
    let bd: BTreeSet<&str> = b.declared.iter().map(String::as_str).collect();
    let cd: BTreeSet<&str> = c.declared.iter().map(String::as_str).collect();

    // The materialized set first: it *consumes* both halves of what would otherwise
    // read as a declared removal plus a proven addition (ADR-0067 §2.6 — one event).
    let materialized: BTreeSet<&str> =
        bd.iter().copied().filter(|l| !bp.contains(l) && cp.contains(l)).collect();

    let emit = |category, label: &str| Event {
        file: c.file.clone(),
        symbol: c.symbol.clone(),
        category,
        label: Some(label.to_owned()),
    };

    let mut events: Vec<Event> = Vec::new();
    for l in cp.difference(&bp) {
        if materialized.contains(l) {
            events.push(emit(Category::Materialized, l));
        } else {
            events.push(emit(Category::ProvenAdded, l));
        }
    }
    for l in bp.difference(&cp) {
        // The honesty gate: only an exhaustive *current* summary can claim an
        // absence. Non-exhaustive means "these effects, and possibly more", which
        // is exactly the state in which a missing label proves nothing.
        let category =
            if c.exhaustive { Category::ProvenRemoved } else { Category::ProvenRemovedMaybe };
        events.push(emit(category, l));
    }
    for l in cd.difference(&bd) {
        events.push(emit(Category::DeclaredAdded, l));
    }
    for l in bd.difference(&cd) {
        // A materialized label left the declared lane by becoming proven; that is
        // already one reported event and must not double as a bound removal.
        if !materialized.contains(l) {
            events.push(emit(Category::DeclaredRemoved, l));
        }
    }
    if b.exhaustive != c.exhaustive {
        let category =
            if c.exhaustive { Category::CoverageCompleted } else { Category::CoverageNarrowed };
        events.push(Event {
            file: c.file.clone(),
            symbol: c.symbol.clone(),
            category,
            label: None,
        });
    }
    events
}

/// The one-line footer, or `None` when every key was on both sides.
#[must_use]
pub fn footer(diff: &Diff) -> Option<String> {
    (diff.not_in_baseline > 0 || diff.no_longer_present > 0).then(|| {
        format!(
            "{} functions not in baseline, {} no longer present",
            diff.not_in_baseline, diff.no_longer_present
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{Category, Entry, diff, footer, parse, render};

    fn entry(symbol: &str, proven: &[&str], declared: &[&str], exhaustive: bool) -> Entry {
        Entry {
            file: "a.php".into(),
            symbol: symbol.into(),
            proven: proven.iter().map(|s| (*s).to_owned()).collect(),
            declared: declared.iter().map(|s| (*s).to_owned()).collect(),
            exhaustive,
        }
    }

    #[test]
    fn identical_captures_produce_no_events() {
        let e = entry("Checkout::confirm", &["io.db"], &[], true);
        let d = diff(std::slice::from_ref(&e), std::slice::from_ref(&e));
        assert!(d.events.is_empty(), "got: {:?}", d.events);
        assert_eq!(d.compared, 1);
        assert_eq!(footer(&d), None);
    }

    #[test]
    fn a_proven_addition_is_the_headline() {
        let b = entry("Checkout::confirm", &[], &[], true);
        let c = entry("Checkout::confirm", &["io.net.http"], &[], true);
        let d = diff(&[b], &[c]);
        assert_eq!(d.events.len(), 1);
        assert_eq!(d.events[0].category, Category::ProvenAdded);
        assert_eq!(d.events[0].line(), "a.php Checkout::confirm: + io.net.http");
    }

    #[test]
    fn a_removal_needs_an_exhaustive_current_side() {
        let b = entry("f", &["io.db"], &[], true);
        let confident = diff(std::slice::from_ref(&b), &[entry("f", &[], &[], true)]);
        assert_eq!(confident.events[0].category, Category::ProvenRemoved);
        assert_eq!(confident.events[0].line(), "a.php f: - io.db");

        // Non-exhaustive current side: the same candidate, hedged, plus its own
        // coverage event — the two are never folded together.
        let hedged = diff(&[b], &[entry("f", &[], &[], false)]);
        let cats: Vec<Category> = hedged.events.iter().map(|e| e.category).collect();
        assert!(cats.contains(&Category::ProvenRemovedMaybe), "got: {cats:?}");
        assert!(cats.contains(&Category::CoverageNarrowed));
        assert!(!cats.contains(&Category::ProvenRemoved), "no confident claim, got: {cats:?}");
    }

    #[test]
    fn a_declared_bound_becoming_proven_is_one_event() {
        let b = entry("Checkout::confirm", &[], &["io.db"], true);
        let c = entry("Checkout::confirm", &["io.db"], &[], true);
        let d = diff(&[b], &[c]);
        assert_eq!(d.events.len(), 1, "one materialization, not remove+add: {:?}", d.events);
        assert_eq!(d.events[0].category, Category::Materialized);
        assert_eq!(
            d.events[0].line(),
            "a.php Checkout::confirm: ≤→ io.db (declared bound now proven)"
        );
    }

    #[test]
    fn subsumption_is_not_collapsed() {
        // Baseline `io`, current `io.net.http`: an add and a remove-candidate, kept
        // literal on purpose (see `diff`'s docs).
        let b = entry("f", &["io"], &[], true);
        let c = entry("f", &["io.net.http"], &[], true);
        let d = diff(&[b], &[c]);
        let cats: Vec<Category> = d.events.iter().map(|e| e.category).collect();
        assert_eq!(cats, vec![Category::ProvenAdded, Category::ProvenRemoved]);
    }

    #[test]
    fn a_renamed_function_is_silent_but_counted() {
        let b = entry("oldName", &["io.db"], &[], true);
        let c = entry("newName", &["io.db"], &[], true);
        let d = diff(&[b], &[c]);
        assert!(d.events.is_empty(), "no removal noise, got: {:?}", d.events);
        assert_eq!(d.compared, 0);
        assert_eq!(footer(&d).unwrap(), "1 functions not in baseline, 1 no longer present");
    }

    #[test]
    fn the_file_round_trips() {
        let text = render(vec![entry("f", &["io.db"], &["output"], false)]);
        let doc = parse(&text).expect("readable");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.functions.len(), 1);
        assert_eq!(doc.functions[0].declared, vec!["output"]);
        assert!(text.contains("\"steins-effects-baseline\": 1"));
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let text = r#"{"steins-effects-baseline": 99, "functions": []}"#;
        assert!(parse(text).unwrap_err().contains("not readable"));
    }
}
