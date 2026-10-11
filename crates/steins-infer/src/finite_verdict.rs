//! The verdict on a **finite** fact with more than one member (`Fact::OneOf`), at an
//! argument and at a return (ADR-0043's 2026-10-11 amendment on #950).
//!
//! Before it, such a fact fell between the rungs: the exact rung
//! ([`is_type_error`]) judges one literal, and the abstract rungs
//! (`maybe_arg_premise`, `return_premise`) declined every finite fact "because the
//! exact rung judged it". `$b = $r ? null : 1; ti($b)` therefore went unreported
//! while the same call with a `?int` parameter reported, though PHP raises a
//! `TypeError` on the `null` path of both.
//!
//! # The rule table
//!
//! A member is **rejected** when [`is_type_error`] says so for that literal in the
//! file's own mode (strict or coercive), so the per-member judgment is the exact
//! rung's, never a second table. `null` is not rejected where the position admits it
//! on its own: an implicit-nullable default, or the internal-null coercive carve-out.
//!
//! ```text
//! fact                       members rejected   stratum        verdict
//! -------------------------- ------------------ -------------- -------------------------------
//! Singleton                  1 of 1             Verified       type.argument-mismatch /
//!                                                              type.return-mismatch (the exact
//!                                                              rung, unchanged)
//! OneOf                      all                Verified       the same definite id: every path
//!                                                              throws (decision D6)
//! OneOf                      all                Asserted       silent: the proof layer needs a
//!                                                              Verified premise (ADR-0052 §5)
//! OneOf                      some               Verified       type.maybe-argument-mismatch /
//!                                                              type.maybe-return-mismatch
//!                                                              (Floor::Strict)
//! OneOf                      some               Asserted       phpdoc.maybe-argument-mismatch /
//!                                                              phpdoc.maybe-return-mismatch
//! OneOf                      none               any            silent
//! Union / Refined / General  all                any            silent (issue #291's ruling)
//! Union / Refined / General  some               any            the possibly grade, unchanged
//! ```
//!
//! Which carriers reach this judgment: a variable whose value lane holds the
//! `OneOf`, and a call whose proven return summary is one (an argument `g($x)`, or
//! a returned `f($x)` through the same virtual binding `$v = f($x); return $v;`
//! takes). A `OneOf` read from a declared arm list, a builtin's return envelope or
//! an array shape stays declined: those lanes are `Asserted` declarations rather
//! than proven values, and they have their own tables.
//!
//! [`is_type_error`]: crate::arg_check::is_type_error

use steins_domain::Val;
use steins_syntax::{ArgValue, NativeType, Param};

use crate::arg_check::{implicit_null_accepted, is_type_error};
use crate::cx::Cx;
use crate::env::{Stratum, arg_of_val, render_val};
use crate::project::Diagnostic;
use crate::{
    PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, PHPDOC_MAYBE_RETURN_MISMATCH_ID, RETURN_ID,
    TYPE_MAYBE_ARGUMENT_MISMATCH_ID, TYPE_MAYBE_RETURN_MISMATCH_ID,
};

/// What a native type says about the members of a `OneOf`.
pub(crate) enum FiniteVerdict {
    /// No member is rejected.
    Nothing,
    /// Some members are rejected, some accepted: the rejected ones, rendered.
    Some(Vec<String>),
    /// Every member is rejected.
    Every,
}

/// Judge each member of a `OneOf` by the exact rung, in the file's mode.
/// `null_ok` is the position's own reason to admit a `null` the type does not spell.
pub(crate) fn finite_verdict(
    cx: &Cx,
    ty: &NativeType,
    members: &[Val],
    null_ok: bool,
) -> FiniteVerdict {
    let rejected: Vec<String> = members
        .iter()
        .filter(|m| {
            let arg = arg_of_val(m);
            is_type_error(cx, ty, &arg) && !(null_ok && matches!(arg, ArgValue::Null))
        })
        .map(render_val)
        .collect();
    if rejected.is_empty() {
        FiniteVerdict::Nothing
    } else if rejected.len() == members.len() {
        FiniteVerdict::Every
    } else {
        FiniteVerdict::Some(rejected)
    }
}

