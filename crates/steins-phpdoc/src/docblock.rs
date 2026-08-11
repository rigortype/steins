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
    /// `true` when this is an assertion-family or `@var` tag whose target is a
    /// property / `$this->…` position rather than a plain variable. Such targets
    /// are parsed (so the tag is recognized, not treated as malformed) but the
    /// consumers that act on a *variable* — the assert-exemption reader and the
    /// inline-`@var` cast seeding (ADR-0073) — must skip them: an assertion on a
    /// property says nothing about a call-site argument, and a `@var` naming
    /// `$obj->prop` speaks about the property, never about `$obj` itself (acting
    /// on the receiver there could manufacture findings). See
    /// [`crate::docblock::TagKind::Assert`].
    pub property_target: bool,
    /// The **effect labels** of an interop envelope (ADR-0082), in source order —
    /// the `io.db` / `nondet.time` of `@phpstan-impure io.db, nondet.time`. Raw
    /// dot-path strings exactly as written: recognizing a label is this scanner's
    /// job, validating it against the effect registry is not. Empty for every
    /// other tag kind, and for an envelope written without labels (the ⊤ bound of
    /// a bare `@phpstan-all-methods-impure`).
    pub labels: Vec<String>,
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

/// Which **conditional-purity** contract a tag declares (ADR-0063 §2 decision 2).
///
/// Both spellings are merged upstream in `phpstan/phpdoc-parser` 2.3.3
/// (`PureUnlessCallableIsImpureTagValueNode`, `PureUnlessParameterIsPassedTagValueNode`),
/// whose grammar for either tag is `parseRequiredVariableName` followed by an
/// optional description — no type. Steins honors the spelling as merged; it does
/// not invent one (ADR-0016 lets us lead, but only where upstream has settled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurityCondition {
    /// `@pure-unless-callable-is-impure $cb` — the declaring function is pure
    /// except for whatever the callable bound to `$cb` does.
    CallableIsImpure,
    /// `@pure-unless-parameter-passed $out` — the declaring function is pure
    /// unless the named parameter is actually supplied at the call site. The
    /// by-ref sister of the callable form, and the declarative twin of P2's
    /// conditional out-param rows.
    ParameterIsPassed,
}

impl PurityCondition {
    /// Recognize a conditional-purity tag name, returning the condition and
    /// whether it carried the `@phpstan-` precedence prefix.
    ///
    /// Upstream registers exactly the bare and `@phpstan-`-prefixed spellings for
    /// this family — there is **no** `@psalm-` alias — so unlike the rest of
    /// [`TagKind::from_name`] this does not strip `psalm-`.
    fn from_tag_name(name: &str) -> Option<(Self, bool)> {
        let (bare, prefixed) = match name.strip_prefix("phpstan-") {
            Some(rest) => (rest, true),
            None => (name, false),
        };
        let cond = match bare {
            "pure-unless-callable-is-impure" => Self::CallableIsImpure,
            "pure-unless-parameter-passed" => Self::ParameterIsPassed,
            _ => return None,
        };
        Some((cond, prefixed))
    }
}

/// Which **interop-envelope** family a purity tag belongs to (ADR-0082).
///
/// An interop envelope is the *unchecked* docblock spelling of an effect envelope:
/// upstream's own purity tags, parameterized with a label list at upstream's own
/// suggestion. The label list rides in [`DocTag::labels`]; this enum records only
/// which tag was written, and nothing here interprets either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeTag {
    /// `@pure` / `@psalm-pure` / `@phpstan-pure` — the empty envelope (Steins reads
    /// it as `{mutate.local}`, ADR-0082 §3). Takes no labels: any trailing text is
    /// an ignored description, and the tag is recognized regardless.
    Pure,
    /// `@impure` / `@phpstan-impure` **with** a conforming label list — the bound
    /// `≤ labels`. A bare (or nonconforming) one is ⊤, which adds no information,
    /// so it is not a tag at all and never reaches this enum (ADR-0082 §3).
    Impure,
    /// `@phpstan-all-methods-pure` — the class-level fallback, no labels.
    AllMethodsPure,
    /// `@phpstan-all-methods-impure`, bare or labeled. Unlike the method-level
    /// impure tag this **is** a tag when bare: upstream gives it standing meaning
    /// of its own, and the empty label list is then the ⊤ bound.
    AllMethodsImpure,
}

impl EnvelopeTag {
    /// Recognize an interop-envelope tag name, returning the family and whether it
    /// carried a `@phpstan-`/`@psalm-` precedence prefix.
    ///
    /// Spellings are matched whole, before the shared prefix strip of
    /// [`TagKind::from_name`], because the accepted set is **not** uniform across
    /// the family: PHPStan implements `@impure` / `@phpstan-impure` with no
    /// `@psalm-` alias, and the class-level pair exists under its one spelling
    /// only. Deviation from the spec's upstream list: the `@phan-pure` /
    /// `@phan-side-effect-free` spellings are skipped — Steins reads no `@phan-`
    /// tag anywhere, and this slice does not open that surface.
    fn from_tag_name(name: &str) -> Option<(Self, bool)> {
        Some(match name {
            "pure" => (Self::Pure, false),
            "phpstan-pure" | "psalm-pure" => (Self::Pure, true),
            "impure" => (Self::Impure, false),
            "phpstan-impure" => (Self::Impure, true),
            // The class-level pair has no unprefixed spelling to be the plain
            // sibling of, so `prefixed` records the `@phpstan-` it is written with.
            "phpstan-all-methods-pure" => (Self::AllMethodsPure, true),
            "phpstan-all-methods-impure" => (Self::AllMethodsImpure, true),
            _ => return None,
        })
    }

    /// Whether the family accepts a label list at all. The pure side never does:
    /// "pure, except it performs effects" is a contradiction, so a trailing text is
    /// read as a description (upstream behavior) rather than as a bound.
    const fn takes_labels(self) -> bool {
        matches!(self, Self::Impure | Self::AllMethodsImpure)
    }
}

/// The envelope-bearing tag kinds Steins reads, plus the assertion family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Param,
    Return,
    Var,
    Throws,
    /// A conditional-purity tag (`@pure-unless-callable-is-impure $cb` and its
    /// by-ref sister). It carries **no type**: the payload is the parameter name
    /// alone, in the shared [`DocTag::var_name`] field, and `type_text` is empty.
    ConditionalPurity(PurityCondition),
    /// An assertion tag (`@phpstan-assert` / `@psalm-assert` and the
    /// `-if-true`/`-if-false` variants). `negated` records the leading `!` of the
    /// negated form (`@phpstan-assert !T $x`). The declared type and target reuse
    /// the shared [`DocTag`] fields (`type_text` / `var_name`), so consumers read
    /// an assertion just like a `@param`; only these two facets are assert-specific.
    ///
    /// Only the **prefixed** spellings are recognized — PHPStan has no bare
    /// `@assert` tag, so an unprefixed `@assert` is not a tag at all.
    Assert { kind: AssertKind, negated: bool },
    /// A trace annotation (`@psalm-trace $x`, ADR-0074 §2) — the docblock spelling
    /// of the dump surface's question. Like the assertion family it exists in
    /// **prefixed form only** (`@psalm-trace` is the canonical Psalm vocabulary,
    /// `@phpstan-trace` rides the uniform strip; bare `@trace` is not a tag), and
    /// like [`Self::ConditionalPurity`] its payload is variable names alone in
    /// the shared [`DocTag::var_name`] field, `type_text` empty — no type, no
    /// expression. A comma-list payload (`$a, $b` — Psalm's multi-variable form,
    /// ADR-0074 §7) scans as **one `DocTag` per named variable, in source
    /// order**, every span shared (the whole tag's), so consumers read the list
    /// exactly like N single-variable tags. A malformed item anywhere in the
    /// list — a non-`$` token between commas, a dangling comma — drops the
    /// whole tag: silence is the safe side (a missed trace is a missed service,
    /// never a wrong answer).
    ///
    /// Named `TraceTag`, not `Trace`: bare `trace` is the trace IR's word in this
    /// codebase (ADR-0074 §4's naming rule).
    TraceTag,
    /// An **interop envelope** (ADR-0082, issue #303): one of upstream's purity
    /// tags, optionally parameterized with a list of effect labels. It carries
    /// **no type** — `type_text` is empty and the payload is the label list in
    /// [`DocTag::labels`], which is empty for the label-free spellings.
    ///
    /// The family's recognized spellings and their bare-tag rules live on
    /// [`EnvelopeTag`]; the label grammar lives on [`scan_label_list`].
    InteropEnvelope(EnvelopeTag),
}

