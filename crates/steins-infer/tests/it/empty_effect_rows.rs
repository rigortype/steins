//! The empty effect rows of ADR-0101 §3.20 (issue #1000): the scale-free bcmath rounders, a
//! built `DateTime`'s or `DateTimeImmutable`'s readers and setters, `DateTimeZone`'s constructor,
//! `getName` and `getOffset`, and `DateTime::createFromTimestamp` read neither the default zone nor
//! the clock, so a `@phpstan-pure` function over them is exhaustive and silent. Two limits are
//! pinned beside it. A non-literal object argument that a coercive string parameter converts through
//! `__toString` still runs user code, so the call is not exhaustive. A bound receiver (a declared
//! `DateTime` parameter) may be a subclass that overrides the method, so it keeps the gap.
//!
//! The witness rows of the catalog's `effects.rs` tests and the PHP 8.5.11 probes of the S6b-2
//! review decide each empty row; these tests pin that the call site reaches it.

use steins_infer::{EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

fn summary_of(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol} in {src}"))
}

/// `f({params})` runs `body`, under `@phpstan-pure`. The body is statements, so a local can
/// hold the built value.
fn pure_f(params: &str, body: &str) -> String {
    format!("<?php\n/** @phpstan-pure */\nfunction f({params}) {{ {body} }}\n")
}

/// [`pure_f`] in a file under `declare(strict_types=1)`. A method call on a `new` receiver
/// records no operand shapes, so its `string` parameter reads blind in coercive code; strict
/// typing is what rules the coercion out (ADR-0099 §4.2, as `builtin_user_code_reach` does).
fn strict_pure_f(params: &str, body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n/** @phpstan-pure */\nfunction f({params}) {{ {body} }}\n")
}

/// `f` carries no label, is exhaustive, has no gap, and draws no envelope finding.
fn exhaustive_and_silent(src: &str) {
    let s = summary_of(src, "f");
    assert!(s.labels.is_empty() && s.exhaustive && s.gaps.is_empty(), "{src}\n{s:?}");
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let findings: Vec<_> = check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == EFFECT_ID)
        .collect();
    assert!(findings.is_empty(), "{src}\n{findings:#?}");
}

#[test]
fn the_scale_free_rounders_are_exhaustive_and_silent() {
    for call in ["bcceil('1.5')", "bcfloor('-1.5')", "bcround('1.5')", "bcround('1.2345', 2)"] {
        exhaustive_and_silent(&pure_f("", &format!("return {call};")));
    }
}

/// The receiver must name its class exactly (`(new DateTime(...))->m()`): a variable the frame
/// writes names no class to the engine (ADR-0067), so the row answers only the exact spelling.
/// The file is strict, which rules out the `__toString` coercion the `string` parameters allow.
#[test]
fn a_built_value_read_or_set_in_place_is_exhaustive_and_silent() {
    for body in [
        "return (new DateTime('@0'))->format('Y');",
        "return (new DateTimeImmutable('@0'))->format('Y-m-d H:i:s e');",
        "return (new DateTime('@0'))->getTimestamp();",
        "return (new DateTime('@0'))->getTimezone();",
        "return (new DateTime('@0'))->getOffset();",
        "return (new DateTime('@0'))->setTimestamp(1700000000);",
        "return (new DateTime('@0'))->setTime(10, 20, 30);",
        "return (new DateTime('@0'))->setDate(2020, 2, 29);",
        "return (new DateTime('@0'))->setISODate(2020, 10, 3);",
        "return (new DateTime('@0'))->setTimezone(new DateTimeZone('Europe/Paris'));",
        "return (new DateTime('@0'))->modify('tomorrow');",
        "return (new DateTime('@0'))->modify('now');",
        "return (new DateTime('@0'))->modify('+1 day');",
        "return (new DateTimeImmutable('@0'))->modify('first day of next month');",
    ] {
        exhaustive_and_silent(&strict_pure_f("", body));
    }
}

/// In coercive code the same literal-argument call keeps the gap: its `string` parameter reads
/// blind on a `new` receiver, since the site records no operand shapes for it. Pinned so that a
/// later widening of the shapes shows up here as a deliberate change.
#[test]
fn a_literal_string_argument_on_a_new_receiver_is_blind_in_coercive_code() {
    let s = summary_of(&pure_f("", "return (new DateTime('@0'))->format('Y');"), "f");
    assert!(!s.exhaustive && s.gaps == ["user-code-reach"], "{s:?}");
}

#[test]
fn the_zone_and_the_timestamp_factory_are_exhaustive_and_silent() {
    for body in [
        "return (new DateTimeZone('Europe/London'))->getName();",
        "return (new DateTimeZone('+02:00'))->getOffset(new DateTime('@0'));",
        "return new DateTimeZone('UTC');",
        "return DateTime::createFromTimestamp(1719835200);",
    ] {
        exhaustive_and_silent(&pure_f("", body));
    }
}

/// A variable the frame writes names no class, so the row does not reach `$d->format()` and the
/// call keeps its gap. The precision is the engine's, not the row's, and this pins it.
#[test]
fn a_variable_receiver_names_no_class_and_keeps_the_gap() {
    let s = summary_of(&pure_f("", "$d = new DateTime('@0'); return $d->format('Y');"), "f");
    assert!(!s.exhaustive, "a written variable is a dynamic receiver: {s:?}");
}

#[test]
fn an_object_argument_that_coerces_still_reaches_user_code() {
    let class = "final class Name { public function __toString(): string { return 'x'; } }\n";
    for body in [
        "return bcround($o);",
        "return (new DateTime('@0'))->format($o);",
        "return (new DateTime('@0'))->modify($o);",
        "return new DateTimeZone($o);",
    ] {
        let src = format!("<?php\n{class}/** @phpstan-pure */\nfunction f(Name $o) {{ {body} }}\n");
        let s = summary_of(&src, "f");
        assert!(!s.exhaustive, "an object argument runs __toString: {body}: {s:?}");
    }
}

#[test]
fn a_bound_receiver_keeps_the_gap() {
    // `$d` may be a subclass of `DateTime` that overrides `format`, so the row does not answer.
    let s = summary_of(&pure_f("DateTime $d", "return $d->format('Y');"), "f");
    assert!(!s.exhaustive, "a bound receiver is open: {s:?}");
}
