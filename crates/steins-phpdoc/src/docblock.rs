//! A thin docblock scanner: pulls typed tags (`@param`, `@return`, `@var`,
//! `@throws`) from a raw `/** … */` comment, with the byte span of each
//! candidate type expression. Feeds type strings to [`crate::parse_type`]
//! (ADR-0029); not a full PhpDoc parser. A type wrapped across lines (rare,
//! e.g. multi-line `array{…}` in `@param`) is not reassembled and the tag is
//! simply dropped — safe, since a missing envelope only silences. Spans are
//! relative to the passed text; add the docblock's own source offset to map
//! them back into a file.

use crate::ast::Span;

/// A typed tag recovered from a docblock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTag {
    pub kind: TagKind,
    /// Candidate type-expression text, trimmed. May still carry a trailing
    /// description for `@return`/`@throws`/`@var`; [`crate::parse_type`] reads
    /// only the type prefix.
    pub type_text: String,
    /// Span of `type_text` within the scanned text.
    pub type_span: Span,
    /// Span of the whole physical line (newline-exclusive); the transform
    /// engine (ADR-0034) deletes a tag's line with this when promoting its type.
    pub line_span: Span,
    /// Span of the tag itself, `@` to its last meaningful token — narrower than
    /// [`Self::line_span`] (excludes gutter/whitespace); used for in-line
    /// deletion when delimiters share the line.
    pub tag_span: Span,
    /// The `$foo` name when the tag carries one.
    pub var_name: Option<String>,
    /// `true` for a `@phpstan-`/`@psalm-`-prefixed spelling. PHPStan prefers a
    /// prefixed tag over the plain one for the same target (ADR-0029).
    pub prefixed: bool,
    /// `true` when an assertion/`@var` tag targets a property/`$this->…`
    /// rather than a plain variable — parsed but skipped by variable-acting
    /// consumers (ADR-0073). See [`crate::docblock::TagKind::Assert`].
    pub property_target: bool,
    /// Effect labels of an interop envelope (ADR-0082), source order, raw and
    /// unvalidated. Empty for other kinds and for a label-free envelope.
    pub labels: Vec<String>,
}

/// The three shapes of an assertion tag (PHPStan/Psalm `@…-assert` family): the
/// target narrows after the function returns (`Always`) or conditionally on its
/// boolean result — a post-condition, never a precondition (see [`TagKind::Assert`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertKind {
    /// `@phpstan-assert T $x` — holds unconditionally on normal return.
    Always,
    /// `@phpstan-assert-if-true T $x` — holds when the function returns `true`.
    IfTrue,
    /// `@phpstan-assert-if-false T $x` — holds when the function returns `false`.
    IfFalse,
}

/// Which **conditional-purity** contract a tag declares (ADR-0063 §2 decision 2;
/// merged upstream in `phpstan/phpdoc-parser` 2.3.3). Grammar for either is
/// `parseRequiredVariableName` + optional description, no type (ADR-0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurityCondition {
    /// `@pure-unless-callable-is-impure $cb` — pure except for whatever the
    /// callable bound to `$cb` does.
    CallableIsImpure,
    /// `@pure-unless-parameter-passed $out` — pure unless `$out` is actually
    /// supplied at the call site. By-ref sister of the callable form.
    ParameterIsPassed,
}

impl PurityCondition {
    /// Recognize a conditional-purity tag name → (condition, `@phpstan-`
    /// prefix?). Does not strip `psalm-` (unlike [`TagKind::from_name`]):
    /// upstream has no `@psalm-` alias for this family.
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

/// Which **interop-envelope** family a purity tag belongs to (ADR-0082): the
/// *unchecked* spelling of an effect envelope — upstream's purity tags,
/// parameterized with a label list riding in [`DocTag::labels`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeTag {
    /// `@pure` / `@psalm-pure` / `@phpstan-pure` — empty envelope, reads as
    /// `{mutate.local}` (ADR-0082 §3). No labels; trailing text is an ignored description.
    Pure,
    /// `@impure` / `@phpstan-impure` **with** a conforming label list — bound
    /// `≤ labels`. Bare or nonconforming is ⊤, not a tag at all (ADR-0082 §3).
    Impure,
    /// `@phpstan-all-methods-pure` — class-level fallback, no labels.
    AllMethodsPure,
    /// `@phpstan-all-methods-impure`, bare or labeled. Unlike the method-level
    /// tag, bare **is** a tag here: standing meaning upstream, empty labels = ⊤.
    AllMethodsImpure,
}

