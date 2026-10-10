//! The setting readers whose read the call decides, at a call site (ADR-0101 §3.9, S4; renamed
//! from `locale` in S6-core, when a gate came to name its cell).
//!
//! `sort`, `substr_compare`, `pathinfo`, the `ctype_*` predicates, `strnatcmp`, `escapeshellarg`,
//! `strip_tags`, `parse_url` and `strftime` are catalogued with the locale read
//! ([`steins_catalog::effect_labels`]), but read it only where the call reaches the routine that
//! consults it ([`steins_catalog::setting_read_gate`]): under a mode argument (a sort's `$flags`,
//! `substr_compare`'s case switch) or over content the routine classifies (a non-empty string, a
//! `<`, a conversion that names the locale). A label is proven at a call only where the read
//! happens on every run of the call as written, so the row is the upper bound this module
//! narrows, in the three ways the criterion of ADR-0101 §3.2 allows:
//!
//! * the deciding argument is **omitted**: the parameter's default decides;
//! * it is a **literal** the scan evaluates (a string, an integer, an engine constant, a `|` of
//!   such terms, a boolean, or for a `ctype_*` predicate a `null`, float or array literal, a `new`
//!   expression or a by-value parameter whose declared type keeps it from a string and an
//!   integer): the read is proven or dropped by what the catalog states for that value;
//! * anything else, a variable, an expression the scan cannot evaluate, a named or spread
//!   argument list, or the builtin handed over as a callback, is
//!   [`GapKind::ValueDependentRead`] and no label.
//!
//! A bare constant is read as PHP resolves it ([`global_const_fact`]): a namespaced twin the
//! project declares shadows the global one, and a twin the scan cannot read is the gap.

use steins_catalog::{
    DateGate, GateArg, IniAccess, PrecisionGate, Rendered, SettingCell, SettingReadGate,
    clock_gate, ini_call, precision_gate, setting_read_gate,
};
use steins_domain::{Fact, Val};
use steins_syntax::{ArgLiteral, CallTarget, ConstArgs, ConstInt, NameRef, NotText, RefKind};

use super::GapKind;
use super::reach::{FloatClass, Frame};
use crate::cx::Cx;
use crate::global_consts::global_const_fact;

/// Narrow the row `labels` of a call to the builtin `builtin` (spelled `name`), when it is a
/// gated reader, by what the call shows of its deciding arguments, and say whether the read
/// depends on a value the call site cannot see. `positional` is the call's argument count,
/// `None` for a named or spread list.
pub(super) fn narrow_labels(
    (cx, frame): (&Cx, &Frame),
    (name, builtin): (&NameRef, &str),
    (positional, consts): (Option<usize>, &ConstArgs),
    labels: &mut Vec<&'static str>,
) -> Option<GapKind> {
    let gate = setting_read_gate(builtin)?;
    let args = gate_args(gate, (cx, frame), name, (positional, consts));
    // An accessor shown to have nothing to set from writes nothing (ADR-0101 §3.13).
    if args.as_deref().is_some_and(|args| !gate.writes(args)) {
        let write = gate.cell().write_label();
        labels.retain(|label| *label != write);
    }
    let verdict = if gate.always_reads() { Some(true) } else { args.and_then(|a| gate.reads(&a)) };
    match verdict {
        Some(true) => None,
        Some(false) => {
            let read = gate.cell().read_label();
            labels.retain(|label| *label != read);
            None
        }
        // A residue read the call leaves open keeps its label, as the clock's does (ADR-0101
        // §3.14, §3.16): the label is the upper bound, and no gap is raised.
        None if gate.keeps_undecided() => None,
        None => unreadable_mode(builtin, labels),
    }
}

/// The `precision` read of a float renderer (ADR-0101 §3.15, S6a): `strval`, `settype`, `implode`,
/// `print_r`, `var_export`, `json_encode`, `serialize`, `var_dump` and `debug_zval_dump` render a
/// float through `precision` or `serialize_precision` (one cell) and read nothing of a value that
/// holds none, so the catalog's row is the upper bound and the call decides it by the three-way
/// rule of §3.2: a value shown to be a float, or to hold one at the depth the function walks, is
/// the proven read; every value shown to hold none drops it; any other, a named or spread argument
/// list included, is [`GapKind::ValueDependentRead`] and no label. A name with no
/// [`precision_gate`] decides nothing.
pub(super) fn narrow_precision(
    (cx, frame): (&Cx, &Frame),
    (name, builtin): (&NameRef, &str),
    (positional, consts): (Option<usize>, &ConstArgs),
    labels: &mut Vec<&'static str>,
) -> Option<GapKind> {
    let gate = precision_gate(builtin)?;
    let verdict =
        positional.and_then(|arity| precision_read(gate, (cx, frame), name, (arity, consts)));
    if verdict == Some(true) {
        return None;
    }
    let read = SettingCell::Precision.read_label();
    labels.retain(|label| *label != read);
    verdict.is_none().then_some(GapKind::ValueDependentRead)
}

