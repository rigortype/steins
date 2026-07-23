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
    /// Span of the whole *physical line* this tag was scanned from, within the
    /// docblock text (`[line_start, line_end)`, newline-exclusive). The transform
    /// engine (ADR-0034) uses this to delete a tag's entire line when promoting
    /// its type to a native declaration.
    pub line_span: Span,
    /// Span of the tag itself within the docblock text — from the `@` to the end
    /// of its last meaningful token (the `$var` for `@param`/`@var`/assert tags,
    /// the type/description tail otherwise). Narrower than [`Self::line_span`]
    /// (which includes the leading `*`-gutter and trailing whitespace); used for
    /// an in-line tag deletion when the line also carries docblock delimiters.
    pub tag_span: Span,
    /// The parameter/variable name (`$foo`) when the tag carries one.
    pub var_name: Option<String>,
    /// `true` when the tag was written with a `@phpstan-`/`@psalm-` prefix
    /// (`@phpstan-param`, `@psalm-return`, …). PHPStan gives these precedence over
    /// the plain `@param`/`@return` for the same target, so consumers should prefer
    /// a prefixed tag when both are present (ADR-0029).
    pub prefixed: bool,
    /// `true` when this is an assertion-family tag whose target is a property /
    /// `$this->…` position rather than a plain parameter. Such targets are parsed
    /// (so the tag is recognized, not treated as malformed) but carry **no
    /// exemption effect** in the current slice — a docblock property assertion says
    /// nothing about the acceptability of a call-site *argument*. See
    /// [`crate::docblock::TagKind::Assert`].
    pub assert_property_target: bool,
}

/// The three shapes of an assertion tag (PHPStan/Psalm `@…-assert` family).
///
/// An assertion tag narrows a target *after* the annotated function returns
/// (`Always`) or conditionally on its boolean result (`IfTrue`/`IfFalse`). The
/// declared type is therefore a **post-condition**, never a precondition — see
/// [`TagKind::Assert`] for why that matters to envelope checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertKind {
    /// `@phpstan-assert T $x` — holds unconditionally on normal return.
    Always,
    /// `@phpstan-assert-if-true T $x` — holds when the function returns `true`.
    IfTrue,
    /// `@phpstan-assert-if-false T $x` — holds when the function returns `false`.
    IfFalse,
}

/// The envelope-bearing tag kinds Steins reads, plus the assertion family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Param,
    Return,
    Var,
    Throws,
    /// An assertion tag (`@phpstan-assert` / `@psalm-assert` and the
    /// `-if-true`/`-if-false` variants). `negated` records the leading `!` of the
    /// negated form (`@phpstan-assert !T $x`). The declared type and target reuse
    /// the shared [`DocTag`] fields (`type_text` / `var_name`), so consumers read
    /// an assertion just like a `@param`; only these two facets are assert-specific.
    ///
    /// Only the **prefixed** spellings are recognized — PHPStan has no bare
    /// `@assert` tag, so an unprefixed `@assert` is not a tag at all.
    Assert { kind: AssertKind, negated: bool },
}

impl TagKind {
    /// Recognize a tag name, returning its kind and whether it carried a
    /// `@phpstan-`/`@psalm-` precedence prefix. Assert kinds are provisional here:
    /// `negated` is set to `false` and fixed up by [`scan_line`] once the type text
    /// (which carries the leading `!`) has been isolated.
    fn from_name(name: &str) -> Option<(TagKind, bool)> {
        let (bare, prefixed) = match name
            .strip_prefix("phpstan-")
            .or_else(|| name.strip_prefix("psalm-"))
        {
            Some(rest) => (rest, true),
            None => (name, false),
        };
        let kind = match bare {
            "param" => TagKind::Param,
            "return" => TagKind::Return,
            "var" => TagKind::Var,
            "throws" => TagKind::Throws,
            // Assertion tags exist only in prefixed form (`@phpstan-assert`,
            // `@psalm-assert`); a bare `@assert` is not a recognized tag.
            "assert" if prefixed => TagKind::Assert { kind: AssertKind::Always, negated: false },
            "assert-if-true" if prefixed => {
                TagKind::Assert { kind: AssertKind::IfTrue, negated: false }
            }
            "assert-if-false" if prefixed => {
                TagKind::Assert { kind: AssertKind::IfFalse, negated: false }
            }
            _ => return None,
        };
        Some((kind, prefixed))
    }

    fn carries_var_name(self) -> bool {
        matches!(self, TagKind::Param | TagKind::Var | TagKind::Assert { .. })
    }

    fn is_assert(self) -> bool {
        matches!(self, TagKind::Assert { .. })
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
    // The byte offset of the `@` — the start of the tag proper (past the gutter).
    let at_offset = i;
    // Read the tag name.
    let name_start = i + 1;
    let mut j = name_start;
    while j < line_end && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-') {
        j += 1;
    }
    let name = &text[name_start..j];
    let (mut kind, prefixed) = TagKind::from_name(name)?;

    // The remainder of the line, minus a trailing ` */` and whitespace.
    let mut rest_start = j;
    while rest_start < line_end && (bytes[rest_start] == b' ' || bytes[rest_start] == b'\t') {
        rest_start += 1;
    }

