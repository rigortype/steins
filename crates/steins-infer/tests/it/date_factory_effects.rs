//! Every spelling of "build a date from a string" reads the default zone and
//! the clock the way `new DateTime(...)` does (issue #848): `date_create()` and
//! its immutable twin, the `*_from_format` functions, and the static
//! `createFromFormat` factories all take the constructors' row, which the call
//! site narrows by its string, format and zone object (ADR-0101 §3.17). The
//! factories that copy an existing value read nothing.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

fn exceeded(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// The labels of every envelope finding, in order.
fn labels_exceeded(src: &str) -> Vec<String> {
    exceeded(src)
        .iter()
        .map(|d| d.message.split(" has effect ").nth(1).unwrap_or("").to_owned())
        .map(|rest| rest.split([',', ' ']).next().unwrap_or("").to_owned())
        .collect()
}

const ZONE: &str = "global.read.setting.timezone";
const CLOCK: &str = "nondet.time";

#[test]
fn date_create_reads_the_zone_and_the_clock_as_new_datetime_does() {
    let src = "<?php\n/** @pure */\nfunction now(): \\DateTime|false { return date_create(); }\n";
    let f = exceeded(src);
    let messages: Vec<&str> = f.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "date_create() has effect global.read.setting.timezone, but now() is declared @phpstan-pure",
            "date_create() has effect nondet.time, but now() is declared @phpstan-pure",
        ]
    );
    let s = summary(src, "now");
    assert_eq!(s.labels, [ZONE, CLOCK]);
    assert!(s.exhaustive, "{s:?}");
    // The constructor it spells, for comparison.
    let ctor = "<?php\n/** @pure */\nfunction now(): \\DateTime { return new \\DateTime(); }\n";
    assert_eq!(summary(ctor, "now").labels, s.labels);
}

/// Each function spelling takes the row and narrows it by its string or format (ADR-0101 §3.17):
/// a date with no zone keeps the zone and drops the clock, a format that resets its fields drops
/// the clock, and a format that does not keeps both.
#[test]
fn every_function_spelling_takes_the_row() {
    for (call, expected) in [
        ("date_create('2020-01-01')", &[ZONE][..]),
        ("date_create_immutable()", &[ZONE, CLOCK][..]),
        ("date_create_from_format('Y-m-d', $s)", &[ZONE, CLOCK][..]),
        ("date_create_immutable_from_format('!Y-m-d', $s)", &[ZONE][..]),
    ] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): object|false {{ return {call}; }}\n"
        );
        assert_eq!(labels_exceeded(&src), expected, "{call}");
        assert!(summary(&src, "f").exhaustive, "{call}");
    }
}

#[test]
fn the_static_format_factories_take_the_row() {
    for class in ["DateTime", "DateTimeImmutable"] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\n\
             function f(string $s): object|false {{ return \\{class}::createFromFormat('Y-m-d', $s); }}\n"
        );
        let f = exceeded(&src);
        assert_eq!(f.len(), 2, "{f:#?}");
        let expected = format!("{class}::createFromFormat() has effect {ZONE}");
        assert!(f[0].message.starts_with(&expected), "{}", f[0].message);
        let expected = format!("{class}::createFromFormat() has effect {CLOCK}");
        assert!(f[1].message.starts_with(&expected), "{}", f[1].message);
        assert!(summary(&src, "f").exhaustive);
        // A zone object and a resetting format leave nothing.
        let reset = format!(
            "<?php\n#[\\Steins\\Pure]\n\
             function f(string $s): object|false {{ \
             return \\{class}::createFromFormat('!Y-m-d', $s, new \\DateTimeZone('UTC')); }}\n"
        );
        assert!(exceeded(&reset).is_empty(), "{:#?}", exceeded(&reset));
    }
}

#[test]
fn a_subclass_that_does_not_override_the_factory_runs_the_engines() {
    let src = "<?php\nclass Moment extends \\DateTimeImmutable {}\n\
               #[\\Steins\\Pure]\nfunction f(string $s): object|false { return Moment::createFromFormat('Y', $s); }\n\
               class Instant extends \\DateTime {\n    \
               #[\\Steins\\Pure]\n    \
               public static function parse(string $s): static|false { return parent::createFromFormat('Y|', $s); }\n}\n";
    let f = exceeded(src);
    let messages: Vec<&str> = f.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 3, "{f:#?}");
    assert!(messages[0].starts_with("Moment::createFromFormat() has effect global.read.setting.timezone"), "{messages:?}");
    assert!(messages[1].starts_with("Moment::createFromFormat() has effect nondet.time"), "{messages:?}");
    // `parent::` reads the arguments of its own call: the reset drops the clock.
    assert!(messages[2].starts_with("parent::createFromFormat() has effect global.read.setting.timezone"), "{messages:?}");
}

#[test]
fn the_copying_factories_read_nothing() {
    let src = "<?php\n#[\\Steins\\Pure]\n\
               function thaw(\\DateTimeImmutable $d): \\DateTime { return \\DateTime::createFromImmutable($d); }\n\
               #[\\Steins\\Pure]\n\
               function freeze(\\DateTime $d): \\DateTimeImmutable { return \\DateTimeImmutable::createFromMutable($d); }\n\
               #[\\Steins\\Pure]\n\
               function same(\\DateTimeInterface $d): \\DateTime { return \\DateTime::createFromInterface($d); }\n";
    assert!(exceeded(src).is_empty(), "{:#?}", exceeded(src));
    for symbol in ["thaw", "freeze", "same"] {
        let s = summary(src, symbol);
        assert!(s.labels.is_empty() && s.exhaustive, "{symbol}: {s:?}");
    }
}
