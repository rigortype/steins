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
        .find(|s| s.symbol == symbol || s.qualified == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol} in {src}"))
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

/// `basename` reads the locale on every call: `php_basename` consults the locale-derived
/// `CG(ascii_compatible_locale)` before it looks at a byte, the empty string included.
#[test]
fn basename_reads_the_locale_on_every_call() {
    for call in ["basename($s)", "basename($s, '.php')", "basename('')", "\\basename('/a/b')"] {
        proves("string $s", call, &[READ]);
    }
}

/// A `ctype_*` predicate classifies the first byte of a non-empty string and an `int` in
/// -128..=255, and returns `false` before it consults a table for every other type
/// (`ctype_fallback`): a literal decides, a declared type that keeps a parameter from a string and
/// an integer rules the read out, and anything else is the gap.
#[test]
fn a_ctype_predicate_reads_only_a_string_or_a_small_int() {
    for name in [
        "ctype_alnum", "ctype_alpha", "ctype_cntrl", "ctype_graph", "ctype_lower", "ctype_print",
        "ctype_punct", "ctype_space", "ctype_upper",
    ] {
        for arg in ["'a'", "'12'", "\"\\xE4\"", "65", "0", "255"] {
            proves("", &format!("{name}({arg})"), &[READ]);
        }
        for arg in ["''", "256", "1000", "true", "false", "null", "1.5", "[]", "[1, 'a']"] {
            proves("", &format!("{name}({arg})"), &[]);
        }
        // A negative literal is an operator shape and an object a constructor the reach rule does
        // not rule out, so the body stays `…?`; the read is decided all the same.
        for (arg, labels) in [
            ("-1", vec![READ]),
            ("-128", vec![READ]),
            ("-129", vec![]),
            ("new \\stdClass", vec![]),
        ] {
            let s = row("", &format!("return {name}({arg});"));
            assert_eq!(s.labels, labels, "{name}({arg}): {s:?}");
            assert!(!s.gaps.contains(&DEPENDS), "{name}({arg}): {s:?}");
        }
    }
    // The types that keep a by-value parameter from a string and an integer.
    for hint in ["bool", "float", "?bool", "array", "iterable", "object", "null|bool", "\\Countable"] {
        let s = row(&format!("{hint} $v"), "return ctype_alpha($v);");
        assert!(s.labels.is_empty() && !s.gaps.contains(&DEPENDS), "{hint}: {s:?}");
    }
    let s = row("bool $b", "return ctype_alpha($b);");
    assert!(s.exhaustive && s.labels.is_empty(), "{s:?}");
    // Everything else may be a string or a small integer.
    for hint in ["string", "int", "?int", "int|bool", "mixed", "callable", "string|array", ""] {
        let signature = if hint.is_empty() { "$v".to_owned() } else { format!("{hint} $v") };
        depends(&signature, "ctype_alpha($v)", &[]);
    }
    // The declaration holds only while the frame leaves the parameter alone.
    depends("bool $b", "ctype_alpha($b . 'x')", &[]);
    let rebound = row("bool $b", "$b = 'x'; return ctype_alpha($b);");
    assert!(rebound.gaps.contains(&DEPENDS) && !rebound.labels.iter().any(|l| l == READ), "{rebound:?}");
    let by_ref = row("bool $b", "settype($b, 'string'); return ctype_alpha($b);");
    assert!(by_ref.gaps.contains(&DEPENDS) && !by_ref.labels.iter().any(|l| l == READ), "{by_ref:?}");
    // A named or spread argument list shows no position.
    depends("array $a", "ctype_alpha(...$a)", &[]);
}