/// [`PrecisionGate::reads`] over what the call shows of its rendered values and its extra
/// arguments.
fn precision_read(
    gate: PrecisionGate,
    (cx, frame): (&Cx, &Frame),
    name: &NameRef,
    (arity, consts): (usize, &ConstArgs),
) -> Option<bool> {
    let values: Vec<Option<Rendered>> = gate
        .values(arity)
        .into_iter()
        .map(|(position, depth)| {
            let at = u8::try_from(position).ok()?;
            let (_, evidence) = consts.rendered.iter().find(|(p, _)| *p == at)?;
            match frame.float_class_at(cx, evidence, depth) {
                FloatClass::Yes => Some(Rendered::Float),
                FloatClass::No => Some(Rendered::NoFloat),
                FloatClass::Unknown => None,
            }
        })
        .collect();
    let extras: Vec<Option<GateArg<'_>>> = gate
        .extras()
        .iter()
        .map(|&position| {
            if position >= arity {
                Some(GateArg::Omitted)
            } else {
                argument((cx, frame), name, consts, position)
            }
        })
        .collect();
    gate.reads(&values, &extras)
}

/// The clock half of a time-family call (ADR-0101 §3.14): `nondet.time` is dropped where the
/// call shows the timestamp it is handed (an integer literal), and kept where the timestamp is
/// omitted, `null`, or anything the scan cannot read. It is an upper bound the call site only
/// ever narrows, so an unreadable timestamp is no gap: the label stays, as it did before the
/// timezone cell. A name with no [`clock_gate`] decides nothing.
pub(super) fn narrow_clock(
    (cx, frame): (&Cx, &Frame),
    (name, builtin): (&NameRef, &str),
    (positional, consts): (Option<usize>, &ConstArgs),
    labels: &mut Vec<&'static str>,
) {
    let Some(gate) = clock_gate(builtin) else { return };
    let Some(arity) = positional else { return };
    let args: Vec<Option<GateArg<'_>>> = gate
        .positions()
        .iter()
        .map(|&position| {
            if position >= arity {
                Some(GateArg::Omitted)
            } else {
                timestamp((cx, frame), name, consts, position)
            }
        })
        .collect();
    if gate.reads(&args) == Some(false) {
        labels.retain(|label| *label != CLOCK_LABEL);
    }
}

/// What the call shows of the timestamp (or field) at `position`: an integer it evaluates, a
/// `null`, or a value shown not to be `null` (a non-`null` literal or form, a parameter, property
/// or call whose declared type excludes it). Anything else is `None`, the label kept.
fn timestamp<'c>(
    (cx, frame): (&Cx, &Frame),
    name: &NameRef,
    consts: &'c ConstArgs,
    position: usize,
) -> Option<GateArg<'c>> {
    let at = u8::try_from(position).ok()?;
    match argument((cx, frame), name, consts, position) {
        Some(GateArg::Int(v)) => return Some(GateArg::Int(v)),
        Some(GateArg::Null) => return Some(GateArg::Null),
        Some(_) => return Some(GateArg::NonNull),
        None => {}
    }
    let (_, evidence) = consts.timestamps.iter().find(|(p, _)| *p == at)?;
    frame.non_null(cx, evidence).then_some(GateArg::NonNull)
}

/// The time family's clock label.
const CLOCK_LABEL: &str = "nondet.time";

/// The two reads of a `DateTime` constructor-side call (ADR-0101 §3.17): `new DateTime(...)`,
/// `new DateTimeImmutable(...)`, `createFromFormat`, `date_create*`. The row carries the default
/// zone and the clock as the upper bound; each is dropped where the call shows it is not read
/// ([`DateGate::reads_zone`], [`DateGate::reads_clock`]): a zone object shown passed, a literal
/// string that names its zone and every field, a format that resets the fields it does not
/// parse. Anything the call does not show keeps the label and raises no gap, as the clock does
/// (§3.14). `positional` is the call's argument count, `None` for a named or spread list, which
/// keeps both.
pub(super) fn narrow_date(
    (cx, frame): (&Cx, &Frame),
    gate: DateGate,
    (positional, consts): (Option<usize>, &ConstArgs),
    labels: &mut Vec<&'static str>,
) {
    let Some(arity) = positional else { return };
    let args: Vec<Option<GateArg<'_>>> = gate
        .positions()
        .iter()
        .map(|&position| {
            if position >= arity {
                Some(GateArg::Omitted)
            } else {
                date_argument((cx, frame), consts, position)
            }
        })
        .collect();
    if gate.reads_zone(&args) == Some(false) {
        let read = SettingCell::Timezone.read_label();
        labels.retain(|label| *label != read);
    }
    if gate.reads_clock(&args) == Some(false) {
        labels.retain(|label| *label != CLOCK_LABEL);
    }
}

