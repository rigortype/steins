//! The `DateTime` constructors and their function spellings (ADR-0101 §3.17, issue #1000,
//! S6b-2).
//!
//! `new DateTime($s, $tz)`, `new DateTimeImmutable(...)`, `date_create(...)`,
//! `date_create_immutable(...)`, the static `createFromFormat` factories and
//! `date_create*_from_format` all run `php_date_initialize` (php-src `php-8.5.11`,
//! `ext/date/php_date.c`). Their row is `{global.read.setting.timezone, nondet.time}`, an upper
//! bound over two reads the call decides:
//!
//! * **The zone.** With a `DateTimeZone` passed, the object's zone is the one used and the default
//!   is never consulted. Without one, the default zone (`get_timezone_info()`) builds the
//!   `now` the parsed string's missing fields are filled from, and it is the zone of the result
//!   unless the string names one. A string that names a zone still reads the default where it
//!   leaves a field to `now`: `new DateTime('now GMT')` copies the default zone's wall clock and
//!   moves with it, and so do `'today utc'`, `'now Z'`, `'now +01:00'`. Only an identifier zone
//!   (`UTC` spelled in capitals, `Europe/London`) builds `now` in the named zone. So a literal is
//!   **none** only when every field comes from it ([`classify`]): an absolute date, with or
//!   without a time, followed by a zone; a keyword followed by ` UTC`; an `@` timestamp, whose
//!   zone is `+00:00` whatever zone is passed. A keyword alone, or an absolute date naming no
//!   zone, is the **proven** read. Any other literal, and any string the call does not spell, is
//!   undecided: timelib reads `est`, `cet`, `a` (a military zone), `Japan`, `Zulu`, `-05` and
//!   `+5` as zones, so no lexical blacklist is sound.
//! * **The clock.** `php_date_initialize` calls `php_time()` on every construction, and
//!   `timelib_fill_holes` copies from it only the fields the string leaves unset. An absolute
//!   date fills every field (a date without a time zeroes the time, and the microseconds are
//!   zeroed whenever a field is set), and an `@` timestamp does too, so the value does not depend
//!   on when the call runs: the clock is dropped there. The seed's DST flag, which keeps the clock
//!   on `mktime`, is consulted only for a time that names its zone (`do_adjust_timezone` reads
//!   `tz->dst` under `have_zone`), where the parsed zone has set it. A keyword reads it. For a
//!   `createFromFormat` call, a format holding an unescaped `!` or `|` resets every field it does
//!   not parse to the epoch, so the clock is dropped; any other format keeps it.
//!
//! An undecided call **keeps** both labels and raises no gap, as the clock gate does (§3.14): the
//! row is the upper bound the call only narrows. The second argument is the zone object; a
//! value shown not to be `null` is a `DateTimeZone` (the parameter is `?DateTimeZone`, and any
//! other value throws `TypeError` before either read), so it drops the zone read whatever the
//! string is. The factories that copy a built value (`createFromMutable`, `createFromImmutable`,
//! `createFromInterface`) read neither and keep their empty row.

use super::GateArg;

/// Which arguments of a constructor-side call decide its two reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateGate {
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `($datetime = "now", ?DateTimeZone $timezone = null)`: the constructors, `date_create`
    /// and `date_create_immutable`.
    Text,
    /// `($format, $datetime, ?DateTimeZone $timezone = null)`: `createFromFormat` and the
    /// `date_create*_from_format` functions.
    Format,
}

/// The gate of the builtin function `name` (case-insensitive), or `None` for any other name.
#[must_use]
pub fn date_gate(name: &str) -> Option<DateGate> {
    let kind = match name.to_ascii_lowercase().as_str() {
        "date_create" | "date_create_immutable" => Kind::Text,
        "date_create_from_format" | "date_create_immutable_from_format" => Kind::Format,
        _ => return None,
    };
    Some(DateGate { kind })
}

/// The gate of the engine method `class::method` (case-insensitive), or `None` for any other.
#[must_use]
pub fn date_method_gate(class: &str, method: &str) -> Option<DateGate> {
    let class = class.trim_start_matches('\\').to_ascii_lowercase();
    if !matches!(class.as_str(), "datetime" | "datetimeimmutable") {
        return None;
    }
    let kind = match method.to_ascii_lowercase().as_str() {
        "__construct" => Kind::Text,
        "createfromformat" => Kind::Format,
        _ => return None,
    };
    Some(DateGate { kind })
}

