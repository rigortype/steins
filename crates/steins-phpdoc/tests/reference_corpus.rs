//! Ported reference test corpus (ADR-0029): every phpstan/phpdoc-parser `TypeParserTest`
//! input, paired with the real parser's verdict (`harness/phpdoc-oracle/dump.php`).
//!
//! A few inputs are Steins' own additions rather than upstream's, where a spelling
//! Steins gives its own *meaning* still has to parse and canonicalize exactly as
//! the reference parser does — the `unset` pseudo-type (ADR-0087) is one: the
//! grammar agrees on `\DateTime|unset`, only the lowering diverges.
//!
//! Compatibility: OK/PARTIAL + we parse → `Display`/at-end must match *exactly*, else a
//! FAILURE (wrong parse). OK/PARTIAL + outside our subset → error/`Unsupported`, a
//! coverage gap not a failure. Reference errors → we must too, else FAILURE.
//!
//! Coverage ratio (exact/total) is printed; zero wrong-parse failures is the hard
//! invariant, coverage is informational.

use std::fmt::Write as _;

use steins_phpdoc::{TypeKind, parse_type};

#[derive(Debug, Clone, PartialEq, Eq)]
enum RefVerdict {
    /// Entire input consumed; payload is the canonical `__toString`.
    Ok(String),
    /// Prefix parsed, trailing tokens remain; payload is the canonical form.
    Partial(String),
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OurVerdict {
    Ok(String),
    Partial(String),
    Error,
    /// An `Unsupported` node — silence, safe.
    Unsupported,
    /// Not valid UTF-8 (can't ingest as `&str`): skipped.
    NonUtf8,
}

fn c_unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'\\' => out.push('\\'),
                other => {
                    out.push('\\');
                    out.push(other as char);
                }
            }
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn parse_ref_verdict(line: &str) -> RefVerdict {
    if let Some(rest) = line.strip_prefix("OK\t") {
        RefVerdict::Ok(rest.to_owned())
    } else if let Some(rest) = line.strip_prefix("PARTIAL\t") {
        RefVerdict::Partial(rest.to_owned())
    } else if line.starts_with("ERROR\t") {
        RefVerdict::Error
    } else {
        panic!("malformed .expected line: {line:?}");
    }
}

/// Escape control chars so the canonical form stays single-line, matching
/// `dump.php`'s escaping of the reference `__toString`.
fn escape_controls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn contains_unsupported(kind: &TypeKind) -> bool {
    matches!(kind, TypeKind::Unsupported(_))
}

fn our_verdict(input_line_bytes: &[u8]) -> OurVerdict {
    let input = match std::str::from_utf8(input_line_bytes) {
        Ok(s) => s,
        Err(_) => return OurVerdict::NonUtf8,
    };
    let input = c_unescape(input);
    match parse_type(&input) {
        Ok(p) => {
            if contains_unsupported(&p.ty.kind) {
                return OurVerdict::Unsupported;
            }
            let canon = escape_controls(&p.ty.to_string());
            if p.at_end {
                OurVerdict::Ok(canon)
            } else {
                OurVerdict::Partial(canon)
            }
        }
        Err(_) => OurVerdict::Error,
    }
}

#[test]
fn reference_corpus_compatibility() {
    // Bytes, not &str: input has one non-UTF-8 line (raw 0xA0 identifier byte).
    let inputs_raw = include_bytes!("fixtures/reference-types.txt");
    let expected_raw = include_bytes!("fixtures/reference-types.expected");

    let input_lines: Vec<&[u8]> = split_lines(inputs_raw);
    // .expected has comment/blank header lines with nothing in the input to match.
    let expected_lines: Vec<String> = split_lines(expected_raw)
        .into_iter()
        .filter(|l| !l.is_empty() && l[0] != b'#')
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect();

    // Drop trailing empty input line (from a final newline).
    let input_lines: Vec<&[u8]> = input_lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect();

    assert_eq!(
        input_lines.len(),
        expected_lines.len(),
        "fixture .txt and .expected are misaligned ({} inputs vs {} verdicts)",
        input_lines.len(),
        expected_lines.len()
    );

    let total = input_lines.len();
    let mut matched = 0usize;
    let mut coverage_gap = 0usize;
    let mut non_utf8 = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (raw_input, exp_line) in input_lines.iter().zip(&expected_lines) {
        let refv = parse_ref_verdict(exp_line);
        let ours = our_verdict(raw_input);
        let input_disp = String::from_utf8_lossy(raw_input).into_owned();

        match (&refv, &ours) {
            (_, OurVerdict::NonUtf8) => non_utf8 += 1,

            (RefVerdict::Error, OurVerdict::Error | OurVerdict::Unsupported) => matched += 1,
            (RefVerdict::Error, OurVerdict::Ok(c) | OurVerdict::Partial(c)) => {
                failures.push(format!(
                    "WRONG-ACCEPT `{input_disp}`: reference REJECTS, we produced `{c}`"
                ));
            }

            (RefVerdict::Ok(rc), OurVerdict::Ok(oc)) => {
                if rc == oc {
                    matched += 1;
                } else {
                    failures.push(format!(
                        "MISMATCH `{input_disp}`: reference `{rc}` vs ours `{oc}`"
                    ));
                }
            }
            (RefVerdict::Partial(rc), OurVerdict::Partial(oc)) => {
                if rc == oc {
                    matched += 1;
                } else {
                    failures.push(format!(
                        "MISMATCH (partial) `{input_disp}`: reference `{rc}` vs ours `{oc}`"
                    ));
                }
            }
            (
                RefVerdict::Ok(_) | RefVerdict::Partial(_),
                OurVerdict::Error | OurVerdict::Unsupported,
            ) => coverage_gap += 1,

            (RefVerdict::Ok(rc), OurVerdict::Partial(oc)) => failures.push(format!(
                "BOUNDARY `{input_disp}`: reference fully parses `{rc}`, we stopped early at `{oc}`"
            )),
            (RefVerdict::Partial(rc), OurVerdict::Ok(oc)) => failures.push(format!(
                "BOUNDARY `{input_disp}`: reference partial `{rc}`, we consumed all as `{oc}`"
            )),
        }
    }

    let mut report = String::new();
    let _ = writeln!(report, "\n=== steins-phpdoc reference-corpus compatibility ===");
    let _ = writeln!(report, "total fixtures:      {total}");
    let _ = writeln!(
        report,
        "exact agreement:     {matched}  ({:.1}%)",
        100.0 * matched as f64 / total as f64
    );
    let _ = writeln!(report, "coverage gaps:       {coverage_gap}  (reference accepts, we are silent)");
    let _ = writeln!(report, "non-UTF-8 skipped:   {non_utf8}");
    let _ = writeln!(report, "wrong-parse failures:{}", failures.len());
    println!("{report}");

    if !failures.is_empty() {
        let mut msg = format!("{} wrong-parse disagreement(s):\n", failures.len());
        for f in &failures {
            let _ = writeln!(msg, "  - {f}");
        }
        panic!("{msg}\n{report}");
    }

    // Guard against silent regression: we should match the large majority outright.
    assert!(
        matched * 100 >= total * 80,
        "subset coverage regressed: only {matched}/{total} exact matches"
    );
}

/// Split raw bytes into lines on `\n`, dropping a single trailing `\r` per line.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            let mut end = i;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(&bytes[start..end]);
            start = i + 1;
        }
    }
    if start < bytes.len() {
        lines.push(&bytes[start..]);
    }
    lines
}