/// The members, spelled the way the message names the subject.
fn subject_of(members: &[Val]) -> String {
    let named: Vec<String> = members.iter().map(render_val).collect();
    format!("one of {}", named.join(", "))
}

fn arms_clause(rejected: &[String]) -> String {
    if rejected.len() == 1 {
        format!("its {} arm raises a TypeError", rejected[0])
    } else {
        format!("its {} arms raise a TypeError", rejected.join(" and "))
    }
}

fn mode_of(cx: &Cx) -> &'static str {
    if cx.strict() { "strict" } else { "coercive" }
}

/// Judge a `OneOf` argument. Returns whether the **definite** finding was emitted,
/// so the caller can keep its weaker siblings quiet about the same argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_oneof_argument(
    cx: &Cx,
    param: &Param,
    ty: &NativeType,
    callee: &str,
    offset: u32,
    value: &ArgValue,
    members: &[Val],
    stratum: Stratum,
    null_tolerated: bool,
    out: &mut Vec<Diagnostic>,
) -> bool {
    let null_ok = null_tolerated || implicit_null_accepted(param, &ArgValue::Null);
    match finite_verdict(cx, ty, members, null_ok) {
        FiniteVerdict::Nothing => false,
        FiniteVerdict::Every if stratum == Stratum::Verified => {
            let provenance = subject_of(members);
            out.push(cx.diagnostic(offset, value, Some(&provenance), callee, &param.name, ty));
            true
        }
        FiniteVerdict::Every => false,
        FiniteVerdict::Some(rejected) => {
            let id = if stratum == Stratum::Verified {
                TYPE_MAYBE_ARGUMENT_MISMATCH_ID
            } else {
                PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID
            };
            let subject_name = value.render();
            let pos = cx.tree().position(offset);
            out.push(Diagnostic {
                id,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: format!(
                    "argument {subject_name} to {callee}() may not become {} ${} — {subject_name} is {}, and {} ({} mode)",
                    ty.render(),
                    param.name,
                    subject_of(members),
                    arms_clause(&rejected),
                    mode_of(cx),
                ),
                facet: None,
                fix: None,
            });
            false
        }
    }
}

/// Judge a `OneOf` return. `shown` is the returned expression as written (`$b`, or
/// `f(1)` for a call judged through its virtual binding).
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_oneof_return(
    cx: &Cx,
    ret: &NativeType,
    display: &str,
    shown: &ArgValue,
    members: &[Val],
    stratum: Stratum,
    span_start: u32,
    out: &mut Vec<Diagnostic>,
) {
    let (id, message) = match finite_verdict(cx, ret, members, false) {
        FiniteVerdict::Nothing => return,
        FiniteVerdict::Every if stratum == Stratum::Verified => (
            RETURN_ID,
            format!(
                "return {} cannot become {} (return type of {display}()) — proven TypeError on every path: {} ({} mode)",
                shown.render(),
                ret.render(),
                subject_of(members),
                mode_of(cx),
            ),
        ),
        FiniteVerdict::Every => return,
        FiniteVerdict::Some(rejected) => {
            let id = if stratum == Stratum::Verified {
                TYPE_MAYBE_RETURN_MISMATCH_ID
            } else {
                PHPDOC_MAYBE_RETURN_MISMATCH_ID
            };
            let name = shown.render();
            (
                id,
                format!(
                    "return value {name} may not become {} (return type of {display}()) — {name} is {}, and {} ({} mode)",
                    ret.render(),
                    subject_of(members),
                    arms_clause(&rejected),
                    mode_of(cx),
                ),
            )
        }
    };
    let pos = cx.tree().position(span_start);
    out.push(Diagnostic {
        id,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}
