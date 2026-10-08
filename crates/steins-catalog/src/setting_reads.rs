//! The setting readers whose read a call decides (ADR-0101 §3.9, issue #1000, S4).
//!
//! A gate names the [`SettingCell`] whose read it decides ([`SettingReadGate::cell`]). Every gate
//! of S4 and S5 decides the locale's; the cells of the later slices of S6 add their own kinds
//! and name their cell, so the effects pass drops the label the gate's cell spells and no other.
//!
//! `basename` is the one reader this slice states unconditionally: `php_basename` consults the
//! locale-derived `CG(ascii_compatible_locale)` before it looks at a byte, so every call reads it,
//! the empty string included. Every other reader reads only if the call as written reaches the
//! routine that consults the C library, so its row is the **upper bound** and the call site
//! decides it, in the three ways the criterion of ADR-0101 §3.2 allows: a literal argument proves
//! the read or drops it lexically, an omitted one is the parameter's default, and any other (a
//! variable, an expression the scan cannot evaluate, a named or spread argument list) is the
//! `value-dependent-read` gap and never a label. The deciding argument is a **mode** the caller
//! selects, or the **content** the call hands the routine; php-src (`php-8.5.11`) says which:
//!
//! | name | deciding argument | reads the locale when |
//! | --- | --- | --- |
//! | `sort`, `rsort`, `asort`, `arsort` | `$flags` (1) | its base type is `SORT_LOCALE_STRING` (`strcoll`) or `SORT_NATURAL` (`strnatcmp`'s `isspace`, `isdigit`, `toupper`); `SORT_STRING` with `SORT_FLAG_CASE` reads on a floor below 8.2, so it is undecided |
//! | `ksort`, `krsort` | `$flags` (1) | the same, or `SORT_STRING` with `SORT_FLAG_CASE`: the key comparison folds case through `tolower` where the data sorts use the engine's ASCII table |
//! | `substr_compare` | `$case_insensitive` (4) | it is true (`zend_binary_strncasecmp_l`) |
//! | `pathinfo` | `$flags` (1) | it asks for the basename, extension or filename (`php_basename`); the directory name alone reads nothing |
//! | `ctype_*` (not digit, xdigit) | `$text` (0) | a string is not empty (the first byte is classified) or an `int` lies in -128..=255; every other type returns `false` before any table (`ctype_fallback`) |
//! | `strnatcmp`, `strnatcasecmp` | both operands (0, 1) | neither is empty (`strnatcmp_ex` returns on a length of 0 before a table) |
//! | `escapeshellarg` | `$arg` (0) | the string is not empty (`php_mblen` per byte); a NUL byte throws first |
//! | `strip_tags` | `$string` (0) | it holds a `<` (`isspace(p[1])` after one, the first `<` is reached in the start state) |
//! | `parse_url` | `$url` (0) | the first `:` is not at index 0 (`isalpha` over the scheme), or there is no `:`, no leading `//` and some byte other than `?` and `#` (the path, query or fragment is non-empty, and `php_replace_controlchars` calls `iscntrl` on each byte) |
//! | `strftime`, `gmstrftime` | `$format` (0) | it holds a conversion that names the locale: `a A b B c h p r x X`; the numeric ones read nothing, and any other conversion is undecided |
//! | `preg_match`, `preg_match_all`, `preg_replace`, `preg_replace_callback`, `preg_replace_callback_array` (its keys), `preg_filter`, `preg_split`, `preg_grep` | `$pattern` (0), or an array literal of patterns | the literal pattern consults the C library's character tables ([`pattern_reads_locale`](crate::preg::pattern_reads_locale), ADR-0101 §3.10, S5): `\w \W \s \S \b \B`, a POSIX class but `[:digit:]` and `[:xdigit:]`, a caseless flag where a letter can match, the `x` flag over a byte of `0x80..=0xFF`, `[[:<:]]`; the `u` modifier and a leading `(*UCP)` exempt the classes but not the caseless flag, the `x` flag, `[:ascii:]` or a name above ASCII under `(*UCP)` alone. An array reads if any of its patterns does |
//! | the encoding readers (the `$encoding` of `mb_*`, `iconv_*`, `htmlspecialchars` and kin; `mb_internal_encoding` and the other accessors) | `$encoding`, or the accessor's one argument | the argument is omitted or `null`, by class ([`encoding`], ADR-0101 §3.13, S6d); they decide the **encoding** cell's read, and an accessor or `mb_regex_set_options` also its write |
//!
//! A literal that shows no reading trigger is **absent**, not undecided, only where php-src shows
//! the routine skipped; where the trigger is wider than the call can show (a `parse_url` literal
//! that starts with a colon, a conversion this table does not know) the verdict is undecided.
//!
//! The catalog states the decision ([`SettingReadGate::reads`]); the effects pass reads the call.
//! The rows follow `PINNED_PHP` (8.5), as every row does, except that a verdict that differs on
//! 8.1 (the data sorts' case folding) is left undecided: a summary is a per-file fact and cannot
//! depend on the project's PHP floor.

