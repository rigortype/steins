//! `nsrt`: the assertType harness (oracle idea B).
//!
//! It consumes PHPStan's own `PHPStan\Testing\assertType('Type', $expr)` assertion
//! corpus (the `tests/PHPStan/Analyser/nsrt/` directory of a checked-out
//! phpstan-src) as an *oracle for inference*: PHPStan asserts the type it infers
//! for `$expr`, and this harness measures Steins' own rendering of the same
//! expression against it. The product is a ranked inventory of inference gaps to
//! drive the pre-release fix hunt.
//!
//! Recognition is the D3 dump-family seam extended (`steins_infer::collect_assert_types`):
//! `assertType` is matched by resolved FQN and `$expr` is rendered through the exact
//! `PHPStan\dumpType` path (best-fact + speller). It is **harness-only** — a normal
//! `check` never recognizes `assertType`.
//!
//! Each nsrt file is a standalone single-file universe (its own namespace, classes,
//! and `use function PHPStan\Testing\assertType;`), so files are analyzed as
//! SEPARATE single-file projects sharing one resident sidecar folder — fast, and
//! free of cross-file namespace collisions.
//!
//! Three-verdict taxonomy (see [`classify`]):
//!
//! - `match` — semantically equal after normalization (case, `|` order, nullable
//!   forms, int-range spelling). Generous only where equivalence is certain.
//! - `unsupported` — the expected string uses vocabulary Steins deliberately does
//!   not model (`*ERROR*`/`*NEVER*`, `mixed`, generics/shapes, intersections,
//!   `object`, array families, …), named by pattern.
//! - `differ` — Steins renders something semantically different (the gap
//!   inventory), including `unknown` where PHPStan asserts a concrete type (a
//!   reach gap).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{AssertObservation, SidecarFolder, collect_assert_types};

use crate::corpus::{collect_php_files, repo_root};

/// Entry point for `cargo xtask nsrt [DIR]`. `DIR` overrides the default nsrt path.
pub fn run(dir_arg: Option<&str>) -> Result<(), String> {
    let dir = match dir_arg {
        Some(d) => PathBuf::from(d),
        None => default_nsrt_dir(),
    };
    if !dir.is_dir() {
        return Err(format!(
            "nsrt directory not found: {}\n  pass the path explicitly: `cargo xtask nsrt <DIR>`",
            dir.display()
        ));
    }

    let mut files = Vec::new();
    collect_php_files(&dir, &mut files);
    files.sort();
    if files.is_empty() {
        return Err(format!("no .php files under {}", dir.display()));
    }
    println!("nsrt: analyzing {} files under {}\n", files.len(), dir.display());

    let start = Instant::now();

    // One resident sidecar folder, reused across every single-file project (the
    // fold posture the gate uses; ADR-0004). Analysis is single-threaded here — the
    // whole nsrt dir folds in seconds — so one folder is enough.
    let mut folder = SidecarFolder::enabled();

    let mut records: Vec<Record> = Vec::new();
    for f in &files {
        let name = f.strip_prefix(&dir).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => continue, // unreadable → contributes nothing
        };

        // Each file is its own single-file project (a standalone universe).
        let db = SteinsDatabase::default();
        let input = SourceFile::new(&db, name.clone(), text);
        let project = Project::new(&db, vec![input]);
        let observations = collect_assert_types(&db, project, &mut folder);
        for obs in observations {
            records.push(Record::classify(&name, obs));
        }
    }

    let elapsed = start.elapsed();

    report(&records, elapsed.as_secs_f64());
    write_json(&records)?;
    Ok(())
}

/// The default nsrt directory: a sibling phpstan-src checkout, relative to the repo.
fn default_nsrt_dir() -> PathBuf {
    // repo_root = …/repo/rust/steins ; php sibling = …/repo/php/phpstan-src.
    repo_root()
        .join("../../php/phpstan-src/tests/PHPStan/Analyser/nsrt")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("../../php/phpstan-src/tests/PHPStan/Analyser/nsrt"))
}

// ----------------------------------------------------------------------------
// classification
// ----------------------------------------------------------------------------

/// The three-verdict taxonomy. Observations whose expected slot could not be
/// resolved to a plain string (`::class`/concat) never reach [`classify`] — they are
/// recorded as the `"skipped"` housekeeping bucket directly in [`Record::classify`]
/// and kept out of the measurement denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Match,
    Unsupported,
    Differ,
}

