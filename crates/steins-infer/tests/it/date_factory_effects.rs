//! Every spelling of "build a date from a string" reads the clock the way
//! `new DateTime(...)` does (issue #848): `date_create()` and its immutable
//! twin, the `*_from_format` functions, and the static `createFromFormat`
//! factories all take the constructors' argument-blind `nondet.time`. The
//! factories that copy an existing value read nothing.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

fn exceeded(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn one(src: &str) -> Diagnostic {
    let f = exceeded(src);
    assert_eq!(f.len(), 1, "expected exactly one envelope finding, got: {f:#?}");
    f.into_iter().next().unwrap()
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

#[test]
fn date_create_reads_the_clock_as_new_datetime_does() {
    let src = "<?php\n/** @pure */\nfunction now(): \\DateTime|false { return date_create(); }\n";
    let d = one(src);
    assert_eq!(d.message, "date_create() has effect nondet.time, but now() is declared @phpstan-pure");
    let s = summary(src, "now");
    assert_eq!(s.labels, ["nondet.time"]);
    assert!(s.exhaustive, "{s:?}");
    // The constructor it spells, for comparison.
    let ctor = "<?php\n/** @pure */\nfunction now(): \\DateTime { return new \\DateTime(); }\n";
    assert_eq!(summary(ctor, "now").labels, s.labels);
}

#[test]
fn every_function_spelling_takes_the_row() {
    for (call, name) in [
        ("date_create('2020-01-01')", "date_create"),
        ("date_create_immutable()", "date_create_immutable"),
        ("date_create_from_format('Y-m-d', $s)", "date_create_from_format"),
        ("date_create_immutable_from_format('!Y-m-d', $s)", "date_create_immutable_from_format"),
    ] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): object|false {{ return {call}; }}\n"
        );
        let d = one(&src);
        let expected = format!("{name}() has effect nondet.time, but f() is declared #[\\Steins\\Pure]");
        assert_eq!(d.message, expected, "argument-blind, like `date`");
    }
}

#[test]
fn the_static_format_factories_take_the_row() {
    for class in ["DateTime", "DateTimeImmutable"] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\n\
             function f(string $s): object|false {{ return \\{class}::createFromFormat('Y-m-d', $s); }}\n"
        );
        let d = one(&src);
        let expected = format!("{class}::createFromFormat() has effect nondet.time");
        assert!(d.message.starts_with(&expected), "{}", d.message);
        assert!(summary(&src, "f").exhaustive);
    }
}

#[test]
fn a_subclass_that_does_not_override_the_factory_runs_the_engines() {
    let src = "<?php\nclass Moment extends \\DateTimeImmutable {}\n\
               #[\\Steins\\Pure]\nfunction f(string $s): object|false { return Moment::createFromFormat('Y', $s); }\n\
               class Instant extends \\DateTime {\n    \
               #[\\Steins\\Pure]\n    \
               public static function parse(string $s): static|false { return parent::createFromFormat('Y', $s); }\n}\n";
    let f = exceeded(src);
    let messages: Vec<&str> = f.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 2, "{f:#?}");
    assert!(messages[0].starts_with("Moment::createFromFormat() has effect nondet.time"), "{messages:?}");
    assert!(messages[1].starts_with("parent::createFromFormat() has effect nondet.time"), "{messages:?}");
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