mod encoding;

use crate::SettingCell;
use crate::preg::pattern_reads_locale;

/// What a call shows of one deciding argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateArg<'a> {
    /// The call does not supply the argument: the parameter's default decides.
    Omitted,
    /// An integer the scan evaluated (a literal, an engine constant, a `|` of such terms).
    Int(i64),
    /// A literal `true` or `false`.
    Bool(bool),
    /// A string literal, decoded.
    Str(&'a str),
    /// A literal `null`, for the readers whose default is the cell's (the encoding argument).
    /// The locale readers' `null` is [`Self::NotText`].
    Null,
    /// A value shown to be neither a string nor an integer: a `null`, `bool`, `float` or array
    /// literal, or a by-value parameter its declared type keeps from both.
    NotText,
    /// An array literal every one of whose patterns is a string literal, decoded: the patterns
    /// of `preg_replace` and its kin, or the keys of `preg_replace_callback_array`.
    Strs(&'a [String]),
}

/// Which arguments of a gated reader decide its read, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingReadGate {
    cell: SettingCell,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    DataSortFlags,
    KeySortFlags,
    CaseInsensitive,
    PathinfoFlags,
    Ctype,
    StrNat,
    EscapeShellArg,
    StripTags,
    ParseUrl,
    Strftime,
    PregPattern,
    Encoding(encoding::Gate),
}

const SORT_FLAG_CASE: i64 = 8;
const SORT_STRING: i64 = 2;
const SORT_LOCALE_STRING: i64 = 5;
const SORT_NATURAL: i64 = 6;
/// The three `pathinfo` parts `php_basename` produces. `PATHINFO_DIRNAME` (1) is `zend_dirname`'s
/// and reads nothing.
const PATHINFO_BASENAME_PARTS: i64 = 2 | 4 | 8;

/// The gate of the builtin `name` (case-insensitive), or `None` for a name whose read is
/// unconditional or absent.
#[must_use]
pub fn setting_read_gate(name: &str) -> Option<SettingReadGate> {
    let lower = name.to_ascii_lowercase();
    let kind = match lower.as_str() {
        "sort" | "rsort" | "asort" | "arsort" => Kind::DataSortFlags,
        "ksort" | "krsort" => Kind::KeySortFlags,
        "substr_compare" => Kind::CaseInsensitive,
        "pathinfo" => Kind::PathinfoFlags,
        "ctype_alnum" | "ctype_alpha" | "ctype_cntrl" | "ctype_graph" | "ctype_lower"
        | "ctype_print" | "ctype_punct" | "ctype_space" | "ctype_upper" => Kind::Ctype,
        "strnatcmp" | "strnatcasecmp" => Kind::StrNat,
        "escapeshellarg" => Kind::EscapeShellArg,
        "strip_tags" => Kind::StripTags,
        "parse_url" => Kind::ParseUrl,
        "strftime" | "gmstrftime" => Kind::Strftime,
        _ if crate::preg::compiles_pattern_argument(name) => Kind::PregPattern,
        _ => {
            let gate = encoding::gate_of(&lower)?;
            return Some(SettingReadGate { cell: SettingCell::Encoding, kind: Kind::Encoding(gate) });
        }
    };
    Some(SettingReadGate { cell: SettingCell::Locale, kind })
}

