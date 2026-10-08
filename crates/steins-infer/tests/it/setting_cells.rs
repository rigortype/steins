//! The ini functions with a literal option name read and write the cell that owns the name
//! (ADR-0101 §3.11, issue #1000, S6-core): `ini_get('precision')` is the precision cell's read,
//! `ini_set('date.timezone', …)` the timezone cell's read (it returns the old value) and write,
//! and a name no cell owns, a name the call does not spell, and a call whose arguments are not
//! the function's own keep the coarse `global.read` and `global.write`. The value an `ini_set`
//! stores is rendered as a string first, which reads `precision` where it is a float.
//!
//! S6-core colours no other row: every locale verdict of S1 to S5 reads as it did, which the last
//! tests pin beside the suites of those slices.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

const DEPENDS: &str = "value-dependent-read";
const COARSE_READ: &str = "global.read";
const COARSE_WRITE: &str = "global.write";
const LOCALE_READ: &str = "global.read.setting.locale";
const ENV_READ: &str = "global.read.setting.env";
const ENV_WRITE: &str = "global.write.setting.env";

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
const NAMES: [(&str, (&str, &str)); 15] = [
    ("precision", PRECISION),
    ("serialize_precision", PRECISION),
    ("date.timezone", TIMEZONE),
    ("iconv.internal_encoding", ENCODING),
    ("iconv.input_encoding", ENCODING),
    ("iconv.output_encoding", ENCODING),
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

/// `ini_set` and its alias `ini_alter` return the entry's old value (php-src calls
/// `zend_ini_get_value` unconditionally), so they read the cell they rewrite; `ini_restore`
/// returns nothing and only writes.
#[test]
fn ini_set_and_alter_read_and_write_the_cell_and_restore_writes_it() {
    for (name, (read, write)) in NAMES {
        proves("", &format!("ini_set('{name}', '1')"), &[read, write]);
        proves("", &format!("ini_set('{name}', 1)"), &[read, write]);
        proves("", &format!("ini_alter('{name}', '1')"), &[read, write]);
        proves("", &format!("ini_restore('{name}')"), &[write]);
    }
    // A value shown to be no float is the call's own business and reads no other cell.
    proves("string $v", "ini_set('precision', $v)", &[PRECISION.0, PRECISION.1]);
    proves("int $v", "ini_set('date.timezone', $v)", &[TIMEZONE.0, TIMEZONE.1]);
}

/// The value an `ini_set` or `ini_alter` stores is converted to a string first, and a float is
/// rendered through `precision` (`ini_set('include_path', 1234.5678)` stores `1.23E+3` under
/// `precision=3`, witnessed). The three-way rule of ADR-0101 §3.2 over the value: shown a float
/// is the proven `global.read.setting.precision`, shown no float reads nothing, anything else is
/// the `value-dependent-read` gap and no label. The cell's own read and write are there either
/// way.
#[test]
fn the_value_of_an_ini_set_reads_precision_where_it_is_a_float() {
    let cell = [INI.0, INI.1];
    let with_precision = [INI.0, PRECISION.0, INI.1];
    for call in ["ini_set", "ini_alter"] {
        for value in ["1234.5678", "1.5", "(float) $n", "$f"] {
            let s = row("float $f, int $n", &format!("return {call}('include_path', {value});"));
            assert_eq!(s.labels, with_precision, "{call} {value}: {s:?}");
            assert!(!s.gaps.contains(&DEPENDS), "{call} {value}: {s:?}");
        }
        for value in ["'/x'", "7", "null", "true", "$s", "$n", "'a' . $s", "$b"] {
            proves("string $s, int $n, bool $b", &format!("{call}('include_path', {value})"), &cell);
        }
        for value in ["$m", "$n", "$m ?? 0"] {
            let s = row("mixed $m, int|float $n", &format!("return {call}('include_path', {value});"));
            assert_eq!(s.labels, cell, "{call} {value}: {s:?}");
            assert!(s.gaps.contains(&DEPENDS) && !s.exhaustive, "{call} {value}: {s:?}");
        }
    }
    // The cell of `precision` already carries the read, as the old value it returns, so its own
    // value needs no second verdict.
    for value in ["1.5", "$m"] {
        let s = row("mixed $m", &format!("return ini_set('precision', {value});"));
        assert_eq!(s.labels, [PRECISION.0, PRECISION.1], "{value}: {s:?}");
        assert!(!s.gaps.contains(&DEPENDS), "{value}: {s:?}");
    }
    // `ini_get` and `ini_restore` take no value; a name no cell owns keeps the coarse row and
    // decides nothing here.
    proves("mixed $m", "ini_get('include_path')", &[INI.0]);
    labels("mixed $m", "ini_set('display_errors', $m)", &[COARSE_WRITE]);
    let s = row("mixed $m", "return ini_set('display_errors', $m);");
    assert!(!s.gaps.contains(&DEPENDS), "{s:?}");
}

/// A statement is the call too: the write is not an expression's value.
#[test]
fn a_discarded_ini_write_is_the_cells_write() {
    let s = row("", "ini_restore('serialize_precision'); ini_restore('precision');");
    assert_eq!(s.labels, [PRECISION.1], "{s:?}");
    let s = row("", "ini_set('precision', '3'); return ini_get('include_path');");
    assert_eq!(s.labels, [INI.0, PRECISION.0, PRECISION.1], "{s:?}");
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
        // These five also reset the mb-regex encoding, which no cell holds until S6d.
        "default_charset", "internal_encoding", "input_encoding", "output_encoding",
        "mbstring.internal_encoding",
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
/// cell it names and the coarse parents, and no other cell (ADR-0101 §2.2). An `ini_set` also
/// reads the cell, so an envelope that names only the write refuses it.
#[test]
fn an_envelope_admits_the_cell_it_names_and_the_parents() {
    let at = |declared: &str, body: &str| {
        findings(&format!(
            "<?php\n#[\\Steins\\Effect({declared})]\nfunction f(): mixed {{ {body} }}\n"
        ))
    };
    let read = "return ini_get('precision');";
    let set = "return ini_set('date.timezone', 'UTC');";
    let restore = "ini_restore('date.timezone'); return 1;";
    for declared in ["'global.read.setting.precision'", "'global.read.setting'", "'global.read'", "'global'"] {
        assert!(at(declared, read).is_empty(), "{declared}: {:#?}", at(declared, read));
    }
    // The old value is a read: an `ini_set` needs both halves, and `ini_restore` only the write.
    for declared in [
        "'global.read.setting.timezone', 'global.write.setting.timezone'",
        "'global.read.setting', 'global.write.setting'",
        "'global.read', 'global.write'",
        "'global'",
    ] {
        assert!(at(declared, set).is_empty(), "{declared}: {:#?}", at(declared, set));
    }
    for declared in ["'global.write.setting.timezone'", "'global.write.setting'", "'global.write'"] {
        assert!(at(declared, restore).is_empty(), "{declared}: {:#?}", at(declared, restore));
    }
    // The write alone does not cover `ini_set`: it names the cell's read as the excess.
    for declared in ["'global.write.setting.timezone'", "'global.write'"] {
        let d = at(declared, set);
        assert_eq!(d.len(), 1, "{declared}: {d:#?}");
        assert!(d[0].message.contains("global.read.setting.timezone"), "{}", d[0].message);
    }
    // Another cell's read does not cover it, and neither does the other direction.
    for declared in [
        "'global.read.setting.timezone'", "'global.read.setting.locale'",
        "'global.write.setting.precision'", "'global.write'",
    ] {
        let d = at(declared, read);
        assert_eq!(d.len(), 1, "{declared}: {d:#?}");
        assert!(d[0].message.contains("global.read.setting.precision"), "{}", d[0].message);
    }
    let d = at("'global.read.setting.timezone', 'global.write.setting.precision'", set);
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
/// ADR-0101 S6c: `getenv` reads the environment block at every arity and `putenv` writes it. The
/// row is argument-blind, so the count and the name do not narrow it, and a `$_ENV` access is no
/// call: it keeps the coarse superglobal label and never reaches the env cell.
#[test]
fn the_environment_block_is_what_getenv_reads_and_putenv_writes() {
    for call in ["getenv('A')", "getenv()", "getenv('A', true)", "getenv($n)", "getenv(null)"] {
        labels("string $n", call, &[ENV_READ]);
    }
    labels("", "putenv('A=b')", &[ENV_WRITE]);
    labels("string $n", "putenv($n)", &[ENV_WRITE]);
    labels("", "putenv('A')", &[ENV_WRITE]);
    // `$_ENV` is a startup copy; a read of it is no setting read.
    let s = row("", "return $_ENV['A'];");
    assert!(!s.labels.iter().any(|l| l.contains("setting.env")), "{s:?}");
}

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
    labels("", "date_default_timezone_set('UTC')", &[COARSE_WRITE]);
    labels("", "date_default_timezone_get()", &[COARSE_READ]);
    labels("", "mb_regex_encoding('UTF-8')", &[COARSE_WRITE]);
}