impl DateGate {
    /// The positional indexes of the deciding arguments, in the order [`Self::reads_zone`] and
    /// [`Self::reads_clock`] take them: the string (or the format), then the zone object.
    #[must_use]
    pub const fn positions(self) -> &'static [usize] {
        match self.kind {
            Kind::Text => &[0, 1],
            Kind::Format => &[0, 2],
        }
    }

    /// Whether the call reads the default zone: `Some(false)` for a zone object shown passed, or a
    /// literal whose every field and zone is its own; `Some(true)` for no zone object and a
    /// literal that names none; `None` otherwise, the label kept.
    #[must_use]
    pub fn reads_zone(self, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        let text = match (self.kind, args.first().copied().flatten()) {
            (Kind::Text, Some(GateArg::Omitted)) => Some(true),
            (Kind::Text, Some(GateArg::Str(text))) => classify(text).zone,
            _ => None,
        };
        match args.get(1).copied().flatten() {
            Some(GateArg::Omitted | GateArg::Null) => text,
            Some(_) => Some(false),
            None => text.filter(|reads| !reads),
        }
    }

    /// Whether the call reads the clock: `Some(false)` for a literal that fills every field, or a
    /// format that resets the ones it does not parse; `Some(true)` for an omitted string or a
    /// keyword; `None` otherwise, the label kept.
    #[must_use]
    pub fn reads_clock(self, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        match (self.kind, args.first().copied().flatten()?) {
            (Kind::Text, GateArg::Omitted) => Some(true),
            (Kind::Text, GateArg::Str(text)) => classify(text).clock,
            (Kind::Format, GateArg::Str(format)) => resets_fields(format).then_some(false),
            _ => None,
        }
    }
}

/// What a literal date string shows of the two reads, each `None` where it does not decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shown {
    /// Whether the default zone is read, absent a zone object.
    zone: Option<bool>,
    /// Whether the clock is read.
    clock: Option<bool>,
}

/// The keywords that build the value from `now`: the empty string is `"now"`.
const KEYWORDS: [&str; 7] = ["", "now", "today", "midnight", "noon", "tomorrow", "yesterday"];

/// The literal string `text`, read by the grammar this module states and nothing wider:
///
/// * `@` timestamp (`@0`, `@-5`, `@1.5`): no zone read, no clock;
/// * a keyword: both read; a keyword (not the empty one) then ` UTC` in capitals: the clock only;
/// * `YYYY-MM-DD`, optionally then a space or `T` and `HH:MM`, `HH:MM:SS` or `HH:MM:SS.f` (one to
///   six digits): the zone read, no clock; then ` UTC` or ` GMT` (any case), or after a time
///   `Z` or an offset (`+01`, `+0100`, `+01:00`, `+1`, optionally after one space): neither.
///
/// Anything else decides nothing.
fn classify(text: &str) -> Shown {
    const NOTHING: Shown = Shown { zone: None, clock: None };
    const NEITHER: Shown = Shown { zone: Some(false), clock: Some(false) };
    if let Some(stamp) = text.strip_prefix('@') {
        return if is_timestamp(stamp) { NEITHER } else { NOTHING };
    }
    if KEYWORDS.contains(&text) {
        return Shown { zone: Some(true), clock: Some(true) };
    }
    if let Some(word) = text.strip_suffix(" UTC")
        && !word.is_empty()
        && KEYWORDS.contains(&word)
    {
        return Shown { zone: Some(false), clock: Some(true) };
    }
    let Some((timed, rest)) = absolute(text.as_bytes()) else { return NOTHING };
    if rest.is_empty() {
        return Shown { zone: Some(true), clock: Some(false) };
    }
    if names_zone(rest, timed) { NEITHER } else { NOTHING }
}

/// `-?\d+(\.\d+)?`: the timestamp after the `@`.
fn is_timestamp(stamp: &str) -> bool {
    let digits = stamp.strip_prefix('-').unwrap_or(stamp);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, "1"));
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    all_digits(whole) && all_digits(fraction)
}

/// `YYYY-MM-DD`, then optionally a space or `T` and a time: whether a time was read, and the
/// bytes left. `None` when the text does not start so.
fn absolute(bytes: &[u8]) -> Option<(bool, &[u8])> {
    let rest = digits(bytes, 4)?;
    let rest = digits(rest.strip_prefix(b"-")?, 2)?;
    let rest = digits(rest.strip_prefix(b"-")?, 2)?;
    let Some(after) = rest.strip_prefix(b" ").or_else(|| rest.strip_prefix(b"T")) else {
        return Some((false, rest));
    };
    let Some(time) = digits(after, 2).and_then(|r| r.strip_prefix(b":")).and_then(|r| digits(r, 2))
    else {
        return Some((false, rest));
    };
    let Some(seconds) = time.strip_prefix(b":").and_then(|r| digits(r, 2)) else {
        return Some((true, time));
    };
    let Some(fraction) = seconds.strip_prefix(b".") else { return Some((true, seconds)) };
    let count = fraction.iter().take_while(|b| b.is_ascii_digit()).count();
    (1..=6).contains(&count).then_some((true, &fraction[count..]))
}

