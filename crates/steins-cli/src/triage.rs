//! `steins triage` (issue #50, ADR-0095): the adoption instrument. It reads a
//! `check --format json` document — from `--input`, or from a `check` run it
//! starts itself over `<paths>` — and aggregates it into one report: totals,
//! the per-id distribution, the per-file hotspots, the offset family's
//! guard-status buckets, and a short list of hints.
//!
//! Three properties are the design, and every choice below follows from one
//! of them:
//!
//! * **Pure aggregation over the stream.** The only input is the JSON
//!   document `check` already emits; this module names no analyzer type and
//!   asks the analyzer no question. When given paths it runs the binary's own
//!   `check --format json` as a child process and reads its stdout — ONE
//!   analysis pass, the same one a user would have run, with `check`'s code
//!   untouched. What the stream does not carry, triage reports as
//!   "not measured" rather than reaching past the schema for.
//! * **Data model first, renderers second.** [`aggregate`] builds a
//!   [`TriageReport`]; [`render_text`] and [`render_json`] spell the same value.
//!   `--format json` is the report serialized verbatim, so a consumer reads
//!   exactly what the text reader sees.
//! * **A measurement, never a gate.** Hints are advice and buckets are counts;
//!   neither is a finding, and the command exits `0` on every successful run
//!   whatever the counts say. A what-if over a surface the project has not
//!   enabled (`--profile strict`) therefore changes nothing about `check`'s
//!   behavior or exit code — it is measured here, judged by a person, and
//!   enabled (then baselined) in `steins.toml` on that evidence.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::process::{Command, ExitCode, Stdio};

use serde::{Deserialize, Serialize};

use crate::Format;

// The check document (input)

/// The `check --format json` document, as much of it as triage reads. Unknown
/// keys (`fix`, a future addition) are ignored; absent counters default to
/// zero so a hand-written or trimmed stream still aggregates.
#[derive(Deserialize)]
struct CheckDocument {
    #[serde(default)]
    findings: Vec<Finding>,
    /// The surface the run displayed (ADR-0050 §5). Absent in a trimmed
    /// stream; reported as `unknown` then, never guessed.
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    vendor_suppressed: usize,
    #[serde(default)]
    suppressed: usize,
    #[serde(default)]
    baselined: usize,
}

/// One finding of the stream. `layer` and `level` are the ADR-0050 additive
/// fields; a stream without them aggregates under `unknown`.
#[derive(Deserialize)]
struct Finding {
    id: String,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    level: Option<String>,
    path: String,
}

/// The spelling an absent `layer`/`level`/`profile` aggregates under.
const UNKNOWN: &str = "unknown";

// The report (data model)

/// The whole triage report. `--format json` is this value, serialized.
#[derive(Serialize)]
pub struct TriageReport {
    pub source: Source,
    pub summary: Summary,
    pub distribution: Vec<IdRow>,
    pub hotspots: Hotspots,
    pub offset: OffsetGuardStatus,
    pub hints: Vec<Hint>,
}

/// Where the stream came from and which surface it measures.
#[derive(Serialize)]
pub struct Source {
    pub kind: SourceKind,
    /// The `profile` the document names — the surface every count below is
    /// a count *of*.
    pub profile: String,
    /// `--input`'s argument (`-` for stdin); `null` for a `check` run.
    pub input: Option<String>,
    /// The paths handed to `check`; empty for `--input`.
    pub paths: Vec<String>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// Triage ran `check --format json` itself.
    CheckRun,
    /// Triage read a document a previous `check` wrote.
    Input,
}

/// Totals: how much, how severe, which layer, and what the run held back.
#[derive(Serialize)]
pub struct Summary {
    pub findings: usize,
    /// Distinct paths carrying at least one finding.
    pub files: usize,
    /// Findings per exit level (`fail`/`warn`, ADR-0050 §7).
    pub by_level: BTreeMap<String, usize>,
    /// Findings per layer (`proof`/`contract`/`mechanics`/`debug`, ADR-0050 §1).
    pub by_layer: BTreeMap<String, usize>,
    /// The suppression channels' counts, copied from the document: findings
    /// the stream does NOT contain, so a reader knows the totals are of the
    /// displayed surface and not of the whole debt.
    pub held_back: HeldBack,
}