    // Assertion negation: `@phpstan-assert !T $x` puts a `!` in front of the type.
    // Strip it (and any following whitespace) off the type region and record the
    // negation flag on the tag kind, so the shared type/var extraction below sees a
    // clean type just like a `@param`.
    if kind.is_assert() && rest_start < line_end && bytes[rest_start] == b'!' {
        rest_start += 1;
        while rest_start < line_end && (bytes[rest_start] == b' ' || bytes[rest_start] == b'\t') {
            rest_start += 1;
        }
        if let TagKind::Assert { negated, .. } = &mut kind {
            *negated = true;
        }
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

    // For @param/@var/@…-assert, split the type off at the first `$variable`.
    let mut assert_property_target = false;
    let (type_start, type_end, var_name) = if kind.carries_var_name() {
        match find_variable(bytes, rest_start, rest_end) {
            Some(var_pos) => {
                let var_name = read_variable(text, bytes, var_pos, rest_end);
                // A `$this->prop` / `$obj->prop` / `$this::$static` assertion target
                // is a *property*, not a parameter: recognized (not malformed) but
                // exemption-inert this slice. Detect the accessor right after the
                // variable name, and treat a bare `$this` target likewise.
                let var_end = var_pos + var_name.len();
                let followed_by_accessor = bytes[var_end..rest_end.min(bytes.len())]
                    .starts_with(b"->")
                    || bytes[var_end..rest_end.min(bytes.len())].starts_with(b"::");
                if kind.is_assert() && (followed_by_accessor || var_name == "$this") {
                    assert_property_target = true;
                }
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
            // No `$var`. For `@param`/`@var` this is a bare `@var T`: the whole
            // remainder is the type. An assertion tag with no target is malformed —
            // ignore just this tag.
            None if kind.is_assert() => return None,
            None => (rest_start, rest_end, None),
        }
    } else {
        (rest_start, rest_end, None)
    };

    Some(DocTag {
        kind,
        type_text: text[type_start..type_end].to_owned(),
        type_span: Span::new(type_start as u32, type_end as u32),
        // The tag proper runs from its `@` to the end of its trimmed content
        // (`rest_end` already excludes a trailing `*/` and whitespace).
        tag_span: Span::new(at_offset as u32, rest_end as u32),
        // The whole physical line the tag was scanned from (newline-exclusive).
        line_span: Span::new(line_start as u32, line_end as u32),
        var_name,
        prefixed,
        assert_property_target,
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
    fn records_line_and_tag_spans() {
        let doc = "/**\n * @param int $x the count\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        let t = &tags[0];
        // The physical line is " * @param int $x the count" (no trailing newline).
        let line = &doc[t.line_span.start as usize..t.line_span.end as usize];
        assert_eq!(line, " * @param int $x the count");
        // The tag proper runs from the `@` to the end of the trimmed content.
        let tag = &doc[t.tag_span.start as usize..t.tag_span.end as usize];
        assert_eq!(tag, "@param int $x the count");
    }

    #[test]
    fn tag_span_on_single_line_docblock_excludes_delimiters() {
        let doc = "/** @param int $x */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        let t = &tags[0];
        let tag = &doc[t.tag_span.start as usize..t.tag_span.end as usize];
        assert_eq!(tag, "@param int $x");
        // The line span covers the whole single line including delimiters.
        let line = &doc[t.line_span.start as usize..t.line_span.end as usize];
        assert_eq!(line, "/** @param int $x */");
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

    // ---- Assertion family (@phpstan-assert / @psalm-assert) ----

    #[test]
    fn scans_plain_assert() {
        let doc = "/** @phpstan-assert int $x */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Assert { kind: AssertKind::Always, negated: false });
        assert_eq!(tags[0].type_text, "int");
        assert_eq!(tags[0].var_name.as_deref(), Some("$x"));
        assert!(tags[0].prefixed);
        assert!(!tags[0].assert_property_target);
    }

    #[test]
    fn scans_if_true_and_if_false() {
        let doc = "/**\n * @phpstan-assert-if-true non-empty-string $s\n \
                   * @phpstan-assert-if-false null $s\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].kind, TagKind::Assert { kind: AssertKind::IfTrue, negated: false });
        assert_eq!(tags[0].type_text, "non-empty-string");
        assert_eq!(tags[1].kind, TagKind::Assert { kind: AssertKind::IfFalse, negated: false });
        assert_eq!(tags[1].var_name.as_deref(), Some("$s"));
    }

    #[test]
    fn scans_negated_assert() {
        let doc = "/** @phpstan-assert !null $value */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Assert { kind: AssertKind::Always, negated: true });
        // The `!` is stripped off the type text.
        assert_eq!(tags[0].type_text, "null");
        assert_eq!(tags[0].var_name.as_deref(), Some("$value"));
    }

    #[test]
    fn psalm_prefix_is_accepted() {
        let doc = "/** @psalm-assert-if-true Foo $x */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Assert { kind: AssertKind::IfTrue, negated: false });
        assert!(tags[0].prefixed);
    }

    #[test]
    fn bare_assert_is_not_a_tag() {
        // PHPStan has no unprefixed `@assert`; it must not be recognized.
        let doc = "/** @assert int $x */";
        assert!(scan_docblock(doc).is_empty());
    }

    #[test]
    fn property_target_is_marked_unsupported() {
        for doc in [
            "/** @phpstan-assert int $this->prop */",
            "/** @phpstan-assert int $obj->field */",
            "/** @phpstan-assert int $this */",
        ] {
            let tags = scan_docblock(doc);
            assert_eq!(tags.len(), 1, "{doc}");
            assert!(tags[0].kind.is_assert());
            assert!(tags[0].assert_property_target, "{doc} should be a property target");
        }
    }

    #[test]
    fn malformed_assert_is_ignored_only() {
        // No target variable → this tag is dropped, siblings survive.
        let doc = "/**\n * @phpstan-assert int\n * @param string $s\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Param);
    }
}