/// One classified assertType observation.
#[derive(Debug, Clone, serde::Serialize)]
struct Record {
    file: String,
    line: u32,
    verdict: &'static str,
    /// The raw PHPStan expected string (or `<unresolved>` when skipped).
    expected: String,
    /// Steins' rendering.
    got: String,
    asserted: bool,
    /// For `unsupported`: the named vocabulary pattern. For `differ`: the coarse
    /// gap-class key. Empty for `match`/`skipped`.
    class: String,
}

impl Record {
    fn classify(file: &str, obs: AssertObservation) -> Record {
        let AssertObservation { line, expected, got, asserted, .. } = obs;
        let Some(expected) = expected else {
            return Record {
                file: file.to_owned(),
                line,
                verdict: "skipped",
                expected: "<unresolved>".to_owned(),
                got,
                asserted,
                class: String::new(),
            };
        };

        let (verdict, class) = classify(&expected, &got);
        Record {
            file: file.to_owned(),
            line,
            verdict: verdict_name(verdict),
            expected,
            got,
            asserted,
            class,
        }
    }
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Match => "match",
        Verdict::Unsupported => "unsupported",
        Verdict::Differ => "differ",
    }
}

/// Classify one (expected, got) pair. Unsupported-vocabulary expected strings are
/// classified first (Steins does not aim there); otherwise the two are normalized
/// and compared for certain semantic equivalence.
fn classify(expected: &str, got: &str) -> (Verdict, String) {
    if let Some(pattern) = unsupported_pattern(expected) {
        return (Verdict::Unsupported, pattern.to_owned());
    }
    if normalize(expected) == normalize(got) {
        return (Verdict::Match, String::new());
    }
    (Verdict::Differ, gap_class(expected, got))
}

// ----------------------------------------------------------------------------
// unsupported-vocabulary detection (named patterns)
// ----------------------------------------------------------------------------

/// If `expected` uses vocabulary Steins deliberately does not model, return the
/// named pattern; else `None` (it is a supported comparison). An expected string is
/// unsupported iff ANY of its top-level union atoms is unsupported; the returned
/// name is the category of the first such atom (priority order below).
fn unsupported_pattern(expected: &str) -> Option<&'static str> {
    let s = strip_outer_parens(expected.trim());
    // `?X` nullable prefix is supported (handled by the normalizer); everything
    // after this operates on the union atoms.
    let s = s.strip_prefix('?').map(str::trim).unwrap_or(s);
    for atom in split_union(s) {
        if let Some(cat) = atom_unsupported_category(atom.trim()) {
            return Some(cat);
        }
    }
    None
}

/// The unsupported category of a single atom, or `None` if the atom is one Steins
/// can model (a scalar/refined/int-range keyword, a literal, or a plain class name).
fn atom_unsupported_category(atom: &str) -> Option<&'static str> {
    let a = strip_outer_parens(atom).trim();
    let a = a.strip_prefix('?').map(str::trim).unwrap_or(a);

    // Supported shapes first — a positive test keeps the negative list honest.
    if is_supported_atom(a) {
        return None;
    }

    // PHPStan sentinels and set-algebra vocabulary.
    if a.contains('*') {
        return Some("phpstan-special"); // *ERROR*, *NEVER*
    }
    if a.contains('~') {
        return Some("subtraction"); // e.g. mixed~null
    }
    if a.contains('&') {
        return Some("intersection"); // int&object, T&hasMethod(...)
    }
    if a.contains('{') {
        return Some("array-shape"); // array{...}, list{...}
    }
    // A generic `Name<...>` (an int-range `int<lo, hi>` is supported and handled by
    // `is_supported_atom`, so any `<` reaching here is a true generic).
    if a.contains('<') {
        if a.contains("class-string") {
            return Some("class-string");
        }
        if a.starts_with("array<") || a.starts_with("non-empty-array<") {
            return Some("generic-array");
        }
        if a.starts_with("list<") || a.starts_with("non-empty-list<") {
            return Some("generic-list");
        }
        return Some("generic-other");
    }
    if a.starts_with("callable") || a.contains("Closure") || a.contains("\\Closure") {
        return Some("callable");
    }
    if a.contains("key-of") || a.contains("value-of") {
        return Some("key-of-value-of");
    }
    if a.contains("class-string") {
        return Some("class-string");
    }

    // Bare keyword atoms Steins does not render.
    let low = a.to_ascii_lowercase();
    match low.as_str() {
        "mixed" => Some("mixed"),
        "object" => Some("object"),
        "void" | "never" | "resource" | "scalar" | "empty" | "iterable" => Some("other-keyword"),
        "array" | "non-empty-array" => Some("array-family"),
        "list" | "non-empty-list" => Some("list-family"),
        "static" | "self" | "parent" | "$this" => Some("self-static"),
        "callable" => Some("callable"),
        "class-string" => Some("class-string"),
        "" => Some("empty-atom"),
        _ => {
            // A leftover token that is not a plain class name — anything with an
            // interior space or an unexpected punctuation lands here.
            if a.chars().any(|c| c.is_whitespace()) {
                Some("compound")
            } else {
                Some("other")
            }
        }
    }
}

