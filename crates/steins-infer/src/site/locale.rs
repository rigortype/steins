//! The locale readers whose read a mode argument decides, at a call site (ADR-0101 §3.9, S4).
//!
//! `sort` and its siblings, `substr_compare` and `pathinfo` are catalogued with the locale read
//! ([`steins_catalog::effect_labels`]), but read it only under an argument the caller chooses: a
//! sort's `$flags`, `substr_compare`'s `$case_insensitive`, `pathinfo`'s `$flags`
//! ([`steins_catalog::locale_read_gate`]). Like the printf family's format, that argument decides
//! what the call does, and a label is proven at a call only where the read happens on every run of
//! the call as written. So the row is the upper bound this module narrows, in the three ways the
//! criterion of ADR-0101 §3.2 allows:
//!
//! * the argument is **omitted**: the parameter's default decides (`sort($a)` is `SORT_REGULAR`,
//!   which reads nothing; `pathinfo($p)` is `PATHINFO_ALL`, which does);
//! * the argument is a **literal** the scan evaluates (an integer, an engine constant such as
//!   `SORT_LOCALE_STRING`, a `|` of such terms, a `true` or `false`): the read is proven or
//!   dropped by what the catalog states for that value;
//! * anything else, a variable, an expression the scan cannot evaluate, a named or spread
//!   argument list, or the builtin handed over as a callback, is
//!   [`GapKind::ValueDependentRead`] and no label.

use steins_catalog::{GateArg, LocaleReadGate, locale_read_gate};
use steins_syntax::{CallTarget, ConstArgs};

use super::GapKind;
use super::engine::eval_const_int;

/// The locale cell's read, ADR-0101 §2.2.
const LOCALE: &str = "global.read.setting.locale";

/// Narrow the row `labels` of a call to the builtin `name`, when it is a gated reader, by what
/// the call shows of its deciding argument, and say whether the read depends on a value the call
/// site cannot see. `positional` is the call's argument count, `None` for a named or spread list.
pub(super) fn narrow_labels(
    name: &str,
    positional: Option<usize>,
    consts: &ConstArgs,
    labels: &mut Vec<&'static str>,
) -> Option<GapKind> {
    let gate = locale_read_gate(name)?;
    match decide(gate, positional, consts) {
        Some(true) => None,
        Some(false) => {
            labels.retain(|label| *label != LOCALE);
            None
        }
        None => unreadable_mode(name, labels),
    }
}

/// The row of a gated reader handed over as a callback, which its invoker calls with arguments of
/// its choosing, or called with a mode the scan cannot read: the read depends on it, so it is no
/// label and the call is [`GapKind::ValueDependentRead`]. `None` for a name with no gate.
pub(super) fn unreadable_mode(name: &str, labels: &mut Vec<&'static str>) -> Option<GapKind> {
    locale_read_gate(name)?;
    labels.retain(|label| *label != LOCALE);
    Some(GapKind::ValueDependentRead)
}

/// Whether the call reads the locale, or `None` when its deciding argument is not shown.
fn decide(gate: LocaleReadGate, positional: Option<usize>, consts: &ConstArgs) -> Option<bool> {
    let arity = positional?;
    if gate.position() >= arity {
        return gate.reads(GateArg::Omitted);
    }
    gate.reads(argument(consts, gate.position())?)
}

/// What the scan shows of the argument at `position`: an integer it evaluates, or a literal
/// boolean.
fn argument(consts: &ConstArgs, position: usize) -> Option<GateArg> {
    let at = u8::try_from(position).ok()?;
    if let Some((_, expr)) = consts.ints.iter().find(|(p, _)| *p == at) {
        return eval_const_int(expr).map(GateArg::Int);
    }
    if position == 1 {
        return match consts.second {
            Some(CallTarget::Bool(flag)) => Some(GateArg::Bool(flag)),
            _ => None,
        };
    }
    consts.bools.iter().find(|(p, _)| *p == at).map(|(_, flag)| GateArg::Bool(*flag))
}

#[cfg(test)]
mod tests {
    use super::LOCALE;

    /// The label this module drops is the registry entry the rows spell.
    #[test]
    fn the_label_is_the_rows_label() {
        assert!(steins_catalog::is_known_label(LOCALE));
        for name in ["sort", "substr_compare", "pathinfo"] {
            assert!(steins_catalog::effect_labels(name).is_some_and(|l| l.contains(&LOCALE)));
        }
    }
}
