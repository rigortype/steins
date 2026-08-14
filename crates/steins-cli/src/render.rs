//! The `check` render seam (ADR-0054 Part I, slice C1).
//!
//! ADR-0054's first identity: **a format is a serialization of the displayed
//! surface, never a second surface** — the pipeline (vendor → profile → policy
//! → inline ignores → baseline, ADR-0050 §6) decides what exists, a format only
//! how it's spelled. A prior `match format { Text => print_text(…), Json =>
//! print_json(…) }` with nine positional arguments left each format free to
//! consult one more fact, drop a finding, or compute its own exit contribution,
//! with nothing in the type system objecting.
//!
//! So the boundary is a value, not a call: `run_check` builds ONE
//! [`CheckReport`] — displayed findings, active surface, fix run, accounting
//! counters — and hands it to [`render`], which returns the bytes. Every format
//! sees exactly the same report; hiding a finding would require ignoring a
//! slice it was handed, visible in review rather than hidden in an argument
//! list. The exit code is computed by `run_check` from that same report *after*
//! rendering and is never a function of the format (ADR-0050 §7's "surfaced
//! means fail" is identity — ADR-0054 §13 refuses `--exit-zero` and every other
//! format-dependent exit).
//!
//! # The formats
//!
//! * `text` — the human rendering, unchanged byte for byte.
//! * `json` — the machine document, unchanged byte for byte.
//! * `github` — GitHub Actions workflow commands (ADR-0054 §4), annotating a
//!   pull request's diff inline.
//! * `sarif` — SARIF 2.1.0 for code-scanning upload (ADR-0054 §2), in
//!   [`crate::sarif`].
//!
//! `text` and `json` moved here verbatim from `main.rs`, byte-identical by
//! construction; `tests/format_recorded.rs` pins that against recorded output.

use std::collections::HashMap;

use steins_infer::{Diagnostic, Layer};

use crate::profile;
use crate::{FixRun, sarif};

/// Which spelling of the displayed surface `check` emits.
///
/// Deliberately NOT the crate-root `Format` (shared by `annotate`, `transform`,
/// `effect-diff`): those commands render a *different* object, and ADR-0054
/// explicitly excludes "SARIF for `transform`" ("transform's report is a
/// diff/plan, not findings"). A separate enum makes `steins transform --format
/// sarif` a usage error at the parse site, not an unreachable match arm.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckFormat {
    Text,
    Json,
    Github,
    Sarif,
}

impl CheckFormat {
    /// Parse a `--format` value, or `None` for an unknown one (the caller emits
    /// the usage error and exits 2).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            "github" => Some(Self::Github),
            "sarif" => Some(Self::Sarif),
            _ => None,
        }
    }
}

/// The suppression accounting a run reports alongside its findings — counts
/// only, never entries (ADR-0054 §7: re-surfacing findings here would open a
/// fourth suppression channel beside the three ADR-0023 already fixes).
pub struct Accounting<'a> {
    pub vendor_suppressed: usize,
    pub suppressed: usize,
    pub baselined: usize,
    pub stale: usize,
    /// The ADR-0050 §8 drowns-loudly notice, when the active surface exceeds the
    /// baseline's capture surface.
    pub surface_notice: Option<&'a str>,
}

/// Everything a format may see. One value, built once by `run_check`.
pub struct CheckReport<'a> {
    /// The displayed findings, already sorted by `(path, line, column, id)`.
    pub displayed: &'a [Diagnostic],
    /// Findings `check --fix` resolved on disk — no longer findings of the code
    /// as it now stands, so they are reported apart from `displayed` and never
    /// annotated (ADR-0010).
    pub fixed: &'a [Diagnostic],
    pub fix_run: Option<&'a FixRun>,
    pub surface: &'a profile::Surface,
    pub accounting: Accounting<'a>,
    /// The analyzed sources by diagnostic path. SARIF's `partialFingerprints`
    /// reads them for the ADR-0022 baseline hash (ADR-0054 §2); the other three
    /// formats do not consult them.
    pub texts: &'a HashMap<String, String>,
}

