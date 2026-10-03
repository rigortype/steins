//! The locale-independent fix-it (ADR-0101 §3.6, D3): a proven `global.read.setting.locale` at a
//! printf-family call with a literal format, held against an envelope, carries the edit that
//! spells each `f`, `g` and `G` conversion as `F`, `h` and `H`. The fix rides
//! `effect.envelope-exceeded` and `effect.liskov-widened` and nothing else, and is offered only
//! where the source can be edited byte-exactly and the edit takes the read out.

use steins_infer::{Diagnostic, EFFECT_ID, EFFECT_LISKOV_ID, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

const TITLE: &str = "use the locale-independent conversion (F, h, H): under a locale whose decimal point is not '.' the output changes, the decimal point becomes '.' always";

/// A folder that never folds and answers one PHP minor, or none: the run's floor when the project
/// declares no target.
struct Floor(Option<(u16, u16)>);

impl Folder for Floor {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        self.0
    }
}

/// Every effect finding of `src` on PHP 8.5, the Liskov ones included.
fn findings(src: &str) -> Vec<Diagnostic> {
    findings_on(src, Some((8, 5)))
}

/// [`findings`] on a run whose PHP floor is `floor`.
fn findings_on(src: &str, floor: Option<(u16, u16)>) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", &mut Floor(floor))
        .into_iter()
        .filter(|d| d.id == EFFECT_ID || d.id == EFFECT_LISKOV_ID)
        .collect()
}

/// `src` with the edits of every finding's fix spliced in, the way `check --fix` applies them
/// (identical edits once).
fn fixed(src: &str, found: &[Diagnostic]) -> String {
    let mut edits: Vec<_> =
        found.iter().filter_map(|d| d.fix.as_ref()).flat_map(|f| f.edits.clone()).collect();
    edits.sort_by_key(|e| (e.start, e.end));
    edits.dedup();
    let mut out = String::new();
    let mut at = 0_usize;
    for e in edits {
        assert_eq!(e.path, "test.php");
        out.push_str(&src[at..e.start as usize]);
        out.push_str(&e.replacement);
        at = e.end as usize;
    }
    out.push_str(&src[at..]);
    out
}

/// The attribute form and both docblock forms of a pure envelope around `body`.
fn pure_forms(body: &str) -> [String; 3] {
    [
        format!("<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string {{ {body} }}\n"),
        format!("<?php\n/** @phpstan-pure */\nfunction f(float $x): string {{ {body} }}\n"),
        format!("<?php\nclass C {{\n#[\\Steins\\Pure]\npublic function f(float $x): string {{ {body} }}\n}}\n"),
    ]
}

/// Fix `src`, which must carry exactly one finding, and return the fixed source with the finding
/// re-checked clean.
fn fix_once(src: &str) -> String {
    let found = findings(src);
    assert_eq!(found.len(), 1, "{src}: {found:#?}");
    let fix = found[0].fix.as_ref().unwrap_or_else(|| panic!("{src}: no fix on {found:#?}"));
    assert_eq!(fix.title, TITLE);
    let after = fixed(src, &found);
    let rest = findings(&after);
    assert!(rest.is_empty(), "{after}: the locale read is gone, got {rest:#?}");
    after
}

/// Fix the one locale finding of `src` and return the fixed source, re-checked free of the locale
/// read. Other findings (a `%s` of a float reads `precision`) are neither judged nor fixed.
fn fix_locale(src: &str) -> String {
    let found = findings(src);
    let locale = |d: &&Diagnostic| d.message.contains("setting.locale");
    let reads: Vec<_> = found.iter().filter(locale).collect();
    assert_eq!(reads.len(), 1, "{src}: {found:#?}");
    assert_eq!(reads[0].fix.as_ref().map(|f| f.title), Some(TITLE), "{src}: {found:#?}");
    let after = fixed(src, &found);
    let rest = findings(&after);
    assert_eq!(rest.iter().filter(locale).count(), 0, "{after}: {rest:#?}");
    after
}

/// A pure body with `'%.2f'` is fixed to `'%.2F'` and is clean afterwards, in each of the three
/// spellings of a pure envelope.
#[test]
fn a_pure_body_with_percent_f_is_fixed_to_capital_f_and_is_clean() {
    for src in pure_forms("return sprintf('%.2f', $x);") {
        let after = fix_once(&src);
        assert_eq!(after, src.replace("'%.2f'", "'%.2F'"));
    }
}