impl TagKind {
    /// Recognize a tag name, returning its kind and whether it carried a
    /// `@phpstan-`/`@psalm-` precedence prefix. Assert kinds are provisional here:
    /// `negated` is set to `false` and fixed up by [`scan_line`] once the type text
    /// (which carries the leading `!`) has been isolated.
    fn from_name(name: &str) -> Option<(TagKind, bool)> {
        // The conditional-purity family is checked first: its spellings admit no
        // `@psalm-` alias, so it must not go through the shared prefix strip.
        if let Some((cond, prefixed)) = PurityCondition::from_tag_name(name) {
            return Some((TagKind::ConditionalPurity(cond), prefixed));
        }
        // The interop-envelope family likewise: its accepted spellings differ per
        // member (no `@psalm-impure`, no alias for the class-level pair), so it is
        // matched whole rather than through the shared strip.
        if let Some((env, prefixed)) = EnvelopeTag::from_tag_name(name) {
            return Some((TagKind::InteropEnvelope(env), prefixed));
        }
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
            // The trace annotation exists only in prefixed form (`@psalm-trace`
            // canonical, `@phpstan-trace` via the uniform strip); a bare `@trace`
            // is not a recognized tag — the assertion-family precedent verbatim
            // (ADR-0074 §2).
            "trace" if prefixed => TagKind::TraceTag,
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

    fn is_conditional_purity(self) -> bool {
        matches!(self, TagKind::ConditionalPurity(_))
    }

    fn is_trace_annotation(self) -> bool {
        matches!(self, TagKind::TraceTag)
    }

    fn interop_envelope(self) -> Option<EnvelopeTag> {
        match self {
            TagKind::InteropEnvelope(env) => Some(env),
            _ => None,
        }
    }
}

/// Scan a raw docblock (or any text) for typed tags.
pub fn scan_docblock(text: &str) -> Vec<DocTag> {
    let bytes = text.as_bytes();
    let mut tags = Vec::new();
    let mut line_start = 0usize;

    while line_start <= bytes.len() {
        let line_end = memchr(bytes, line_start, b'\n').unwrap_or(bytes.len());
        scan_line(text, line_start, line_end, &mut tags);
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end + 1;
    }
    tags
}

/// Skip the docblock gutter on a physical line — leading whitespace, an optional
/// `/**`, a run of `*`, then whitespace — returning the byte offset of the first
/// non-gutter character within `[line_start, line_end)`.
fn skip_gutter(bytes: &[u8], line_start: usize, line_end: usize) -> usize {
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
    i
}

fn scan_line(text: &str, line_start: usize, line_end: usize, tags: &mut Vec<DocTag>) {
    let bytes = text.as_bytes();
    let i = skip_gutter(bytes, line_start, line_end);

    if i >= line_end || bytes[i] != b'@' {
        return;
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
    let Some((mut kind, prefixed)) = TagKind::from_name(name) else { return };

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
    // Interop envelopes (ADR-0082) are handled before the empty-remainder bail-out
    // below: that guard exists for the tags whose payload *is* a type, and a bare
    // `@phpstan-pure` is a whole tag with nothing after its name.
    if let Some(env) = kind.interop_envelope() {
        let labels =
            if env.takes_labels() { scan_label_list(text, bytes, rest_start, rest_end) } else {
                Vec::new()
            };
        // A bare `@phpstan-impure` is ⊤ — every effect possible, which is exactly
        // what the absence of the tag already means — so it stays a **non-tag**
        // (ADR-0082 §3). A remainder that is not a conforming label list reads as
        // prose (`@phpstan-impure writes to the cache`), i.e. bare, and drops with
        // it. The class-level `all-methods-impure` is the exception: upstream gives
        // the bare form standing meaning, so it records with an empty list.
        if env == EnvelopeTag::Impure && labels.is_empty() {
            return;
        }
        // A tag with nothing after its name ends at that name; `rest_end` would
        // otherwise carry the whitespace the remainder scan skipped over.
        let tag_end = if rest_end > rest_start { rest_end } else { j };
        tags.push(DocTag {
            kind,
            // Zero-width type region: the family declares no type.
            type_text: String::new(),
            type_span: Span::new(rest_start as u32, rest_start as u32),
            tag_span: Span::new(at_offset as u32, tag_end as u32),
            line_span: Span::new(line_start as u32, line_end as u32),
            var_name: None,
            prefixed,
            property_target: false,
            labels,
        });
        return;
    }

    if rest_start >= rest_end {
        return;
    }

    // The trace annotation: variable names only, single or comma-separated
    // (ADR-0074 §7). See [`TagKind::TraceTag`] for the payload contract.
    if kind.is_trace_annotation() {
        let mut names = Vec::new();
        let mut k = rest_start;
        loop {
            if k >= rest_end || bytes[k] != b'$' {
                return;
            }
            let var_name = read_variable(text, bytes, k, rest_end);
            if var_name.len() <= 1 {
                return;
            }
            k += var_name.len();
            names.push(var_name);
            while k < rest_end && (bytes[k] == b' ' || bytes[k] == b'\t') {
                k += 1;
            }
            if k < rest_end && bytes[k] == b',' {
                k += 1;
                while k < rest_end && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                continue;
            }
            // No comma: whatever remains (if anything) is a trailing
            // description, tolerated like the single form's.
            break;
        }
        for var_name in names {
            tags.push(DocTag {
                kind,
                // Zero-width type region: the family declares no type.
                type_text: String::new(),
                type_span: Span::new(rest_start as u32, rest_start as u32),
                tag_span: Span::new(at_offset as u32, rest_end as u32),
                line_span: Span::new(line_start as u32, line_end as u32),
                var_name: Some(var_name),
                prefixed,
                property_target: false,
                labels: Vec::new(),
            });
        }
        return;
    }

    // For @param/@var/@…-assert, split the type off at the first `$variable`.
    let mut property_target = false;
    let (type_start, type_end, var_name) = if kind.is_conditional_purity() {
        // Upstream grammar requires a variable first, then an optional description.
        if bytes[rest_start] != b'$' {
            return;
        }
        let var_name = read_variable(text, bytes, rest_start, rest_end);
        if var_name.len() <= 1 {
            return;
        }
        // Zero-width type region: the family declares no type.
        (rest_start, rest_start, Some(var_name))
    } else if kind.carries_var_name() {
        match find_variable(bytes, rest_start, rest_end) {
            Some(var_pos) => {
                let var_name = read_variable(text, bytes, var_pos, rest_end);
                // A `$this->prop` / `$obj->prop` / `$this::$static` target is a
                // *property*, not a plain variable: recognized (not malformed) but
                // flagged so variable-acting consumers skip it. Detect the accessor
                // right after the variable name, and treat a bare `$this` target
                // likewise. `@param` grammar admits no accessor, so the flag is
                // scoped to the kinds where the property spelling occurs.
                let var_end = var_pos + var_name.len();
                let followed_by_accessor = bytes[var_end..rest_end.min(bytes.len())]
                    .starts_with(b"->")
                    || bytes[var_end..rest_end.min(bytes.len())].starts_with(b"::");
                if (kind.is_assert() || matches!(kind, TagKind::Var))
                    && (followed_by_accessor || var_name == "$this")
                {
                    property_target = true;
                }
                // Type is everything before the variable (trimmed).
                let mut te = var_pos;
                while te > rest_start && (bytes[te - 1] == b' ' || bytes[te - 1] == b'\t') {
                    te -= 1;
                }
                if te <= rest_start {
                    // `@param $x` with no type — nothing to offer.
                    return;
                }
                (rest_start, te, Some(var_name))
            }
            // No `$var`. For `@param`/`@var` this is a bare `@var T`: the whole
            // remainder is the type. An assertion tag with no target is malformed —
            // ignore just this tag.
            None if kind.is_assert() => return,
            None => (rest_start, rest_end, None),
        }
    } else {
        (rest_start, rest_end, None)
    };

    tags.push(DocTag {
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
        property_target,
        labels: Vec::new(),
    });
}

/// The effect labels of an interop envelope's remainder `[start, end)`, following
/// `@phpstan-ignore`'s list-and-comment shape (ADR-0082 §4):
///
/// ```ebnf
/// label-list = label { "," label } [ "(" text-without-close-paren ")" ] ;
/// label      = segment { "." segment } ;
/// segment    = lowercase-letter { lowercase-letter | digit } ;
/// ```
///
/// **Strict list or bare.** The remainder is a label list only when the *whole* of
/// it conforms; anything else — prose (`writes to the cache`), an uppercase or
/// underscored token, a dangling comma, a `(` before any label — yields the empty
/// list, i.e. is read as a tag written bare with a description. There is no
/// partial list: half a bound is a worse claim than none, and the caller's bare
/// rule (drop, or ⊤) is the safe reading either way.
fn scan_label_list(text: &str, bytes: &[u8], start: usize, end: usize) -> Vec<String> {
    let mut labels = Vec::new();
    let mut i = start;
    loop {
        let label_start = i;
        loop {
            // segment = [a-z][a-z0-9]*
            if i >= end || !bytes[i].is_ascii_lowercase() {
                return Vec::new();
            }
            i += 1;
            while i < end && (bytes[i].is_ascii_lowercase() || bytes[i].is_ascii_digit()) {
                i += 1;
            }
            if i < end && bytes[i] == b'.' {
                i += 1;
                continue;
            }
            break;
        }
        labels.push(text[label_start..i].to_owned());
        while i < end && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i < end && bytes[i] == b',' {
            i += 1;
            while i < end && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            continue;
        }
        break;
    }
    // Whatever is left must be the one optional parenthesized comment, whole. A `(`
    // is legal only here — directly after the tag name it sends phpdoc-parser down
    // its Doctrine-annotation path, so that spelling is not a list at all.
    if i < end {
        if bytes[i] != b'(' || bytes[end - 1] != b')' {
            return Vec::new();
        }
        if text[i + 1..end - 1].contains(')') {
            return Vec::new();
        }
    }
    labels
}

/// Whether `name` (the token right after the `@`) is a `@template` declaration
/// tag: `template`, `template-covariant`, or `template-contravariant`, each
/// optionally carrying a `@phpstan-`/`@psalm-` precedence prefix (the ADR-0029
/// prefix rule — the same prefix set [`TagKind::from_name`] recognizes). Returns
/// the declared *variance*: all three spellings declare a template name, and the
/// variance rides along for the callers that need it.
fn template_tag_variance(name: &str) -> Option<Variance> {
    let bare = name
        .strip_prefix("phpstan-")
        .or_else(|| name.strip_prefix("psalm-"))
        .unwrap_or(name);
    match bare {
        "template" => Some(Variance::Invariant),
        "template-covariant" => Some(Variance::Covariant),
        "template-contravariant" => Some(Variance::Contravariant),
        _ => None,
    }
}

/// The variance a `@template` declaration was written with — invariant for a plain
/// `@template`, covariant/contravariant for the two marked spellings (with or
/// without a `@phpstan-`/`@psalm-` prefix).
///
/// Scanned but **not consumed** by contract checking today: a bound (issue #293) is
/// judged the same way whatever the variance. It is recovered here so the
/// `@extends`/`@implements` slice (issue #294) can read it off the declaration
/// instead of treating every template as invariant — an invariant reading of a
/// contravariant parameter is a false positive, not a miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variance {
    /// `@template T` — no variance marker.
    #[default]
    Invariant,
    /// `@template-covariant T`.
    Covariant,
    /// `@template-contravariant T`.
    Contravariant,
}

/// One `@template` declaration, as written: the declared name, the *bound* text
/// after `of`/`as` (PHPStan spells it `of`, Psalm accepts `as`), and the variance
/// marker. Names and bound text are returned verbatim (case preserved); the caller
/// decides case-folding and whether the bound text parses as a type.
///
/// A trailing `= Default` (PHPStan template defaults) is cut from the bound: the
/// default is a different obligation from the upper bound and nothing reads it yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateDecl {
    /// The declared template name, e.g. the `T` in `@template T of array`.
    pub name: String,
    /// The bound text after `of`/`as`, trimmed, or `None` for an unbounded
    /// `@template T`. Never empty when `Some`.
    pub bound: Option<String>,
    /// The declared variance (see [`Variance`]).
    pub variance: Variance,
}

