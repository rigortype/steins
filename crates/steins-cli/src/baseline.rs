//! The `.steins-baseline.jsonl` channel (ADR-0022): the accumulated past at
//! adoption, machine-managed, line-shift-immune.
//!
//! # Format
//!
//! JSONL. A header line `{"steins-baseline":1,"note":"…"}`, then one
//! `{"id","path","hash"[,"surface"]}` entry per line, sorted by `(path, id, hash)`
//! for diff stability. `path` is relative to the baseline file's directory, forward
//! slashes.
//!
//! # Stable hash (no line numbers)
//!
//! [`entry_hash`] is the first 16 hex of SHA-256 over
//! `id + relative-path + the flagged line's trimmed text + the trimmed nearest
//! non-empty line above + below`. This survives unrelated edits elsewhere in the
//! file (line-shift immunity — the ADR's whole point) and intentionally breaks
//! when the flagged line or its immediate neighborhood changes (the finding then
//! correctly resurfaces).
//!
//! # Capture surface (ADR-0050 §8, ADR-0062 A-G10)
//!
//! The header records the **capture surface**: the `profile` name and the resolved
//! id-set the baseline was written under. Two consequences: (a) staleness is
//! computed only over ids *inside the current run's surface* — an unconsumed entry
//! whose id is outside it is *dormant* (kept, not stale, not pruned); (b) a run
//! whose active surface exceeds the captured one prints a one-line notice so it
//! "drowns loudly", never silently.
//!
//! ADR-0062 A-G10 pushes the capture surface down to the **entry**: each entry may
//! carry the profile *rung* (`"default"|"contracts"|"strict"`) it was captured at,
//! and an entry is judged unmatched only on a run whose rung is at or above it. The
//! header is one string for a whole file; the per-entry tag is what keeps a file
//! honest when entries from different surfaces end up in it.
//!
//! **Round-trip and the untagged reading.** The field is omitted entirely when the
//! rung is `default`, so a `default`-captured baseline is byte-identical to one this
//! crate wrote before S6, and an untagged legacy entry reads as **captured at
//! `default`**. That reading is chosen because it is the *behavior-preserving* one:
//! `Default <= every rung`, so the rung clause is vacuously true for every legacy
//! entry and staleness for legacy files is decided exactly as before, by the id
//! alone. An unrecognized spelling (a hand-edit) reads the same way.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use steins_infer::Floor;

use crate::sha256;

/// One baseline entry. Field order is the on-disk key order (serde preserves
/// struct field order): `{"id":…,"path":…,"hash":…[,"surface":…]}`.
///
/// `surface` is the capture **rung** (ADR-0062 A-G10), omitted when `default` — see
/// the module docs for the round-trip rule. It is deliberately NOT part of the
/// match key: a finding matches its entry by `(id, path, hash)` regardless of the
/// surface either was seen on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
}

impl Entry {
    /// This entry's capture rung: the tag when present and recognized, else
    /// `Floor::Default` (the untagged/legacy reading — see the module docs).
    #[must_use]
    pub fn captured_at(&self) -> Floor {
        self.surface.as_deref().and_then(Floor::parse).unwrap_or(Floor::Default)
    }

    /// The on-disk tag for a capture at `rung`: `None` at `default` so the written
    /// line stays byte-identical to a pre-S6 baseline.
    #[must_use]
    pub fn tag_for(rung: Floor) -> Option<String> {
        (rung != Floor::Default).then(|| rung.as_str().to_owned())
    }
}

/// The default baseline filename, looked up in the CWD (ADR-0022).
pub const DEFAULT_FILE: &str = ".steins-baseline.jsonl";

/// The capture surface recorded in a baseline header (ADR-0050 §8): the profile
/// name the baseline was written under and its resolved id-set. Present in headers
/// written by this version; absent (`None` from [`parse_header`]) in pre-ADR-0050
/// baselines, which then simply skip the surface-exceeds notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSurface {
    pub profile: String,
    pub ids: Vec<String>,
}

/// The stable 16-hex hash of a finding (see the module docs). `rel_path` is the
/// already-normalized relative path; `text` is the flagged file's full contents;
/// `line` is 1-based.
#[must_use]
pub fn entry_hash(id: &str, rel_path: &str, text: &str, line: u32) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let idx = (line as usize).saturating_sub(1);
    let cur = lines.get(idx).map_or("", |s| s.trim());
    let above = (0..idx)
        .rev()
        .map(|i| lines[i].trim())
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let below = lines
        .iter()
        .skip(idx + 1)
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let input = format!("{id}\n{rel_path}\n{cur}\n{above}\n{below}");
    sha256::hex(input.as_bytes())[..16].to_owned()
}

