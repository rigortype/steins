//! The precision renderers (ADR-0101 §3.15, issue #1000, S6a).
//!
//! A float becomes text through one of two ini entries, and the catalog gives both the one
//! `precision` cell (`global.read.setting.precision`, registered by S3): `precision` for the
//! conversions that follow `(string) $f` (`EG(precision)`, `smart_str_append_double`), and
//! `serialize_precision` for the ones that must round-trip (`PG(serialize_precision)`). The read
//! is **value-conditional**: the function reads the entry only if the value it renders is a float
//! (or holds one where the function walks into an array or an object), so the row is the upper
//! bound and the call site decides it by the three-way rule of §3.2 over the same float evidence
//! a printf `%s` uses. A float proves the read, a value shown to hold none drops it, and any other
//! is the `value-dependent-read` gap and never a label.
//!
//! | name | ini | values rendered | walks into |
//! | --- | --- | --- | --- |
//! | `strval` | `precision` | `$value` (0) | nothing: an array is `"Array"` |
//! | `settype` | `precision` | `$var` (0), when `$type` (1) is `"string"` | nothing |
//! | `implode`, `join` | `precision` | `$separator` (0) with two arguments, and the elements of the array (1, or 0 alone) | the elements, one level: an element that is an array is `"Array"` |
//! | `print_r` | `precision` | `$value` (0) | arrays and objects, every depth |
//! | `var_export`, `json_encode`, `serialize` | `serialize_precision` | `$value` (0) | arrays and objects, every depth |
//! | `var_dump`, `debug_zval_dump` | `serialize_precision` | every argument | arrays and objects, every depth |
//!
//! `json_encode` has two more ways to differ. `JSON_NUMERIC_CHECK` turns a numeric string into a
//! number before it is written, so with that flag, or with flags the scan cannot evaluate, a
//! value of strings and integers is not shown to hold no float; and a `$depth` argument can end
//! the walk with an error before it reaches the float, so a proven read needs the depth omitted.
//! A non-finite float (`NAN`, `INF`) is written without either entry (`json_encode` refuses it),
//! which the evidence reader treats as undecided.
//!
//! The Q-numbers in the oracle (`setting_precision_oracle.rs`) are the witnesses: each row there
//! runs the call under `ini_set('precision', '3')` and `ini_set('serialize_precision', '3')` and
//! compares.

use super::GateArg;

/// How deep a renderer reads the value at one position, which decides what an array literal
/// there holds for the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// The value is converted to a string whole: an array is `"Array"` and holds nothing read.
    Value,
    /// The value is an array whose elements are each converted to a string whole (`implode`).
    Elements,
    /// The value is walked: every element of every array, and every property of an object.
    Nested,
}

/// What a call shows of one rendered value, read at the depth the gate names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendered {
    /// The call renders a float there.
    Float,
    /// The call renders no float there.
    NoFloat,
}

/// Which arguments of a precision renderer decide whether it reads the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrecisionGate {
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `strval($value)`.
    Strval,
    /// `settype($var, $type)`: only the type `"string"` renders.
    SetType,
    /// `implode($array)` and `implode($separator, $array)`.
    Implode,
    /// One value at position 0, walked.
    Walk,
    /// Every positional argument, walked (`var_dump`, `debug_zval_dump`).
    WalkEach,
    /// `json_encode($value, $flags, $depth)`.
    Json,
}

/// `JSON_NUMERIC_CHECK`.
const JSON_NUMERIC_CHECK: i64 = 32;

/// The one value at position 0, when the call supplies any argument.
fn first_value(arity: usize, depth: Depth) -> Vec<(usize, Depth)> {
    if arity > 0 { vec![(0, depth)] } else { Vec::new() }
}

/// The precision gate of the builtin `name` (case-insensitive), or `None` for a name that renders
/// no float through either entry (`number_format` and `round` read neither; the operators, which
/// are no calls, stay unlabelled under D4).
#[must_use]
pub fn precision_gate(name: &str) -> Option<PrecisionGate> {
    let kind = match name.to_ascii_lowercase().as_str() {
        "strval" => Kind::Strval,
        "settype" => Kind::SetType,
        "implode" | "join" => Kind::Implode,
        "print_r" | "var_export" | "serialize" => Kind::Walk,
        "var_dump" | "debug_zval_dump" => Kind::WalkEach,
        "json_encode" => Kind::Json,
        _ => return None,
    };
    Some(PrecisionGate { kind })
}

