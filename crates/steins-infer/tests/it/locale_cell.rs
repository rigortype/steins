//! The locale cell in the effect lane (ADR-0101, issue #991): the printf
//! family's `%f`, `%g` and `%G` read `LC_NUMERIC`, so a call carries
//! `global.read.setting.locale`; `setlocale` writes the cell; an envelope that
//! does not admit the read is exceeded at the call.
//!
//! The label is the row's, at every call site. The literal-format read that
//! drops it from `sprintf('%d', $x)` is the engine's later slice (ADR-0101
//! §3.2), so until it lands the interim below is the intended answer, and the
//! tests say so.

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

/// The row stands at every printf-family call, whatever the format: `%d`
/// carries the read too, until the engine reads the literal (ADR-0101 §3.2).
/// The body stays exhaustive: a setting read is a known label and never a gap.
#[test]
fn a_printf_family_call_carries_the_locale_read_whatever_its_format() {
    for call in [
        "sprintf('%f', $x)",
        "sprintf('%d', $x)",
        "sprintf('%d-%s', 1, 'a')",
        "sprintf($fmt, $x)",
        "vsprintf('%F', [1.5])",
        "vsprintf('%d', ['a'])",
    ] {
        let s = summary(&body("int $x, string $fmt", &format!("return {call};")), "f");
        assert_eq!(s.labels, [READ], "{call}: {s:?}");
        assert!(s.exhaustive, "{call} is a known row, not a gap: {s:?}");
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
}

/// `setlocale` writes the cell and nothing coarser; `localeconv`, `nl_langinfo`
/// and `strcoll` read it.
#[test]
fn setlocale_writes_the_cell_and_its_readers_read_it() {
    let s = summary(&body("", "return setlocale(LC_ALL, 'de_DE.UTF-8');"), "f");
    assert_eq!(s.labels, ["global.write.setting.locale"], "{s:?}");
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

/// A read-free body is untouched: `number_format`, `strval`, `(string) $f`,
/// `json_encode` of a float read no setting, and a literal that merely contains
/// a percent sign calls nothing.
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
    for envelope in ["global.read", "global.read.setting", "global.read.setting.locale", "global"] {
        let src = format!(
            "<?php\n#[\\Steins\\Effect('{envelope}')]\nfunction f(float $x): string {{ return sprintf('%f', $x); }}\n"
        );
        assert!(findings(&src).is_empty(), "{envelope} admits the read");
    }
    let write = "<?php\n#[\\Steins\\Effect('global.read')]\nfunction f(): void { setlocale(LC_ALL, 'C'); }\n";
    assert_eq!(findings(write).len(), 1, "a global.read envelope is no write");
    let write_ok =
        "<?php\n#[\\Steins\\Effect('global.write')]\nfunction f(): void { setlocale(LC_ALL, 'C'); }\n";
    assert!(findings(write_ok).is_empty(), "global.write admits the cell's write");
}