/// The three letters, with their flags, width, precision and `n$` kept; each of the four
/// printf-family names; `%%f` is left alone.
#[test]
fn each_locale_letter_gets_its_twin_and_the_rest_of_the_spec_is_kept() {
    for (call, expected) in [
        ("sprintf('%g', $x)", "sprintf('%h', $x)"),
        ("sprintf('%G', $x)", "sprintf('%H', $x)"),
        ("sprintf('%+010.3f|%-8.2g|%5.1G', $x, $x, $x)", "sprintf('%+010.3F|%-8.2h|%5.1H', $x, $x, $x)"),
        ("sprintf(\"%'*12.2f\", $x)", "sprintf(\"%'*12.2F\", $x)"),
        ("sprintf('%lf', $x)", "sprintf('%lF', $x)"),
        ("sprintf('%%f %f %%g', $x)", "sprintf('%%f %F %%g', $x)"),
        ("sprintf('%1$f %1$s', $x)", "sprintf('%1$F %1$s', $x)"),
        ("sprintf('%2$f %1$s', 'a', $x)", "sprintf('%2$F %1$s', 'a', $x)"),
        ("sprintf('%d %f %e %F', 1, $x, $x, $x)", "sprintf('%d %F %e %F', 1, $x, $x, $x)"),
        ("vsprintf('%f', [$x])", "vsprintf('%F', [$x])"),
        ("\\sprintf('%f', $x)", "\\sprintf('%F', $x)"),
        ("SPRINTF ( /* c */ '%f' , $x )", "SPRINTF ( /* c */ '%F' , $x )"),
    ] {
        let src = format!("<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string {{ return {call}; }}\n");
        let after = fix_locale(&src);
        assert_eq!(after, src.replace(call, expected), "{call}");
    }
    for call in ["printf('%.1f', $x)", "vprintf('%g', [$x])"] {
        let src = format!("<?php\n#[\\Steins\\Pure]\nfunction f(float $x): void {{ {call}; }}\n");
        let found = findings(&src);
        let locale: Vec<_> = found.iter().filter(|d| d.message.contains("setting.locale")).collect();
        assert_eq!(locale.len(), 1, "{call}: {found:#?}");
        assert!(locale[0].fix.is_some(), "{call}");
        let after = fixed(&src, &found);
        assert!(
            !findings(&after).iter().any(|d| d.message.contains("setting.locale")),
            "{call}: {after}"
        );
    }
}

/// `%%f` is no conversion, so the format has no read to fix and no finding.
#[test]
fn an_escaped_percent_has_nothing_to_fix() {
    for src in pure_forms("return sprintf('%%f', $x);") {
        assert!(findings(&src).is_empty(), "{src}");
    }
}

/// A format that is not a literal is the value-dependent-read gap and not a proven read: no
/// finding, so no fix; and a literal the source spells with a concatenation is not a literal.
#[test]
fn a_format_that_is_not_a_literal_gets_no_fix() {
    for call in ["sprintf($fmt, $x)", "sprintf('%' . 'f', $x)", "sprintf(FMT, $x)"] {
        let src = format!("<?php\n#[\\Steins\\Pure]\nfunction f(float $x, string $fmt): string {{ return {call}; }}\n");
        assert!(findings(&src).iter().all(|d| d.fix.is_none()), "{call}: {:#?}", findings(&src));
    }
}

/// Without an envelope nothing is claimed about the function: no finding and no fix.
#[test]
fn a_body_with_no_envelope_gets_neither_a_finding_nor_a_fix() {
    let src = "<?php\nfunction f(float $x): string { return sprintf('%.2f', $x); }\n";
    assert!(findings(src).is_empty());
    let src = "<?php\n#[\\Steins\\Effect('global.read')]\nfunction f(float $x): string { return sprintf('%.2f', $x); }\n";
    assert!(findings(src).is_empty(), "an envelope that admits the read has nothing to fix");
}

/// A body that also exceeds the envelope with other labels still gets the locale fix, on the
/// locale finding alone; the other findings stay, and so does their lack of a fix.
#[test]
fn another_exceeding_label_leaves_the_locale_fix_in_place() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { $r = rand(); return sprintf('%.2f', $x); }\n";
    let found = findings(src);
    assert_eq!(found.len(), 2, "{found:#?}");
    let with_fix: Vec<_> = found.iter().filter(|d| d.fix.is_some()).collect();
    assert_eq!(with_fix.len(), 1, "{found:#?}");
    assert!(with_fix[0].message.starts_with("sprintf() has effect global.read.setting.locale"));
    let after = fixed(src, &found);
    assert_eq!(after, src.replace("'%.2f'", "'%.2F'"));
    let rest = findings(&after);
    assert_eq!(rest.len(), 1, "{rest:#?}");
    assert!(rest[0].message.starts_with("rand() has effect nondet.random"), "{rest:#?}");
}

