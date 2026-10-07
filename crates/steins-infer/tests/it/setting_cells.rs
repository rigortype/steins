//! The ini functions with a literal option name read and write the cell that owns the name
//! (ADR-0101 §3.11, issue #1000, S6-core): `ini_get('precision')` is the precision cell's read,
//! `ini_set('date.timezone', …)` the timezone cell's write, and a name no cell owns, a name the
//! call does not spell, and a call whose arguments are not the function's own keep the coarse
//! `global.read` and `global.write`.
//!
//! S6-core colours no other row: every locale verdict of S1 to S5 reads as it did, which the last
//! tests pin beside the suites of those slices.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

const COARSE_READ: &str = "global.read";
const COARSE_WRITE: &str = "global.write";
const LOCALE_READ: &str = "global.read.setting.locale";

fn summary(src: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == "f")
        .unwrap_or_else(|| panic!("no summary for f in {src}"))
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn row(signature: &str, body: &str) -> EffectSummary {
    summary(&format!("<?php\nfunction f({signature}) {{ {body} }}\n"))
}

/// The call carries exactly `labels`, and the body stays exhaustive.
fn proves(signature: &str, call: &str, labels: &[&str]) {
    let s = row(signature, &format!("return {call};"));
    assert_eq!(s.labels, labels, "{call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// The call carries exactly `labels`, whatever else the body leaves open.
fn labels(signature: &str, call: &str, expected: &[&str]) {
    let s = row(signature, &format!("return {call};"));
    assert_eq!(s.labels, expected, "{call}: {s:?}");
}

const PRECISION: (&str, &str) = ("global.read.setting.precision", "global.write.setting.precision");
const TIMEZONE: (&str, &str) = ("global.read.setting.timezone", "global.write.setting.timezone");
const ENCODING: (&str, &str) = ("global.read.setting.encoding", "global.write.setting.encoding");
const INI: (&str, &str) = ("global.read.setting.ini", "global.write.setting.ini");

/// Every option name the table owns, with the cell php-src shows it feeding.
const NAMES: [(&str, (&str, &str)); 20] = [
    ("precision", PRECISION),
    ("serialize_precision", PRECISION),
    ("date.timezone", TIMEZONE),
    ("default_charset", ENCODING),
    ("internal_encoding", ENCODING),
    ("input_encoding", ENCODING),
    ("output_encoding", ENCODING),
    ("iconv.internal_encoding", ENCODING),
    ("iconv.input_encoding", ENCODING),
    ("iconv.output_encoding", ENCODING),
    ("mbstring.internal_encoding", ENCODING),
    ("mbstring.language", ENCODING),
    ("mbstring.detect_order", ENCODING),
    ("mbstring.http_input", ENCODING),
    ("mbstring.http_output", ENCODING),
    ("mbstring.substitute_character", ENCODING),
    ("mbstring.strict_detection", ENCODING),
    ("bcmath.scale", INI),
    ("include_path", INI),
    ("error_reporting", INI),
];

/// `ini_get` reads the entry on every call, so a literal name is the owning cell's read.
#[test]
fn ini_get_of_a_literal_name_reads_the_cell() {
    for (name, (read, _)) in NAMES {
        proves("", &format!("ini_get('{name}')"), &[read]);
        proves("", &format!("\\ini_get(\"{name}\")"), &[read]);
        proves("", &format!("INI_GET('{name}')"), &[read]);
    }
}

/// `ini_set`, its alias `ini_alter` and `ini_restore` rewrite the entry they name.
#[test]
fn ini_set_alter_and_restore_of_a_literal_name_write_the_cell() {
    for (name, (_, write)) in NAMES {
        proves("", &format!("ini_set('{name}', '1')"), &[write]);
        proves("", &format!("ini_set('{name}', 1)"), &[write]);
        proves("", &format!("ini_alter('{name}', '1')"), &[write]);
        proves("", &format!("ini_restore('{name}')"), &[write]);
    }
    // The value is the call's own business: a variable of a declared scalar type changes no cell.
    proves("string $v", "ini_set('precision', $v)", &[PRECISION.1]);
    proves("int $v", "ini_set('date.timezone', $v)", &[TIMEZONE.1]);
}

/// A statement is the call too: the write is not an expression's value.
#[test]
fn a_discarded_ini_write_is_the_cells_write() {
    let s = row("", "ini_set('serialize_precision', '17'); ini_restore('precision');");
    assert_eq!(s.labels, [PRECISION.1], "{s:?}");
    let s = row("", "ini_set('precision', '3'); return ini_get('include_path');");
    assert_eq!(s.labels, [INI.0, PRECISION.1], "{s:?}");
}

/// A name the call does not spell as a literal is any entry, so the row is the coarse one: a
/// variable, a concatenation, a constant, a call, a named argument and a spread list.
#[test]
fn a_name_the_call_does_not_spell_keeps_the_coarse_row() {
    for call in ["ini_get($n)", "ini_get('pre' . 'cision')", "ini_get(PHP_EOL)", "ini_get(strrev($n))"] {
        labels("string $n", call, &[COARSE_READ]);
    }
    for call in [
        "ini_set($n, '1')",
        "ini_alter($n, '1')",
        "ini_set('pre' . 'cision', '3')",
        "ini_set(\"precision$n\", '3')",
        "ini_set(self::NAME, '3')",
    ] {
        labels("string $n", call, &[COARSE_WRITE]);
    }
    labels("string $n", "ini_restore($n)", &[COARSE_WRITE]);
    labels("", "ini_get(option: 'precision')", &[COARSE_READ]);
    labels("", "ini_set(option: 'precision', value: '3')", &[COARSE_WRITE]);
    labels("array $a", "ini_get(...$a)", &[COARSE_READ]);
    labels("array $a", "ini_set(...$a)", &[COARSE_WRITE]);
}

/// An entry no cell owns stays the coarse row, as does a spelling the engine does not find: the
/// lookup is exact and case-sensitive.
#[test]
fn an_unmapped_name_keeps_the_coarse_row() {
    for name in [
        "display_errors", "memory_limit", "max_execution_time", "pcre.backtrack_limit",
        "mbstring.regex_retry_limit", "mbstring.encoding_translation", "intl.default_locale",
        "date.default_latitude", "no.such.entry", "", "PRECISION", "Date.Timezone",
        "INCLUDE_PATH", " precision",
    ] {
        labels("", &format!("ini_get('{name}')"), &[COARSE_READ]);
        labels("", &format!("ini_set('{name}', '1')"), &[COARSE_WRITE]);
        labels("", &format!("ini_alter('{name}', '1')"), &[COARSE_WRITE]);
        labels("", &format!("ini_restore('{name}')"), &[COARSE_WRITE]);
    }
    // `ini_get_all` is the residue slice's, and `ini_parse_quantity` reads no entry.
    labels("", "ini_get_all('date')", &[]);
}

/// A call whose argument count is not the function's own raises before it touches an entry, so
/// it is no cell's access and keeps the coarse row.
#[test]
fn a_call_of_the_wrong_arity_keeps_the_coarse_row() {
    labels("", "ini_get('precision', 1)", &[COARSE_READ]);
    labels("", "ini_set('precision')", &[COARSE_WRITE]);
    labels("", "ini_set('precision', '3', 1)", &[COARSE_WRITE]);
    labels("", "ini_alter('precision')", &[COARSE_WRITE]);
    labels("", "ini_restore('precision', 1)", &[COARSE_WRITE]);
}

/// A builtin handed over as a callback is called with arguments of the invoker's choosing, so the
/// cell is not named and the coarse row stands.
#[test]
fn an_ini_function_handed_over_as_a_callback_keeps_the_coarse_row() {
    labels("array $a", "array_map('ini_get', $a)", &[COARSE_READ]);
    labels("", "call_user_func('ini_set', 'precision', '3')", &[COARSE_WRITE]);
    labels("", "call_user_func('ini_get', 'precision')", &[COARSE_READ]);
}

/// A read and a write of different cells are not one another: a declared envelope admits the
/// cell it names and the coarse parent, and no other cell (ADR-0101 §2.2).
#[test]
fn an_envelope_admits_the_cell_it_names_and_the_parents() {
    let at = |declared: &str, body: &str| {
        findings(&format!(
            "<?php\n#[\\Steins\\Effect('{declared}')]\nfunction f(): mixed {{ {body} }}\n"
        ))
    };
    let read = "return ini_get('precision');";
    let write = "return ini_set('date.timezone', 'UTC');";
    for declared in ["global.read.setting.precision", "global.read.setting", "global.read", "global"] {
        assert!(at(declared, read).is_empty(), "{declared}: {:#?}", at(declared, read));
    }
    for declared in ["global.write.setting.timezone", "global.write.setting", "global.write", "global"] {
        assert!(at(declared, write).is_empty(), "{declared}: {:#?}", at(declared, write));
    }
    // Another cell's read does not cover it, and neither does the other direction.
    for declared in [
        "global.read.setting.timezone", "global.read.setting.locale", "global.write.setting.precision",
        "global.write",
    ] {
        let d = at(declared, read);
        assert_eq!(d.len(), 1, "{declared}: {d:#?}");
        assert!(d[0].message.contains("global.read.setting.precision"), "{}", d[0].message);
    }
    let d = at("global.write.setting.precision", write);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("global.write.setting.timezone"), "{}", d[0].message);
    // A pure function over a read of a cell reports it, as over any setting read.
    let pure = findings("<?php\n#[\\Steins\\Pure]\nfunction f(): mixed { return ini_get('include_path'); }\n");
    assert_eq!(pure.len(), 1, "{pure:#?}");
    assert!(pure[0].message.contains("global.read.setting.ini"), "{}", pure[0].message);
}

/// What must not move: the locale rows of S1 to S5 read as they did, beside the new cells. The
/// suites of those slices (`locale_cell`, `locale_readers`, `printf_call_site`, `preg_locale`)
/// pin the rest.
#[test]
fn the_locale_verdicts_of_s1_to_s5_are_unchanged() {
    let write = "global.write.setting.locale";
    for call in [
        "sprintf('%f', 1.5)", "localeconv()", "strcoll($s, $s)", "basename($s)",
        "ctype_alpha('a')", "strftime('%A')", "sort($a, SORT_LOCALE_STRING)",
        "preg_match('/\\w/', $s)", "pathinfo($s)", "substr_compare($s, $s, 0, 1, true)",
    ] {
        let s = row("string $s, array $a", &format!("return {call};"));
        assert!(s.labels.iter().any(|l| l == LOCALE_READ), "{call}: {s:?}");
        assert!(s.labels.iter().all(|l| !l.contains("setting.ini")), "{call}: {s:?}");
    }
    for call in [
        "sprintf('%d', 1)", "ctype_alpha('')", "sort($a)", "preg_match('/\\d/', $s)",
        "pathinfo($s, PATHINFO_DIRNAME)", "number_format(1.5, 2)", "strcmp($s, $s)",
    ] {
        let s = row("string $s, array $a", &format!("return {call};"));
        assert!(!s.labels.iter().any(|l| l == LOCALE_READ), "{call}: {s:?}");
    }
    labels("", "setlocale(LC_ALL, 'C')", &[write]);
    labels("", "setlocale(LC_ALL, '0')", &[LOCALE_READ]);
    labels("", "setlocale(LC_ALL, '')", &[COARSE_READ, write]);
    // A reader the call cannot decide is the gap and no label, whatever cell the ini calls name.
    let s = row("string $p, string $s", "return preg_match($p, $s);");
    assert!(s.labels.is_empty() && s.gaps.contains(&"value-dependent-read"), "{s:?}");
    // The other global writers and readers keep their coarse rows.
    labels("", "putenv('A=b')", &[COARSE_WRITE]);
    labels("", "date_default_timezone_set('UTC')", &[COARSE_WRITE]);
    labels("", "getenv('A')", &[COARSE_READ]);
    labels("", "date_default_timezone_get()", &[COARSE_READ]);
    labels("", "mb_regex_encoding('UTF-8')", &[COARSE_WRITE]);
}