impl EnvelopeTag {
    /// Recognize an interop-envelope tag name → (family, prefixed?). Matched
    /// whole, before the shared prefix strip: the accepted set isn't uniform
    /// (no `@psalm-impure`). Deviation from upstream's list: `@phan-pure` /
    /// `@phan-side-effect-free` are skipped, unread anywhere in Steins.
    fn from_tag_name(name: &str) -> Option<(Self, bool)> {
        Some(match name {
            "pure" => (Self::Pure, false),
            "phpstan-pure" | "psalm-pure" => (Self::Pure, true),
            "impure" => (Self::Impure, false),
            "phpstan-impure" => (Self::Impure, true),
            // No unprefixed spelling for the class-level pair.
            "phpstan-all-methods-pure" => (Self::AllMethodsPure, true),
            "phpstan-all-methods-impure" => (Self::AllMethodsImpure, true),
            _ => return None,
        })
    }

    /// Whether the family accepts labels; pure never does (a contradiction),
    /// so its trailing text always reads as a description.
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
    /// by-ref sister). No type: payload is the parameter name in [`DocTag::var_name`].
    ConditionalPurity(PurityCondition),
    /// An assertion tag (`@phpstan-assert` / `@psalm-assert`, `-if-true`/`-if-false`).
    /// `negated` records the leading `!` (`@phpstan-assert !T $x`); type/target
    /// reuse the shared [`DocTag`] fields. Prefixed only — no bare `@assert`.
    Assert { kind: AssertKind, negated: bool },
    /// A trace annotation (`@psalm-trace $x`, ADR-0074 §2), prefixed form only
    /// (bare `@trace` is not a tag). Payload is variable names in
    /// [`DocTag::var_name`], no type; a comma list (`$a, $b`, ADR-0074 §7) scans
    /// as one `DocTag` per variable sharing the tag's span, and a malformed item
    /// drops the whole tag. Named `TraceTag`: bare `trace` is the trace IR's
    /// word here (ADR-0074 §4).
    TraceTag,
    /// An **interop envelope** (ADR-0082, issue #303): upstream's purity tags,
    /// optionally parameterized with effect labels. No type — payload is
    /// [`DocTag::labels`] (empty for label-free spellings). Spellings live on
    /// [`EnvelopeTag`]; label grammar on `scan_label_list` (private).
    InteropEnvelope(EnvelopeTag),
}