/// Scan a raw docblock for `@template` declarations, returning each declared
/// template *name* — the first identifier token after the tag (the `T` in
/// `@template T`, `@template T of Foo`, `@phpstan-template-covariant T`). Names are
/// returned as written (case preserved); the caller decides case-folding.
///
/// This is the seam that feeds the *template shadow set* (issue #5): a name
/// declared here shadows a same-named class in that declaration's docblock types,
/// so a `@template Model` whose name collides with a real class `Model` is a
/// template parameter (opaque), not the class.
///
/// The name-only projection of [`scan_template_decls`], kept for the shadow-set
/// callers that have nothing to say about bounds or variance.
#[must_use]
pub fn scan_template_names(text: &str) -> Vec<String> {
    scan_template_decls(text).into_iter().map(|d| d.name).collect()
}

/// Scan a raw docblock for `@template` declarations, returning each one whole —
/// name, bound (`of Foo` / `as Foo`), and variance marker — in source order.
///
/// One pass recovers all three on purpose. The bound is what an upper-bound
/// contract needs (issue #293); the variance is what an inheritance-edge reading
/// needs (issue #294), and dropping it there turns a contravariant parameter into a
/// false positive rather than a miss. Nothing here interprets either: the bound is
/// raw text the caller may or may not choose to parse.
#[must_use]
pub fn scan_template_decls(text: &str) -> Vec<TemplateDecl> {
    let bytes = text.as_bytes();
    let mut decls = Vec::new();
    let mut line_start = 0usize;
    while line_start <= bytes.len() {
        let line_end = memchr(bytes, line_start, b'\n').unwrap_or(bytes.len());
        if let Some(decl) = scan_template_line(text, line_start, line_end) {
            decls.push(decl);
        }
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end + 1;
    }
    decls
}

fn scan_template_line(text: &str, line_start: usize, line_end: usize) -> Option<TemplateDecl> {
    let bytes = text.as_bytes();
    let i = skip_gutter(bytes, line_start, line_end);
    if i >= line_end || bytes[i] != b'@' {
        return None;
    }
    // Read the tag name (letters and `-`, matching `scan_line`).
    let name_start = i + 1;
    let mut j = name_start;
    while j < line_end && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-') {
        j += 1;
    }
    let variance = template_tag_variance(&text[name_start..j])?;
    // Skip whitespace, then read the template name: a PHP identifier
    // (`[A-Za-z_\x80-…][A-Za-z0-9_\x80-…]*`).
    while j < line_end && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    let ident_start = j;
    if j >= line_end || !(bytes[j].is_ascii_alphabetic() || bytes[j] == b'_' || bytes[j] >= 0x80) {
        return None; // `@template` with no name — nothing to shadow.
    }
    j += 1;
    while j < line_end && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] >= 0x80) {
        j += 1;
    }
    let name = text[ident_start..j].to_owned();
    Some(TemplateDecl { name, bound: scan_template_bound(text, j, line_end), variance })
}