/// Normalize a diagnostic's file path to a baseline-relative, forward-slash path.
/// Both the file and `base_dir` are canonicalized when possible; if the file is
/// not under `base_dir`, its canonical (or original) path is used as the fallback.
#[must_use]
pub fn relativize(base_dir: &Path, file_path: &str) -> String {
    let abs_file = Path::new(file_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(file_path));
    let abs_base = base_dir.canonicalize().unwrap_or_else(|_| base_dir.to_path_buf());
    let rel = abs_file.strip_prefix(&abs_base).unwrap_or(&abs_file);
    rel.to_string_lossy().replace('\\', "/")
}

/// The directory a baseline `file` lives in (its parent, or `.`).
#[must_use]
pub fn base_dir(file: &Path) -> PathBuf {
    file.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Serialize `entries` (sorted by `path`, then `id`, then `hash`) to the JSONL
/// text of a baseline file, header included. The header records the capture
/// `surface` (ADR-0050 §8): its `profile` name and resolved id-set.
#[must_use]
pub fn render(mut entries: Vec<Entry>, surface: &CaptureSurface) -> String {
    entries.sort_by(|a, b| {
        (a.path.as_str(), a.id.as_str(), a.hash.as_str()).cmp(&(
            b.path.as_str(),
            b.id.as_str(),
            b.hash.as_str(),
        ))
    });
    let header = serde_json::json!({
        "steins-baseline": 1,
        "note": "machine-managed; do not hand-edit",
        "profile": surface.profile,
        "surface": surface.ids,
    });
    let mut out = String::new();
    out.push_str(&serde_json::to_string(&header).expect("serialize baseline header"));
    out.push('\n');
    for e in &entries {
        // A derived-struct serialize never fails and keeps field order.
        out.push_str(&serde_json::to_string(e).expect("serialize baseline entry"));
        out.push('\n');
    }
    out
}

/// Read the capture surface from a baseline file's header line (ADR-0050 §8), or
/// `None` for a pre-ADR-0050 header lacking `profile`/`surface` (a hand-edit or
/// unparsable header is likewise `None`). The header is the first line.
#[must_use]
pub fn parse_header(text: &str) -> Option<CaptureSurface> {
    let first = text.lines().next()?;
    let value: serde_json::Value = serde_json::from_str(first).ok()?;
    let profile = value.get("profile")?.as_str()?.to_owned();
    let ids = value
        .get("surface")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    Some(CaptureSurface { profile, ids })
}

/// Parse a baseline file's JSONL text into entries. The header line is skipped;
/// blank lines and unparsable lines are ignored (a hand-edit tolerance).
#[must_use]
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .skip(1) // header
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect()
}

/// A multiset of baseline entries keyed by `(id, path, hash)`, consumed one-for-
/// one as findings match (duplicate findings against one entry: one suppressed,
/// one reported — ADR-0022's implicit count).
pub struct Matcher {
    /// `key -> (unconsumed count, lowest capture rung seen for that key)`. The rung
    /// is the *minimum* over entries sharing a key: the lowest rung is the one that
    /// makes the entry judgeable on the widest range of runs, so taking it never
    /// hides a stale entry from a surface that could have matched it.
    counts: HashMap<(String, String, String), (usize, Floor)>,
}

impl Matcher {
    #[must_use]
    pub fn new(entries: &[Entry]) -> Self {
        let mut counts: HashMap<(String, String, String), (usize, Floor)> = HashMap::new();
        for e in entries {
            let slot = counts
                .entry((e.id.clone(), e.path.clone(), e.hash.clone()))
                .or_insert((0, Floor::Strict));
            slot.0 += 1;
            slot.1 = slot.1.min(e.captured_at());
        }
        Self { counts }
    }

