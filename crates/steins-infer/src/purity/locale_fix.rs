//! The fix-it of a proven locale read at a printf-family call (ADR-0101 §3.6, D3).
//!
//! `sprintf('%.2f', $x)` reads the locale's decimal point; `sprintf('%.2F', $x)` renders the
//! same text under the `C` locale and always spells it `.`. The fix rides the two envelope
//! findings the read is held against, and only where the source can be edited byte-exactly:
//! the call is a plain call of `sprintf`, `printf`, `vsprintf` or `vprintf` whose first
//! argument is one string literal, quoted with `'` or `"`, that the scan below reads back to
//! the literal value the tree holds. Anything else gets no fix.
//!
//! The scan starts at the callee's name, which both a plain call's site and a higher-order
//! one's are anchored at, so it is exactly the text the parser read the first argument from:
//! the name, optional trivia, `(`, optional trivia, the literal, optional trivia, then `,` or
//! `)`. A literal followed by anything else (`'%f' . $x`) is not the whole argument and is left
//! alone. A conversion letter written as an escape (`"%\x66"`) is edited exactly: the whole
//! escape is replaced by the plain letter of its twin. An octal escape PHP truncates (`\546`)
//! is refused, since the rewrite would lose its warning.

use std::collections::HashMap;

use steins_db::PluginFacts;
use steins_syntax::{SiteKind, SiteOrigin};

use super::EffectSet;
use crate::Sym;
use crate::cx::Cx;
use crate::project::{Fix, FixEdit};
use crate::site::reach::{Frame, literal_format};
use crate::site::{Hit, HitKind, Knowledge, Lane, Target, resolve_site};

/// The title of the fix, which is also its message: under a locale whose decimal point is not
/// `.` the output changes.
pub(crate) const FIX_TITLE: &str = concat!(
    "use the locale-independent conversion (F, h, H): under a locale whose decimal point is ",
    "not '.' the output changes, the decimal point becomes '.' always"
);

/// The label the fix removes.
pub(crate) const LOCALE: &str = "global.read.setting.locale";

/// The edits that spell every locale-reading conversion of `hit`'s literal format
/// locale-independent, or `None` where the site is not a plain printf-family call with a literal
/// format the source can be edited at byte-exactly, or where a `g` or `G` conversion needs the
/// `h` or `H` a PHP below 8.0 lacks (`h_ok` is whether the run's floor reaches 8.0).
pub(crate) fn locale_edits(
    cx: &Cx,
    site: &SiteOrigin,
    hit: &Hit,
    h_ok: bool,
) -> Option<Vec<FixEdit>> {
    if !matches!(hit.kind, HitKind::Function) || !hit.labels.contains(&LOCALE) {
        return None;
    }
    // A string literal in a call's arguments may name a callable, so a printf call's site can be
    // a higher-order one: its span is the whole call and a plain call's is its name, and both
    // start at the name, which is where the scan starts.
    let SiteKind::Call { name, .. } = &site.kind else { return None };
    if !name.simple().eq_ignore_ascii_case(&hit.callee) {
        return None;
    }
    let family = steins_catalog::printf_family(&hit.callee)?;
    let format = literal_format(&site.const_args, &family)?;
    let conversions = steins_catalog::read_format(format)?.locale_conversions;
    // `h` and `H` are PHP 8.0's: below it the call gets no fix at all, since the `f` edits alone
    // would leave the `g` read standing and the `g` edits would print nothing.
    if conversions.is_empty() || (!h_ok && conversions.iter().any(|&(_, twin)| twin != b'F')) {
        return None;
    }
    let literal = Literal::read(cx.tree().source_from(site.span.start)?, site.span.start)?;
    let (decoded, pieces) = literal.decode()?;
    if decoded != format.as_bytes() {
        return None;
    }
    conversions
        .into_iter()
        .map(|(offset, twin)| {
            let (start, end) = *pieces.get(offset)?;
            Some(FixEdit {
                path: cx.path().to_owned(),
                start,
                end,
                replacement: char::from(twin).to_string(),
            })
        })
        .collect()
}

/// The edits that take **every** proven locale read out of one method body, or `None` when some
/// origin of the read is not a printf call of its own body that [`locale_edits`] can edit. A fix
/// that left a finding of the same label standing would not be the remedy of the Liskov finding
/// it rides.
///
/// The body's own sites are told from everything else structurally and not by where a finding
/// says it arose: a callee, a closure or a `new` the body reaches is an edge, and an edge whose
/// proven effects hold the read means an origin the edits cannot reach, so the method gets no fix
/// (a finding's provenance is a name and a line, which a callee on the same line shares).
pub(super) fn method_edits(
    cx: &Cx,
    frame: &Frame,
    (effects, plugins, h_ok): (&HashMap<Sym, EffectSet>, &PluginFacts, bool),
) -> Option<Vec<FixEdit>> {
    let (mut reads, mut editable) = (0_usize, 0_usize);
    let mut edits = Vec::new();
    let knowledge = Knowledge::Catalog { lane: Lane::Effects, plugins: Some(plugins) };
    for site in frame.sites {
        for target in resolve_site(cx, frame, site, &knowledge).targets {
            match target {
                Target::Engine(hit) if hit.labels.contains(&LOCALE) => {
                    reads += 1;
                    if let Some(found) = locale_edits(cx, site, &hit, h_ok) {
                        editable += 1;
                        edits.extend(found);
                    }
                }
                Target::Edge(edge) => {
                    let reaches = effects
                        .get(&edge.sym)
                        .is_some_and(|set| set.findings.iter().any(|f| f.label == LOCALE));
                    if reaches {
                        return None;
                    }
                }
                _ => {}
            }
        }
    }
    edits.sort_by_key(|e| (e.start, e.end));
    edits.dedup();
    (reads > 0 && reads == editable).then_some(edits)
}

