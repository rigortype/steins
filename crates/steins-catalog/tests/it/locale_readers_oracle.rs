//! The S4 witness table of ADR-0101 §3.9 against the engine: each locale reader the catalog
//! colours moves between `C` and a witness locale, and each name it certifies or leaves
//! uncatalogued does not (issue #1000).
//!
//! Every probe runs under `C` and under the witness locales: `de_DE.UTF-8`, a Latin-1 locale
//! (`de_DE.ISO8859-1` on macOS, `de_DE.ISO-8859-1` once glibc has generated it), which is where a
//! byte of `0x80..=0xFF` is a letter, and, where the machine has it, `ja_JP.eucJP`, a variable
//! width locale that the multibyte readers consult. A row asserts the catalog's verdict in both
//! directions:
//!
//! * **soundness**: a probe that moved under a witness locale is a name the catalog colours; a
//!   name it certifies or leaves uncatalogued never moved. This is asserted on every platform.
//! * **precision**: a name the catalog colours has a probe that moved. Whether a byte is a letter,
//!   a space or an invalid sequence is the C library's table, so a row is either [`Expect::Moves`]
//!   (the probe reads a table every library defines the same way: a Latin-1 letter, a day name, a
//!   collation) or [`Expect::MovesOnMacos`] (php-src consults the table and the macOS library
//!   moved it; glibc's UTF-8 locales classify the bytes `0x80..=0xFF` as nothing, so the same
//!   probe may stay still there, and the row is asserted only where it was witnessed).
//!
//! Like `locale_oracle`, every test skips loudly without `php` or the locales unless `CI` is set,
//! where a missing one fails: the CI test job generates `de_DE.UTF-8` and the Latin-1 locale.

use std::process::{Command, Stdio};

use steins_catalog::{GateArg, certified_at_call_site, effect_labels, locale_read_gate};

use super::locale_oracle::oracle_unavailable;

const READ: &str = "global.read.setting.locale";

/// Where a row's probe is asserted to move.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Under `de_DE.UTF-8` or the Latin-1 locale, on every platform.
    Moves,
    /// Under some witness locale, where the library is macOS's.
    MovesOnMacos,
    /// Under none, and the catalog proves no read.
    Stable,
    /// Under none on 8.2 and later, and the catalog leaves the read undecided: the read of an
    /// older engine depends on a PHP floor a per-file summary cannot know.
    Undecided,
    /// The catalog proves the read by a lexical rule that is wider than any witness: the probe
    /// stood still on every locale of this machine (a caseless ASCII pattern, where a glibc
    /// `tr_TR` case map moves `I` and `i`), and a probe that did move is no contradiction.
    ReadsByRule,
}

