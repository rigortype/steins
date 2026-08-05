//! The capture-group structure of a **literal** PCRE pattern (issue #149,
//! slice A of #148).
//!
//! [`capture_groups`] turns a PHP pattern string — delimiters, expression, and
//! trailing modifiers — into an ordered description of the groups it captures.
//! It is deliberately **not** a PCRE implementation. It answers only for
//! patterns whose group structure it can establish with certainty, and for
//! everything else it answers `None`.
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
//!
//! # Not in this module
//!
//! The trust gate (is the pattern a proven literal?), the out-parameter seed,
//! and the modifier-dependent entry shape (`PREG_OFFSET_CAPTURE`,
//! `PREG_UNMATCHED_AS_NULL`) all belong to later slices. This module knows only
//! about the pattern string.

/// The capture groups of a pattern, in numeric order.
///
/// Index `0` of [`groups`](CaptureGroups::groups) is **group 1**. The
/// whole-match entry `$matches[0]` is not a capture group and is not listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureGroups {
    /// Groups in numeric order, index 0 being group 1 (the whole-match entry
    /// is not a group and is not listed).
    pub groups: Vec<CaptureGroup>,
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
    let body = split_delimited(pattern)?;
    let mut parser = Parser {
        src: body.as_bytes(),
        pos: 0,
        groups: Vec::new(),
    };
    parser.alternation()?;
    if parser.pos != parser.src.len() {
        // The scan stopped on a `)` with no group open: unbalanced.
        return None;
    }
    let raw = parser.take_groups()?;
    Some(CaptureGroups {
        groups: apply_trailing_absence(raw),
    })
}

