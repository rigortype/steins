//! Source files that are not valid UTF-8 (issue #927) — an interim inside ADR-0080 §3.2.
//!
//! # The defect
//!
//! A file is read as bytes and decoded with U+FFFD standing in for every ill-formed
//! sequence, and everything downstream — the parser, every span, every splice — runs on that
//! decoded text. A legacy Shift-JIS or EUC-JP file therefore arrives with distinct byte
//! strings collapsed into the same text: `"\x82\xA0"` and `"\x82\xA2"` are both
//! `"\u{FFFD}\u{FFFD}"`, so the lowered literals compare equal (a manufactured
//! `array.duplicate-key`, a `===` folded to `true`, a `strlen` of `6`), and two property
//! names that differ only in such bytes are one property.
//!
//! # The interim
//!
//! The decoded `String` stays the text spans and offsets are computed on — moving the source
//! to bytes is ADR-0080 §3.2 and stays deferred. What changes is that the decode no longer
//! throws the bytes away. [`decode_source`] records, for a file that is not valid UTF-8, a
//! [`Utf8Loss`]: one point per replacement (the decoded offset it sits at, and the one to
//! three raw bytes it replaced). Raw and decoded text are then interconvertible, and two
//! lanes read the map:
//!
//! * **Values.** A string literal whose token spans a loss point lowers to the bytes the
//!   source spells (`restore_literal`, `restore_part`), so it becomes a byte-string
//!   `PhpStr` and compares, hashes and keys by bytes, and the fold lane declines it
//!   (ADR-0080 §2.6). The parser's own unescaper does the escapes, so this module owns no
//!   second reading of them.
//! * **Names.** A name token (class, function, method, property, variable, constant) that
//!   spans a loss point cannot be told from another that differs in the replaced bytes, so
//!   the file is marked ([`SourceTree::names_lossy`](crate::SourceTree::names_lossy)) and the
//!   analyzer makes no claim from it (ADR-0080 §2.5, extended to names that are *lossily
//!   decoded* rather than non-UTF-8 values). A string literal read *as* a name (a callable,
//!   an effect label) does **not** mark the file: it takes the per-site silence of §2.5
//!   (`literal_name` answers `None` for non-UTF-8 bytes), because a non-UTF-8 name can only
//!   name something whose own declaring token is lossy, and that file is marked already.
//!
//! A valid UTF-8 file has no loss, enters no scope, and is untouched — a genuine `U+FFFD`
//! in one is an ordinary character, keyed and compared as it always was.
//!
//! # Scope and lifetime
//!
//! The lowering has no context struct to hang the map on and threading one would touch every
//! walker (the reason [`crate::memo`] and [`crate::stack_guard`] are thread-locals too), so
//! the map is active for exactly one [`crate::SourceTree::parse_with_loss`] call.

use std::borrow::Cow;
use std::cell::RefCell;

use mago_allocator::LocalArena;
use mago_span::HasSpan;
use mago_syntax::cst::{LiteralString, LiteralStringPart, Node, Program};
use mago_syntax_core::utils::parse_literal_string_in;

use crate::ast::Span;
use crate::{children, to_span};

/// The three bytes of U+FFFD, which is what a loss point occupies in the decoded text.
const REPLACEMENT: [u8; 3] = [0xEF, 0xBF, 0xBD];

/// One ill-formed sequence the decode replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Point {
    /// Where the replacement character starts in the decoded text.
    at: u32,
    /// How many raw bytes it replaced: one to three, the longest ill-formed prefix.
    len: u8,
    /// The raw bytes, the first `len` of them.
    bytes: [u8; 3],
}

/// How a file that is not valid UTF-8 was decoded: its replacement points, in order.
///
/// Together with the decoded text this is the raw file — [`Utf8Loss::raw_bytes`] puts it
/// back — and it is kept as the points alone rather than a second copy of the file because
/// that is all the raw bytes add. Empty for no file: [`decode_source`] answers `None`
/// for a valid one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Utf8Loss {
    points: Vec<Point>,
}