impl TagKind {
    /// Recognize a tag name → (kind, prefixed?). Assert's `negated` is
    /// provisional (`false`), fixed up by [`scan_line`] once the leading `!`
    /// has been isolated.
    fn from_name(name: &str) -> Option<(TagKind, bool)> {
        // Neither family's spellings are uniform across `@phpstan-`/`@psalm-`,
        // so each is checked, matching whole, before the shared prefix strip.
        if let Some((cond, prefixed)) = PurityCondition::from_tag_name(name) {
            return Some((TagKind::ConditionalPurity(cond), prefixed));
        }
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
            // Prefixed form only, like the assertion family (ADR-0074 §2); bare
            // `@assert`/`@trace` are not recognized tags.
            "assert" if prefixed => TagKind::Assert { kind: AssertKind::Always, negated: false },
            "assert-if-true" if prefixed => {
                TagKind::Assert { kind: AssertKind::IfTrue, negated: false }
            }
            "assert-if-false" if prefixed => {
                TagKind::Assert { kind: AssertKind::IfFalse, negated: false }
            }
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

/// Skip the docblock gutter on a physical line (whitespace, optional `/**`, a
/// run of `*`, whitespace), returning the first non-gutter byte offset.
fn skip_gutter(bytes: &[u8], line_start: usize, line_end: usize) -> usize {
    let mut i = line_start;
    while i < line_end && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i + 2 < line_end && &bytes[i..i + 3] == b"/**" { // `/**` also counts as gutter
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
    let at_offset = i; // start of the tag proper, past the gutter
    let name_start = i + 1;
    let mut j = name_start;
    while j < line_end && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-') {
        j += 1;
    }
    let name = &text[name_start..j];
    let Some((mut kind, prefixed)) = TagKind::from_name(name) else { return };

    let mut rest_start = j; // remainder of the line, minus trailing ` */` and whitespace
    while rest_start < line_end && (bytes[rest_start] == b' ' || bytes[rest_start] == b'\t') {
        rest_start += 1;
    }

    // `@phpstan-assert !T $x`: strip the negation `!` so extraction below sees a clean type.
    if kind.is_assert() && rest_start < line_end && bytes[rest_start] == b'!' {
        rest_start += 1;
        while rest_start < line_end && (bytes[rest_start] == b' ' || bytes[rest_start] == b'\t') {
            rest_start += 1;
        }
        if let TagKind::Assert { negated, .. } = &mut kind {
            *negated = true;
        }
    }
    let mut rest_end = line_end; // trim trailing `*/` and whitespace below
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
    // Before the empty-remainder bail-out: bare `@phpstan-pure` is a whole tag
    // with nothing after its name.
    if let Some(env) = kind.interop_envelope() {
        let labels =
            if env.takes_labels() { scan_label_list(text, bytes, rest_start, rest_end) } else {
                Vec::new()
            };
        // Bare `@phpstan-impure` is ⊤ (ADR-0082 §3); `all-methods-impure` is the
        // bare-meaning exception.
        if env == EnvelopeTag::Impure && labels.is_empty() {
            return;
        }
        let tag_end = if rest_end > rest_start { rest_end } else { j }; // bare ends at name
        tags.push(DocTag {
            kind,
            type_text: String::new(), // zero-width: the family declares no type
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

    // Variable names only, single or comma-separated (ADR-0074 §7).
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
            break; // no comma: remainder is a trailing description
        }
        for var_name in names {
            tags.push(DocTag {
                kind,
                type_text: String::new(), // zero-width: the family declares no type
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
        // Grammar requires a variable first, then an optional description.
        if bytes[rest_start] != b'$' {
            return;
        }
        let var_name = read_variable(text, bytes, rest_start, rest_end);
        if var_name.len() <= 1 {
            return;
        }
        (rest_start, rest_start, Some(var_name)) // zero-width: the family declares no type
    } else if kind.carries_var_name() {
        match find_variable(bytes, rest_start, rest_end) {
            Some(var_pos) => {
                let var_name = read_variable(text, bytes, var_pos, rest_end);
                // `$this->prop`/`$obj->prop`/`$this::$static` is a property target,
                // flagged for variable-acting consumers to skip (`@param` admits no accessor).
                let var_end = var_pos + var_name.len();
                let followed_by_accessor = bytes[var_end..rest_end.min(bytes.len())]
                    .starts_with(b"->")
                    || bytes[var_end..rest_end.min(bytes.len())].starts_with(b"::");
                if (kind.is_assert() || matches!(kind, TagKind::Var))
                    && (followed_by_accessor || var_name == "$this")
                {
                    property_target = true;
                }
                let mut te = var_pos; // type is everything before the variable, trimmed
                while te > rest_start && (bytes[te - 1] == b' ' || bytes[te - 1] == b'\t') {
                    te -= 1;
                }
                if te <= rest_start {
                    return; // `@param $x` with no type — nothing to offer
                }
                (rest_start, te, Some(var_name))
            }
            None if kind.is_assert() => return, // malformed: assert with no target
            None => (rest_start, rest_end, None),
        }
    } else {
        (rest_start, rest_end, None)
    };

    tags.push(DocTag {
        kind,
        type_text: text[type_start..type_end].to_owned(),
        type_span: Span::new(type_start as u32, type_end as u32),
        // `@` to the end of trimmed content (`rest_end` already excludes `*/`).
        tag_span: Span::new(at_offset as u32, rest_end as u32),
        line_span: Span::new(line_start as u32, line_end as u32), // whole physical line
        var_name,
        prefixed,
        property_target,
        labels: Vec::new(),
    });
}

/// The effect labels of an interop envelope's remainder `[start, end)`, following
/// `@phpstan-ignore`'s list-and-comment shape (ADR-0082 §4):
/// ```ebnf
/// label-list = label { "," label } [ "(" text-without-close-paren ")" ] ;
/// label      = segment { "." segment } ;
/// segment    = lowercase-letter { lowercase-letter | digit } ;
/// ```
/// Strict list or bare: the remainder is a label list only when the *whole* of
/// it conforms (no uppercase/underscore, no dangling comma, no `(` before any
/// label); anything else yields the empty list — half a bound is a worse claim
/// than none.
fn scan_label_list(text: &str, bytes: &[u8], start: usize, end: usize) -> Vec<String> {
    let mut labels = Vec::new();
    let mut i = start;
    loop {
        let label_start = i;
        loop { // segment = [a-z][a-z0-9]*, per the EBNF above
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
    if i < end { // what's left must be the one optional parenthesized comment, whole
        if bytes[i] != b'(' || bytes[end - 1] != b')' {
            return Vec::new();
        }
        if text[i + 1..end - 1].contains(')') {
            return Vec::new();
        }
    }
    labels
}

/// Whether `name` is a `@template` (or `-covariant`/`-contravariant`) declaration
/// tag, optionally `@phpstan-`/`@psalm-`-prefixed (ADR-0029); returns the variance.
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

/// The variance a `@template` declaration was written with. Unconsumed by
/// contract checking yet (issue #293); recovered for the `@extends`/`@implements`
/// slice (issue #294), where invariant-by-default false-positives on contravariance.
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

/// One `@template` declaration, as written: name, *bound* text after `of`
/// (PHPStan) / `as` (Psalm), and variance, verbatim (case preserved). A
/// trailing `= Default` is cut from the bound (a separate, unread obligation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateDecl {
    /// The declared template name, e.g. the `T` in `@template T of array`.
    pub name: String,
    /// The bound text after `of`/`as`, trimmed; `None` if unbounded, never empty when `Some`.
    pub bound: Option<String>,
    /// The declared variance (see [`Variance`]).
    pub variance: Variance,
}

/// Scan a raw docblock for `@template` declarations, returning each declared
/// name (the `T` in `@template T`), case preserved. Feeds the *template shadow
/// set* (issue #5): a declared name shadows a same-named class in that
/// declaration's docblock types. Name-only projection of [`scan_template_decls`].
#[must_use]
pub fn scan_template_names(text: &str) -> Vec<String> {
    scan_template_decls(text).into_iter().map(|d| d.name).collect()
}

/// Scan a raw docblock for `@template` declarations, whole — name, bound, and
/// variance, raw and unparsed — in source order, one pass (bound serves issue
/// #293, variance issue #294).
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
    let name_start = i + 1;
    let mut j = name_start;
    while j < line_end && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-') {
        j += 1;
    }
    let variance = template_tag_variance(&text[name_start..j])?;
    while j < line_end && (bytes[j] == b' ' || bytes[j] == b'\t') { // then the template name
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

/// The bound text of a `@template` line: after `of`/`as`, trailing `*/` and
/// `= Default` cut off. `None` when the line carries no bound keyword.
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
    if let Some(cut) = rest.find("*/") { // a one-line docblock ends on the same line
        rest = &rest[..cut];
    }
    if let Some(cut) = rest.find('=') { // cut a template default; no bound spelling has `=`
        rest = &rest[..cut];
    }
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// Whether `name` is an **inheritance-edge** tag: `extends` / `implements` or
/// their `template-` spellings, each optionally `@phpstan-`/`@psalm-`-prefixed
/// (ADR-0029). Not folded into [`template_tag_variance`]: `@template-extends`
/// declares no template name, and keeping the families separate is pinned by a
/// scanner test.
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

/// Scan a raw docblock for **inheritance-edge type arguments** — the `Box<int>`
/// in `@extends Box<int>` — in source order (ADR-0032 amendment, issue #294).
/// A phpdoc fact, not a syntax one: PHP source carries no `<int>`, so this is
/// recovered from the docblock, same pass shape as [`scan_template_decls`].
/// Each entry is the tag's raw tail, unparsed like [`TemplateDecl::bound`]; a
/// tail that doesn't parse contributes nothing and its siblings are unaffected.
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
    if let Some(cut) = rest.find("*/") { // a one-line docblock ends on the same line
        rest = &rest[..cut];
    }
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

// ---------------------------------------------------------------------------
// Magic-member tags: `@method` / `@property*` / `@mixin` / the `@phpstan-type`
// pair (ADR-0049 A14, issue #195).
//
// These declare members that live somewhere the index cannot see. Steins reads
// them not as member sources but as **obstacles** to an absence proof, so the
// scan recovers only presence and subject, never parses the type expression,
// and never fails on an unrecognizable tail (subject comes back empty instead).
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
    /// `@mixin Target` — members live on another class; the whole tag is the obstacle.
    Mixin,
    /// `@phpstan-type Alias = …` / `@psalm-type` — a local type alias, read for
    /// presence only.
    TypeAlias,
    /// `@phpstan-import-type Alias from Other` / `@psalm-import-type`.
    ImportedTypeAlias,
}

impl MagicTagKind {
    /// The tag's canonical spelling, printed by a posture report.
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

    /// Whether the tag is `@mixin` — the only kind whose subject is followed transitively.
    #[must_use]
    pub const fn is_mixin(self) -> bool {
        matches!(self, Self::Mixin)
    }

    /// Whether the tag declares a **member** — a method or property that exists
    /// at runtime and that the index cannot enumerate.
    ///
    /// This is the question ADR-0049 A14 records an obstacle on, and it is not
    /// the same question as "is this a magic tag" (issue #471). A14 names its
    /// set precisely — "the `@method` / `@property` / `@mixin` tags once they
    /// are read" — and the type-alias pair is not in it, because
    /// `@phpstan-type` / `@phpstan-import-type` declare **no member**. An alias
    /// is a type abbreviation, visible entirely in the docblock that spells it,
    /// so nothing about it makes a class unenumerable. A14's recording contract
    /// makes the same point from the other side: an obstacle must be
    /// dischargeable per subject by a plugin pack (ADR-0039), and there is no
    /// member here for a pack to declare.
    ///
    /// The alias tags are still **parsed and recorded as tags** — this predicate
    /// selects what becomes an obstacle, nothing else.
    #[must_use]
    pub const fn declares_member(self) -> bool {
        match self {
            Self::Method | Self::Property | Self::PropertyRead | Self::PropertyWrite | Self::Mixin => true,
            Self::TypeAlias | Self::ImportedTypeAlias => false,
        }
    }

    /// Recognize a magic-member tag name, applying the ADR-0029 prefix rule.
    /// The type-alias pair is prefixed-only — bare `@type` is not a tag.
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
    /// The tag's subject as written: method name for `@method`, property name
    /// without `$` for `@property*`, class reference (leading `\` preserved)
    /// for `@mixin`, alias name for `@phpstan-type`. Empty when the tail gave
    /// none — the tag still records; the obstacle is its presence.
    pub subject: String,
    /// Span of the tag, `@` to the end of trimmed content ([`DocTag::tag_span`] convention).
    pub tag_span: Span,
}

/// Scan a class-like docblock for the magic-member tags (ADR-0049 A14).
/// Separate from [`scan_docblock`] on purpose: those tags carry a type Steins
/// parses into an envelope, these carry one it refuses to parse.
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

/// The method name in an `@method` tail: the identifier before the first
/// non-nested `(` that isn't a parenthesized *type*'s (`callable(int): string`,
/// `Closure(): void`). Never parses the type, so an unrecognized tail yields an
/// empty name and the tag still records.
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

/// Type names PHPDoc lets carry a parenthesized signature, so they must not
/// read as an `@method` name. Case-insensitive, leading `\` ignored.
fn is_parenthesized_type_name(ident: &str) -> bool {
    let bare = ident.trim_start_matches('\\');
    ["callable", "closure", "pure-callable", "pure-closure"]
        .iter()
        .any(|k| bare.eq_ignore_ascii_case(k))
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Read a class reference token at `start`; a generic argument list or
/// punctuation ends it. Leading `\` preserved (fully-qualified vs import-relative).
fn read_class_ref(text: &str, bytes: &[u8], start: usize, end: usize) -> String {
    let mut j = start;
    while j < end && (is_ident_byte(bytes[j]) || bytes[j] == b'\\') {
        j += 1;
    }
    text[start..j].to_owned()
}

/// Read a bare identifier (no `\`) — the alias name of the `@phpstan-type` pair.
fn read_identifier(text: &str, bytes: &[u8], start: usize, end: usize) -> String {
    let mut j = start;
    while j < end && is_ident_byte(bytes[j]) {
        j += 1;
    }
    text[start..j].to_owned()
}

/// Find the byte offset of the first `$name` variable in `[start, end)`: the
/// first `$` followed by an identifier char — good enough for `@param T $x`.
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
        let s = tags[0].type_span;
        assert_eq!(&doc[s.start as usize..s.end as usize], "array<int, string>");
    }

    #[test]
    fn records_line_and_tag_spans() {
        let doc = "/**\n * @param int $x the count\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        let t = &tags[0];
        let line = &doc[t.line_span.start as usize..t.line_span.end as usize];
        assert_eq!(line, " * @param int $x the count");
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
        assert_eq!(tags[0].type_text, "null"); // `!` stripped off the type text
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
        let doc = "/** @assert int $x */"; // PHPStan has no unprefixed `@assert`
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
        assert_eq!(tags[0].type_text, ""); // declares a condition, not a type
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
        // Grammar is `parseRequiredVariableName` + optional description.
        let doc = "/**\n * @pure-unless-callable-is-impure $fn as long as it is pure\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].var_name.as_deref(), Some("$fn"));
    }

    #[test]
    fn psalm_prefixed_conditional_purity_is_not_a_tag() {
        // No `@psalm-` alias for this family.
        assert!(scan_docblock("/** @psalm-pure-unless-callable-is-impure $cb */").is_empty());
    }

    // ---- Trace annotation (ADR-0074 §2, issue #94) -------------------------

    #[test]
    fn scans_psalm_trace_with_a_variable_payload() {
        let doc = "/** @psalm-trace $x */"; // canonical spelling: Psalm's own vocabulary
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::TraceTag);
        assert_eq!(tags[0].var_name.as_deref(), Some("$x"));
        assert_eq!(tags[0].type_text, ""); // variable name only, no type
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
        // Neither upstream tool recognizes an unprefixed `@trace` (ADR-0074 §2).
        assert!(scan_docblock("/** @trace $x */").is_empty());
    }

    #[test]
    fn trace_annotation_comma_list_scans_one_tag_per_variable() {
        // ADR-0074 §7's multi-variable form.
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
        let tags = scan_docblock("/** @psalm-trace $a, $b watch these */");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].var_name.as_deref(), Some("$a"));
        assert_eq!(tags[1].var_name.as_deref(), Some("$b"));
    }

    #[test]
    fn trace_annotation_list_with_a_malformed_item_drops_the_whole_tag() {
        // No half-answered list: a bad token or dangling comma drops the whole tag.
        assert!(scan_docblock("/** @psalm-trace $a, b */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a, int $b */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a, $ */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a, */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $a,, $b */").is_empty());
    }

    #[test]
    fn trace_annotation_without_a_variable_is_malformed() {
        // Payload must be a variable name first — no type, no expression.
        assert!(scan_docblock("/** @psalm-trace */").is_empty());
        assert!(scan_docblock("/** @psalm-trace int $x */").is_empty());
        assert!(scan_docblock("/** @psalm-trace $ */").is_empty());
    }

    #[test]
    fn conditional_purity_needs_the_variable_first() {
        // A description preceding the variable is malformed; this tag alone drops.
        let doc = "/**\n * @pure-unless-callable-is-impure the $cb param\n * @param string $s\n */";
        let tags = scan_docblock(doc);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Param);
    }

    #[test]
    fn bare_impure_is_still_not_a_tag() {
        // ⊤ adds no information over the tag's absence (ADR-0082 §3).
        assert!(scan_docblock("/** @impure */").is_empty());
        assert!(scan_docblock("/** @phpstan-impure */").is_empty());
    }

    // ---- Interop envelopes (ADR-0082, issue #303) --------------------------

    /// The `(kind, labels)` every envelope test below asserts on.
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
        // Unlike ⊤, the empty envelope carries information (ADR-0082 §3).
        for doc in ["/** @pure */", "/** @phpstan-pure */", "/** @psalm-pure */"] {
            let tags = scan_docblock(doc);
            assert_eq!(tags.len(), 1, "{doc}");
            assert_eq!(tags[0].kind, TagKind::InteropEnvelope(EnvelopeTag::Pure), "{doc}");
            assert!(tags[0].labels.is_empty(), "{doc}");
            assert_eq!(tags[0].type_text, "", "{doc}"); // declares a bound, not a type
            assert!(tags[0].var_name.is_none(), "{doc}");
        }
        assert!(!scan_docblock("/** @pure */")[0].prefixed);
        assert!(scan_docblock("/** @phpstan-pure */")[0].prefixed);
        assert!(scan_docblock("/** @psalm-pure */")[0].prefixed);
    }