impl SettingReadGate {
    /// The cell whose read the gate decides: the label [`SettingCell::read_label`] spells is the
    /// one the call keeps or drops.
    #[must_use]
    pub const fn cell(self) -> SettingCell {
        self.cell
    }

    /// The positional indexes of the deciding arguments, in the order [`Self::reads`] takes them.
    #[must_use]
    pub const fn positions(self) -> &'static [usize] {
        match self.kind {
            Kind::DataSortFlags | Kind::KeySortFlags | Kind::PathinfoFlags => &[1],
            Kind::CaseInsensitive => &[4],
            Kind::StrNat => &[0, 1],
            Kind::Encoding(gate) => gate.positions(),
            Kind::Ctype
            | Kind::EscapeShellArg
            | Kind::StripTags
            | Kind::ParseUrl
            | Kind::Strftime
            | Kind::PregPattern => &[0],
        }
    }

    /// Whether the read happens at a call whose deciding arguments show `args` (one entry per
    /// [`Self::positions`], `None` for an argument the scan shows nothing of), or `None` when
    /// they do not decide it: the caller treats that as the read depending on a value the site
    /// cannot see.
    #[must_use]
    pub fn reads(self, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        let first = args.first().copied().flatten();
        match self.kind {
            Kind::DataSortFlags => sort_reads(first?, false),
            Kind::KeySortFlags => sort_reads(first?, true),
            Kind::CaseInsensitive => match first? {
                GateArg::Omitted => Some(false),
                GateArg::Bool(flag) => Some(flag),
                _ => None,
            },
            Kind::PathinfoFlags => match first? {
                GateArg::Omitted => Some(true),
                GateArg::Int(flags) => Some(flags & PATHINFO_BASENAME_PARTS != 0),
                _ => None,
            },
            Kind::Ctype => match first? {
                GateArg::Str(text) => Some(!text.is_empty()),
                GateArg::Int(v) => Some((-128..=255).contains(&v)),
                GateArg::Bool(_) | GateArg::NotText | GateArg::Null => Some(false),
                GateArg::Omitted | GateArg::Strs(_) => None,
            },
            Kind::StrNat => strnat_reads(first, args.get(1).copied().flatten()),
            Kind::EscapeShellArg => match first? {
                GateArg::Str(arg) if arg.contains('\0') => None,
                GateArg::Str(arg) => Some(!arg.is_empty()),
                _ => None,
            },
            Kind::StripTags => match first? {
                GateArg::Str(text) => Some(text.contains('<')),
                _ => None,
            },
            Kind::ParseUrl => match first? {
                GateArg::Str(url) => parse_url_reads(url),
                _ => None,
            },
            Kind::Strftime => match first? {
                GateArg::Str(format) => strftime_reads(format),
                _ => None,
            },
            Kind::PregPattern => match first? {
                GateArg::Str(pattern) => pattern_reads_locale(pattern),
                GateArg::Strs(patterns) => patterns_read_locale(patterns),
                // No pattern, no compile: `ArgumentCountError` first.
                GateArg::Omitted => Some(false),
                _ => None,
            },
            Kind::Encoding(gate) => gate.reads(args),
        }
    }

    /// Whether the call may write the cell the gate names, at a call whose deciding arguments
    /// show `args` (as [`Self::reads`] takes them). Only an accessor (`mb_internal_encoding` and
    /// its kin) and `mb_regex_set_options` write, and only when given something to set; every
    /// other gate answers `true`, and its row has no write to keep or drop.
    #[must_use]
    pub fn writes(self, args: &[Option<GateArg<'_>>]) -> bool {
        match self.kind {
            Kind::Encoding(gate) => gate.writes(args),
            _ => true,
        }
    }
}

