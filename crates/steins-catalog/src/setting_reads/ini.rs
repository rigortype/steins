//! The residue cell's gated readers (ADR-0101 §3.16, issue #1000, S6e): the bcmath functions and
//! the two that return the old value of an ini entry they may rewrite.
//!
//! The residue cell is `bcmath.scale`, `include_path` and `error_reporting` (and
//! `max_execution_time`, which `set_time_limit` rewrites), one ini entry at a time. php-src
//! (`php-8.5.11`) decides the shapes:
//!
//! | name | deciding argument | reads the cell when | writes it when |
//! | --- | --- | --- | --- |
//! | `bcadd bccomp bcdiv bcdivmod bcmod bcmul bcpow bcsub` | `$scale` (2) | it is omitted or `null` (`BCG(bc_precision)` is the default) | never |
//! | `bcsqrt` | `$scale` (1) | the same | never |
//! | `bcpowmod` | `$scale` (3) | the same | never |
//! | `bcscale` | `$scale` (0) | always: it returns the old value | it is given a non-`null` value |
//! | `error_reporting` | `$error_level` (0) | always: it returns the old value | it is given a non-`null` value |
//!
//! Witnessed on PHP 8.5.11 (`s6e-bc.php`, `s6e-err.php`): under `bcscale(0)` and `bcscale(3)`
//! `bcadd('1', '2')` is `'3'` and `'3.000'`, and `bcadd('1', '2', 2)`, `bcadd('1', '2', '2')`,
//! `bcadd('1', '2', true)` and a `$scale` variable of `2` are the same under both; `bcscale(null)`
//! returns the cell and leaves it as it was; `error_reporting(null)` does the same; and `bcceil`,
//! `bcfloor` and `bcround` read nothing. The bcmath rows follow `PINNED_PHP` (8.5), and their
//! `$scale` positions are the generated parameter names' (`param_facts_generated.rs`), which the
//! tests below pin.
//!
//! A `$scale` the scan cannot show is **undecided**, and the row keeps the read, as the clock
//! gate keeps `nondet.time` ([`crate::setting_reads::clock`]): the label is an upper bound there
//! and no `value-dependent-read` gap is raised. The `$scale` is shown **not to be `null`** through
//! the same [`GateArg::NonNull`] evidence the time family uses (ADR-0101 §3.14).

use super::GateArg;

/// How a residue reader reaches its cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gate {
    /// A bcmath function that reads the scale cell only when `$scale` is omitted or `null`. The
    /// positions are the generated table's, one entry.
    Scale(&'static [usize]),
    /// `bcscale` and `error_reporting`: the read happens on every call (the old value comes
    /// back), and the write happens when the first argument is given a non-`null` value.
    OldValue,
}

/// The gate of the builtin `name` (lowercase), or `None` for a name outside the residue.
pub(super) fn gate_of(name: &str) -> Option<Gate> {
    match name {
        "bcadd" | "bccomp" | "bcdiv" | "bcdivmod" | "bcmod" | "bcmul" | "bcpow" | "bcsub" => {
            Some(Gate::Scale(&[2]))
        }
        "bcsqrt" => Some(Gate::Scale(&[1])),
        "bcpowmod" => Some(Gate::Scale(&[3])),
        "bcscale" | "error_reporting" => Some(Gate::OldValue),
        _ => None,
    }
}