/// `strnatcmp_ex` returns on a zero length before it consults a table: a literal empty operand
/// settles the call whatever the other is, two non-empty literals read, and one the call does not
/// show is the gap.
#[test]
fn strnatcmp_reads_only_over_two_non_empty_operands() {
    for name in ["strnatcmp", "strnatcasecmp"] {
        proves("", &format!("{name}('a1', 'a2')"), &[READ]);
        proves("string $s", &format!("{name}('a', '')"), &[]);
        proves("string $s", &format!("{name}('', $s)"), &[]);
        proves("string $s", &format!("{name}($s, '')"), &[]);
        depends("string $s", &format!("{name}($s, 'a')"), &[]);
        depends("string $s", &format!("{name}($s, $s)"), &[]);
    }
}

/// `escapeshellarg` classifies each byte of a non-empty string (and a NUL byte throws first);
/// `strip_tags` calls `isspace` at the first `<` and reads nothing without one.
#[test]
fn escapeshellarg_and_strip_tags_read_by_content() {
    proves("", "escapeshellarg('x')", &[READ]);
    proves("", "escapeshellarg('')", &[]);
    depends("string $s", "escapeshellarg($s)", &[]);
    depends("", "escapeshellarg(\"a\\0b\")", &[]);
    for call in ["strip_tags('<b>x</b>')", "strip_tags('a < b')", "strip_tags('x <p>', '<p>')"] {
        proves("", call, &[READ]);
    }
    for call in ["strip_tags('plain > text')", "strip_tags('')", "strip_tags('a & b', '<p>')"] {
        proves("", call, &[]);
    }
    depends("string $s", "strip_tags($s)", &[]);
}

/// `parse_url` calls `isalpha` over a scheme (the first colon is not at index 0) and `iscntrl`
/// over each component it produces: a string with a scheme or a plain path reads, the empty
/// string reads nothing, and a leading colon, a leading `//` and a string of only `?` and `#`
/// may fail before any component, so they are the gap.
#[test]
fn parse_url_reads_by_the_shape_of_a_literal_url() {
    for call in [
        "parse_url('http://x/y')",
        "parse_url('mailto:a@b')",
        "parse_url('/path/to')",
        "parse_url('a')",
        "parse_url('?q=1')",
        "parse_url('http://x/y', PHP_URL_HOST)",
    ] {
        proves("", call, &[READ]);
    }
    proves("", "parse_url('')", &[]);
    for call in ["parse_url(':80')", "parse_url('//host/x')", "parse_url('?')", "parse_url($u)"] {
        depends("string $u", call, &[]);
    }
}

/// `strftime` names the locale only through some conversions; the clock stays on the row.
#[test]
fn strftime_reads_the_locale_only_for_the_conversions_that_name_it() {
    for call in ["strftime('%A')", "strftime('%a %d %b', $t)", "gmstrftime('%c', 0)", "strftime('%x %X')"] {
        proves("int $t", call, &[READ, "nondet.time"]);
    }
    for call in ["strftime('%Y-%m-%d %H:%M:%S', $t)", "strftime('%s', 0)", "gmstrftime('%%', $t)", "strftime('')"] {
        proves("int $t", call, &["nondet.time"]);
    }
    for call in ["strftime($f)", "strftime($f, $t)", "strftime('%A %Q')", "strftime('%Ed')", "strftime('%')"] {
        depends("string $f, int $t", call, &["nondet.time"]);
    }
}

