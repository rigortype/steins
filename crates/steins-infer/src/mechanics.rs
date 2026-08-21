//! Two mechanics-layer ids that stand alone: `syntax.unparsable` (ADR-0079 / issue
//! #180), the one finding a broken file emits, and `array.duplicate-key` (ADR-0078 /
//! issue #187) over a literal's normalized keys.

use steins_syntax::duplicate_array_keys;

use crate::{ARRAY_DUPLICATE_KEY_ID, Cx, Diagnostic, FileUnit, SYNTAX_UNPARSABLE_ID};

// ---------------------------------------------------------------------------
// `syntax.unparsable` (ADR-0079, issue #180, mechanics layer).
// ---------------------------------------------------------------------------

/// Emit the one `syntax.unparsable` finding a broken file earns (ADR-0079 §2.1):
/// positioned at the FIRST recovered parse error, naming the count of further
/// ones. One per file, never one per error — recovery cascades make every later
/// position unreliable, so those are reported as a *count* rather than as
/// positions (which would be guesses). A file that parses emits nothing.
///
/// `dams` is the ADR-0046 §2 vendor answer, read off the site list rather than
/// re-derived here so the finding's own words cannot drift from the dam's
/// behavior: a non-vendor break silences the existence family project-wide and
/// the message says so; a vendor break does not.
pub(crate) fn emit_parse_failure(unit: &FileUnit, dams: bool, out: &mut Vec<Diagnostic>) {
    let errors = unit.tree.parse_errors();
    let Some(first) = errors.first() else { return };
    let pos = unit.tree.position(first.span.start);
    let tail = match errors.len() - 1 {
        0 => String::new(),
        1 => " (and 1 further parse error in this file)".to_owned(),
        n => format!(" (and {n} further parse errors in this file)"),
    };
    let consequence = if dams {
        ", and while it stands no existence-absence claim can be proven anywhere in the project"
    } else {
        ""
    };
    out.push(Diagnostic {
        id: SYNTAX_UNPARSABLE_ID,
        path: unit.path.to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "this file does not parse — {}{tail}; nothing else is reported from it{consequence}",
            first.message,
        ),
        facet: None,
        fix: None,
    });
}

// ---------------------------------------------------------------------------
// `array.duplicate-key` (ADR-0078, issue #187, mechanics layer).
// ---------------------------------------------------------------------------

/// The `array.duplicate-key` check: every literal array expression in the
/// file, keys compared through `steins_syntax`'s own A12 coercion and
/// next-auto-index resolution (`duplicate_array_keys`) — no second coercion
/// table here. One finding per shadowed entry, positioned at the LATER
/// (winning) occurrence and naming the line of the earlier one it silently
/// overwrites.
pub(crate) fn check_array_duplicate_keys(cx: &Cx, out: &mut Vec<Diagnostic>) {
    for site in cx.tree().array_literal_sites() {
        for dup in duplicate_array_keys(site, cx.php_minor) {
            let winner_pos = cx.tree().position(dup.winner_span.start);
            let shadowed_pos = cx.tree().position(dup.shadowed_span.start);
            out.push(Diagnostic {
                id: ARRAY_DUPLICATE_KEY_ID,
                path: cx.path().to_owned(),
                line: winner_pos.line,
                column: winner_pos.column,
                message: format!(
                    "array key {} is declared twice — this entry silently overwrites the earlier one at line {}",
                    dup.key.render(),
                    shadowed_pos.line,
                ),
                facet: None,
                fix: None,
            });
        }
    }
}