impl PrecisionGate {
    /// The positions the call renders and the depth it reads each at, for a call that passes
    /// `arity` positional arguments, in position order. A position the call does not supply
    /// renders nothing and is absent.
    #[must_use]
    pub fn values(self, arity: usize) -> Vec<(usize, Depth)> {
        match self.kind {
            Kind::Strval | Kind::SetType => first_value(arity, Depth::Value),
            Kind::Implode => match arity {
                0 => Vec::new(),
                1 => vec![(0, Depth::Elements)],
                _ => vec![(0, Depth::Value), (1, Depth::Elements)],
            },
            Kind::Walk | Kind::Json => first_value(arity, Depth::Nested),
            Kind::WalkEach => (0..arity).map(|position| (position, Depth::Nested)).collect(),
        }
    }

    /// The positional indexes of the arguments that are no rendered value but decide the read, in
    /// the order [`Self::reads`] takes them: `settype`'s type, `json_encode`'s flags and depth.
    #[must_use]
    pub const fn extras(self) -> &'static [usize] {
        match self.kind {
            Kind::SetType => &[1],
            Kind::Json => &[1, 2],
            _ => &[],
        }
    }

    /// Whether the call reads the ini entry, given what it shows of each rendered value
    /// ([`Self::values`], `None` for one the scan shows nothing of) and of each extra argument
    /// ([`Self::extras`]): `Some(true)` when a rendered value is a float, `Some(false)` when every
    /// one is shown to hold none, and `None` when they do not decide it, which the caller treats
    /// as the read depending on a value the site cannot see.
    #[must_use]
    pub fn reads(
        self,
        values: &[Option<Rendered>],
        extras: &[Option<GateArg<'_>>],
    ) -> Option<bool> {
        let rendered = if values.contains(&Some(Rendered::Float)) {
            Some(true)
        } else if values.iter().all(|v| *v == Some(Rendered::NoFloat)) {
            Some(false)
        } else {
            None
        };
        let extra = |index: usize| extras.get(index).copied().flatten();
        match self.kind {
            Kind::SetType => match extra(0)? {
                GateArg::Str(ty) if ty.eq_ignore_ascii_case("string") => rendered,
                GateArg::Str(_) => Some(false),
                _ => None,
            },
            Kind::Json => json_reads(rendered, extra(0), extra(1)),
            _ => rendered,
        }
    }
}