    #[test]
    fn pure_ignores_a_trailing_description() {
        // The pure side takes no labels: trailing text is always a description.
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
        // Spaced and tight commas alike; a dot-path is one label.
        assert_eq!(
            envelopes("/** @phpstan-impure io.db, nondet.time */"),
            impure(&["io.db", "nondet.time"])
        );
        assert_eq!(
            envelopes("/** @phpstan-impure io.db,nondet.time,exit */"),
            impure(&["io.db", "nondet.time", "exit"])
        );
        // The registry, not the scanner, decides which label names exist.
        assert_eq!(
            envelopes("/** @phpstan-impure io.fs.write, io.net.http */"),
            impure(&["io.fs.write", "io.net.http"])
        );
        assert_eq!(envelopes("/** @phpstan-impure io2 */"), impure(&["io2"]));
    }

    #[test]
    fn impure_accepts_a_trailing_paren_comment_after_labels() {
        // Legal only after a label (see the negative test).
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
        // `@impure` behaves exactly like `@phpstan-impure`: bare is ⊤, labeled is a bound.
        assert!(scan_docblock("/** @impure */").is_empty());
        assert_eq!(envelopes("/** @impure io */"), impure(&["io"]));
        assert!(!scan_docblock("/** @impure io */")[0].prefixed);
        assert!(scan_docblock("/** @phpstan-impure io */")[0].prefixed);
    }

