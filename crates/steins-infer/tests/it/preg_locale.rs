//! The preg family's locale read in the effect lane (ADR-0101 §3.10, issue #1000, S5): a literal
//! pattern decides whether the call consults the locale's character tables, and a pattern the
//! site cannot read is the `value-dependent-read` gap.
//!
//! The witness rows of `locale_readers_oracle.rs` and the pattern verdicts of
//! `steins_catalog::preg::pattern_reads_locale` decide the read; these tests pin that every
//! compiling function reaches the same verdict through its own call shape (a single pattern, an
//! array of patterns, the keys of `preg_replace_callback_array`), that the gap is no label, and
//! that an envelope reports the proven read only.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

const READ: &str = "global.read.setting.locale";
const DEPENDS: &str = "value-dependent-read";

fn summary(body: &str) -> EffectSummary {
    let src = format!("<?php\nfunction f($p, $s) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
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

/// The call carries exactly the locale read, and the body stays exhaustive.
fn reads(call: &str) {
    let s = summary(&format!("return {call};"));
    assert_eq!(s.labels, [READ], "{call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// The call carries exactly the locale read; the callbacks it hands over keep the body `…?`.
fn reads_with_callbacks(call: &str) {
    let s = summary(&format!("return {call};"));
    assert_eq!(s.labels, [READ], "{call}: {s:?}");
    assert!(!s.gaps.contains(&DEPENDS), "{call}: {s:?}");
}

/// The call carries no label and no `value-dependent-read` gap; the callbacks it hands over keep
/// the body `…?`.
fn silent_with_callbacks(call: &str) {
    let s = summary(&format!("return {call};"));
    assert!(s.labels.is_empty() && !s.gaps.contains(&DEPENDS), "{call}: {s:?}");
}

/// The call carries no label, and the body stays exhaustive.
fn silent(call: &str) {
    let s = summary(&format!("return {call};"));
    assert!(s.labels.is_empty(), "{call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// The call carries no label and the `value-dependent-read` gap.
fn depends(call: &str) {
    let s = summary(&format!("return {call};"));
    assert!(s.labels.is_empty(), "{call}: {s:?}");
    assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    assert!(!s.exhaustive, "{call}: {s:?}");
}

/// Every function that compiles a pattern, called with `pattern` and literal arguments
/// otherwise.
fn calls(pattern: &str) -> Vec<String> {
    vec![
        format!("preg_match({pattern}, 'a')"),
        format!("preg_match_all({pattern}, 'a')"),
        format!("preg_replace({pattern}, '_', 'a')"),
        format!("preg_filter({pattern}, '_', 'a')"),
        format!("preg_split({pattern}, 'a b')"),
        format!("preg_grep({pattern}, ['a'])"),
        format!("preg_replace_callback({pattern}, fn ($m) => 'y', 'a')"),
    ]
}

/// The witnessed readers read through every function that compiles (P18, P19, P28, S21, S22: one
/// compiler), and the witnessed exemptions drop the read through every one of them.
#[test]
fn every_compiling_function_reaches_the_patterns_verdict() {
    for pattern in [
        r"'/^\w$/'",
        r"'/^\W$/'",
        r"'/\bx/'",
        r"'/a\B/'",
        r"'/^\s$/'",
        "'/^[[:alpha:]]$/'",
        "'/^[[:blank:]]$/'",
        "'/^A$/i'",
        r"'/^(?i:\xC4)$/'",
        r"'/(*UTF)^\w$/'",
        "\"/^a\\xA0b$/x\"",
        // A caseless flag compares through the table under UCP too; a named back reference, an
        // `x` comment's swallowed tokens, `[[:<:]]`, `[:ascii:]` and the `r` flag are readers.
        r"'/(*UCP)^\xC4$/i'",
        "'/^i$/iu'",
        "'/^(?<a>.)(?P=a)$/i'",
        "'/[[:<:]]a/'",
        "'/^[[:ascii:]]$/u'",
        "\"/^#\\Q\n\\w$/x\"",
        r"'/^\w$/r'",
    ] {
        for call in calls(pattern) {
            reads(&call);
        }
    }
    for pattern in [
        r"'/^\w$/u'",
        "'/^[[:alpha:]]$/u'",
        r"'/^\d$/'",
        "'/^[[:digit:]]$/'",
        "'/^[[:xdigit:]]$/'",
        "'/^[a-z]$/'",
        "\"/^\\xE4$/\"",
        r"'/^\p{L}$/'",
        r"'/^\h$/'",
        "'/^.$/'",
        "'/^a b$/x'",
    ] {
        for call in calls(pattern) {
            silent(&call);
        }
    }
}

/// `preg_replace_callback_array` takes its patterns as the keys of the map.
#[test]
fn preg_replace_callback_array_reads_the_keys() {
    reads_with_callbacks(r"preg_replace_callback_array(['/\w/' => fn ($m) => 'y'], 'a')");
    reads_with_callbacks(
        r"preg_replace_callback_array(['/a/' => fn ($m) => 'y', '/\s/' => fn ($m) => 'z'], 'a')",
    );
    silent_with_callbacks(r"preg_replace_callback_array(['/\w/u' => fn ($m) => 'y'], 'a')");
    silent_with_callbacks(
        r"preg_replace_callback_array(['/\d/' => fn ($m) => 'y', '/a/' => fn ($m) => 'z'], 'a')",
    );
    // A computed key, or a map with no key at all, is a pattern the site cannot read.
    depends(r"preg_replace_callback_array(['/a/' => fn ($m) => 'y', $p => fn ($m) => 'z'], 'a')");
}

/// An array of patterns reads if any of them does; an array with an element the site cannot read
/// as a literal is the gap whole (the other patterns would be compiled first, but a bad one stops
/// the loop, so a later reader is not certain to run).
#[test]
fn an_array_of_patterns_reads_if_any_pattern_does() {
    reads(r"preg_replace(['/a/', '/\s/'], '_', 'a')");
    reads(r"preg_replace(['/a/', '/b/i'], '_', 'a')");
    reads(r"preg_filter(['/\w/', '/b/'], ['x', 'y'], 'a')");
    silent(r"preg_replace(['/a/', '/b/', '/\d/u'], '_', 'a')");
    silent("preg_replace([], '_', 'a')");
    depends(r"preg_replace(['/a/', $p], '_', 'a')");
    depends(r"preg_replace(['/a/', $p, '/\s/'], '_', 'a')");
    depends(r"preg_replace(['/a/', '/b/' . $p], '_', 'a')");
    depends(r"preg_replace(['/a/', ...$s], '_', 'a')");
    depends(r"preg_replace(['/a/', ['/b/']], '_', 'a')");
}

/// A pattern the site cannot read as a literal is the gap, and so is one the reader declines
/// (PCRE2 would refuse to compile it): never a label.
#[test]
fn a_pattern_the_site_cannot_read_is_the_gap() {
    for call in [
        "preg_match($p, 'a')",
        "preg_match('/' . $p . '/', 'a')",
        "preg_match(\"/{$p}/\", 'a')",
        "preg_match(self::PATTERN, 'a')",
        "preg_replace($p, '_', 'a')",
        "preg_split($p, 'a')",
        "preg_match(pattern: '/\\w/', subject: 'a')",
        "preg_match(...$s)",
        "preg_match('/[/', 'a')",
        r"preg_match('/a\y/', 'a')",
        "preg_match('/a/e', 'a')",
    ] {
        let s = summary(&format!("return {call};"));
        assert!(s.labels.is_empty(), "{call}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    }
}

/// `preg_quote` compiles nothing and keeps its empty row; the two error readers have no row and
/// gain no read.
#[test]
fn what_compiles_nothing_reads_nothing() {
    silent("preg_quote(\"\\xE4.\")");
    silent("preg_quote('a.b', '/')");
    for call in ["preg_last_error()", "preg_last_error_msg()"] {
        let s = summary(&format!("return {call};"));
        assert!(s.labels.is_empty() && !s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    }
}

/// A preg function handed over as a callback is called with a pattern of the invoker's choosing.
#[test]
fn a_preg_function_handed_over_as_a_callback_is_the_gap() {
    for callee in ["preg_match", "preg_grep", "preg_split"] {
        let s = summary(&format!("return array_map('{callee}', $s);"));
        assert!(!s.labels.iter().any(|l| l == READ), "{callee}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{callee}: {s:?}");
    }
    let s = summary("return array_map('preg_quote', $s);");
    assert!(!s.gaps.contains(&DEPENDS), "{s:?}");
}

/// A body that holds a proven read carries it up to its callers, and one that holds none stays
/// pure: the label is the callee's, so a caller of `f` is as exhaustive as `f`.
#[test]
fn the_read_propagates_to_a_caller() {
    let src = "<?php\nfunction reader() { return preg_match('/\\s/', 'a'); }\n\
               function plain() { return preg_match('/\\d/', 'a'); }\n\
               function top() { return reader() + plain(); }\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    let summaries = effect_summary(&tree, &functions, &classes);
    let of = |name: &str| summaries.iter().find(|s| s.symbol == name).expect(name);
    assert_eq!(of("reader").labels, [READ]);
    assert!(of("plain").labels.is_empty() && of("plain").exhaustive);
    assert_eq!(of("top").labels, [READ], "{:?}", of("top"));
}

/// An envelope that does not admit the read is exceeded at the call with a proven read, and one
/// that admits it by prefix, a `u` pattern and a pattern the site cannot read are not (ADR-0101
/// §3.5: only a proven read is a default-floor finding).
#[test]
fn a_pure_envelope_over_a_reading_pattern_is_exceeded() {
    let pure = |body: &str| {
        format!("<?php\n#[\\Steins\\Pure]\nfunction f(string $p): int|false {{ return {body}; }}\n")
    };
    let d = findings(&pure(r"preg_match('/\s/', 'a b')"));
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains(READ), "{}", d[0].message);
    for body in [
        r"preg_match('/\s/u', 'a b')",
        r"preg_match('/\d/', '1')",
        "preg_match($p, 'a b')",
        "preg_quote('a')",
    ] {
        assert!(findings(&pure(body)).is_empty(), "{body}: {:#?}", findings(&pure(body)));
    }
    let admitted = "<?php\n#[\\Steins\\Effects('global.read')]\nfunction f(): int|false { return preg_match('/\\s/', 'a'); }\n";
    assert!(findings(admitted).is_empty());
}