/// Render `report` in `format`. The returned `String` is the command's entire
/// stdout — written with a single `out!`, so an empty report writes no bytes.
pub fn render(report: &CheckReport<'_>, format: CheckFormat) -> String {
    match format {
        CheckFormat::Text => text(report),
        CheckFormat::Json => json(report),
        CheckFormat::Github => github(report),
        CheckFormat::Sarif => sarif::render(report),
    }
}

// Format auto-detection (ADR-0054 §6)

/// The environment variable GitHub Actions sets on every step it runs.
const GITHUB_ACTIONS_ENV: &str = "GITHUB_ACTIONS";

/// The format for a run that passed no `--format`, read from the process
/// environment. See [`detect`] for the rule.
pub fn detect_from_env() -> CheckFormat {
    detect(std::env::var(GITHUB_ACTIONS_ENV).ok().as_deref())
}

/// Detection **detects the consumer, never the context** (ADR-0054 §6): it only
/// changes the spelling, never the surface, profile, pipeline or exit code
/// (format invariance, §1, checked by `tests/format_github.rs`).
///
/// Only `GITHUB_ACTIONS` detects. A generic `CI=true` is refused by §13 — "some
/// CI" names no rendering, so `text` stays the answer there. `sarif` is never
/// auto-selected either; it's a file artifact for a deliberate upload step, not
/// a log rendering.
///
/// A pure function of the variable's value, so the rule is unit-testable
/// without mutating the test process's environment.
pub fn detect(github_actions: Option<&str>) -> CheckFormat {
    // Case-insensitive so a runner spelling it `TRUE` still gets annotations;
    // anything else (including GitHub's own never-written `false`) stays `text`.
    match github_actions {
        Some(v) if v.eq_ignore_ascii_case("true") => CheckFormat::Github,
        _ => CheckFormat::Text,
    }
}

// The level mapping (ADR-0054 §3)

/// How a finding is spelled to a CI ingestion surface: SARIF's `level` and the
/// GitHub workflow command are the same decision under two names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CiLevel {
    Error,
    Warning,
    Note,
}

impl CiLevel {
    /// The SARIF `level` string.
    pub const fn sarif(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }

    /// The GitHub workflow command name.
    pub const fn command(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "notice",
        }
    }
}

/// The ADR-0054 §3 mapping: **the level keys on ADR-0050 §7's level, with one
/// debug-layer carve-out.**
///
/// Layer is semantic identity, not severity (ADR-0050 §1), so the exit level
/// decides, not the layer — CI level and exit code answer the same question
/// ("does CI act on this?") and must not disagree. The debug lane is carried
/// rather than omitted: ADR-0054 §13 refuses omission because a serializer that
/// dropped the annotation would show a red run with nothing explaining it. A
/// warn-level dump is an *answer to a question the code asked* (ADR-0053 §1),
/// so it takes SARIF's third level (`note`), not `warning`.
///
/// The `Layer` match is exhaustive on purpose: a future layer cannot silently
/// fall into a level. Written as the general rule behind the ADR's table
/// (debug + fail → `error`, debug + warn → `note`) rather than an id list, so
/// `debug.trace` (ADR-0074 §8, added after ADR-0054) maps correctly without an
/// amendment.
pub fn ci_level(id: &str, surface: &profile::Surface) -> CiLevel {
    let level = surface.level(id);
    match steins_infer::layer(id) {
        Some(Layer::Debug) => match level {
            profile::Level::Fail => CiLevel::Error,
            profile::Level::Warn => CiLevel::Note,
        },
        Some(Layer::Proof | Layer::Contract | Layer::Mechanics) | None => match level {
            profile::Level::Fail => CiLevel::Error,
            profile::Level::Warn => CiLevel::Warning,
        },
    }
}

fn text(report: &CheckReport<'_>) -> String {
    let mut out = String::new();
    for d in report.displayed {
        // ADR-0050 §7: fail-level prints `error[…]`, warn-level `warning[…]`.
        let kind = match report.surface.level(d.id) {
            profile::Level::Fail => "error",
            profile::Level::Warn => "warning",
        };
        out.push_str(&format!(
            "{}:{}:{}: {kind}[{}]: {}\n",
            d.path, d.line, d.column, d.id, d.message
        ));
    }
    plain_tail(&mut out, report);
    out
}