impl Gate {
    /// The positional indexes of the deciding arguments.
    pub(super) const fn positions(self) -> &'static [usize] {
        match self {
            Self::Scale(at) => at,
            Self::OldValue => &[0],
        }
    }

    /// Whether the read happens at a call whose deciding argument shows `args`, or `None` when
    /// the argument is not shown. [`Gate::OldValue`] reads whatever it is given.
    pub(super) fn reads(self, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        match self {
            Self::OldValue => Some(true),
            Self::Scale(_) => match args.first().copied().flatten()? {
                GateArg::Omitted | GateArg::Null => Some(true),
                _ => Some(false),
            },
        }
    }

    /// Whether the call writes the cell, at a call whose deciding argument shows `args`: an
    /// omitted or `null` argument writes nothing, anything else (and an argument not shown) may.
    pub(super) fn writes(self, args: &[Option<GateArg<'_>>]) -> bool {
        match self {
            Self::OldValue => !matches!(args.first(), Some(Some(GateArg::Omitted | GateArg::Null))),
            Self::Scale(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Gate, gate_of};
    use crate::param_facts;
    use crate::setting_reads::GateArg;

    /// The scale position of each bcmath name is the position of `scale` in the generated table
    /// (`param_facts`), and the names with no scale parameter have no gate.
    #[test]
    fn the_scale_positions_are_the_generated_parameter_names() {
        for name in [
            "bcadd", "bccomp", "bcdiv", "bcdivmod", "bcmod", "bcmul", "bcpow", "bcpowmod", "bcsqrt",
            "bcsub",
        ] {
            let facts = param_facts(name).unwrap_or_else(|| panic!("{name} has no param facts"));
            let at = facts.param_names.iter().position(|n| *n == "scale");
            let Some(Gate::Scale(positions)) = gate_of(name) else { panic!("{name} is ungated") };
            assert_eq!(positions, [at.expect("no scale parameter")], "{name}");
        }
        for name in ["bcceil", "bcfloor", "bcround", "bcscale", "error_reporting", "include_path"] {
            assert!(!matches!(gate_of(name), Some(Gate::Scale(_))), "{name}");
        }
        assert_eq!(gate_of("bcscale"), Some(Gate::OldValue));
        assert_eq!(gate_of("error_reporting"), Some(Gate::OldValue));
        assert_eq!(gate_of("bcceil"), None, "bcceil reads nothing, witnessed");
    }

    /// An omitted or `null` scale reads the cell; a literal, a value shown not to be `null` and
    /// a value the scan cannot see decide it the same way the clock gate does.
    #[test]
    fn a_scale_reads_only_when_omitted_or_null() {
        let gate = gate_of("bcadd").expect("bcadd");
        assert_eq!(gate.reads(&[Some(GateArg::Omitted)]), Some(true));
        assert_eq!(gate.reads(&[Some(GateArg::Null)]), Some(true));
        for shown in [
            GateArg::Int(0),
            GateArg::Int(2),
            GateArg::Str("2"),
            GateArg::Bool(true),
            GateArg::NotText,
            GateArg::NonNull,
        ] {
            assert_eq!(gate.reads(&[Some(shown)]), Some(false), "{shown:?}");
        }
        assert_eq!(gate.reads(&[None]), None, "a variable the scan cannot see");
        assert_eq!(gate.reads(&[]), None, "a named or spread list");
    }

    /// `bcscale` and `error_reporting` read on every call, and write only when given a value.
    #[test]
    fn the_old_value_readers_read_always_and_write_when_given_a_value() {
        for name in ["bcscale", "error_reporting"] {
            let gate = gate_of(name).expect(name);
            for args in [
                vec![Some(GateArg::Omitted)],
                vec![Some(GateArg::Null)],
                vec![Some(GateArg::Int(3))],
                vec![None],
                vec![],
            ] {
                assert_eq!(gate.reads(&args), Some(true), "{name} {args:?}");
            }
            assert!(!gate.writes(&[Some(GateArg::Omitted)]), "{name}: no argument");
            assert!(!gate.writes(&[Some(GateArg::Null)]), "{name}: a null writes nothing");
            assert!(gate.writes(&[Some(GateArg::Int(3))]), "{name}");
            assert!(gate.writes(&[Some(GateArg::NonNull)]), "{name}");
            assert!(gate.writes(&[None]), "{name}: an argument not shown may write");
            assert!(gate.writes(&[]), "{name}: a named or spread list may write");
        }
    }
}
