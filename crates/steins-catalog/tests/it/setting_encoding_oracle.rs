//! The S6d witness table of ADR-0101 §3.13 against the engine: each encoding reader the catalog
//! colours moves when the encoding cell does, each call it drops or leaves undecided is checked
//! the same way, and each writer's call moves the readers after it (issue #1000).
//!
//! A row is a PHP expression that returns a string and a `setup` that writes the cell: the
//! expression runs twice, bare and after the setup, and the outputs are compared. The setups are
//! the ones the catalog colours as writes (`ini_set` of a name the cell owns, an accessor given a
//! value), so a row that moves also checks the write it uses. A row asserts the catalog's verdict
//! in both directions, as `locale_readers_oracle` does:
//!
//! * **soundness**: a probe that moved is a call the catalog proves a read or leaves undecided,
//!   never one it drops;
//! * **precision**: a call the catalog proves a read moved.
//!
//! Every function the table probes exists on both PHP minors the CI matrix carries (8.4 and
//! 8.5). Like the other oracles, the test skips loudly without `php` (or without its `mbstring`
//! and `iconv` extensions) unless `CI` is set, where a missing one fails.

use std::process::{Command, Stdio};

use steins_catalog::{GateArg, SettingCell, effect_labels, ini_cell, setting_read_gate};

use super::locale_oracle::oracle_unavailable;

const READ: &str = "global.read.setting.encoding";
const WRITE: &str = "global.write.setting.encoding";

