//! `check --format sarif`: SARIF 2.1.0 for code-scanning ingestion (ADR-0054
//! Part I §2, slice C2).
//!
//! # What is committed
//!
//! One `run`, `version: "2.1.0"`, the standard `$schema` URI, and the minimal
//! shape ADR-0054 §2 fixes:
//!
//! * `tool.driver` — `name`, `semanticVersion`, `informationUri`, and `rules`.
//! * `tool.driver.rules` — one `reportingDescriptor` **per id present in the
//!   displayed results**, deduped and sorted. Not the full registry, and not the
//!   surface's capture set: that set already has exactly one carrier (the
//!   baseline capture header, ADR-0050 §8), duplicating it into every SARIF
//!   invites divergence, and ingestion needs only the referenced rules.
//! * `results[]` — `ruleId`, `ruleIndex`, `level`, `message.text`, one physical
//!   location, the registry-declared facets under `properties`, and
//!   `partialFingerprints`.
//! * `run.automationDetails.id` — `steins/{profile}`, so parallel uploads under
//!   different profiles (a `default` gate beside a `contracts` debt dashboard)
//!   do not clobber each other's alert categories.
//! * `run.properties` — the same accounting envelope `json` carries. **Counts
//!   only, never entries.**
//!
//! # What is deliberately absent
//!
//! * **`suppressions`.** SARIF's suppression machinery is unused (ADR-0054
//!   §7/§13). Re-emitting baselined or inline-ignored findings as suppressed
//!   results would make the format a second suppression UI beside the three
//!   channels ADR-0023 fixes as the whole surface, and would leak the baseline's
//!   contents into every upload. Suppressed findings appear as counts in
//!   `run.properties`, exactly as `json`/`text` do, and no further.
//! * **Any level knob.** The mapping is [`crate::render::ci_level`] and nothing
//!   configures it (§13 keeps per-layer overrides deferred; a severity knob
//!   re-imports the numeric ladder through the side door).
//! * **Omission of the debug lane.** §13 refuses it outright: a fail-level dump
//!   reds the run in every format, and a SARIF log that dropped the result would
//!   show a red run with nothing explaining it.
//!
//! # The path contract, stated honestly
//!
//! Paths pass through as given — relative stays relative, absolute stays
//! absolute — with backslashes normalized to forward slashes as SARIF's
//! `artifactLocation.uri` requires. GitHub's upload wants repo-root-relative
//! paths, so the documented idiom is "invoke from the repo root with relative
//! paths"; Steins does not guess a repo root it was not shown. Output goes to
//! stdout like every format; there is no `--output` flag (redirection is the
//! shell's job).

use std::collections::BTreeMap;

use steins_infer::Diagnostic;

use crate::baseline;
use crate::render::{CheckReport, ci_level};

/// The OASIS-published schema for SARIF 2.1.0 (errata 01), which is the URI the
/// SARIF SDK itself emits and GitHub's code-scanning upload accepts. ADR-0054 §2
/// says "the standard `$schema` URI" without picking among the several mirrors
/// in circulation; this is the normative one.
const SCHEMA: &str = "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";

/// The `partialFingerprints` key. Versioned in the name, per SARIF convention,
/// so a future hash revision can ship beside this one rather than silently
/// changing what alert tracking matches on.
const FINGERPRINT_KEY: &str = "steinsFindingHash/v1";

