//! Whether a **literal** PCRE pattern consults the C library's character tables (ADR-0101
//! §3.10, issue #1000, slice S5).
//!
//! `pcre_get_compiled_regex_cache` hands PCRE2 the tables `pcre2_maketables()` builds from the
//! process locale once a script has called `setlocale` with `LC_CTYPE` or `LC_ALL`, so a pattern
//! reads the locale cell exactly when PCRE2 asks those tables something. The modifier `u` sets
//! `PCRE2_UTF` **and** `PCRE2_UCP`, and UCP is what routes `\w`, `\s`, `\b`, the POSIX classes
//! and caseless matching to Unicode properties instead of the tables; `(*UTF)` alone does not,
//! so the exemption is UCP and not UTF.
//!
//! [`pattern_reads_locale`] decides lexically, with a byte scan of its own (the group-structure
//! reader above declines on `x`, `n` and every `(*…)` verb, which is exactly where this one has
//! to answer). Outside UCP a pattern reads iff it holds
//!
//! * `\w \W \s \S \b \B` or a POSIX class other than `[:digit:]` and `[:xdigit:]`, which C
//!   fixes in every locale (C11 7.4.1.5);
//! * a **caseless flag** (the modifier `i`, or an `i` that an inline option group sets) and a
//!   byte the flag can fold: an ASCII letter, a byte of `0x80..=0xFF`, a numeric escape or a
//!   back reference. The narrower reading (a high byte only) needs a glibc `tr_TR` witness,
//!   since there the case map of ASCII `I` and `i` is not the C one;
//! * the **`x` flag** and a byte of `0x80..=0xFF`: PCRE2 skips what the table's `isspace` says
//!   in the pattern, and `0xA0` is a space in some locales;
//! * a group or reference name with a byte of `0x80..=0xFF`, whose validity the table decides.
//!
//! Everything else is exempt, and witnessed so: `\d`, `\p{..}`, literal bytes and ranges, `\h`,
//! `\v`, `.`, `\Q..\E` and a `preg_quote`d literal. Under UCP the one table PCRE2 still asks is
//! the `x` flag's whitespace (witnessed: `/^a\xC2\xA0b$/xu` skips the no-break space under
//! `de_DE.UTF-8` and not under `C`), so a pattern with `u` or a leading `(*UCP)` reads iff it is
//! extended and holds a byte of `0x80..=0xFF`.
//!
//! A pattern this reader cannot parse as PCRE2 does (unbalanced, an unknown escape or verb, an
//! unknown modifier) is `None`, never a verdict: the caller treats it as a read that depends on a
//! value the site cannot see.

use super::split_pattern;

/// The `preg_*` functions that compile their first argument (or, for
/// `preg_replace_callback_array`, the keys of it): every one of them reaches the same compiler.
/// `preg_quote`, `preg_last_error` and `preg_last_error_msg` compile nothing.
#[must_use]
pub fn compiles_pattern_argument(name: &str) -> bool {
    [
        "preg_match",
        "preg_match_all",
        "preg_replace",
        "preg_replace_callback",
        "preg_replace_callback_array",
        "preg_filter",
        "preg_split",
        "preg_grep",
    ]
    .iter()
    .any(|f| name.eq_ignore_ascii_case(f))
}

/// Whether compiling and running the full PHP pattern `pattern` (delimiters, expression and
/// modifiers, as passed to `preg_match`) reads the locale tables: `Some(true)` it does,
/// `Some(false)` it does not, `None` the reader cannot parse it as PCRE2 does.
#[must_use]
pub fn pattern_reads_locale(pattern: &str) -> Option<bool> {
    let (body, modifiers) = split_pattern(pattern)?;
    let (mut unicode, mut caseless, mut extended) = (false, false, false);
    for &m in modifiers {
        match m {
            // PHP ignores space, LF and CR between modifiers.
            b' ' | b'\n' | b'\r' | b'm' | b's' | b'A' | b'D' | b'S' | b'U' | b'X' | b'J' | b'n' => {}
            b'u' => unicode = true,
            b'i' => caseless = true,
            b'x' => extended = true,
            _ => return None,
        }
    }
    let src = body.as_bytes();
    let start = leading_options(src);
    if unicode || start.ucp {
        // UCP routes everything but the `x` flag's whitespace away from the tables.
        let high = src.iter().any(|b| *b >= 0x80);
        return Some(high && (extended || sets_inline_flag(src, b'x')));
    }
    let mut scan = Scan { src, pos: start.end, caseless, extended, ..Scan::default() };
    scan.run()?;
    Some(scan.tables || (scan.caseless && scan.foldable) || (scan.extended && scan.high))
}