/// What the catalog says of the call a row probes.
#[derive(Clone, Copy)]
enum Verdict {
    /// A gated reader: the call as written, by its deciding arguments.
    Gate(&'static str, &'static [Option<GateArg<'static>>]),
    /// A reader with no gate: its row carries the read on every call.
    Row(&'static str),
    /// The setup's call is a write of the cell: the name and the arguments shown.
    Writes(&'static str, &'static [Option<GateArg<'static>>]),
    /// The ini name the setup's `ini_set` spells is the cell's.
    IniName(&'static str),
    /// The ini name the setup's `ini_set` spells is no cell's: it drops, and nothing may move.
    Unmapped(&'static str),
}

/// What the probe is expected to do between the bare run and the run after the setup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Moves {
    Yes,
    No,
    /// Not asserted: the engine's answer depends on the C library (glibc moves, macOS does not).
    Maybe,
}

struct Row {
    label: &'static str,
    setup: &'static str,
    probe: &'static str,
    verdict: Verdict,
    moves: Moves,
}

const fn row(
    label: &'static str,
    setup: &'static str,
    probe: &'static str,
    verdict: Verdict,
    moves: Moves,
) -> Row {
    Row { label, setup, probe, verdict, moves }
}

use GateArg::{Null, Omitted, Str};
use Moves::{Maybe, No, Yes};
use Verdict::{Gate, IniName, Row as Ungated, Unmapped, Writes};

/// `default_charset` to Latin-1, the write every `mb_*`, `iconv_*` and HTML reader follows.
const CHARSET: &str = "ini_set('default_charset', 'ISO-8859-1');";

const ROWS: &[Row] = &[
    // The plain mb_* class: an omitted or null encoding follows the cell, a name does not.
    row(
        "N1 mb_strlen omitted",
        CHARSET,
        r#"mb_strlen("\xC3\xA4")"#,
        Gate("mb_strlen", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N1b mb_strlen null",
        CHARSET,
        r#"mb_strlen("\xC3\xA4", null)"#,
        Gate("mb_strlen", &[Some(Null)]),
        Yes,
    ),
    row(
        "N2 mb_strlen named",
        CHARSET,
        r#"mb_strlen("\xC3\xA4", 'UTF-8')"#,
        Gate("mb_strlen", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "N3 mb_strpos omitted",
        CHARSET,
        r#"var_export(mb_strpos("\xC3\xA4b", 'b'), true)"#,
        Gate("mb_strpos", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N3b mb_strpos named at position 3",
        CHARSET,
        r#"var_export(mb_strpos("\xC3\xA4b", 'b', 0, 'UTF-8'), true)"#,
        Gate("mb_strpos", &[Some(Str("UTF-8"))]),
        No,
    ),
    // The substituting class: omitted follows the cell; a name is undecided because the
    // substitution character, which the cell holds, is read on an invalid input.
    row(
        "N4 mb_strtoupper omitted",
        CHARSET,
        r#"bin2hex(mb_strtoupper("\xC3\xA4"))"#,
        Gate("mb_strtoupper", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N4b mb_strtoupper named, valid input",
        CHARSET,
        r#"bin2hex(mb_strtoupper("\xC3\xA4", 'UTF-8'))"#,
        Gate("mb_strtoupper", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "N4c mb_strtoupper named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_strtoupper("a\xFFb", 'UTF-8'))"#,
        Gate("mb_strtoupper", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "N4d mb_substr named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_substr("a\xFFb", 1, 1, 'UTF-8'))"#,
        Gate("mb_substr", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "N4e mb_convert_encoding from omitted",
        CHARSET,
        r#"bin2hex(mb_convert_encoding("\xC3\xA4", 'UTF-16BE'))"#,
        Gate("mb_convert_encoding", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N4f mb_convert_encoding from named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_convert_encoding("a\xFFb", 'UTF-16BE', 'UTF-8'))"#,
        Gate("mb_convert_encoding", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    // The HTML functions: the empty string names the default; an empty subject returns first.
    row(
        "N8 htmlspecialchars omitted",
        CHARSET,
        r#"bin2hex(htmlspecialchars("<\xE4>"))"#,
        Gate("htmlspecialchars", &[Some(Str("<\u{e4}>")), Some(Omitted)]),
        Yes,
    ),
    row(
        "N9 htmlspecialchars named",
        CHARSET,
        r#"bin2hex(htmlspecialchars("<\xE4>", ENT_QUOTES, 'UTF-8'))"#,
        Gate("htmlspecialchars", &[Some(Str("<\u{e4}>")), Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "N9b htmlspecialchars empty name",
        CHARSET,
        r#"bin2hex(htmlspecialchars("<\xE4>", ENT_QUOTES, ''))"#,
        Gate("htmlspecialchars", &[Some(Str("<\u{e4}>")), Some(Str(""))]),
        Yes,
    ),
    row(
        "N9c htmlspecialchars empty subject",
        CHARSET,
        r#"bin2hex(htmlspecialchars(""))"#,
        Gate("htmlspecialchars", &[Some(Str("")), Some(Omitted)]),
        No,
    ),
    row(
        "N10 htmlentities omitted",
        CHARSET,
        r#"bin2hex(htmlentities("\xE4"))"#,
        Gate("htmlentities", &[Some(Str("\u{e4}")), Some(Omitted)]),
        Yes,
    ),
    row(
        "N10b html_entity_decode omitted",
        CHARSET,
        r#"bin2hex(html_entity_decode('&euro;'))"#,
        Gate("html_entity_decode", &[Some(Str("&euro;")), Some(Omitted)]),
        Yes,
    ),
    row(
        "N10c html_entity_decode without an ampersand",
        CHARSET,
        r#"bin2hex(html_entity_decode('euro'))"#,
        Gate("html_entity_decode", &[Some(Str("euro")), Some(Omitted)]),
        No,
    ),
    row(
        "N10d get_html_translation_table omitted",
        CHARSET,
        r#"count(get_html_translation_table(HTML_ENTITIES))"#,
        Gate("get_html_translation_table", &[Some(Omitted)]),
        Yes,
    ),
    // The iconv readers.
    row(
        "N12 iconv_strlen omitted",
        CHARSET,
        r#"iconv_strlen("\xC3\xA4")"#,
        Gate("iconv_strlen", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N13 iconv_strlen named",
        CHARSET,
        r#"iconv_strlen("\xC3\xA4", 'UTF-8')"#,
        Gate("iconv_strlen", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "N13b iconv_strrpos empty needle",
        CHARSET,
        r#"var_export(iconv_strrpos("\xC3\xA4", ''), true)"#,
        Gate("iconv_strrpos", &[Some(Str("")), Some(Omitted)]),
        No,
    ),
    // The accessors: no argument reads, an argument writes.
    row(
        "N6 mb_internal_encoding()",
        CHARSET,
        "mb_internal_encoding()",
        Gate("mb_internal_encoding", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N6b mb_internal_encoding('..') writes",
        "mb_internal_encoding('ISO-8859-1');",
        r#"mb_strlen("\xC3\xA4")"#,
        Writes("mb_internal_encoding", &[Some(Str("ISO-8859-1"))]),
        Yes,
    ),
    row(
        "N6c mb_http_output()",
        CHARSET,
        "mb_http_output()",
        Gate("mb_http_output", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N6d mb_detect_order()",
        "mb_detect_order(['ASCII']);",
        "implode(',', mb_detect_order())",
        Gate("mb_detect_order", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N6e mb_detect_order([..]) writes",
        "mb_detect_order(['ASCII']);",
        "implode(',', mb_detect_order())",
        Writes("mb_detect_order", &[Some(GateArg::NotText)]),
        Yes,
    ),
    row(
        "N6f mb_language()",
        "ini_set('mbstring.language', 'Japanese');",
        "mb_language()",
        Gate("mb_language", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N6g mb_substitute_character()",
        "mb_substitute_character(0x41);",
        "var_export(mb_substitute_character(), true)",
        Gate("mb_substitute_character", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "N6h mb_substitute_character(..) writes",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_scrub("a\xFFb", 'UTF-8'))"#,
        Writes("mb_substitute_character", &[Some(GateArg::Int(0x41))]),
        Yes,
    ),
    // The mb-regex state is the cell's: the ini names that reset it, the encoding accessor,
    // and the options accessor move every function that compiles a pattern.
    row(
        "R1 mb_regex_encoding()",
        CHARSET,
        "mb_regex_encoding()",
        Gate("mb_regex_encoding", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "R1b mb_regex_encoding('..') writes",
        "mb_regex_encoding('ISO-8859-1');",
        r#"var_export(mb_ereg('^.b$', "\xC3\xA4b"), true)"#,
        Writes("mb_regex_encoding", &[Some(Str("ISO-8859-1"))]),
        Yes,
    ),
    row(
        "R2 mb_ereg",
        CHARSET,
        r#"var_export(mb_ereg('^.b$', "\xC3\xA4b"), true)"#,
        Ungated("mb_ereg"),
        Yes,
    ),
    row(
        "R3 mb_eregi",
        CHARSET,
        r#"var_export(mb_eregi('^.B$', "\xC3\xA4b"), true)"#,
        Ungated("mb_eregi"),
        Yes,
    ),
    row(
        "R4 mb_ereg_replace",
        CHARSET,
        r#"bin2hex(mb_ereg_replace('^.', 'x', "\xC3\xA4b"))"#,
        Ungated("mb_ereg_replace"),
        Yes,
    ),
    row(
        "R5 mb_eregi_replace",
        CHARSET,
        r#"bin2hex(mb_eregi_replace('^.', 'x', "\xC3\xA4b"))"#,
        Ungated("mb_eregi_replace"),
        Yes,
    ),
    row(
        "R6 mb_ereg_match",
        CHARSET,
        r#"var_export(mb_ereg_match('.b$', "\xC3\xA4b"), true)"#,
        Ungated("mb_ereg_match"),
        Yes,
    ),
    row(
        "R7 mb_split",
        CHARSET,
        r#"count(mb_split('(?=b)', "\xC3\xA4b")) . '|' . count(mb_split('.', "\xC3\xA4b"))"#,
        Ungated("mb_split"),
        Yes,
    ),
    row(
        "R8 mb_regex_set_options() reads",
        "mb_regex_set_options('x');",
        "mb_regex_set_options()",
        Gate("mb_regex_set_options", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "R8b mb_regex_set_options('..') writes",
        "mb_regex_set_options('x');",
        r#"var_export(mb_ereg('a b', 'ab'), true)"#,
        Writes("mb_regex_set_options", &[Some(Str("x"))]),
        Yes,
    ),
    // iconv's own entries.
    row(
        "I1 iconv_get_encoding('internal_encoding')",
        CHARSET,
        "iconv_get_encoding('internal_encoding')",
        Gate("iconv_get_encoding", &[Some(Str("internal_encoding"))]),
        Yes,
    ),
    row(
        "I1b iconv_get_encoding('no_such_type')",
        CHARSET,
        "var_export(iconv_get_encoding('no_such_type'), true)",
        Gate("iconv_get_encoding", &[Some(Str("no_such_type"))]),
        No,
    ),
    row(
        "I2 iconv_set_encoding writes",
        "iconv_set_encoding('internal_encoding', 'ISO-8859-1');",
        "iconv_get_encoding('internal_encoding')",
        Writes("iconv_set_encoding", &[]),
        Yes,
    ),
    // The five ini names that reset the mb-regex encoding, which S6-core left unmapped.
    row(
        "C1 default_charset",
        CHARSET,
        r#"mb_strlen("\xC3\xA4")"#,
        IniName("default_charset"),
        Yes,
    ),
    row(
        "C2 internal_encoding",
        "ini_set('internal_encoding', 'ISO-8859-1');",
        r#"mb_strlen("\xC3\xA4")"#,
        IniName("internal_encoding"),
        Yes,
    ),
    row(
        "C3 mbstring.internal_encoding",
        "ini_set('mbstring.internal_encoding', 'ISO-8859-1');",
        r#"mb_strlen("\xC3\xA4")"#,
        IniName("mbstring.internal_encoding"),
        Yes,
    ),
    row(
        "C4 input_encoding",
        "ini_set('input_encoding', 'ISO-8859-1');",
        "iconv_get_encoding('input_encoding')",
        IniName("input_encoding"),
        Yes,
    ),
    row(
        "C5 output_encoding",
        "ini_set('output_encoding', 'ISO-8859-1');",
        "mb_http_output()",
        IniName("output_encoding"),
        Yes,
    ),
    row(
        "C6 the regex encoding follows default_charset",
        CHARSET,
        "mb_regex_encoding()",
        IniName("default_charset"),
        Yes,
    ),
    // A literal name on an invalid subject, after the substitution character changes: the plain
    // class stays still (the "no read" verdict), the substituting class moves (the "undecided" one).
    row(
        "P1 mb_check_encoding named, invalid input",
        "mb_substitute_character(0x41);",
        r#"var_export(mb_check_encoding("a\xFFb", 'UTF-8'), true)"#,
        Gate("mb_check_encoding", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P2 mb_chr named, invalid input",
        "mb_substitute_character(0x41);",
        r#"var_export(mb_chr(0xD800, 'UTF-8'), true)"#,
        Gate("mb_chr", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P3 mb_ord named, invalid input",
        "mb_substitute_character(0x41);",
        r#"var_export(mb_ord("\xFF", 'UTF-8'), true)"#,
        Gate("mb_ord", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P4 mb_strcut named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_strcut("a\xFFb", 0, 3, 'UTF-8'))"#,
        Gate("mb_strcut", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P5 mb_stripos named, invalid input",
        "mb_substitute_character(0x41);",
        r#"var_export(mb_stripos("a\xFFb", 'B', 0, 'UTF-8'), true)"#,
        Gate("mb_stripos", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P6 mb_strlen named, invalid input",
        "mb_substitute_character(0x41);",
        r#"mb_strlen("a\xFFb", 'UTF-8')"#,
        Gate("mb_strlen", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P7 mb_strpos named, invalid input",
        "mb_substitute_character(0x41);",
        r#"var_export(mb_strpos("a\xFFb", 'b', 0, 'UTF-8'), true)"#,
        Gate("mb_strpos", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P8 mb_strripos named, invalid input",
        "mb_substitute_character(0x41);",
        r#"var_export(mb_strripos("a\xFFb", 'B', 0, 'UTF-8'), true)"#,
        Gate("mb_strripos", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P9 mb_strrpos named, invalid input",
        "mb_substitute_character(0x41);",
        r#"var_export(mb_strrpos("a\xFFb", 'b', 0, 'UTF-8'), true)"#,
        Gate("mb_strrpos", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P10 mb_strwidth named, invalid input",
        "mb_substitute_character(0x41);",
        r#"mb_strwidth("a\xFFb", 'UTF-8')"#,
        Gate("mb_strwidth", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "P11 mb_substr_count named, invalid input",
        "mb_substitute_character(0x41);",
        r#"mb_substr_count("a\xFFb", 'b', 'UTF-8')"#,
        Gate("mb_substr_count", &[Some(Str("UTF-8"))]),
        No,
    ),
    row(
        "S12 mb_scrub named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_scrub("a\xFFb", 'UTF-8'))"#,
        Gate("mb_scrub", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S13 mb_convert_case named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_convert_case("a\xFFb", MB_CASE_UPPER, 'UTF-8'))"#,
        Gate("mb_convert_case", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S14 mb_strtolower named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_strtolower("a\xFFb", 'UTF-8'))"#,
        Gate("mb_strtolower", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S15 mb_ucfirst named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_ucfirst("a\xFFb", 'UTF-8'))"#,
        Gate("mb_ucfirst", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S16 mb_trim named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_trim("a\xFFb", "a", 'UTF-8'))"#,
        Gate("mb_trim", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S17 mb_strimwidth named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_strimwidth("a\xFFb", 0, 5, '', 'UTF-8'))"#,
        Gate("mb_strimwidth", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S18 mb_convert_kana named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_convert_kana("a\xFFb", 'KV', 'UTF-8'))"#,
        Gate("mb_convert_kana", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S19 mb_encode_numericentity named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_encode_numericentity("a\xFFb", [0x80, 0x10ffff, 0, 0xffffff], 'UTF-8'))"#,
        Gate("mb_encode_numericentity", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S20 mb_decode_numericentity named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_decode_numericentity("&#228;\xFF", [0x80, 0x10ffff, 0, 0xffffff], 'UTF-8'))"#,
        Gate("mb_decode_numericentity", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "S21 mb_str_split named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(implode(',', mb_str_split("a\xFFb", 1, 'UTF-16BE')))"#,
        Gate("mb_str_split", &[Some(Str("UTF-16BE"))]),
        Yes,
    ),
    row(
        "S22 mb_strstr named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_strstr("a\xC3\xA4b\xE3\x81\x82", 'b', false, 'SJIS'))"#,
        Gate("mb_strstr", &[Some(Str("SJIS"))]),
        Yes,
    ),
    row(
        "S23 mb_strrchr named, invalid input",
        "mb_substitute_character(0x41);",
        r#"bin2hex(mb_strrchr("a\xC3\xA4b\xE3\x81\x82", 'b', false, 'SJIS'))"#,
        Gate("mb_strrchr", &[Some(Str("SJIS"))]),
        Yes,
    ),
    // The illegal-character counter (`MBSTRG(illegalchars)`): the converters add to it and the
    // zero-argument `mb_check_encoding()` reads it, so both are the cell's.
    row(
        "W1 mb_convert_encoding counts illegal characters",
        r#"mb_convert_encoding("a\xFFb", 'UTF-16BE');"#,
        "var_export(mb_check_encoding(), true)",
        Writes("mb_convert_encoding", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "W1b mb_convert_encoding with a named source counts them too",
        r#"mb_convert_encoding("a\xFFb", 'UTF-16BE', 'UTF-8');"#,
        "var_export(mb_check_encoding(), true)",
        Writes("mb_convert_encoding", &[Some(Str("UTF-8"))]),
        Yes,
    ),
    row(
        "W2 mb_scrub counts illegal characters",
        r#"mb_scrub("a\xFFb");"#,
        "var_export(mb_check_encoding(), true)",
        Writes("mb_scrub", &[Some(Omitted)]),
        Yes,
    ),
    row(
        "W3 mb_check_encoding() reads the counter",
        r#"mb_convert_encoding("a\xFFb", 'UTF-16BE');"#,
        "var_export(mb_check_encoding(), true)",
        Gate("mb_check_encoding", &[Some(Omitted)]),
        Yes,
    ),
    // The two regex limits are read by every search and change a result silently.
    row(
        "L1 mbstring.regex_retry_limit",
        "ini_set('mbstring.regex_retry_limit', '1000');",
        r#"var_export(mb_ereg('(a+)+c|x', str_repeat('a', 18) . 'bx'), true)"#,
        IniName("mbstring.regex_retry_limit"),
        Yes,
    ),
    // glibc's `//TRANSLIT` consults the locale; macOS's libiconv does not, so no movement is
    // asserted, and the catalog must not call the read absent.
    row(
        "T1 iconv_mime_decode //TRANSLIT",
        "setlocale(LC_ALL, 'C.UTF-8', 'en_US.UTF-8');",
        r#"bin2hex(iconv_mime_decode('=?UTF-8?B?w6nigqzjgYI=?=', 0, 'ASCII//TRANSLIT'))"#,
        Gate("iconv_mime_decode", &[Some(Str("ASCII//TRANSLIT"))]),
        Maybe,
    ),
    row(
        "T2 iconv_mime_decode_headers //TRANSLIT",
        "setlocale(LC_ALL, 'C.UTF-8', 'en_US.UTF-8');",
        r#"bin2hex(json_encode(iconv_mime_decode_headers("Subject: =?UTF-8?B?w6nigqzjgYI=?=", 0, 'ASCII//TRANSLIT')))"#,
        Gate("iconv_mime_decode_headers", &[Some(Str("ASCII//TRANSLIT"))]),
        Maybe,
    ),
    row(
        "T3 iconv_strlen //IGNORE",
        "setlocale(LC_ALL, 'C.UTF-8', 'en_US.UTF-8');",
        r#"var_export(iconv_strlen("a\xFFb", 'UTF-8//IGNORE'), true)"#,
        Gate("iconv_strlen", &[Some(Str("UTF-8//IGNORE"))]),
        Maybe,
    ),
    // An ini entry no cell owns moves none of the readers above.
    row(
        "C7 mbstring.encoding_translation",
        "ini_set('mbstring.encoding_translation', '1');",
        r#"mb_strlen("\xC3\xA4") . mb_regex_encoding() . var_export(mb_ereg('^.b$', "\xC3\xA4b"), true)"#,
        Unmapped("mbstring.encoding_translation"),
        No,
    ),
];

/// Whether the catalog proves a read (`Some(true)`), proves none (`Some(false)`), or leaves the
/// call undecided (`None`) for a row's verdict. A write verdict is checked on its own and answers
/// `Some(true)` so the row's expectation reads the same way.
fn catalog_reads(verdict: Verdict) -> Option<bool> {
    match verdict {
        Gate(name, args) => {
            let gate = setting_read_gate(name).unwrap_or_else(|| panic!("{name} has no gate"));
            assert_eq!(gate.cell(), SettingCell::Encoding, "{name}");
            assert!(effect_labels(name).is_some_and(|l| l.contains(&READ)), "{name}");
            assert_eq!(gate.positions().len(), args.len(), "{name}");
            gate.reads(args)
        }
        Ungated(name) => {
            assert!(setting_read_gate(name).is_none(), "{name} is gated");
            assert!(effect_labels(name).is_some_and(|l| l.contains(&READ)), "{name}");
            Some(true)
        }
        Writes(name, args) => {
            assert!(effect_labels(name).is_some_and(|l| l.contains(&WRITE)), "{name}");
            if let Some(gate) = setting_read_gate(name) {
                assert!(gate.writes(args), "{name}: the write is dropped");
            }
            Some(true)
        }
        IniName(name) => {
            assert_eq!(ini_cell(name), Some(SettingCell::Encoding), "{name}");
            Some(true)
        }
        Unmapped(name) => {
            assert_eq!(ini_cell(name), None, "{name}");
            Some(false)
        }
    }
}

/// The bare ini the table runs under: every entry the cell reads is set, so a developer's
/// `php.ini` cannot move a row.
const BASE: [&str; 18] = [
    "error_reporting=0",
    "display_errors=0",
    "default_charset=UTF-8",
    "internal_encoding=",
    "input_encoding=",
    "output_encoding=",
    "mbstring.internal_encoding=",
    "mbstring.http_input=",
    "mbstring.http_output=",
    "mbstring.language=neutral",
    "mbstring.detect_order=",
    "mbstring.substitute_character=63",
    "mbstring.strict_detection=0",
    "iconv.internal_encoding=",
    "iconv.input_encoding=",
    "iconv.output_encoding=",
    "mbstring.encoding_translation=0",
    "mbstring.regex_retry_limit=1000000",
];

fn run(script: &str) -> String {
    let mut cmd = Command::new("php");
    for entry in BASE {
        cmd.args(["-d", entry]);
    }
    let out = cmd.args(["-r", script]).stdout(Stdio::piped()).output().expect("run php");
    assert!(out.status.success(), "php failed on {script}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn php_ready() -> bool {
    if Command::new("php").arg("--version").output().is_err() {
        oracle_unavailable("php is not on PATH");
        return false;
    }
    let ready = run("echo extension_loaded('mbstring') && extension_loaded('iconv') ? 'y' : 'n';");
    if ready != "y" {
        oracle_unavailable("php lacks mbstring or iconv");
        return false;
    }
    true
}

/// Every row against the engine, in both directions.
#[test]
fn the_encoding_verdicts_match_the_engine() {
    if !php_ready() {
        return;
    }
    for row in ROWS {
        let bare = run(&format!("echo {};", row.probe));
        let after = run(&format!("{} echo {};", row.setup, row.probe));
        let moved = bare != after;
        let reads = catalog_reads(row.verdict);
        let label = row.label;
        assert!(reads != Some(false) || !moved, "{label}: moved ({bare:?} -> {after:?}), dropped");
        assert!(reads != Some(true) || moved, "{label}: proven and did not move ({bare:?})");
        match row.moves {
            Yes => assert!(moved, "{label}: expected to move, stayed {bare:?}"),
            No => assert!(!moved, "{label}: expected to stay, moved {bare:?} -> {after:?}"),
            Maybe => {}
        }
    }
}

/// The table is not vacuous: it holds every verdict kind, and both outcomes of the soundness and
/// the precision rule.
#[test]
fn the_table_covers_every_verdict_and_outcome() {
    let rows = ROWS;
    let reads: Vec<Option<bool>> = rows.iter().map(|r| catalog_reads(r.verdict)).collect();
    assert!(reads.contains(&Some(true)) && reads.contains(&Some(false)));
    assert!(rows.iter().any(|r| matches!(r.verdict, Writes(..))));
    assert!(rows.iter().any(|r| matches!(r.verdict, Ungated(_))));
    assert!(rows.iter().any(|r| matches!(r.verdict, IniName(_))));
    assert!(rows.iter().any(|r| matches!(r.verdict, Unmapped(_))));
    assert!(rows.iter().any(|r| r.moves == Yes) && rows.iter().any(|r| r.moves == No));
    // A call the catalog drops or leaves undecided has a row that stays still (the drop) and a
    // row on an invalid input that moves (the substitution character).
    assert!(rows.iter().any(|r| r.label.starts_with("N4c") && r.moves == Yes));
}
