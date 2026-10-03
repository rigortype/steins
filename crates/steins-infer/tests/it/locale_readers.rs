//! The locale readers beyond printf in the effect lane (ADR-0101 §3.9, issue #1000, S4): the
//! `ctype_*` predicates, the byte and path readers, `strftime`, the sort family under the flags
//! that read, `substr_compare` under case-insensitivity, `number_format` certified with no read
//! and `setlocale($c, '0')` narrowed to the read.
//!
//! A name whose read is unconditional for the call as written is a label. A name whose read
//! depends on a mode argument the call does not show is the `value-dependent-read` gap, and a
//! literal mode argument decides it either way: the same three-way rule the printf family
//! follows (`printf_call_site.rs`).

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

const READ: &str = "global.read.setting.locale";
const DEPENDS: &str = "value-dependent-read";

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn row(signature: &str, body: &str) -> EffectSummary {
    summary(&format!("<?php\nfunction f({signature}) {{ {body} }}\n"), "f")
}

/// The call carries exactly `labels`, and the body stays exhaustive.
fn proves(signature: &str, call: &str, labels: &[&str]) {
    let s = row(signature, &format!("return {call};"));
    assert_eq!(s.labels, labels, "{call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// The call carries `labels` and the `value-dependent-read` gap.
fn depends(signature: &str, call: &str, labels: &[&str]) {
    let s = row(signature, &format!("return {call};"));
    assert_eq!(s.labels, labels, "{call}: {s:?}");
    assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    assert!(!s.exhaustive, "{call}: {s:?}");
}

/// The names whose read is unconditional for the call as written: a call to one is the read,
/// whatever its argument holds. Each consults the locale in php-src and moved under a locale
/// other than `C` on PHP 8.5.11 (`catalog/tests/it/locale_oracle.rs` holds the witnesses).
#[test]
fn an_unconditional_locale_reader_carries_the_read() {
    for call in [
        "ctype_alnum($s)",
        "ctype_alpha($s)",
        "ctype_cntrl($s)",
        "ctype_graph($s)",
        "ctype_lower($s)",
        "ctype_print($s)",
        "ctype_punct($s)",
        "ctype_space($s)",
        "ctype_upper($s)",
        "\\ctype_alpha($s)",
        "basename($s)",
        "basename($s, '.php')",
        "strnatcmp($s, 'a1')",
        "strnatcasecmp($s, 'a1')",
        "escapeshellarg($s)",
        "strip_tags($s)",
        "parse_url($s)",
        "parse_url($s, PHP_URL_HOST)",
    ] {
        proves("string $s", call, &[READ]);
    }
}

/// `ctype_digit` and `ctype_xdigit` stay as they were. C11 7.4.1.5 and 7.4.1.12 fix their sets
/// in every locale, and the engine moved on neither in any locale witnessed, so they read no
/// setting that can change an answer; no row claims one and no row certifies them either.
#[test]
fn ctype_digit_and_xdigit_are_left_uncatalogued() {
    for call in ["ctype_digit($s)", "ctype_xdigit($s)"] {
        let s = row("string $s", &format!("return {call};"));
        assert!(s.labels.is_empty(), "{call}: {s:?}");
        assert!(s.gaps.contains(&"no-effect-row"), "{call}: {s:?}");
    }
}

/// `strftime` and `gmstrftime` read the locale's day and month names beside the clock: the
/// time family's argument-blind `nondet.time` (the timezone cell sharpens it later) and the
/// locale read.
#[test]
fn strftime_reads_the_locale_beside_the_clock() {
    for call in ["strftime('%A')", "strftime('%A', $t)", "gmstrftime('%A', $t)", "gmstrftime('%B')"] {
        proves("int $t", call, &[READ, "nondet.time"]);
    }
}

/// `number_format` renders with `%.*F`, which never consults the locale (php-src
/// `_php_math_number_format_ex`; witnessed on 8.1 and 8.5): it is certified at the call site
/// with no read, so a string-typed separator that could be an object still reaches
/// `__toString` and nothing else does.
#[test]
fn number_format_is_certified_with_no_setting_read() {
    for call in ["number_format($x)", "number_format($x, 2)", "number_format($x, 2, ',', '.')"] {
        proves("float $x", call, &[]);
    }
    let s = row("float $x, object $sep", "return number_format($x, 2, $sep, ',');");
    assert!(s.labels.is_empty(), "{s:?}");
    assert!(s.gaps.contains(&"user-code-reach"), "an object separator runs __toString: {s:?}");
    assert!(!s.gaps.contains(&DEPENDS), "{s:?}");
}

/// The sort family reads the locale only under the flags that route the comparison through
/// the C library: `SORT_LOCALE_STRING` (`strcoll`) and `SORT_NATURAL` (`strnatcmp`'s `isspace`,
/// `isdigit`, `toupper`). A literal flag decides; no flag is `SORT_REGULAR`.
#[test]
fn a_sort_reads_the_locale_only_under_a_flag_that_does() {
    for name in ["sort", "rsort", "asort", "arsort", "ksort", "krsort"] {
        for flags in ["", ", SORT_REGULAR", ", SORT_NUMERIC", ", SORT_STRING", ", 2", ", SORT_STRING | SORT_FLAG_CASE | 0"] {
            if name.starts_with('k') && flags.contains("FLAG_CASE") {
                continue;
            }
            let call = format!("{name}($a{flags})");
            let s = row("array $a", &format!("{call};"));
            assert_eq!(s.labels, ["mutate.local"], "{call}: {s:?}");
            assert!(!s.gaps.contains(&DEPENDS), "{call}: {s:?}");
        }
        for flags in [
            "SORT_LOCALE_STRING",
            "SORT_NATURAL",
            "SORT_NATURAL | SORT_FLAG_CASE",
            "SORT_LOCALE_STRING | SORT_FLAG_CASE",
            "5",
            "6",
            "14",
        ] {
            let call = format!("{name}($a, {flags})");
            let s = row("array $a", &format!("{call};"));
            assert_eq!(s.labels, [READ, "mutate.local"], "{call}: {s:?}");
            assert!(!s.gaps.contains(&DEPENDS), "{call}: {s:?}");
        }
    }
}

/// `ksort` and `krsort` compare string keys case-insensitively through `tolower`, a C-library
/// call (`php_array_key_compare_string_case_unstable_i` uses `zend_binary_strcasecmp_l`),
/// where the data sorts use the engine's ASCII folding: only the key sorts read under
/// `SORT_STRING | SORT_FLAG_CASE` (witnessed: `krsort` of `"\xC4"` and `"\xE4"` moves under
/// `de_DE.ISO8859-1`).
#[test]
fn the_key_sorts_read_under_a_case_folding_string_flag() {
    for name in ["ksort", "krsort"] {
        let call = format!("{name}($a, SORT_STRING | SORT_FLAG_CASE)");
        let s = row("array $a", &format!("{call};"));
        assert_eq!(s.labels, [READ, "mutate.local"], "{call}: {s:?}");
    }
    for name in ["sort", "rsort", "asort", "arsort"] {
        let call = format!("{name}($a, SORT_STRING | SORT_FLAG_CASE)");
        let s = row("array $a", &format!("{call};"));
        assert_eq!(s.labels, ["mutate.local"], "{call}: {s:?}");
    }
}

/// A flag the call does not show, or one the scan cannot evaluate, is the gap and never an
/// argument-blind label: `sort($a, $flags)` reads the locale only when `$flags` says so. A named
/// or spread list hides the position, which is the same gap.
#[test]
fn a_sort_flag_the_site_cannot_read_is_a_gap_and_not_a_label() {
    for call in ["sort($a, $flags)", "sort($a, SORT_STRING | $flags)", "sort($a, FLAGS_FROM_SOMEWHERE)"] {
        let s = row("array $a, int $flags", &format!("{call};"));
        assert_eq!(s.labels, ["mutate.local"], "{call}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
        assert!(!s.exhaustive, "{call}: {s:?}");
    }
    for call in ["ksort($a, flags: SORT_STRING)", "sort(...$args)"] {
        let s = row("array $a, array $args", &format!("{call};"));
        assert!(!s.labels.iter().any(|l| l == READ), "{call}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    }
}

/// `substr_compare` folds case through `tolower` only when its fifth argument is true.
#[test]
fn substr_compare_reads_the_locale_only_when_case_insensitive() {
    for call in [
        "substr_compare($s, 'ab', 0)",
        "substr_compare($s, 'ab', 0, null)",
        "substr_compare($s, 'ab', 0, 2)",
        "substr_compare($s, 'ab', 0, 2, false)",
        "substr_compare($s, 'ab', 0, null, FALSE)",
    ] {
        proves("string $s", call, &[]);
    }
    for call in [
        "substr_compare($s, 'ab', 0, 2, true)",
        "substr_compare($s, 'ab', 0, null, true)",
        "substr_compare($s, 'ab', 0, null, TRUE)",
    ] {
        proves("string $s", call, &[READ]);
    }
    for call in [
        "substr_compare($s, 'ab', 0, 2, $ci)",
        "substr_compare($s, 'ab', 0, 2, 1)",
        "substr_compare($s, 'ab', 0, 2, !false)",
        "substr_compare($s, 'ab', 0, case_insensitive: true)",
        "substr_compare($s, ...$rest)",
    ] {
        depends("string $s, bool $ci, array $rest", call, &[]);
    }
}

/// `pathinfo` reads the locale through `php_basename` for the basename, extension and
/// filename parts, and not for the directory name alone.
#[test]
fn pathinfo_reads_the_locale_unless_only_the_directory_is_asked_for() {
    for call in [
        "pathinfo($s)",
        "pathinfo($s, PATHINFO_ALL)",
        "pathinfo($s, PATHINFO_BASENAME)",
        "pathinfo($s, PATHINFO_EXTENSION)",
        "pathinfo($s, PATHINFO_FILENAME)",
        "pathinfo($s, PATHINFO_DIRNAME | PATHINFO_EXTENSION)",
        "pathinfo($s, 15)",
    ] {
        proves("string $s", call, &[READ]);
    }
    for call in ["pathinfo($s, PATHINFO_DIRNAME)", "pathinfo($s, 1)"] {
        proves("string $s", call, &[]);
    }
    for call in ["pathinfo($s, $opt)", "pathinfo($s, PATHINFO_DIRNAME | $opt)", "pathinfo(...$rest)"] {
        depends("string $s, int $opt, array $rest", call, &[]);
    }
}

/// `setlocale($c, '0')` only queries the cell: the read alone, no write (ADR-0101 D6). The
/// query is the exact string `"0"`: php-src compares the whole `zend_string`, so `"0\0x"`
/// is an attempt to set a locale named `0` and keeps the row, as does a call with a fallback
/// locale, an integer and anything the call does not show.
#[test]
fn setlocale_with_a_literal_zero_is_a_read_and_no_write() {
    for call in ["setlocale(LC_ALL, '0')", "setlocale(LC_NUMERIC, \"0\")", "\\setlocale(LC_ALL, '0')"] {
        proves("", call, &[READ]);
    }
    for call in [
        "setlocale(LC_ALL, \"0\\0x\")",
        "setlocale(LC_ALL, '0', 'C')",
        "setlocale(LC_ALL, 0)",
        "setlocale(LC_ALL, $l)",
        "setlocale(LC_ALL, ['0'])",
    ] {
        let s = row("string $l", &format!("return {call};"));
        assert_eq!(s.labels, ["global.read", "global.write.setting.locale"], "{call}: {s:?}");
    }
    // The write of a named locale is unchanged, and the environment read of `''` stays.
    proves("", "setlocale(LC_ALL, 'C')", &["global.write.setting.locale"]);
    let s = row("", "return setlocale(LC_ALL, '');");
    assert_eq!(s.labels, ["global.read", "global.write.setting.locale"], "{s:?}");
}

/// A builtin handed over as a callback is called with arguments the invoker chooses: the
/// unconditional readers keep the read, and the mode-conditional ones are the gap.
#[test]
fn a_reader_handed_over_as_a_callback_follows_the_same_rule() {
    let s = row("array $a", "usort($a, 'strnatcmp');");
    assert!(s.labels.iter().any(|l| l == READ), "{s:?}");
    let s = row("array $a", "return array_map('basename', $a);");
    assert!(s.labels.iter().any(|l| l == READ), "{s:?}");
    for callee in ["pathinfo", "substr_compare"] {
        let s = row("array $a", &format!("return array_map('{callee}', $a);"));
        assert!(!s.labels.iter().any(|l| l == READ), "{callee}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{callee}: {s:?}");
    }
}

/// An envelope that does not admit the read is exceeded at the call, and one that admits it by
/// prefix is not (ADR-0101 §3.5): a pure function over `ctype_alpha` reports.
#[test]
fn a_pure_envelope_over_a_locale_reader_is_exceeded() {
    let pure = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): bool { return ctype_alpha($s); }\n";
    let d = findings(pure);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("global.read.setting.locale"), "{}", d[0].message);
    let admitted = "<?php\n#[\\Steins\\Effects('global.read')]\nfunction f(string $s): bool { return ctype_alpha($s); }\n";
    assert!(findings(admitted).is_empty());
    // `ctype_digit` is no claim, and `number_format` is certified: neither exceeds.
    let digit = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): string { return number_format(1.5, 2); }\n";
    assert!(findings(digit).is_empty());
}

/// What must not move: a name that reads no setting keeps its row, and the printf family and
/// `setlocale`'s write are the ones S1 to S3 left. A comparison on bytes is not the C
/// library's, and the 8.2 ASCII folding functions read nothing.
#[test]
fn a_name_that_reads_no_setting_keeps_its_row() {
    for call in [
        "strcmp($s, $s)",
        "strcasecmp($s, $s)",
        "strncasecmp($s, $s, 1)",
        "strtoupper($s)",
        "ucfirst($s)",
        "str_contains($s, $s)",
        "dirname($s)",
        "round(1.5)",
        "json_encode($s)",
        "is_numeric($s)",
    ] {
        let s = row("string $s", &format!("return {call};"));
        assert!(!s.labels.iter().any(|l| l.starts_with("global.")), "{call}: {s:?}");
        assert!(!s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    }
    for call in ["sprintf('%f', 1.5)", "localeconv()", "strcoll($s, $s)"] {
        proves("string $s", call, &[READ]);
    }
}