impl Utf8Loss {
    /// How many ill-formed sequences the decode replaced; never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the decode replaced nothing. Never true of a value [`decode_source`] returns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The raw file: `text` — the decode this loss came with — with each replacement
    /// character swapped back for the bytes it replaced.
    #[must_use]
    pub fn raw_bytes(&self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len());
        let mut cursor = 0usize;
        for p in &self.points {
            let at = p.at as usize;
            out.extend_from_slice(&text.as_bytes()[cursor..at]);
            out.extend_from_slice(&p.bytes[..usize::from(p.len)]);
            cursor = at + REPLACEMENT.len();
        }
        out.extend_from_slice(&text.as_bytes()[cursor..]);
        out
    }
}

/// A file's bytes as the text the analysis reads, and — when they are not valid UTF-8 — the
/// record of what the decode replaced.
///
/// The text is `String::from_utf8_lossy(&bytes)` byte for byte: one U+FFFD per ill-formed
/// sequence, which is what every span and fingerprint was always computed on. A valid file
/// takes the buffer it came in without a copy.
#[must_use]
pub fn decode_source(bytes: Vec<u8>) -> (String, Option<Utf8Loss>) {
    match String::from_utf8(bytes) {
        Ok(text) => (text, None),
        Err(e) => decode_lossy(e.as_bytes()),
    }
}

fn decode_lossy(bytes: &[u8]) -> (String, Option<Utf8Loss>) {
    let mut text = String::with_capacity(bytes.len());
    let mut points = Vec::new();
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        let invalid = chunk.invalid();
        if invalid.is_empty() {
            continue;
        }
        let mut raw = [0u8; 3];
        raw[..invalid.len()].copy_from_slice(invalid);
        points.push(Point {
            at: u32::try_from(text.len()).unwrap_or(u32::MAX),
            len: invalid.len() as u8,
            bytes: raw,
        });
        text.push('\u{FFFD}');
    }
    let loss = (!points.is_empty()).then_some(Utf8Loss { points });
    (text, loss)
}

// ---------------------------------------------------------------------------
// The per-parse scope.
// ---------------------------------------------------------------------------

struct Active {
    points: Vec<Point>,
}