/// The pattern-start options `(*UTF)(*UCP)(*CRLF)…` before the expression proper.
struct Leading {
    ucp: bool,
    end: usize,
}

/// The options PCRE2 accepts at the start of a pattern, which are not backtracking verbs.
fn is_start_option(name: &[u8]) -> bool {
    matches!(
        name,
        b"UTF8"
            | b"UTF"
            | b"UCP"
            | b"NOTEMPTY"
            | b"NOTEMPTY_ATSTART"
            | b"NO_AUTO_POSSESS"
            | b"NO_DOTSTAR_ANCHOR"
            | b"NO_JIT"
            | b"NO_START_OPT"
            | b"CR"
            | b"LF"
            | b"CRLF"
            | b"ANYCRLF"
            | b"ANY"
            | b"NUL"
            | b"BSR_ANYCRLF"
            | b"BSR_UNICODE"
    ) || name.starts_with(b"LIMIT_")
}

fn leading_options(src: &[u8]) -> Leading {
    let mut out = Leading { ucp: false, end: 0 };
    while src[out.end..].starts_with(b"(*") {
        let rest = &src[out.end + 2..];
        let Some(close) = rest.iter().position(|b| *b == b')') else { break };
        let name = rest[..close].split(|b| *b == b'=').next().unwrap_or_default();
        if !is_start_option(name) {
            break;
        }
        out.ucp |= name == b"UCP";
        out.end += 2 + close + 1;
    }
    out
}

/// Whether some `(?…)` option group of `src` sets the flag `flag`, read loosely (an escaped
/// parenthesis is not told from a group, which can only over-read).
fn sets_inline_flag(src: &[u8], flag: u8) -> bool {
    src.windows(2).enumerate().filter(|(_, w)| w == b"(?").any(|(i, _)| {
        let rest = &src[i + 2..];
        let flags = rest.iter().take_while(|b| b.is_ascii_alphabetic() || matches!(**b, b'^' | b'-'));
        let flags: Vec<u8> = flags.copied().collect();
        matches!(rest.get(flags.len()), Some(b')' | b':'))
            && flags.iter().take_while(|b| **b != b'-').any(|b| *b == flag)
    })
}

/// The tokens of a pattern that decide the verdict, collected by one pass.
#[derive(Default)]
struct Scan<'a> {
    src: &'a [u8],
    pos: usize,
    /// A construct that asks the tables whatever the subject is: `\w`, a POSIX class, a name.
    tables: bool,
    /// A byte a caseless flag can fold: a letter, a high byte, a numeric escape, a reference.
    foldable: bool,
    /// A raw byte of `0x80..=0xFF` (or an escape spelling one), which the `x` flag may skip.
    high: bool,
    caseless: bool,
    extended: bool,
}

const POSIX_READING: &[&[u8]] = &[
    b"alnum", b"alpha", b"ascii", b"blank", b"cntrl", b"graph", b"lower", b"print", b"punct",
    b"space", b"upper", b"word",
];
const POSIX_FIXED: &[&[u8]] = &[b"digit", b"xdigit"];

/// The alphabetic spellings of a group that holds more pattern (PCRE2 10.34).
const ALPHA_GROUPS: &[&[u8]] = &[
    b"pla",
    b"plb",
    b"nla",
    b"nlb",
    b"napla",
    b"naplb",
    b"atomic",
    b"sr",
    b"asr",
    b"positive_lookahead",
    b"positive_lookbehind",
    b"negative_lookahead",
    b"negative_lookbehind",
    b"non_atomic_positive_lookahead",
    b"non_atomic_positive_lookbehind",
    b"script_run",
    b"atomic_script_run",
];