/// Two conversions in one format, and two calls in one body, each carry their own edits.
#[test]
fn every_locale_conversion_of_every_call_is_fixed() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf('%f/%g', $x, $x) . sprintf('%G', $x); }\n";
    let found = findings(src);
    assert_eq!(found.len(), 2, "{found:#?}");
    let after = fixed(src, &found);
    assert_eq!(
        after,
        "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf('%F/%h', $x, $x) . sprintf('%H', $x); }\n"
    );
    assert!(findings(&after).is_empty());
}

/// The literal's spelling: a single-quoted format with an escaped quote and backslash keeps its
/// escapes; a double-quoted one with an escape written for the conversion letter is edited
/// exactly, the whole escape replaced by the plain letter; a heredoc is not edited.
#[test]
fn the_edit_is_exact_on_the_source_bytes() {
    // Escapes in the text around the conversion shift the byte offsets between the value and
    // the source; the edit lands on the letter all the same.
    for (call, expected) in [
        (r"sprintf('it\'s \\ %f', $x)", r"sprintf('it\'s \\ %F', $x)"),
        (r#"sprintf("tab\t%f\n", $x)"#, r#"sprintf("tab\t%F\n", $x)"#),
        (r#"sprintf("\x25.1f", $x)"#, r#"sprintf("\x25.1F", $x)"#),
        (r#"sprintf("é %f", $x)"#, r#"sprintf("é %F", $x)"#),
        (r#"sprintf("\u{e9}%f", $x)"#, r#"sprintf("\u{e9}%F", $x)"#),
        // `"\045f"` is `%f` spelled with an octal escape: both conversions are edited.
        (r#"sprintf("\045f %f", $x)"#, r#"sprintf("\045F %F", $x)"#),
    ] {
        let src = format!("<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string {{ return {call}; }}\n");
        let after = fix_once(&src);
        assert_eq!(after, src.replace(call, expected), "{call}");
    }
}

/// A conversion letter written as an escape is edited exactly: `"%\x66"` is `%f`, and the fix
/// replaces the four bytes `\x66` with `F`.
#[test]
fn an_escaped_conversion_letter_is_replaced_whole() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf(\"%.2\\x66\", $x); }\n";
    let after = fix_once(src);
    assert_eq!(after, src.replace("\\x66", "F"));
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf(\"%\\x47\", $x); }\n";
    let after = fix_once(src);
    assert_eq!(after, src.replace("\\x47", "H"));
}

/// A heredoc or nowdoc format, and a format the call hands over by name, get no fix: the
/// finding, where there is one, stays unfixed.
#[test]
fn a_format_the_source_cannot_be_edited_at_gets_no_fix() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf(<<<'F'\n%f\nF, $x); }\n";
    for d in findings(src) {
        assert!(d.fix.is_none(), "{d:#?}");
    }
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf(format: '%f', values: $x); }\n";
    for d in findings(src) {
        assert!(d.fix.is_none(), "{d:#?}");
    }
}

/// A Liskov widening whose proven read is a literal printf of the method's own body carries the
/// fix too; one that has another origin of the read does not.
#[test]
fn the_liskov_finding_carries_the_fix_when_the_edit_removes_every_origin() {
    let abs = "interface Fmt { #[\\Steins\\Pure] public function f(float $x): string; }\n";
    let own = format!(
        "<?php\n{abs}class Impl implements Fmt {{ public function f(float $x): string {{ return sprintf('%.2f', $x); }} }}\n"
    );
    let found = findings(&own);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].id, EFFECT_LISKOV_ID);
    assert_eq!(fixed(&own, &found), own.replace("'%.2f'", "'%.2F'"));
    assert!(findings(&fixed(&own, &found)).is_empty());

    // Another origin of the read, in a callee the edit does not reach: no fix.
    let via = format!(
        "<?php\n{abs}function fmt(float $x): string {{ return sprintf('%.2f', $x); }}\n\
         class Impl implements Fmt {{ public function f(float $x): string {{ return fmt($x) . sprintf('%.1f', $x); }} }}\n"
    );
    let found = findings(&via);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].id, EFFECT_LISKOV_ID);
    assert!(found[0].fix.is_none(), "{found:#?}");
}