/// Whether a single atom is one Steins can render (so it is fair to *compare*, not
/// classify unsupported). Scalar/refined/int-range keywords, scalar literals, and
/// plain class names all qualify.
fn is_supported_atom(a: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "int",
        "float",
        "string",
        "bool",
        "true",
        "false",
        "null",
        "non-empty-string",
        "non-falsy-string",
        "numeric-string",
        "positive-int",
        "negative-int",
        "non-negative-int",
    ];
    let low = a.to_ascii_lowercase();
    if KEYWORDS.contains(&low.as_str()) {
        return true;
    }
    if is_int_range(&low) {
        return true;
    }
    if is_int_literal(a) || is_float_literal(a) {
        return true;
    }
    if a.starts_with('\'') && a.ends_with('\'') && a.len() >= 2 {
        return true; // a string literal
    }
    // Reserved lowercase keywords look class-like but name vocabulary Steins does
    // not render as a class — they must NOT pass as a plain class name.
    if RESERVED_UNSUPPORTED_KEYWORDS.contains(&low.as_str()) {
        return false;
    }
    // A plain class name: `Foo`, `\Foo\Bar`, `Foo\Bar` — letters/digits/underscore
    // and namespace separators only, starting class-like.
    is_plain_class_name(a)
}

/// Bare lowercase keywords that are syntactically class-like but denote vocabulary
/// Steins does not model — never a plain class name, always an unsupported atom.
const RESERVED_UNSUPPORTED_KEYWORDS: &[&str] = &[
    "mixed", "object", "void", "never", "resource", "scalar", "empty", "iterable",
    "array", "list", "callable", "static", "self", "parent",
];

/// `int<lo, hi>` where lo/hi are `min`/`max`/signed integers (whitespace-tolerant).
fn is_int_range(low: &str) -> bool {
    let Some(inner) = low.strip_prefix("int<").and_then(|s| s.strip_suffix('>')) else {
        return false;
    };
    let mut parts = inner.split(',');
    let (Some(lo), Some(hi), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    is_range_bound(lo.trim()) && is_range_bound(hi.trim())
}

fn is_range_bound(b: &str) -> bool {
    b == "min" || b == "max" || is_int_literal(b)
}

fn is_int_literal(a: &str) -> bool {
    let t = a.strip_prefix('-').unwrap_or(a);
    !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit())
}

fn is_float_literal(a: &str) -> bool {
    let t = a.strip_prefix('-').unwrap_or(a);
    let mut dot = false;
    let mut digit = false;
    for c in t.chars() {
        match c {
            '0'..='9' => digit = true,
            '.' if !dot => dot = true,
            _ => return false,
        }
    }
    dot && digit
}

fn is_plain_class_name(a: &str) -> bool {
    let t = a.strip_prefix('\\').unwrap_or(a);
    if t.is_empty() {
        return false;
    }
    let first = t.as_bytes()[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'\\')
}

// ----------------------------------------------------------------------------
// normalization (certain equivalences only)
// ----------------------------------------------------------------------------

/// Canonicalize a supported type string for equivalence comparison: strip enclosing
/// parens, expand a leading `?` nullable, normalize int-range spelling and each
/// atom, then sort the union (so `|` order and duplicate atoms do not matter).
fn normalize(s: &str) -> String {
    let s = strip_outer_parens(s.trim());
    // `?X` ⇒ `X|null`. The union split below then carries the `null` atom.
    let expanded: String = if let Some(rest) = s.strip_prefix('?') {
        format!("{}|null", rest.trim())
    } else {
        s.to_owned()
    };
    let mut atoms: Vec<String> =
        split_union(&expanded).into_iter().map(|a| normalize_atom(a.trim())).collect();
    atoms.sort();
    atoms.dedup();
    atoms.join("|")
}