#[derive(Serialize)]
pub struct HeldBack {
    pub vendor_suppressed: usize,
    pub suppressed: usize,
    pub baselined: usize,
}

/// One diagnostic id's row in the distribution.
#[derive(Serialize)]
pub struct IdRow {
    pub id: String,
    pub layer: String,
    pub level: String,
    pub count: usize,
    /// Distinct files the id fires in — the systemic-vs-localised signal
    /// beside the raw count.
    pub files: usize,
}

/// The per-file view. `entries` is capped (`limit`, the `--top` flag) so a
/// monorepo's report stays readable; `files_with_findings` says how many the
/// cap hid.
#[derive(Serialize)]
pub struct Hotspots {
    pub files_with_findings: usize,
    pub limit: usize,
    pub entries: Vec<Hotspot>,
}

#[derive(Serialize)]
pub struct Hotspot {
    pub path: String,
    pub count: usize,
    pub by_id: Vec<IdCount>,
}

#[derive(Serialize)]
pub struct IdCount {
    pub id: String,
    pub count: usize,
}

/// The offset family's guard-status buckets (issue #50, ADR-0062 A-G10). Each
/// bucket is either measured from the stream or honestly `not-measured`, with
/// the reason spelled out — the stream is the whole input, and this section
/// says which of its questions the stream can answer.
#[derive(Serialize)]
pub struct OffsetGuardStatus {
    /// Every `offset.*` finding in the stream, whichever bucket it falls in.
    pub family_total: usize,
    pub buckets: Vec<Bucket>,
}

#[derive(Serialize)]
pub struct Bucket {
    pub name: &'static str,
    pub status: BucketStatus,
    /// The count when measured; `null` otherwise.
    pub count: Option<usize>,
    /// The ids the bucket counts.
    pub ids: Vec<&'static str>,
    pub note: String,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum BucketStatus {
    Measured,
    NotMeasured,
}

/// A hint: advice from one recognizer of the [`CATALOGUE`]. Never a finding —
/// it names no line, carries no level, and touches no exit code.
#[derive(Serialize)]
pub struct Hint {
    pub recognizer: &'static str,
    pub advice: String,
}

// The offset vocabulary triage reads

/// `offset.missing` (ADR-0049 §7): a key provably absent from a proven
/// container — the `provably-missing` bucket.
const OFFSET_MISSING: &str = "offset.missing";

/// `offset.maybe-missing` (ADR-0062 A-G10, issue #51): an optional-key read
/// no guard discharged, on the `strict` rung only — the `unguarded` bucket.
const OFFSET_MAYBE_MISSING: &str = "offset.maybe-missing";

/// The built-in rung that admits [`OFFSET_MAYBE_MISSING`]. The document names
/// its profile but not the id set that profile resolved to, so this is the one
/// surface triage can *know* measured the bucket; a user profile that
/// `extends = "strict"` is recognized only when the id actually fires.
const STRICT_PROFILE: &str = "strict";

/// Bucket names, in report order.
const BUCKET_UNGUARDED: &str = "unguarded";
const BUCKET_DISCHARGED: &str = "guarded-and-discharged";
const BUCKET_PROVABLY_MISSING: &str = "provably-missing";

/// The default `--top` for [`Hotspots`].
const DEFAULT_HOTSPOT_LIMIT: usize = 10;

// Aggregation

/// Everything the recognizers may look at: the report sections built so far
/// plus the uncapped per-file counts the hotspot cap would otherwise hide.
struct Aggregate {
    summary: Summary,
    distribution: Vec<IdRow>,
    /// path → (total, id → count), uncapped.
    per_file: BTreeMap<String, (usize, BTreeMap<String, usize>)>,
    offset: OffsetGuardStatus,
    profile: String,
    /// `offset.maybe-missing` paths, one entry per finding.
    optional_read_paths: Vec<String>,
}

/// Build the report from a parsed document. Pure: same document, same report.
fn aggregate(doc: &CheckDocument, source: Source, hotspot_limit: usize) -> TriageReport {
    let profile = doc.profile.clone().unwrap_or_else(|| UNKNOWN.to_owned());

    let mut by_level: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_layer: BTreeMap<String, usize> = BTreeMap::new();
    // id → (layer, level, count, files)
    let mut per_id: BTreeMap<&str, (String, String, usize, BTreeMap<&str, ()>)> = BTreeMap::new();
    let mut per_file: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    let mut optional_read_paths = Vec::new();

    for f in &doc.findings {
        let layer = f.layer.clone().unwrap_or_else(|| UNKNOWN.to_owned());
        let level = f.level.clone().unwrap_or_else(|| UNKNOWN.to_owned());
        *by_level.entry(level.clone()).or_insert(0) += 1;
        *by_layer.entry(layer.clone()).or_insert(0) += 1;
        let row = per_id
            .entry(f.id.as_str())
            .or_insert_with(|| (layer, level, 0, BTreeMap::new()));
        row.2 += 1;
        row.3.insert(f.path.as_str(), ());
        let file = per_file.entry(f.path.clone()).or_insert_with(|| (0, BTreeMap::new()));
        file.0 += 1;
        *file.1.entry(f.id.clone()).or_insert(0) += 1;
        if f.id == OFFSET_MAYBE_MISSING {
            optional_read_paths.push(f.path.clone());
        }
    }

    let mut distribution: Vec<IdRow> = per_id
        .into_iter()
        .map(|(id, (layer, level, count, files))| IdRow {
            id: id.to_owned(),
            layer,
            level,
            count,
            files: files.len(),
        })
        .collect();
    // Heaviest first; ties by id so the order is a function of the stream.
    distribution.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.id.cmp(&b.id)));

    let summary = Summary {
        findings: doc.findings.len(),
        files: per_file.len(),
        by_level,
        by_layer,
        held_back: HeldBack {
            vendor_suppressed: doc.vendor_suppressed,
            suppressed: doc.suppressed,
            baselined: doc.baselined,
        },
    };

    let offset = offset_buckets(&distribution, &profile);

    let agg = Aggregate {
        summary,
        distribution,
        per_file,
        offset,
        profile,
        optional_read_paths,
    };
    let hints = run_catalogue(&agg);
    let hotspots = hotspots(&agg.per_file, hotspot_limit);

    TriageReport {
        source,
        summary: agg.summary,
        distribution: agg.distribution,
        hotspots,
        offset: agg.offset,
        hints,
    }
}