/// The bound text of a `@template` line: everything after an `of`/`as` keyword,
/// with the docblock's trailing `*/` and any `= Default` suffix cut off. `None`
/// when the line carries no bound keyword (or nothing after it).
fn scan_template_bound(text: &str, after_name: usize, line_end: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut j = after_name;
    while j < line_end && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    let kw_start = j;
    while j < line_end && bytes[j].is_ascii_alphabetic() {
        j += 1;
    }
    if !matches!(&text[kw_start..j], "of" | "as") {
        return None;
    }
    let mut rest = &text[j..line_end];
    // A one-line docblock ends on the same line as its tag.
    if let Some(cut) = rest.find("*/") {
        rest = &rest[..cut];
    }
    // A template *default* (`@template T of array = array{}`) is a separate
    // obligation from the upper bound and nothing reads it; cut it here so the
    // bound text stays parseable. No bound spelling contains a top-level `=`.
    if let Some(cut) = rest.find('=') {
        rest = &rest[..cut];
    }
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// Whether `name` (the token right after the `@`) is an **inheritance-edge** tag:
/// `extends` / `implements`, their `template-` spellings (`@template-extends`,
/// `@template-implements`), each optionally carrying a `@phpstan-`/`@psalm-`
/// precedence prefix (the ADR-0029 prefix rule).
///
/// Deliberately *not* folded into [`template_tag_variance`]: `@template-extends`
/// declares no template name, and the scanner test that pins it as a non-template
/// is what keeps the two families apart.
fn is_inheritance_tag(name: &str) -> bool {
    let bare = name
        .strip_prefix("phpstan-")
        .or_else(|| name.strip_prefix("psalm-"))
        .unwrap_or(name);
    matches!(
        bare,
        "extends" | "implements" | "template-extends" | "template-implements"
    )
}

/// Scan a raw docblock for **inheritance-edge type arguments** — the `Box<int>` in
/// `@extends Box<int>`, the `Producer<Dog>` in `@implements Producer<Dog>` — in
/// source order (ADR-0032 amendment, issue #294).
///
/// The type argument written on an inheritance edge is a *phpdoc* fact, not a
/// syntax one: nothing in PHP source carries `<int>`, so the class syntax node
/// keeps its bare `extends`/`implements` name references and the parameterization
/// is recovered here, from the class docblock, in the same pass shape as
/// [`scan_template_decls`].
///
/// Each entry is the tag's raw tail (the docblock's closing `*/` cut, trimmed) —
/// unparsed on purpose, exactly as [`TemplateDecl::bound`] is. The caller decides
/// whether it parses as a type and in whose namespace its class name resolves; a
/// tail that does not parse contributes nothing and its siblings are unaffected.
#[must_use]
pub fn scan_inheritance_args(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut line_start = 0usize;
    while line_start <= bytes.len() {
        let line_end = memchr(bytes, line_start, b'\n').unwrap_or(bytes.len());
        if let Some(arg) = scan_inheritance_line(text, line_start, line_end) {
            out.push(arg);
        }
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end + 1;
    }
    out
}

fn scan_inheritance_line(text: &str, line_start: usize, line_end: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let i = skip_gutter(bytes, line_start, line_end);
    if i >= line_end || bytes[i] != b'@' {
        return None;
    }
    let name_start = i + 1;
    let mut j = name_start;
    while j < line_end && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-') {
        j += 1;
    }
    if !is_inheritance_tag(&text[name_start..j]) {
        return None;
    }
    let mut rest = &text[j..line_end];
    // A one-line docblock ends on the same line as its tag.
    if let Some(cut) = rest.find("*/") {
        rest = &rest[..cut];
    }
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

// ---------------------------------------------------------------------------
// Magic-member tags: `@method` / `@property*` / `@mixin` / the `@phpstan-type`
// pair (ADR-0049 A14, issue #195).
//
// These declare members that live *somewhere the index cannot see*. Steins does
// not read them as member sources; it reads them as **obstacles** — a class-like
// carrying one is not enumerable for an absence proof. So the scan below recovers
// exactly two things: the tag's presence and its subject. It never parses the
// tag's type expression, and it never fails on one: an unrecognizable tail leaves
// the subject empty and the record stands, because the obstacle is the tag, not
// its shape.
// ---------------------------------------------------------------------------

/// Which magic-member docblock tag a [`MagicMemberTag`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MagicTagKind {
    /// `@method [static] [Type] name(…)` — a method that exists only at runtime.
    Method,
    /// `@property [Type] $name`.
    Property,
    /// `@property-read [Type] $name`.
    PropertyRead,
    /// `@property-write [Type] $name`.
    PropertyWrite,
    /// `@mixin Target` — *the members live on another class*, the one tag whose
    /// whole meaning is the obstacle.
    Mixin,
    /// `@phpstan-type Alias = …` / `@psalm-type` — a local type alias. Read for
    /// presence only: an alias is not a member, but a class-like that spells one
    /// carries docblock vocabulary the engine does not resolve.
    TypeAlias,
    /// `@phpstan-import-type Alias from Other` / `@psalm-import-type`.
    ImportedTypeAlias,
}

impl MagicTagKind {
    /// The tag's canonical spelling — what a reader looks for in their source,
    /// and what a posture report prints (the [`crate::docblock`] analogue of
    /// steins-syntax's give-up-list labels).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Method => "@method",
            Self::Property => "@property",
            Self::PropertyRead => "@property-read",
            Self::PropertyWrite => "@property-write",
            Self::Mixin => "@mixin",
            Self::TypeAlias => "@phpstan-type",
            Self::ImportedTypeAlias => "@phpstan-import-type",
        }
    }

    /// Whether the tag names a class-like whose own members are pulled in
    /// (`@mixin`) — the only kind whose subject is followed transitively.
    #[must_use]
    pub const fn is_mixin(self) -> bool {
        matches!(self, Self::Mixin)
    }

    /// Recognize a magic-member tag name (the token right after the `@`),
    /// applying the ADR-0029 `@phpstan-`/`@psalm-` prefix rule. The type-alias
    /// pair exists in prefixed form only — a bare `@type` is not a tag.
    fn from_name(name: &str) -> Option<Self> {
        let (bare, prefixed) = match name
            .strip_prefix("phpstan-")
            .or_else(|| name.strip_prefix("psalm-"))
        {
            Some(rest) => (rest, true),
            None => (name, false),
        };
        Some(match bare {
            "method" => Self::Method,
            "property" => Self::Property,
            "property-read" => Self::PropertyRead,
            "property-write" => Self::PropertyWrite,
            "mixin" => Self::Mixin,
            "type" if prefixed => Self::TypeAlias,
            "import-type" if prefixed => Self::ImportedTypeAlias,
            _ => return None,
        })
    }
}

/// One magic-member tag found on a class-like docblock: its kind and its subject,
/// with no type parsing at all.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MagicMemberTag {
    pub kind: MagicTagKind,
    /// The tag's subject **as written**: the method name for `@method`, the
    /// property name *without* its `$` for `@property*`, the target class
    /// reference (leading `\` preserved, so the caller can classify it) for
    /// `@mixin`, and the alias name for the `@phpstan-type` pair. Empty when the
    /// tag's tail gave none — the tag is still recorded, because the obstacle is
    /// its presence.
    pub subject: String,
    /// Span of the tag within the scanned docblock text, from its `@` to the end
    /// of its trimmed content (the [`DocTag::tag_span`] convention).
    pub tag_span: Span,
}

/// Scan a class-like docblock for the magic-member tags (ADR-0049 A14).
///
/// Deliberately separate from [`scan_docblock`]: those tags carry a type Steins
/// parses into an envelope, these carry one it refuses to. Mixing them would put
/// an unparsed `type_text` into a struct whose whole contract is that the text is
/// a type expression.
#[must_use]
pub fn scan_magic_member_tags(text: &str) -> Vec<MagicMemberTag> {
    let bytes = text.as_bytes();
    let mut tags = Vec::new();
    let mut line_start = 0usize;
    while line_start <= bytes.len() {
        let line_end = memchr(bytes, line_start, b'\n').unwrap_or(bytes.len());
        if let Some(tag) = scan_magic_line(text, line_start, line_end) {
            tags.push(tag);
        }
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end + 1;
    }
    tags
}