/// An array of patterns reads if any of them does; one the reader declines leaves the verdict
/// undecided unless another pattern already reads. An empty array compiles nothing.
fn patterns_read_locale(patterns: &[String]) -> Option<bool> {
    let verdicts: Vec<Option<bool>> = patterns.iter().map(|p| pattern_reads_locale(p)).collect();
    if verdicts.contains(&Some(true)) {
        Some(true)
    } else if verdicts.contains(&None) {
        None
    } else {
        Some(false)
    }
}

/// `php_get_data_compare_func`'s and `php_get_key_compare_func`'s dispatch: the type is the flags
/// with `SORT_FLAG_CASE` removed, and a type that is not one of the named ones is `SORT_REGULAR`.
fn sort_reads(flags: GateArg<'_>, key_sort: bool) -> Option<bool> {
    let flags = match flags {
        GateArg::Omitted => return Some(false),
        GateArg::Int(flags) => flags,
        _ => return None,
    };
    let base = flags & !SORT_FLAG_CASE;
    if matches!(base, SORT_LOCALE_STRING | SORT_NATURAL) {
        return Some(true);
    }
    if base == SORT_STRING && flags & SORT_FLAG_CASE != 0 {
        // A key sort folds case through `tolower` on every version; a data sort does so only
        // before 8.2, which a per-file summary cannot know.
        return key_sort.then_some(true);
    }
    Some(false)
}

/// `strnatcmp_ex` returns on a zero length before it consults a table: a literal empty operand
/// settles the call whatever the other is; otherwise both must be shown non-empty.
fn strnat_reads(a: Option<GateArg<'_>>, b: Option<GateArg<'_>>) -> Option<bool> {
    let empty = |arg: Option<GateArg<'_>>| matches!(arg, Some(GateArg::Str("")));
    let text = |arg: Option<GateArg<'_>>| matches!(arg, Some(GateArg::Str(s)) if !s.is_empty());
    if empty(a) || empty(b) {
        Some(false)
    } else if text(a) && text(b) {
        Some(true)
    } else {
        None
    }
}

/// `php_url_parse_ex2` (`ext/standard/url.c`): the scheme loop calls `isalpha` from the first byte
/// when the first `:` is not at index 0, and every component it produces goes through
/// `php_replace_controlchars` (`iscntrl` per byte). A string with no colon and no leading `//`
/// produces a path, query or fragment from any byte other than `?` and `#`. The other shapes (a
/// leading colon, a leading `//`, only `?` and `#`) may fail before any component, so they are
/// undecided; the empty string produces an empty path and reads nothing.
fn parse_url_reads(url: &str) -> Option<bool> {
    if url.is_empty() {
        return Some(false);
    }
    match url.find(':') {
        Some(0) => None,
        Some(_) => Some(true),
        None if url.starts_with("//") => None,
        None if url.bytes().any(|b| b != b'?' && b != b'#') => Some(true),
        None => None,
    }
}

/// The conversions of the C `strftime` that name the locale (`LC_TIME`): the abbreviated and full
/// weekday and month names, `%c`, `%x` and `%X`, and the AM/PM markers and the 12-hour time that
/// holds one. Each is witnessed moving on macOS (`%P` is glibc's and `%v`, `%+` BSD's, so they are undecided), and
/// POSIX names them locale-dependent. The numeric conversions read nothing. A flag (`_ - 0 ^ #`),
/// a width and an `E` or `O` modifier may precede a conversion; a modifier on a conversion that
/// does not name the locale, a conversion this table does not know and a dangling `%` are
/// undecided. The empty format returns `false` before the C call.
fn strftime_reads(format: &str) -> Option<bool> {
    const NAMES: &[u8] = b"aAbBchprxX";
    const NUMERIC: &[u8] = b"CdDeFgGHIjklmMnRsStTuUVwWyYzZ%";
    let mut reads = false;
    let mut bytes = format.bytes().peekable();
    while let Some(b) = bytes.next() {
        if b != b'%' {
            continue;
        }
        while bytes.next_if(|c| matches!(c, b'_' | b'-' | b'0' | b'^' | b'#')).is_some() {}
        while bytes.next_if(u8::is_ascii_digit).is_some() {}
        let modified = bytes.next_if(|c| matches!(c, b'E' | b'O')).is_some();
        let conv = bytes.next()?;
        if NAMES.contains(&conv) {
            reads = true;
        } else if modified || !NUMERIC.contains(&conv) {
            return None;
        }
    }
    Some(reads)
}

