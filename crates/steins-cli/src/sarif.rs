//! `check --format sarif`: SARIF 2.1.0 for code-scanning ingestion (ADR-0054
//! Part I §2, slice C2).
//!
//! # What is committed
//!
//! One `run`, `version: "2.1.0"`, the standard `$schema` URI:
//!
//! * `tool.driver` — `name`, `semanticVersion`, `informationUri`, `rules`.
//! * `tool.driver.rules` — one `reportingDescriptor` per id in the displayed
//!   results (deduped, sorted), not the full registry.
//! * `results[]` — `ruleId`, `ruleIndex`, `level`, `message.text`, one physical
//!   location, registry-declared facets under `properties`, `partialFingerprints`.
//! * `run.automationDetails.id` — `steins/{profile}`, so parallel uploads under
//!   different profiles don't clobber each other's alert categories.
//! * `run.properties` — the same accounting envelope `json` carries. Counts
//!   only, never entries.
//!
//! # What is deliberately absent
//!
//! * **`suppressions`** — unused (ADR-0054 §7/§13): re-emitting baselined or
//!   ignored findings as suppressed results would open a second suppression UI
//!   beside the three channels ADR-0023 already fixes, and would leak baseline
//!   contents into every upload. Suppressed findings appear only as counts.
//! * **Any level knob** — the mapping is [`crate::render::ci_level`]; §13 keeps
//!   per-layer overrides deferred, so nothing configures it here.
//! * **The debug lane** — §13 refuses it: a SARIF log that dropped the
//!   fail-level result would show a red run with nothing explaining it.
//!
//! # The path contract
//!
//! Paths pass through as given, backslashes normalized to forward slashes
//! (SARIF's `artifactLocation.uri`). GitHub wants repo-root-relative paths, so
//! the documented idiom is "invoke from the repo root"; Steins does not guess a
//! repo root it wasn't shown. Output goes to stdout like every format — no
//! `--output` flag.

use std::collections::BTreeMap;

use steins_infer::Diagnostic;

use crate::baseline;
use crate::render::{CheckReport, ci_level};

/// The normative SARIF 2.1.0 (errata 01) `$schema` URI (ADR-0054 §2), among
/// several mirrors in circulation — the one the SARIF SDK and GitHub emit/accept.
const SCHEMA: &str = "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";

/// The `partialFingerprints` key, versioned in the name so a future hash
/// revision can ship beside this one without silently changing alert matching.
const FINGERPRINT_KEY: &str = "steinsFindingHash/v1";

/// Render `report` as a SARIF 2.1.0 log.
pub fn render(report: &CheckReport<'_>) -> String {
    // Ids present in the displayed results, deduped/sorted; BTreeMap also hands
    // back each result's `ruleIndex`.
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
            // No prose descriptions in the registry today, so id doubles as one;
            // `fullDescription`/`helpUri` enrichment is deferred to a docs site.
            "shortDescription": { "text": id },
            "defaultConfiguration": { "level": ci_level(id, report.surface).sarif() },
            // ADR-0050 §2: layer travels with the rule as identity, not severity —
            // hence `properties`, never `level`.
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
                // Same 1-based numbers as `text`/`json`. `columnKind` stays at
                // the SARIF default until a real divergence forces a decision.
                "region": { "startLine": d.line, "startColumn": d.column },
            }
        }],
    });
    // Registry-declared facets ride `properties`, mirroring `json` (ADR-0050 §4).
    if let Some(facet) = d.facet {
        obj["properties"] = serde_json::json!({ facet.key(): facet.value() });
    }
    // `partialFingerprints`: the ADR-0022 baseline hash of the flagged line's
    // neighborhood, reused so code-scanning alert tracking gets the baseline's
    // stability for free. Hashed over the diagnostic's own (normalized) path,
    // not a baseline-relative one, so a fingerprint doesn't move just because
    // `--baseline` pointed elsewhere — under the documented "run from repo root"
    // idiom the two paths coincide anyway. Omitted (not faked) when source text
    // is unavailable, since a hash of an empty neighborhood would just collide.
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
