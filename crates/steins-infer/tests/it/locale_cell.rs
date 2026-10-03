//! The locale cell in the effect lane (ADR-0101, issue #991): the printf
//! family's `%f`, `%g` and `%G` read `LC_NUMERIC`, so a call carries
//! `global.read.setting.locale`; `setlocale` writes the cell; an envelope that
//! does not admit the read is exceeded at the call.
//!
//! The label is the row's, and a call site that reads a literal format drops it
//! when no conversion is `f`, `g` or `G` (ADR-0101 §3.2, `printf_call_site.rs`
//! holds that table); a format the call does not show keeps it.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

const READ: &str = "global.read.setting.locale";

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

fn body(signature: &str, body: &str) -> String {
    format!("<?php\nfunction f({signature}) {{ {body} }}\n")
}

/// A printf-family call carries the locale read where its format reads it: a literal `f`, `g`
/// or `G` conversion (flags, width, precision and `n$` included). The body stays exhaustive: a
/// setting read is a known label and never a gap.
#[test]
fn a_printf_family_call_carries_the_locale_read_where_the_format_may_read_it() {
    for call in [
        "sprintf('%f', $x)",
        "sprintf('%.2f', $x)",
        "sprintf('%05.1f', $x)",
        "sprintf('%1$.3g', $x)",
        "sprintf('%G', $x)",
        "sprintf('%d %f', 1, $x)",
        "sprintf('%%%f', $x)",
        "vsprintf('%f', [1.5])",
    ] {
        let s = summary(&body("int $x, string $fmt, array $args", &format!("return {call};")), "f");
        assert!(s.labels.iter().any(|l| l == READ), "{call}: {s:?}");
        assert!(s.exhaustive, "{call} is a known row, not a gap: {s:?}");
    }
}

/// A format the call does not show, or the parser cannot read, is the calibration's
/// `value-dependent-read` gap for the locale read and never a label: `'%d'` reads nothing, so
/// no label is true on every path (ADR-0101 §3.2). `printf` keeps its output label.
#[test]
fn a_format_the_site_cannot_read_is_a_gap_and_not_a_locale_label() {
    for call in [
        "sprintf($fmt, $x)",
        "sprintf($fmt)",
        "sprintf('%' . $fmt, $x)",
        "sprintf(\"%d{$fmt}\", $x)",
        "sprintf('%*d', 3, $x)",
        "sprintf('%q', $x)",
        "sprintf('%d %q', $x)",
        "vsprintf($fmt, ['a'])",
        "sprintf(...$args)",
        "sprintf(format: '%d', values: 1)",
    ] {
        let s = summary(&body("int $x, string $fmt, array $args", &format!("return {call};")), "f");
        assert!(s.labels.is_empty(), "{call}: {s:?}");
        assert!(s.gaps.contains(&"value-dependent-read"), "{call}: {s:?}");
        assert!(!s.exhaustive, "{call}: {s:?}");
    }
    for call in ["printf($fmt, $x)", "vprintf($fmt, $args)"] {
        let s = summary(&body("int $x, string $fmt, array $args", &format!("{call};")), "f");
        assert_eq!(s.labels, ["io.output.buffer"], "{call}: {s:?}");
        assert!(s.gaps.contains(&"value-dependent-read"), "{call}: {s:?}");
    }
}