#[cfg(test)]
mod tests {
    use super::{GateArg, SettingCell, parse_url_reads, setting_read_gate, strftime_reads};

    const PREG_NAMES: [&str; 8] = [
        "preg_match",
        "preg_match_all",
        "preg_replace",
        "preg_replace_callback",
        "preg_replace_callback_array",
        "preg_filter",
        "preg_split",
        "preg_grep",
    ];

    fn reads(name: &str, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        setting_read_gate(name).expect(name).reads(args)
    }

    fn one(name: &str, arg: GateArg<'_>) -> Option<bool> {
        reads(name, &[Some(arg)])
    }

    /// The gated names, and the positions their deciding arguments take.
    #[test]
    fn the_gated_names_and_positions() {
        for name in ["sort", "rsort", "asort", "arsort", "ksort", "krsort", "PathInfo"] {
            assert_eq!(setting_read_gate(name).expect(name).positions(), [1], "{name}");
        }
        assert_eq!(setting_read_gate("substr_compare").expect("row").positions(), [4]);
        assert_eq!(setting_read_gate("strnatcmp").expect("row").positions(), [0, 1]);
        for name in ["ctype_alpha", "escapeshellarg", "strip_tags", "parse_url", "gmstrftime"] {
            assert_eq!(setting_read_gate(name).expect(name).positions(), [0], "{name}");
        }
        for name in PREG_NAMES {
            assert_eq!(setting_read_gate(name).expect(name).positions(), [0], "{name}");
        }
        // `basename` reads on every call, so it has no gate; the sets C fixes have no row.
        for name in [
            "usort", "natsort", "array_multisort", "basename", "strcmp", "ctype_digit",
            "ctype_xdigit", "preg_quote", "preg_last_error", "preg_last_error_msg",
        ] {
            assert_eq!(setting_read_gate(name), None, "{name}");
        }
    }

    /// A data sort reads under `SORT_LOCALE_STRING` and `SORT_NATURAL` with or without
    /// `SORT_FLAG_CASE`; a key sort also under the case-folding string sort; a data sort under
    /// that flag pair reads on 8.1 and not on 8.2, so it is undecided.
    #[test]
    fn the_sort_flags_that_read() {
        for flags in [5, 6, 5 | 8, 6 | 8, 13, 14] {
            assert_eq!(one("sort", GateArg::Int(flags)), Some(true), "{flags}");
            assert_eq!(one("krsort", GateArg::Int(flags)), Some(true), "{flags}");
        }
        for flags in [0, 1, 2, 3, 4, 8, 9, 7, 15, 16, -1] {
            assert_eq!(one("asort", GateArg::Int(flags)), Some(false), "{flags}");
        }
        assert_eq!(one("ksort", GateArg::Int(2 | 8)), Some(true), "keys fold case by tolower");
        assert_eq!(one("sort", GateArg::Int(2 | 8)), None, "a data sort reads before 8.2");
        assert_eq!(one("ksort", GateArg::Int(2)), Some(false));
        assert_eq!(one("ksort", GateArg::Int(1 | 8)), Some(false));
        assert_eq!(one("rsort", GateArg::Omitted), Some(false), "SORT_REGULAR");
        assert_eq!(one("sort", GateArg::Bool(true)), None);
        assert_eq!(reads("sort", &[None]), None);
    }