/// Canonicalize a single atom. String literals are kept verbatim (case-sensitive);
/// int-range spellings collapse to the named keyword form; everything else lowercases
/// (PHP scalar keywords and class names are case-insensitive).
fn normalize_atom(a: &str) -> String {
    let a = strip_outer_parens(a).trim();
    // A string literal: keep exactly (case & quoting are semantic).
    if a.starts_with('\'') {
        return a.to_owned();
    }
    let a = a.strip_prefix('\\').unwrap_or(a); // drop a leading namespace slash
    let low = a.to_ascii_lowercase();
    // Collapse the three int-range spellings onto one canonical keyword, and vice
    // versa, so `positive-int` == `int<1, max>` etc.
    match canonical_int_range(&low) {
        Some(canon) => canon,
        None => low,
    }
}

/// Map an int-range atom (either the named keyword or the `int<lo, hi>` interval)
/// to one canonical spelling, so the two forms compare equal. Returns `None` for a
/// non-int-range atom.
fn canonical_int_range(low: &str) -> Option<String> {
    match low {
        "positive-int" => return Some("int<1,max>".to_owned()),
        "non-negative-int" => return Some("int<0,max>".to_owned()),
        "negative-int" => return Some("int<min,-1>".to_owned()),
        _ => {}
    }
    let inner = low.strip_prefix("int<")?.strip_suffix('>')?;
    let mut parts = inner.split(',');
    let lo = parts.next()?.trim();
    let hi = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }
    if !is_range_bound(lo) || !is_range_bound(hi) {
        return None;
    }
    Some(format!("int<{lo},{hi}>"))
}