/// An origin of the read that is not the method's own printf is told from it structurally, and
/// not by the name and line a finding quotes: a callee, a private method or a closure that
/// shares the line with the method's own `sprintf` leaves the read standing, so the Liskov
/// finding carries no fix (each of these printed "fixed" and re-reported before).
#[test]
fn a_liskov_finding_carries_no_fix_when_another_origin_shares_its_line() {
    let abs = "interface Fmt { #[\\Steins\\Pure] public function f(float $x): string; }\n";
    for body in [
        // A callee declared on the same line.
        "function fmt(float $x): string { return sprintf('%.2f', $x); } class Impl implements Fmt { public function f(float $x): string { return fmt($x) . sprintf('%.1f', $x); } }",
        // An arrow function in a one-line body.
        "class Impl implements Fmt { public function f(float $x): string { $c = fn($y) => sprintf('%g', $y); return sprintf('%.1f', $x) . $c($x); } }",
        // A private method on the same line.
        "class Impl implements Fmt { private function h(float $x): string { return sprintf('%g', $x); } public function f(float $x): string { return $this->h($x) . sprintf('%.1f', $x); } }",
        // A closure whose printf ends on the line of the method's own.
        "class Impl implements Fmt { public function f(float $x): string {\n$c = function ($y) {\nreturn sprintf('%g', $y); }; return sprintf('%.1f', $x) . $c($x); } }",
    ] {
        let src = format!("<?php\n{abs}{body}\n");
        let found = findings(&src);
        let liskov: Vec<_> = found.iter().filter(|d| d.id == EFFECT_LISKOV_ID).collect();
        assert_eq!(liskov.len(), 1, "{body}: {found:#?}");
        assert!(liskov[0].fix.is_none(), "{body}: {found:#?}");
    }
}

/// An octal escape PHP truncates (`\546` is `f` and a warning) is not rewritten, since the
/// plain letter would lose the warning; one that fits a byte is.
#[test]
fn an_octal_escape_above_a_byte_gets_no_fix() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf(\"%.2\\546\", $x); }\n";
    for d in findings(src) {
        assert!(d.fix.is_none(), "{d:#?}");
    }
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string { return sprintf(\"%.2\\146\", $x); }\n";
    let after = fix_once(src);
    assert_eq!(after, src.replace("\\146", "F"));
}

/// A callee's printf is not the caller's body: the transitive finding names an origin elsewhere
/// and carries no fix.
#[test]
fn a_transitive_finding_carries_no_fix() {
    let src = "<?php\nfunction fmt(float $x): string { return sprintf('%.2f', $x); }\n\
               #[\\Steins\\Pure]\nfunction f(float $x): string { return fmt($x); }\n";
    let found = findings(src);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].fix.is_none(), "{found:#?}");
}

/// `h` and `H` need a floor known to be PHP 8.0 or later: with a floor below it, or none known
/// (nothing declared and no runtime answering), a call with a `g` or `G` conversion gets no fix,
/// whatever else its format holds; `F` is offered on any floor.
#[test]
fn h_and_capital_h_are_offered_only_on_a_known_php_8_floor() {
    let call = |format: &str| {
        format!(
            "<?php\n#[\\Steins\\Pure]\nfunction f(float $x): string {{ return sprintf('{format}', $x, $x); }}\n"
        )
    };
    for floor in [None, Some((7, 4)), Some((5, 6))] {
        for format in ["%.3g", "%G", "%f|%g"] {
            let found = findings_on(&call(format), floor);
            assert!(found.iter().any(|d| d.message.contains("setting.locale")), "{format}");
            assert!(found.iter().all(|d| d.fix.is_none()), "{format} on {floor:?}: {found:#?}");
        }
        let f_only = call("%.2f");
        let found = findings_on(&f_only, floor);
        assert_eq!(fixed(&f_only, &found), f_only.replace("%.2f", "%.2F"), "{floor:?}");
    }
    for floor in [(8, 0), (8, 5)] {
        let src = call("%.3g|%G");
        let found = findings_on(&src, Some(floor));
        assert_eq!(fixed(&src, &found), src.replace("%.3g|%G", "%.3h|%H"), "{floor:?}");
    }
}