/// Render `report` as a SARIF 2.1.0 log.
pub fn render(report: &CheckReport<'_>) -> String {
    // The rule table: the ids actually present in the displayed results, deduped
    // and sorted. `BTreeMap` gives both at once and hands back the index each
    // result's `ruleIndex` needs.
    let ids: BTreeMap<&str, usize> = {
        let mut sorted: Vec<&str> = report.displayed.iter().map(|d| d.id).collect();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.into_iter().enumerate().map(|(i, id)| (id, i)).collect()
    };
    let mut rules: Vec<serde_json::Value> = Vec::with_capacity(ids.len());
    for &id in ids.keys() {
        rules.push(serde_json::json!({
            "id": id,
            // The registry carries no prose descriptions today, so the id is
            // the description. Enrichment (`fullDescription`, `helpUri`) is
            // deferred-with-design to a docs site; the shape already carries it.
            "shortDescription": { "text": id },
            "defaultConfiguration": { "level": ci_level(id, report.surface).sarif() },
            // ADR-0050 §2's promise: the layer travels with the rule. It is
            // semantic identity, not severity — which is why it rides in
            // `properties` and never in `level`.
            "properties": { "layer": steins_infer::layer(id).map(steins_infer::Layer::as_str) },
        }));
    }

    let results: Vec<serde_json::Value> =
        report.displayed.iter().map(|d| result(d, &ids, report)).collect();

    let doc = serde_json::json!({
        "$schema": SCHEMA,
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "steins",
                    "semanticVersion": env!("CARGO_PKG_VERSION"),
                    "informationUri": env!("CARGO_PKG_REPOSITORY"),
                    "rules": rules,
                }
            },
            // Parallel uploads under different profiles must not clobber each
            // other's alert categories (ADR-0054 §2).
            "automationDetails": { "id": format!("steins/{}", report.surface.name) },
            "results": results,
            // Counts only — never entries (§7).
            "properties": {
                "profile": report.surface.name,
                "vendorSuppressed": report.accounting.vendor_suppressed,
                "suppressed": report.accounting.suppressed,
                "baselined": report.accounting.baselined,
            },
        }],
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => format!("{s}\n"),
        Err(e) => {
            errln!("steins: failed to serialize sarif: {e}");
            String::new()
        }
    }
}

/// One displayed finding as a SARIF `result`.
fn result(
    d: &Diagnostic,
    ids: &BTreeMap<&str, usize>,
    report: &CheckReport<'_>,
) -> serde_json::Value {
    let uri = uri(&d.path);
    let mut obj = serde_json::json!({
        "ruleId": d.id,
        "ruleIndex": ids[d.id],
        "level": ci_level(d.id, report.surface).sarif(),
        // Verbatim: message wording is not a contract (ADR-0023) — the id is.
        "message": { "text": d.message },
        "locations": [{
            "physicalLocation": {
                "artifactLocation": { "uri": uri },
                // The same 1-based numbers `text` and `json` print. `columnKind`
                // is left at the SARIF default: if ingestion ever renders a
                // divergence that gets a recorded fix, not a preemptive guess.
                "region": { "startLine": d.line, "startColumn": d.column },
            }
        }],
    });
    // Registry-declared facets ride `properties` (`"origin": "direct"`),
    // mirroring `json` (ADR-0050 §4).
    if let Some(facet) = d.facet {
        obj["properties"] = serde_json::json!({ facet.key(): facet.value() });
    }
    // `partialFingerprints`: the ADR-0022 baseline hash of the flagged line's
    // neighborhood. The hash exists precisely so identity survives unrelated
    // edits; handing it to code scanning gives alert tracking the same stability
    // the baseline already has — one identity function, two consumers, zero new
    // machinery.
    //
    // It is computed over the path this document *shows* (the diagnostic path,
    // normalized), not over a baseline-relative path: the baseline's own
    // relativization is a function of where the baseline file sits, and a
    // fingerprint that moved because `--baseline` pointed elsewhere would defeat
    // the stability it exists for. Under the documented CI idiom — invoke from
    // the repo root with relative paths — the two coincide anyway.
    //
    // Omitted rather than faked when the source text is unavailable: a hash over
    // an empty neighborhood is the same value for every such finding, which is a
    // collision, not a fingerprint.
    if let Some(text) = report.texts.get(&d.path) {
        let hash = baseline::entry_hash(d.id, &uri, text, d.line);
        obj["partialFingerprints"] = serde_json::json!({ FINGERPRINT_KEY: hash });
    }
    obj
}

/// A diagnostic path as a SARIF `artifactLocation.uri`: as given, with
/// backslashes normalized to forward slashes.
fn uri(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_pass_through_with_forward_slashes() {
        assert_eq!(uri("src/Greeter.php"), "src/Greeter.php");
        assert_eq!(uri("./src/Greeter.php"), "./src/Greeter.php");
        assert_eq!(uri("/abs/src/Greeter.php"), "/abs/src/Greeter.php");
        assert_eq!(uri("src\\Greeter.php"), "src/Greeter.php");
    }
}