/// What the catalog says of the name a probe exercises.
#[derive(Clone, Copy)]
enum Verdict {
    /// The row carries the read and nothing gates it.
    Row(&'static str),
    /// A gated name: the call as written decides, here by the argument shown.
    Gate(&'static str, &'static [Option<GateArg<'static>>]),
    /// Certified pure at a call site: no row, no read.
    Certified(&'static str),
    /// A known name with no setting row and no certification.
    Uncatalogued(&'static str),
    /// A name whose row says nothing of a setting: pure or otherwise, no `global.read.setting`.
    NoSetting(&'static str),
    /// A call `narrowed_setlocale_labels` decides: the name and the locale it is given.
    Narrowed(&'static str, &'static str),
}

impl Verdict {
    /// Whether the catalog proves the call reads the locale (`Some(true)`), proves it does not
    /// (`Some(false)`), or leaves it to a value the call does not show (`None`).
    fn reads(self) -> Option<bool> {
        match self {
            Self::Row(name) => {
                assert!(locale_read_gate(name).is_none(), "{name} is gated");
                Some(effect_labels(name).is_some_and(|l| l.contains(&READ)))
            }
            Self::Gate(name, args) => {
                let gate = locale_read_gate(name).unwrap_or_else(|| panic!("{name} has no gate"));
                assert!(effect_labels(name).is_some_and(|l| l.contains(&READ)), "{name}");
                assert_eq!(gate.positions().len(), args.len(), "{name}");
                gate.reads(args)
            }
            Self::Certified(name) => {
                assert!(certified_at_call_site(name), "{name} is not certified");
                assert!(effect_labels(name).is_none(), "{name} has a row");
                Some(false)
            }
            Self::Uncatalogued(name) => {
                assert!(effect_labels(name).is_none() && !certified_at_call_site(name), "{name}");
                Some(false)
            }
            Self::Narrowed(name, locale) => {
                let labels = steins_catalog::narrowed_setlocale_labels(name, locale, 2);
                assert!(labels.is_some_and(|l| !l.iter().any(|x| x.starts_with("global.write"))));
                Some(labels.is_some_and(|l| l.contains(&READ)))
            }
            Self::NoSetting(name) => {
                assert!(
                    effect_labels(name)
                        .is_none_or(|labels| labels.iter().all(|l| !l.starts_with("global."))),
                    "{name} reads a setting on its row"
                );
                Some(false)
            }
        }
    }
}

/// One witness row: a PHP expression returning a string, the catalog's verdict on the call it
/// exercises, and where it is asserted to move.
struct Row {
    label: &'static str,
    probe: &'static str,
    verdict: Verdict,
    expect: Expect,
}

const fn row(label: &'static str, probe: &'static str, verdict: Verdict, expect: Expect) -> Row {
    Row { label, probe, verdict, expect }
}

/// The 256-byte tables of `ctype_digit` and `ctype_xdigit`, which is all a locale can move.
const DIGIT_TABLE: &str =
    r#"implode(",", array_map(fn($b) => var_export(ctype_digit(chr($b)), true), range(0, 255)))"#;
const XDIGIT_TABLE: &str =
    r#"implode(",", array_map(fn($b) => var_export(ctype_xdigit(chr($b)), true), range(0, 255)))"#;

/// The rows of what the catalog colours, certifies or leaves out, name by name.
fn rows() -> Vec<Row> {
    use Expect::{Moves, MovesOnMacos, Stable, Undecided};
    use GateArg::Str;
    use Verdict::{Certified, Gate, Row as Reads, Uncatalogued};
    let sort = |flags: &'static str| match flags {
        // `$a` holds `ä` in UTF-8, which `de_DE` collates beside `a`.
        "locale" => r#"(function () { $a = ["\xC3\xA4", "b", "a"]; sort($a, SORT_LOCALE_STRING); return implode(",", $a); })()"#,
        "natural" => r#"(function () { $a = ["\xA0", "b", "a"]; sort($a, SORT_NATURAL); return implode(",", $a); })()"#,
        "regular" => r#"(function () { $a = ["\xC3\xA4", "b", "a", "\xE4", "\xC4"]; sort($a); return bin2hex(implode(",", $a)); })()"#,
        "string" => r#"(function () { $a = ["\xC3\xA4", "b", "a", "\xE4", "\xC4"]; sort($a, SORT_STRING); return bin2hex(implode(",", $a)); })()"#,
        // Before 8.2 the data sorts fold case through `tolower` too (`string_case_compare_function`
        // was ASCII-only from 8.2): the catalog follows the 8.5 pin, so older engines skip.
        _ => r#"PHP_VERSION_ID < 80200 ? "skip" : (function () { $a = ["x" => "\xC4", "y" => "\xE4", "z" => "b"]; asort($a, SORT_STRING | SORT_FLAG_CASE); return implode(",", array_keys($a)); })()"#,
    };
    vec![
        // The character classes of a byte: a Latin-1 letter is a letter, and a byte that is no
        // character is nothing, whichever locale says so.
        row("ctype_alpha", r#"var_export(ctype_alpha("\xE4"), true)"#, Gate("ctype_alpha", &[Some(Str("\u{e4}"))]), Moves),
        row("ctype_alnum", r#"var_export(ctype_alnum("\xE4"), true)"#, Gate("ctype_alnum", &[Some(Str("\u{e4}"))]), Moves),
        row("ctype_lower", r#"var_export(ctype_lower("\xE4"), true)"#, Gate("ctype_lower", &[Some(Str("\u{e4}"))]), Moves),
        row("ctype_upper", r#"var_export(ctype_upper("\xC4"), true)"#, Gate("ctype_upper", &[Some(Str("\u{c4}"))]), Moves),
        row("ctype_graph", r#"var_export(ctype_graph("\xE4"), true)"#, Gate("ctype_graph", &[Some(Str("\u{e4}"))]), Moves),
        row("ctype_print", r#"var_export(ctype_print("\xE4"), true)"#, Gate("ctype_print", &[Some(Str("\u{e4}"))]), Moves),
        row("ctype_punct", r#"var_export(ctype_punct("\xA1"), true)"#, Gate("ctype_punct", &[Some(Str("\u{a1}"))]), MovesOnMacos),
        row("ctype_cntrl", r#"var_export(ctype_cntrl("\x80"), true)"#, Gate("ctype_cntrl", &[Some(Str("\u{80}"))]), MovesOnMacos),
        row("ctype_space", r#"var_export(ctype_space("\xA0"), true)"#, Gate("ctype_space", &[Some(Str("\u{a0}"))]), MovesOnMacos),
        // C fixes these two sets in every locale: no row, and no byte moved.
        row("ctype_digit", DIGIT_TABLE, Uncatalogued("ctype_digit"), Stable),
        row("ctype_xdigit", XDIGIT_TABLE, Uncatalogued("ctype_xdigit"), Stable),
        // `LC_TIME`.
        row("strftime", r#"@strftime("%A %B", 86400 * 40)"#, Gate("strftime", &[Some(Str("%A %B"))]), Moves),
        row("gmstrftime", r#"@gmstrftime("%A %B", 86400 * 40)"#, Gate("gmstrftime", &[Some(Str("%A %B"))]), Moves),
        // `toupper`, `isspace`.
        row("strnatcasecmp", r#"strnatcasecmp("\xE4", "\xC4")"#, Gate("strnatcasecmp", &[Some(Str("\u{e4}")), Some(Str("\u{c4}"))]), Moves),
        row("strnatcmp", r#"strnatcmp("\xA0", "a1")"#, Gate("strnatcmp", &[Some(Str("\u{a0}")), Some(Str("a1"))]), MovesOnMacos),
        // `tolower`, only when case-insensitive.
        row(
            "substr_compare, case-insensitive",
            r#"substr_compare("\xE4", "\xC4", 0, null, true)"#,
            Gate("substr_compare", &[Some(GateArg::Bool(true))]),
            Moves,
        ),
        row(
            "substr_compare, case-sensitive",
            r#"substr_compare("\xE4", "\xC4", 0, null, false)"#,
            Gate("substr_compare", &[Some(GateArg::Bool(false))]),
            Stable,
        ),
        row(
            "substr_compare, no switch",
            r#"substr_compare("\xE4", "\xC4", 0)"#,
            Gate("substr_compare", &[Some(GateArg::Omitted)]),
            Stable,
        ),
        // The sorts: `strcoll` under `SORT_LOCALE_STRING`, `strnatcmp` under `SORT_NATURAL`, and
        // nothing under the flags that compare bytes or fold ASCII case.
        row("sort, SORT_LOCALE_STRING", sort("locale"), Gate("sort", &[Some(GateArg::Int(5))]), Moves),
        row("sort, SORT_NATURAL", sort("natural"), Gate("sort", &[Some(GateArg::Int(6))]), MovesOnMacos),
        row("sort, no flags", sort("regular"), Gate("sort", &[Some(GateArg::Omitted)]), Stable),
        row("sort, SORT_STRING", sort("string"), Gate("sort", &[Some(GateArg::Int(2))]), Stable),
        row("asort, SORT_STRING | SORT_FLAG_CASE", sort("fold"), Gate("asort", &[Some(GateArg::Int(10))]), Undecided),
        row(
            "rsort, SORT_STRING | SORT_FLAG_CASE",
            r#"PHP_VERSION_ID < 80200 ? "skip" : (function () { $a = ["\xC4", "\xE4", "b"]; rsort($a, SORT_STRING | SORT_FLAG_CASE); return bin2hex(implode(",", $a)); })()"#,
            Gate("rsort", &[Some(GateArg::Int(10))]),
            Undecided,
        ),
        // The key sorts fold case through `tolower`.
        row(
            "krsort, SORT_STRING | SORT_FLAG_CASE",
            r#"(function () { $a = ["\xC4" => 1, "\xE4" => 2, "b" => 3]; krsort($a, SORT_STRING | SORT_FLAG_CASE); return bin2hex(implode(",", array_keys($a))); })()"#,
            Gate("krsort", &[Some(GateArg::Int(10))]),
            Moves,
        ),
        row(
            "ksort, SORT_STRING",
            r#"(function () { $a = ["\xC4" => 1, "\xE4" => 2, "b" => 3]; ksort($a, SORT_STRING); return bin2hex(implode(",", array_keys($a))); })()"#,
            Gate("ksort", &[Some(GateArg::Int(2))]),
            Stable,
        ),
        // `php_basename`'s algorithm follows the locale's multibyte state.
        row("basename", r#"bin2hex(basename("\x8E/"))"#, Reads("basename"), MovesOnMacos),
        row(
            "pathinfo, PATHINFO_BASENAME",
            r#"bin2hex(serialize(pathinfo("\x8E/", PATHINFO_BASENAME)))"#,
            Gate("pathinfo", &[Some(GateArg::Int(2))]),
            MovesOnMacos,
        ),
        row(
            "pathinfo, PATHINFO_DIRNAME",
            r#"bin2hex(serialize(pathinfo("\x8E/a", PATHINFO_DIRNAME)))"#,
            Gate("pathinfo", &[Some(GateArg::Int(1))]),
            Stable,
        ),
        // `php_mblen` drops a byte that is no character of the locale.
        row("escapeshellarg", r#"bin2hex(escapeshellarg("\xE4"))"#, Gate("escapeshellarg", &[Some(Str("\u{e4}"))]), MovesOnMacos),
        // `isspace` after a `<`, `isalpha` in a scheme.
        row("strip_tags", r#"strip_tags("<\xA0b>x")"#, Gate("strip_tags", &[Some(Str("<\u{a0}b>x"))]), MovesOnMacos),
        row("parse_url", r#"bin2hex(serialize(parse_url("a\xE4://x/y")))"#, Gate("parse_url", &[Some(Str("a\u{e4}://x/y"))]), Moves),
        // `%.*F` never consults the locale: values, precisions, separators, the non-finite ones.
        row(
            "number_format",
            r#"implode("|", [number_format(1234.5), number_format(1234.5678, 2), number_format(-0.5), number_format(0.000001, 8), number_format(1e15, 3, ",", "."), number_format(1234567.891, 2, "\xE4", " "), number_format(-1234.567, 1), number_format(NAN), number_format(INF, 2), number_format(0.5), number_format(1.5, 0), number_format(1234.5, -2)])"#,
            Certified("number_format"),
            Stable,
        ),
    ]
}

/// The rows of the S5 witness table of ADR-0101 §3.10 (`s5-preg.php`, `s5-preg-2.php`): each
/// literal pattern feature of `preg_*` against the engine, between `C` and a witness locale.
/// Every compiling function reaches one compiler, so the function rows show that the verdict is
/// the pattern's whichever function carries it.
fn preg_rows() -> Vec<Row> {
    use Expect::{Moves, MovesOnMacos, ReadsByRule, Stable};
    use GateArg::Str;
    use Verdict::{Gate, Row as Reads};
    // The verdict of `preg_match` over a literal pattern; the argument slice lives for the run.
    let m = |pattern: &'static str| {
        Gate("preg_match", Box::leak(Box::new([Some(Str(pattern))])) as &'static [_])
    };
    vec![
        // What the tables answer: `\w`, `\W`, `\b`, `\B`, `\s`, the POSIX classes.
        row("preg P1 w", r#"var_export(preg_match('/^\w$/', "\xE4"), true)"#, m(r"/^\w$/"), Moves),
        row("preg P3 W", r#"var_export(preg_match('/^\W$/', "\xE4"), true)"#, m(r"/^\W$/"), Moves),
        row("preg P6 b", r#"var_export(preg_match('/\bx/', "\xE4x"), true)"#, m(r"/\bx/"), Moves),
        row("preg S25 B", r#"var_export(preg_match('/a\B/', "a\xE4"), true)"#, m(r"/a\B/"), Moves),
        row("preg S4 s", r#"var_export(preg_match('/^\s$/', "\xA0"), true)"#, m(r"/^\s$/"), MovesOnMacos),
        row("preg P7 alpha", r#"var_export(preg_match('/^[[:alpha:]]$/', "\xE4"), true)"#, m("/^[[:alpha:]]$/"), Moves),
        row("preg S16 upper", r#"var_export(preg_match('/^[[:upper:]]$/', "\xC4"), true)"#, m("/^[[:upper:]]$/"), Moves),
        row("preg S6 blank", r#"var_export(preg_match('/^[[:blank:]]$/', "\xA0"), true)"#, m("/^[[:blank:]]$/"), MovesOnMacos),
        // A caseless flag over a high byte folds through the tables, in every spelling.
        row("preg P9 i", r#"var_export(preg_match('/^\xC4$/i', "\xE4"), true)"#, m(r"/^\xC4$/i"), Moves),
        row("preg S14 inline group", r#"var_export(preg_match('/^(?i:\xC4)$/', "\xE4"), true)"#, m(r"/^(?i:\xC4)$/"), Moves),
        row("preg P22 inline flag", r#"var_export(preg_match('/^(?i)\xC4$/', "\xE4"), true)"#, m(r"/^(?i)\xC4$/"), Moves),
        row("preg S26 range under i", r#"var_export(preg_match("/^[\xC0-\xDE]$/i", "\xE4"), true)"#, m("/^[\u{c0}-\u{de}]$/i"), Moves),
        // The `x` flag skips what the table calls a space: a raw byte, and under `u` too.
        row("preg S10 x byte", r#"var_export(preg_match("/^a\xA0b$/x", 'ab'), true)"#, m("/^a\u{a0}b$/x"), MovesOnMacos),
        row("preg x byte under u", r#"var_export(preg_match("/^a\xC2\xA0b$/xu", 'ab'), true)"#, m("/^a\u{a0}b$/xu"), MovesOnMacos),
        row("preg x byte under UCP", r#"var_export(preg_match("/(*UCP)^a\xA0b$/x", 'ab'), true)"#, m("/(*UCP)^a\u{a0}b$/x"), MovesOnMacos),
        // `(*UTF)` is not UCP.
        row("preg P23 UTF only", r#"var_export(preg_match('/(*UTF)^\w$/', "\xC3\xA4"), true)"#, m(r"/(*UTF)^\w$/"), Moves),
        // The same compiler behind every function (P18, P19, P28, S21, S22, and the two others).
        row("preg P18 replace", r#"bin2hex(preg_replace('/\w/', '_', "a\xE4"))"#, Gate("preg_replace", &[Some(Str(r"/\w/"))]), Moves),
        row("preg P19 split", r#"count(preg_split('/\W/', "a\xE4b"))"#, Gate("preg_split", &[Some(Str(r"/\W/"))]), Moves),
        row("preg P28 grep", r#"count(preg_grep('/^\w$/', ["\xE4", 'a']))"#, Gate("preg_grep", &[Some(Str(r"/^\w$/"))]), Moves),
        row(
            "preg S21 callback array",
            r#"bin2hex(preg_replace_callback_array(['/\w/' => fn($m) => '_'], "\xE4"))"#,
            Gate("preg_replace_callback_array", &[Some(Str(r"/\w/"))]),
            Moves,
        ),
        row("preg S22 filter", r#"var_export(preg_filter('/\w/', '_', "\xE4"), true)"#, Gate("preg_filter", &[Some(Str(r"/\w/"))]), Moves),
        row("preg match_all", r#"var_export(preg_match_all('/\w/', "\xE4"), true)"#, Gate("preg_match_all", &[Some(Str(r"/\w/"))]), Moves),
        row(
            "preg replace callback",
            r#"bin2hex(preg_replace_callback('/\w/', fn($m) => '_', "\xE4"))"#,
            Gate("preg_replace_callback", &[Some(Str(r"/\w/"))]),
            Moves,
        ),
        // The lexical rule's `i` over a letter: no witness moved on this machine (ASCII only, or
        // UTF-8 bytes a Latin-1 table folds nowhere), the rule reads for the glibc `tr_TR` hazard.
        row("preg P10 i ascii", r#"var_export(preg_match('/^A$/i', 'a'), true)"#, m("/^A$/i"), ReadsByRule),
        row("preg P14 i range", r#"var_export(preg_match('/^[a-z]$/i', "\xC4"), true)"#, m("/^[a-z]$/i"), ReadsByRule),
        row("preg S1 i", r#"var_export(preg_match('/^i$/i', 'I'), true)"#, m("/^i$/i"), ReadsByRule),
        row("preg S2 I", r#"var_export(preg_match('/^I$/i', 'i'), true)"#, m("/^I$/i"), ReadsByRule),
        row("preg S3 i class", r#"var_export(preg_match('/^[a-z]$/i', 'I'), true)"#, m("/^[a-z]$/i"), ReadsByRule),
        row("preg S23 i word", r#"var_export(preg_match('/^ABC$/i', 'abc'), true)"#, m("/^ABC$/i"), ReadsByRule),
        row("preg S9 i bytes", r#"var_export(preg_match("/^\xC3\x84$/i", "\xC3\xA4"), true)"#, m("/^\u{c3}\u{84}$/i"), ReadsByRule),
        // `u` and a leading `(*UCP)` route the classes and the folding to Unicode properties.
        row("preg P2 w under u", r#"var_export(preg_match('/^\w$/u', "\xC3\xA4"), true)"#, m(r"/^\w$/u"), Stable),
        row("preg S8 iu", r#"var_export(preg_match("/^\xC3\x84$/iu", "\xC3\xA4"), true)"#, m("/^\u{c3}\u{84}$/iu"), ReadsByRule),
        row("preg P20 iu", r#"var_export(preg_match('/^\xC3\x84$/iu', "\xC3\xA4"), true)"#, m(r"/^\xC3\x84$/iu"), ReadsByRule),
        row("preg P21 alpha under u", r#"var_export(preg_match('/^[[:alpha:]]$/u', "\xC3\xA4"), true)"#, m("/^[[:alpha:]]$/u"), Stable),
        row("preg S24 UCP and i", r#"var_export(preg_match('/(*UCP)^\xC4$/i', "\xE4"), true)"#, m(r"/(*UCP)^\xC4$/i"), ReadsByRule),
        row("preg P24 UCP and w", r#"var_export(preg_match('/(*UCP)^\w$/', "\xE4"), true)"#, m(r"/(*UCP)^\w$/"), Stable),
        // The sets C fixes, and the patterns that ask no table.
        row("preg P4 d", r#"var_export(preg_match('/^\d$/', "\xB2"), true)"#, m(r"/^\d$/"), Stable),
        row("preg P16 d ascii", r#"var_export(preg_match('/^\d$/', '5'), true)"#, m(r"/^\d$/"), Stable),
        row("preg S18 digit", r#"var_export(preg_match('/^[[:digit:]]$/', "\xB2"), true)"#, m("/^[[:digit:]]$/"), Stable),
        row("preg S19 xdigit", r#"var_export(preg_match('/^[[:xdigit:]]$/', "\xE4"), true)"#, m("/^[[:xdigit:]]$/"), Stable),
        row("preg P11 byte", r#"var_export(preg_match("/^\xE4$/", "\xE4"), true)"#, m("/^\u{e4}$/"), Stable),
        row("preg P12 byte range", r#"var_export(preg_match("/^[\xE0-\xEF]$/", "\xE4"), true)"#, m("/^[\u{e0}-\u{ef}]$/"), Stable),
        row("preg S15 quoted", r#"var_export(preg_match("/^\\Q\xE4\\E$/", "\xE4"), true)"#, m("/^\\Q\u{e4}\\E$/"), Stable),
        row("preg P13 ascii range", r#"var_export(preg_match('/^[a-z]$/', "\xE4"), true)"#, m("/^[a-z]$/"), Stable),
        row("preg S12 property", r#"var_export(preg_match('/^\p{L}$/', "\xE4"), true)"#, m(r"/^\p{L}$/"), Stable),
        row("preg S13 property under u", r#"var_export(preg_match('/^\p{L}$/u', "\xC3\xA4"), true)"#, m(r"/^\p{L}$/u"), Stable),
        row("preg P25 h", r#"var_export(preg_match('/^\h$/', "\xA0"), true)"#, m(r"/^\h$/"), Stable),
        row("preg P26 v", r#"var_export(preg_match('/^\v$/', "\x85"), true)"#, m(r"/^\v$/"), Stable),
        row("preg P17 dot", r#"var_export(preg_match('/^.$/', "\xE4"), true)"#, m("/^.$/"), Stable),
        row("preg S11 x ascii", r#"var_export(preg_match('/^a b$/x', 'ab'), true)"#, m("/^a b$/x"), Stable),
        // Caseless matching under `u` compares through the table too: no macOS witness moved
        // (glibc's `tr_TR` case map is the evidence, `pcre2_match.c:1052-1056`), the rule reads
        // wherever a letter can match, and nothing where none can.
        row("preg D1 iu", r#"var_export(preg_match('/^i$/iu', "\xC3\x9D"), true)"#, m("/^i$/iu"), ReadsByRule),
        row("preg D4 iu range", r#"var_export(preg_match('/^[a-z]$/iu', "\xC3\x9D"), true)"#, m("/^[a-z]$/iu"), ReadsByRule),
        row("preg F1 i spanning range", r#"var_export(preg_match('/^[!-~]$/i', "\xFD"), true)"#, m("/^[!-~]$/i"), ReadsByRule),
        row("preg i dot", r#"var_export(preg_match('/^.$/i', "\xFD"), true)"#, m("/^.$/i"), ReadsByRule),
        row("preg K1 i digits", r#"var_export(preg_match('/^[0-9]$/i', "\xB2"), true)"#, m("/^[0-9]$/i"), Stable),
        row("preg i digits under u", r#"var_export(preg_match('/^[0-9]$/iu', "\xC2\xB2"), true)"#, m("/^[0-9]$/iu"), Stable),
        // A named back reference is compared folded in every spelling.
        row("preg A named backref", r#"var_export(preg_match('/^(?<a>.)(?P=a)$/i', "\xC4\xE4"), true)"#, m("/^(?<a>.)(?P=a)$/i"), Moves),
        row("preg A2 k backref", r#"var_export(preg_match('/^(?<a>.)\k<a>$/i', "\xC4\xE4"), true)"#, m(r"/^(?<a>.)\k<a>$/i"), Moves),
        row("preg K5 numbered backref", r#"var_export(preg_match('/^(.)\1$/i', "\xC4\xE4"), true)"#, m(r"/^(.)\1$/i"), Moves),
        // An `x` comment hides its line and nothing after it: a `\Q` or `(?#` in the comment
        // swallows no token past the newline.
        row("preg B x comment quote", r#"var_export(preg_match("/^#\\Q\n\\w$/x", "\xE4"), true)"#, m("/^#\\Q\n\\w$/x"), Moves),
        row("preg B2 x comment group", r#"var_export(preg_match("/^#(?#\n\\w(a)?$/x", "\xE4"), true)"#, m("/^#(?#\n\\w(a)?$/x"), Moves),
        row("preg x comment holds the reader", r#"var_export(preg_match("/^a #\\w\nb$/x", "ab"), true)"#, m("/^a #\\w\nb$/x"), Stable),
        row("preg J1 x comment high byte", r#"var_export(preg_match("/^a#\xA0\nb$/x", "ab"), true)"#, m("/^a#\u{a0}\nb$/x"), Stable),
        // `[[:<:]]` and `[[:>:]]` are `\b` in disguise.
        row("preg C word start", r#"var_export(preg_match('/[[:<:]]\xE4/', " \xE4"), true)"#, m(r"/[[:<:]]\xE4/"), Moves),
        row("preg C2 word end", r#"var_export(preg_match('/\xE4[[:>:]]/', "\xE4 "), true)"#, m(r"/\xE4[[:>:]]/"), Moves),
        // UCP keeps `[:ascii:]` on the table, and a name above ASCII without UTF.
        row("preg E4 ascii under u", r#"var_export(preg_match('/^[[:ascii:]]$/u', "\xC3\xA4"), true)"#, m("/^[[:ascii:]]$/u"), Moves),
        row("preg E4b negated ascii under u", r#"var_export(preg_match('/^[[:^ascii:]]$/u', "\xC3\xA4"), true)"#, m("/^[[:^ascii:]]$/u"), Moves),
        row("preg R6 UCP name", r#"var_export(preg_match("/(*UCP)(?<\xE4>a)/", 'a'), true)"#, m("/(*UCP)(?<\u{e4}>a)/"), Moves),
        row("preg R7 u name", r#"var_export(preg_match("/(?<\xC3\xA4>a)/u", 'a'), true)"#, m("/(?<\u{e4}>a)/u"), Stable),
        // The `r` modifier is a valid flag, and `\w` under it reads as ever (PHP 8.4 and later).
        row("preg r flag", r#"PHP_VERSION_ID < 80400 ? "skip" : var_export(preg_match('/^\w$/r', "\xE4"), true)"#, m(r"/^\w$/r"), Moves),
        // `preg_quote` compiles nothing.
        row("preg P27 quote", r#"bin2hex(preg_quote("\xE4."))"#, Reads("preg_quote"), Stable),
    ]
}

/// The rows of what a call does not reach, and of the names that read no setting.
fn silent_rows() -> Vec<Row> {
    use Expect::{Moves, MovesOnMacos, Stable};
    use GateArg::{Bool, Int, NotText, Str};
    use Verdict::{Gate, Narrowed, NoSetting};
    vec![
        // What a call does not reach: the numeric conversions of `strftime`, a `ctype_*` argument
        // that is no string and no small integer, an empty operand, a string with no `<`.
        row(
            "strftime, numeric conversions",
            r#"@strftime("%Y-%m-%d %H:%M:%S", 86400 * 40)"#,
            Gate("strftime", &[Some(Str("%Y-%m-%d %H:%M:%S"))]),
            Stable,
        ),
        row("strftime, %s", r#"@strftime("%s", 86400 * 40)"#, Gate("strftime", &[Some(Str("%s"))]), Stable),
        row("ctype_alpha, int 228", r#"var_export(ctype_alpha(228), true)"#, Gate("ctype_alpha", &[Some(Int(228))]), Moves),
        row("ctype_alpha, int 300", r#"var_export(ctype_alpha(300), true)"#, Gate("ctype_alpha", &[Some(Int(300))]), Stable),
        row("ctype_alpha, empty string", r#"var_export(ctype_alpha(""), true)"#, Gate("ctype_alpha", &[Some(Str(""))]), Stable),
        row("ctype_alpha, bool", r#"var_export(ctype_alpha(true), true)"#, Gate("ctype_alpha", &[Some(Bool(true))]), Stable),
        row("ctype_alpha, float", r#"var_export(ctype_alpha(228.0), true)"#, Gate("ctype_alpha", &[Some(NotText)]), Stable),
        row("ctype_alpha, null", r#"var_export(ctype_alpha(null), true)"#, Gate("ctype_alpha", &[Some(NotText)]), Stable),
        row("ctype_alpha, array", r#"var_export(ctype_alpha([228]), true)"#, Gate("ctype_alpha", &[Some(NotText)]), Stable),
        row("ctype_alpha, object", r#"var_export(ctype_alpha(new stdClass), true)"#, Gate("ctype_alpha", &[Some(NotText)]), Stable),
        row("ctype_alpha, int -28", r#"var_export(ctype_alpha(-28), true)"#, Gate("ctype_alpha", &[Some(Int(-28))]), Moves),
        row("ctype_alpha, int -129", r#"var_export(ctype_alpha(-129), true)"#, Gate("ctype_alpha", &[Some(Int(-129))]), Stable),
        row(
            "strnatcmp, an empty operand",
            r#"strnatcmp("\xA0", "")"#,
            Gate("strnatcmp", &[Some(Str("\u{a0}")), Some(Str(""))]),
            Stable,
        ),
        row(
            "strip_tags, no tag",
            r#"strip_tags("a\xA0 b > c")"#,
            Gate("strip_tags", &[Some(Str("a\u{a0} b > c"))]),
            Stable,
        ),
        row("escapeshellarg, empty", r#"bin2hex(escapeshellarg(""))"#, Gate("escapeshellarg", &[Some(Str(""))]), Stable),
        row("parse_url, empty", r#"bin2hex(serialize(parse_url("")))"#, Gate("parse_url", &[Some(Str(""))]), Stable),
        row(
            "parse_url, a plain path",
            r#"bin2hex(serialize(parse_url("\x80")))"#,
            Gate("parse_url", &[Some(Str("\u{80}"))]),
            MovesOnMacos,
        ),
        // The query form reads the cell.
        row("setlocale, '0'", r#"setlocale(LC_ALL, "0")"#, Narrowed("setlocale", "0"), Moves),
        // Names that read no setting keep moving nowhere.
        row("ucfirst", r#"PHP_VERSION_ID < 80200 ? "skip" : bin2hex(ucfirst("\xE4a"))"#, NoSetting("ucfirst"), Stable),
        row("trim", r#"bin2hex(trim("\xA0a\xA0"))"#, NoSetting("trim"), Stable),
        row("str_contains", r#"var_export(str_contains("\xE4a", "\xC4"), true)"#, NoSetting("str_contains"), Stable),
        row("json_encode", r#"json_encode([1.5, 0.1 + 0.2, 1e25])"#, NoSetting("json_encode"), Stable),
    ]
}

/// The PHP that evaluates every probe under `C` and under each witness locale and prints one
/// line per probe: its label, a tab and one digit per witness (`1` moved, `0` did not, `-` the
/// locale is missing).
fn script(rows: &[Row]) -> String {
    let mut probes = String::new();
    for r in rows {
        probes.push_str(&format!("{:?} => static function () {{ return {}; }},\n", r.label, r.probe));
    }
    format!(
        r#"
        error_reporting(0);
        $latin1 = null;
        foreach (['de_DE.ISO8859-1', 'de_DE.ISO-8859-1', 'de_DE.iso88591'] as $l) {{
            if (setlocale(LC_ALL, $l) !== false) {{ $latin1 = $l; break; }}
        }}
        if ($latin1 === null || setlocale(LC_ALL, 'de_DE.UTF-8') === false) {{ echo "NOLOCALE\n"; exit; }}
        $extra = setlocale(LC_ALL, 'ja_JP.eucJP') !== false ? 'ja_JP.eucJP' : null;
        echo "PHPVERSION\t", PHP_VERSION_ID, "\n";
        $probes = [
{probes}        ];
        $run = function (string $locale, callable $f): string {{
            setlocale(LC_ALL, $locale);
            try {{ return (string) $f(); }} catch (\Throwable $e) {{ return 'ERR'; }}
        }};
        foreach ($probes as $label => $f) {{
            $c = $run('C', $f);
            $bits = '';
            foreach (['de_DE.UTF-8', $latin1, $extra] as $w) {{
                $bits .= $w === null ? '-' : ($run($w, $f) !== $c ? '1' : '0');
            }}
            echo $label, "\t", $bits, "\n";
        }}
    "#
    )
}

/// Run `script` under php: its stdout, or `None` (a skip) without php or the locales.
fn run_php(script: &str) -> Option<String> {
    if Command::new("php").arg("--version").output().is_err() {
        oracle_unavailable("php is not on PATH");
        return None;
    }
    let out = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", script])
        .stdout(Stdio::piped())
        .output()
        .expect("run php");
    assert!(out.status.success(), "php failed");
    let text = String::from_utf8(out.stdout).expect("utf8");
    if text.starts_with("NOLOCALE") {
        oracle_unavailable("de_DE.UTF-8 or a de_DE Latin-1 locale is not installed");
        return None;
    }
    Some(text)
}

/// Each reader the catalog colours moves between `C` and a witness locale, and nothing it
/// certifies, gates away or leaves out does.
#[test]
fn the_locale_readers_move_exactly_where_the_catalog_says() {
    let mut rows = rows();
    rows.extend(silent_rows());
    rows.extend(preg_rows());
    let Some(text) = run_php(&script(&rows)) else { return };
    let mut lines = text.lines().filter_map(|l| l.split_once('\t'));
    let version: u32 = match lines.next() {
        Some(("PHPVERSION", id)) => id.parse().expect("a version id"),
        other => panic!("no version line: {other:?}"),
    };
    // The catalog follows the 8.5 pin: before 8.2 the case folding of the data sorts and of
    // `ucfirst` differs, and the 8.1 sources of `strip_tags` and `parse_url` hand `isspace` and
    // `isalpha` a signed `char`, which no probe here moves. An older engine is held to the
    // soundness direction and to the rows that stay stable, not to the positive one.
    let modern = version >= 80200;
    let observed: Vec<(&str, &str)> = lines.collect();
    assert_eq!(observed.len(), rows.len(), "{text}");
    for (r, (label, bits)) in rows.iter().zip(observed) {
        assert_eq!(r.label, label);
        let bits = bits.as_bytes();
        let standard = bits[..2].contains(&b'1');
        let anywhere = bits.contains(&b'1');
        let verdict = r.verdict.reads();
        let (claimed, may_read) = (verdict == Some(true), verdict != Some(false));
        assert!(may_read || !anywhere, "{label} moved under a locale and the catalog calls it silent");
        match r.expect {
            Expect::Moves => {
                assert!(claimed, "{label}: a row that must move is not coloured");
                assert!(
                    standard || !modern,
                    "{label} is coloured and did not move under de_DE or Latin-1"
                );
            }
            Expect::MovesOnMacos => {
                assert!(claimed, "{label}: a row that moves on macOS is not coloured");
                if cfg!(target_os = "macos") && modern {
                    assert!(anywhere, "{label} is coloured and did not move on macOS");
                }
            }
            Expect::Stable => {
                assert_eq!(verdict, Some(false), "{label} did not move and the catalog colours it");
                assert!(!anywhere, "{label} moved");
            }
            Expect::Undecided => {
                assert_eq!(verdict, None, "{label} is a version-dependent read");
                assert!(!anywhere, "{label} moved");
            }
            Expect::ReadsByRule => {
                assert!(claimed, "{label}: a row the rule reads is not coloured");
            }
        }
    }
}

/// `setlocale($c, '0')` queries: it answers the current locale and changes nothing, so the
/// narrowing to the read alone (D6) is the whole effect; `"0\0x"` is not the query and does not
/// answer the locale.
#[test]
fn a_zero_locale_queries_the_cell_and_writes_nothing() {
    let script = r#"
        error_reporting(0);
        if (setlocale(LC_ALL, 'de_DE.UTF-8') === false) { echo "NOLOCALE\n"; exit; }
        $first = setlocale(LC_ALL, '0');
        $second = setlocale(LC_ALL, '0');
        $numeric = setlocale(LC_NUMERIC, '0');
        $time = setlocale(LC_TIME, '0');
        echo $first === 'de_DE.UTF-8' && $first === $second && $numeric === $first && $time === $first ? 'same' : 'moved', "\n";
        echo var_export(setlocale(LC_ALL, "0\0x"), true), "\n";
        echo setlocale(LC_ALL, '0'), "\n";
        setlocale(LC_ALL, 'C');
        echo setlocale(LC_ALL, '0'), "\n";
    "#;
    let Some(text) = run_php(script) else { return };
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines, ["same", "false", "de_DE.UTF-8", "C"], "{text}");
}