/// Strip fully-enclosing `(...)` pairs (repeatedly). A pair only encloses when its
/// opening paren matches the final closing paren at depth zero.
fn strip_outer_parens(s: &str) -> &str {
    let mut s = s.trim();
    loop {
        let bytes = s.as_bytes();
        if bytes.first() != Some(&b'(') || bytes.last() != Some(&b')') {
            return s;
        }
        // Verify the opening paren closes exactly at the end (not two siblings).
        let mut depth = 0i32;
        let mut encloses = true;
        for (i, ch) in s.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && i != s.len() - 1 {
                        encloses = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !encloses {
            return s;
        }
        s = s[1..s.len() - 1].trim();
    }
}

/// Split a type string on top-level `|`, respecting `'...'` string literals and the
/// nesting depth of `<>`, `{}`, and `()`. (Supported comparison strings carry no
/// brackets, but the splitter stays correct for the unsupported-detector's pass.)
fn split_union(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '\'' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_str = true,
            '<' | '{' | '(' => depth += 1,
            '>' | '}' | ')' => depth -= 1,
            '|' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

// ----------------------------------------------------------------------------
// gap-class heuristic (differ grouping)
// ----------------------------------------------------------------------------

/// A coarse gap-class key for a `differ`, keyed by the expected string's shape and
/// Steins' rendering kind — the ranking axis that drives the fix hunt.
fn gap_class(expected: &str, got: &str) -> String {
    format!("expected:{} | steins:{}", shape_of(expected), kind_of(got))
}

/// The coarse shape of the expected type: single-atom category, or a union label.
fn shape_of(s: &str) -> String {
    let norm = normalize(s);
    let atoms: Vec<&str> = norm.split('|').collect();
    if atoms.len() == 1 {
        return atom_kind(atoms[0]).to_owned();
    }
    let has_null = atoms.contains(&"null");
    let non_null: Vec<&str> = atoms.iter().copied().filter(|a| *a != "null").collect();
    if has_null && non_null.len() == 1 {
        return format!("nullable-{}", atom_kind(non_null[0]));
    }
    if non_null.iter().all(|a| is_scalarish(a)) {
        return if has_null { "scalar-union-null".to_owned() } else { "scalar-union".to_owned() };
    }
    "union".to_owned()
}

/// The coarse kind of Steins' rendering (its own vocabulary).
fn kind_of(got: &str) -> String {
    if got == "unknown" {
        return "unknown".to_owned();
    }
    if got == "no declared contract" {
        return "no-contract".to_owned();
    }
    let norm = normalize(got);
    let atoms: Vec<&str> = norm.split('|').collect();
    if atoms.len() == 1 {
        return atom_kind(atoms[0]).to_owned();
    }
    if atoms.iter().all(|a| is_scalarish(a)) {
        "scalar-union".to_owned()
    } else {
        "union".to_owned()
    }
}

/// The category of one normalized atom.
fn atom_kind(a: &str) -> &'static str {
    if a == "null" {
        return "null";
    }
    if a == "true" || a == "false" {
        return "bool-literal";
    }
    if a == "bool" {
        return "bool";
    }
    if a.starts_with('\'') {
        return "string-literal";
    }
    if is_int_literal(a) {
        return "int-literal";
    }
    if is_float_literal(a) {
        return "float-literal";
    }
    if a.starts_with("int<") {
        return "int-range";
    }
    match a {
        "int" => "int",
        "float" => "float",
        "string" => "string",
        "non-empty-string" | "non-falsy-string" | "numeric-string" => "refined-string",
        _ => {
            if is_plain_class_name(a) {
                "class"
            } else {
                "other"
            }
        }
    }
}

fn is_scalarish(a: &str) -> bool {
    !matches!(atom_kind(a), "class" | "other")
}

// ----------------------------------------------------------------------------
// reporting
// ----------------------------------------------------------------------------

fn report(records: &[Record], elapsed: f64) {
    let total = records.len();
    let count = |v: &str| records.iter().filter(|r| r.verdict == v).count();
    let (m, u, d, s) = (count("match"), count("unsupported"), count("differ"), count("skipped"));
    // The measurement denominator excludes skipped (unresolvable expected slots).
    let measured = m + u + d;
    let pct = |n: usize| if measured == 0 { 0.0 } else { 100.0 * n as f64 / measured as f64 };

    println!("=== nsrt assertType harness — verdict summary ===\n");
    println!("total assertType observations: {total}");
    println!("  skipped (expected unresolvable ::class/concat): {s}");
    println!("measured (match + unsupported + differ):          {measured}\n");
    println!("  {:<13} {:>6}   {:>6}", "verdict", "count", "% meas");
    println!("  {}", "-".repeat(30));
    println!("  {:<13} {:>6}   {:>5.1}%", "match", m, pct(m));
    println!("  {:<13} {:>6}   {:>5.1}%", "unsupported", u, pct(u));
    println!("  {:<13} {:>6}   {:>5.1}%", "differ", d, pct(d));
    println!("  {}", "-".repeat(30));
    println!("  {:<13} {:>6}   ({:.2}s)\n", "TOTAL meas", measured, elapsed);

    // Unsupported pattern breakdown.
    let mut unsup: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records.iter().filter(|r| r.verdict == "unsupported") {
        *unsup.entry(r.class.as_str()).or_insert(0) += 1;
    }
    let mut unsup_sorted: Vec<(&&str, &usize)> = unsup.iter().collect();
    unsup_sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("=== unsupported-vocabulary patterns ({u} total) ===\n");
    for (pat, n) in &unsup_sorted {
        println!("  {:<20} {:>6}", pat, n);
    }

    // Differ gap-class ranking.
    let mut gaps: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records.iter().filter(|r| r.verdict == "differ") {
        *gaps.entry(r.class.as_str()).or_insert(0) += 1;
    }
    let mut gaps_sorted: Vec<(&&str, &usize)> = gaps.iter().collect();
    gaps_sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\n=== differ gap-class ranking ({d} total) ===\n");
    for (gc, n) in &gaps_sorted {
        println!("  {:>6}  {}", n, gc);
    }

    // Top-30 differ listing (file:line, expected vs got).
    let differs: Vec<&Record> = records.iter().filter(|r| r.verdict == "differ").collect();
    println!("\n=== top-30 differs (expected vs got) ===\n");
    for r in differs.iter().take(30) {
        let mark = if r.asserted { " (asserted)" } else { "" };
        println!(
            "  {}:{}\n      expected: {}\n      got:      {}{}",
            r.file, r.line, r.expected, r.got, mark
        );
    }
    if differs.len() > 30 {
        println!("\n  … and {} more differs (see the JSON dump).", differs.len() - 30);
    }
}

/// Write the full machine-readable record set for the follow-up fix slices.
fn write_json(records: &[Record]) -> Result<(), String> {
    let scratch = scratch_dir();
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("cannot create scratch dir {}: {e}", scratch.display()))?;
    let path = scratch.join("nsrt-asserttype.json");
    let json = serde_json::to_string_pretty(records)
        .map_err(|e| format!("serializing records: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("writing {}: {e}", path.display()))?;
    println!("\nnsrt: wrote {} records to {}", records.len(), path.display());
    Ok(())
}

/// The session scratchpad directory (falls back to the repo `target/` if unset).
fn scratch_dir() -> PathBuf {
    std::env::var_os("CLAUDE_SCRATCHPAD")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target").join("nsrt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nullable_forms_are_equivalent() {
        assert_eq!(normalize("int|null"), normalize("?int"));
        assert_eq!(normalize("?int"), normalize("null|int"));
    }

    #[test]
    fn union_order_and_dedup() {
        assert_eq!(normalize("string|int"), normalize("int|string"));
        assert_eq!(normalize("int|int|string"), normalize("string|int"));
    }

    #[test]
    fn int_range_spellings_collapse() {
        assert_eq!(normalize("positive-int"), normalize("int<1, max>"));
        assert_eq!(normalize("non-negative-int"), normalize("int<0, max>"));
        assert_eq!(normalize("negative-int"), normalize("int<min, -1>"));
        // A genuinely different interval must NOT collapse to a named class.
        assert_ne!(normalize("positive-int"), normalize("int<2, max>"));
    }

    #[test]
    fn case_insensitivity_for_keywords_and_classes() {
        assert_eq!(normalize("INT"), normalize("int"));
        assert_eq!(normalize("\\Foo\\Bar"), normalize("Foo\\Bar"));
        assert_eq!(normalize("STDCLASS"), normalize("stdClass"));
    }

    #[test]
    fn string_literals_keep_case_and_order() {
        // Case is semantic for string literals.
        assert_ne!(normalize("'A'|'B'"), normalize("'a'|'b'"));
        // But order still does not matter.
        assert_eq!(normalize("'a'|'b'"), normalize("'b'|'a'"));
    }

    #[test]
    fn parenthesized_union_strips() {
        assert_eq!(normalize("(float|int)"), normalize("int|float"));
        assert_eq!(normalize("(DOMAttr|false)"), normalize("false|DOMAttr"));
    }

    #[test]
    fn classify_match_and_differ() {
        assert_eq!(classify("int", "int").0, Verdict::Match);
        assert_eq!(classify("positive-int", "int<1, max>").0, Verdict::Match);
        assert_eq!(classify("int", "unknown").0, Verdict::Differ);
        assert_eq!(classify("int", "string").0, Verdict::Differ);
    }

    #[test]
    fn unsupported_patterns_are_named() {
        assert_eq!(unsupported_pattern("*ERROR*"), Some("phpstan-special"));
        assert_eq!(unsupported_pattern("*NEVER*"), Some("phpstan-special"));
        assert_eq!(unsupported_pattern("mixed"), Some("mixed"));
        assert_eq!(unsupported_pattern("array{}"), Some("array-shape"));
        assert_eq!(unsupported_pattern("array<string>"), Some("generic-array"));
        assert_eq!(unsupported_pattern("list<int>"), Some("generic-list"));
        assert_eq!(unsupported_pattern("object"), Some("object"));
        assert_eq!(unsupported_pattern("non-empty-array"), Some("array-family"));
        assert_eq!(unsupported_pattern("int&object"), Some("intersection"));
        assert_eq!(unsupported_pattern("mixed~null"), Some("subtraction"));
        assert_eq!(unsupported_pattern("class-string<T>"), Some("class-string"));
        // Supported vocab returns None (fair to compare).
        assert_eq!(unsupported_pattern("int|null"), None);
        assert_eq!(unsupported_pattern("positive-int"), None);
        assert_eq!(unsupported_pattern("stdClass"), None);
        assert_eq!(unsupported_pattern("'foo'|'bar'"), None);
    }

    #[test]
    fn supported_atoms_are_not_flagged_unsupported() {
        for a in ["int", "float", "string", "bool", "true", "false", "null",
                  "non-empty-string", "numeric-string", "int<0, 5>", "-3", "1.5",
                  "'x'", "stdClass", "\\Foo\\Bar"] {
            assert!(is_supported_atom(a), "{a} should be supported");
        }
    }

    #[test]
    fn skipped_is_kept_out_of_measurement() {
        let obs = AssertObservation {
            path: "f.php".into(),
            line: 3,
            column: 1,
            expected: None,
            got: "unknown".into(),
            asserted: false,
        };
        let rec = Record::classify("f.php", obs);
        assert_eq!(rec.verdict, "skipped");
    }
}
