//! Inline `@steins-ignore` suppression (ADR-0023), following `@phpstan-ignore`'s
//! spec verbatim.
//!
//! A comment `@steins-ignore <id-list> (optional reason)` suppresses matching
//! object-level diagnostics. Placement copies `@phpstan-ignore`: a comment that
//! **trails code on a line** suppresses matching findings reported on *that* line;
//! a comment **alone on its own line** suppresses findings on the *next* line
//! ([`SourceTree::is_line_leading`] draws the distinction).
//!
//! IDs are registry-governed ([`DIAGNOSTIC_IDS`]) with ADR-0022 prefix semantics:
//! `type.*` (and bare `type`) matches `type.argument-mismatch`. Two always-on
//! meta-diagnostics keep the channel from rotting:
//!
//! * [`SUPPRESS_UNMATCHED_ID`] — an ignore id that matches nothing on its target
//!   line (the anti-rot mechanism), reported at the comment.
//! * [`SUPPRESS_UNKNOWN_ID`] — an unknown/malformed id in an ignore, reported at
//!   the comment.
//!
//! Meta-diagnostics are themselves **exempt** from both suppression channels
//! (suppressing the suppressor is a loop — ADR-0023's channels govern only
//! object-level findings), so this module never re-feeds them through matching.

use std::collections::HashSet;

use steins_syntax::SourceTree;

use crate::Diagnostic;
use crate::{
    CALL_ON_NULL_ID, EFFECT_ID, ID, PARAM_MISMATCH_ID, RETURN_ID, RETURN_MISMATCH_ID,
    UNKNOWN_LABEL_ID,
};

/// The registry id for an `@steins-ignore` whose diagnostic id matches nothing on
/// its target line (ADR-0023 anti-rot). Exempt from suppression.
pub const SUPPRESS_UNMATCHED_ID: &str = "suppress.unmatched";

/// The registry id for an unknown/malformed diagnostic id in an `@steins-ignore`
/// (ADR-0022 registry-governed). Exempt from suppression.
pub const SUPPRESS_UNKNOWN_ID: &str = "suppress.unknown-id";

/// The diagnostic-id registry (ADR-0022): the closed set of ids Steins currently
/// emits. `@steins-ignore` ids are validated against it (prefix-aware), and the
/// baseline records these ids verbatim.
pub const DIAGNOSTIC_IDS: &[&str] = &[
    ID,
    RETURN_ID,
    PARAM_MISMATCH_ID,
    RETURN_MISMATCH_ID,
    CALL_ON_NULL_ID,
    EFFECT_ID,
    UNKNOWN_LABEL_ID,
    SUPPRESS_UNMATCHED_ID,
    SUPPRESS_UNKNOWN_ID,
];

/// The result of applying inline ignores to a batch of object-level findings.
pub struct InlineOutcome {
    /// Object-level findings that were **not** suppressed (fed onward to the
    /// baseline channel and, ultimately, printed).
    pub kept: Vec<Diagnostic>,
    /// How many object-level findings inline ignores suppressed.
    pub suppressed: usize,
    /// The meta-diagnostics produced (`suppress.unmatched` / `suppress.unknown-id`).
    /// Never suppressed or baselined themselves.
    pub meta: Vec<Diagnostic>,
}

/// A parsed `@steins-ignore` directive from one comment.
struct Directive {
    /// The raw id tokens (comma-separated list; may include unknown/malformed).
    patterns: Vec<String>,
    /// The line the directive suppresses on (its own line if trailing, else next).
    target_line: u32,
    /// The comment's own 1-based line/column (where meta-diagnostics are reported).
    line: u32,
    column: u32,
}

/// Whether an ignore `pattern` (`type`, `type.*`, or `type.argument-mismatch`)
/// **matches** a concrete diagnostic `id` under ADR-0022 prefix subsumption: a
/// bare/`.*` family matches every id beneath it; an exact id matches itself.
/// Segment-aware, so `type` does not match `typex.*`.
fn pattern_matches(pattern: &str, id: &str) -> bool {
    let norm = pattern.strip_suffix(".*").unwrap_or(pattern);
    id == norm || id.strip_prefix(norm).is_some_and(|rest| rest.starts_with('.'))
}

/// Whether an ignore `pattern` is **registry-governed** (ADR-0022): after
/// stripping a trailing `.*`, it equals a registry id or is a family prefix of at
/// least one. Unknown/malformed patterns earn `suppress.unknown-id`.
#[must_use]
pub fn pattern_is_known(pattern: &str) -> bool {
    let norm = pattern.strip_suffix(".*").unwrap_or(pattern);
    if norm.is_empty() {
        return false;
    }
    DIAGNOSTIC_IDS
        .iter()
        .any(|&r| r == norm || r.strip_prefix(norm).is_some_and(|rest| rest.starts_with('.')))
}

/// Extract the text following `@steins-ignore` in a comment, trimming the comment
/// terminator (`*/`) and surrounding whitespace. `None` if the marker is absent.
fn extract_directive(text: &str) -> Option<&str> {
    let idx = text.find("@steins-ignore")?;
    let mut rest = &text[idx + "@steins-ignore".len()..];
    if let Some(end) = rest.find("*/") {
        rest = &rest[..end];
    }
    Some(rest.trim())
}

