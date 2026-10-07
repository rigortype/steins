//! Whether a **literal** PCRE pattern consults the C library's character tables (ADR-0101
//! §3.10, issue #1000, slice S5).
//!
//! `pcre_get_compiled_regex_cache` hands PCRE2 the tables `pcre2_maketables()` builds from the
//! process locale once a script has called `setlocale` with `LC_CTYPE` or `LC_ALL`, so a pattern
//! reads the locale cell exactly when PCRE2 asks those tables something. The modifier `u` sets
//! `PCRE2_UTF` **and** `PCRE2_UCP`, and UCP is what routes `\w`, `\s`, `\b`, the POSIX classes
//! (but `[:ascii:]`) to Unicode properties instead of the tables; `(*UTF)` alone does not, so the
//! exemption is UCP and not UTF.
//!
//! [`pattern_reads_locale`] decides lexically, with a byte scan of its own (the group-structure
//! reader above declines on `x`, `n` and every `(*…)` verb, which is exactly where this one has
//! to answer). Outside UCP a pattern reads iff it holds
//!
//! * `\w \W \s \S \b \B`, `[[:<:]]` or `[[:>:]]` (rewritten to `\b`), or a POSIX class other
//!   than `[:digit:]` and `[:xdigit:]`, which C fixes in every locale (C11 7.4.1.5);
//! * a group or reference name with a byte of `0x80..=0xFF`, whose validity the table decides.
//!
//! In **every** mode a pattern also reads iff it holds
//!
//! * a **caseless flag** (the modifier `i`, or an `i` that an inline option group sets) and a
//!   character it can match that is a letter: a literal letter or byte of `0x80..=0xFF`, a
//!   range whose span holds a letter (`[!-~]`), `.`, a negated class, `\w`, `\D`, `\S`, `\N`,
//!   `\p{..}`, a POSIX class with letters, a numeric escape or a back reference (`\1`, `\k<n>`,
//!   `(?P=n)`). Under UTF and UCP too, caseless matching of an ASCII pattern character compares
//!   through the locale's lowercase table (`pcre2_match.c:1052-1056`, and the fcc table the
//!   JIT builds from it), so on glibc's `tr_TR`, where the case map of `I` and `i` is not the
//!   C one, `/^id$/iu` against `ID` moves. That cannot be witnessed on macOS; the rule extends
//!   D-S5a to `u` on that evidence. The pattern reads nothing only where every character it can
//!   match is provably no letter: digits, punctuation, or ranges confined to those;
//! * the **`x` flag** and a byte of `0x80..=0xFF`: PCRE2 skips what the table's `isspace` says
//!   in the pattern, and `0xA0` is a space in some locales (witnessed under `u` too);
//! * `[[:ascii:]]` or `[[:^ascii:]]`, which PCRE2 keeps on the table under UCP
//!   (`pcre2_compile.c:752`), and, under `(*UCP)` without `u`, a group name with a byte of
//!   `0x80..=0xFF` (`read_name`).
//!
//! Everything else is exempt, and witnessed so: `\d`, `\h`, `\v`, `\R`, literal bytes and
//! ranges without a caseless flag, `\Q..\E` and a `preg_quote`d literal. An `x`-mode `#` outside
//! a class comments out the rest of its line, so what the comment holds is not scanned.
//!
//! A pattern this reader cannot parse as PCRE2 does (unbalanced, an unknown escape or verb, an
//! unknown modifier) is `None`, never a verdict: the caller treats it as a read that depends on a
//! value the site cannot see.

mod scan;

use scan::Scan;

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
            b' ' | b'\n' | b'\r' | b'm' | b's' | b'A' | b'D' | b'S' | b'U' | b'X' | b'J' | b'n'
            | b'r' => {}
            b'u' => unicode = true,
            b'i' => caseless = true,
            b'x' => extended = true,
            _ => return None,
        }
    }
    let src = body.as_bytes();
    let start = leading_options(src);
    let ucp = unicode || start.ucp;
    if ucp {
        // UCP leaves the tables but for the `x` flag's whitespace, a caseless flag, `[:ascii:]`
        // and (without UTF) a name: only a pattern that may hold one of them is scanned.
        let high = src.iter().any(|b| *b >= 0x80);
        if high && (extended || sets_inline_flag(src, b'x')) {
            return Some(true);
        }
        let ascii = src.windows(7).any(|w| w == b"ascii:]");
        if !(caseless || sets_inline_flag(src, b'i') || ascii || (high && !unicode)) {
            return Some(false);
        }
    }
    let mut scan = Scan {
        src,
        pos: start.end,
        caseless,
        extended,
        x_now: extended,
        any_newline: start.any_newline,
        ..Scan::default()
    };
    scan.run()?;
    let folded = scan.caseless && scan.foldable;
    let skipped = scan.extended && scan.high;
    Some(if ucp {
        scan.ascii_class || folded || skipped || (scan.names && !unicode)
    } else {
        scan.tables || scan.names || folded || skipped
    })
}