impl Active {
    /// The points whose replacement character starts in `[start, end)`.
    fn within(&self, start: u32, end: u32) -> &[Point] {
        let lo = self.points.partition_point(|p| p.at < start);
        let hi = self.points.partition_point(|p| p.at < end);
        &self.points[lo..hi]
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// Activates a file's loss map for one parse and restores the previous state on drop.
///
/// A parse with no loss (every parse of a valid file) installs `None`, so every question
/// below answers "no loss" after one thread-local read.
pub(crate) struct Scope {
    previous: Option<Active>,
}

impl Scope {
    pub(crate) fn enter(loss: Option<&Utf8Loss>) -> Self {
        let next = loss
            .filter(|l| !l.is_empty())
            .map(|l| Active { points: l.points.clone() });
        Self { previous: ACTIVE.with_borrow_mut(|a| std::mem::replace(a, next)) }
    }

    /// Whether this parse has a loss map at all.
    pub(crate) fn is_lossy(&self) -> bool {
        ACTIVE.with_borrow(Option::is_some)
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|a| *a = self.previous.take());
    }
}

// ---------------------------------------------------------------------------
// Values: a literal's bytes, through the map.
// ---------------------------------------------------------------------------

/// The bytes a string-literal token spells, or `None` when the parser could not decode its
/// escapes.
///
/// In a file with no loss under the token this is the parser's own `value`, borrowed.
#[must_use]
pub(crate) fn restore_literal<'a>(ls: &'a LiteralString<'_>) -> Option<Cow<'a, [u8]>> {
    restore(to_span(ls.span), ls.raw, ls.value?, Spelling::Quoted)
}

/// [`restore_literal`] for one literal part of an interpolated string.
///
/// Reachable only from `lower_interpolation`, which handles the `"…$x…"` form alone: a
/// heredoc or nowdoc lowers to `Other` and never gets here.
#[must_use]
pub(crate) fn restore_part<'a>(part: &'a LiteralStringPart<'_>) -> Option<Cow<'a, [u8]>> {
    restore(to_span(part.span), part.raw, part.value?, Spelling::Part)
}

/// How a token's `raw` spelling relates to its `value`.
#[derive(Clone, Copy)]
enum Spelling {
    /// A whole literal token: an optional `b` prefix, the quotes, then the content.
    Quoted,
    /// One literal part of an interpolated string: double-quoted content, no quotes around it.
    Part,
}

/// Put the bytes the decode replaced back into `value`.
///
/// `value` is the parser's reading of `raw`, both of the *decoded* text, so each loss point
/// inside the token is three bytes of U+FFFD in both. Those bytes are copied across verbatim
/// — an escape consumes only ASCII, so a point is never inside one — which makes the
/// unescape **compositional at a point**: cutting `raw` there and unescaping the pieces
/// separately gives the pieces of `value`. The raw bytes are therefore spliced between the
/// separately-unescaped segments, and the parser's own unescaper does the unescaping, so
/// this function carries no table of escapes of its own.
///
/// The splice is checked, not trusted: the same walk with U+FFFD at each point must give
/// `value` back exactly, or the answer is `None` and the literal declines.
fn restore<'a>(
    span: Span,
    raw: &[u8],
    value: &'a [u8],
    spelling: Spelling,
) -> Option<Cow<'a, [u8]>> {
    let points = ACTIVE.with_borrow(|active| {
        active.as_ref().map(|a| a.within(span.start, span.end).to_vec()).unwrap_or_default()
    });
    if points.is_empty() {
        return Some(Cow::Borrowed(value));
    }
    let (lead, content, quote) = match spelling {
        Spelling::Quoted => quoted_content(raw)?,
        // The parser decodes the parts of an interpolated string as double-quoted content,
        // so a segment is run through the same decode. Only an interpolated string reaches
        // here today: a heredoc or nowdoc lowers to `Other`. If heredoc lowering lands, note
        // that mago decodes a heredoc part with `\"` as an escape and PHP does not, so the
        // check against `value` below would pass while both disagree with PHP.
        Spelling::Part => (0, raw, b'"'),
    };
    let arena = LocalArena::new();
    let unescape = |segment: &[u8]| -> Option<Vec<u8>> {
        parse_literal_string_in(&arena, segment, Some(quote), false).map(<[u8]>::to_vec)
    };
    let mut restored = Vec::with_capacity(value.len());
    let mut plain = Vec::with_capacity(value.len());
    let mut cursor = 0usize;
    for p in &points {
        let at = (p.at as usize).checked_sub(span.start as usize + lead)?;
        let segment = unescape(content.get(cursor..at)?)?;
        restored.extend_from_slice(&segment);
        plain.extend_from_slice(&segment);
        restored.extend_from_slice(&p.bytes[..usize::from(p.len)]);
        plain.extend_from_slice(&REPLACEMENT);
        cursor = at + REPLACEMENT.len();
    }
    let tail = unescape(content.get(cursor..)?)?;
    restored.extend_from_slice(&tail);
    plain.extend_from_slice(&tail);
    (plain == value).then_some(Cow::Owned(restored))
}

/// Split a whole literal token into its content and the quote its escapes follow.
///
/// Returns the byte offset the content starts at within `raw`, the content, and the quote
/// character; `None` for a token that is not a closed quoted literal.
fn quoted_content(raw: &[u8]) -> Option<(usize, &[u8], u8)> {
    let lead = usize::from(matches!(raw.first(), Some(b'b' | b'B')));
    let quote = *raw.get(lead).filter(|q| matches!(q, b'"' | b'\''))?;
    let content = raw.get(lead + 1..raw.len().checked_sub(1)?)?;
    (raw.last() == Some(&quote)).then_some((lead + 1, content, quote))
}