/// The capped per-file view, heaviest file first, ties by path.
fn hotspots(
    per_file: &BTreeMap<String, (usize, BTreeMap<String, usize>)>,
    limit: usize,
) -> Hotspots {
    let mut entries: Vec<Hotspot> = per_file
        .iter()
        .map(|(path, (count, by_id))| {
            let mut by_id: Vec<IdCount> =
                by_id.iter().map(|(id, n)| IdCount { id: id.clone(), count: *n }).collect();
            by_id.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.id.cmp(&b.id)));
            Hotspot { path: path.clone(), count: *count, by_id }
        })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
    entries.truncate(limit);
    Hotspots { files_with_findings: per_file.len(), limit, entries }
}

/// The count of one id in the distribution, zero when absent.
fn count_of(distribution: &[IdRow], id: &str) -> usize {
    distribution.iter().find(|r| r.id == id).map_or(0, |r| r.count)
}

/// The three guard-status buckets (issue #50), derived from what the stream
/// carries:
///
/// * `provably-missing` is `offset.missing`, on every surface — always measured.
/// * `unguarded` is `offset.maybe-missing`, admitted by the `strict` rung
///   alone. Measured when the document names that profile or the id fires;
///   otherwise the bucket says which run would measure it.
/// * `guarded-and-discharged` counts reads a proof deleted — and a deleted
///   finding leaves nothing in a stream of findings. Never measured from this
///   input; the note says so. Making it measurable is a `check`-side count of
///   discharges (a schema addition ADR-0095 records and this command does not
///   make).
fn offset_buckets(distribution: &[IdRow], profile: &str) -> OffsetGuardStatus {
    let family_total: usize =
        distribution.iter().filter(|r| r.id.starts_with("offset.")).map(|r| r.count).sum();
    let maybe_missing = count_of(distribution, OFFSET_MAYBE_MISSING);
    let strict_measured = profile == STRICT_PROFILE || maybe_missing > 0;
    let unguarded = if strict_measured {
        Bucket {
            name: BUCKET_UNGUARDED,
            status: BucketStatus::Measured,
            count: Some(maybe_missing),
            ids: vec![OFFSET_MAYBE_MISSING],
            note: format!(
                "optional-key reads no guard discharges on their path; each would be a finding on the `{STRICT_PROFILE}` surface"
            ),
        }
    } else {
        Bucket {
            name: BUCKET_UNGUARDED,
            status: BucketStatus::NotMeasured,
            count: None,
            ids: vec![OFFSET_MAYBE_MISSING],
            note: format!(
                "surface `{profile}` does not admit `{OFFSET_MAYBE_MISSING}`; run `steins triage --profile {STRICT_PROFILE} <paths>` to measure the what-if"
            ),
        }
    };
    let discharged = Bucket {
        name: BUCKET_DISCHARGED,
        status: BucketStatus::NotMeasured,
        count: None,
        ids: Vec::new(),
        note: "a discharged read leaves no finding, and the check stream carries findings only; measuring this bucket needs a check-side count of discharges".to_owned(),
    };
    let provably_missing = Bucket {
        name: BUCKET_PROVABLY_MISSING,
        status: BucketStatus::Measured,
        count: Some(count_of(distribution, OFFSET_MISSING)),
        ids: vec![OFFSET_MISSING],
        note: "reads of a key provably absent from a proven container; on the default surface, so these are runtime warnings today".to_owned(),
    };
    OffsetGuardStatus { family_total, buckets: vec![unguarded, discharged, provably_missing] }
}