    /// Try to consume one entry matching `(id, path, hash)`. Returns `true` (and
    /// decrements) on a match, `false` when no unconsumed entry remains.
    pub fn take(&mut self, id: &str, path: &str, hash: &str) -> bool {
        let key = (id.to_owned(), path.to_owned(), hash.to_owned());
        match self.counts.get_mut(&key) {
            Some((n, _)) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    }

    /// Surface-aware staleness (ADR-0050 §8, ADR-0062 A-G10): the number of
    /// unconsumed entries that the current run **could** have matched. Two
    /// independent conditions, both required:
    ///
    /// * the entry's id is inside the current run's surface (`in_surface`) — an id
    ///   this profile never looked for is **dormant**, kept and not counted; and
    /// * the entry's capture rung is at or below `rung` — an entry captured at
    ///   `strict` never cries unmatched on a `default` run, even for an id whose
    ///   floor would admit it, because that run did not analyze the same surface.
    ///
    /// Passing `Floor::Strict` with `|_| true` recovers the pre-ADR-0050
    /// unconditional stale count. Legacy untagged entries read as `Floor::Default`,
    /// so the second clause is vacuous for them and behavior is unchanged.
    #[must_use]
    pub fn stale_count_within(&self, rung: Floor, in_surface: impl Fn(&str) -> bool) -> usize {
        self.counts
            .iter()
            .filter(|((id, _, _), (n, captured))| *n > 0 && *captured <= rung && in_surface(id))
            .map(|(_, (n, _))| *n)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use steins_infer::Floor;

    use super::{CaptureSurface, Entry, Matcher, entry_hash, parse, parse_header, render};

    #[test]
    fn hash_is_line_number_independent_but_neighborhood_sensitive() {
        let a = "<?php\nfunction w(int $x): int { return $x; }\nw(\"abc\");\n";
        // Same flagged line and neighbors, shifted down by unrelated lines above.
        let b = "<?php\n\n// note\nfunction w(int $x): int { return $x; }\nw(\"abc\");\n";
        let ha = entry_hash("type.argument-mismatch", "a.php", a, 3);
        let hb = entry_hash("type.argument-mismatch", "a.php", b, 5);
        assert_eq!(ha, hb, "line-shift immunity");

        // Editing the flagged line changes the hash.
        let c = "<?php\nfunction w(int $x): int { return $x; }\nw(\"xyz\");\n";
        assert_ne!(ha, entry_hash("type.argument-mismatch", "a.php", c, 3));
        assert_eq!(ha.len(), 16, "16 hex chars");
    }

    #[test]
    fn matcher_consumes_one_for_one() {
        let e = Entry { id: "x".into(), path: "a".into(), hash: "h".into(), surface: None };
        let mut m = Matcher::new(&[e.clone(), e.clone()]);
        assert!(m.take("x", "a", "h"));
        assert!(m.take("x", "a", "h"));
        assert!(!m.take("x", "a", "h"), "third finding exhausts the two entries");
        assert_eq!(m.stale_count_within(Floor::Strict, |_| true), 0);
    }

    #[test]
    fn unconsumed_entries_are_stale() {
        let e = Entry { id: "x".into(), path: "a".into(), hash: "h".into(), surface: None };
        let m = Matcher::new(&[e]);
        assert_eq!(m.stale_count_within(Floor::Strict, |_| true), 1, "never matched → stale");
    }

    #[test]
    fn render_sorts_and_writes_header() {
        let surface = CaptureSurface {
            profile: "default".into(),
            ids: vec!["call.on-null".into(), "type.argument-mismatch".into()],
        };
        let out = render(
            vec![
                Entry { id: "b".into(), path: "z.php".into(), hash: "2".into(), surface: None },
                Entry { id: "a".into(), path: "a.php".into(), hash: "1".into(), surface: None },
            ],
            &surface,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains(r#""steins-baseline":1"#));
        assert!(lines[1].contains(r#""path":"a.php""#), "sorted by path first");
        assert!(lines[2].contains(r#""path":"z.php""#));
        // Field order id, path, hash.
        assert!(lines[1].starts_with(r#"{"id":"a","path":"a.php","hash":"1"}"#));
    }

    #[test]
    fn header_round_trips_capture_surface() {
        let surface = CaptureSurface {
            profile: "throws-direct".into(),
            ids: vec!["call.on-null".into(), "throw.undeclared".into()],
        };
        let out = render(vec![], &surface);
        assert_eq!(parse_header(&out), Some(surface));
    }

    #[test]
    fn pre_adr_0050_header_has_no_capture_surface() {
        // A legacy header with no profile/surface keys parses to None (skip notice).
        let legacy = "{\"steins-baseline\":1,\"note\":\"machine-managed; do not hand-edit\"}\n";
        assert_eq!(parse_header(legacy), None);
    }

    #[test]
    fn stale_within_treats_out_of_surface_entries_as_dormant() {
        let entries = vec![
            Entry { id: "throw.undeclared".into(), path: "a".into(), hash: "h1".into(), surface: None },
            Entry { id: "call.on-null".into(), path: "b".into(), hash: "h2".into(), surface: None },
        ];
        let m = Matcher::new(&entries);
        // Neither consumed. Under a proof-only surface, throw.undeclared is dormant.
        assert_eq!(m.stale_count_within(Floor::Strict, |_| true), 2, "raw stale counts both");
        assert_eq!(
            m.stale_count_within(Floor::Strict, |id| id == "call.on-null"),
            1,
            "only the in-surface entry is stale; the other is dormant"
        );
    }

    #[test]
    fn an_untagged_legacy_entry_reads_as_captured_at_default() {
        // The round-trip choice (ADR-0062 A-G10): absent tag = `default`, which makes
        // the rung clause vacuous and leaves legacy staleness decided by id alone.
        let e: Entry = serde_json::from_str(r#"{"id":"x","path":"a","hash":"h"}"#).unwrap();
        assert_eq!(e.surface, None);
        assert_eq!(e.captured_at(), Floor::Default);
        // An unrecognized spelling (a hand-edit) reads the same way.
        let odd: Entry =
            serde_json::from_str(r#"{"id":"x","path":"a","hash":"h","surface":"nope"}"#).unwrap();
        assert_eq!(odd.captured_at(), Floor::Default);
    }

    #[test]
    fn a_default_capture_writes_the_pre_s6_bytes() {
        // Byte-identity for the common case: no `surface` key is written at the
        // `default` rung, so a default baseline diffs clean against an older one.
        let surface = CaptureSurface { profile: "default".into(), ids: vec!["a".into()] };
        let out = render(
            vec![Entry {
                id: "a".into(),
                path: "a.php".into(),
                hash: "1".into(),
                surface: Entry::tag_for(Floor::Default),
            }],
            &surface,
        );
        let line = out.lines().nth(1).unwrap();
        assert_eq!(line, r#"{"id":"a","path":"a.php","hash":"1"}"#);
    }

    #[test]
    fn a_strict_capture_round_trips_its_rung() {
        let surface = CaptureSurface { profile: "strict".into(), ids: vec!["a".into()] };
        let out = render(
            vec![Entry {
                id: "a".into(),
                path: "a.php".into(),
                hash: "1".into(),
                surface: Entry::tag_for(Floor::Strict),
            }],
            &surface,
        );
        let line = out.lines().nth(1).unwrap();
        assert_eq!(line, r#"{"id":"a","path":"a.php","hash":"1","surface":"strict"}"#);
        let back = parse(&out);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].captured_at(), Floor::Strict);
    }

    #[test]
    fn a_strict_captured_entry_is_dormant_on_a_default_run() {
        // A-G10's rule, stated on the entry rather than only on the id: the run that
        // never analyzed the strict surface must not call the entry unmatched.
        let entries = vec![Entry {
            id: "call.on-null".into(),
            path: "a".into(),
            hash: "h".into(),
            surface: Some("strict".into()),
        }];
        let m = Matcher::new(&entries);
        // The id itself IS fireable at default — so only the capture rung can make
        // this dormant, which is exactly what the per-entry tag buys.
        assert_eq!(m.stale_count_within(Floor::Default, |_| true), 0, "dormant on a default run");
        assert_eq!(m.stale_count_within(Floor::Contracts, |_| true), 0, "still below strict");
        assert_eq!(m.stale_count_within(Floor::Strict, |_| true), 1, "judged on a strict run");
    }

    #[test]
    fn a_default_captured_entry_is_judged_on_every_run() {
        let entries = vec![Entry {
            id: "call.on-null".into(),
            path: "a".into(),
            hash: "h".into(),
            surface: None,
        }];
        let m = Matcher::new(&entries);
        for rung in [Floor::Default, Floor::Contracts, Floor::Strict] {
            assert_eq!(m.stale_count_within(rung, |_| true), 1, "{rung:?}");
        }
    }

    #[test]
    fn the_surface_tag_is_not_part_of_the_match_key() {
        // A finding matches its entry by `(id, path, hash)`; which surface either was
        // seen on is bookkeeping, never identity.
        let e = Entry {
            id: "x".into(),
            path: "a".into(),
            hash: "h".into(),
            surface: Some("strict".into()),
        };
        let mut m = Matcher::new(&[e]);
        assert!(m.take("x", "a", "h"), "a strict-captured entry still matches");
    }
}