    /// `substr_compare` reads only when case-insensitive, `pathinfo` unless only the
    /// directory is asked for.
    #[test]
    fn the_other_mode_gates() {
        assert_eq!(one("substr_compare", GateArg::Omitted), Some(false));
        assert_eq!(one("substr_compare", GateArg::Bool(false)), Some(false));
        assert_eq!(one("substr_compare", GateArg::Bool(true)), Some(true));
        assert_eq!(one("substr_compare", GateArg::Int(1)), None);
        assert_eq!(one("pathinfo", GateArg::Omitted), Some(true), "PATHINFO_ALL");
        for flags in [15, 2, 4, 8, 1 | 4, 3, 14] {
            assert_eq!(one("pathinfo", GateArg::Int(flags)), Some(true), "{flags}");
        }
        for flags in [1, 0, 16] {
            assert_eq!(one("pathinfo", GateArg::Int(flags)), Some(false), "{flags}");
        }
    }

    /// `ctype_*` classifies the first byte of a non-empty string and an `int` in -128..=255,
    /// and returns `false` for everything else before it consults a table.
    #[test]
    fn ctype_reads_a_string_or_a_small_int_and_nothing_else() {
        assert_eq!(one("ctype_alpha", GateArg::Str("a")), Some(true));
        assert_eq!(one("ctype_alpha", GateArg::Str("12")), Some(true), "the table says false");
        assert_eq!(one("ctype_alpha", GateArg::Str("")), Some(false));
        for v in [-128, -1, 0, 65, 255] {
            assert_eq!(one("ctype_upper", GateArg::Int(v)), Some(true), "{v}");
        }
        for v in [-129, 256, 1000, i64::MAX] {
            assert_eq!(one("ctype_upper", GateArg::Int(v)), Some(false), "{v}");
        }
        assert_eq!(one("ctype_punct", GateArg::Bool(true)), Some(false));
        assert_eq!(one("ctype_punct", GateArg::NotText), Some(false));
        assert_eq!(reads("ctype_punct", &[None]), None);
        assert_eq!(one("ctype_punct", GateArg::Omitted), None);
    }

    /// `strnatcmp_ex` consults a table only when both operands are non-empty.
    #[test]
    fn strnatcmp_reads_only_over_two_non_empty_operands() {
        let s = |t| Some(GateArg::Str(t));
        assert_eq!(reads("strnatcmp", &[s("a"), s("b")]), Some(true));
        assert_eq!(reads("strnatcasecmp", &[s("a"), s("")]), Some(false));
        assert_eq!(reads("strnatcmp", &[None, s("")]), Some(false), "one empty operand decides");
        assert_eq!(reads("strnatcmp", &[s(""), None]), Some(false));
        assert_eq!(reads("strnatcmp", &[s("a"), None]), None);
        assert_eq!(reads("strnatcmp", &[None, None]), None);
    }

    /// `strip_tags` reads at its first `<`, `escapeshellarg` per byte, and a NUL byte throws first.
    #[test]
    fn strip_tags_and_escapeshellarg_read_by_content() {
        assert_eq!(one("strip_tags", GateArg::Str("a <b> c")), Some(true));
        assert_eq!(one("strip_tags", GateArg::Str("plain text > here")), Some(false));
        assert_eq!(one("strip_tags", GateArg::Str("")), Some(false));
        assert_eq!(one("strip_tags", GateArg::Int(5)), None);
        assert_eq!(one("escapeshellarg", GateArg::Str("x")), Some(true));
        assert_eq!(one("escapeshellarg", GateArg::Str("")), Some(false));
        assert_eq!(one("escapeshellarg", GateArg::Str("a\0b")), None);
    }

    /// The shapes of `parse_url` the call can show: a scheme colon reads, a path with no colon
    /// reads, and the shapes that may fail before any component are undecided.
    #[test]
    fn parse_url_reads_by_the_shape_of_the_url() {
        for url in ["http://x/y", "a:b", "mailto:x@y", "/path", "a", "?q=1", "#f", "x?y", "a//b"] {
            assert_eq!(parse_url_reads(url), Some(true), "{url}");
        }
        assert_eq!(parse_url_reads(""), Some(false));
        for url in [":80", "://x", "//host/x", "//", "?", "#", "?#", "##"] {
            assert_eq!(parse_url_reads(url), None, "{url}");
        }
    }