/// A literal format that shows no `f`, `g` or `G` drops the locale read (ADR-0101 §3.2,
/// #991): `sprintf('%d-%s', 1, 'a')` is pure again, and every other conversion, `%%f`
/// and a format with no spec stay silent. Dropping the locale read is not the claim that
/// the call reads no setting: see `printf_call_site.rs` for `precision`.
#[test]
fn a_literal_format_with_no_f_g_or_capital_g_drops_the_locale_read() {
    for call in [
        "sprintf('%d-%s', 1, 'a')",
        "sprintf('%F', $x)",
        "sprintf('%.2F', $x)",
        "sprintf('%e', $x)",
        "sprintf('%E', $x)",
        "sprintf('%h', $x)",
        "sprintf('%H', $x)",
        "sprintf('%d', $x)",
        "sprintf('%5.1e|%u|%c|%o|%x|%X|%b', $x, $x, $x, $x, $x, $x, $x)",
        "sprintf('%%f')",
        "sprintf('100%%f')",
        "sprintf('plain')",
        "sprintf(\"%d\", $x)",
        "vsprintf('%F', [1.5])",
        "vsprintf('%d', ['a'])",
    ] {
        let s = summary(&body("int $x, string $fmt", &format!("return {call};")), "f");
        assert_eq!(s.labels, Vec::<String>::new(), "{call}: {s:?}");
        assert!(s.exhaustive, "{call}: {s:?}");
    }
}

/// `vsprintf` follows `sprintf`, and `printf`/`vprintf` keep their output label
/// beside the read.
#[test]
fn the_v_spellings_follow_their_siblings_and_printf_keeps_its_output_label() {
    let sprintf = summary(&body("int $x", "return sprintf('%f', $x);"), "f");
    let vsprintf = summary(&body("int $x", "return vsprintf('%f', [$x]);"), "f");
    assert_eq!(sprintf.labels, vsprintf.labels);
    for call in ["printf('%f', $x)", "vprintf('%f', [$x])"] {
        let s = summary(&body("int $x", &format!("{call};")), "f");
        assert_eq!(s.labels, [READ, "io.output.buffer"], "{call}: {s:?}");
    }
    // A format that shows no read leaves `printf` its output label alone.
    for call in ["printf('%d', $x)", "vprintf('%F', [$x])"] {
        let s = summary(&body("int $x", &format!("{call};")), "f");
        assert_eq!(s.labels, ["io.output.buffer"], "{call}: {s:?}");
    }
    // A `%s` over a vector may render a float, which a vector's elements do not show: the gap.
    let s = summary(&body("array $x", "vprintf('%s', $x);"), "f");
    assert_eq!(s.labels, ["io.output.buffer"], "{s:?}");
    assert!(s.gaps.contains(&"value-dependent-read"), "{s:?}");
}

/// `setlocale` writes the cell; `localeconv`, `nl_langinfo` and `strcoll` read it.
/// A locale of `''` or `null`, or one the call does not show, also reads the
/// environment block, a coarse `global.read` until the environment cell has a
/// label; a written non-empty locale reads none.
#[test]
fn setlocale_writes_the_cell_and_its_readers_read_it() {
    for call in ["setlocale(LC_ALL, 'de_DE.UTF-8')", "setlocale(LC_ALL, 'C')", "\\setlocale(LC_ALL, \"C\")"] {
        let s = summary(&body("", &format!("return {call};")), "f");
        assert_eq!(s.labels, ["global.write.setting.locale"], "{call}: {s:?}");
    }
    for call in [
        "setlocale(LC_ALL, '')",
        "setlocale(LC_ALL, null)",
        "setlocale(LC_ALL, $l)",
        "setlocale(LC_ALL, '0', 'C')",
        "setlocale(LC_ALL, 'xx_XX', '')",
        "setlocale(LC_ALL, ['C', ''])",
        "setlocale(LC_ALL)",
        // C stops at the NUL, so these name `''` (the environment) and a locale called `0`; only
        // the whole string `'0'` is the query (`locale_readers.rs`).
        "setlocale(LC_ALL, \"\\0\")",
        "setlocale(LC_ALL, \"\\0C\")",
        "setlocale(LC_ALL, \"\\x00\")",
        "setlocale(LC_ALL, \"0\\0x\")",
    ] {
        let s = summary(&body("string $l", &format!("return {call};")), "f");
        assert_eq!(s.labels, ["global.read", "global.write.setting.locale"], "{call}: {s:?}");
    }
    for call in ["localeconv()", "nl_langinfo(CODESET)", "strcoll('a', 'b')"] {
        let s = summary(&body("", &format!("return {call};")), "f");
        assert_eq!(s.labels, [READ], "{call}: {s:?}");
    }
}