/// What a constructor-side call shows of its argument at `position`: a literal (a string, an
/// integer, `null`, or another literal), or a value shown not to be `null` (a `new` expression, a
/// parameter or property declared so).
fn date_argument<'c>(
    (cx, frame): (&Cx, &Frame),
    consts: &'c ConstArgs,
    position: usize,
) -> Option<GateArg<'c>> {
    let at = u8::try_from(position).ok()?;
    if let Some((_, literal)) = consts.literals.iter().find(|(p, _)| *p == at) {
        return Some(match literal {
            ArgLiteral::Str(text) => GateArg::Str(text),
            ArgLiteral::Int(v) => GateArg::Int(*v),
            ArgLiteral::Null => GateArg::Null,
            ArgLiteral::Other => GateArg::NotText,
        });
    }
    let (_, evidence) = consts.timestamps.iter().find(|(p, _)| *p == at)?;
    frame.non_null(cx, evidence).then_some(GateArg::NonNull)
}

/// The row of a gated reader handed over as a callback, which its invoker calls with arguments of
/// its choosing, or called with arguments the scan cannot read: the read depends on them, so it
/// is no label and the call is [`GapKind::ValueDependentRead`]. `None` for a name with no gate.
pub(super) fn unreadable_mode(name: &str, labels: &mut Vec<&'static str>) -> Option<GapKind> {
    let read = match setting_read_gate(name) {
        Some(gate) => gate.cell().read_label(),
        // A float renderer handed over as a callback renders the values its invoker chooses.
        None => precision_gate(name).map(|_| SettingCell::Precision.read_label())?,
    };
    labels.retain(|label| *label != read);
    Some(GapKind::ValueDependentRead)
}

/// The `precision` read of an `ini_set` or `ini_alter` call that [`ini_call`] maps to a cell
/// (ADR-0101 §3.11): the new value is converted to a string before the entry is touched
/// (`zval_get_tmp_string`), and a float is rendered through `precision`
/// (`ini_set('include_path', 1234.5678)` stores `1.23E+3` at `precision=3`). The cell's own
/// read and write are the catalog's narrowed row; this is the value argument's, decided by the
/// three-way rule of §3.2 over the same float evidence as a `%s`: a value shown a float is the
/// proven `global.read.setting.precision`, a value shown no float reads nothing, and any other
/// is the [`GapKind::ValueDependentRead`] gap and no label. An `ini_set` of the precision cell
/// itself already carries that read, as the old value it returns. An unmapped name keeps the
/// coarse row and decides nothing here.
pub(super) fn ini_value_read(
    (cx, frame): (&Cx, &Frame),
    builtin: &str,
    (positional, consts): (Option<usize>, &ConstArgs),
    labels: &mut Vec<&'static str>,
) -> Option<GapKind> {
    let Some(CallTarget::Literal(option)) = consts.first.as_ref() else { return None };
    let call = ini_call(builtin, option, positional?)?;
    if call.access != IniAccess::Set || call.cell == SettingCell::Precision {
        return None;
    }
    let class = match consts.float_evidence.iter().find(|(position, _)| *position == 1) {
        Some((_, evidence)) => frame.float_class(cx, evidence),
        None => FloatClass::Unknown,
    };
    match class {
        FloatClass::Yes => {
            let read = SettingCell::Precision.read_label();
            if !labels.contains(&read) {
                labels.push(read);
            }
            None
        }
        FloatClass::No => None,
        FloatClass::Unknown => Some(GapKind::ValueDependentRead),
    }
}

/// What the call shows of each of the gate's deciding arguments, or `None` for a named or spread
/// argument list, whose positions cannot be read.
fn gate_args<'c>(
    gate: SettingReadGate,
    (cx, frame): (&Cx, &Frame),
    name: &NameRef,
    (positional, consts): (Option<usize>, &'c ConstArgs),
) -> Option<Vec<Option<GateArg<'c>>>> {
    let arity = positional?;
    Some(
        gate.positions()
            .iter()
            .map(|&position| {
                if position >= arity {
                    Some(GateArg::Omitted)
                } else if gate.reads_on_null() {
                    timestamp((cx, frame), name, consts, position)
                } else {
                    argument((cx, frame), name, consts, position)
                }
            })
            .collect(),
    )
}

