//! The capture-group structure of a **literal** PCRE pattern (issue #149,
//! slice A of #148).
//!
//! [`capture_groups`] turns a PHP pattern string — delimiters, expression, and
//! trailing modifiers — into an ordered description of the groups it captures,
//! each carrying a [`MatchedText`] summary of what its entry can hold. It is
//! deliberately **not** a PCRE implementation. It answers only for patterns
//! whose group structure it can establish with certainty, and for everything
//! else it answers `None`.
//!
//! This is knowledge about a PHP extension's *argument* rather than its
//! signature, and so a peer of the crate's other tables: `out_params` already
//! says position 2 of `preg_match` is written, and this says what the written
//! array can hold. It stands alone — nothing else in the crate consults it yet,
//! and it consults nothing.
//!
//! # A decline is the normal answer, and it is silent
//!
//! `None` carries no diagnostic and no finding. Callers must treat it as "no
//! knowledge", never as "the pattern is wrong": a decline covers both a
//! construct this reader does not model and a pattern PCRE itself would refuse
//! to compile.
//!
//! The asymmetry that forces this discipline is the group **count**. Every
//! index after a miscounted construct shifts, so a single wrong verdict about
//! whether `(?i)` or `(*SKIP)` opens a group would silently mistype every later
//! entry of `$matches`. There is no safe partial answer, so anything unmodelled
//! declines outright.
//!
//! The converse does **not** hold: an answer is not a certificate that PCRE
//! would compile the pattern. Structural breakage this reader can see —
//! unbalanced delimiters or parentheses, an unterminated class — declines, but
//! a pattern can be well-nested and still be rejected by PCRE for reasons that
//! do not touch group structure. Callers that need "this pattern compiles" must
//! establish it some other way.
//!
//! # Measured, not recalled
//!
//! Every numbering and absence claim in this module was produced by running PHP
//! 8.5.9, and several are counter-intuitive:
//!
//! * A named group occupies a numeric index **as well as** its string key —
//!   the name is additional, never a replacement.
//! * The `x` (extended) and `n` (no-auto-capture) modifiers both change the
//!   count: under `x` a `#` comment can swallow a `(`, and under `n` a plain
//!   `(…)` stops capturing altogether. Both decline, in every spelling —
//!   trailing modifier, `(?x)`, `(?n:…)`.
//! * `\Q…\E` makes an enclosed `(` literal, **including inside a character
//!   class**, where it can also hide the class terminator.
//! * A POSIX class inside a bracket class (`[[:alpha:](]`) hides both a `]`
//!   and a `(` from a naive scan.
//! * `(*MARK:x)` adds a `'MARK'` key to `$matches` that is not a capture group
//!   at all, which is why the whole `(*…)` verb family declines.
//! * A group inside a **negative** lookaround is never populated on a
//!   successful match, so it can go unmatched wherever it sits — while a group
//!   inside a *positive* lookaround always participates, and therefore still
//!   closes the trailing-absence window for the groups before it.
//! * `\d` matches **Unicode** digits under the `u` modifier — PHP turns on
//!   PCRE2's Unicode properties with it, so `preg_match('/(\d+)/u', '١٢٣', $m)`
//!   succeeds while `is_numeric('١٢٣')` is `false`. `[0-9]` is unaffected. This
//!   is why the digit rule reads the modifier list before it answers.
//! * `\K` moves where the **overall match** starts, so `preg_match('/a\K0/',
//!   'a0', $m)` gives `$m[0] === '0'` — a two-character expression whose whole
//!   match is one falsy character. Group entries are untouched by it.
//!
//! # Why a mis-read of an uncompilable pattern is not a lie
//!
//! The length summary rests on an ordinary reading of quantifiers and
//! concatenation, and a pattern PCRE would *reject* can defeat it (`/0**/` is
//! not two characters). That costs nothing: a pattern PCRE refuses to compile
//! makes `preg_match` return `false`, so no caller ever reaches a branch where
//! this summary is asserted about anything. The claims here are conditional on
//! a match having happened, and an uncompilable pattern never matches.
//!
//! # Not in this module
//!
//! The trust gate (is the pattern a proven literal?), the out-parameter seed,
//! and the flag-dependent entry shapes (`PREG_OFFSET_CAPTURE`,
//! `PREG_UNMATCHED_AS_NULL`, `PREG_SET_ORDER` — issue #168) all live with the
//! seed's consumer in `steins-infer`. This module knows only about the pattern
//! string.
//!
//! # The literal enumeration (issue #177, slice F)
//!
//! Alongside the one-sided [`MatchedText`] floors, each group carries
//! [`literals`](CaptureGroup::literals): the sub-pattern's whole **language**,
//! when it is provably finite and small — `(a)` is `'a'`, `(£|€)` is
//! `'£'|'€'`, `(a(b))` is `'ab'` (a nested group is transparent: its own
//! entry keeps its own language). The enumeration rides this parser's own
//! walk — the same atoms, the same alternation and concatenation — and it
//! declines to `None`, silently and per group, far more often than it answers:
//!
//! * any quantifier other than exactly-one (`{1}` included) on an atom inside
//!   the body — `?`, `*`, `+`, `{n}`, `{n,m}` all decline the enclosing
//!   enumeration, and a quantifier admitting more than one iteration declines
//!   the enumeration of every group inside its atom too (measured:
//!   `preg_match('/(baz){2}/', 'bazbaz', $m)` gives `$m[1] === 'baz'`, the
//!   last iteration, so enumerating it would be sound — but the oracle this
//!   slice is calibrated against declines there, and agreement beats
//!   sharpness, the same trade slice E already made);
//! * a group's **own** quantifier is the one exception the calibration
//!   demands: `(b)?` still enumerates `'b'`, because the quantifier belongs
//!   to the surrounding text and the entry holds the body's single iteration
//!   whenever the group participates (measured, and the projection layers
//!   supply the `''`/`null`/absence story for the other paths);
//! * every character class, even `[ab]` — the enumeration-vs-class boundary
//!   is where miscounting starts;
//! * every escape that is not punctuation-for-itself: `\d` and friends denote
//!   sets, `\x{30}` is a second spelling of `'0'`, and one spelling per atom
//!   is the rule;
//! * `.`, backreferences, subroutine calls, `\K`;
//! * the `i` modifier, trailing or inline: measured,
//!   `preg_match('/(a)/i', 'A', $m)` captures `'A'`, so the pattern's own
//!   spelling is NOT the language — and the oracle declines case-insensitive
//!   patterns wholesale (`([xXa])/i` is `non-empty-string` there while the
//!   case-sensitive twin enumerates), so the case product is not attempted
//!   either;
//! * more than [`LITERAL_UNION_CAP`] members at any intermediate step.
//!
//! Zero-width constructs contribute the empty string (`(^a$)` is `'a'`,
//! measured), an empty alternation branch contributes `''` as a genuine
//! member (`(|a)` is `''|'a'`, measured), and `\Q…\E` contributes its bytes
//! verbatim. A decline never disturbs the [`MatchedText`] summary next to it.

/// The most members a [`literals`](CaptureGroup::literals) union may carry.
///
/// Chosen from two measurements (issue #177): the consumer domain's own
/// finite-value layer holds at most eight members before it widens, and the
/// largest literal union the calibration oracle expects on a row this
/// enumeration can reach has three (`''|'a'|'b'`) — so eight never truncates
/// an expectation while keeping every product this parser can build trivially
/// small. Past the cap the enumeration declines, at whatever intermediate
/// step the excess appears.
pub const LITERAL_UNION_CAP: usize = 8;

/// The capture groups of a pattern, in numeric order.
///
/// Index `0` of [`groups`](CaptureGroups::groups) is **group 1**. The
/// whole-match entry `$matches[0]` is not a capture group and is not listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureGroups {
    /// Groups in numeric order, index 0 being group 1 (the whole-match entry
    /// is not a group and is not listed).
    pub groups: Vec<CaptureGroup>,
    /// The whole expression, which is what `$matches[0]` holds.
    pub whole: MatchedText,
}

/// What the text a sub-pattern matched is guaranteed to look like.
///
/// Both fields are **one-sided**: each states a floor the matched text cannot
/// go below, and each degrades to "no knowledge" (`0` / `false`) rather than
/// guessing. A construct this reader does not model contributes nothing here
/// instead of declining the pattern, because a weaker summary only weakens the
/// fact a caller may state, while a wrong one manufactures one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedText {
    /// A lower bound on the number of **characters** the sub-pattern consumes.
    ///
    /// Characters, not bytes, and deliberately so: `£` is one character and two
    /// bytes, so counting characters is the weaker — and therefore always
    /// sound — bound on `strlen()`. It is what separates `''` (bound ≥ 1) and
    /// `'0'` (bound ≥ 2) from the strings a sub-pattern can actually produce.
    pub min_chars: u32,
    /// Whether **every** string the sub-pattern can produce is made only of
    /// ASCII digits `0`–`9`.
    ///
    /// A claim about the whole language, not the common case, and false
    /// wherever this reader cannot establish it for every alternative. Combined
    /// with `min_chars >= 1` it is exactly `is_numeric()`: measured, PHP calls
    /// every non-empty ASCII digit run numeric, leading zeros and 400 digits
    /// included.
    pub digits_only: bool,
}

impl CaptureGroups {
    /// The number of capture groups — the highest numeric key `$matches` can
    /// carry on a successful match.
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Whether the pattern captures nothing, so a successful match yields only
    /// `$matches[0]`.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// One capture group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureGroup {
    /// `(?<name>…)` / `(?'name'…)` / `(?P<name>…)`; `None` for a plain `(…)`.
    pub name: Option<String>,
    /// Whether an unmatched instance of this group could be the LAST populated
    /// entry — see the trailing-absence rule below.
    ///
    /// PHP drops a trailing unmatched group from `$matches` entirely, while an
    /// unmatched group with a populated entry after it appears as `''`
    /// (measured: `preg_match('/(a)(b)?/', 'a', $m)` gives keys `[0, 1]`,
    /// whereas `preg_match('/(a)(b)?(c)/', 'ac', $m)` gives
    /// `[0, 1, 2 => '', 3]`). So this flag is `true` exactly when the group can
    /// go unmatched at all **and** no later group is guaranteed to participate.
    ///
    /// The field is biased towards `true` on purpose. A spuriously optional key
    /// only weakens the fact a caller may state; a spuriously required one is
    /// unsound. Where this reader cannot prove a group always participates, it
    /// assumes it may not.
    pub can_be_trailing_absent: bool,
    /// Whether a successful match can leave this group's entry **present and
    /// equal to `''`** — the other half of the trailing-absence rule, and the
    /// one that governs the entry's *type* rather than its presence.
    ///
    /// The two halves are easy to conflate and PHPStan's own expectation for
    /// `/(a)(b)*(c)(d)*/` shows them apart:
    /// `array{0: …, 1: 'a', 2: string, 3: 'c', 4?: non-empty-string}`. The
    /// middle `(b)*` and the trailing `(d)*` are the same sub-pattern and get
    /// different element types, because an unmatched middle group is *present*
    /// as `''` while an unmatched trailing group is *absent*. So a middle
    /// optional group admits the empty string however its own
    /// [`MatchedText::min_chars`] reads, and only the last group keeps the
    /// non-empty claim.
    ///
    /// Measured, and the reason "trailing" is not the same as
    /// `can_be_trailing_absent`: `preg_match('/(a)(b)?(c)?/', 'ac', $m)` gives
    /// `$m[2] === ''` even though group 2 *can* be the last populated entry on
    /// another path. Any group with a group after it can be present-and-empty,
    /// so only the final group is exempt.
    pub can_be_present_empty: bool,
    /// Whether **some successful match can leave this group unmatched at all** —
    /// the raw, position-independent bit the two positional projections above are
    /// computed from (issue #168).
    ///
    /// This is the `preg_match_all` PATTERN_ORDER padding predicate: in that mode
    /// every column has exactly `ret` entries and an unmatched group contributes
    /// `''` (or `null` under `PREG_UNMATCHED_AS_NULL`) to its column **wherever it
    /// sits** (measured: `preg_match_all('/(\d)(a)?/', '1a 2 3a', $m)` gives
    /// `$m[2] === ['a', '', 'a']`). The middle-vs-trailing machinery of the two
    /// fields above is a `preg_match` phenomenon and must not be consulted for a
    /// column element — which is why the raw bit is carried rather than
    /// reconstructed from the projections.
    ///
    /// Same bias as its projections: `true` wherever participation cannot be
    /// proven, because a spurious `''`/`null` union member only weakens the fact
    /// while a missing one manufactures it.
    pub can_go_unmatched: bool,
    /// What this group's entry holds when the group participates — its body,
    /// with the group's own quantifier excluded.
    ///
    /// The quantifier belongs to the surrounding text, not to the capture: a
    /// repeated group captures its **last** iteration, so `preg_match('/(0){2}/',
    /// '00', $m)` gives `$m[1] === '0'` while `$m[0] === '00'` (measured).
    pub body: MatchedText,
    /// The body's whole language, when provably finite and small (issue #177):
    /// **every** string this group's entry can hold when the group
    /// participates, sorted and deduped, `1..=`[`LITERAL_UNION_CAP`] members.
    ///
    /// `None` is the normal answer and is silent — the decline discipline is
    /// the module-level story. `Some` is a claim about the whole language, so
    /// a consumer may state the union as the entry's exact type wherever the
    /// group participates; the unmatched paths (`''` padding, `null`, an
    /// absent key) stay the projection fields' business, exactly as they are
    /// for the [`MatchedText`] refinements.
    pub literals: Option<Vec<String>>,
}

