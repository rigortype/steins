//! The byte scan behind [`pattern_reads_locale`](super::pattern_reads_locale): one pass over a
//! PCRE2 expression that collects the tokens deciding the verdict, and declines (`None`) on
//! anything PCRE2 would refuse.

/// The tokens of a pattern that decide the verdict, collected by one pass.
#[derive(Default)]
pub(super) struct Scan<'a> {
    pub(super) src: &'a [u8],
    pub(super) pos: usize,
    /// A construct that asks the tables whatever the subject is outside UCP: `\w`, `\s`, `\b`, a
    /// POSIX class, `[[:<:]]`.
    pub(super) tables: bool,
    /// `[:ascii:]` (or its negation), which stays table-based under UCP as well.
    pub(super) ascii_class: bool,
    /// A group or reference name with a byte of `0x80..=0xFF`, which the table validates unless
    /// the pattern is UTF.
    pub(super) names: bool,
    /// The pattern can match a letter, so a caseless flag compares through the table: a literal
    /// letter or high byte, a range spanning a letter, `.`, a negated class, a class escape or
    /// POSIX class that holds letters, a numeric escape or a back reference.
    pub(super) foldable: bool,
    /// A raw byte of `0x80..=0xFF` (or an escape spelling one), which the `x` flag may skip.
    pub(super) high: bool,
    /// A caseless flag is set somewhere.
    pub(super) caseless: bool,
    /// The `x` flag is set somewhere.
    pub(super) extended: bool,
    /// The `x` flag is in force at `pos`: a `#` outside a class starts a comment.
    pub(super) x_now: bool,
    /// The `x` flag at each enclosing group's opening, restored at its `)`.
    pub(super) stack: Vec<bool>,
    /// The newline convention, which decides where an `x` comment ends.
    pub(super) newline: Newline,
}

/// The newline conventions a pattern-start option selects (`(*CR)`, `(*LF)`, `(*CRLF)`,
/// `(*ANYCRLF)`, `(*ANY)`, `(*NUL)`); the default is LF.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Newline {
    #[default]
    Lf,
    Cr,
    Crlf,
    AnyCrlf,
    Nul,
    /// LF, VT, FF, CR, NEL and the Unicode separators, which differ by UTF mode: not modelled.
    Any,
}

const POSIX_NAMES: &[&[u8]] = &[
    b"alnum", b"alpha", b"ascii", b"blank", b"cntrl", b"digit", b"graph", b"lower", b"print",
    b"punct", b"space", b"upper", b"word", b"xdigit",
];
/// The POSIX classes whose members hold no letter, so a caseless flag has nothing to fold in them.
const POSIX_NO_LETTERS: &[&[u8]] = &[b"blank", b"cntrl", b"digit", b"punct", b"space"];
/// The sets C fixes in every locale (C11 7.4.1.5 and 7.4.1.12).
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

