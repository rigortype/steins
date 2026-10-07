//! The locale readers whose read the call decides, at a call site (ADR-0101 §3.9, S4).
//!
//! `sort`, `substr_compare`, `pathinfo`, the `ctype_*` predicates, `strnatcmp`, `escapeshellarg`,
//! `strip_tags`, `parse_url` and `strftime` are catalogued with the locale read
//! ([`steins_catalog::effect_labels`]), but read it only where the call reaches the routine that
//! consults it ([`steins_catalog::locale_read_gate`]): under a mode argument (a sort's `$flags`,
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

use steins_catalog::{GateArg, LocaleReadGate, locale_read_gate};
use steins_domain::{Fact, Val};
use steins_syntax::{CallTarget, ConstArgs, ConstInt, NameRef, NotText, RefKind};

use super::GapKind;
use super::reach::Frame;
use crate::cx::Cx;
use crate::global_consts::global_const_fact;

/// The locale cell's read, ADR-0101 §2.2.
const LOCALE: &str = "global.read.setting.locale";

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
    let gate = locale_read_gate(builtin)?;
    match decide(gate, (cx, frame), name, (positional, consts)) {
        Some(true) => None,
        Some(false) => {
            labels.retain(|label| *label != LOCALE);
            None
        }
        None => unreadable_mode(builtin, labels),
    }
}

/// The row of a gated reader handed over as a callback, which its invoker calls with arguments of
/// its choosing, or called with arguments the scan cannot read: the read depends on them, so it
/// is no label and the call is [`GapKind::ValueDependentRead`]. `None` for a name with no gate.
pub(super) fn unreadable_mode(name: &str, labels: &mut Vec<&'static str>) -> Option<GapKind> {
    locale_read_gate(name)?;
    labels.retain(|label| *label != LOCALE);
    Some(GapKind::ValueDependentRead)
}

/// Whether the call reads the locale, or `None` when its deciding arguments do not show it.
fn decide(
    gate: LocaleReadGate,
    (cx, frame): (&Cx, &Frame),
    name: &NameRef,
    (positional, consts): (Option<usize>, &ConstArgs),
) -> Option<bool> {
    let arity = positional?;
    let args: Vec<Option<GateArg<'_>>> = gate
        .positions()
        .iter()
        .map(|&position| {
            if position >= arity {
                Some(GateArg::Omitted)
            } else {
                argument((cx, frame), name, consts, position)
            }
        })
        .collect();
    gate.reads(&args)
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
    use super::LOCALE;

    /// The label this module drops is the registry entry the rows spell.
    #[test]
    fn the_label_is_the_rows_label() {
        assert!(steins_catalog::is_known_label(LOCALE));
        for name in ["sort", "substr_compare", "pathinfo", "ctype_alpha", "strftime"] {
            assert!(steins_catalog::effect_labels(name).is_some_and(|l| l.contains(&LOCALE)));
        }
    }
}