/// `json_encode`'s verdict: a float proves the read unless a `$depth` can cut the walk short, and
/// a value free of floats drops it unless `JSON_NUMERIC_CHECK` can make one of a string.
fn json_reads(
    rendered: Option<bool>,
    flags: Option<GateArg<'_>>,
    depth: Option<GateArg<'_>>,
) -> Option<bool> {
    match rendered? {
        true => matches!(depth, Some(GateArg::Omitted)).then_some(true),
        false => match flags {
            Some(GateArg::Omitted) => Some(false),
            Some(GateArg::Int(flags)) if flags & JSON_NUMERIC_CHECK == 0 => Some(false),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Depth, GateArg, Rendered, precision_gate};

    const FLOAT: Option<Rendered> = Some(Rendered::Float);
    const NO_FLOAT: Option<Rendered> = Some(Rendered::NoFloat);

    fn reads(
        name: &str,
        values: &[Option<Rendered>],
        extras: &[Option<GateArg<'_>>],
    ) -> Option<bool> {
        precision_gate(name).expect(name).reads(values, extras)
    }

    /// The gated names, and the names that read neither entry.
    #[test]
    fn the_gated_names() {
        for name in [
            "strval", "STRVAL", "settype", "implode", "join", "print_r", "var_export",
            "json_encode", "serialize", "var_dump", "debug_zval_dump",
        ] {
            assert!(precision_gate(name).is_some(), "{name}");
        }
        for name in ["number_format", "round", "floatval", "intval", "sprintf", "ini_set", "echo"] {
            assert!(precision_gate(name).is_none(), "{name}");
        }
    }

    /// Which positions each renderer renders, and at which depth.
    #[test]
    fn the_rendered_positions() {
        let values = |name: &str, arity| precision_gate(name).expect(name).values(arity);
        assert_eq!(values("strval", 1), [(0, Depth::Value)]);
        assert_eq!(values("strval", 0), []);
        assert_eq!(values("implode", 1), [(0, Depth::Elements)]);
        assert_eq!(values("join", 2), [(0, Depth::Value), (1, Depth::Elements)]);
        assert_eq!(values("print_r", 2), [(0, Depth::Nested)]);
        assert_eq!(values("json_encode", 3), [(0, Depth::Nested)]);
        let each = [(0, Depth::Nested), (1, Depth::Nested), (2, Depth::Nested)];
        assert_eq!(values("var_dump", 3), each);
        assert_eq!(values("var_dump", 0), []);
        assert_eq!(precision_gate("settype").expect("row").extras(), [1]);
        assert_eq!(precision_gate("json_encode").expect("row").extras(), [1, 2]);
        assert_eq!(precision_gate("strval").expect("row").extras(), [] as [usize; 0]);
    }

    /// A float proves the read wherever it sits, a value free of floats drops it, and a value the
    /// scan shows nothing of leaves it undecided unless another value proves it.
    #[test]
    fn the_three_way_rule() {
        for name in ["strval", "implode", "print_r", "var_export", "serialize", "var_dump"] {
            assert_eq!(reads(name, &[FLOAT], &[]), Some(true), "{name}");
            assert_eq!(reads(name, &[NO_FLOAT], &[]), Some(false), "{name}");
            assert_eq!(reads(name, &[None], &[]), None, "{name}");
            assert_eq!(reads(name, &[NO_FLOAT, FLOAT], &[]), Some(true), "{name}");
            assert_eq!(reads(name, &[None, FLOAT], &[]), Some(true), "{name}");
            assert_eq!(reads(name, &[NO_FLOAT, None], &[]), None, "{name}");
        }
        assert_eq!(reads("var_dump", &[], &[]), Some(false), "no argument renders nothing");
    }

    /// `settype` renders only to a string, and the type is read case-insensitively.
    #[test]
    fn settype_reads_for_the_string_type() {
        let ty = |t| [Some(GateArg::Str(t))];
        assert_eq!(reads("settype", &[FLOAT], &ty("string")), Some(true));
        assert_eq!(reads("settype", &[FLOAT], &ty("STRING")), Some(true));
        assert_eq!(reads("settype", &[NO_FLOAT], &ty("string")), Some(false));
        assert_eq!(reads("settype", &[None], &ty("string")), None);
        for other in ["int", "integer", "float", "array", "bool", "null"] {
            assert_eq!(reads("settype", &[FLOAT], &ty(other)), Some(false), "{other}");
            assert_eq!(reads("settype", &[None], &ty(other)), Some(false), "{other}");
        }
        assert_eq!(reads("settype", &[FLOAT], &[None]), None);
        assert_eq!(reads("settype", &[FLOAT], &[Some(GateArg::NotText)]), None);
    }

    /// `json_encode`: a numeric-check flag or an unreadable one makes a float-free value
    /// undecided, and a depth makes a float undecided.
    #[test]
    fn json_encode_flags_and_depth() {
        let omitted = Some(GateArg::Omitted);
        let int = |v| Some(GateArg::Int(v));
        assert_eq!(reads("json_encode", &[FLOAT], &[omitted, omitted]), Some(true));
        assert_eq!(reads("json_encode", &[FLOAT], &[int(32), omitted]), Some(true));
        assert_eq!(reads("json_encode", &[FLOAT], &[None, omitted]), Some(true));
        assert_eq!(reads("json_encode", &[FLOAT], &[omitted, int(1)]), None);
        assert_eq!(reads("json_encode", &[FLOAT], &[omitted, None]), None);
        assert_eq!(reads("json_encode", &[NO_FLOAT], &[omitted, omitted]), Some(false));
        assert_eq!(reads("json_encode", &[NO_FLOAT], &[int(128 | 4194304), omitted]), Some(false));
        assert_eq!(reads("json_encode", &[NO_FLOAT], &[int(32), omitted]), None);
        assert_eq!(reads("json_encode", &[NO_FLOAT], &[int(32 | 128), omitted]), None);
        assert_eq!(reads("json_encode", &[NO_FLOAT], &[None, omitted]), None);
        assert_eq!(reads("json_encode", &[NO_FLOAT], &[omitted, int(1)]), Some(false));
        assert_eq!(reads("json_encode", &[None], &[omitted, omitted]), None);
    }
}