fn scan_magic_line(text: &str, line_start: usize, line_end: usize) -> Option<MagicMemberTag> {
    let bytes = text.as_bytes();
    let i = skip_gutter(bytes, line_start, line_end);
    if i >= line_end || bytes[i] != b'@' {
        return None;
    }
    let at_offset = i;
    let name_start = i + 1;
    let mut j = name_start;
    while j < line_end && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-') {
        j += 1;
    }
    let kind = MagicTagKind::from_name(&text[name_start..j])?;

    let mut rest_start = j;
    while rest_start < line_end && (bytes[rest_start] == b' ' || bytes[rest_start] == b'\t') {
        rest_start += 1;
    }
    let mut rest_end = line_end;
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

    let subject = match kind {
        MagicTagKind::Method => magic_method_name(text, bytes, rest_start, rest_end),
        MagicTagKind::Property | MagicTagKind::PropertyRead | MagicTagKind::PropertyWrite => {
            find_variable(bytes, rest_start, rest_end)
                .map(|p| read_variable(text, bytes, p, rest_end)[1..].to_owned())
                .unwrap_or_default()
        }
        MagicTagKind::Mixin => read_class_ref(text, bytes, rest_start, rest_end),
        MagicTagKind::TypeAlias | MagicTagKind::ImportedTypeAlias => {
            read_identifier(text, bytes, rest_start, rest_end)
        }
    };
    Some(MagicMemberTag {
        kind,
        subject,
        tag_span: Span::new(at_offset as u32, rest_end as u32),
    })
}