/// The pattern-start options `(*UTF)(*UCP)(*CRLF)…` before the expression proper.
struct Leading {
    ucp: bool,
    any_newline: bool,
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
    let mut out = Leading { ucp: false, any_newline: false, end: 0 };
    while src[out.end..].starts_with(b"(*") {
        let rest = &src[out.end + 2..];
        let Some(close) = rest.iter().position(|b| *b == b')') else { break };
        let name = rest[..close].split(|b| *b == b'=').next().unwrap_or_default();
        if !is_start_option(name) {
            break;
        }
        out.ucp |= name == b"UCP";
        out.any_newline |= name == b"ANY";
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
            ("P21 [:alpha:] with u", "/^[[:alpha:]]$/u"),
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

    /// Caseless matching compares through the table under `u` and UCP as well (`pcre2_match.c`'s
    /// lowercase table, the JIT's fcc table): a caseless flag reads whenever the pattern can match
    /// a letter. The macOS witnesses D1 to D5 and S8, S24 stand still (no `tr_TR` case map moves
    /// there); the rule is the glibc evidence, as D-S5a's `i` rule is.
    #[test]
    fn a_caseless_flag_reads_under_u_and_ucp_where_a_letter_can_match() {
        for (row, pattern) in [
            ("D1", "/^i$/iu"),
            ("D2", "/^I$/iu"),
            ("D3", "/^I$/iu"),
            ("D4", "/^[a-z]$/iu"),
            ("D5", "/^i+$/iu"),
            ("P20", "/^\u{c3}\u{84}$/iu"),
            ("S8", "/^\u{c4}$/iu"),
            ("S24", r"/(*UCP)^\xC4$/i"),
            ("G1 an escaped high character", r"/^\x{c4}$/iu"),
            ("a range spanning letters", "/^[!-~]$/iu"),
            ("a range from punctuation to a letter", "/^[ -a]$/iu"),
            ("dot", "/^.$/iu"),
            ("a negated class", "/^[^0-9]$/iu"),
            ("a negated digit class", r"/^\D$/iu"),
            ("a property", r"/^\p{L}$/iu"),
            ("w with i under UCP", r"/(*UCP)^\w$/i"),
            ("a POSIX class with letters", "/^[[:alpha:]]$/iu"),
            ("xdigit holds a to f", "/^[[:xdigit:]]$/iu"),
            ("a negated digit POSIX class", "/^[[:^digit:]]$/iu"),
            ("a back reference", r"/^(.)\1$/iu"),
            ("an inline flag", "/^(?i)a$/u"),
            ("an inline group", "/^(?i:a)b$/u"),
        ] {
            assert_eq!(reads(pattern), Some(true), "{row}: {pattern}");
        }
        // Without a caseless flag, or with nothing a letter can match, `u` reads nothing.
        for (row, pattern) in [
            ("no flag", "/^i$/u"),
            ("digits", "/^[0-9]+$/iu"),
            ("punctuation", "#^[!-/:-@]$#iu"),
            ("a class of punctuation", r"/^[\[-`{-~]$/iu"),
            ("d", r"/^\d$/iu"),
            ("h and v", r"/^\h\v$/iu"),
            ("a POSIX class with no letters", "/^[[:digit:][:punct:][:space:]]$/iu"),
            ("a letter only in a name", r"/(?<abc>\d)/iu"),
        ] {
            assert_eq!(reads(pattern), Some(false), "{row}: {pattern}");
        }
        // The modifier, not the group, decides the letter-free cases above; a pattern with a
        // letter and no flag at all is none.
        assert_eq!(reads("/^abc$/u"), Some(false));
        assert_eq!(reads(r"/(*UCP)^abc$/"), Some(false));
    }

    /// `(?P=name)` is a back reference like `\k<name>`, compared folded (B1); the other spellings
    /// of a subroutine call fold nothing.
    #[test]
    fn a_named_back_reference_is_foldable_in_every_spelling() {
        for pattern in [
            "/^(?<a>.)(?P=a)$/i",
            r"/^(?<a>.)\k<a>$/i",
            r"/^(?<a>.)\k{a}$/i",
            r"/^(?<a>.)\k'a'$/i",
            r"/^(?<a>.)\g{a}$/i",
            r"/^(.)\g{1}$/i",
            r"/^(.)\g{-1}$/i",
            r"/^(\d)(?P=x)(?<x>.)$/i",
            // Nothing else here can match a letter: the reference alone is the reader.
            r"/^(?<a>\d)(?P=a)$/i",
            r"/^(?<a>\d)\k<a>$/i",
        ] {
            assert_eq!(reads(pattern), Some(true), "{pattern}");
        }
        for pattern in [r"/^(?<a>\d)(?P=a)$/", r"/^(?<a>\d)(?&a)$/i", r"/^(?P<a>\d)(?P>a)$/i"] {
            assert_eq!(reads(pattern), Some(false), "{pattern}");
        }
    }

    /// An `x`-mode `#` outside a class comments out the rest of its line, so a `\Q` or `(?#` in
    /// the comment swallows nothing of what follows the newline (B2); in a class, after a
    /// backslash, in `\Q..\E`, and where `x` is off, it is literal.
    #[test]
    fn an_extended_comment_hides_its_line_and_nothing_after_it() {
        for (row, pattern) in [
            ("B", "/^#\\Q\n\\w$/x"),
            ("B2", "/^#(?#\n\\w(a)?$/x"),
            ("an unclosed class in the comment", "/\\w #[\n/x"),
            ("an unclosed group in the comment", "/\\w #(\n/x"),
            ("a comment then a reader", "/a # \\d\n\\s/x"),
            ("an inline x", "/(?x)a # (\n\\w/"),
            ("a scoped x ends at its group", "/(?x:a # (\n)\\w/"),
            ("x restored after the group", "/(?x:a)#\\w/"),
            ("a cleared x", "/(?x)a(?-x)#\\w/"),
            ("an escaped hash", "/(?x)a\\#\\w/"),
            ("a hash in a class", "/(?x)[#]\\w/"),
            ("a hash in a quote", "/(?x)\\Q#\\E\\w/"),
            ("no x", "/a#\\w/"),
        ] {
            assert_eq!(reads(pattern), Some(true), "{row}: {pattern:?}");
        }
        for (row, pattern) in [
            ("a comment holds the reader", "/a # \\w\n\\d/x"),
            ("a comment holds a quote", "/a #\\Q\n\\d/x"),
            ("an inline x comment", "/(?x)a # \\w\n/"),
            ("a comment to the end", "/(?x)a # \\w/"),
            ("a CR ends it", "/a #\\w\r\\d/x"),
            ("a scoped x comment", "/(?x: a # \\w\n)\\d/"),
            ("an unmatched paren in the comment", "/\\d #)\n/x"),
            ("a high byte in a comment is not skipped whitespace", "/^a#\u{a0}\nb$/x"),
        ] {
            assert_eq!(reads(pattern), Some(false), "{row}: {pattern:?}");
        }
    }

    /// UCP leaves the tables but for `[:ascii:]` and, without UTF, a name above ASCII (B3).
    #[test]
    fn ucp_still_asks_the_table_for_ascii_and_for_a_non_utf_name() {
        for pattern in [
            "/^[[:ascii:]]$/u",
            "/^[[:^ascii:]]$/u",
            "/(*UCP)^[[:ascii:]]$/",
            "/(*UCP)^[[:^ascii:]]$/",
            "/(*UCP)(?<\u{e4}>a)/",
            "/(*UCP)(?<a>a)\\k<\u{e4}>?/",
        ] {
            assert_eq!(reads(pattern), Some(true), "{pattern:?}");
        }
        // With UTF a name is checked against Unicode properties (R1 to R8), and the other
        // classes under UCP stay put.
        for pattern in [
            "/(?<\u{e4}>a)\\k<\u{e4}>/u",
            "/(?<\u{e4}>a)(?P=\u{e4})/u",
            "/(?<\u{e4}>a)(?&\u{e4})/u",
            "/(?<\u{e4}>a)\\g{\u{e4}}/u",
            "/(*MARK:\u{e4})a/u",
            "/^[[:cntrl:][:print:][:graph:][:punct:][:blank:][:xdigit:]]$/u",
        ] {
            assert_eq!(reads(pattern), Some(false), "{pattern:?}");
        }
        // Outside UCP a name above ASCII reads.
        assert_eq!(reads("/(?<\u{e4}>a)/"), Some(true));
        assert_eq!(reads("/(*UTF)(?<\u{e4}>a)/"), Some(true));
    }

    /// `[[:<:]]` and `[[:>:]]` are rewritten to `\b(?=\w)` and `\b(?<=\w)`, and the `r` modifier
    /// and `(?r)` (PHP 8.4) are valid: none of them is a decline (B4).
    #[test]
    fn word_boundary_classes_and_the_r_flag_are_known() {
        assert_eq!(reads("/[[:<:]]\\xE4/"), Some(true));
        assert_eq!(reads("/\\xE4[[:>:]]/"), Some(true));
        assert_eq!(reads("/[[:<:]]a/u"), Some(false));
        assert_eq!(reads(r"/^\w$/r"), Some(true));
        assert_eq!(reads(r"/^(?r)\w$/"), Some(true));
        assert_eq!(reads(r"/^(?r)\d$/"), Some(false));
        assert_eq!(reads("/^a$/ri"), Some(true));
        assert_eq!(reads(r"/^\d$/r"), Some(false));
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
            "/a)/",
            "/(a/",
            "/(?:a/",
            "/(?x:a/x",
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
