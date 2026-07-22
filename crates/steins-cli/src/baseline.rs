//! The `.steins-baseline.jsonl` channel (ADR-0022): the accumulated past at
//! adoption, machine-managed, line-shift-immune.
//!
//! # Format
//!
//! JSONL. A header line `{"steins-baseline":1,"note":"…"}`, then one
//! `{"id","path","hash"}` entry per line, sorted by `(path, id, hash)` for diff
//! stability. `path` is relative to the baseline file's directory, forward
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sha256;

/// One baseline entry. Field order is the on-disk key order (serde preserves
/// struct field order): `{"id":…,"path":…,"hash":…}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub hash: String,
}

/// The default baseline filename, looked up in the CWD (ADR-0022).
pub const DEFAULT_FILE: &str = ".steins-baseline.jsonl";

/// The header line written first (machine-managed marker).
const HEADER: &str = r#"{"steins-baseline":1,"note":"machine-managed; do not hand-edit"}"#;

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
/// text of a baseline file, header included.
#[must_use]
pub fn render(mut entries: Vec<Entry>) -> String {
    entries.sort_by(|a, b| {
        (a.path.as_str(), a.id.as_str(), a.hash.as_str()).cmp(&(
            b.path.as_str(),
            b.id.as_str(),
            b.hash.as_str(),
        ))
    });
    let mut out = String::new();
    out.push_str(HEADER);
    out.push('\n');
    for e in &entries {
        // A derived-struct serialize never fails and keeps field order.
        out.push_str(&serde_json::to_string(e).expect("serialize baseline entry"));
        out.push('\n');
    }
    out
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
    counts: HashMap<(String, String, String), usize>,
}

impl Matcher {
    #[must_use]
    pub fn new(entries: &[Entry]) -> Self {
        let mut counts: HashMap<(String, String, String), usize> = HashMap::new();
        for e in entries {
            *counts.entry((e.id.clone(), e.path.clone(), e.hash.clone())).or_insert(0) += 1;
        }
        Self { counts }
    }

    /// Try to consume one entry matching `(id, path, hash)`. Returns `true` (and
    /// decrements) on a match, `false` when no unconsumed entry remains.
    pub fn take(&mut self, id: &str, path: &str, hash: &str) -> bool {
        let key = (id.to_owned(), path.to_owned(), hash.to_owned());
        match self.counts.get_mut(&key) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    }

    /// The number of baseline entries never consumed (stale — the flagged code
    /// changed or the finding was fixed).
    #[must_use]
    pub fn stale_count(&self) -> usize {
        self.counts.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, Matcher, entry_hash, render};

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
        let e = Entry { id: "x".into(), path: "a".into(), hash: "h".into() };
        let mut m = Matcher::new(&[e.clone(), e.clone()]);
        assert!(m.take("x", "a", "h"));
        assert!(m.take("x", "a", "h"));
        assert!(!m.take("x", "a", "h"), "third finding exhausts the two entries");
        assert_eq!(m.stale_count(), 0);
    }

    #[test]
    fn unconsumed_entries_are_stale() {
        let e = Entry { id: "x".into(), path: "a".into(), hash: "h".into() };
        let m = Matcher::new(&[e]);
        assert_eq!(m.stale_count(), 1, "never matched → stale");
    }

    #[test]
    fn render_sorts_and_writes_header() {
        let out = render(vec![
            Entry { id: "b".into(), path: "z.php".into(), hash: "2".into() },
            Entry { id: "a".into(), path: "a.php".into(), hash: "1".into() },
        ]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains(r#""steins-baseline":1"#));
        assert!(lines[1].contains(r#""path":"a.php""#), "sorted by path first");
        assert!(lines[2].contains(r#""path":"z.php""#));
        // Field order id, path, hash.
        assert!(lines[1].starts_with(r#"{"id":"a","path":"a.php","hash":"1"}"#));
    }
}