/// Whether the character range `lo..=hi` holds a letter, or a byte above ASCII.
fn range_has_letter(lo: u32, hi: u32) -> bool {
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    hi >= 0x80 || (lo <= 0x5A && hi >= 0x41) || (lo <= 0x7A && hi >= 0x61)
}

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

    /// Scan the whole expression; every group opened must be closed.
    pub(super) fn run(&mut self) -> Option<()> {
        while let Some(b) = self.next() {
            match b {
                b'\\' => {
                    self.escape(false)?;
                }
                b'[' => self.bracket()?,
                b'(' => self.group()?,
                b')' => self.x_now = self.stack.pop()?,
                b'#' if self.x_now => self.comment()?,
                b'.' => self.foldable = true,
                _ => self.literal(b),
            }
        }
        self.stack.is_empty().then_some(())
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

    /// The bytes up to `close`, consumed with it: a name, whose validity the table decides when a
    /// byte is above ASCII.
    fn name(&mut self, close: u8) -> Option<()> {
        loop {
            let b = self.next()?;
            if b == close {
                return Some(());
            }
            self.names |= b >= 0x80;
        }
    }

    /// An `x`-mode comment, from the `#` already read to the end of its line, which is where the
    /// newline convention says (LF by default: a CR, VT, FF or NUL inside it is comment text).
    /// What the comment holds is not scanned, so it must end exactly where PCRE2 ends it: an
    /// early end would scan a `\Q` or `(?#` the comment still holds and let it hide the pattern
    /// after the newline. `(*ANY)` is not modelled and declines.
    fn comment(&mut self) -> Option<()> {
        while let Some(b) = self.next() {
            let ends = match self.newline {
                Newline::Lf => b == b'\n',
                Newline::Cr => b == b'\r',
                Newline::Nul => b == 0,
                Newline::AnyCrlf => b == b'\r' || b == b'\n',
                Newline::Crlf => b == b'\r' && self.peek() == Some(b'\n'),
                Newline::Any => return None,
            };
            if ends {
                if self.newline == Newline::Crlf || (self.newline == Newline::AnyCrlf && b == b'\r') {
                    self.eat(b'\n');
                }
                return Some(());
            }
        }
        Some(())
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

    /// `\` and what follows it, the backslash already read: the character it spells, when it
    /// spells exactly one (the endpoint of a range in a class).
    fn escape(&mut self, in_class: bool) -> Option<Option<u32>> {
        let c = self.next()?;
        let mut value = None;
        match c {
            b'w' | b'W' | b'S' => {
                self.tables = true;
                self.foldable = true;
            }
            b's' | b'B' => self.tables = true,
            b'b' if in_class => value = Some(8),
            b'b' => self.tables = true,
            b'Q' => self.quoted(),
            b'p' | b'P' => {
                self.foldable = true;
                if self.eat(b'{') {
                    self.name(b'}')?;
                } else {
                    self.next()?;
                }
            }
            b'c' => {
                self.next()?;
                value = Some(0);
            }
            b'x' => {
                let v = if self.eat(b'{') {
                    let v = self.digits(16, 8);
                    self.eat(b'}').then_some(v)?
                } else {
                    self.digits(16, 2)
                };
                self.spelled(v);
                value = Some(v);
            }
            b'o' => {
                self.eat(b'{').then_some(())?;
                let v = self.digits(8, 11);
                self.eat(b'}').then_some(())?;
                self.spelled(v);
                value = Some(v);
            }
            b'0' => value = Some(self.digits(8, 2)),
            b'1'..=b'9' if in_class => {
                // No back references in a class: an octal escape.
                self.pos -= 1;
                let v = self.digits(8, 3);
                self.spelled(v);
                value = Some(v);
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
            b'D' | b'H' | b'V' | b'N' | b'X' | b'C' => self.foldable = true,
            b'a' => value = Some(7),
            b'e' => value = Some(27),
            b'f' => value = Some(12),
            b'n' => value = Some(10),
            b'r' => value = Some(13),
            b't' => value = Some(9),
            b'd' | b'h' | b'v' | b'R' | b'K' | b'G' | b'A' | b'Z' | b'z' | b'E' => {}
            // PCRE2 refuses every other letter (`\y`, `\l`, `\U`, …).
            b if b.is_ascii_alphabetic() => return None,
            b => {
                self.high |= b >= 0x80;
                value = Some(u32::from(b));
            }
        }
        self.foldable |= c >= 0x80;
        Some(value)
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

    /// A `[`: the whole-class word boundaries `[[:<:]]` and `[[:>:]]` (PCRE2 rewrites them to
    /// `\b(?=\w)` and `\b(?<=\w)`), or a bracket class.
    fn bracket(&mut self) -> Option<()> {
        let rest = &self.src[self.pos..];
        if rest.starts_with(b"[:<:]]") || rest.starts_with(b"[:>:]]") {
            self.pos += 6;
            self.tables = true;
            self.foldable = true;
            return Some(());
        }
        self.class()
    }

    /// A bracket class, the `[` already read. A negated class matches letters, and so does a
    /// range that spans one; the members are tracked as characters so a range's ends are known.
    fn class(&mut self) -> Option<()> {
        self.foldable |= self.eat(b'^');
        let mut first = true;
        let mut last: Option<u32> = None;
        let mut dash = false;
        loop {
            let b = self.next()?;
            let member = match b {
                b']' if !first => return Some(()),
                b'\\' => self.escape(true)?,
                b'[' => {
                    self.class_bracket()?;
                    None
                }
                b'-' if last.is_some() && !dash && self.peek() != Some(b']') => {
                    dash = true;
                    first = false;
                    continue;
                }
                _ => {
                    self.literal(b);
                    Some(u32::from(b))
                }
            };
            match (member, last, dash) {
                (Some(hi), Some(lo), true) => {
                    self.foldable |= range_has_letter(lo, hi);
                    last = None;
                }
                (Some(v), _, _) => last = Some(v),
                (None, _, _) => last = None,
            }
            dash = false;
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
        let (negated, name) = match name.strip_prefix(b"^") {
            Some(name) => (true, name),
            None => (false, name),
        };
        if !POSIX_NAMES.contains(&name) {
            return None;
        }
        self.tables |= !POSIX_FIXED.contains(&name);
        self.ascii_class |= name == b"ascii";
        self.foldable |= negated || !POSIX_NO_LETTERS.contains(&name);
        self.pos += end + 3;
        Some(())
    }

    /// Open a group: its `x` flag is restored at the `)`.
    fn open(&mut self) {
        self.stack.push(self.x_now);
    }

    /// A group, the `(` already read.
    fn group(&mut self) -> Option<()> {
        if self.eat(b'*') {
            return self.verb();
        }
        if !self.eat(b'?') {
            self.open();
            return Some(());
        }
        match self.peek()? {
            b'#' => self.name(b')'),
            b':' | b'=' | b'!' | b'>' | b'|' => {
                self.pos += 1;
                self.open();
                Some(())
            }
            b'<' => {
                self.pos += 1;
                if !matches!(self.peek(), Some(b'=' | b'!')) {
                    self.name(b'>')?;
                } else {
                    self.pos += 1;
                }
                self.open();
                Some(())
            }
            b'P' => {
                self.pos += 1;
                match self.next()? {
                    b'<' => {
                        self.name(b'>')?;
                        self.open();
                        Some(())
                    }
                    // `(?P=name)` is a back reference, compared folded under a caseless flag.
                    b'=' => {
                        self.foldable = true;
                        self.name(b')')
                    }
                    b'>' => self.name(b')'),
                    _ => None,
                }
            }
            b'\'' => {
                self.pos += 1;
                self.name(b'\'')?;
                self.open();
                Some(())
            }
            b'&' | b'R' | b'C' | b'0'..=b'9' | b'+' => self.name(b')'),
            b'(' => {
                self.open();
                self.condition()
            }
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

    /// `(?imsxnJUr-^…)` and `(?…:`: the flags an option group sets. A group with no body
    /// (`(?x)`) applies to the rest of the enclosing group; one with a body (`(?x:`) opens it.
    fn inline_flags(&mut self) -> Option<()> {
        let (mut negated, mut x) = (false, self.x_now);
        loop {
            match self.next()? {
                b')' => {
                    self.x_now = x;
                    return Some(());
                }
                b':' => {
                    self.open();
                    self.x_now = x;
                    return Some(());
                }
                b'-' => negated = true,
                b'^' => x = false,
                b'i' => self.caseless |= !negated,
                b'x' => {
                    x = !negated;
                    self.extended |= x;
                }
                b'm' | b's' | b'n' | b'J' | b'U' | b'a' | b'D' | b'S' | b'W' | b'P' | b'T'
                | b'r' => {}
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
            if ALPHA_GROUPS.contains(&name) {
                self.open();
                return Some(());
            }
            return self.name(b')');
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