/// The bytes after exactly `n` ASCII digits at the start of `bytes`.
fn digits(bytes: &[u8], n: usize) -> Option<&[u8]> {
    let head = bytes.get(..n)?;
    head.iter().all(u8::is_ascii_digit).then(|| &bytes[n..])
}

/// Whether `rest`, all that follows an absolute date (`timed`: with a time), is exactly a zone
/// this module reads: ` UTC` or ` GMT` in any case, or after a time `Z`, ` Z` or an offset.
fn names_zone(rest: &[u8], timed: bool) -> bool {
    if rest.eq_ignore_ascii_case(b" utc") || rest.eq_ignore_ascii_case(b" gmt") {
        return true;
    }
    if !timed {
        return false;
    }
    let rest = rest.strip_prefix(b" ").unwrap_or(rest);
    if rest.eq_ignore_ascii_case(b"z") {
        return true;
    }
    let Some(offset) = rest.strip_prefix(b"+").or_else(|| rest.strip_prefix(b"-")) else {
        return false;
    };
    let all_digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    match offset.len() {
        1 | 2 | 4 => all_digits(offset),
        5 => offset[2] == b':' && all_digits(&offset[..2]) && all_digits(&offset[3..]),
        _ => false,
    }
}

/// Whether a `createFromFormat` format holds an unescaped `!` or `|`: every field it does not
/// parse is reset to the epoch's instead of the current time's.
fn resets_fields(format: &str) -> bool {
    let mut bytes = format.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'\\' => {
                bytes.next();
            }
            b'!' | b'|' => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{GateArg, Shown, classify, date_gate, date_method_gate};

    const fn shown(zone: Option<bool>, clock: Option<bool>) -> Shown {
        Shown { zone, clock }
    }

    /// The constructors and the function spellings have a gate; the copying factories and the
    /// other names have none.
    #[test]
    fn the_gated_names_and_positions() {
        for name in ["date_create", "DATE_CREATE_IMMUTABLE"] {
            assert_eq!(date_gate(name).expect(name).positions(), [0, 1], "{name}");
        }
        for name in ["date_create_from_format", "date_create_immutable_from_format"] {
            assert_eq!(date_gate(name).expect(name).positions(), [0, 2], "{name}");
        }
        for class in ["DateTime", "\\DateTimeImmutable", "datetime"] {
            let gate = |method| date_method_gate(class, method).expect(class).positions();
            assert_eq!(gate("__CONSTRUCT"), [0, 1]);
            assert_eq!(gate("createFromFormat"), [0, 2]);
        }
        for (class, method) in [
            ("DateTime", "createFromImmutable"),
            ("DateTimeImmutable", "createFromMutable"),
            ("DateTime", "createFromInterface"),
            ("DateTime", "modify"),
            ("DateTimeZone", "__construct"),
            ("App\\DateTime", "__construct"),
        ] {
            assert_eq!(date_method_gate(class, method), None, "{class}::{method}");
        }
        for name in ["date", "strtotime", "date_create_from_mutable", "checkdate"] {
            assert_eq!(date_gate(name), None, "{name}");
        }
    }

    /// The literals the grammar reads, each with what it shows.
    #[test]
    fn the_literal_grammar() {
        let both = shown(Some(true), Some(true));
        for text in ["", "now", "today", "midnight", "noon", "tomorrow", "yesterday"] {
            assert_eq!(classify(text), both, "{text:?}");
        }
        let clock_only = shown(Some(false), Some(true));
        for text in ["now UTC", "today UTC", "tomorrow UTC"] {
            assert_eq!(classify(text), clock_only, "{text:?}");
        }
        let zone_only = shown(Some(true), Some(false));
        for text in [
            "2020-01-01", "2020-01-01 00:00", "2020-01-01T10:00:00", "2020-01-01 10:00:00.123456",
            "2017-02-30",
        ] {
            assert_eq!(classify(text), zone_only, "{text:?}");
        }
        let neither = shown(Some(false), Some(false));
        for text in [
            "@0", "@-5", "@1.5", "@1700000000", "2020-01-01 UTC", "2020-01-01 gmt",
            "2024-01-18 00:00 UTC", "2020-01-01T10:00:00Z", "2020-01-01 10:00 z",
            "2016-01-21T21:11:30.123456+00:00", "2013-03-29T05:13:35-0600",
            "2013-03-29T05:13:35-05", "2013-03-29T05:13:35+5", "2013-03-29 05:13:35 +1",
        ] {
            assert_eq!(classify(text), neither, "{text:?}");
        }
        // Each of these names a zone timelib reads, takes a field from `now` under a zone that is
        // not an identifier, or is a shape the grammar does not read: undecided.
        let nothing = shown(None, None);
        for text in [
            "now GMT", "now utc", "now Z", "now +01:00", "today GMT", " UTC", "2020-01-01 est",
            "2020-01-01 EST", "2020-01-01 cet", "2020-01-01 10:00 a", "2020-01-01 t", "2020-01-01T",
            "2020-01-01 Japan", "2020-01-01 Zulu", "2020-01-01 Europe/London", "2020-01-01+01",
            "2020-01-01 10:00 GMT+9", "01/02/2020", "+1 day", "monday", "10:00", "NOW", " now",
            "@", "@ 0", "@0.", "@.5", "@+5", "2020-01-01 10:00:00.1234567", "2020-01-01 +1 day",
            "2020-01-01 10:00 +01:0", "2020-01-01 10:00 +123",
        ] {
            assert_eq!(classify(text), nothing, "{text:?}");
        }
    }

    /// A zone object shown passed drops the zone read whatever the string is; an omitted or
    /// `null` one leaves it to the string; an unknown one drops it only where the string does.
    #[test]
    fn the_zone_object_decides_first() {
        let gate = date_gate("date_create").expect("gate");
        let s = |text| Some(GateArg::Str(text));
        let omitted = Some(GateArg::Omitted);
        assert_eq!(gate.reads_zone(&[omitted, omitted]), Some(true), "date_create()");
        assert_eq!(gate.reads_zone(&[s("2020-01-01"), omitted]), Some(true));
        assert_eq!(gate.reads_zone(&[s("2020-01-01"), Some(GateArg::Null)]), Some(true));
        assert_eq!(gate.reads_zone(&[s("2020-01-01"), Some(GateArg::NonNull)]), Some(false));
        assert_eq!(gate.reads_zone(&[None, Some(GateArg::NonNull)]), Some(false));
        assert_eq!(gate.reads_zone(&[s("2020-01-01"), None]), None);
        assert_eq!(gate.reads_zone(&[s("@0"), None]), Some(false));
        assert_eq!(gate.reads_zone(&[None, omitted]), None);
        assert_eq!(gate.reads_zone(&[Some(GateArg::Null), omitted]), None);
        let format = date_gate("date_create_from_format").expect("gate");
        assert_eq!(format.reads_zone(&[s("Y-m-d"), omitted]), None, "a format is not read");
        assert_eq!(format.reads_zone(&[s("Y-m-d"), Some(GateArg::NonNull)]), Some(false));
    }

    /// The clock: a literal that fills every field drops it, a format that resets the fields it
    /// does not parse drops it, and an escaped reset does not.
    #[test]
    fn the_clock_by_the_string_or_the_format() {
        let gate = date_method_gate("DateTimeImmutable", "__construct").expect("gate");
        let s = |text| Some(GateArg::Str(text));
        assert_eq!(gate.reads_clock(&[Some(GateArg::Omitted), None]), Some(true));
        assert_eq!(gate.reads_clock(&[s("now"), Some(GateArg::NonNull)]), Some(true));
        assert_eq!(gate.reads_clock(&[s("2020-01-01"), None]), Some(false));
        assert_eq!(gate.reads_clock(&[s("@0"), None]), Some(false));
        assert_eq!(gate.reads_clock(&[s("+1 day"), None]), None);
        assert_eq!(gate.reads_clock(&[None, None]), None);
        let format = date_method_gate("DateTime", "createFromFormat").expect("gate");
        for f in ["!Y-m-d", "Y-m-d|", "!d", "H:i|", "Y-m-d\\\\|"] {
            assert_eq!(format.reads_clock(&[s(f), None]), Some(false), "{f}");
        }
        for f in ["Y-m-d", "\\!Y-m-d", "\\|Y-m-d", "Y-m-d H:i:s", "U"] {
            assert_eq!(format.reads_clock(&[s(f), None]), None, "{f}");
        }
    }
}