/// A body reaching the read through a callee inherits it, so every caller of a
/// printf-family body is no longer silent about the locale.
#[test]
fn a_caller_inherits_the_read_through_its_callee() {
    let src = "<?php\nfunction fmt(float $x): string { return sprintf('%.2f', $x); }\n\
               function f(float $x): string { return fmt($x); }\n";
    let s = summary(src, "f");
    assert_eq!(s.labels, [READ], "{s:?}");
    assert!(s.exhaustive, "{s:?}");
}

/// A body with no printf-family call carries no setting label: `(string) $f`,
/// `strval`, `json_encode` and `round` of a float carry none, and a literal that
/// merely contains a percent sign calls nothing. (The string cast and `strval`
/// do read the `precision` ini, which is its own cell under D4 whose label only a
/// printf `%s` colours so far (float-to-string operator sites wait for ADR-0008's
/// opt-in), so "no label" is the claim and not "reads no setting".)
#[test]
fn a_body_with_no_printf_family_call_keeps_no_read() {
    for call in ["(string) $f", "strval($f)", "json_encode($f)", "round($f, 2)", "'%f'"] {
        let s = summary(&body("float $f", &format!("return {call};")), "f");
        assert!(!s.labels.iter().any(|l| l.starts_with("global.")), "{call}: {s:?}");
    }
}

/// An envelope that does not admit the read is exceeded at the call, and one
/// that admits it by prefix is not (ADR-0101 §3.5).
#[test]
fn a_pure_envelope_over_the_read_is_exceeded_and_a_global_read_envelope_admits_it() {
    let pure = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf('%f', $x); }\n";
    let d = findings(pure);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(
        d[0].message,
        "sprintf() has effect global.read.setting.locale, but f() is declared #[\\Steins\\Pure]"
    );
    // A format with no `f`, `g` or `G` leaves nothing to exceed.
    let pure = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf('%F', $x); }\n";
    assert!(findings(pure).is_empty(), "{:#?}", findings(pure));
    for envelope in ["global.read", "global.read.setting", "global.read.setting.locale", "global"] {
        let src = format!(
            "<?php\n#[\\Steins\\Effect('{envelope}')]\nfunction f(float $x): string {{ return sprintf('%f', $x); }}\n"
        );
        assert!(findings(&src).is_empty(), "{envelope} admits the read");
    }
    // A written non-empty locale is a write and nothing else: `global.write` admits it
    // (as it did before the environment read was modelled), `global.read` does not.
    let call = "function f(): void { setlocale(LC_ALL, 'C'); }\n";
    let write_only = format!("<?php\n#[\\Steins\\Effect('global.write')]\n{call}");
    assert!(findings(&write_only).is_empty(), "global.write admits the locale write");
    let read_only = format!("<?php\n#[\\Steins\\Effect('global.read')]\n{call}");
    let found: Vec<String> = findings(&read_only).into_iter().map(|d| d.message).collect();
    assert_eq!(found.len(), 1, "a global.read envelope is no write: {found:#?}");
    assert!(found[0].contains("global.write.setting.locale"), "{found:#?}");
    // A locale of `''` also reads the environment: neither coarse envelope covers both.
    let call = "function f(): void { setlocale(LC_ALL, ''); }\n";
    let write_only = format!("<?php\n#[\\Steins\\Effect('global.write')]\n{call}");
    let found: Vec<String> = findings(&write_only).into_iter().map(|d| d.message).collect();
    assert_eq!(found.len(), 1, "a global.write envelope is no read: {found:#?}");
    assert!(found[0].contains("setlocale() has effect global.read,"), "{found:#?}");
    let both = format!("<?php\n#[\\Steins\\Effect('global')]\n{call}");
    assert!(findings(&both).is_empty(), "global admits the write and the read");
}