/// Parse the id list from a directive body: everything before an optional
/// parenthesized reason, comma-separated, trimmed, non-empty tokens.
fn parse_id_list(rest: &str) -> Vec<String> {
    let id_part = rest.find('(').map_or(rest, |p| &rest[..p]);
    id_part
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Collect every `@steins-ignore` directive in a file, resolving placement.
fn directives(tree: &SourceTree) -> Vec<Directive> {
    let mut out = Vec::new();
    for c in tree.comments() {
        let Some(body) = extract_directive(&c.text) else { continue };
        let patterns = parse_id_list(body);
        let pos = tree.position(c.span.start);
        // Trailing comment → this line; own-line comment → next line.
        let target_line =
            if tree.is_line_leading(c.span.start) { pos.line + 1 } else { pos.line };
        out.push(Directive { patterns, target_line, line: pos.line, column: pos.column });
    }
    out
}

/// Apply inline `@steins-ignore` suppression to object-level `findings`. `files`
/// pairs every analyzed file's diagnostic path with its parsed tree, so each
/// finding's comments can be consulted. Findings whose path is not among `files`
/// are kept untouched.
#[must_use]
pub fn apply_inline_ignores(
    findings: Vec<Diagnostic>,
    files: &[(String, &SourceTree)],
) -> InlineOutcome {
    let mut kept = Vec::new();
    let mut suppressed = 0usize;
    let mut meta = Vec::new();
    let known_paths: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

    for (path, tree) in files {
        let dirs = directives(tree);
        // Per-directive, per-pattern "used" flags, to drive `suppress.unmatched`.
        let mut used: Vec<Vec<bool>> = dirs.iter().map(|d| vec![false; d.patterns.len()]).collect();

        for f in findings.iter().filter(|f| &f.path == path) {
            let mut is_suppressed = false;
            for (di, d) in dirs.iter().enumerate() {
                if d.target_line != f.line {
                    continue;
                }
                for (pi, pat) in d.patterns.iter().enumerate() {
                    if pattern_is_known(pat) && pattern_matches(pat, f.id) {
                        used[di][pi] = true;
                        is_suppressed = true;
                    }
                }
            }
            if is_suppressed {
                suppressed += 1;
            } else {
                kept.push(f.clone());
            }
        }

        // Meta-diagnostics: unknown ids, then unmatched (still-unused) valid ids.
        for (di, d) in dirs.iter().enumerate() {
            if d.patterns.is_empty() {
                meta.push(meta_diag(
                    SUPPRESS_UNKNOWN_ID,
                    path,
                    d,
                    "malformed @steins-ignore (no diagnostic id given)".to_owned(),
                ));
                continue;
            }
            for (pi, pat) in d.patterns.iter().enumerate() {
                if !pattern_is_known(pat) {
                    meta.push(meta_diag(
                        SUPPRESS_UNKNOWN_ID,
                        path,
                        d,
                        format!("@steins-ignore names unknown diagnostic id '{pat}'"),
                    ));
                } else if !used[di][pi] {
                    meta.push(meta_diag(
                        SUPPRESS_UNMATCHED_ID,
                        path,
                        d,
                        format!(
                            "@steins-ignore of {pat} matches no diagnostic on line {}",
                            d.target_line
                        ),
                    ));
                }
            }
        }
    }

    // Findings for files not in the batch (should not arise) pass through.
    for f in findings {
        if !known_paths.contains(f.path.as_str()) {
            kept.push(f);
        }
    }

    InlineOutcome { kept, suppressed, meta }
}

/// Build a meta-diagnostic at a directive's comment location.
fn meta_diag(id: &'static str, path: &str, d: &Directive, message: String) -> Diagnostic {
    Diagnostic { id, path: path.to_owned(), line: d.line, column: d.column, message }
}

#[cfg(test)]
mod tests {
    use super::{extract_directive, parse_id_list, pattern_is_known, pattern_matches};

    #[test]
    fn prefix_and_bare_family_match() {
        assert!(pattern_matches("type.argument-mismatch", "type.argument-mismatch"));
        assert!(pattern_matches("type.*", "type.argument-mismatch"));
        assert!(pattern_matches("type", "type.argument-mismatch"));
        // The `type.return-mismatch` id joins the same `type.*` family.
        assert!(pattern_matches("type.return-mismatch", "type.return-mismatch"));
        assert!(pattern_matches("type.*", "type.return-mismatch"));
        assert!(pattern_matches("type", "type.return-mismatch"));
        assert!(pattern_matches("effect", "effect.envelope-exceeded"));
        // Segment-aware: `type` must not match a differently-rooted family.
        assert!(!pattern_matches("type", "typex.foo"));
        assert!(!pattern_matches("effect", "type.argument-mismatch"));
    }

    #[test]
    fn known_vs_unknown_ids() {
        assert!(pattern_is_known("type.argument-mismatch"));
        assert!(pattern_is_known("type.return-mismatch"));
        assert!(pattern_is_known("type.*"));
        assert!(pattern_is_known("type"));
        assert!(pattern_is_known("effect.envelope-exceeded"));
        assert!(pattern_is_known("suppress.unmatched"));
        // Typos and unknown families.
        assert!(!pattern_is_known("type.bogus"));
        assert!(!pattern_is_known("nope"));
        assert!(!pattern_is_known(""));
    }

    #[test]
    fn directive_extraction_handles_all_comment_shapes() {
        assert_eq!(extract_directive("// @steins-ignore type.x"), Some("type.x"));
        assert_eq!(extract_directive("# @steins-ignore type.x (why)"), Some("type.x (why)"));
        assert_eq!(extract_directive("/* @steins-ignore type.x */"), Some("type.x"));
        assert_eq!(extract_directive("// unrelated comment"), None);
    }

    #[test]
    fn id_list_splits_comma_and_strips_reason() {
        assert_eq!(parse_id_list("type.x, effect.y (reason here)"), vec!["type.x", "effect.y"]);
        assert_eq!(parse_id_list("type.x"), vec!["type.x"]);
        assert!(parse_id_list("(only a reason)").is_empty());
    }
}