const BACKTRACKING_VERBS: &[&[u8]] =
    &[b"ACCEPT", b"FAIL", b"F", b"COMMIT", b"PRUNE", b"SKIP", b"THEN"];

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn eat(&mut self, b: u8) -> bool {
        let hit = self.peek() == Some(b);
        self.pos += usize::from(hit);
        hit
    }

    fn run(&mut self) -> Option<()> {
        while let Some(b) = self.next() {
            match b {
                b'\\' => self.escape(false)?,
                b'[' => self.class()?,
                b'(' => self.group()?,
                _ => self.literal(b),
            }
        }
        Some(())
    }

    /// A byte that stands for itself.
    fn literal(&mut self, b: u8) {
        if b.is_ascii_alphabetic() {
            self.foldable = true;
        } else if b >= 0x80 {
            self.foldable = true;
            self.high = true;
        }
    }

    /// A character spelled by a numeric escape: foldable when it is a letter or above ASCII.
    fn spelled(&mut self, value: u32) {
        if value >= 0x80 {
            self.high = true;
        }
        if value >= 0x80 || u8::try_from(value).is_ok_and(|b| b.is_ascii_alphabetic()) {
            self.foldable = true;
        }
    }

    /// The bytes up to `close`, consumed with it: a name, whose validity the table decides when
    /// a byte is above ASCII.
    fn name(&mut self, close: u8) -> Option<()> {
        loop {
            let b = self.next()?;
            if b == close {
                return Some(());
            }
            self.tables |= b >= 0x80;
        }
    }

    /// The body of `\Q…\E`, to the `\E` or the end: literal, so only a caseless flag reads it.
    fn quoted(&mut self) {
        while let Some(b) = self.next() {
            if b == b'\\' && self.peek() == Some(b'E') {
                self.pos += 1;
                return;
            }
            self.foldable |= b.is_ascii_alphabetic() || b >= 0x80;
        }
    }

    /// The digits of `radix` that follow, up to `max` of them, as a number.
    fn digits(&mut self, radix: u32, max: usize) -> u32 {
        let mut value = 0u32;
        for _ in 0..max {
            let Some(d) = self.peek().and_then(|b| char::from(b).to_digit(radix)) else { break };
            value = value.saturating_mul(radix).saturating_add(d);
            self.pos += 1;
        }
        value
    }

    /// `\` and what follows it, the backslash already read.
    fn escape(&mut self, in_class: bool) -> Option<()> {
        let c = self.next()?;
        match c {
            b'w' | b'W' | b's' | b'S' | b'B' => self.tables = true,
            b'b' => self.tables |= !in_class,
            b'Q' => self.quoted(),
            b'p' | b'P' => {
                if self.eat(b'{') {
                    self.name(b'}')?;
                } else {
                    self.next()?;
                }
            }
            b'c' => {
                self.next()?;
            }
            b'x' => {
                let value = if self.eat(b'{') {
                    let value = self.digits(16, 8);
                    self.eat(b'}').then_some(value)?
                } else {
                    self.digits(16, 2)
                };
                self.spelled(value);
            }
            b'o' => {
                self.eat(b'{').then_some(())?;
                let value = self.digits(8, 11);
                self.eat(b'}').then_some(())?;
                self.spelled(value);
            }
            b'0' => {
                self.digits(8, 2);
            }
            b'1'..=b'9' => {
                // A back reference, or an octal escape when it names more groups than exist.
                self.pos -= 1;
                let at = self.pos;
                let octal = self.digits(8, 3);
                self.pos = at;
                self.digits(10, usize::MAX);
                self.foldable = true;
                self.high |= octal >= 0x80;
            }
            b'g' | b'k' => self.reference()?,
            b'd' | b'D' | b'h' | b'H' | b'v' | b'V' | b'R' | b'N' | b'X' | b'K' | b'G' | b'A'
            | b'Z' | b'z' | b'C' | b'a' | b'e' | b'f' | b'n' | b'r' | b't' | b'E' => {}
            // PCRE2 refuses every other letter (`\y`, `\l`, `\U`, …).
            b if b.is_ascii_alphabetic() => return None,
            b => self.high |= b >= 0x80,
        }
        self.foldable |= c >= 0x80;
        Some(())
    }

    /// `\g…` and `\k…`: a numbered or named reference, which a caseless flag compares folded.
    fn reference(&mut self) -> Option<()> {
        self.foldable = true;
        match self.peek()? {
            b'{' => self.name(b'}'),
            b'<' => self.name(b'>'),
            b'\'' => self.name(b'\''),
            _ => {
                self.eat(b'-');
                self.eat(b'+');
                self.digits(10, usize::MAX);
                Some(())
            }
        }
    }

    /// A bracket class, the `[` already read.
    fn class(&mut self) -> Option<()> {
        self.eat(b'^');
        let mut first = true;
        loop {
            let b = self.next()?;
            match b {
                b']' if !first => return Some(()),
                b'\\' => self.escape(true)?,
                b'[' => self.class_bracket()?,
                _ => self.literal(b),
            }
            first = false;
        }
    }

    /// A `[` inside a class: the start of a POSIX class, or a literal.
    fn class_bracket(&mut self) -> Option<()> {
        let Some(&kind) = self.src.get(self.pos).filter(|b| matches!(**b, b':' | b'.' | b'=')) else {
            return Some(());
        };
        let Some(end) = posix_syntax_end(&self.src[self.pos + 1..], kind) else { return Some(()) };
        // PCRE2 refuses collating elements and equivalence classes.
        (kind == b':').then_some(())?;
        let name = &self.src[self.pos + 1..self.pos + 1 + end];
        let name = name.strip_prefix(b"^").unwrap_or(name);
        if POSIX_READING.contains(&name) {
            self.tables = true;
        } else if !POSIX_FIXED.contains(&name) {
            return None;
        }
        self.pos += end + 3;
        Some(())
    }

    /// A group, the `(` already read.
    fn group(&mut self) -> Option<()> {
        if self.eat(b'*') {
            return self.verb();
        }
        if !self.eat(b'?') {
            return Some(());
        }
        match self.peek()? {
            b'#' => self.name(b')'),
            b':' | b'=' | b'!' | b'>' | b'|' => {
                self.pos += 1;
                Some(())
            }
            b'<' => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'=' | b'!')) {
                    self.pos += 1;
                    return Some(());
                }
                self.name(b'>')
            }
            b'P' => {
                self.pos += 1;
                match self.next()? {
                    b'<' => self.name(b'>'),
                    b'=' | b'>' => self.name(b')'),
                    _ => None,
                }
            }
            b'\'' => {
                self.pos += 1;
                self.name(b'\'')
            }
            b'&' | b'R' | b'C' | b'0'..=b'9' | b'+' => self.name(b')'),
            b'(' => self.condition(),
            b'-' if self.src.get(self.pos + 1).is_some_and(u8::is_ascii_digit) => self.name(b')'),
            _ => self.inline_flags(),
        }
    }

    /// `(?(`: a lookaround condition leaves its own group to the main loop; any other condition
    /// is a name or a number.
    fn condition(&mut self) -> Option<()> {
        if matches!(self.src.get(self.pos + 1), Some(b'?' | b'*')) {
            return Some(());
        }
        self.pos += 1;
        self.name(b')')
    }

    /// `(?imsxnJU-^…)` and `(?…:`: the flags an option group sets.
    fn inline_flags(&mut self) -> Option<()> {
        let mut negated = false;
        loop {
            match self.next()? {
                b')' | b':' => return Some(()),
                b'-' => negated = true,
                b'^' => {}
                b'i' => self.caseless |= !negated,
                b'x' => self.extended |= !negated,
                b'm' | b's' | b'n' | b'J' | b'U' | b'a' | b'D' | b'S' | b'W' | b'P' | b'T' => {}
                _ => return None,
            }
        }
    }

    /// `(*NAME)`, `(*NAME:arg)` and the alphabetic assertions, the `(*` already read.
    fn verb(&mut self) -> Option<()> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_') {
            self.pos += 1;
        }
        let name = &self.src[start..self.pos];
        if self.eat(b':') {
            return if ALPHA_GROUPS.contains(&name) { Some(()) } else { self.name(b')') };
        }
        (self.eat(b')') && BACKTRACKING_VERBS.contains(&name)).then_some(())
    }
}