/// Read the capture-group structure of a PHP PCRE pattern, or decline.
///
/// `pattern` is the full PHP form — leading whitespace, delimiter, expression,
/// closing delimiter, modifiers — exactly as it would be passed to
/// `preg_match`.
///
/// Returns `None` for every pattern whose group structure cannot be
/// established. This is a silent decline, not an error report; see the module
/// documentation.
pub fn capture_groups(pattern: &str) -> Option<CaptureGroups> {
    let Delimited { body, ucp_digits, case_insensitive } = split_delimited(pattern)?;
    let mut parser = Parser {
        src: body.as_bytes(),
        pos: 0,
        groups: Vec::new(),
        ucp_digits,
        match_start_reset: false,
        case_flagged: false,
    };
    let whole = parser.alternation()?;
    if parser.pos != parser.src.len() {
        // The scan stopped on a `)` with no group open: unbalanced.
        return None;
    }
    // `\K` discards everything matched before it from the overall match, so the
    // expression's own length says nothing about `$matches[0]` — measured,
    // `preg_match('/a\K0/', 'a0', $m)` gives the falsy `'0'` for a two-character
    // expression. Group entries keep their text, so only entry 0 gives up.
    let whole = if parser.match_start_reset { MatchedText::OPAQUE } else { whole.text };
    // Case-insensitivity multiplies every letter's spellings, and the pattern's
    // own spelling is not the language (measured: `/(a)/i` captures `'A'`) — so
    // an `i` anywhere, trailing or inline, declines every enumeration while
    // leaving the one-sided floors untouched.
    let case_blind = case_insensitive || parser.case_flagged;
    let raw = parser.take_groups()?;
    Some(CaptureGroups {
        groups: apply_trailing_absence(raw, case_blind),
        whole,
    })
}

impl MatchedText {
    /// No knowledge at all: the sub-pattern may match anything, including `''`.
    const OPAQUE: MatchedText = MatchedText { min_chars: 0, digits_only: false };
    /// The empty sub-pattern, and the identity of [`MatchedText::concat`].
    /// `digits_only` holds vacuously — `''` contains no non-digit.
    const EMPTY: MatchedText = MatchedText { min_chars: 0, digits_only: true };
    /// Exactly one character, of no known kind.
    const ONE_CHAR: MatchedText = MatchedText { min_chars: 1, digits_only: false };
    /// Exactly one ASCII digit.
    const ONE_DIGIT: MatchedText = MatchedText { min_chars: 1, digits_only: true };

    /// One sub-pattern followed by another. Lengths add (saturating, because a
    /// bound that stops growing is still a bound) and both halves must be
    /// digit-only for the concatenation to be.
    fn concat(self, next: MatchedText) -> MatchedText {
        MatchedText {
            min_chars: self.min_chars.saturating_add(next.min_chars),
            digits_only: self.digits_only && next.digits_only,
        }
    }

    /// One branch of an alternation or the other: the floor is the weaker of
    /// the two, and a claim survives only if it holds on every branch.
    fn alternate(self, other: MatchedText) -> MatchedText {
        MatchedText {
            min_chars: self.min_chars.min(other.min_chars),
            digits_only: self.digits_only && other.digits_only,
        }
    }

    /// A sub-pattern repeated at least `min_reps` times. Digit-ness is
    /// unaffected — repeating digits, zero times included, yields digits.
    fn repeat(self, min_reps: u32) -> MatchedText {
        MatchedText {
            min_chars: self.min_chars.saturating_mul(min_reps),
            digits_only: self.digits_only,
        }
    }

    /// The summary of a run of literal bytes.
    ///
    /// UTF-8 continuation bytes are not counted, so a multi-byte character
    /// counts once. For a pattern that is not valid UTF-8 that under-counts,
    /// which is the sound direction for a floor.
    fn literal_run(bytes: &[u8]) -> MatchedText {
        MatchedText {
            min_chars: bytes.iter().filter(|b| !is_utf8_continuation(**b)).count().try_into().unwrap_or(u32::MAX),
            digits_only: bytes.iter().all(u8::is_ascii_digit),
        }
    }
}

/// Whether a byte continues a multi-byte UTF-8 character rather than opening one.
fn is_utf8_continuation(b: u8) -> bool {
    b & 0b1100_0000 == 0b1000_0000
}

/// A sub-pattern's language while it is being built: byte strings, because the
/// parser walks bytes and a multi-byte character is stitched back together by
/// concatenation of its byte atoms. `None` is a decline and is absorbing.
type Lang = Option<Vec<Vec<u8>>>;

/// What the parser knows about one sub-pattern: the one-sided [`MatchedText`]
/// floors, plus the whole language where it is provably finite and small
/// (issue #177). The two travel together through the same walk, and the
/// language declining never disturbs the floors.
struct SubPattern {
    text: MatchedText,
    lang: Lang,
}

impl SubPattern {
    /// A sub-pattern with no language claim.
    fn undescribed(text: MatchedText) -> SubPattern {
        SubPattern { text, lang: None }
    }

    /// A zero-width construct: it consumes nothing and contributes `''`.
    fn zero_width() -> SubPattern {
        SubPattern { text: MatchedText::EMPTY, lang: Some(vec![Vec::new()]) }
    }

    /// One sub-pattern followed by another: floors add, languages multiply.
    fn concat(self, next: SubPattern) -> SubPattern {
        let lang = match (self.lang, next.lang) {
            (Some(a), Some(b)) => lang_bounded(
                a.iter()
                    .flat_map(|x| {
                        b.iter().map(move |y| {
                            let mut joined = x.clone();
                            joined.extend_from_slice(y);
                            joined
                        })
                    })
                    .collect(),
            ),
            _ => None,
        };
        SubPattern { text: self.text.concat(next.text), lang }
    }

    /// One alternation branch or the other: floors weaken, languages unite.
    fn alternate(self, other: SubPattern) -> SubPattern {
        let lang = match (self.lang, other.lang) {
            (Some(mut a), Some(b)) => {
                a.extend(b);
                lang_bounded(a)
            }
            _ => None,
        };
        SubPattern { text: self.text.alternate(other.text), lang }
    }
}

/// Sort, dedupe, and bound a language; past [`LITERAL_UNION_CAP`] the
/// enumeration declines, at whatever intermediate step the excess appears.
fn lang_bounded(mut members: Vec<Vec<u8>>) -> Lang {
    members.sort();
    members.dedup();
    (members.len() <= LITERAL_UNION_CAP).then_some(members)
}

/// A raw byte-string language as the public [`CaptureGroup::literals`] field.
///
/// Every member a quantifier-free walk of a valid-UTF-8 pattern can build is
/// itself valid UTF-8 (alternation splits on ASCII bytes, so a multi-byte
/// character is never divided between branches), and every construct that
/// could isolate a partial character — a quantifier on one byte of it —
/// already declines. The conversion still checks rather than trusts: an
/// invalid member declines the whole enumeration, because a literal fact that
/// cannot be spelled must not be stated.
fn lang_into_literals(lang: Lang) -> Option<Vec<String>> {
    lang?.into_iter().map(|m| String::from_utf8(m).ok()).collect()
}

/// Apply the trailing-absence rule to groups already tagged with whether they
/// can go unmatched.
///
/// Walking backwards: a group is trailing-absent when it can go unmatched and
/// nothing after it is guaranteed to participate. The first group that cannot
/// go unmatched shuts the window for everything before it, because its own
/// entry is always populated.
/// The companion rule for `can_be_present_empty` is coarser and cheaper: a group
/// with **any** group after it can be present-and-empty, because that later
/// group may participate on a path where this one does not. Only the final group
/// is exempt, since nothing can be populated after it to keep its entry alive.
fn apply_trailing_absence(raw: Vec<RawGroup>, case_blind: bool) -> Vec<CaptureGroup> {
    let last = raw.len().saturating_sub(1);
    let mut out: Vec<CaptureGroup> = Vec::with_capacity(raw.len());
    let mut guaranteed_after = false;
    for (i, g) in raw.into_iter().enumerate().rev() {
        out.push(CaptureGroup {
            name: g.name,
            can_be_trailing_absent: g.can_go_unmatched && !guaranteed_after,
            can_be_present_empty: g.can_go_unmatched && i != last,
            can_go_unmatched: g.can_go_unmatched,
            body: g.body,
            literals: if case_blind { None } else { lang_into_literals(g.lang) },
        });
        if !g.can_go_unmatched {
            guaranteed_after = true;
        }
    }
    out.reverse();
    out
}

// ---------------------------------------------------------------------------
// Delimiters and modifiers
// ---------------------------------------------------------------------------

/// Modifiers that provably leave the capture numbering alone.
///
/// `x`, `n`, and `J` are absent on purpose: each changes what counts as a
/// group, so each declines. Every other letter declines too — an unmodelled
/// modifier is exactly the case this reader must not guess about.
const NUMBERING_NEUTRAL_MODIFIERS: &[u8] = b"imsADSUXu";

/// A pattern with its delimiters and modifiers taken off.
struct Delimited<'a> {
    /// The bare expression.
    body: &'a str,
    /// Whether the `u` modifier is set, which switches PCRE2's Unicode
    /// properties on and so widens `\d` past ASCII (see the module docs).
    ucp_digits: bool,
    /// Whether the `i` modifier is set, which multiplies every letter's
    /// spellings and so declines the literal enumeration (issue #177) while
    /// leaving the numbering and the floors alone.
    case_insensitive: bool,
}

/// Strip the delimiters and modifiers, yielding the bare expression.
///
/// Mirrors what PHP itself does before handing the pattern to PCRE: skip
/// leading whitespace, take the first character as the delimiter, and — for the
/// four bracket pairs — track nesting to find its partner. Everything after the
/// closing delimiter is modifiers.
///
/// Measured: `'{a{b}'` fails to compile because the nesting scan never returns
/// to depth zero, while `'(((a)))'` yields the two-group expression `((a))`.
fn split_delimited(pattern: &str) -> Option<Delimited<'_>> {
    let bytes = pattern.as_bytes();
    let mut start = 0;
    while start < bytes.len() && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    let delim = *bytes.get(start)?;
    // PHP takes the first *byte* as the delimiter and rejects an alphanumeric
    // one. A non-ASCII lead byte declines rather than being scanned for: this
    // is a byte-wise scan, and a delimiter that is half a character would cut
    // the expression mid-sequence.
    if !delim.is_ascii() || delim.is_ascii_alphanumeric() || delim == b'\\' {
        return None;
    }
    let closing = match delim {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        b'<' => b'>',
        other => other,
    };
    let nests = closing != delim;

    let mut depth = 1usize;
    let mut i = start + 1;
    let end = loop {
        let c = *bytes.get(i)?;
        if c == b'\\' {
            // A backslash escapes the next byte, delimiter or not. That is what
            // makes `'/a\/b/'` a three-character expression rather than a
            // pattern that ends early.
            bytes.get(i + 1)?;
            i += 2;
            continue;
        }
        if c == closing {
            depth -= 1;
            if depth == 0 {
                break i;
            }
        } else if nests && c == delim {
            depth += 1;
        }
        i += 1;
    };

    let mut ucp_digits = false;
    let mut case_insensitive = false;
    for &m in &bytes[end + 1..] {
        // PHP ignores space, LF, and CR between modifiers — but not tab.
        if m == b' ' || m == b'\n' || m == b'\r' {
            continue;
        }
        if !NUMBERING_NEUTRAL_MODIFIERS.contains(&m) {
            return None;
        }
        ucp_digits |= m == b'u';
        case_insensitive |= m == b'i';
    }

    // The scan is byte-wise, but it only ever stops on ASCII, and UTF-8 never
    // encodes an ASCII byte inside a multi-byte sequence, so both cuts land on
    // character boundaries.
    let body = pattern.get(start + 1..end)?;
    Some(Delimited { body, ucp_digits, case_insensitive })
}