// The recognizer catalogue

/// One recognizer: a name (the `recognizer` field of every hint it emits) and
/// a function that reads the aggregate and pushes zero or more pieces of
/// advice. The name is attached here, once, so a recognizer cannot sign its
/// advice with another row's name.
struct Recognizer {
    name: &'static str,
    run: fn(&Aggregate, &mut Vec<String>),
}

/// The catalogue, in report order. Adding a recognizer is one row here and
/// one function below; `tests/triage.rs` pins each row with a stream that
/// fires it, so a row cannot vanish unnoticed.
const CATALOGUE: &[Recognizer] = &[
    Recognizer { name: "strict-what-if-unmeasured", run: strict_what_if_unmeasured },
    Recognizer { name: "baseline-hides-debt", run: baseline_hides_debt },
    Recognizer { name: "localised-id", run: localised_id },
    Recognizer { name: "optional-reads-concentrated", run: optional_reads_concentrated },
];

fn run_catalogue(agg: &Aggregate) -> Vec<Hint> {
    let mut hints = Vec::new();
    for r in CATALOGUE {
        let mut advice = Vec::new();
        (r.run)(agg, &mut advice);
        hints.extend(advice.into_iter().map(|advice| Hint { recognizer: r.name, advice }));
    }
    hints
}

/// The `unguarded` bucket is not measured: the stream came from a surface
/// below `strict`. The adoption flow's first step is the what-if, so say how.
fn strict_what_if_unmeasured(agg: &Aggregate, hints: &mut Vec<String>) {
    let unmeasured = agg
        .offset
        .buckets
        .iter()
        .any(|b| b.name == BUCKET_UNGUARDED && b.status == BucketStatus::NotMeasured);
    if unmeasured {
        hints.push(format!(
            "this report measures surface `{}`; the strict what-if is one run away: `steins triage --profile {STRICT_PROFILE} <paths>` counts the optional-key reads `strict` would report, with check's behavior and exit code unchanged",
            agg.profile
        ));
    }
}

/// The document held findings back behind a baseline. The totals are of the
/// displayed surface, not of the whole debt, and a reader deciding whether to
/// raise a stage wants the whole debt.
fn baseline_hides_debt(agg: &Aggregate, hints: &mut Vec<String>) {
    let n = agg.summary.held_back.baselined;
    if n > 0 {
        hints.push(format!(
            "{n} finding(s) sit in the baseline and are not counted above; rerun with --ignore-baseline to measure the whole debt before judging a stage change"
        ));
    }
}