/// PCRE2's `check_posix_syntax`: the length of the name when `rest` (the bytes after `[:`) holds
/// `name:]` with no `]` before it, else `None`, and the `[` is a literal.
fn posix_syntax_end(rest: &[u8], terminator: u8) -> Option<usize> {
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            b'\\' if matches!(rest.get(i + 1), Some(b']' | b'\\')) => i += 1,
            b'[' if rest.get(i + 1) == Some(&terminator) => return None,
            b']' => return None,
            c if c == terminator && rest.get(i + 1) == Some(&b']') => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{compiles_pattern_argument, pattern_reads_locale};

    fn reads(pattern: &str) -> Option<bool> {
        pattern_reads_locale(pattern)
    }

    /// The rows of the S5 witness table that keep the read: the engine moved between `C` and a
    /// locale on each (`s5-preg.php`, `s5-preg-2.php`, PHP 8.5.11 and 8.1.32).
    #[test]
    fn the_witnessed_readers_read() {
        for (row, pattern) in [
            ("P1 \\w", r"/^\w$/"),
            ("P3 \\W", r"/^\W$/"),
            ("P6 \\b", r"/\bx/"),
            ("S25 \\B", r"/a\B/"),
            ("S4 \\s", r"/^\s$/"),
            ("P5 \\s", r"/^\s$/"),
            ("P7 [:alpha:]", "/^[[:alpha:]]$/"),
            ("P8 [:space:]", "/^[[:space:]]$/"),
            ("S16 [:upper:]", "/^[[:upper:]]$/"),
            ("S6 [:blank:]", "/^[[:blank:]]$/"),
            ("S17 [:punct:]", "/^[[:punct:]]$/"),
            ("P9 /i over a high byte", r"/^\xC4$/i"),
            ("P22 (?i) over a high byte", r"/^(?i)\xC4$/"),
            ("S14 (?i:..)", r"/^(?i:\xC4)$/"),
            ("S26 a range under /i", "/^[\\xC0-\\xDE]$/i"),
            ("S10 /x with a high byte", "/^a\u{a0}b$/x"),
            ("P23 (*UTF) is not UCP", r"/(*UTF)^\w$/"),
            ("P18 preg_replace", r"/\w/"),
            ("P19 preg_split", r"/\W/"),
            ("P28 preg_grep", r"/^\w$/"),
            ("S21 preg_replace_callback_array", r"/\w/"),
            ("S22 preg_filter", r"/\w/"),
        ] {
            assert_eq!(reads(pattern), Some(true), "{row}: {pattern}");
        }
    }

    /// `i` over a letter reads by rule (D-S5a), including the rows whose witness stood still on
    /// this machine: ASCII only, where a glibc `tr_TR` case map moves `I` and `i`.
    #[test]
    fn a_caseless_flag_over_a_letter_reads_by_rule() {
        for (row, pattern) in [
            ("P10", "/^A$/i"),
            ("P14", "/^[a-z]$/i"),
            ("S1", "/^i$/i"),
            ("S2", "/^I$/i"),
            ("S3", "/^[a-z]$/i"),
            ("S23", "/^ABC$/i"),
            ("S9 UTF-8 bytes without u", "/^\u{c4}\u{84}$/i"),
            ("an inline flag", "/a(?i)b/"),
            ("an inline group", "/a(?i:b)c/"),
            ("a set-and-clear group", "/(?i-s)b/"),
            ("a \\Q..\\E run", r"/\Qabc\E/i"),
            ("a hex escape of a letter", r"/\x41/i"),
            ("a back reference", r"/(\d)\1/i"),
        ] {
            assert_eq!(reads(pattern), Some(true), "{row}: {pattern}");
        }
    }

    /// What the table leaves out: no `u` witness stays still, and neither does a pattern with
    /// nothing a table could answer.
    #[test]
    fn the_witnessed_exemptions_read_nothing() {
        for (row, pattern) in [
            ("P2 \\w with u", r"/^\w$/u"),
            ("P20 /iu", "/^\u{c3}\u{84}$/iu"),
            ("S8 /iu", "/^\u{c4}$/iu"),
            ("P21 [:alpha:] with u", "/^[[:alpha:]]$/u"),
            ("S24 (*UCP) and /i", r"/(*UCP)^\xC4$/i"),
            ("P24 (*UCP) and \\w", r"/(*UCP)^\w$/"),
            ("(*UTF)(*UCP) together", r"/(*UTF)(*UCP)^\w$/"),
            ("(*UCP) after another option", r"/(*CRLF)(*UCP)\s/"),
            ("P4 \\d", r"/^\d$/"),
            ("P16 \\d", r"/^\d$/"),
            ("S20 \\d", r"/^\d$/"),
            ("S18 [:digit:]", "/^[[:digit:]]$/"),
            ("S19 [:xdigit:]", "/^[[:xdigit:]]$/"),
            ("a negated [:digit:]", "/^[[:^digit:]]$/"),
            ("P11 a literal byte", "/^\u{e4}$/"),
            ("P12 a byte range", "/^[\u{e0}-\u{ef}]$/"),
            ("S15 \\Q..\\E", "/^\\Q\u{e4}\\E$/"),
            ("P13 [a-z]", "/^[a-z]$/"),
            ("S12 \\p{L}", r"/^\p{L}$/"),
            ("S13 \\p{L} with u", r"/^\p{L}$/u"),
            ("a short property", r"/^\pL$/"),
            ("P25 \\h", r"/^\h$/"),
            ("P26 \\v", r"/^\v$/"),
            ("P17 .", "/^.$/"),
            ("S11 /x over ASCII", "/^a b$/x"),
            ("\\b inside a class is a backspace", r"/[\b]/"),
            ("i with no letter", r"/^\d+[0-9._-]*$/i"),
            ("a named group under /i", r"/(?<year>\d+)/i"),
            ("a POSIX name is no letter", r"/[[:digit:]]/i"),
            ("a \\Q..\\E run without /i", r"/\Qa\w\E/"),
            ("a comment group", r"/a(?#a \w comment)b/"),
        ] {
            assert_eq!(reads(pattern), Some(false), "{row}: {pattern}");
        }
    }

    /// `x` asks the table whether a byte is whitespace, under UCP as well: the witnessed
    /// `/^a\xC2\xA0b$/xu` skips the no-break space under `de_DE.UTF-8` and not under `C`.
    #[test]
    fn the_x_flag_reads_a_high_byte_even_under_ucp() {
        assert_eq!(reads("/^a\u{a0}b$/xu"), Some(true));
        assert_eq!(reads("/(*UCP)^a\u{a0}b$/x"), Some(true));
        assert_eq!(reads("/(?x)^a\u{a0}b$/u"), Some(true));
        assert_eq!(reads("/^a\u{a0}b$/u"), Some(false), "without x the byte is a character");
        assert_eq!(reads("/^a b$/xu"), Some(false));
        assert_eq!(reads("/^a\u{a0}b$/x"), Some(true));
        assert_eq!(reads(r"/^a\xA0b$/x"), Some(true), "an escape is counted too");
        assert_eq!(reads("/^\\Q\u{a0}\\E$/x"), Some(false), "a quoted byte is not skipped");
        assert_eq!(reads("/(?-x)^a\u{a0}b$/"), Some(false));
    }

    /// The tokens that read, in every position they can stand.
    #[test]
    fn a_reading_token_reads_in_a_class_a_group_and_after_a_verb() {
        for pattern in [
            r"/[\w-]/",
            r"/[^\s]/",
            r"/[a\W]/",
            r"/(?:\s+|x)/",
            r"/(?<name>\w)/",
            r"/(?=\w)/",
            r"/(?<!\s)a/",
            r"/(*SKIP)\w/",
            r"/(*MARK:x)\bfoo/",
            r"/(*pla:\w)/",
            r"/(?(?=\w)a|b)/",
            r"/(?(1)\w|b)(a)/",
            "/[[:word:]]+/",
            "/[[:^alpha:]]/",
            "/[]\\w]/",
            "{\\w+}",
            "#a\\sb#",
            "(\\w(a)(b))",
        ] {
            assert_eq!(reads(pattern), Some(true), "{pattern}");
        }
    }

    /// An escaped delimiter and an escaped metacharacter are literals, and a `\Q` that is never
    /// closed runs to the end.
    #[test]
    fn escapes_and_quotes_are_literals() {
        assert_eq!(reads(r"/a\/b\.c\\d/"), Some(false));
        assert_eq!(reads(r"/\\w/"), Some(false), "an escaped backslash then a letter");
        assert_eq!(reads(r"/\Q\w"), None, "the delimiter is inside the quote");
        // PHP finds the closing delimiter first, so the quote holds `\w` and reads nothing.
        assert_eq!(reads(r"/\Q\w/"), Some(false));
        assert_eq!(reads(r"/\Qa.b\E\w/"), Some(true));
        assert_eq!(reads(r"/[\Q]\E\w]/"), Some(true));
    }

    /// A pattern the reader cannot parse as PCRE2 does is no verdict.
    #[test]
    fn what_the_parser_declines_is_none() {
        for pattern in [
            "",
            "abc",
            "/abc",
            "/[abc/",
            r"/a\y/",
            r"/\l/",
            r"/\p/",
            r"/a(*FOO)b/",
            r"/a(*MARK)b/",
            r"/(?z)a/",
            "/[[:nope:]]/",
            "/[[.a.]]/",
            "/[[=a=]]/",
            "/a/e",
            "/a/ug",
            r"/\x{/",
            r"/\o41/",
            "/(?P<n/",
        ] {
            assert_eq!(reads(pattern), None, "{pattern:?}");
        }
        // The `u` modifier and a leading `(*UCP)` answer without parsing the body.
        assert_eq!(reads("/[abc/u"), Some(false));
        assert_eq!(reads(r"/(*UCP)a\y/"), Some(false));
    }

    /// Delimiters, leading whitespace, and modifiers that are no letter.
    #[test]
    fn delimiters_and_modifiers_are_read_as_php_reads_them() {
        assert_eq!(reads("  /^\\w$/"), Some(true));
        assert_eq!(reads("~^\\d~"), Some(false));
        assert_eq!(reads("#^a#msxADSUXJn"), Some(false));
        assert_eq!(reads("/a/ i"), Some(true), "space between modifiers is ignored");
        assert_eq!(reads("/a/\ti"), None, "a tab is a modifier PHP refuses");
        assert_eq!(reads("(^\\w$)"), Some(true));
        assert_eq!(reads("[a\\]b]"), Some(false));
        assert_eq!(reads("/(?i)\\d/"), Some(false));
    }

    /// An `i` is only a flag where it sets one: a group name, a verb argument and a comment
    /// spelling it are not.
    #[test]
    fn an_i_that_sets_nothing_is_not_a_caseless_flag() {
        assert_eq!(reads(r"/(?<if>\d)/"), Some(false));
        assert_eq!(reads(r"/(?-i)a/"), Some(false), "`a` is a letter but the flag is only cleared");
        assert_eq!(reads(r"/(*MARK:i)\d/"), Some(false));
        assert_eq!(reads(r"/(?#i)\d/"), Some(false));
    }

    #[test]
    fn the_compiling_functions_are_the_eight() {
        for name in [
            "preg_match",
            "PREG_MATCH_ALL",
            "preg_replace",
            "preg_replace_callback",
            "preg_replace_callback_array",
            "preg_filter",
            "preg_split",
            "preg_grep",
        ] {
            assert!(compiles_pattern_argument(name), "{name}");
        }
        for name in ["preg_quote", "preg_last_error", "preg_last_error_msg", "preg_match_x", "mb_ereg"]
        {
            assert!(!compiles_pattern_argument(name), "{name}");
        }
    }
}