/// A [`Fix`] of `edits`, or `None` for none.
pub(crate) fn fix_of(edits: Vec<FixEdit>) -> Option<Fix> {
    (!edits.is_empty()).then_some(Fix { title: FIX_TITLE, edits })
}

/// The first argument's string literal as the source writes it.
struct Literal<'a> {
    /// The text between the quotes.
    body: &'a [u8],
    /// The quote.
    quote: u8,
    /// The file offset of `body`'s first byte.
    base: u32,
}

/// What a literal evaluates to, and for each of those bytes the `[start, end)` file span of the
/// source piece that spelled it: a plain byte is itself, an escape is the whole escape.
type Decoded = (Vec<u8>, Vec<(u32, u32)>);

impl<'a> Literal<'a> {
    /// Read the literal that opens the argument list of the call whose name starts at `at`
    /// (`tail` is the file from there).
    fn read(tail: &'a str, at: u32) -> Option<Self> {
        let bytes = tail.as_bytes();
        let name = bytes
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'\\') || **b >= 0x80)
            .count();
        let mut i = skip_trivia(bytes, name)?;
        if bytes.get(i) != Some(&b'(') {
            return None;
        }
        i = skip_trivia(bytes, i + 1)?;
        let quote = *bytes.get(i)?;
        if quote != b'\'' && quote != b'"' {
            return None;
        }
        let open = i + 1;
        let mut j = open;
        loop {
            match *bytes.get(j)? {
                b'\\' => j += 2,
                b if b == quote => break,
                _ => j += 1,
            }
        }
        let after = skip_trivia(bytes, j + 1)?;
        if !matches!(bytes.get(after), Some(b',' | b')')) {
            return None;
        }
        let base = at.checked_add(u32::try_from(open).ok()?)?;
        Some(Self { body: bytes.get(open..j)?, quote, base })
    }

    /// The literal's value and where each byte of it came from; `None` for a spelling this scan
    /// does not read as PHP does.
    fn decode(&self) -> Option<Decoded> {
        let mut out = Decoded::default();
        let mut at = 0;
        while at < self.body.len() {
            let (len, bytes) =
                if self.body[at] == b'\\' { self.escape(at)? } else { (1, vec![self.body[at]]) };
            let start = self.base.checked_add(u32::try_from(at).ok()?)?;
            let end = start.checked_add(u32::try_from(len).ok()?)?;
            for byte in bytes {
                out.0.push(byte);
                out.1.push((start, end));
            }
            at += len;
        }
        Some(out)
    }

    /// The escape at `at` (a backslash): its source length and the bytes it stands for. Where
    /// PHP keeps the backslash (an unknown escape, or anything but `\\` and `\'` in a
    /// single-quoted literal) it is its own one-byte piece.
    fn escape(&self, at: usize) -> Option<(usize, Vec<u8>)> {
        let next = self.body.get(at + 1).copied()?;
        if self.quote == b'\'' {
            return Some(match next {
                b'\\' | b'\'' => (2, vec![next]),
                _ => (1, vec![b'\\']),
            });
        }
        let rest = &self.body[at + 1..];
        Some(match next {
            b'n' => (2, vec![b'\n']),
            b'r' => (2, vec![b'\r']),
            b't' => (2, vec![b'\t']),
            b'v' => (2, vec![0x0B]),
            b'e' => (2, vec![0x1B]),
            b'f' => (2, vec![0x0C]),
            b'\\' | b'$' | b'"' => (2, vec![next]),
            b'0'..=b'7' => {
                let digits = rest.iter().take(3).take_while(|b| (b'0'..=b'7').contains(b)).count();
                let value = rest[..digits].iter().fold(0_u32, |n, d| n * 8 + u32::from(d - b'0'));
                // `\400` and above are truncated by PHP with a warning: refused (`try_from`).
                (1 + digits, vec![u8::try_from(value).ok()?])
            }
            b'x' if rest.get(1).is_some_and(u8::is_ascii_hexdigit) => {
                let digits = rest[1..].iter().take(2).take_while(|b| b.is_ascii_hexdigit()).count();
                (2 + digits, vec![u8::try_from(hex(&rest[1..=digits])?).ok()?])
            }
            b'u' if rest.get(1) == Some(&b'{') => {
                let close = rest.iter().position(|&b| b == b'}')?;
                let ch = char::from_u32(hex(&rest[2..close])?)?;
                (close + 2, ch.to_string().into_bytes())
            }
            _ => (1, vec![b'\\']),
        })
    }
}

/// The value of a non-empty run of hex digits, or `None` for an empty run or one past `u32`.
fn hex(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0_u32, |n, d| {
        n.checked_mul(16)?.checked_add(char::from(*d).to_digit(16)?)
    })
}

/// Skip whitespace and comments from `at`; `None` for a comment that does not end.
fn skip_trivia(bytes: &[u8], mut at: usize) -> Option<usize> {
    loop {
        match bytes.get(at) {
            Some(b' ' | b'\t' | b'\r' | b'\n') => at += 1,
            Some(b'#') if bytes.get(at + 1) != Some(&b'[') => at = line_end(bytes, at),
            Some(b'/') if bytes.get(at + 1) == Some(&b'/') => at = line_end(bytes, at),
            Some(b'/') if bytes.get(at + 1) == Some(&b'*') => {
                let close = bytes[at + 2..].windows(2).position(|w| w == b"*/")?;
                at += 2 + close + 2;
            }
            _ => return Some(at),
        }
    }
}

/// The offset of the newline that ends the line `at` is on, or the end of the text.
fn line_end(bytes: &[u8], at: usize) -> usize {
    bytes[at..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |n| at + n)
}