    #[test]
    fn psalm_prefixed_impure_is_not_a_tag() {
        // No `@psalm-impure` alias, labeled or not (ADR-0082 §5).
        assert!(scan_docblock("/** @psalm-impure */").is_empty());
        assert!(scan_docblock("/** @psalm-impure io */").is_empty());
    }

    #[test]
    fn a_nonconforming_impure_remainder_reads_as_bare() {
        // A remainder that isn't a whole label list is prose, i.e. bare, i.e. ⊤.
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
        // Zero labels means bare, and bare impure is a non-tag.
        assert!(scan_docblock("/** @phpstan-impure (writes to the cache) */").is_empty());
        assert!(scan_docblock("/** @impure (why) */").is_empty());
        // Unclosed or re-opened isn't the one trailing comment either.
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
        // Unlike `@phpstan-impure`, bare has standing meaning (distributes over methods).
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
        // Nonconforming remainder falls back to bare: tag survives, bound doesn't.
        assert_eq!(
            envelopes("/** @phpstan-all-methods-impure talks to Redis */"),
            [(TagKind::InteropEnvelope(EnvelopeTag::AllMethodsImpure), Vec::new())]
        );
    }

    #[test]
    fn the_class_level_pair_has_no_aliases() {
        // Exactly the two `@phpstan-` spellings.
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
        let doc = "/** @phpstan-pure */"; // label-free ends at the name
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
        // Neither drops nor is dropped by type-carrying tags on the same docblock.
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
        // Matched before the envelope family, so it's not read as `@pure` with a description.
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
        // ADR-0073's zero-FP guard: never read as a cast of the receiver variable.
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
        let tags = scan_docblock("/** @var array{a: int} $arr */"); // plain target unflagged
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
        // `of` (PHPStan) and `as` (Psalm) both introduce the bound.
        assert_eq!(decl("/** @template T of array */").bound.as_deref(), Some("array"));
        assert_eq!(decl("/** @template T as \\Countable */").bound.as_deref(), Some("\\Countable"));
        assert_eq!(
            decl("/**\n * @template T of int|list<int>\n */").bound.as_deref(),
            Some("int|list<int>")
        );
        // A bare template, a description, or a nameless keyword carries no bound.
        assert_eq!(decl("/** @template T */").bound, None);
        assert_eq!(decl("/** @template T the element type */").bound, None);
        assert_eq!(decl("/** @template T of */").bound, None);
    }