// ---------------------------------------------------------------------------
// The expression parser
// ---------------------------------------------------------------------------

/// A group as the parser collects it, before the trailing-absence rule runs.
struct RawGroup {
    name: Option<String>,
    /// Whether some successful match can leave this group unset — it sits under
    /// a `?`/`*`/`{0,n}`, in one branch of an alternation, inside a conditional,
    /// or inside a negative lookaround.
    can_go_unmatched: bool,
    /// The group's body, filled in once the body has been parsed.
    body: MatchedText,
    /// The body's language, filled in with the body — and wiped again by a
    /// surrounding quantifier that admits more than one iteration (issue #177:
    /// the capture then comes from an iteration, which the calibration oracle
    /// declines to model, so this does too).
    lang: Lang,
}

/// Inline flag letters. `x`, `n`, and `J` are listed so `(?xn)` is *recognised*
/// as a flag group and then declined, rather than falling through to the
/// catch-all and declining for the wrong reason.
const INLINE_FLAG_LETTERS: &[u8] = b"imsxnJU";

/// Inline flags that change what counts as a group.
const NUMBERING_HOSTILE_FLAGS: &[u8] = b"xnJ";

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    groups: Vec<RawGroup>,
    /// The `u` modifier is set, so `\d` is not ASCII-only.
    ucp_digits: bool,
    /// A `\K` was seen, so the overall match starts somewhere the expression's
    /// own length cannot locate.
    match_start_reset: bool,
    /// An inline flag group mentioned `i` (setting or clearing — either way a
    /// region of the pattern may be case-insensitive), so every literal
    /// enumeration declines (issue #177).
    case_flagged: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn rest(&self) -> &'a [u8] {
        &self.src[self.pos..]
    }

    /// Consume `s` if it comes next.
    fn eat(&mut self, s: &[u8]) -> bool {
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    /// Mark every group recorded since `from` as able to go unmatched.
    fn mark_optional_from(&mut self, from: usize) {
        for g in &mut self.groups[from..] {
            g.can_go_unmatched = true;
        }
    }

    /// The groups collected so far, or `None` if two share a name.
    ///
    /// Duplicate names need `(?J)` or `(?|…)`, both of which already decline,
    /// so this is a second line of defence against a numbering rule this reader
    /// does not model.
    fn take_groups(self) -> Option<Vec<RawGroup>> {
        for (i, g) in self.groups.iter().enumerate() {
            let Some(name) = &g.name else { continue };
            if self.groups[..i]
                .iter()
                .any(|earlier| earlier.name.as_deref() == Some(name.as_str()))
            {
                return None;
            }
        }
        Some(self.groups)
    }

    /// `branch ('|' branch)*`, stopping at `)` or end of input.
    ///
    /// With more than one branch, every group inside is skippable: some
    /// successful match takes a different branch. That over-approximates —
    /// nothing here proves the other branches are reachable — but it errs
    /// towards `can_be_trailing_absent`, which is the safe side.
    fn alternation(&mut self) -> Option<SubPattern> {
        let start = self.groups.len();
        let mut branches = 1usize;
        let mut sum = self.branch()?;
        loop {
            if self.peek() != Some(b'|') {
                break;
            }
            self.pos += 1;
            branches += 1;
            sum = sum.alternate(self.branch()?);
        }
        if branches > 1 {
            self.mark_optional_from(start);
        }
        Some(sum)
    }

    /// One alternation branch: atoms until `|`, `)`, or end of input.
    ///
    /// The empty branch is a genuine sub-pattern whose language is `{''}` —
    /// measured, `preg_match('/(|a)/', 'a', $m)` succeeds with `$m[1] === ''`,
    /// so `(|a)` enumerates to `''|'a'`.
    fn branch(&mut self) -> Option<SubPattern> {
        let mut sum = SubPattern::zero_width();
        while let Some(c) = self.peek() {
            if c == b'|' || c == b')' {
                break;
            }
            sum = sum.concat(self.atom()?);
        }
        Some(sum)
    }

    /// One quantified atom.
    fn atom(&mut self) -> Option<SubPattern> {
        let start = self.groups.len();
        let sub = match self.peek()? {
            b'(' => self.open_paren()?,
            b'[' => self.char_class()?,
            b'\\' => self.escape()?,
            _ => self.literal_byte(),
        };
        let quant = self.quantifier();
        if quant.min_reps == 0 {
            self.mark_optional_from(start);
        }
        if !quant.at_most_one {
            // More than one iteration reachable: the capture of any group
            // inside comes from an iteration. Measured, `/(baz){2}/` on
            // 'bazbaz' still captures `'baz'` — the last iteration — so the
            // body language would be sound; but the calibration oracle
            // declines every multi-iteration quantifier (`(baz){2}` expects
            // only `non-falsy-string` there), and agreement beats sharpness.
            for g in &mut self.groups[start..] {
                g.lang = None;
            }
        }
        // The atom's contribution to the surrounding language survives only an
        // exactly-once quantifier (none, `{1}`, `{1,1}`): `?` and `{0,1}` make
        // it variable and the rest make it repeated, and both decline (issue
        // #177 — the group's own entry above keeps its body language, because
        // the quantifier belongs to the surrounding text, not the capture).
        let exactly_once = quant.min_reps == 1 && quant.at_most_one;
        let lang = if exactly_once { sub.lang } else { None };
        Some(SubPattern { text: sub.text.repeat(quant.min_reps), lang })
    }

    /// One byte that is not the start of a group, a class, or an escape.
    ///
    /// `^` and `$` are the ones that matter: they assert a position rather than
    /// consuming a character, so a pattern like `/(^0$)/` must still be read as
    /// one character and stay falsy-capable.
    fn literal_byte(&mut self) -> SubPattern {
        let Some(c) = self.peek() else { return SubPattern::undescribed(MatchedText::EMPTY) };
        self.pos += 1;
        match c {
            // Zero-width, and zero contribution to the language: measured,
            // `preg_match('/(^a$)/', 'a', $m)` gives `$m[1] === 'a'`.
            b'^' | b'$' => SubPattern::zero_width(),
            // The dot names a set, not a literal; the stray quantifier bytes
            // sit in a pattern PCRE rejects ("nothing to repeat"), where a
            // language claim would be about matches that never happen.
            b'.' | b'*' | b'+' | b'?' => SubPattern::undescribed(MatchedText::ONE_CHAR),
            // Counted with the character its lead byte opened, so `£` is one.
            // The byte still joins the language: concatenation stitches a
            // multi-byte character back together from its byte atoms, and
            // every construct that could isolate one byte of it declines.
            c if is_utf8_continuation(c) => {
                SubPattern { text: MatchedText::EMPTY, lang: Some(vec![vec![c]]) }
            }
            c if c.is_ascii_digit() => {
                SubPattern { text: MatchedText::ONE_DIGIT, lang: Some(vec![vec![c]]) }
            }
            _ => SubPattern { text: MatchedText::ONE_CHAR, lang: Some(vec![vec![c]]) },
        }
    }

    /// Dispatch on what follows a `(`.
    fn open_paren(&mut self) -> Option<SubPattern> {
        debug_assert_eq!(self.peek(), Some(b'('));

        // Backtracking control verbs. `(*MARK:x)` puts a `'MARK'` key into
        // `$matches` that is not a group, and `(*ACCEPT)` truncates the match,
        // so the whole family declines rather than being waved through as
        // non-capturing.
        if self.rest().starts_with(b"(*") {
            return None;
        }

        if !self.rest().starts_with(b"(?") {
            self.pos += 1;
            return self.capturing_group(None);
        }

        // Order matters: `(?<=` and `(?<!` must be tested before `(?<name>`,
        // and `(?P<name>` before the inline-flag fallthrough.
        if self.eat(b"(?:") || self.eat(b"(?>") {
            // Non-capturing, and its body is matched in place — language
            // included. An atomic group only prunes backtracking, so every
            // string it can consume is still in its body's language and the
            // over-approximation stays sound.
            let sub = self.alternation()?;
            self.close_group()?;
            return Some(sub);
        }
        if self.eat(b"(?=") || self.eat(b"(?<=") {
            // Everything inside a *positive* lookaround participates whenever
            // the match succeeds — but it is an assertion, so it consumes no
            // characters and contributes none. Measured:
            // `preg_match('/(0(?=x))/', '0x', $m)` gives `$m[1] === '0'`, and
            // `preg_match('/(a(?=x))/', 'ax', $m)` gives `$m[1] === 'a'`.
            self.alternation()?;
            self.close_group()?;
            return Some(SubPattern::zero_width());
        }
        if self.eat(b"(?!") || self.eat(b"(?<!") {
            // A negative lookaround succeeds only when its body did NOT match,
            // so a group inside it is never populated. Measured:
            // `preg_match('/(a)(?!(b))/', 'a', $m)` leaves group 2 absent.
            let start = self.groups.len();
            self.alternation()?;
            self.close_group()?;
            self.mark_optional_from(start);
            return Some(SubPattern::zero_width());
        }
        if self.eat(b"(?#") {
            // A comment runs to the first `)`. A backslash does not escape
            // inside it: `'/(?#a\)b)/'` fails to compile. Measured:
            // `preg_match('/(a(?#hi)b)/', 'ab', $m)` gives `$m[1] === 'ab'`.
            self.skip_to_close_paren()?;
            return Some(SubPattern::zero_width());
        }
        if self.rest().starts_with(b"(?P=") {
            // A named backreference, not a group. What it matches is whatever
            // the referenced group did, which may be nothing at all: measured,
            // `preg_match('/(a?)b\1/', 'b', $m)` matches.
            self.pos += 4;
            self.skip_to_close_paren()?;
            return Some(SubPattern::undescribed(MatchedText::OPAQUE));
        }
        if self.eat(b"(?P<") {
            return self.named_group(b'>');
        }
        if self.eat(b"(?<") {
            return self.named_group(b'>');
        }
        if self.eat(b"(?'") {
            return self.named_group(b'\'');
        }
        if self.rest().starts_with(b"(?(") {
            self.pos += 3;
            return self.conditional();
        }
        // `(?|…)` branch reset, `(?R)` recursion, `(?1)` / `(?+1)` / `(?&name)`
        // / `(?P>name)` subroutine calls, and `(?C…)` callouts all reach the
        // inline-flag reader, which declines because their leading character is
        // not a flag letter.
        self.inline_flags()
    }

    /// `(?<name>…)`, `(?'name'…)`, `(?P<name>…)` — the cursor sits on the first
    /// character of the name, and `terminator` closes it.
    ///
    /// Measured: a named group takes a numeric index too, so this pushes one
    /// group exactly like a plain `(…)` and merely records the name alongside.
    fn named_group(&mut self, terminator: u8) -> Option<SubPattern> {
        let name_start = self.pos;
        while let Some(c) = self.peek() {
            if c == terminator {
                break;
            }
            // PCRE names are `[A-Za-z_][A-Za-z0-9_]*`; anything else declines.
            // Measured: `(?<1n>a)`, `(?<n->a)`, and `(?<>a)` all fail to
            // compile.
            let ok = c == b'_'
                || c.is_ascii_alphabetic()
                || (c.is_ascii_digit() && self.pos > name_start);
            if !ok {
                return None;
            }
            self.pos += 1;
        }
        if self.pos == name_start || self.peek()? != terminator {
            return None;
        }
        let name = std::str::from_utf8(&self.src[name_start..self.pos]).ok()?;
        let name = name.to_owned();
        self.pos += 1;
        self.capturing_group(Some(name))
    }

    /// Record a capture group, parse its body, and hand the body back — both to
    /// the group's own entry and to the surrounding text, which is where its
    /// quantifier will be applied. The language rides along on both sides,
    /// which is what makes a nested group transparent to an enclosing one:
    /// measured, `preg_match('/(a(b))/', 'ab', $m)` gives `$m[1] === 'ab'`
    /// while `$m[2] === 'b'`.
    fn capturing_group(&mut self, name: Option<String>) -> Option<SubPattern> {
        let index = self.groups.len();
        self.groups.push(RawGroup {
            name,
            can_go_unmatched: false,
            body: MatchedText::OPAQUE,
            lang: None,
        });
        let body = self.alternation()?;
        self.close_group()?;
        self.groups[index].body = body.text;
        self.groups[index].lang = body.lang.clone();
        Some(body)
    }

    /// `(?(cond)yes|no)` — the cursor sits just past `(?(`.
    ///
    /// Only the number and name conditions are modelled. An assertion condition
    /// (`(?(?=…)`) hides a whole sub-pattern, `(?(R…)` is a recursion test, and
    /// `(?(DEFINE)…)` holds groups that occupy indices yet never participate;
    /// all three decline.
    fn conditional(&mut self) -> Option<SubPattern> {
        let cond_start = self.pos;
        while self.peek()? != b')' {
            self.pos += 1;
        }
        let cond = std::str::from_utf8(&self.src[cond_start..self.pos]).ok()?;
        self.pos += 1;
        if !condition_is_modelled(cond) {
            return None;
        }
        // Whichever arm runs, the other one's groups stay unset — and with a
        // single arm the whole body may be skipped — so the entire subtree can
        // go unmatched. `alternation` reads the two arms as branches of one
        // alternation, which is wrong for the length (the condition is not an
        // arm), so the whole construct contributes nothing. The groups inside
        // keep their own body languages: a language is a claim about the entry
        // *when the group participates*, and which arm ran does not change
        // what the arm's own body can capture.
        let start = self.groups.len();
        self.alternation()?;
        self.close_group()?;
        self.mark_optional_from(start);
        Some(SubPattern::undescribed(MatchedText::OPAQUE))
    }

    /// `(?flags)` and `(?flags:…)` — the cursor sits on the `(`.
    fn inline_flags(&mut self) -> Option<SubPattern> {
        let flags_start = self.pos + 2;
        let mut i = flags_start;
        while let Some(&c) = self.src.get(i) {
            if c == b'-' || INLINE_FLAG_LETTERS.contains(&c) {
                i += 1;
            } else {
                break;
            }
        }
        let flags = &self.src[flags_start..i];
        if flags.is_empty() || flags == b"-" {
            return None;
        }
        if flags.iter().any(|c| NUMBERING_HOSTILE_FLAGS.contains(c)) {
            // Measured: `/(?x) a#(z)\n(b)/` and `/(?n)(a)(?<x>b)/` both report
            // one group where a flag-blind reader would say two.
            return None;
        }
        if flags.contains(&b'i') {
            // Setting or clearing — either way some region's case behavior is
            // not the default, and the literal enumeration declines pattern-wide
            // (issue #177; measured, `/(?i)(a)/` on 'A' captures 'A').
            self.case_flagged = true;
        }
        match self.src.get(i)? {
            // `(?i)` — a bare setting, spanning the rest of the enclosing
            // group. It opens nothing, so there is no body and no `)` to match
            // beyond this one.
            b')' => {
                self.pos = i + 1;
                Some(SubPattern::zero_width())
            }
            // `(?i:…)` — a scoped setting on a non-capturing group.
            b':' => {
                self.pos = i + 1;
                let sub = self.alternation()?;
                self.close_group()?;
                Some(sub)
            }
            // A digit here means `(?-1)`/`(?+1)`: a relative subroutine call,
            // not a flag setting.
            _ => None,
        }
    }

    /// Consume the `)` that closes the group currently being parsed.
    fn close_group(&mut self) -> Option<()> {
        if self.peek()? != b')' {
            return None;
        }
        self.pos += 1;
        Some(())
    }

    /// Consume everything through the next `)`, treating it as opaque.
    fn skip_to_close_paren(&mut self) -> Option<()> {
        while self.peek()? != b')' {
            self.pos += 1;
        }
        self.pos += 1;
        Some(())
    }

    /// An escape sequence outside a character class.
    ///
    /// For **group counting** a backslash and one following byte would do — no
    /// PCRE escape can introduce an unescaped `(`, `)`, or `[`, and `\Q…\E` is
    /// the only one that changes what a later `(` means. The length summary is
    /// what forces the rest of this arm list: an escape whose argument is left
    /// behind gets counted as extra literal characters, and every one of the
    /// forms below can spell the character `0`. Measured, all of `\x{30}`,
    /// `\x30`, `\060`, `\o{60}`, `\p{Nd}`, `\N` and `\C` match `'0'`, so
    /// reading `\x{30}` as `\x` plus a `{30}` quantifier would claim a
    /// thirty-character floor for a string that is exactly `'0'`.
    ///
    /// Every arm consumes exactly what the old backslash-plus-quantifier
    /// reading consumed, so the group numbering is untouched.
    fn escape(&mut self) -> Option<SubPattern> {
        debug_assert_eq!(self.peek(), Some(b'\\'));
        let next = *self.src.get(self.pos + 1)?;
        self.pos += 2;
        Some(match next {
            // A literal run, which ends at `\E` or at the end of the expression.
            b'Q' => self.take_quoted(),
            // A stray `\E` closes nothing and matches nothing.
            b'E' => SubPattern::zero_width(),
            // Zero-width assertions.
            b'A' | b'b' | b'B' | b'G' | b'z' | b'Z' => SubPattern::zero_width(),
            // `\K` discards what was matched before it from `$matches[0]`.
            // What it leaves a surrounding *group's* entry holding is a claim
            // this reader has not measured, so the language declines.
            b'K' => {
                self.match_start_reset = true;
                SubPattern::undescribed(MatchedText::EMPTY)
            }
            // The one escape that carries a digit claim — and only without the
            // `u` modifier, which widens it to every Unicode decimal digit.
            b'd' => SubPattern::undescribed(if self.ucp_digits {
                MatchedText::ONE_CHAR
            } else {
                MatchedText::ONE_DIGIT
            }),
            // One character, of a kind that is not known to be a digit. `\R`
            // can match the two characters of a CRLF, so one is still a floor.
            b'D' | b'w' | b'W' | b's' | b'S' | b'h' | b'H' | b'v' | b'V' | b'R' | b'X' | b'C'
            | b'a' | b'e' | b'f' | b'n' | b'r' | b't' => {
                SubPattern::undescribed(MatchedText::ONE_CHAR)
            }
            // `\cA` — one control character, spelled with a following byte.
            b'c' => {
                self.src.get(self.pos)?;
                self.pos += 1;
                SubPattern::undescribed(MatchedText::ONE_CHAR)
            }
            // One character named by a code point or a property. `\x{30}` is a
            // second spelling of `'0'`, and one spelling per atom is the
            // enumeration's rule, so the language declines with the set-like
            // properties rather than modelling code-point arithmetic.
            b'x' | b'o' | b'p' | b'P' | b'N' => {
                self.skip_escape_argument();
                SubPattern::undescribed(MatchedText::ONE_CHAR)
            }
            // A backreference or a subroutine call: whatever the referenced
            // group matched, which may be the empty string.
            b'g' | b'k' => {
                self.skip_escape_argument();
                SubPattern::undescribed(MatchedText::OPAQUE)
            }
            // `\1` is a backreference, `\060` an octal code point, and telling
            // them apart needs the group count. Neither claim is worth the
            // ambiguity, so both give up their length.
            b'0'..=b'9' => {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
                SubPattern::undescribed(MatchedText::OPAQUE)
            }
            // An escaped letter this reader does not know is one PCRE rejects,
            // so the floor it gives up costs nothing.
            c if c.is_ascii_alphabetic() => SubPattern::undescribed(MatchedText::OPAQUE),
            // An escaped punctuation character stands for itself — measured,
            // `preg_match('/(a\.b)/', 'a.b', $m)` gives `$m[1] === 'a.b'` and
            // `'axb'` does not match.
            c => SubPattern { text: MatchedText::literal_run(&[c]), lang: Some(vec![vec![c]]) },
        })
    }

    /// Consume an escape's argument: a `{…}`, `<…>` or `'…'` group, or a bare
    /// run of the characters a code point or property name is spelled with.
    ///
    /// Bounded on both sides: a delimited form that runs past a character no
    /// name may hold consumes nothing at all, so a malformed escape can never
    /// swallow the `(` that follows it. The bare run only ever eats
    /// alphanumerics and signs, which no group construct starts with.
    fn skip_escape_argument(&mut self) {
        let close = match self.peek() {
            Some(b'{') => b'}',
            Some(b'<') => b'>',
            Some(b'\'') => b'\'',
            _ => {
                while self.peek().is_some_and(is_escape_argument_byte) {
                    self.pos += 1;
                }
                return;
            }
        };
        let mut i = self.pos + 1;
        while let Some(&c) = self.src.get(i) {
            if c == close {
                self.pos = i + 1;
                return;
            }
            if !is_escape_argument_byte(c) && c != b'_' && c != b'.' {
                return;
            }
            i += 1;
        }
    }

    /// Read a `\Q` literal run, which ends at `\E` or at the end of input.
    ///
    /// The run is one literal, bytes verbatim — measured,
    /// `preg_match('/(\Qa|b\E)/', 'a|b', $m)` gives `$m[1] === 'a|b'`.
    fn take_quoted(&mut self) -> SubPattern {
        let src = self.src;
        let start = self.pos;
        while self.pos < src.len() {
            if self.rest().starts_with(b"\\E") {
                let run = &src[start..self.pos];
                self.pos += 2;
                return SubPattern {
                    text: MatchedText::literal_run(run),
                    lang: Some(vec![run.to_vec()]),
                };
            }
            self.pos += 1;
        }
        let run = &src[start..];
        SubPattern { text: MatchedText::literal_run(run), lang: Some(vec![run.to_vec()]) }
    }

    /// A bracketed character class. Nothing inside one is a group.
    ///
    /// Finding the real terminator is the whole job, and three rules do it, all
    /// measured: a `]` in first position (after an optional `^`) is a literal,
    /// `\Q…\E` hides a `]`, and a POSIX class `[:alpha:]` carries a `]` of its
    /// own — `'/[[:alpha:](]+(b)/'` has one group, not two.
    ///
    /// A class never enumerates, `[ab]` included (issue #177): the
    /// enumeration-vs-class boundary is where miscounting starts, so v1 keeps
    /// the whole family on the decline side.
    fn char_class(&mut self) -> Option<SubPattern> {
        debug_assert_eq!(self.peek(), Some(b'['));
        let src = self.src;
        self.pos += 1;
        let body_start = self.pos;
        self.eat(b"^");
        self.eat(b"]");
        loop {
            match self.peek()? {
                b']' => {
                    let body = &src[body_start..self.pos];
                    self.pos += 1;
                    // A class matches exactly one character, whatever its body
                    // holds — so only the digit claim needs the body read.
                    return Some(SubPattern::undescribed(MatchedText {
                        min_chars: 1,
                        digits_only: class_is_digits_only(body, self.ucp_digits),
                    }));
                }
                b'\\' => {
                    if self.eat(b"\\Q") {
                        self.take_quoted();
                    } else {
                        self.pos += 1;
                        self.src.get(self.pos)?;
                        self.pos += 1;
                    }
                }
                b'[' => {
                    // Collating elements (`[.ch.]`) and equivalence classes
                    // (`[=a=]`) are not supported by PCRE2 at all, so they
                    // decline rather than being read as a literal `[`.
                    if self.rest().starts_with(b"[.") || self.rest().starts_with(b"[=") {
                        return None;
                    }
                    if self.rest().starts_with(b"[:") {
                        self.skip_posix_class()?;
                    } else {
                        // A bare `[` inside a class is a literal.
                        self.pos += 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// A POSIX class such as `[:alpha:]` or `[:^alpha:]`, cursor on the `[`.
    fn skip_posix_class(&mut self) -> Option<()> {
        let mut i = self.pos + 2;
        if self.src.get(i) == Some(&b'^') {
            i += 1;
        }
        let name_start = i;
        while self.src.get(i).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
        }
        if i == name_start || !self.src[i..].starts_with(b":]") {
            // Not POSIX syntax after all; PCRE reads the `[` as a literal.
            self.pos += 1;
            return Some(());
        }
        self.pos = i + 2;
        Some(())
    }

    /// Read the quantifier after an atom. Zero `min_reps` means the atom can be
    /// skipped entirely; `min_reps` one with `at_most_one` is the answer for an
    /// unquantified atom.
    ///
    /// A `{…}` that is not a well-formed quantifier is a literal brace, which
    /// PCRE accepts: `'/(a)b{x}(c)/'` matches the text `b{x}` and has two
    /// groups.
    fn quantifier(&mut self) -> Quant {
        let quant = match self.peek() {
            Some(b'?') => {
                self.pos += 1;
                Quant { min_reps: 0, at_most_one: true }
            }
            Some(b'*') => {
                self.pos += 1;
                Quant { min_reps: 0, at_most_one: false }
            }
            Some(b'+') => {
                self.pos += 1;
                Quant { min_reps: 1, at_most_one: false }
            }
            Some(b'{') => match self.braced_quantifier() {
                Some(quant) => quant,
                None => return Quant::ONCE,
            },
            _ => return Quant::ONCE,
        };
        // A trailing `?` (lazy) or `+` (possessive) changes the search order,
        // never how few or how many times the atom may repeat.
        if matches!(self.peek(), Some(b'?' | b'+')) {
            self.pos += 1;
        }
        quant
    }

    /// `{n}`, `{n,}`, `{n,m}`, `{,m}` — consumed only if well formed. Answers
    /// the repeat bounds, saturating rather than overflowing on a count no
    /// subject could ever be long enough to satisfy (which also keeps the
    /// `at_most_one` claim honest: a saturated count is far above one).
    fn braced_quantifier(&mut self) -> Option<Quant> {
        let mut i = self.pos + 1;
        let digits_start = i;
        while self.src.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let min_digits = &self.src[digits_start..i];
        let has_comma = self.src.get(i) == Some(&b',');
        let max_start = i + 1;
        if has_comma {
            i += 1;
            while self.src.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        if self.src.get(i) != Some(&b'}') {
            return None;
        }
        // `{}` and `{,}` are not quantifiers; `{,m}` is `{0,m}`.
        if min_digits.is_empty() && (!has_comma || i == digits_start + 1) {
            return None;
        }
        let max_digits = if has_comma { &self.src[max_start..i] } else { min_digits };
        self.pos = i + 1;
        let min_reps = saturating_decimal(min_digits);
        // `{n,}` has no ceiling; `{n}` repeats exactly `n` times.
        let at_most_one = (!has_comma || !max_digits.is_empty())
            && saturating_decimal(max_digits) <= 1;
        Some(Quant { min_reps, at_most_one })
    }
}

/// An atom's quantifier, reduced to what the two consumers need: the floor
/// multiplies by `min_reps`, and the literal enumeration survives only where
/// `at_most_one` holds (a multi-iteration quantifier hands the capture to an
/// iteration — see [`Parser::atom`]).
struct Quant {
    /// The fewest times the atom may repeat.
    min_reps: u32,
    /// Whether the atom can repeat at most once (`?`, `{1}`, `{0,1}`, `{0}`,
    /// or no quantifier at all).
    at_most_one: bool,
}

impl Quant {
    /// The unquantified atom: exactly once.
    const ONCE: Quant = Quant { min_reps: 1, at_most_one: true };
}

/// A digit run as a saturating decimal count.
fn saturating_decimal(digits: &[u8]) -> u32 {
    let mut n: u32 = 0;
    for d in digits {
        n = n.saturating_mul(10).saturating_add(u32::from(d - b'0'));
    }
    n
}

/// Whether a byte may appear in an escape's argument — a code point, a Unicode
/// property name, or a group name or number.
fn is_escape_argument_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'+' || b == b'-'
}

/// Whether a character class admits **only** ASCII digits, reading its body —
/// everything between the `[` and its terminator.
///
/// Only four members are modelled: a digit, a range whose both ends are digits,
/// and `\d` or `[:digit:]` where the `u` modifier has not widened them.
/// Everything else answers `false`, which is the only claim a caller may not act
/// on. A negated class answers `false` outright — measured,
/// `preg_match('/([^a])/', '0', $m)` captures a digit, and any other complement
/// can too.
fn class_is_digits_only(body: &[u8], ucp_digits: bool) -> bool {
    /// The one POSIX class whose members are all digits — measured ASCII-only
    /// without `u` (`'/([[:digit:]]+)/'` refuses Arabic-Indic digits) and
    /// Unicode-wide with it, exactly like `\d`.
    const POSIX_DIGIT: &[u8] = b"[:digit:]";

    if body.first() == Some(&b'^') {
        return false;
    }
    let mut i = 0;
    while i < body.len() {
        if body[i] == b'[' {
            if body[i..].starts_with(POSIX_DIGIT) && !ucp_digits {
                i += POSIX_DIGIT.len();
                continue;
            }
            return false;
        }
        if body[i] == b'\\' {
            if body.get(i + 1) == Some(&b'd') && !ucp_digits {
                i += 2;
                continue;
            }
            return false;
        }
        if !body[i].is_ascii_digit() {
            return false;
        }
        if body.get(i + 1) == Some(&b'-') {
            if !body.get(i + 2).is_some_and(u8::is_ascii_digit) {
                return false;
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    true
}

/// Whether a conditional's condition is one this reader models.
///
/// Accepts a group number (`1`, `+1`, `-1`) and a group name — bare, or in
/// angle brackets. The quoted spelling `(?('n')…)` is deliberately absent:
/// measured, PCRE2 rejects it even though `(?'n'…)` is a valid *group*
/// spelling, so it is not a construct to model.
///
/// Everything else declines — notably `VERSION>=…`, any name starting with `R`
/// (which may be the recursion test rather than a name), and `DEFINE`, whose
/// body holds groups that occupy numeric indices yet are *never* populated.
/// Measured: `preg_match('/(?(DEFINE)(?<w>a))(?&w)(b)/', 'ab', $m)` reports
/// group 1 as `''` on every successful match.
fn condition_is_modelled(cond: &str) -> bool {
    let number = cond.strip_prefix(['+', '-']).unwrap_or(cond);
    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    let (name, delimited) = match cond.strip_prefix('<') {
        Some(rest) => (rest.strip_suffix('>'), true),
        None => (Some(cond), false),
    };
    let Some(name) = name else { return false };
    if !delimited && (name.starts_with('R') || name == "DEFINE" || name == "VERSION") {
        return false;
    }
    is_group_name(name)
}

/// Whether `name` is a syntactically valid PCRE group name.
fn is_group_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|b| b == b'_' || b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names of a pattern's groups, in numeric order.
    ///
    /// Panics on a decline, so every use of it is also an assertion that the
    /// reader answered.
    fn names(pattern: &str) -> Vec<Option<String>> {
        capture_groups(pattern)
            .unwrap_or_else(|| panic!("expected an answer for {pattern}"))
            .groups
            .iter()
            .map(|g| g.name.clone())
            .collect()
    }

    /// The expected argument for [`names`].
    fn expect(names: &[Option<&str>]) -> Vec<Option<String>> {
        names.iter().map(|n| n.map(str::to_owned)).collect()
    }

    /// How many groups a pattern captures.
    fn count(pattern: &str) -> usize {
        capture_groups(pattern)
            .unwrap_or_else(|| panic!("expected an answer for {pattern}"))
            .len()
    }

    /// `can_be_trailing_absent` per group, in numeric order.
    fn absent(pattern: &str) -> Vec<bool> {
        capture_groups(pattern)
            .unwrap_or_else(|| panic!("expected an answer for {pattern}"))
            .groups
            .iter()
            .map(|g| g.can_be_trailing_absent)
            .collect()
    }

    /// `can_be_present_empty` per group, in numeric order.
    fn present_empty(pattern: &str) -> Vec<bool> {
        capture_groups(pattern)
            .unwrap_or_else(|| panic!("expected an answer for {pattern}"))
            .groups
            .iter()
            .map(|g| g.can_be_present_empty)
            .collect()
    }

    /// The whole expression's matched-text summary — what `$matches[0]` holds.
    fn whole(pattern: &str) -> MatchedText {
        capture_groups(pattern)
            .unwrap_or_else(|| panic!("expected an answer for {pattern}"))
            .whole
    }

    /// Each group's body summary, in numeric order.
    fn bodies(pattern: &str) -> Vec<MatchedText> {
        capture_groups(pattern)
            .unwrap_or_else(|| panic!("expected an answer for {pattern}"))
            .groups
            .iter()
            .map(|g| g.body)
            .collect()
    }

    /// The character floor of every group body, in numeric order.
    fn floors(pattern: &str) -> Vec<u32> {
        bodies(pattern).iter().map(|t| t.min_chars).collect()
    }

    /// Whether each group body is provably digits-only, in numeric order.
    fn digits(pattern: &str) -> Vec<bool> {
        bodies(pattern).iter().map(|t| t.digits_only).collect()
    }

    /// Assert a decline, which is the reader's whole safety story.
    fn declines(pattern: &str) {
        assert_eq!(
            capture_groups(pattern),
            None,
            "expected a decline for {pattern}"
        );
    }

    // -----------------------------------------------------------------------
    // Plain captures
    // -----------------------------------------------------------------------

    #[test]
    fn plain_groups_are_counted_in_source_order() {
        // Measured: `preg_match('/(\d+)-(\w+)/', '12-ab', $m)` fills keys 1, 2.
        assert_eq!(names(r"/(\d+)-(\w+)/"), expect(&[None, None]));
    }

    #[test]
    fn a_pattern_without_groups_answers_with_an_empty_list() {
        let read = capture_groups("/abc/").expect("no groups is still an answer");
        assert!(read.is_empty());
        assert_eq!(read.len(), 0);
        assert!(capture_groups("//").expect("the empty pattern").is_empty());
    }

    #[test]
    fn nested_groups_number_outermost_first() {
        assert_eq!(count("/((a)(b))(c)/"), 4);
    }

    // -----------------------------------------------------------------------
    // Non-capturing constructs — each one shifts every later index if miscounted
    // -----------------------------------------------------------------------

    #[test]
    fn a_non_capturing_group_is_not_a_group() {
        assert_eq!(count("/(?:a)(b)/"), 1);
    }

    #[test]
    fn lookarounds_are_not_groups() {
        assert_eq!(count("/(?=a)(a)/"), 1);
        assert_eq!(count("/(?!x)(a)/"), 1);
        assert_eq!(count("/(?<=x)(a)/"), 1);
        assert_eq!(count("/(?<!x)(a)/"), 1);
    }

    #[test]
    fn a_group_inside_a_lookaround_still_counts() {
        // Measured: `preg_match('/(a)(?=(b))/', 'ab', $m)` fills keys 1 and 2.
        assert_eq!(count("/(a)(?=(b))/"), 2);
    }

    #[test]
    fn an_atomic_group_is_not_a_group() {
        assert_eq!(count("/(?>a)(b)/"), 1);
    }

    #[test]
    fn inline_flag_settings_are_not_groups() {
        assert_eq!(count("/(?i)(A)/"), 1);
        assert_eq!(count("/(?i:A)(b)/"), 1);
        assert_eq!(count("/(?-i)(a)/"), 1);
        assert_eq!(count("/(?i-sm:A)(b)/"), 1);
        assert_eq!(count("/(?U)(a)/"), 1);
    }

    #[test]
    fn a_comment_group_is_not_a_group() {
        assert_eq!(count("/(?#comment)(a)/"), 1);
        // A `(` inside the comment must not open anything either.
        assert_eq!(count("/(?#a ( b)(c)/"), 1);
    }

    #[test]
    fn a_named_backreference_is_not_a_group() {
        // Measured: `preg_match('/(?P<z>a)(?P=z)(b)/', 'aab', $m)` fills 1, 2.
        assert_eq!(names("/(?P<z>a)(?P=z)(b)/"), expect(&[Some("z"), None]));
    }

    #[test]
    fn k_and_numeric_backreferences_are_not_groups() {
        assert_eq!(count(r"/a\K(b)/"), 1);
        assert_eq!(count(r"/(a)\1(b)/"), 2);
        assert_eq!(count(r"/(?<n>a)\k<n>(b)/"), 2);
    }

    #[test]
    fn a_simple_conditional_is_not_a_group() {
        // Measured: `preg_match('/(a)?(?(1)b|c)(d)/', 'cd', $m)` fills 1, 2 —
        // the conditional itself takes no index.
        assert_eq!(count("/(a)?(?(1)b|c)(d)/"), 2);
        assert_eq!(count("/(?<n>a)?(?(<n>)b|c)(d)/"), 2);
        assert_eq!(count("/(?<n>a)?(?(n)b|c)(d)/"), 2);
        assert_eq!(count("/(a)?(?(+1)b|c)(d)/"), 2);
        assert_eq!(count("/(a)?(?(-1)b|c)(d)/"), 2);
        // Measured: PCRE2 rejects the quoted condition spelling outright.
        declines("/(a)?(?('1')b|c)(d)/");
    }

    // -----------------------------------------------------------------------
    // Named groups
    // -----------------------------------------------------------------------

    #[test]
    fn a_named_group_occupies_a_numeric_index_too() {
        // Measured: `preg_match('/(?<year>\d{4})-(?<mon>\d{2})/', '2026-08', $m)`
        // gives keys `[0, 'year', 1, 'mon', 2]` — the name is additional, never
        // a replacement.
        assert_eq!(
            names(r"/(?<year>\d{4})-(?<mon>\d{2})/"),
            expect(&[Some("year"), Some("mon")])
        );
    }

    #[test]
    fn all_three_named_spellings_read_alike() {
        assert_eq!(names("/(?<n>a)(b)/"), expect(&[Some("n"), None]));
        assert_eq!(names("/(?'n'a)(b)/"), expect(&[Some("n"), None]));
        assert_eq!(names("/(?P<n>a)(b)/"), expect(&[Some("n"), None]));
    }

    #[test]
    fn a_name_may_carry_digits_and_underscores_after_the_first_character() {
        assert_eq!(names("/(?<n1>a)/"), expect(&[Some("n1")]));
        assert_eq!(names("/(?<_n>a)/"), expect(&[Some("_n")]));
    }

    #[test]
    fn a_malformed_name_declines() {
        // Measured: `(?<1n>a)`, `(?<n->a)`, and `(?<>a)` all fail to compile.
        declines("/(?<1n>a)/");
        declines("/(?<n->a)/");
        declines("/(?<>a)/");
        declines("/(?<n>a/");
    }

    // -----------------------------------------------------------------------
    // Escape and character-class traps
    // -----------------------------------------------------------------------

    #[test]
    fn an_escaped_paren_is_not_a_group() {
        assert_eq!(count(r"/\((a)/"), 1);
        assert_eq!(count(r"/\)\((a)/"), 1);
        assert_eq!(count(r"/\\(a)/"), 1);
    }

    #[test]
    fn a_paren_inside_a_character_class_is_not_a_group() {
        assert_eq!(count("/[(](a)/"), 1);
        assert_eq!(count("/[)](a)/"), 1);
        assert_eq!(count("/[a-z(](b)/"), 1);
    }

    #[test]
    fn a_leading_close_bracket_in_a_class_is_a_literal() {
        // The classic traps: in `[]]` and `[^]]` the first `]` is a member, not
        // the terminator. Reading it as the terminator would leave `](a)` at
        // top level and still find one group here — so the assertion that bites
        // is the one where the class body holds a paren.
        assert_eq!(count(r"/[\]](a)/"), 1);
        assert_eq!(count("/[]](a)/"), 1);
        assert_eq!(count("/[^]](a)/"), 1);
        assert_eq!(count("/[])](a)/"), 1);
        assert_eq!(count("/[^](](a)/"), 1);
    }

    #[test]
    fn a_posix_class_hides_a_bracket_and_a_paren() {
        // Measured: `preg_match('/[[:alpha:](]+(b)/', 'x(b', $m)` fills key 1
        // only. A scan that stopped at the `:]` would see two groups.
        assert_eq!(count("/[[:alpha:](]+(b)/"), 1);
        assert_eq!(count("/[^[:alpha:](]+(b)/"), 1);
        assert_eq!(count("/[[:^alpha:](]+(b)/"), 1);
    }

    #[test]
    fn a_bare_bracket_inside_a_class_is_a_literal() {
        // Measured: `preg_match('/[a[b](c)/', 'ac', $m)` fills key 1.
        assert_eq!(count("/[a[b](c)/"), 1);
    }

    #[test]
    fn an_escaped_bracket_does_not_close_a_class() {
        assert_eq!(count(r"/[a\]b(](c)/"), 1);
        assert_eq!(count(r"/[\x{5D}](a)/"), 1);
    }

    #[test]
    fn quoted_runs_make_parens_literal() {
        // Measured: `preg_match('/\Q(a)\E(b)/', '(a)b', $m)` fills key 1.
        assert_eq!(count(r"/\Q(a)\E(b)/"), 1);
        assert_eq!(count(r"/\Qx\E(a)/"), 1);
        // An unterminated `\Q` runs to the end of the expression.
        assert_eq!(count(r"/x\Q(a)/"), 0);
    }

    #[test]
    fn a_quoted_run_inside_a_class_hides_the_terminator() {
        // Measured: `preg_match('/[\Qa]\E](b)/', 'ab', $m)` fills key 1.
        assert_eq!(count(r"/[\Qa]\E](b)/"), 1);
        assert_eq!(count(r"/[\Q]\E](b)/"), 1);
    }

    #[test]
    fn a_hex_escape_does_not_open_a_group() {
        // `\x{28}` is the character `(`, but as a literal.
        assert_eq!(count(r"/\x{28}(a)/"), 1);
        assert_eq!(count(r"/\050(a)/"), 1);
        assert_eq!(count(r"/\p{L}(a)/"), 1);
    }

    // -----------------------------------------------------------------------
    // Delimiters
    // -----------------------------------------------------------------------

    #[test]
    fn every_delimiter_style_reads_alike() {
        for pattern in ["/(a)/", "#(a)#", "~(a)~", "%(a)%", "!(a)!", "|(a)|", "\"(a)\""] {
            assert_eq!(count(pattern), 1, "for {pattern}");
        }
    }

    #[test]
    fn bracket_delimiters_nest() {
        // Measured: `'((a))'` is the one-group expression `(a)`, while
        // `'(((a)))'` is the two-group expression `((a))`.
        assert_eq!(count("((a))"), 1);
        assert_eq!(count("(((a)))"), 2);
        assert_eq!(count("{(a)}"), 1);
        assert_eq!(count("[(a)]"), 1);
        assert_eq!(count("<(a)>"), 1);
    }

    #[test]
    fn an_escaped_delimiter_does_not_close_the_pattern() {
        assert_eq!(count(r"/a\/b(c)/"), 1);
        assert_eq!(count(r"#a\#b(c)#"), 1);
    }

    #[test]
    fn leading_whitespace_before_the_delimiter_is_skipped() {
        // Measured: `preg_match(' /(a)/', 'a', $m)` matches.
        assert_eq!(count(" /(a)/"), 1);
        assert_eq!(count("\n/(a)/"), 1);
    }

    #[test]
    fn a_missing_or_unbalanced_delimiter_declines() {
        declines("");
        declines("/");
        declines("/(a");
        declines("abc");
        // Measured: `'{a{b}'` fails to compile — the nesting scan never gets
        // back to depth zero.
        declines("{a{b}");
        // An alphanumeric or backslash delimiter is rejected by PHP itself.
        declines("a(b)a");
        declines(r"\(a)\");
        // A non-ASCII lead byte would cut the expression mid-character.
        declines("あ(a)あ");
    }

    #[test]
    fn multibyte_text_inside_the_expression_is_read_byte_wise_without_harm() {
        // No ASCII byte appears inside a UTF-8 multi-byte sequence, so a
        // byte-wise scan cannot mistake one for a paren or a bracket.
        assert_eq!(count("/(あ)い(う)/u"), 2);
        assert_eq!(count("/[あい](う)/u"), 1);
    }

    #[test]
    fn numbering_neutral_modifiers_are_accepted() {
        for modifier in ["i", "m", "s", "A", "D", "S", "U", "X", "u", "imsu", " i ", ""] {
            let pattern = format!("/(a)/{modifier}");
            assert_eq!(count(&pattern), 1, "for {pattern}");
        }
    }

    #[test]
    fn an_unknown_modifier_declines() {
        // Measured: `'/(a)/z'` and `'/(a)/g'` fail to compile, and a tab is not
        // one of the modifier separators PHP skips.
        declines("/(a)/z");
        declines("/(a)/g");
        declines("/(a)/\t");
        // Measured: `'/a/b/'` closes at the FIRST unescaped delimiter, leaving
        // `b/` as the modifier text.
        declines("/a/b/");
    }

    // -----------------------------------------------------------------------
    // Modifiers and flags that change what counts as a group
    // -----------------------------------------------------------------------

    #[test]
    fn the_extended_modifier_declines() {
        // Measured: under `x` a `#` comment swallows a `(`, so
        // `'/ a # comment ( here\n(b)/x'` has ONE group.
        declines("/ a # comment ( here\n(b)/x");
        declines("/(a) (b)/x");
        declines("/(?x) a#(z)\n(b)/");
        declines("/(?xx) a (b)/");
        declines("/(?x:a (b))/");
    }

    #[test]
    fn the_no_auto_capture_modifier_declines() {
        // Measured: `preg_match('/(a)(b)/n', 'ab', $m)` fills NO numeric key —
        // a plain group stops capturing entirely.
        declines("/(a)(b)/n");
        declines("/(?n)(a)(?<x>b)/");
        declines("/(?n:(a))(b)/");
    }

    #[test]
    fn duplicate_name_flags_decline() {
        // Measured: under `J` both `(?<n>a)` and `(?<n>b)` exist and the string
        // key resolves to the last one — a numbering rule this reader does not
        // model.
        declines("/(?J)(?<n>a)(?<n>b)/");
        declines("/(?J:(?<n>a)(?<n>b))/");
        declines("/(?<n>a)(?<n>b)/J");
        // Even without the flag, a repeated name declines rather than answering.
        declines("/(?<n>a)(?<n>b)/");
    }

    // -----------------------------------------------------------------------
    // Declines
    // -----------------------------------------------------------------------

    #[test]
    fn branch_reset_declines() {
        // Measured: `preg_match('/(?|(a)|(b))(c)/', 'bc', $m)` has TWO groups,
        // because both branches share index 1.
        declines("/(?|(a)|(b))(c)/");
    }

    #[test]
    fn recursion_and_subroutine_calls_decline() {
        declines(r"/\((?:[^()]|(?R))*\)/");
        declines("/(a)(?1)/");
        declines("/(a)(?-1)/");
        declines("/(a)(?+1)(b)/");
        declines("/(?<n>a)(?&n)/");
        declines("/(?P<n>a)(?P>n)/");
    }

    #[test]
    fn backtracking_verbs_decline() {
        // `(*MARK:x)` adds a `'MARK'` key to `$matches` that is not a capture
        // group at all, and `(*ACCEPT)` truncates the match.
        declines("/(*SKIP)(a)/");
        declines("/(*MARK:x)(a)/");
        declines("/(*ACCEPT)(a)/");
        declines("/(*UTF)(a)/");
        declines("/a(*FAIL)|(b)/");
    }

    #[test]
    fn callouts_decline() {
        declines("/(?C1)(a)/");
        declines("/(?C)(a)/");
    }

    #[test]
    fn unmodelled_conditionals_decline() {
        // A DEFINE body holds groups that occupy indices yet never participate.
        declines("/(?(DEFINE)(?<w>a))(?&w)(b)/");
        // An assertion condition hides a whole sub-pattern.
        declines("/(?(?=a)a|b)(c)/");
        // A recursion test is not a group name.
        declines("/(a)?(?(R)b|c)(d)/");
        declines("/(a)?(?(R1)b|c)(d)/");
        declines("/(?(VERSION>=10.0)(a)|(b))(c)/");
    }

    #[test]
    fn structural_breakage_declines() {
        // Measured: both fail to compile.
        declines("/(a))/");
        declines("/[a(b)/");
        declines("/(a/");
        declines("/[a/");
        declines("/(?:a/");
        declines("/(?<n>a/");
    }

    #[test]
    fn an_unrecognised_paren_construct_declines() {
        declines("/(?)(a)/");
        declines("/(?^i)(a)/");
        declines("/(?~a)(b)/");
    }

    // -----------------------------------------------------------------------
    // The trailing-absence rule
    // -----------------------------------------------------------------------

    #[test]
    fn the_probe_cases_from_the_parent_issue() {
        // Measured: `preg_match('/(a)(b)?/', 'a', $m)` gives keys `[0, 1]` —
        // group 2 is ABSENT, not `''`.
        assert_eq!(absent("/(a)(b)?/"), [false, true]);
        // Measured: `preg_match('/(a)(b)?(c)/', 'ac', $m)` gives
        // `[0, 1, 2 => '', 3]` — group 2 is present-but-empty, because group 3
        // always participates.
        assert_eq!(absent("/(a)(b)?(c)/"), [false, false, false]);
    }

    #[test]
    fn a_group_that_cannot_go_unmatched_is_never_trailing_absent() {
        assert_eq!(absent(r"/(\d+)-(\w+)/"), [false, false]);
        assert_eq!(absent("/(a)(b){1,2}/"), [false, false]);
        assert_eq!(absent("/(a)(b)+/"), [false, false]);
        assert_eq!(absent("/(a)(b){2}/"), [false, false]);
    }

    #[test]
    fn every_min_zero_quantifier_spelling_makes_a_group_optional() {
        for pattern in [
            "/(a)(b)?/",
            "/(a)(b)*/",
            "/(a)(b){0}/",
            "/(a)(b){0,2}/",
            "/(a)(b){,3}/",
            "/(a)(b)?+/",
            "/(a)(b)??/",
            "/(a)(b)*?/",
        ] {
            assert_eq!(absent(pattern), [false, true], "for {pattern}");
        }
    }

    #[test]
    fn a_literal_brace_is_not_a_quantifier() {
        // Measured: `preg_match('/(a)b{x}(c)/', 'ab{x}c', $m)` matches, so
        // `{x}` is literal text and leaves group 1 non-optional.
        assert_eq!(absent("/(a)b{x}(c)/"), [false, false]);
        assert_eq!(absent("/(a){}(b)/"), [false, false]);
    }

    #[test]
    fn alternation_makes_every_branch_group_skippable() {
        // Measured: `preg_match('/(a)|(b)/', 'a', $m)` leaves group 2 absent.
        assert_eq!(absent("/(a)|(b)/"), [true, true]);
        // Measured: `preg_match('/(?:(a)|(b))(c)/', 'bc', $m)` gives
        // `[0, 1 => '', 2, 3]` — group 3 always participates, so neither
        // branch group can be the last populated entry.
        assert_eq!(absent("/(?:(a)|(b))(c)/"), [false, false, false]);
    }

    #[test]
    fn optionality_reaches_nested_groups() {
        // Measured: `preg_match('/((a)(b))?(c)/', 'c', $m)` gives all of 1, 2,
        // 3 as `''` with 4 populated.
        assert_eq!(absent("/((a)(b))?(c)/"), [false, false, false, false]);
        // Measured: `preg_match('/((a)?)/', '', $m)` gives keys `[0, 1]` —
        // group 2 is absent.
        assert_eq!(absent("/((a)?)/"), [false, true]);
        // Measured: `preg_match('/(x(a)?)(c)/', 'xc', $m)` gives group 2 as
        // `''`.
        assert_eq!(absent("/(x(a)?)(c)/"), [false, false, false]);
    }

    #[test]
    fn a_group_inside_a_negative_lookaround_can_always_go_unmatched() {
        // Measured: `preg_match('/(a)(?!(b))/', 'ac', $m)` gives keys `[0, 1]`
        // — group 2 is absent even though nothing quantifies it.
        assert_eq!(absent("/(a)(?!(b))/"), [false, true]);
        assert_eq!(absent("/(a)(?<!(b))/"), [false, true]);
    }

    #[test]
    fn a_group_inside_a_positive_lookaround_still_closes_the_window() {
        // Measured: `preg_match('/(a)?(?=(b))/', 'b', $m)` gives
        // `[0, 1 => '', 2 => 'b']` — group 2 participates, so group 1 is
        // present-but-empty rather than absent.
        assert_eq!(absent("/(a)?(?=(b))/"), [false, false]);
        assert_eq!(absent("/(a)?(?<=(b))/"), [false, false]);
    }

    #[test]
    fn a_conditional_body_can_always_go_unmatched() {
        // Whichever arm runs, the other arm's groups stay unset.
        assert_eq!(absent("/(a)?(?(1)(b)|(c))/"), [true, true, true]);
        // ...and a trailing mandatory group still closes the window.
        assert_eq!(absent("/(a)?(?(1)(b)|(c))(d)/"), [
            false, false, false, false
        ]);
    }

    #[test]
    fn named_groups_follow_the_same_absence_rule() {
        // Measured: `preg_match('/(?<x>a)(?<y>b)?/', 'a', $m)` drops BOTH the
        // `'y'` key and the numeric `2`; with a trailing `(?<z>c)` neither
        // vanishes.
        assert_eq!(absent("/(?<x>a)(?<y>b)?/"), [false, true]);
        assert_eq!(absent("/(?<x>a)(?<y>b)?(?<z>c)/"), [false, false, false]);
    }

    #[test]
    fn optionality_propagates_across_a_trailing_run() {
        assert_eq!(absent("/(a)(b)?(c)?/"), [false, true, true]);
        assert_eq!(absent("/(a)?(b)/"), [false, false]);
    }

    // -----------------------------------------------------------------------
    // The present-as-empty rule — the other half of trailing absence
    // -----------------------------------------------------------------------

    #[test]
    fn the_middle_and_trailing_halves_of_the_same_sub_pattern_part_ways() {
        // The case the whole coupling turns on. PHPStan's expectation is
        // `array{0: non-falsy-string, 1: 'a', 2: string, 3: 'c', 4?:
        // non-empty-string}`: `(b)*` and `(d)*` are the same sub-pattern and
        // get different element types.
        //
        // Measured: `preg_match('/(a)(b)*(c)(d)*/', 'ac', $m)` gives
        // `['ac', 'a', '', 'c']` — the middle group is PRESENT as `''` while
        // the trailing one is gone. So group 2 must admit the empty string
        // however its own floor reads, and group 4 must not.
        assert_eq!(present_empty("/(a)(b)*(c)(d)*/"), [false, true, false, false]);
        assert_eq!(absent("/(a)(b)*(c)(d)*/"), [false, false, false, true]);
        // ...and both bodies read identically, which is exactly why the flag
        // and not the body has to carry the difference.
        assert_eq!(bodies("/(a)(b)*(c)(d)*/")[1], bodies("/(a)(b)*(c)(d)*/")[3]);
        assert_eq!(floors("/(a)(b)*(c)(d)*/"), [1, 1, 1, 1]);
    }

    #[test]
    fn a_trailing_absent_group_can_still_be_present_as_empty() {
        // The trap inside the trap: `can_be_trailing_absent` is not enough on
        // its own. Measured, `preg_match('/(a)(b)?(c)?/', 'ac', $m)` gives
        // `['ac', 'a', '', 'c']` — group 2 is flagged trailing-absent (group 3
        // may not participate) and is STILL present as `''` on the path where
        // group 3 does. Only the last group is exempt.
        assert_eq!(absent("/(a)(b)?(c)?/"), [false, true, true]);
        assert_eq!(present_empty("/(a)(b)?(c)?/"), [false, true, false]);
    }

    #[test]
    fn a_group_that_always_participates_is_never_present_as_empty() {
        assert_eq!(present_empty(r"/(\d+)-(\w+)/"), [false, false]);
        assert_eq!(present_empty("/(a)(b)+/"), [false, false]);
        // A middle optional group with a mandatory group after it.
        assert_eq!(present_empty("/(a)(b)?(c)/"), [false, true, false]);
        // Measured: `preg_match('/(a)?(?=(b))/', 'b', $m)` gives
        // `['b', '', 'b']` — group 1 is present as `''`.
        assert_eq!(present_empty("/(a)?(?=(b))/"), [true, false]);
    }

    #[test]
    fn the_last_group_of_a_pattern_is_never_present_as_empty() {
        // Nothing can be populated after it to keep its entry alive, so an
        // unmatched last group is absent rather than empty. Measured:
        // `preg_match('/((a)?)/', '', $m)` gives keys `[0, 1]` only.
        assert_eq!(present_empty("/(a)?/"), [false]);
        assert_eq!(present_empty("/((a)?)/"), [false, false]);
    }

    // -----------------------------------------------------------------------
    // The character floor
    // -----------------------------------------------------------------------

    #[test]
    fn a_floor_counts_the_characters_a_sub_pattern_must_consume() {
        assert_eq!(floors("/(abc)/"), [3]);
        assert_eq!(floors("/(a|bc)/"), [1]);
        assert_eq!(floors("/(a(b|cd)e)/"), [3, 1]);
        assert_eq!(whole("/(a)(b)*(c)(d)*/").min_chars, 2);
        assert_eq!(whole("/Price: /").min_chars, 7);
    }

    #[test]
    fn a_quantifier_multiplies_the_floor_but_not_the_capture() {
        // Measured: `preg_match('/(0){2}/', '00', $m)` gives `['00', '0']` —
        // the group captures its LAST iteration, so its own quantifier belongs
        // to the surrounding text and not to its entry.
        assert_eq!(floors("/(0){2}/"), [1]);
        assert_eq!(whole("/(0){2}/").min_chars, 2);
        assert_eq!(floors("/((?:ab){2})/"), [4]);
        assert_eq!(floors("/(a+)/"), [1]);
        assert_eq!(floors("/(a*)/"), [0]);
        assert_eq!(floors("/(a{3,5})/"), [3]);
        assert_eq!(floors("/(a{0,5})/"), [0]);
    }

    #[test]
    fn a_zero_width_construct_adds_nothing_to_the_floor() {
        // Measured: all of these capture the single character `'0'`, so a
        // floor of two would call a falsy string non-falsy.
        assert_eq!(floors("/(^0$)/"), [1]);
        assert_eq!(floors(r"/(\b0\b)/"), [1]);
        assert_eq!(floors(r"/(\A0\z)/"), [1]);
        assert_eq!(floors("/(0(?=x))/"), [1]);
        assert_eq!(floors("/((?<=x)0)/"), [1]);
        assert_eq!(floors("/(0(?!x))/"), [1]);
        assert_eq!(floors("/((?#comment)0)/"), [1]);
        assert_eq!(floors("/((?i)0)/"), [1]);
    }

    #[test]
    fn a_multi_byte_literal_counts_as_one_character() {
        // `£` is two bytes and one character, and PHPStan reads `(£|€)` as
        // `non-empty-string` rather than `non-falsy-string`. Characters are the
        // weaker — hence always sound — bound on `strlen()`.
        assert_eq!(floors("/(£|€)/u"), [1]);
        assert_eq!(floors("/(£€)/u"), [2]);
    }

    #[test]
    fn an_escape_that_names_one_character_contributes_one() {
        // Measured: every one of these matches the single character `'0'`. A
        // reader that let `{30}` fall through to the quantifier would claim a
        // thirty-character floor for a string that is exactly `'0'`.
        assert_eq!(floors(r"/(\x{30})/"), [1]);
        assert_eq!(floors(r"/(\x30)/"), [1]);
        assert_eq!(floors(r"/(\o{60})/"), [1]);
        assert_eq!(floors(r"/(\p{Nd})/"), [1]);
        assert_eq!(floors(r"/(\N)/"), [1]);
        assert_eq!(floors(r"/(\C)/"), [1]);
        assert_eq!(floors(r"/(\cA)/"), [1]);
        assert_eq!(floors(r"/(\N{U+0030})/u"), [1]);
        assert_eq!(floors(r"/(\pL)/"), [1]);
    }

    #[test]
    fn a_backreference_or_an_octal_escape_gives_up_its_floor() {
        // Measured: `preg_match('/((a?)b\2)/', 'b', $m)` matches, so a
        // backreference can consume nothing. `\060` is the character `'0'` and
        // telling the two spellings apart needs the group count.
        assert_eq!(floors(r"/((a?)b\2)/"), [1, 0]);
        assert_eq!(floors(r"/(\060)/"), [0]);
        assert_eq!(floors(r"/(a)(b\1)/"), [1, 1]);
        assert_eq!(floors(r"/((?<n>a)\k<n>)/"), [1, 1]);
        assert_eq!(floors(r"/((?P<n>a)(?P=n))/"), [1, 1]);
    }

    #[test]
    fn the_match_start_reset_costs_entry_zero_its_floor() {
        // Measured: `preg_match('/a\K0/', 'a0', $m)` gives `['0']` — a
        // two-character expression whose whole match is one falsy character.
        // Group entries are untouched: `preg_match('/(a\Kb)/', 'ab', $m)`
        // gives `['b', 'ab']`.
        assert_eq!(whole(r"/a\K0/").min_chars, 0);
        assert_eq!(floors(r"/(a\Kb)/"), [2]);
    }

    #[test]
    fn an_alternation_floor_is_the_weakest_branch() {
        // Measured: `preg_match('/(a|b)|(?:c)/', 'c', $m)` gives `['c']`, one
        // character — which is why PHPStan calls entry 0 `non-empty-string`
        // and not `non-falsy-string`.
        assert_eq!(whole("/(a|b)|(?:c)/").min_chars, 1);
        assert_eq!(whole("/(ab)|(?:cd)/").min_chars, 2);
        assert_eq!(whole("/(ab)|c/").min_chars, 1);
    }

    #[test]
    fn a_conditional_gives_up_its_floor() {
        // The arms are not branches of one alternation — the condition sits in
        // front of them — so nothing here is worth a length claim. A reader
        // that took the two arms for an alternation would add two characters
        // that no match need contain.
        assert_eq!(whole("/x(a)?(?(1)bb|cc)/").min_chars, 1);
    }

    // -----------------------------------------------------------------------
    // The digit rule
    // -----------------------------------------------------------------------

    #[test]
    fn a_sub_pattern_that_can_only_produce_digits_says_so() {
        assert_eq!(digits(r"/(\d+)/"), [true]);
        assert_eq!(digits("/([0-9]+)/"), [true]);
        assert_eq!(digits(r"/([\d]{4})/"), [true]);
        assert_eq!(digits("/(007)/"), [true]);
        assert_eq!(digits("/(0|12)/"), [true]);
        assert_eq!(digits(r"/(\d+(?=x))/"), [true]);
        assert_eq!(digits("/([0-46-9])/"), [true]);
        // Measured: `preg_match('/([[:digit:]]+)/', '١٢٣', $m)` fails without
        // the `u` modifier, so the POSIX class is ASCII there.
        assert_eq!(digits("/([[:digit:]])/"), [true]);
        assert_eq!(digits("/([[:digit:]0-9])/"), [true]);
    }

    #[test]
    fn the_unicode_modifier_takes_the_digit_claim_off_backslash_d() {
        // Measured, and it overturns the obvious reading: PHP's `u` modifier
        // turns on PCRE2's Unicode properties, so `preg_match('/(\d+)/u',
        // '١٢٣', $m)` succeeds — while `is_numeric('١٢٣')` is `false`. An
        // explicit `[0-9]` is unaffected.
        assert_eq!(digits(r"/(\d+)/u"), [false]);
        assert_eq!(digits(r"/([\d])/u"), [false]);
        // Measured the same way: `preg_match('/([[:digit:]]+)/u', '١٢٣', $m)`
        // succeeds, so the POSIX class loses the claim under `u` too.
        assert_eq!(digits("/([[:digit:]])/u"), [false]);
        assert_eq!(digits("/([0-9]+)/u"), [true]);
        assert_eq!(digits("/(42)/u"), [true]);
    }

    #[test]
    fn a_sub_pattern_that_can_produce_anything_else_declines_the_digit_claim() {
        // Measured: `preg_match('/([\d.]+)/', '...', $m)` captures `'...'`,
        // and `preg_match('/([^a])/', '0', $m)` shows a negated class reaching
        // digits from the other side.
        assert_eq!(digits(r"/([\d.]{10})/"), [false]);
        assert_eq!(digits(r"/(\w+)/"), [false]);
        assert_eq!(digits("/([^a])/"), [false]);
        assert_eq!(digits("/([^0-9])/"), [false]);
        assert_eq!(digits("/(.)/"), [false]);
        assert_eq!(digits("/([0-9a])/"), [false]);
        // Measured: both spellings of a negated POSIX class reach `'a'`.
        assert_eq!(digits("/([[:^digit:]])/"), [false]);
        assert_eq!(digits("/([^[:digit:]])/"), [false]);
        assert_eq!(digits("/([[:alpha:]])/"), [false]);
        assert_eq!(digits(r"/(\d|x)/"), [false]);
        assert_eq!(digits(r"/(\dx)/"), [false]);
        assert_eq!(digits(r"/(\x{30})/"), [false]);
        assert_eq!(digits("/([]0])/"), [false]);
    }

    #[test]
    fn a_group_body_reads_independently_of_its_neighbours() {
        // The shape PHPStan spells `array{0: non-falsy-string, num:
        // numeric-string, 1: numeric-string, 2: non-empty-string}`.
        assert_eq!(floors(r"/\w-(?P<num>\d+)-(\w)/"), [1, 1]);
        assert_eq!(digits(r"/\w-(?P<num>\d+)-(\w)/"), [true, false]);
        // Measured: `preg_match('/\w-(?P<num>\d+)-(\w)/', 'a-12-b', $m)`
        // matches, and no subject shorter than five characters can.
        assert_eq!(whole(r"/\w-(?P<num>\d+)-(\w)/").min_chars, 5);
    }

    // -----------------------------------------------------------------------
    // The literal enumeration (issue #177, slice F)
    // -----------------------------------------------------------------------

    /// Each group's enumerated language, in numeric order — `None` per group
    /// is the decline, and every `Some` below was observed by running the
    /// pattern through PHP 8.5.9 in the same work session.
    fn literals(pattern: &str) -> Vec<Option<Vec<String>>> {
        capture_groups(pattern)
            .unwrap_or_else(|| panic!("expected an answer for {pattern}"))
            .groups
            .iter()
            .map(|g| g.literals.clone())
            .collect()
    }

    /// The expected argument for [`literals`]: one `Some` union, members in
    /// the reader's sorted order.
    fn union(members: &[&str]) -> Option<Vec<String>> {
        Some(members.iter().map(|m| (*m).to_owned()).collect())
    }

    #[test]
    fn a_single_literal_enumerates_to_its_one_spelling() {
        // Measured: `preg_match('/(a)/', 'xay', $m)` gives `$m[1] === 'a'`.
        assert_eq!(literals("/(a)/"), [union(&["a"])]);
        // Measured: `'a.b'` matches and `'axb'` does not — the escaped dot is
        // itself, not a set.
        assert_eq!(literals(r"/(a\.b)/"), [union(&["a.b"])]);
        // Measured: `preg_match('~(\{2})~', 'x{2}y', $m)` gives `'{2}'` — a
        // brace with nothing to quantify is a literal.
        assert_eq!(literals(r"~(\{2})~"), [union(&["{2}"])]);
        // Measured: `preg_match('/(\Qa|b\E)/', 'a|b', $m)` gives `'a|b'`.
        assert_eq!(literals(r"/(\Qa|b\E)/"), [union(&["a|b"])]);
    }

    #[test]
    fn an_alternation_of_literals_enumerates_every_branch() {
        // Measured: `'xbary'` captures `'bar'` and `'xfooy'` captures `'foo'`.
        assert_eq!(literals("/(foo|bar)/"), [union(&["bar", "foo"])]);
        // Measured: both currencies captured, one character each, two bytes —
        // the byte atoms of a multi-byte character stitch back together.
        assert_eq!(literals("/Price: (£|€)/"), [union(&["£", "€"])]);
        // Duplicate branches are one member.
        assert_eq!(literals("/(a|a)/"), [union(&["a"])]);
    }

    #[test]
    fn an_empty_branch_contributes_the_empty_string_as_a_member() {
        // Measured: `preg_match('/(|a)/', 'a', $m)` succeeds with `$m[1] ===
        // ''`, and `preg_match('/(|a)a$/', 'aa', $m)` reaches `'a'`.
        assert_eq!(literals("/(|a)/"), [union(&["", "a"])]);
        assert_eq!(literals("/(a|)/"), [union(&["", "a"])]);
    }

    #[test]
    fn a_nested_group_is_transparent_to_its_encloser() {
        // Measured: `preg_match('/(a(b))/', 'ab', $m)` gives `$m[1] === 'ab'`
        // and `$m[2] === 'b'` — the oracle's own `~^(a(b))$~` expectation.
        assert_eq!(literals("/(a(b))/"), [union(&["ab"]), union(&["b"])]);
        // A non-capturing wrapper is transparent the same way, and the product
        // of small alternations enumerates up to the cap: measured, `'ace'`
        // and `'bdf'` both capture themselves.
        assert_eq!(
            literals("/((?:a|b)(?:c|d)(?:e|f))/"),
            [union(&["ace", "acf", "ade", "adf", "bce", "bcf", "bde", "bdf"])]
        );
    }

    #[test]
    fn past_the_cap_the_enumeration_declines() {
        // One more doubling than the case above: sixteen members.
        assert_eq!(literals("/((?:a|b)(?:c|d)(?:e|f)(?:g|h))/"), [None]);
    }

    #[test]
    fn zero_width_constructs_contribute_the_empty_string() {
        // Measured: `preg_match('/(^a$)/', 'a', $m)` gives `$m[1] === 'a'`.
        assert_eq!(literals("/(^a$)/"), [union(&["a"])]);
        // Measured: `preg_match('/(a(?=x))/', 'ax', $m)` gives `$m[1] === 'a'`.
        assert_eq!(literals("/(a(?=x))/"), [union(&["a"])]);
        // Measured: `preg_match('/(a(?#hi)b)/', 'ab', $m)` gives `'ab'`.
        assert_eq!(literals("/(a(?#hi)b)/"), [union(&["ab"])]);
    }

    #[test]
    fn a_groups_own_single_iteration_quantifier_keeps_its_body_language() {
        // The quantifier belongs to the surrounding text, not the capture:
        // measured, `preg_match('/x(b)?/', 'xb', $m)` gives `$m[1] === 'b'`,
        // and the oracle's `~^a\.(b)?(c)?d~` expectation is `1?: ''|'b'` — the
        // `''` member is the present-empty projection's business, joined on by
        // the consumer, not a language member here.
        assert_eq!(literals("/x(b)?/"), [union(&["b"])]);
        assert_eq!(literals(r"/a\.(b)?(c)?d/"), [union(&["b"]), union(&["c"])]);
    }

    #[test]
    fn a_multi_iteration_quantifier_declines_the_groups_inside_it() {
        // Measured: `preg_match('/(baz){2}/', 'bazbaz', $m)` gives `$m[1] ===
        // 'baz'` — the last iteration, so enumerating WOULD be sound. The
        // calibration oracle declines there (`(baz){2}` expects only
        // `non-falsy-string`), and agreement beats sharpness, so this declines
        // too — the measured decision of issue #177.
        assert_eq!(literals("/(baz){2}/"), [None]);
        assert_eq!(literals("/(d)*/"), [None]);
        assert_eq!(literals("/(b)+/"), [None]);
        assert_eq!(literals("/(a(b){2})/"), [None, None]);
    }

    #[test]
    fn any_quantifier_inside_the_body_declines_the_enumeration() {
        // The oracle enumerates `(a|bc?)` to `'a'|'b'|'bc'`; v1 stays on the
        // decline side of every body-level quantifier, `{1}` excepted — the
        // floors still answer, so the entry keeps its slice-E refinement.
        assert_eq!(literals("/(a|bc?)/"), [None]);
        assert_eq!(literals("/(ab*)/"), [None]);
        assert_eq!(literals("/(ab+)/"), [None]);
        // Measured: `preg_match('/(a{2})/', 'aa', $m)` gives `'aa'` — sound to
        // enumerate, declined for the same agreement reason as `(baz){2}`.
        assert_eq!(literals("/(a{2})/"), [None]);
        assert_eq!(literals("/(a{2,3})/"), [None]);
        // `{1}` is the identity: measured, `preg_match('/(a{1})/', 'a', $m)`
        // gives `'a'`.
        assert_eq!(literals("/(a{1})/"), [union(&["a"])]);
        // The inner `?` declines the encloser, never the inner group's own
        // entry: the oracle's `~^(a(b)?)$~` row expects `1: 'a'|'ab', 2?: 'b'`
        // and v1 answers `non-empty-string` for group 1, `'b'` for group 2.
        assert_eq!(literals("/(a(b)?)/"), [None, union(&["b"])]);
    }

    #[test]
    fn sets_and_references_decline_the_enumeration() {
        // Every character class, even a two-member one — the v1 boundary.
        assert_eq!(literals("/([ab])/"), [None]);
        assert_eq!(literals("/([157])/"), [None]);
        // Set-denoting escapes, the dot, and code-point spellings.
        assert_eq!(literals(r"/(\d)/"), [None]);
        assert_eq!(literals("/(.)/"), [None]);
        assert_eq!(literals(r"/(\x{30})/"), [None]);
        // A mixed alternation declines whole: one undescribed branch is enough.
        assert_eq!(literals(r"/(a|\d)/"), [None]);
        // Backreferences, in both spellings.
        assert_eq!(literals(r"/(a)(\1)/"), [union(&["a"]), None]);
        assert_eq!(literals(r"/(?<x>a)((?P=x))/"), [union(&["a"]), None]);
    }

    #[test]
    fn case_insensitivity_declines_every_enumeration() {
        // Measured: `preg_match('/(a)/i', 'A', $m)` gives `$m[1] === 'A'` —
        // the pattern's own spelling is not the language. The oracle declines
        // case-insensitive rows wholesale (its `([xXa])/i` expectation is
        // `non-empty-string` where the case-sensitive twin enumerates), so the
        // case product is not attempted either — the other measured decision
        // of issue #177.
        assert_eq!(literals("/(a)/i"), [None]);
        // Even caseless atoms decline under `i`, matching the oracle's
        // `(£|€)(\d+)/i` row.
        assert_eq!(literals("/(£|€)/i"), [None]);
        // Inline spellings, bare and scoped: measured, both capture `'A'`.
        assert_eq!(literals("/(?i)(a)/"), [None]);
        assert_eq!(literals("/(?i:(a))/"), [None]);
        // The floors survive the decline untouched.
        assert_eq!(floors("/(ab)/i"), [2]);
    }

    #[test]
    fn the_enumeration_rides_along_named_groups_and_conditionals() {
        assert_eq!(literals("/(?<named>foo|bar)/"), [union(&["bar", "foo"])]);
        // A conditional's arms keep their own groups' languages — a language
        // is a claim about the entry when the group participates.
        assert_eq!(literals("/(a)?(?(1)(b)|(c))/"), [
            union(&["a"]),
            union(&["b"]),
            union(&["c"])
        ]);
    }
}