/// Apply the trailing-absence rule to groups already tagged with whether they
/// can go unmatched.
///
/// Walking backwards: a group is trailing-absent when it can go unmatched and
/// nothing after it is guaranteed to participate. The first group that cannot
/// go unmatched shuts the window for everything before it, because its own
/// entry is always populated.
fn apply_trailing_absence(raw: Vec<RawGroup>) -> Vec<CaptureGroup> {
    let mut out: Vec<CaptureGroup> = Vec::with_capacity(raw.len());
    let mut guaranteed_after = false;
    for g in raw.into_iter().rev() {
        out.push(CaptureGroup {
            name: g.name,
            can_be_trailing_absent: g.can_go_unmatched && !guaranteed_after,
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

/// Strip the delimiters and modifiers, yielding the bare expression.
///
/// Mirrors what PHP itself does before handing the pattern to PCRE: skip
/// leading whitespace, take the first character as the delimiter, and — for the
/// four bracket pairs — track nesting to find its partner. Everything after the
/// closing delimiter is modifiers.
///
/// Measured: `'{a{b}'` fails to compile because the nesting scan never returns
/// to depth zero, while `'(((a)))'` yields the two-group expression `((a))`.
fn split_delimited(pattern: &str) -> Option<&str> {
    let bytes = pattern.as_bytes();
    let mut start = 0;
    while start < bytes.len() && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    let delim = *bytes.get(start)?;
    if delim.is_ascii_alphanumeric() || delim == b'\\' {
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

    for &m in &bytes[end + 1..] {
        // PHP ignores space, LF, and CR between modifiers — but not tab.
        if m == b' ' || m == b'\n' || m == b'\r' {
            continue;
        }
        if !NUMBERING_NEUTRAL_MODIFIERS.contains(&m) {
            return None;
        }
    }

    // The scan is byte-wise, but it only ever stops on ASCII, and UTF-8 never
    // encodes an ASCII byte inside a multi-byte sequence, so both cuts land on
    // character boundaries.
    pattern.get(start + 1..end)
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
    fn alternation(&mut self) -> Option<()> {
        let start = self.groups.len();
        let mut branches = 1usize;
        loop {
            self.branch()?;
            if self.peek() == Some(b'|') {
                self.pos += 1;
                branches += 1;
                continue;
            }
            break;
        }
        if branches > 1 {
            self.mark_optional_from(start);
        }
        Some(())
    }

    /// One alternation branch: atoms until `|`, `)`, or end of input.
    fn branch(&mut self) -> Option<()> {
        while let Some(c) = self.peek() {
            if c == b'|' || c == b')' {
                break;
            }
            self.atom()?;
        }
        Some(())
    }

    /// One quantified atom.
    fn atom(&mut self) -> Option<()> {
        let start = self.groups.len();
        match self.peek()? {
            b'(' => self.open_paren()?,
            b'[' => self.char_class()?,
            b'\\' => self.escape()?,
            _ => self.pos += 1,
        }
        if self.quantifier_min_is_zero() {
            self.mark_optional_from(start);
        }
        Some(())
    }

    /// Dispatch on what follows a `(`.
    fn open_paren(&mut self) -> Option<()> {
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
            self.groups.push(RawGroup {
                name: None,
                can_go_unmatched: false,
            });
            self.alternation()?;
            return self.close_group();
        }

        // Order matters: `(?<=` and `(?<!` must be tested before `(?<name>`,
        // and `(?P<name>` before the inline-flag fallthrough.
        if self.eat(b"(?:") || self.eat(b"(?>") || self.eat(b"(?=") || self.eat(b"(?<=") {
            // Non-capturing, and everything inside a *positive* lookaround
            // participates whenever the match succeeds.
            self.alternation()?;
            return self.close_group();
        }
        if self.eat(b"(?!") || self.eat(b"(?<!") {
            // A negative lookaround succeeds only when its body did NOT match,
            // so a group inside it is never populated. Measured:
            // `preg_match('/(a)(?!(b))/', 'a', $m)` leaves group 2 absent.
            let start = self.groups.len();
            self.alternation()?;
            self.close_group()?;
            self.mark_optional_from(start);
            return Some(());
        }
        if self.eat(b"(?#") {
            // A comment runs to the first `)`. A backslash does not escape
            // inside it: `'/(?#a\)b)/'` fails to compile.
            return self.skip_to_close_paren();
        }
        if self.rest().starts_with(b"(?P=") {
            // A named backreference, not a group.
            self.pos += 4;
            return self.skip_to_close_paren();
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
    fn named_group(&mut self, terminator: u8) -> Option<()> {
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
        self.groups.push(RawGroup {
            name: Some(name),
            can_go_unmatched: false,
        });
        self.alternation()?;
        self.close_group()
    }

    /// `(?(cond)yes|no)` — the cursor sits just past `(?(`.
    ///
    /// Only the number and name conditions are modelled. An assertion condition
    /// (`(?(?=…)`) hides a whole sub-pattern, `(?(R…)` is a recursion test, and
    /// `(?(DEFINE)…)` holds groups that occupy indices yet never participate;
    /// all three decline.
    fn conditional(&mut self) -> Option<()> {
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
        // go unmatched.
        let start = self.groups.len();
        self.alternation()?;
        self.close_group()?;
        self.mark_optional_from(start);
        Some(())
    }

    /// `(?flags)` and `(?flags:…)` — the cursor sits on the `(`.
    fn inline_flags(&mut self) -> Option<()> {
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
        match self.src.get(i)? {
            // `(?i)` — a bare setting, spanning the rest of the enclosing
            // group. It opens nothing, so there is no body and no `)` to match
            // beyond this one.
            b')' => {
                self.pos = i + 1;
                Some(())
            }
            // `(?i:…)` — a scoped setting on a non-capturing group.
            b':' => {
                self.pos = i + 1;
                self.alternation()?;
                self.close_group()
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
    /// `\Q…\E` is the one that matters: it makes every enclosed character
    /// literal, so `'/\Q(a)\E(b)/'` has exactly one group. Every other escape
    /// consumes its backslash and one following byte, which is enough — no
    /// PCRE escape can introduce an unescaped `(`, `)`, or `[`.
    fn escape(&mut self) -> Option<()> {
        debug_assert_eq!(self.peek(), Some(b'\\'));
        if self.eat(b"\\Q") {
            self.skip_quoted();
            return Some(());
        }
        self.pos += 1;
        self.src.get(self.pos)?;
        self.pos += 1;
        Some(())
    }

    /// Skip a `\Q` literal run, which ends at `\E` or at the end of input.
    fn skip_quoted(&mut self) {
        while self.pos < self.src.len() {
            if self.rest().starts_with(b"\\E") {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }

    /// A bracketed character class. Nothing inside one is a group.
    ///
    /// Finding the real terminator is the whole job, and three rules do it, all
    /// measured: a `]` in first position (after an optional `^`) is a literal,
    /// `\Q…\E` hides a `]`, and a POSIX class `[:alpha:]` carries a `]` of its
    /// own — `'/[[:alpha:](]+(b)/'` has one group, not two.
    fn char_class(&mut self) -> Option<()> {
        debug_assert_eq!(self.peek(), Some(b'['));
        self.pos += 1;
        self.eat(b"^");
        self.eat(b"]");
        loop {
            match self.peek()? {
                b']' => {
                    self.pos += 1;
                    return Some(());
                }
                b'\\' => {
                    if self.eat(b"\\Q") {
                        self.skip_quoted();
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

    /// Read the quantifier after an atom, answering whether its minimum is
    /// zero — that is, whether the atom can be skipped entirely.
    ///
    /// A `{…}` that is not a well-formed quantifier is a literal brace, which
    /// PCRE accepts: `'/(a)b{x}(c)/'` matches the text `b{x}` and has two
    /// groups.
    fn quantifier_min_is_zero(&mut self) -> bool {
        let min_zero = match self.peek() {
            Some(b'?' | b'*') => {
                self.pos += 1;
                true
            }
            Some(b'+') => {
                self.pos += 1;
                false
            }
            Some(b'{') => match self.braced_quantifier() {
                Some(min_zero) => min_zero,
                None => return false,
            },
            _ => return false,
        };
        // A trailing `?` (lazy) or `+` (possessive) changes the search order,
        // never whether the atom may be skipped.
        if matches!(self.peek(), Some(b'?' | b'+')) {
            self.pos += 1;
        }
        min_zero
    }

    /// `{n}`, `{n,}`, `{n,m}`, `{,m}` — consumed only if well formed. Answers
    /// whether the minimum is zero.
    fn braced_quantifier(&mut self) -> Option<bool> {
        let mut i = self.pos + 1;
        let digits_start = i;
        while self.src.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let min_digits = &self.src[digits_start..i];
        let has_comma = self.src.get(i) == Some(&b',');
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
        self.pos = i + 1;
        Some(min_digits.iter().all(|d| *d == b'0'))
    }
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
}