/// Everything `text` prints after the findings: the `--fix` run's report and
/// the suppression accounting. Shared with `github` (§4: plain lines are inert
/// in a workflow log, and accounting must not become format-dependent).
fn plain_tail(out: &mut String, report: &CheckReport<'_>) {
    // ADR-0010: each applied finding marked `fixed[…]`; a refusal prints its
    // named reason and the diagnostics the edits would have surfaced. Both
    // empty on a plain run.
    for d in report.fixed {
        out.push_str(&format!(
            "{}:{}:{}: fixed[{}]: {}\n",
            d.path, d.line, d.column, d.id, d.message
        ));
    }
    if let Some(r) = report.fix_run.and_then(|run| run.refusal.as_ref()) {
        out.push_str(&format!("fix refused ({}): {}\n", r.reason, r.detail));
        for d in &r.new_diagnostics {
            out.push_str(&format!(
                "  {}:{}:{}: [{}] {}\n",
                d.path, d.line, d.column, d.id, d.message
            ));
        }
    }
    // Suppression accounting (ADR-0022/0023/0015): each line prints only when
    // nonzero; vendor (ADR-0015) prints first.
    let a = &report.accounting;
    if a.vendor_suppressed > 0 {
        out.push_str(&format!(
            "{} findings in vendor suppressed (--vendor-diagnostics to show)\n",
            a.vendor_suppressed
        ));
    }
    if a.suppressed > 0 {
        out.push_str(&format!("{} diagnostics suppressed by inline ignores\n", a.suppressed));
    }
    if a.baselined > 0 {
        out.push_str(&format!("{} findings in baseline\n", a.baselined));
    }
    if a.stale > 0 {
        out.push_str(&format!(
            "{} baseline entries no longer match (stale — rerun --set-baseline)\n",
            a.stale
        ));
    }
    // Drowns-loudly notice (ADR-0050 §8), after the accounting.
    if let Some(notice) = a.surface_notice {
        out.push_str(&format!("{notice}\n"));
    }
}

// json

/// One finding as a `--format json` object. Shared by the `findings` array,
/// the `--fix` run's `fixed` array, a refusal's `new_diagnostics`, and the MCP
/// server's `check` tool, so they all spell a finding identically.
pub fn finding_json(d: &Diagnostic, surface: &profile::Surface) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "id": d.id,
        // ADR-0050 §2: additive; always present (every emitted id is registered).
        "layer": steins_infer::layer(d.id).map(steins_infer::Layer::as_str),
        // ADR-0050 §7: the exit level (`fail|warn`), additive.
        "level": surface.level(d.id).as_str(),
        "path": d.path,
        "line": d.line,
        "column": d.column,
        "message": d.message,
    });
    // ADR-0050 §4: registry-declared facet, additive — own key
    // (`"origin": "direct"|"propagated"`) only on ids that declare one.
    if let Some(facet) = d.facet {
        obj[facet.key()] = serde_json::Value::String(facet.value().to_owned());
    }
    // ADR-0010: fix payload, additive. Edit objects mirror steins-edit's `Edit`
    // serialization (`path` + `span {start, end}` + `replacement`) so a consumer
    // applies them with the same splice the transform surface speaks.
    if let Some(fix) = &d.fix {
        let edits: Vec<serde_json::Value> = fix
            .edits
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path,
                    "span": { "start": e.start, "end": e.end },
                    "replacement": e.replacement,
                })
            })
            .collect();
        obj["fix"] = serde_json::json!({ "title": fix.title, "edits": edits });
    }
    obj
}