/// An id with a handful of findings or more, most of them in one file, is a
/// local repair rather than a project-wide pattern — fix the file before
/// freezing the id into a baseline.
fn localised_id(agg: &Aggregate, hints: &mut Vec<String>) {
    const MIN_COUNT: usize = 5;
    for row in &agg.distribution {
        if row.count < MIN_COUNT || row.files < 2 {
            continue;
        }
        // The file carrying most of this id, ties by path.
        let Some((path, in_file)) = agg
            .per_file
            .iter()
            .filter_map(|(path, (_, by_id))| by_id.get(&row.id).map(|n| (path, *n)))
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        else {
            continue;
        };
        // ≥ 80% in one file.
        if in_file * 5 >= row.count * 4 {
            hints.push(format!(
                "`{}`: {in_file} of {} findings are in {path} — localised, not systemic; fixing that file clears most of the id before it is enabled or baselined",
                row.id, row.count
            ));
        }
    }
}

/// Optional-key reads (`offset.maybe-missing`) concentrated under one
/// directory: the shape they read is one shape, and a typed object in its
/// place (a shape-to-DTO transform) discharges them together. Enabling
/// `strict` first would freeze them as debt instead.
fn optional_reads_concentrated(agg: &Aggregate, hints: &mut Vec<String>) {
    const MIN_COUNT: usize = 3;
    let total = agg.optional_read_paths.len();
    if total < MIN_COUNT {
        return;
    }
    let mut per_dir: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &agg.optional_read_paths {
        *per_dir.entry(parent_dir(p)).or_insert(0) += 1;
    }
    let Some((dir, n)) =
        per_dir.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
    else {
        return;
    };
    // ≥ 60% under one directory.
    if n * 5 >= total * 3 {
        hints.push(format!(
            "{n} of {total} `{OFFSET_MAYBE_MISSING}` reads sit under `{dir}` — a shape-to-DTO transform (one typed object in place of the optional-key array) discharges them together; enabling `{STRICT_PROFILE}` before that lands would baseline them as debt"
        ));
    }
}

/// The directory part of a finding path, as the path is spelled (`.` for a
/// bare file name).
fn parent_dir(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((dir, _)) => dir,
        None => ".",
    }
}

// Rendering

/// The text rendering: five sections, in the data model's order, each present
/// even when empty so a reader can tell "measured zero" from "not shown".
pub fn render_text(r: &TriageReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "triage of surface `{}`: {} finding(s) in {} file(s)\n",
        r.source.profile, r.summary.findings, r.summary.files
    ));

    out.push_str("\nSummary\n");
    out.push_str(&format!("  by level: {}\n", counts_line(&r.summary.by_level)));
    out.push_str(&format!("  by layer: {}\n", counts_line(&r.summary.by_layer)));
    let h = &r.summary.held_back;
    out.push_str(&format!(
        "  held back: vendor {}, inline ignores {}, baseline {}\n",
        h.vendor_suppressed, h.suppressed, h.baselined
    ));

    out.push_str("\nDistribution\n");
    if r.distribution.is_empty() {
        out.push_str("  (no findings)\n");
    }
    for row in &r.distribution {
        out.push_str(&format!(
            "  {:>6}  {}  ({} file(s), {}/{})\n",
            row.count, row.id, row.files, row.layer, row.level
        ));
    }

    out.push_str(&format!(
        "\nHotspots (top {} of {} file(s))\n",
        r.hotspots.entries.len(),
        r.hotspots.files_with_findings
    ));
    if r.hotspots.entries.is_empty() {
        out.push_str("  (no findings)\n");
    }
    for h in &r.hotspots.entries {
        out.push_str(&format!("  {:>6}  {}\n", h.count, h.path));
        let ids: Vec<String> = h.by_id.iter().map(|c| format!("{} {}", c.count, c.id)).collect();
        out.push_str(&format!("          {}\n", ids.join(", ")));
    }

    out.push_str(&format!(
        "\nOffset guard status ({} offset.* finding(s))\n",
        r.offset.family_total
    ));
    for b in &r.offset.buckets {
        match b.status {
            BucketStatus::Measured => {
                let ids = b.ids.join(", ");
                out.push_str(&format!(
                    "  {:<24}{:>6}  [{ids}]\n    {}\n",
                    b.name,
                    b.count.unwrap_or(0),
                    b.note
                ));
            }
            BucketStatus::NotMeasured => {
                out.push_str(&format!("  {:<24}  not measured\n    {}\n", b.name, b.note));
            }
        }
    }

    out.push_str("\nHints (advice, not findings; the exit code is 0 either way)\n");
    if r.hints.is_empty() {
        out.push_str("  (none)\n");
    }
    for h in &r.hints {
        out.push_str(&format!("  - [{}] {}\n", h.recognizer, h.advice));
    }
    out
}