    #[test]
    fn scans_template_bound_without_its_default() {
        assert_eq!(decl("/** @template T of array = array{} */").bound.as_deref(), Some("array"));
        assert_eq!(decl("/** @template TValue = mixed */").bound, None);
    }

    /// Variance survives the scanner for issue #294, though unconsumed today.
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
        let d = decl("/** @template-covariant TValue of string */"); // same line, same pass
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
        // `@template-extends` is a class-relation tag, not a declaration.
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
        // Both `@template-` spellings and precedence prefixes reach the same edge.
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
        // Matched on the whole tag name.
        assert!(scan_inheritance_args("/** @extendsomething Box<int> */").is_empty());
    }

    #[test]
    fn an_unparameterized_edge_still_yields_its_tail() {
        // Text, not meaning: a bare `@extends Box` is well-formed, no type arguments.
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
        // Generics, shapes and parenthesized callable types aren't the parameter list.
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
        // The tag's presence is the obstacle; an empty subject never drops it.
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
        assert_eq!( // generic argument list ends the reference
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
        assert!(scan_magic_member_tags("/** @type int $x */").is_empty()); // bare not a tag
    }

    #[test]
    fn only_the_member_declaring_kinds_are_obstacles() {
        // Issue #471: the alias pair is scanned like any other magic tag and is
        // NOT an ADR-0049 A14 obstacle — it declares no member. The two
        // questions were one predicate before, and a class whose only docblock
        // tag was `@phpstan-type` went silent for the whole absence family.
        for k in [
            MagicTagKind::Method,
            MagicTagKind::Property,
            MagicTagKind::PropertyRead,
            MagicTagKind::PropertyWrite,
            MagicTagKind::Mixin,
        ] {
            assert!(k.declares_member(), "{}", k.label());
        }
        for k in [MagicTagKind::TypeAlias, MagicTagKind::ImportedTypeAlias] {
            assert!(!k.declares_member(), "{}", k.label());
        }
    }

    #[test]
    fn magic_scan_ignores_the_envelope_tags_and_vice_versa() {
        let doc = "/**\n * @param int $n\n * @return string\n * @template T\n */";
        assert!(scan_magic_member_tags(doc).is_empty());
        assert!(scan_docblock("/**\n * @method int foo()\n * @mixin Bar\n */").is_empty());
    }

    #[test]
    fn every_magic_kind_labels_itself_with_its_own_spelling() {
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
        assert!(kinds.iter().filter(|k| k.is_mixin()).count() == 1); // only @mixin is followed
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