fn json(report: &CheckReport<'_>) -> String {
    let surface = report.surface;
    let array: Vec<serde_json::Value> =
        report.displayed.iter().map(|d| finding_json(d, surface)).collect();
    let mut doc = serde_json::json!({
        "findings": array,
        "profile": surface.name,
        "vendor_suppressed": report.accounting.vendor_suppressed,
        "suppressed": report.accounting.suppressed,
        "baselined": report.accounting.baselined,
    });
    // `--fix` report, present only when the flag was passed (a plain run's
    // document is byte-identical to before).
    if let Some(run) = report.fix_run {
        let fixed_arr: Vec<serde_json::Value> =
            report.fixed.iter().map(|d| finding_json(d, surface)).collect();
        let refusal = run.refusal.as_ref().map(|r| {
            let new_ds: Vec<serde_json::Value> =
                r.new_diagnostics.iter().map(|d| finding_json(d, surface)).collect();
            serde_json::json!({
                "reason": r.reason,
                "detail": r.detail,
                "new_diagnostics": new_ds,
            })
        });
        doc["fix"] = serde_json::json!({
            "applied": run.applied,
            "fixed": fixed_arr,
            "refusal": refusal,
        });
    }
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => format!("{s}\n"),
        Err(e) => {
            // Unreachable for a `serde_json::Value` tree; kept for parity with
            // the pre-seam code.
            errln!("steins: failed to serialize json: {e}");
            String::new()
        }
    }
}

// github (ADR-0054 §4)

/// GitHub's documented escaping has **two registers** (ADR-0054 §4, fixtures in
/// `tests/format_github.rs`): the message is *data*, escaping `%`, `\r`, `\n`.
/// `%` goes first or it would re-encode the escapes it just wrote.
fn escape_data(s: &str) -> String {
    s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

/// The property register: the data escapes plus `:` and `,`, which delimit the
/// `key=value` list and the `::` that ends it.
fn escape_property(s: &str) -> String {
    escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

fn github(report: &CheckReport<'_>) -> String {
    let mut out = String::new();
    // One command per finding, no cap (ADR-0054 §5/§13 refuse a steins-side
    // truncation, which would be a silent format-keyed suppression channel).
    for d in report.displayed {
        out.push_str(&format!(
            "::{} file={},line={},col={},title={}::{}\n",
            ci_level(d.id, report.surface).command(),
            escape_property(&d.path),
            d.line,
            d.column,
            escape_property(d.id),
            escape_data(&d.message),
        ));
    }
    // Then the same plain accounting `text` prints (ADR-0054 §4). A fixed
    // finding is not annotated (it's no longer true of the code on disk), but
    // dropping the `--fix` line would leave this the one format saying nothing
    // about what it just rewrote.
    plain_tail(&mut out, report);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_names_the_consumer_not_the_context() {
        assert_eq!(detect(Some("true")), CheckFormat::Github);
        assert_eq!(detect(Some("TRUE")), CheckFormat::Github);
        assert_eq!(detect(Some("false")), CheckFormat::Text);
        assert_eq!(detect(None), CheckFormat::Text);
        // A generic CI signal names no rendering (ADR-0054 §13).
        assert_eq!(detect(Some("1")), CheckFormat::Text);
    }

    #[test]
    fn the_two_escaping_registers() {
        // Data: `%`, CR, LF.
        assert_eq!(escape_data("100% done"), "100%25 done");
        assert_eq!(escape_data("a\r\nb"), "a%0D%0Ab");
        // `%` escaped first, so input escape sequences survive as literal text.
        assert_eq!(escape_data("%0A"), "%250A");
        // Data leaves the property delimiters alone.
        assert_eq!(escape_data("a:b,c"), "a:b,c");
        // Properties additionally escape `:` and `,`.
        assert_eq!(escape_property("C:\\a,b"), "C%3A\\a%2Cb");
        assert_eq!(escape_property("100%,:"), "100%25%2C%3A");
    }

    #[test]
    fn only_the_named_formats_parse() {
        assert_eq!(CheckFormat::parse("text"), Some(CheckFormat::Text));
        assert_eq!(CheckFormat::parse("json"), Some(CheckFormat::Json));
        assert_eq!(CheckFormat::parse("github"), Some(CheckFormat::Github));
        assert_eq!(CheckFormat::parse("sarif"), Some(CheckFormat::Sarif));
        assert_eq!(CheckFormat::parse("Text"), None);
        assert_eq!(CheckFormat::parse("gitlab"), None);
    }
}