/// `key n, key n` in key order; `none` for an empty map.
fn counts_line(m: &BTreeMap<String, usize>) -> String {
    if m.is_empty() {
        return "none".to_owned();
    }
    m.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
}

/// The JSON rendering: the data model, serialized, one trailing newline.
pub fn render_json(r: &TriageReport) -> String {
    match serde_json::to_string_pretty(r) {
        Ok(s) => format!("{s}\n"),
        Err(e) => {
            // Unreachable for a tree of plain structs; kept for parity with
            // `render::json`.
            errln!("steins: failed to serialize json: {e}");
            String::new()
        }
    }
}

// The command

/// The synopsis the no-argument usage and the `2` exits print.
pub(crate) const USAGE: &str = "steins triage [--format text|json] [--input <file>|-] [--top <n>] [--profile <name>] [--no-php] [--no-cache] [--no-tolerated-effects] [--vendor-diagnostics] [--ignore-baseline] [--baseline <path>] [<paths...>]";

pub(crate) fn run_triage(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut input: Option<String> = None;
    let mut top = DEFAULT_HOTSPOT_LIMIT;
    // Flags handed to `check` verbatim: they decide what the stream contains.
    let mut passthrough: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --format requires an argument (text|json)");
                    return ExitCode::from(2);
                };
                format = match value.as_str() {
                    "text" => Format::Text,
                    "json" => Format::Json,
                    other => {
                        errln!("steins: unknown format `{other}` (text|json)");
                        return ExitCode::from(2);
                    }
                };
                i += 2;
            }
            "--input" => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: --input requires a file argument (or `-` for stdin)");
                    return ExitCode::from(2);
                };
                input = Some(value.clone());
                i += 2;
            }
            "--top" => {
                let parsed = args.get(i + 1).and_then(|v| v.parse::<usize>().ok());
                let Some(n) = parsed else {
                    errln!("steins: --top requires a non-negative integer");
                    return ExitCode::from(2);
                };
                top = n;
                i += 2;
            }
            flag @ ("--profile" | "--baseline") => {
                let Some(value) = args.get(i + 1) else {
                    errln!("steins: {flag} requires an argument");
                    return ExitCode::from(2);
                };
                passthrough.push(flag.to_owned());
                passthrough.push(value.clone());
                i += 2;
            }
            flag @ ("--no-php"
            | "--no-cache"
            | "--no-tolerated-effects"
            | "--vendor-diagnostics"
            | "--ignore-baseline") => {
                passthrough.push(flag.to_owned());
                i += 1;
            }
            other if other.starts_with("--") => {
                errln!("steins: unknown flag `{other}` for triage");
                errln!("usage: {USAGE}");
                return ExitCode::from(2);
            }
            other => {
                paths.push(other.to_owned());
                i += 1;
            }
        }
    }

    // One source or the other: a document to read, or paths to run `check` over.
    let (text, source) = match (input, paths.is_empty()) {
        (Some(_), false) => {
            errln!("steins: --input and <paths> cannot combine (one stream per report)");
            return ExitCode::from(2);
        }
        (None, true) => {
            errln!("steins: no --input and no paths given");
            errln!("usage: {USAGE}");
            return ExitCode::from(2);
        }
        (Some(name), true) => {
            if !passthrough.is_empty() {
                errln!(
                    "steins: `{}` belongs to a check run; --input reads a stream check already wrote",
                    passthrough.join(" ")
                );
                return ExitCode::from(2);
            }
            let read = if name == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s).map(|_| s)
            } else {
                std::fs::read_to_string(&name)
            };
            let text = match read {
                Ok(t) => t,
                Err(e) => {
                    errln!("steins: cannot read --input {name}: {e}");
                    return ExitCode::from(2);
                }
            };
            let source =
                Source { kind: SourceKind::Input, profile: String::new(), input: Some(name), paths };
            (text, source)
        }
        (None, false) => match check_stream(&passthrough, &paths) {
            Ok(text) => {
                let source = Source {
                    kind: SourceKind::CheckRun,
                    profile: String::new(),
                    input: None,
                    paths,
                };
                (text, source)
            }
            Err(code) => return code,
        },
    };

    let doc: CheckDocument = match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            errln!("steins: the input is not a `check --format json` document: {e}");
            return ExitCode::from(2);
        }
    };
    let mut source = source;
    source.profile = doc.profile.clone().unwrap_or_else(|| UNKNOWN.to_owned());

    let report = aggregate(&doc, source, top);
    let rendered = match format {
        Format::Text => render_text(&report),
        Format::Json => render_json(&report),
    };
    out!("{rendered}");
    // A measurement, not a gate: the counts never decide the exit.
    ExitCode::SUCCESS
}