// ---------------------------------------------------------------------------
// Names.
// ---------------------------------------------------------------------------

/// A string literal read *as* a name — a callable's, an effect label — or `None` when the
/// bytes it spells are not valid UTF-8.
///
/// The bytes come through [`restore_literal`], so a replaced byte is the byte the file
/// spells and the name is non-UTF-8, not a U+FFFD that reads alike for two names. `None`
/// takes the site's existing decline (an opaque callback, `RunArg::Other`, an unrecognized
/// envelope) and does **not** mark the file: a non-UTF-8 name can only name something whose
/// declaring token is itself over a replaced byte, and that file is marked by
/// [`names_touch_a_loss`] (ADR-0080 §2.5, per-site silence).
#[must_use]
pub(crate) fn literal_name(ls: &LiteralString<'_>) -> Option<String> {
    let bytes = restore_literal(ls)?;
    std::str::from_utf8(&bytes).ok().map(str::to_owned)
}

/// Whether any name token in `program` spans a loss point.
///
/// Walks only the subtrees whose span holds a point, so a file with a handful of lossy
/// comments costs a handful of descents. A node on that path that is not a name and has no
/// children is a string, inline text or a comment-like leaf and says nothing; a name node
/// that holds a point is the answer. Name nodes are the identifiers (`Identifier` and its
/// three forms) and variables, which is every position a class, function, method, property,
/// constant, label, attribute or variable name is written.
#[must_use]
pub(crate) fn names_touch_a_loss(program: &Program<'_>) -> bool {
    let touched = |start: u32, end: u32| {
        ACTIVE.with_borrow(|a| a.as_ref().is_some_and(|a| !a.within(start, end).is_empty()))
    };
    fn visit(node: &Node<'_, '_>, touched: &dyn Fn(u32, u32) -> bool) -> bool {
        let span = node.span();
        if !touched(span.start.offset, span.end.offset) {
            return false;
        }
        match node {
            Node::Identifier(_)
            | Node::LocalIdentifier(_)
            | Node::QualifiedIdentifier(_)
            | Node::FullyQualifiedIdentifier(_)
            | Node::DirectVariable(_) => true,
            _ => children(node).iter().any(|c| visit(c, touched)),
        }
    }
    visit(&Node::Program(program), &touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_file_has_no_loss_and_keeps_its_buffer() {
        let (text, loss) = decode_source("<?php echo \"\u{3042}\u{FFFD}\";".as_bytes().to_vec());
        assert_eq!(text, "<?php echo \"\u{3042}\u{FFFD}\";");
        assert!(loss.is_none(), "a genuine U+FFFD is a character, not a loss");
    }

    #[test]
    fn the_decode_is_from_utf8_lossy_and_the_raw_bytes_come_back() {
        for case in [
            b"<?php \x80 \xe3\x81 end\n".to_vec(),
            vec![0xff, 0xfe, 0xfd],
            b"<?php $a = \"\x82\xa0\";\n".to_vec(),
            // A truncated four-byte sequence is one ill-formed run of three bytes.
            b"<?php // \xf0\x9f\x98 x\n".to_vec(),
            // A genuine replacement character beside a stray byte.
            b"\xef\xbf\xbd\x80\xef\xbf\xbd".to_vec(),
        ] {
            let (text, loss) = decode_source(case.clone());
            assert_eq!(text, String::from_utf8_lossy(&case), "{case:?}");
            let loss = loss.expect("each case holds an ill-formed sequence");
            assert_eq!(loss.raw_bytes(&text), case, "{case:?}");
        }
    }

    #[test]
    fn a_loss_point_records_where_and_what() {
        let (_, loss) = decode_source(b"ab\xe3\x81cd\x80".to_vec());
        let loss = loss.unwrap();
        assert_eq!(loss.len(), 2);
        assert_eq!(loss.points[0], Point { at: 2, len: 2, bytes: [0xe3, 0x81, 0] });
        assert_eq!(loss.points[1], Point { at: 7, len: 1, bytes: [0x80, 0, 0] });
    }
}