/// A bare constant is read as PHP resolves it: a namespaced twin the file declares shadows the
/// global one, a fully qualified spelling does not, and a twin the scan cannot read is the gap.
#[test]
fn a_namespaced_constant_twin_shadows_the_global_flag() {
    let file = |decl: &str, flag: &str| {
        format!("<?php\nnamespace App;\n{decl}\nfunction f(string $p) {{ return pathinfo($p, {flag}); }}\n")
    };
    let shadowed = summary(&file("const PATHINFO_DIRNAME = 2;", "PATHINFO_DIRNAME"), "f");
    assert_eq!(shadowed.labels, [READ], "{shadowed:?}");
    let qualified = summary(&file("const PATHINFO_DIRNAME = 2;", "\\PATHINFO_DIRNAME"), "f");
    assert!(qualified.labels.is_empty() && qualified.gaps.is_empty(), "{qualified:?}");
    let plain = summary(&file("", "PATHINFO_DIRNAME"), "f");
    assert!(plain.labels.is_empty() && plain.gaps.is_empty(), "{plain:?}");
    let unreadable = summary(&file("const PATHINFO_DIRNAME = SOME_FLAG | 1;", "PATHINFO_DIRNAME"), "f");
    assert!(unreadable.labels.is_empty() && unreadable.gaps.contains(&DEPENDS), "{unreadable:?}");
    let sort = "<?php\nnamespace App;\nconst SORT_NATURAL = 0;\nfunction g(array $a) { sort($a, SORT_NATURAL); }\n";
    let s = summary(sort, "g");
    assert!(!s.labels.iter().any(|l| l == READ), "the twin is SORT_REGULAR here: {s:?}");
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
        for flags in ["", ", SORT_REGULAR", ", SORT_NUMERIC", ", SORT_STRING", ", 2", ", SORT_DESC | SORT_FLAG_CASE"] {
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
/// call (`php_array_key_compare_string_case_unstable_i` uses `zend_binary_strcasecmp_l`), where
/// the data sorts use the engine's ASCII folding from 8.2 and `tolower` before: only the key
/// sorts read under `SORT_STRING | SORT_FLAG_CASE`, and a data sort is undecided there, since a
/// per-file summary cannot know the project's PHP floor.
#[test]
fn the_key_sorts_read_under_a_case_folding_string_flag() {
    for name in ["ksort", "krsort"] {
        let call = format!("{name}($a, SORT_STRING | SORT_FLAG_CASE)");
        let s = row("array $a", &format!("{call};"));
        assert_eq!(s.labels, [READ, "mutate.local"], "{call}: {s:?}");
        assert!(!s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    }
    for name in ["sort", "rsort", "asort", "arsort"] {
        let call = format!("{name}($a, SORT_STRING | SORT_FLAG_CASE)");
        let s = row("array $a", &format!("{call};"));
        assert_eq!(s.labels, ["mutate.local"], "{call}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
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
/// unconditional reader keeps the read, and every reader the call decides is the gap.
#[test]
fn a_reader_handed_over_as_a_callback_follows_the_same_rule() {
    let s = row("array $a", "return array_map('basename', $a);");
    assert!(s.labels.iter().any(|l| l == READ), "{s:?}");
    for callee in ["pathinfo", "substr_compare", "ctype_alpha", "strnatcmp", "strip_tags", "strftime"] {
        let s = row("array $a", &format!("return array_map('{callee}', $a);"));
        assert!(!s.labels.iter().any(|l| l == READ), "{callee}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{callee}: {s:?}");
    }
}

/// An envelope that does not admit the read is exceeded at the call, and one that admits it by
/// prefix is not (ADR-0101 §3.5): a pure function over `ctype_alpha` reports.
#[test]
fn a_pure_envelope_over_a_locale_reader_is_exceeded() {
    let pure = "<?php\n#[\\Steins\\Pure]\nfunction f(): bool { return ctype_alpha('a'); }\n";
    let d = findings(pure);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("global.read.setting.locale"), "{}", d[0].message);
    let admitted = "<?php\n#[\\Steins\\Effect('global.read')]\nfunction f(): bool { return ctype_alpha('a'); }\n";
    assert!(findings(admitted).is_empty());
    // The attribute is read: an envelope that does not admit the read reports it.
    let refused = admitted.replace("global.read", "global.write");
    let d = findings(&refused);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("global.read.setting.locale"), "{}", d[0].message);
    // A call that may or may not reach the table is no proven read, so no finding at the
    // default floor: a `bool`, which never does, and a `string`, which may be empty.
    for signature in ["bool $b", "string $s"] {
        let src = format!("<?php\n#[\\Steins\\Pure]\nfunction f({signature}): bool {{ return ctype_alpha(${}); }}\n", &signature[signature.len() - 1..]);
        assert!(findings(&src).is_empty(), "{signature}: {:#?}", findings(&src));
    }
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