/// What the scan shows of the argument at `position`: a string, an integer it evaluates, a
/// boolean, or a value that is neither a string nor an integer.
fn argument<'c>(
    (cx, frame): (&Cx, &Frame),
    name: &NameRef,
    consts: &'c ConstArgs,
    position: usize,
) -> Option<GateArg<'c>> {
    let at = u8::try_from(position).ok()?;
    // The literals of an encoding-taking call, at any position (ADR-0101 §3.13).
    if let Some((_, literal)) = consts.literals.iter().find(|(p, _)| *p == at) {
        return Some(match literal {
            ArgLiteral::Str(text) => GateArg::Str(text),
            ArgLiteral::Int(v) => GateArg::Int(*v),
            ArgLiteral::Null => GateArg::Null,
            ArgLiteral::Other => GateArg::NotText,
        });
    }
    // An array literal of string-literal patterns (a `preg_*` call's first argument).
    if position == 0
        && let Some(patterns) = &consts.patterns
    {
        return Some(GateArg::Strs(patterns));
    }
    let literal = match position {
        0 => consts.first.as_ref(),
        1 => consts.second.as_ref(),
        _ => None,
    };
    match literal {
        Some(CallTarget::Literal(text)) => return Some(GateArg::Str(text)),
        Some(CallTarget::Bool(flag)) => return Some(GateArg::Bool(*flag)),
        _ => {}
    }
    if let Some((_, expr)) = consts.ints.iter().find(|(p, _)| *p == at) {
        return eval_int(cx, name, expr).map(GateArg::Int);
    }
    if let Some((_, flag)) = consts.bools.iter().find(|(p, _)| *p == at) {
        return Some(GateArg::Bool(*flag));
    }
    match &consts.not_text.iter().find(|(p, _)| *p == at)?.1 {
        NotText::Literal => Some(GateArg::NotText),
        NotText::Param(param) => param_not_text(cx, frame, param).then_some(GateArg::NotText),
    }
}

/// Whether the by-value parameter `param` is shown to hold neither a string nor an integer: no
/// call of the frame takes it by reference, and its declared type has no `string`, `int`,
/// `mixed` or `callable` member (a function name is a string). The coercive and the strict mode
/// both bind the declared type, and the scan has already shown the frame never writes it.
fn param_not_text(cx: &Cx, frame: &Frame, param: &str) -> bool {
    if frame.rebound_by_call(cx, param) {
        return false;
    }
    let Some(span) = frame.params.iter().find(|p| p.name == param).and_then(|p| p.hint_span)
    else {
        return false;
    };
    cx.tree().source_slice(span).is_some_and(|hint| {
        let mut members = hint
            .split(|c: char| matches!(c, '|' | '&' | '(' | ')' | '?') || c.is_whitespace())
            .filter(|m| !m.is_empty())
            .peekable();
        members.peek().is_some()
            && members.all(|m| {
                !matches!(m.to_ascii_lowercase().as_str(), "string" | "int" | "mixed" | "callable")
            })
    })
}

/// The value of a constant integer expression as PHP resolves its names at `name`'s position: a
/// bare constant is the namespace's before the global one ([`global_const_fact`]), a project twin
/// the scan cannot read is `None`, and so is a constant the catalog or project states no integer
/// for.
fn eval_int(cx: &Cx, name: &NameRef, expr: &ConstInt) -> Option<i64> {
    let constant = |raw: &String, kind| {
        let reference = NameRef { raw: raw.clone(), kind, offset: name.offset };
        match global_const_fact(cx, &reference)?.0 {
            Fact::Singleton(Val::Int(v)) => Some(v),
            _ => None,
        }
    };
    match expr {
        ConstInt::Int(v) => Some(*v),
        ConstInt::Const(raw) => constant(raw, RefKind::Unqualified),
        ConstInt::Global(raw) => constant(raw, RefKind::FullyQualified),
        ConstInt::Or(terms) => {
            terms.iter().try_fold(0, |acc, term| Some(acc | eval_int(cx, name, term)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use steins_catalog::{SettingCell, setting_read_gate};

    /// The label this module drops is the registry entry the rows spell: the read of the cell the
    /// gate names.
    #[test]
    fn the_label_is_the_rows_label() {
        for name in ["sort", "substr_compare", "pathinfo", "ctype_alpha", "strftime", "preg_match"] {
            let read = setting_read_gate(name).expect(name).cell().read_label();
            assert_eq!(read, SettingCell::Locale.read_label(), "{name}");
            assert!(steins_catalog::is_known_label(read), "{name}");
            assert!(steins_catalog::effect_labels(name).is_some_and(|l| l.contains(&read)));
        }
    }
}