/// The method name in an `@method` tail: the identifier that opens the parameter
/// list, i.e. the one immediately before the first `(` that is not nested inside
/// a generic/shape bracket run and not the `(` of a parenthesized *type*
/// (`callable(int): string`, `Closure(): void`). The type itself is never parsed,
/// so an unrecognized tail simply yields an empty name — the tag still records.
fn magic_method_name(text: &str, bytes: &[u8], start: usize, end: usize) -> String {
    let mut depth = 0u32;
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'<' | b'{' | b'[' => depth += 1,
            b'>' | b'}' | b']' => depth = depth.saturating_sub(1),
            b'(' if depth == 0 => {
                let mut s = i;
                while s > start && is_ident_byte(bytes[s - 1]) {
                    s -= 1;
                }
                if s < i {
                    let ident = &text[s..i];
                    if !is_parenthesized_type_name(ident) {
                        return ident.to_owned();
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    String::new()
}

/// The type names PHPDoc lets carry a parenthesized signature, which therefore
/// must not read as an `@method` name. Compared case-insensitively, with any
/// leading `\` ignored (`\Closure(): void`).
fn is_parenthesized_type_name(ident: &str) -> bool {
    let bare = ident.trim_start_matches('\\');
    ["callable", "closure", "pure-callable", "pure-closure"]
        .iter()
        .any(|k| bare.eq_ignore_ascii_case(k))
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Read a class reference token (identifier bytes plus `\` separators) at `start`.
/// A trailing generic argument list (`Foo<int>`) or punctuation ends it; the
/// leading `\` is preserved so the caller can tell a fully-qualified name from an
/// import-relative one.
fn read_class_ref(text: &str, bytes: &[u8], start: usize, end: usize) -> String {
    let mut j = start;
    while j < end && (is_ident_byte(bytes[j]) || bytes[j] == b'\\') {
        j += 1;
    }
    text[start..j].to_owned()
}

/// Read a bare identifier at `start` (no namespace separators) — the alias name
/// of the `@phpstan-type` pair.
fn read_identifier(text: &str, bytes: &[u8], start: usize, end: usize) -> String {
    let mut j = start;
    while j < end && is_ident_byte(bytes[j]) {
        j += 1;
    }
    text[start..j].to_owned()
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
        assert!(!tags[0].property_target);
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

    // ---- Conditional purity (ADR-0063 P4; phpdoc-parser 2.3.3) -------------

    #[test]
    fn scans_pure_unless_callable_is_impure() {
        let doc = "/** @pure-unless-callable-is-impure $callback */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::ConditionalPurity(PurityCondition::CallableIsImpure));
        assert_eq!(tags[0].var_name.as_deref(), Some("$callback"));
        // The family declares a condition, not a type.
        assert_eq!(tags[0].type_text, "");
        assert!(!tags[0].prefixed);
    }

    #[test]
    fn scans_pure_unless_parameter_passed_and_the_phpstan_prefix() {
        let tags = scan_docblock("/** @phpstan-pure-unless-parameter-passed $matches */");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::ConditionalPurity(PurityCondition::ParameterIsPassed));
        assert_eq!(tags[0].var_name.as_deref(), Some("$matches"));
        assert!(tags[0].prefixed, "the `@phpstan-` spelling is the prefixed form");
    }

    #[test]
    fn conditional_purity_tolerates_a_trailing_description() {
        // Upstream grammar is `parseRequiredVariableName` + optional description.
        let doc = "/**\n * @pure-unless-callable-is-impure $fn as long as it is pure\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].var_name.as_deref(), Some("$fn"));
    }

    #[test]
    fn psalm_prefixed_conditional_purity_is_not_a_tag() {
        // Upstream registers only the bare and `@phpstan-` spellings for this
        // family — there is no `@psalm-` alias to honor.
        assert!(scan_docblock("/** @psalm-pure-unless-callable-is-impure $cb */").is_empty());
    }

    // ---- Trace annotation (ADR-0074 §2, issue #94) -------------------------

    #[test]
    fn scans_psalm_trace_with_a_variable_payload() {
        // The canonical spelling: Psalm's own vocabulary (ADR-0029 compat).
        let doc = "/** @psalm-trace $x */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::TraceTag);
        assert_eq!(tags[0].var_name.as_deref(), Some("$x"));
        // Variable name only — the tag declares no type.
        assert_eq!(tags[0].type_text, "");
        assert!(tags[0].prefixed);
    }

    #[test]
    fn phpstan_trace_rides_the_uniform_prefix_strip() {
        let tags = scan_docblock("/** @phpstan-trace $value */");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::TraceTag);
        assert_eq!(tags[0].var_name.as_deref(), Some("$value"));
    }

    #[test]
    fn bare_trace_is_not_a_tag() {
        // Neither upstream tool recognizes an unprefixed `@trace`; recognizing it
        // would be invented vocabulary (the assertion-family precedent, ADR-0074 §2).
        assert!(scan_docblock("/** @trace $x */").is_empty());
    }

    #[test]
    fn trace_annotation_comma_list_scans_one_tag_per_variable() {
        // `@psalm-trace $a, $b` is Psalm's multi-variable form (ADR-0074 §7):
        // one `DocTag` per named variable, in source order, spaced and tight
        // commas alike. Every span is the whole tag's, so the consumer reports
        // each variable at the tag's own position.
        for doc in ["/** @psalm-trace $a, $b */", "/** @psalm-trace $a,$b */"] {
            let tags = scan_docblock(doc);
            assert_eq!(tags.len(), 2, "{doc}");
            assert!(tags.iter().all(|t| t.kind == TagKind::TraceTag));
            assert_eq!(tags[0].var_name.as_deref(), Some("$a"));
            assert_eq!(tags[1].var_name.as_deref(), Some("$b"));
            assert_eq!(tags[0].tag_span, tags[1].tag_span, "the list shares the tag's span");
            assert!(tags.iter().all(|t| t.type_text.is_empty()));
        }
    }

    #[test]
    fn trace_annotation_list_tolerates_a_trailing_description() {
        // A description after the LAST variable is tolerated exactly like the
        // single form's; it never reads as another list item.
        let tags = scan_docblock("/** @psalm-trace $a, $b watch these */");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].var_name.as_deref(), Some("$a"));
        assert_eq!(tags[1].var_name.as_deref(), Some("$b"));
    }

    #[test]
    fn trace_annotation_list_with_a_malformed_item_drops_the_whole_tag() {
        // A non-`$` token between commas, or a dangling comma, is a malformed
        // list: the WHOLE tag drops (no half-answered list) — silence is the
        // safe side, mirroring the single form's malformed posture.
        assert!(scan_docblock("/** @psalm-trace $a, b */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a, int $b */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a, $ */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a, */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a,, $b */").is_empty());
    }

    #[test]
    fn trace_annotation_without_a_variable_is_malformed() {
        // The payload must be a variable name first (no type, no expression).
        assert!(scan_docblock("/** @psalm-trace */").is_empty());
        assert!(scan_docblock("/** @psalm-trace int $x */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $ */").is_empty());
    }

    #[test]
    fn conditional_purity_needs_the_variable_first() {
        // `parseRequiredVariableName` reads the *next* token; a description that
        // precedes the variable is not the grammar. Malformed → this tag alone is
        // dropped, siblings survive.
        let doc = "/**\n * @pure-unless-callable-is-impure the $cb param\n * @param string $s\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Param);
    }

    #[test]
    fn bare_impure_is_still_not_a_tag() {
        // A bare `@phpstan-impure` is ⊤ — every effect possible — which is what the
        // absence of the tag already says, so reading it would import the
        // metadata-only lie ADR-0063 refused. ADR-0082 §3 keeps this side of the
        // negative test verbatim; only the *pure* side evolved (see
        // `bare_pure_is_the_mutate_local_envelope`).
        assert!(scan_docblock("/** @impure */").is_empty());
        assert!(scan_docblock("/** @phpstan-impure */").is_empty());
    }

    // ---- Interop envelopes (ADR-0082, issue #303) --------------------------

    /// The `(kind, labels)` of every tag a docblock scans to — the shape every
    /// envelope test below asserts on.
    fn envelopes(doc: &str) -> Vec<(TagKind, Vec<String>)> {
        scan_docblock(doc).into_iter().map(|t| (t.kind, t.labels)).collect()
    }

    fn impure(labels: &[&str]) -> Vec<(TagKind, Vec<String>)> {
        vec![(
            TagKind::InteropEnvelope(EnvelopeTag::Impure),
            labels.iter().map(|s| (*s).to_owned()).collect(),
        )]
    }

    #[test]
    fn bare_pure_is_the_mutate_local_envelope() {
        // Unlike ⊤, the empty envelope carries information — it is the
        // `{mutate.local}` bound (ADR-0082 §3) — so all three upstream spellings
        // are read. What they *mean* is the declared lane's business, not the
        // scanner's; here they simply become tags with no labels.
        for doc in ["/** @pure */", "/** @phpstan-pure */", "/** @psalm-pure */"] {
            let tags = scan_docblock(doc);
            assert_eq!(tags.len(), 1, "{doc}");
            assert_eq!(tags[0].kind, TagKind::InteropEnvelope(EnvelopeTag::Pure), "{doc}");
            assert!(tags[0].labels.is_empty(), "{doc}");
            // The family declares a bound, not a type.
            assert_eq!(tags[0].type_text, "", "{doc}");
            assert!(tags[0].var_name.is_none(), "{doc}");
        }
        // The bare spelling is the plain form; the two prefixes are the prefixed one.
        assert!(!scan_docblock("/** @pure */")[0].prefixed);
        assert!(scan_docblock("/** @phpstan-pure */")[0].prefixed);
        assert!(scan_docblock("/** @psalm-pure */")[0].prefixed);
    }

    #[test]
    fn pure_ignores_a_trailing_description() {
        // "Pure, except it performs effects" is a contradiction, so the pure side
        // takes no labels: trailing text is a description, exactly as upstream
        // reads it, and never keeps the tag from being recognized.
        assert_eq!(
            envelopes("/** @phpstan-pure no side effects at all */"),
            [(TagKind::InteropEnvelope(EnvelopeTag::Pure), Vec::new())]
        );
        // Even text that *would* parse as a label list is only a description here.
        assert_eq!(
            envelopes("/** @pure io.db */"),
            [(TagKind::InteropEnvelope(EnvelopeTag::Pure), Vec::new())]
        );
    }

    #[test]
    fn impure_scans_its_label_list() {
        assert_eq!(envelopes("/** @phpstan-impure io */"), impure(&["io"]));
        // Spaced and tight commas alike, and a dot-path is one label.
        assert_eq!(
            envelopes("/** @phpstan-impure io.db, nondet.time */"),
            impure(&["io.db", "nondet.time"])
        );
        assert_eq!(
            envelopes("/** @phpstan-impure io.db,nondet.time,exit */"),
            impure(&["io.db", "nondet.time", "exit"])
        );
        // A deep path and a digit-bearing segment are labels like any other: the
        // registry, not the scanner, decides which names exist.
        assert_eq!(
            envelopes("/** @phpstan-impure io.fs.write, io.net.http */"),
            impure(&["io.fs.write", "io.net.http"])
        );
        assert_eq!(envelopes("/** @phpstan-impure io2 */"), impure(&["io2"]));
    }

    #[test]
    fn impure_accepts_a_trailing_paren_comment_after_labels() {
        // `@phpstan-ignore`'s shape verbatim: the list, then one parenthesized
        // comment. It is legal only *after* a label (see the negative test).
        assert_eq!(
            envelopes("/** @phpstan-impure io.db (reads the clock for cache TTL) */"),
            impure(&["io.db"])
        );
        assert_eq!(
            envelopes("/**\n * @phpstan-impure io.db, nondet.time (refreshes the entry)\n */"),
            impure(&["io.db", "nondet.time"])
        );
    }

    #[test]
    fn the_unprefixed_impure_spelling_is_accepted() {
        // PHPStan implements `@impure` and `@phpstan-impure`; the bare one is ⊤
        // like its prefixed sibling, and a labeled one is a bound like it too.
        assert!(scan_docblock("/** @impure */").is_empty());
        assert_eq!(envelopes("/** @impure io */"), impure(&["io"]));
        assert!(!scan_docblock("/** @impure io */")[0].prefixed);
        assert!(scan_docblock("/** @phpstan-impure io */")[0].prefixed);
    }

    #[test]
    fn psalm_prefixed_impure_is_not_a_tag() {
        // The accepted spellings mirror PHPStan's implemented set exactly; there is
        // no `@psalm-impure` to honor, labeled or not (ADR-0082 §5).
        assert!(scan_docblock("/** @psalm-impure */").is_empty());
        assert!(scan_docblock("/** @psalm-impure io */").is_empty());
    }

    #[test]
    fn a_nonconforming_impure_remainder_reads_as_bare() {
        // Strict list or bare: a remainder that is not a whole label list is prose,
        // and a prose-only `@phpstan-impure` is the ⊤ non-tag. Never a partial list.
        for doc in [
            "/** @phpstan-impure writes to the cache */",
            "/** @phpstan-impure IO */",
            "/** @phpstan-impure io_db */",
            "/** @phpstan-impure io.DB */",
            "/** @phpstan-impure 1io */",
            "/** @phpstan-impure io. */",
            "/** @phpstan-impure io, */",
            "/** @phpstan-impure io,, exit */",
            "/** @phpstan-impure io exit */",
            "/** @phpstan-impure \\Foo */",
        ] {
            assert!(scan_docblock(doc).is_empty(), "{doc}");
        }
    }

    #[test]
    fn a_paren_comment_without_labels_is_not_a_list() {
        // A `(` directly after the tag name is phpdoc-parser's Doctrine-annotation
        // path (`phpDoc.parseError`), so the grammar forbids it: zero labels means
        // bare, and bare impure is a non-tag.
        assert!(scan_docblock("/** @phpstan-impure (writes to the cache) */").is_empty());
        assert!(scan_docblock("/** @impure (why) */").is_empty());
        // An unclosed or re-opened comment is not the one trailing comment either.
        assert!(scan_docblock("/** @phpstan-impure io (unclosed */").is_empty());
        assert!(scan_docblock("/** @phpstan-impure io (a) (b) */").is_empty());
    }

    #[test]
    fn class_level_pure_takes_no_labels() {
        assert_eq!(
            envelopes("/** @phpstan-all-methods-pure */"),
            [(TagKind::InteropEnvelope(EnvelopeTag::AllMethodsPure), Vec::new())]
        );
        // Trailing text is a description, like the method-level pure tag's.
        assert_eq!(
            envelopes("/** @phpstan-all-methods-pure a value object */"),
            [(TagKind::InteropEnvelope(EnvelopeTag::AllMethodsPure), Vec::new())]
        );
    }

    #[test]
    fn class_level_impure_is_a_tag_bare_and_labeled() {
        // Unlike `@phpstan-impure`, the bare class-level tag has standing meaning
        // upstream (it distributes over the methods), so it records — with the
        // empty label list standing for the ⊤ bound.
        assert_eq!(
            envelopes("/** @phpstan-all-methods-impure */"),
            [(TagKind::InteropEnvelope(EnvelopeTag::AllMethodsImpure), Vec::new())]
        );
        assert_eq!(
            envelopes("/** @phpstan-all-methods-impure io.net */"),
            [(
                TagKind::InteropEnvelope(EnvelopeTag::AllMethodsImpure),
                vec!["io.net".to_owned()]
            )]
        );
        assert_eq!(
            envelopes("/** @phpstan-all-methods-impure io.net, nondet.time (a Redis client) */"),
            [(
                TagKind::InteropEnvelope(EnvelopeTag::AllMethodsImpure),
                vec!["io.net".to_owned(), "nondet.time".to_owned()]
            )]
        );
        // A nonconforming remainder falls back to bare — the tag survives, the
        // bound does not.
        assert_eq!(
            envelopes("/** @phpstan-all-methods-impure talks to Redis */"),
            [(TagKind::InteropEnvelope(EnvelopeTag::AllMethodsImpure), Vec::new())]
        );
    }

    #[test]
    fn the_class_level_pair_has_no_aliases() {
        // Exactly the two `@phpstan-` spellings, no prefix variants, no misspellings.
        for doc in [
            "/** @all-methods-pure */",
            "/** @psalm-all-methods-pure */",
            "/** @phpstan-all-method-pure */",
            "/** @phpstan-all-methods-purely */",
            "/** @all-methods-impure */",
            "/** @psalm-all-methods-impure io */",
            "/** @phpstan-allmethods-impure io */",
        ] {
            assert!(scan_docblock(doc).is_empty(), "{doc}");
        }
    }

    #[test]
    fn envelope_spans_cover_the_tag_only() {
        // A label-free tag ends at its own name, not at the gutter whitespace after
        // it; a labeled one ends at the end of its trimmed remainder.
        let doc = "/** @phpstan-pure */";
        let t = &scan_docblock(doc)[0];
        assert_eq!(&doc[t.tag_span.start as usize..t.tag_span.end as usize], "@phpstan-pure");
        let doc = "/** @phpstan-impure io.db (why) */";
        let t = &scan_docblock(doc)[0];
        assert_eq!(
            &doc[t.tag_span.start as usize..t.tag_span.end as usize],
            "@phpstan-impure io.db (why)"
        );
        assert_eq!(&doc[t.line_span.start as usize..t.line_span.end as usize], doc);
    }

    #[test]
    fn envelopes_scan_alongside_their_siblings() {
        // The family is a member of the read set like any other: it neither drops
        // nor is dropped by the type-carrying tags on the same docblock.
        let doc = "/**\n * @phpstan-impure io.db\n * @param string $key\n * @return int\n */";
        assert_eq!(
            envelopes(doc),
            [
                (TagKind::InteropEnvelope(EnvelopeTag::Impure), vec!["io.db".to_owned()]),
                (TagKind::Param, Vec::new()),
                (TagKind::Return, Vec::new()),
            ]
        );
    }

    #[test]
    fn the_conditional_purity_family_still_wins_its_spellings() {
        // `@pure-unless-callable-is-impure` is not a `@pure` with a description:
        // the conditional family is matched before the envelope family, and each
        // keeps its own kind.
        assert_eq!(
            envelopes("/** @pure-unless-callable-is-impure $cb */"),
            [(TagKind::ConditionalPurity(PurityCondition::CallableIsImpure), Vec::new())]
        );
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
            assert!(tags[0].property_target, "{doc} should be a property target");
        }
    }

    #[test]
    fn var_property_target_is_marked() {
        // A `@var` naming a property position must never read as a cast of the
        // receiver variable (ADR-0073's zero-FP guard).
        for doc in [
            "/** @var int $this->prop */",
            "/** @var int $obj->field */",
            "/** @var int $this */",
        ] {
            let tags = scan_docblock(doc);
            assert_eq!(tags.len(), 1, "{doc}");
            assert_eq!(tags[0].kind, TagKind::Var);
            assert!(tags[0].property_target, "{doc} should be a property target");
        }
        // …while a plain variable target stays unflagged.
        let tags = scan_docblock("/** @var array{a: int} $arr */");
        assert_eq!(tags.len(), 1);
        assert!(!tags[0].property_target);
        assert_eq!(tags[0].var_name.as_deref(), Some("$arr"));
    }

    // ---- @template name scanning (issue #5 shadow set) ----

    #[test]
    fn scans_plain_template_name() {
        assert_eq!(scan_template_names("/** @template T */"), vec!["T"]);
        assert_eq!(scan_template_names("/**\n * @template Model\n */"), vec!["Model"]);
    }

    #[test]
    fn scans_template_with_bound_and_default() {
        // The name projection is unchanged by a bound or a default.
        assert_eq!(scan_template_names("/** @template T of \\Countable */"), vec!["T"]);
        assert_eq!(scan_template_names("/** @template TValue = mixed */"), vec!["TValue"]);
    }

    fn decl(doc: &str) -> TemplateDecl {
        let mut ds = scan_template_decls(doc);
        assert_eq!(ds.len(), 1, "{doc}");
        ds.remove(0)
    }

    #[test]
    fn scans_template_bound_text() {
        // `of` (PHPStan) and `as` (Psalm) both introduce the upper bound, and the
        // trailing `*/` of a one-line docblock is not part of it.
        assert_eq!(decl("/** @template T of array */").bound.as_deref(), Some("array"));
        assert_eq!(decl("/** @template T as \\Countable */").bound.as_deref(), Some("\\Countable"));
        assert_eq!(
            decl("/**\n * @template T of int|list<int>\n */").bound.as_deref(),
            Some("int|list<int>")
        );
        // A bare template, a description, and a nameless keyword carry no bound.
        assert_eq!(decl("/** @template T */").bound, None);
        assert_eq!(decl("/** @template T the element type */").bound, None);
        assert_eq!(decl("/** @template T of */").bound, None);
    }

    #[test]
    fn scans_template_bound_without_its_default() {
        // A template *default* is a different obligation; the bound stops at `=`.
        assert_eq!(decl("/** @template T of array = array{} */").bound.as_deref(), Some("array"));
        assert_eq!(decl("/** @template TValue = mixed */").bound, None);
    }

    /// Variance survives the scanner even though contract checking does not consume
    /// it yet — issue #294 reads it off here, and an invariant reading of a
    /// contravariant parameter is a false positive rather than a miss.
    #[test]
    fn scans_template_variance_markers() {
        assert_eq!(decl("/** @template T */").variance, Variance::Invariant);
        assert_eq!(decl("/** @template-covariant T */").variance, Variance::Covariant);
        assert_eq!(decl("/** @template-contravariant T */").variance, Variance::Contravariant);
        assert_eq!(decl("/** @psalm-template-covariant TKey */").variance, Variance::Covariant);
        assert_eq!(
            decl("/** @phpstan-template-contravariant T of array */").variance,
            Variance::Contravariant
        );
        // Variance and bound come off the same line in the same pass.
        let d = decl("/** @template-covariant TValue of string */");
        assert_eq!(d.name, "TValue");
        assert_eq!(d.bound.as_deref(), Some("string"));
        assert_eq!(d.variance, Variance::Covariant);
    }

    #[test]
    fn scans_variance_and_prefixed_variants() {
        assert_eq!(scan_template_names("/** @template-covariant T */"), vec!["T"]);
        assert_eq!(scan_template_names("/** @template-contravariant T */"), vec!["T"]);
        assert_eq!(scan_template_names("/** @phpstan-template T */"), vec!["T"]);
        assert_eq!(scan_template_names("/** @psalm-template-covariant TKey */"), vec!["TKey"]);
    }

    #[test]
    fn scans_multiple_templates() {
        let doc = "/**\n * @template TKey\n * @template TValue of \\Stringable\n */";
        assert_eq!(scan_template_names(doc), vec!["TKey", "TValue"]);
    }

    #[test]
    fn ignores_nameless_and_non_template_tags() {
        assert!(scan_template_names("/** @template */").is_empty());
        assert!(scan_template_names("/** @param int $x */").is_empty());
        // `@template-extends`/`@extends` are class-relation tags, not declarations.
        assert!(scan_template_names("/** @template-extends Foo<int> */").is_empty());
    }

    // ---- Inheritance-edge type arguments (ADR-0032 amendment, issue #294) --

    #[test]
    fn scans_extends_and_implements_arguments() {
        assert_eq!(scan_inheritance_args("/** @extends Box<int> */"), vec!["Box<int>"]);
        assert_eq!(
            scan_inheritance_args("/** @implements Producer<Dog> */"),
            vec!["Producer<Dog>"]
        );
        // Both `@template-` spellings and both precedence prefixes reach the same
        // edge (ADR-0029), and a nested argument survives verbatim.
        assert_eq!(
            scan_inheritance_args("/** @template-extends Box<list<int>> */"),
            vec!["Box<list<int>>"]
        );
        assert_eq!(
            scan_inheritance_args("/** @phpstan-implements Producer<Dog> */"),
            vec!["Producer<Dog>"]
        );
        assert_eq!(
            scan_inheritance_args("/** @psalm-template-implements Producer<Dog> */"),
            vec!["Producer<Dog>"]
        );
    }

    #[test]
    fn scans_every_inheritance_edge_in_order() {
        let doc = "/**\n * @extends Box<int>\n * @implements Producer<Dog>\n */";
        assert_eq!(scan_inheritance_args(doc), vec!["Box<int>", "Producer<Dog>"]);
    }

    #[test]
    fn ignores_non_inheritance_and_empty_edges() {
        assert!(scan_inheritance_args("/** @template T */").is_empty());
        assert!(scan_inheritance_args("/** @param Box<int> $b */").is_empty());
        assert!(scan_inheritance_args("/** @extends */").is_empty());
        // `@extended` is a different tag; the match is on the whole tag name.
        assert!(scan_inheritance_args("/** @extendsomething Box<int> */").is_empty());
    }

    #[test]
    fn an_unparameterized_edge_still_yields_its_tail() {
        // The scanner recovers text, not meaning: a bare `@extends Box` is a
        // well-formed tag with no type arguments, and the caller's parse decides
        // that it carries none.
        assert_eq!(scan_inheritance_args("/** @extends Box */"), vec!["Box"]);
    }

    // ---- Magic-member tags (ADR-0049 A14, issue #195) ----------------------

    fn magic(doc: &str) -> Vec<(MagicTagKind, String)> {
        scan_magic_member_tags(doc).into_iter().map(|t| (t.kind, t.subject)).collect()
    }

    #[test]
    fn scans_method_tag_and_its_name() {
        assert_eq!(magic("/** @method int foo() */"), [(MagicTagKind::Method, "foo".into())]);
        assert_eq!(
            magic("/** @method static self make(int $n) builds one */"),
            [(MagicTagKind::Method, "make".into())]
        );
        // No return type at all is legal.
        assert_eq!(magic("/** @method run() */"), [(MagicTagKind::Method, "run".into())]);
    }

    #[test]
    fn method_name_survives_a_complex_return_type() {
        // The type is never parsed; generics, shapes and parenthesized callable
        // types must not be mistaken for the parameter list.
        for (doc, name) in [
            ("/** @method Collection<int, string> map(callable(int): string $c) */", "map"),
            ("/** @method array{a: int, b: list<string>} rows() */", "rows"),
            ("/** @method callable(int): string factory() */", "factory"),
            ("/** @method \\Closure(): void lazy() */", "lazy"),
            ("/** @method $this whereIn(string $c, array $v) */", "whereIn"),
        ] {
            assert_eq!(magic(doc), [(MagicTagKind::Method, name.into())], "{doc}");
        }
    }

    #[test]
    fn a_method_tag_with_an_unreadable_tail_still_records() {
        // The obstacle is the tag's presence; an empty subject never drops it.
        assert_eq!(magic("/** @method */"), [(MagicTagKind::Method, String::new())]);
        assert_eq!(magic("/** @method int */"), [(MagicTagKind::Method, String::new())]);
    }

    #[test]
    fn scans_the_property_family() {
        assert_eq!(
            magic("/**\n * @property int $count\n * @property-read ?Foo $foo the foo\n \
                   * @property-write list<string> $names\n */"),
            [
                (MagicTagKind::Property, "count".into()),
                (MagicTagKind::PropertyRead, "foo".into()),
                (MagicTagKind::PropertyWrite, "names".into()),
            ]
        );
    }

    #[test]
    fn scans_mixin_target_as_written() {
        assert_eq!(magic("/** @mixin Builder */"), [(MagicTagKind::Mixin, "Builder".into())]);
        assert_eq!(
            magic("/** @mixin \\Illuminate\\Database\\Eloquent\\Builder */"),
            [(MagicTagKind::Mixin, "\\Illuminate\\Database\\Eloquent\\Builder".into())]
        );
        // A generic argument list ends the reference.
        assert_eq!(
            magic("/** @mixin Builder<static> */"),
            [(MagicTagKind::Mixin, "Builder".into())]
        );
    }

    #[test]
    fn scans_the_type_alias_pair_in_prefixed_form_only() {
        assert_eq!(
            magic("/** @phpstan-type UserRow array{id: int} */"),
            [(MagicTagKind::TypeAlias, "UserRow".into())]
        );
        assert_eq!(
            magic("/** @psalm-import-type UserRow from UserRepo */"),
            [(MagicTagKind::ImportedTypeAlias, "UserRow".into())]
        );
        // A bare `@type` is not a tag in either upstream tool.
        assert!(scan_magic_member_tags("/** @type int $x */").is_empty());
    }

    #[test]
    fn magic_scan_ignores_the_envelope_tags_and_vice_versa() {
        let doc = "/**\n * @param int $n\n * @return string\n * @template T\n */";
        assert!(scan_magic_member_tags(doc).is_empty());
        assert!(scan_docblock("/**\n * @method int foo()\n * @mixin Bar\n */").is_empty());
    }

    #[test]
    fn every_magic_kind_labels_itself_with_its_own_spelling() {
        // The label is what a posture report prints and what a reader greps for,
        // so each kind must carry its own — never a shared family word.
        let kinds = [
            MagicTagKind::Method,
            MagicTagKind::Property,
            MagicTagKind::PropertyRead,
            MagicTagKind::PropertyWrite,
            MagicTagKind::Mixin,
            MagicTagKind::TypeAlias,
            MagicTagKind::ImportedTypeAlias,
        ];
        let labels: Vec<&str> = kinds.iter().map(|k| k.label()).collect();
        assert!(labels.iter().all(|l| l.starts_with('@')));
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "labels must be distinct: {labels:?}");
        // `@mixin` is the only kind whose subject the caller follows.
        assert!(kinds.iter().filter(|k| k.is_mixin()).count() == 1);
    }

    #[test]
    fn magic_tags_accept_the_precedence_prefixes() {
        assert_eq!(magic("/** @phpstan-method int foo() */"), [(MagicTagKind::Method, "foo".into())]);
        assert_eq!(magic("/** @psalm-property int $n */"), [(MagicTagKind::Property, "n".into())]);
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