    /// The conversions that name the locale read; the numeric ones do not; the rest are
    /// undecided.
    #[test]
    fn strftime_reads_by_its_conversions() {
        for format in ["%A", "%a %b", "%B %Y", "%c", "%x", "%X", "%p", "%r", "%h", "%-d %A", "%EX", "%OB"] {
            assert_eq!(strftime_reads(format), Some(true), "{format}");
        }
        for format in [
            "", "plain", "%Y-%m-%d %H:%M:%S", "%s", "%%", "%e %k %l %j %u %w", "%_d %-m %010Y",
            "%Z %z %F %T %R %D", "100%%",
        ] {
            assert_eq!(strftime_reads(format), Some(false), "{format}");
        }
        for format in ["%", "%Q", "%+", "%v", "%P", "%Ed", "%Od", "%A %Q", "%Y %", "%E"] {
            assert_eq!(strftime_reads(format), None, "{format}");
        }
    }

    /// A literal pattern decides by the verdict of `pattern_reads_locale`, for every name that
    /// compiles one; an array of patterns reads if any of them does, an empty one compiles
    /// nothing, and a pattern the reader declines is undecided unless another already reads.
    #[test]
    fn a_preg_call_reads_by_its_literal_pattern() {
        for name in PREG_NAMES {
            assert_eq!(one(name, GateArg::Str(r"/^\w$/")), Some(true), "{name}");
            assert_eq!(one(name, GateArg::Str(r"/^\d$/")), Some(false), "{name}");
            assert_eq!(one(name, GateArg::Str(r"/^\w$/u")), Some(false), "{name}");
            assert_eq!(one(name, GateArg::Str(r"/a\y/")), None, "{name}");
            assert_eq!(one(name, GateArg::Omitted), Some(false), "{name}");
            assert_eq!(one(name, GateArg::Bool(true)), None, "{name}");
            assert_eq!(one(name, GateArg::NotText), None, "{name}");
            assert_eq!(reads(name, &[None]), None, "{name}");
        }
        let list = |patterns: &[&str]| -> Option<bool> {
            let owned: Vec<String> = patterns.iter().map(|p| (*p).to_owned()).collect();
            one("preg_replace", GateArg::Strs(&owned))
        };
        assert_eq!(list(&[]), Some(false));
        assert_eq!(list(&["/a/", "/b/"]), Some(false));
        assert_eq!(list(&["/a/", r"/\s/"]), Some(true), "any reading pattern reads");
        assert_eq!(list(&[r"/\y/", r"/\s/"]), Some(true), "a pattern that reads beats a decline");
        assert_eq!(list(&["/a/", r"/\y/"]), None);
    }

    /// Every gated name carries the read on its row, as the upper bound the gate narrows, and
    /// the gate names the cell that read is: every gate of S4 and S5 decides the locale's.
    #[test]
    fn a_gated_name_carries_the_read_on_its_row() {
        for name in [
            "sort", "rsort", "asort", "arsort", "ksort", "krsort", "substr_compare", "pathinfo",
            "ctype_alnum", "ctype_alpha", "ctype_cntrl", "ctype_graph", "ctype_lower",
            "ctype_print", "ctype_punct", "ctype_space", "ctype_upper", "strnatcmp",
            "strnatcasecmp", "escapeshellarg", "strip_tags", "parse_url", "strftime", "gmstrftime",
            "preg_match", "preg_match_all", "preg_replace", "preg_replace_callback",
            "preg_replace_callback_array", "preg_filter", "preg_split", "preg_grep",
        ] {
            let row = crate::effect_labels(name);
            let gate = setting_read_gate(name).expect(name);
            assert_eq!(gate.cell(), SettingCell::Locale, "{name}");
            assert!(row.is_some_and(|l| l.contains(&gate.cell().read_label())), "{name}");
            assert_eq!(gate.cell().read_label(), "global.read.setting.locale", "{name}");
        }
    }
}
