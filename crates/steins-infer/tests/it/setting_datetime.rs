//! The `DateTime` constructors in the effect lane (ADR-0101 §3.17, issue #1000, S6b-2): `new
//! DateTime(...)`, `new DateTimeImmutable(...)`, `createFromFormat` and the `date_create*`
//! spellings carry the default zone and the clock as the upper bound, and the call drops each
//! read its arguments rule out: a zone object shown passed drops the zone, a literal that names
//! its zone and every field drops it, a literal that fills every field drops the clock, a format
//! with a reset drops the clock. Anything else keeps the label, and no call is a gap.
//!
//! The witness rows of `steins-catalog`'s `setting_datetime_oracle` decide each verdict against
//! PHP; these tests pin that the call site reaches it through every call shape.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

const ZONE: &str = "global.read.setting.timezone";
const CLOCK: &str = "nondet.time";

fn summary_of(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol} in {src}"))
}

/// `f({params}) { return {call}; }` carries exactly `labels` and stays exhaustive, with no gap.
fn proves(params: &str, call: &str, labels: &[&str]) {
    let s = carries(params, call, labels);
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// `f({params}) { return {call}; }` carries exactly `labels`, whatever else the body leaves open.
fn carries(params: &str, call: &str, labels: &[&str]) -> EffectSummary {
    let src = format!("<?php\nfunction f({params}) {{ return {call}; }}\n");
    let s = summary_of(&src, "f");
    assert_eq!(s.labels, labels, "{call}: {s:?}");
    s
}

/// The string decides both reads when no zone object is passed.
#[test]
fn the_literal_string_decides_the_zone_and_the_clock() {
    for class in ["\\DateTime", "\\DateTimeImmutable", "DateTime"] {
        proves("", &format!("new {class}()"), &[ZONE, CLOCK]);
        proves("", &format!("new {class}"), &[ZONE, CLOCK]);
        proves("", &format!("new {class}('now')"), &[ZONE, CLOCK]);
        proves("", &format!("new {class}('tomorrow')"), &[ZONE, CLOCK]);
        proves("", &format!("new {class}('2020-01-01')"), &[ZONE]);
        proves("", &format!("new {class}('2020-01-01 10:00:00')"), &[ZONE]);
        proves("", &format!("new {class}('2020-01-01T10:00:00.5')"), &[ZONE]);
        proves("", &format!("new {class}('@0')"), &[]);
        proves("", &format!("new {class}('@1700000000')"), &[]);
        proves("", &format!("new {class}('2020-01-01 10:00 UTC')"), &[]);
        proves("", &format!("new {class}('2020-01-01T10:00:00+01:00')"), &[]);
        proves("", &format!("new {class}('2020-01-01T10:00:00Z')"), &[]);
        proves("", &format!("new {class}('now UTC')"), &[CLOCK]);
        // A zone that is not an identifier copies the default zone's fields for `now`, and a
        // shape the grammar does not read keeps both.
        proves("", &format!("new {class}('now GMT')"), &[ZONE, CLOCK]);
        proves("", &format!("new {class}('2020-01-01 est')"), &[ZONE, CLOCK]);
        proves("", &format!("new {class}('+1 day')"), &[ZONE, CLOCK]);
    }
    for name in ["date_create", "date_create_immutable"] {
        proves("", &format!("{name}('2020-01-01')"), &[ZONE]);
        proves("", &format!("{name}('@0')"), &[]);
        proves("", &format!("{name}('now UTC')"), &[CLOCK]);
        proves("", &format!("{name}()"), &[ZONE, CLOCK]);
    }
}

/// A zone object the call shows passed drops the zone whatever the string is; one that may be
/// `null` leaves it to the string.
#[test]
fn a_zone_object_shown_passed_drops_the_zone() {
    // `new DateTimeZone(...)` has no row of its own, so the body is not exhaustive.
    let utc = "new \\DateTimeZone('UTC')";
    carries("", &format!("new \\DateTime('now', {utc})"), &[CLOCK]);
    carries("", &format!("new \\DateTime('2020-01-01', {utc})"), &[]);
    carries("", &format!("date_create_immutable('tomorrow', {utc})"), &[CLOCK]);
    proves("\\DateTimeZone $tz", "new \\DateTime('now', $tz)", &[CLOCK]);
    // The function spellings reach a user subclass of `DateTimeZone` (`user-code-reach`).
    carries("\\DateTimeZone $tz", "date_create_immutable('tomorrow', $tz)", &[CLOCK]);
    proves("\\DateTimeZone $tz", "new \\DateTime('2020-01-01', $tz)", &[]);
    proves("string $s, \\DateTimeZone $tz", "new \\DateTimeImmutable($s, $tz)", &[CLOCK]);
    proves("?\\DateTimeZone $tz", "new \\DateTime('2020-01-01', $tz)", &[ZONE]);
    proves("\\DateTimeZone $tz = null", "new \\DateTime('2020-01-01', $tz)", &[ZONE]);
    proves("$tz", "new \\DateTime('2020-01-01', $tz)", &[ZONE]);
    proves("$tz", "new \\DateTime('@0', $tz)", &[]);
    proves("", "new \\DateTime('2020-01-01', null)", &[ZONE]);
}

/// A string the call does not spell keeps both labels and raises no gap.
#[test]
fn an_unread_string_keeps_both_labels() {
    proves("string $s", "new \\DateTime($s)", &[ZONE, CLOCK]);
    proves("string $s", "date_create($s)", &[ZONE, CLOCK]);
    proves("string $s", "new \\DateTime('@' . $s)", &[ZONE, CLOCK]);
    // A named or spread argument list is read for nothing.
    carries("", "new \\DateTime(datetime: '@0')", &[ZONE, CLOCK]);
    carries("array $a", "new \\DateTime(...$a)", &[ZONE, CLOCK]);
    // An alias is resolved by the engine, but the scan reads the arguments of the two spellings
    // only: the row stays whole.
    let src = "<?php\nuse DateTimeImmutable as Instant;\nfunction f() { return new Instant('@0'); }\n";
    let s = summary_of(src, "f");
    assert_eq!(s.labels, [ZONE, CLOCK], "{s:?}");
}

/// `createFromFormat` drops the clock for a format that resets the fields it does not parse, and
/// the zone for a zone object; the format's own zone is not read.
#[test]
fn the_format_factories() {
    let params = "string $s, \\DateTimeZone $tz";
    for spelling in ["\\DateTime::createFromFormat", "date_create_immutable_from_format"] {
        proves(params, &format!("{spelling}('Y-m-d', $s)"), &[ZONE, CLOCK]);
        proves(params, &format!("{spelling}('!Y-m-d', $s)"), &[ZONE]);
        proves(params, &format!("{spelling}('Y-m-d|', $s)"), &[ZONE]);
        proves(params, &format!("{spelling}('\\\\!Y-m-d', $s)"), &[ZONE, CLOCK]);
        proves(params, &format!("{spelling}('Y-m-d e', $s)"), &[ZONE, CLOCK]);
        // A `!` after a zone conversion keeps the zone flag and the clock's DST flag (§3.17).
        proves(params, &format!("{spelling}('e !Y-m-d H:i', $s)"), &[ZONE, CLOCK]);
        proves(params, &format!("{spelling}('!Y-m-d H:i e', $s)"), &[ZONE]);
        carries(params, &format!("{spelling}('Y-m-d', $s, $tz)"), &[CLOCK]);
        carries(params, &format!("{spelling}('!Y-m-d', $s, $tz)"), &[]);
        proves(params, &format!("{spelling}('!Y-m-d', $s, null)"), &[ZONE]);
    }
}

/// A subclass's `parent::__construct(...)` reads its own call's arguments, and a project
/// constructor that forwards a variable keeps the row.
#[test]
fn a_subclass_forwarding_to_the_engine() {
    let src = "<?php\nclass Day extends \\DateTimeImmutable {\n    \
               public function __construct() { parent::__construct('2020-01-01'); }\n}\n\
               class Instant extends \\DateTime {\n    \
               public function __construct(string $s, \\DateTimeZone $z) { parent::__construct($s, $z); }\n}\n\
               class Moment extends \\DateTime {\n    \
               public function __construct(string $s) { parent::__construct($s); }\n}\n\
               function day() { return new Day(); }\n\
               function moment() { return new Moment('@0'); }\n";
    assert_eq!(summary_of(src, "Day::__construct").labels, [ZONE]);
    assert_eq!(summary_of(src, "Instant::__construct").labels, [CLOCK]);
    assert_eq!(summary_of(src, "Moment::__construct").labels, [ZONE, CLOCK]);
    assert_eq!(summary_of(src, "day").labels, [ZONE]);
    // The literal reaches the project constructor, not the engine's: the row stays whole.
    assert_eq!(summary_of(src, "moment").labels, [ZONE, CLOCK]);
}

/// The copying factories read neither.
#[test]
fn the_copying_factories_read_neither() {
    proves("\\DateTime $d", "\\DateTimeImmutable::createFromMutable($d)", &[]);
    proves("\\DateTimeInterface $d", "\\DateTime::createFromInterface($d)", &[]);
}
