//! The clock half of the time family (ADR-0101 §3.14, issue #1000, S6b-1).
//!
//! The family's row used to be `nondet.time`, argument-blind: `date('Y', 0)` carried it though
//! nothing about that call depends on when it runs. The clock is read only where a timestamp is
//! missing, and the timezone cell is read apart from it, so the two halves of the row are decided
//! separately. The timezone half is **unconditional** in php-src (`php-8.5.11`,
//! `ext/date/php_date.c`): `date`, `idate`, `mktime`, `strtotime`, `getdate`, `localtime` and
//! `strftime` call `get_timezone_info()` on every call, and their rows name it. What this gate
//! decides is the clock:
//!
//! | name | deciding argument | reads the clock when |
//! | --- | --- | --- |
//! | `date`, `idate`, `gmdate` | `$timestamp` (1) | it is omitted or `null` (`if (ts_is_null) ts = php_time()`) |
//! | `strftime`, `gmstrftime` | `$timestamp` (1) | the same |
//! | `strtotime` | `$baseTimestamp` (1) | the same: `timelib_unixtime2local(now, preset_ts_is_null ? php_time() : preset_ts)` |
//! | `getdate`, `localtime` | `$timestamp` (0) | the same |
//! | `gmmktime` | `$hour` .. `$year` (0 to 5) | any of the six is omitted or `null`: `php_mktime` seeds `now` from `php_time()` and overwrites only the fields it is given |
//!
//! `mktime` has **no gate** and keeps the clock at every arity. It seeds `now` from the clock as
//! `gmmktime` does, and for a local time the seed's DST flag survives into `timelib_update_ts`
//! (`do_adjust_timezone` consults `tz->dst` when it resolves the repeated hour of a fall-back
//! transition), so six literal fields can still depend on when the call runs. `gmmktime` seeds
//! from UTC, where the flag is 0 and no zone lookup follows.
//!
//! A timestamp the call shows **not to be `null`** is supplied: an integer literal or constant,
//! and any argument whose type at the call excludes `null` (an `int` parameter, `time()`,
//! arithmetic, a property declared `int`; [`GateArg::NonNull`]). The value is the caller's, and
//! whatever clock produced it is read where it was produced (`time()` carries `nondet.time`
//! itself). An omitted one and a literal `null` read the clock; any other argument (a value
//! whose type may be `null` or is unknown, a named or spread argument list) is undecided, and
//! the row keeps the label: the label is an upper bound there, as every time-family row was.

use super::GateArg;

/// Which arguments of a time-family call decide whether it reads the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockGate {
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// One optional nullable `?int` timestamp at `position`.
    TimestampSupplied { position: u8 },
    /// `gmmktime`: the clock fills every field the call leaves out.
    AllFieldsSupplied,
}

/// The clock gate of the builtin `name` (case-insensitive), or `None` for a name that reads the
/// clock unconditionally (`time`, `microtime`, `mktime`) or never.
#[must_use]
pub fn clock_gate(name: &str) -> Option<ClockGate> {
    let position = match name.to_ascii_lowercase().as_str() {
        "date" | "idate" | "gmdate" | "strftime" | "gmstrftime" | "strtotime" => 1,
        "getdate" | "localtime" => 0,
        "gmmktime" => return Some(ClockGate { kind: Kind::AllFieldsSupplied }),
        _ => return None,
    };
    Some(ClockGate { kind: Kind::TimestampSupplied { position } })
}

impl ClockGate {
    /// The positional indexes of the deciding arguments, in the order [`Self::reads`] takes them.
    #[must_use]
    pub const fn positions(self) -> &'static [usize] {
        match self.kind {
            Kind::TimestampSupplied { position: 0 } => &[0],
            Kind::TimestampSupplied { .. } => &[1],
            Kind::AllFieldsSupplied => &[0, 1, 2, 3, 4, 5],
        }
    }

    /// Whether the call reads the clock, at a call whose deciding arguments show `args` (as
    /// [`SettingReadGate::reads`](super::SettingReadGate::reads) takes them): `Some(true)` for an
    /// omitted or `null` timestamp, `Some(false)` where every deciding argument is an integer
    /// or shown non-`null`, and `None` when they do not decide it, which the caller treats as the label kept.
    #[must_use]
    pub fn reads(self, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        let unsupplied =
            |arg: &Option<GateArg<'_>>| matches!(arg, Some(GateArg::Omitted | GateArg::Null));
        if args.iter().any(unsupplied) {
            return Some(true);
        }
        let supplied = |arg: &Option<GateArg<'_>>| matches!(arg, Some(GateArg::Int(_) | GateArg::NonNull));
        args.iter().all(supplied).then_some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{GateArg, clock_gate};

    fn reads(name: &str, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        clock_gate(name).expect(name).reads(args)
    }

    /// The gated names and the positions their deciding arguments take; the names that read the
    /// clock on every call, or never, have no gate.
    #[test]
    fn the_gated_names_and_positions() {
        for name in ["date", "idate", "gmdate", "strftime", "GMSTRFTIME", "strtotime"] {
            assert_eq!(clock_gate(name).expect(name).positions(), [1], "{name}");
        }
        for name in ["getdate", "localtime"] {
            assert_eq!(clock_gate(name).expect(name).positions(), [0], "{name}");
        }
        assert_eq!(clock_gate("gmmktime").expect("row").positions(), [0, 1, 2, 3, 4, 5]);
        for name in ["time", "microtime", "hrtime", "mktime", "checkdate", "date_create", "gmidate"]
        {
            assert_eq!(clock_gate(name), None, "{name}");
        }
    }

    /// An omitted or `null` timestamp reads the clock, an integer literal does not, and any other
    /// argument leaves it undecided.
    #[test]
    fn a_timestamp_decides_the_clock() {
        for name in ["date", "idate", "gmdate", "strftime", "gmstrftime", "strtotime"] {
            assert_eq!(reads(name, &[Some(GateArg::Omitted)]), Some(true), "{name}");
            assert_eq!(reads(name, &[Some(GateArg::Null)]), Some(true), "{name}");
            assert_eq!(reads(name, &[Some(GateArg::Int(0))]), Some(false), "{name}");
            assert_eq!(reads(name, &[Some(GateArg::Int(-1))]), Some(false), "{name}");
            assert_eq!(reads(name, &[Some(GateArg::NonNull)]), Some(false), "{name}");
            for other in [GateArg::Str("0"), GateArg::Bool(true), GateArg::NotText] {
                assert_eq!(reads(name, &[Some(other)]), None, "{name}");
            }
            assert_eq!(reads(name, &[None]), None, "{name}");
        }
    }

    /// `gmmktime` is clock-free only when all six fields are shown supplied; one omitted or
    /// `null` field is the proven read whatever the others are.
    #[test]
    fn gmmktime_needs_every_field() {
        let int = Some(GateArg::Int(1));
        let omitted = Some(GateArg::Omitted);
        assert_eq!(reads("gmmktime", &[int; 6]), Some(false));
        let non_null = Some(GateArg::NonNull);
        assert_eq!(reads("gmmktime", &[non_null; 6]), Some(false));
        assert_eq!(reads("gmmktime", &[int, non_null, int, non_null, int, int]), Some(false));
        assert_eq!(reads("gmmktime", &[int, int, int, int, int, omitted]), Some(true));
        assert_eq!(reads("gmmktime", &[int, Some(GateArg::Null), int, int, int, int]), Some(true));
        assert_eq!(reads("gmmktime", &[int, None, int, int, int, int]), None);
        assert_eq!(reads("gmmktime", &[int, None, int, int, int, omitted]), Some(true));
    }
}
