//! A thin docblock scanner: it pulls typed tags (`@param`, `@return`, `@var`,
//! `@throws`) out of a raw `/** … */` comment together with the byte span of the
//! candidate type expression.
//!
//! This is deliberately *not* a full PhpDoc parser — it is the seam that feeds
//! type strings to [`crate::parse_type`] (ADR-0029). It scans physical lines, so
//! a type that wraps across lines (a rare multi-line `array{…}` in a `@param`) is
//! not reassembled; such a tag is simply not emitted, which is safe — a missing
//! envelope only silences.
//!
//! Spans are relative to the start of the passed text; add the docblock's own
//! source offset to map them back into a file.

use crate::ast::Span;

/// A typed tag recovered from a docblock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTag {
    pub kind: TagKind,
    /// The candidate type-expression text (leading/trailing whitespace trimmed).
    /// For `@return`/`@throws`/`@var` this may still carry a trailing
    /// description; [`crate::parse_type`] consumes only the type prefix.
    pub type_text: String,
    /// Span of `type_text` within the scanned docblock text.
    pub type_span: Span,
    /// The parameter/variable name (`$foo`) when the tag carries one.
    pub var_name: Option<String>,
}

/// The four envelope-bearing tag kinds Steins reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Param,
    Return,
    Var,
    Throws,
}

impl TagKind {
    fn from_name(name: &str) -> Option<TagKind> {
        match name {
            "param" => Some(TagKind::Param),
            "return" => Some(TagKind::Return),
            "var" => Some(TagKind::Var),
            "throws" => Some(TagKind::Throws),
            _ => None,
        }
    }

    fn carries_var_name(self) -> bool {
        matches!(self, TagKind::Param | TagKind::Var)
    }
}

/// Scan a raw docblock (or any text) for typed tags.
pub fn scan_docblock(text: &str) -> Vec<DocTag> {
    let bytes = text.as_bytes();
    let mut tags = Vec::new();
    let mut line_start = 0usize;

    // Walk physical lines. On each line, strip a leading run of whitespace and
    // `*` (the docblock gutter), then look for a leading `@tag`.
    while line_start <= bytes.len() {
        let line_end = memchr(bytes, line_start, b'\n').unwrap_or(bytes.len());
        if let Some(tag) = scan_line(text, line_start, line_end) {
            tags.push(tag);
        }
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end + 1;
    }
    tags
}

fn scan_line(text: &str, line_start: usize, line_end: usize) -> Option<DocTag> {
    let bytes = text.as_bytes();
    // Skip the gutter: whitespace, then optional `*`, then whitespace.
    let mut i = line_start;
    while i < line_end && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    // A leading `/**` also counts as gutter.
    if i + 2 < line_end && &bytes[i..i + 3] == b"/**" {
        i += 3;
    }
    while i < line_end && bytes[i] == b'*' {
        i += 1;
    }
    while i < line_end && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }

    if i >= line_end || bytes[i] != b'@' {
        return None;
    }
    // Read the tag name.
    let name_start = i + 1;
    let mut j = name_start;
    while j < line_end && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-') {
        j += 1;
    }
    let name = &text[name_start..j];
    let kind = TagKind::from_name(name)?;

    // The remainder of the line, minus a trailing ` */` and whitespace.
    let mut rest_start = j;
    while rest_start < line_end && (bytes[rest_start] == b' ' || bytes[rest_start] == b'\t') {
        rest_start += 1;
    }
    let mut rest_end = line_end;
    // Trim trailing `*/` and whitespace.
    while rest_end > rest_start
        && (bytes[rest_end - 1] == b' '
            || bytes[rest_end - 1] == b'\t'
            || bytes[rest_end - 1] == b'\r')
    {
        rest_end -= 1;
    }
    if rest_end >= rest_start + 2 && &bytes[rest_end - 2..rest_end] == b"*/" {
        rest_end -= 2;
        while rest_end > rest_start
            && (bytes[rest_end - 1] == b' ' || bytes[rest_end - 1] == b'\t')
        {
            rest_end -= 1;
        }
    }
    if rest_start >= rest_end {
        return None;
    }

    // For @param/@var, split the type off at the first `$variable`.
    let (type_start, type_end, var_name) = if kind.carries_var_name() {
        match find_variable(bytes, rest_start, rest_end) {
            Some(var_pos) => {
                let var_name = read_variable(text, bytes, var_pos, rest_end);
                // Type is everything before the variable (trimmed).
                let mut te = var_pos;
                while te > rest_start && (bytes[te - 1] == b' ' || bytes[te - 1] == b'\t') {
                    te -= 1;
                }
                if te <= rest_start {
                    // `@param $x` with no type — nothing to offer.
                    return None;
                }
                (rest_start, te, Some(var_name))
            }
            // No `$var`: treat the whole remainder as the type (e.g. bare `@var T`).
            None => (rest_start, rest_end, None),
        }
    } else {
        (rest_start, rest_end, None)
    };

    Some(DocTag {
        kind,
        type_text: text[type_start..type_end].to_owned(),
        type_span: Span::new(type_start as u32, type_end as u32),
        var_name,
    })
}

/// Find the byte offset of the first `$name` variable within `[start, end)` that
/// is not part of a `$this`-in-type position. We accept the first `$` followed by
/// an identifier char — good enough for `@param T $x`.
fn find_variable(bytes: &[u8], start: usize, end: usize) -> Option<usize> {
    let mut i = start;
    while i < end {
        if bytes[i] == b'$'
            && i + 1 < end
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_' || bytes[i + 1] >= 0x80)
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn read_variable(text: &str, bytes: &[u8], pos: usize, end: usize) -> String {
    let mut j = pos + 1;
    while j < end && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] >= 0x80) {
        j += 1;
    }
    text[pos..j].to_owned()
}

fn memchr(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    (from..bytes.len()).find(|&i| bytes[i] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_param_type_and_name() {
        let doc = "/**\n * @param array<int, string> $items the items\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Param);
        assert_eq!(tags[0].type_text, "array<int, string>");
        assert_eq!(tags[0].var_name.as_deref(), Some("$items"));
        // Span should point at the type text within the docblock.
        let s = tags[0].type_span;
        assert_eq!(&doc[s.start as usize..s.end as usize], "array<int, string>");
    }

    #[test]
    fn extracts_return_and_throws() {
        let doc = "/**\n * @return int|null the count\n * @throws \\RuntimeException\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].kind, TagKind::Return);
        assert_eq!(tags[0].type_text, "int|null the count");
        assert_eq!(tags[1].kind, TagKind::Throws);
        assert_eq!(tags[1].type_text, "\\RuntimeException");
    }

    #[test]
    fn extracts_var_without_name() {
        let doc = "/** @var non-empty-list<string> */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Var);
        assert_eq!(tags[0].type_text, "non-empty-list<string>");
    }

    #[test]
    fn ignores_untyped_tags() {
        let doc = "/**\n * @deprecated do not use\n * @see Foo::bar\n */";
        assert!(scan_docblock(doc).is_empty());
    }
}