/// Run this binary's own `check --format json` over `paths` and return its
/// stdout. `check`'s stderr passes through (the sound-subset notice, runtime
/// warnings) so nothing a user would have seen is lost. Its exit `0`/`1` are
/// both a stream (findings or none); `2` is a usage or config error and is
/// forwarded as this command's exit.
fn check_stream(passthrough: &[String], paths: &[String]) -> Result<String, ExitCode> {
    let exe = std::env::current_exe().map_err(|e| {
        errln!("steins: cannot locate the steins binary to run check: {e}");
        ExitCode::from(2)
    })?;
    let output = Command::new(exe)
        .arg("check")
        .arg("--format")
        .arg("json")
        .args(passthrough)
        .args(paths)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| {
            errln!("steins: cannot run check: {e}");
            ExitCode::from(2)
        })?;
    match output.status.code() {
        Some(0 | 1) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Some(code) => Err(ExitCode::from(u8::try_from(code).unwrap_or(1))),
        None => {
            errln!("steins: check was terminated by a signal");
            Err(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(json: &str) -> CheckDocument {
        serde_json::from_str(json).expect("a check document")
    }

    fn input_source() -> Source {
        Source {
            kind: SourceKind::Input,
            profile: "default".to_owned(),
            input: Some("-".to_owned()),
            paths: Vec::new(),
        }
    }

    /// The ids this module spells by hand are the registry's spellings —
    /// pinned here so a rename in `steins-infer` fails this test rather than
    /// quietly emptying a bucket. The command itself never reads the registry.
    #[test]
    fn the_offset_vocabulary_matches_the_registry() {
        assert_eq!(OFFSET_MISSING, steins_infer::OFFSET_MISSING_ID);
        assert_eq!(OFFSET_MAYBE_MISSING, steins_infer::OFFSET_MAYBE_MISSING_ID);
        assert_eq!(STRICT_PROFILE, steins_infer::Floor::Strict.as_str());
    }

    /// A document with none of the additive fields still aggregates, under
    /// `unknown` rather than by guessing.
    #[test]
    fn a_trimmed_stream_aggregates_under_unknown() {
        let d = doc(r#"{"findings":[{"id":"x.y","path":"a.php"}]}"#);
        let r = aggregate(&d, input_source(), DEFAULT_HOTSPOT_LIMIT);
        assert_eq!(r.summary.findings, 1);
        assert_eq!(r.summary.by_layer.get("unknown"), Some(&1));
        assert_eq!(r.summary.by_level.get("unknown"), Some(&1));
        assert_eq!(r.distribution[0].layer, "unknown");
        assert_eq!(r.summary.held_back.baselined, 0);
    }

    #[test]
    fn distribution_is_heaviest_first_then_by_id() {
        let d = doc(
            r#"{"findings":[
                {"id":"b.b","path":"a.php"},{"id":"a.a","path":"a.php"},
                {"id":"c.c","path":"a.php"},{"id":"c.c","path":"b.php"}]}"#,
        );
        let r = aggregate(&d, input_source(), DEFAULT_HOTSPOT_LIMIT);
        let ids: Vec<&str> = r.distribution.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["c.c", "a.a", "b.b"]);
        assert_eq!(r.distribution[0].files, 2);
    }

    #[test]
    fn hotspots_are_capped_and_say_so() {
        let d = doc(
            r#"{"findings":[
                {"id":"x","path":"a.php"},{"id":"x","path":"a.php"},
                {"id":"x","path":"b.php"},{"id":"y","path":"c.php"}]}"#,
        );
        let r = aggregate(&d, input_source(), 1);
        assert_eq!(r.hotspots.files_with_findings, 3);
        assert_eq!(r.hotspots.entries.len(), 1);
        assert_eq!(r.hotspots.entries[0].path, "a.php");
        assert_eq!(r.hotspots.limit, 1);
    }

    #[test]
    fn the_unguarded_bucket_is_measured_on_strict_and_not_below() {
        let below = doc(r#"{"findings":[],"profile":"default"}"#);
        let r = aggregate(&below, input_source(), DEFAULT_HOTSPOT_LIMIT);
        let unguarded = &r.offset.buckets[0];
        assert_eq!(unguarded.name, BUCKET_UNGUARDED);
        assert_eq!(unguarded.status, BucketStatus::NotMeasured);
        assert_eq!(unguarded.count, None);

        let strict = doc(r#"{"findings":[],"profile":"strict"}"#);
        let r = aggregate(&strict, input_source(), DEFAULT_HOTSPOT_LIMIT);
        assert_eq!(r.offset.buckets[0].status, BucketStatus::Measured);
        assert_eq!(r.offset.buckets[0].count, Some(0));

        // A user profile that admits the id is recognized by the id firing.
        let user = doc(
            r#"{"findings":[{"id":"offset.maybe-missing","path":"a.php"}],"profile":"mine"}"#,
        );
        let r = aggregate(&user, input_source(), DEFAULT_HOTSPOT_LIMIT);
        assert_eq!(r.offset.buckets[0].status, BucketStatus::Measured);
        assert_eq!(r.offset.buckets[0].count, Some(1));
    }

    #[test]
    fn the_discharged_bucket_is_never_measured_from_a_stream() {
        let d = doc(r#"{"findings":[],"profile":"strict"}"#);
        let r = aggregate(&d, input_source(), DEFAULT_HOTSPOT_LIMIT);
        let b = &r.offset.buckets[1];
        assert_eq!(b.name, BUCKET_DISCHARGED);
        assert_eq!(b.status, BucketStatus::NotMeasured);
        assert!(b.note.contains("findings only"), "{}", b.note);
    }

    #[test]
    fn the_provably_missing_bucket_counts_offset_missing() {
        let d = doc(
            r#"{"findings":[
                {"id":"offset.missing","path":"a.php"},{"id":"offset.missing","path":"b.php"},
                {"id":"offset.on-unsupported","path":"a.php"}],"profile":"default"}"#,
        );
        let r = aggregate(&d, input_source(), DEFAULT_HOTSPOT_LIMIT);
        let b = &r.offset.buckets[2];
        assert_eq!(b.name, BUCKET_PROVABLY_MISSING);
        assert_eq!(b.count, Some(2));
        // The family total keeps the id no bucket names.
        assert_eq!(r.offset.family_total, 3);
    }

    #[test]
    fn parent_dir_reads_the_path_as_spelled() {
        assert_eq!(parent_dir("src/Http/A.php"), "src/Http");
        assert_eq!(parent_dir("A.php"), ".");
        assert_eq!(parent_dir("/A.php"), "/");
    }

    #[test]
    fn localised_id_needs_a_second_file_and_eighty_percent() {
        // 4 in a.php, 1 in b.php: 80%, fires.
        let d = doc(
            r#"{"findings":[
                {"id":"x","path":"a.php"},{"id":"x","path":"a.php"},{"id":"x","path":"a.php"},
                {"id":"x","path":"a.php"},{"id":"x","path":"b.php"}],"profile":"strict"}"#,
        );
        let r = aggregate(&d, input_source(), DEFAULT_HOTSPOT_LIMIT);
        assert!(r.hints.iter().any(|h| h.recognizer == "localised-id"));
        // All in one file: a single-file id is not "localised", it's the whole story.
        let d = doc(
            r#"{"findings":[
                {"id":"x","path":"a.php"},{"id":"x","path":"a.php"},{"id":"x","path":"a.php"},
                {"id":"x","path":"a.php"},{"id":"x","path":"a.php"}],"profile":"strict"}"#,
        );
        let r = aggregate(&d, input_source(), DEFAULT_HOTSPOT_LIMIT);
        assert!(!r.hints.iter().any(|h| h.recognizer == "localised-id"));
    }
}
