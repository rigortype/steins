//! The return side's possibly grade (ADR-0081 §8's 2026-08-27 amendment, issue
//! #537): some arm of the returned value's abstract fact is rejected by the
//! enclosing function's **native** return type and some is accepted.
//!
//! The argument pair's judgment at the return seam, and deliberately nothing more.
//! It reaches [`is_type_error`] through the same base-level lift
//! ([`native_rejects_base`]) and the same witness set, takes the same
//! minimum-stratum split into two ids (ADR-0052 §5), and takes the
//! all-arms-rejected verdict on the argument side's terms: silent for a `Verified`
//! fact (issue #291's ruling), the possibly id for an `Asserted` one (#1099). (A
//! finite `OneOf` has its own table: `finite_verdict` judges it member by member, and
//! one whose every member is rejected is definite when `Verified`, since every path
//! throws — decision D6 of #1051.) PHP
//! applies one coercion table to both boundaries — all 144 return-position cells of
//! `harness/coercion-grid` answered exactly as their parameter twins at 8.5.9, and
//! so did the object cells (a `__toString` object into `string` in coercive mode,
//! nothing else) — so there is no second table here either.
//!
//! **One thing this seam has that the argument seam does not: object arms.** The
//! shape the issue is about is `function f(A|B $x): B { return $x; }`, and `A|B`
//! has no [`Fact`] at all — the value domain is object-free (ADR-0035/0038/0043),
//! so the union lives only in the declared-arm lane. A class arm is therefore
//! judged through [`Cx::object_is_type_error`], which decides an object of an
//! **exact** class, and only where the arm's class can have no subclass to answer
//! differently ([`Cx::class_has_no_subclass`]). A non-final class arm declines: a
//! subclass may implement an interface the return type accepts, and convicting it
//! would be a false positive no floor makes safe (ADR-0002).
//!
//! **Carrier: `ArgValue::Var`.** `return $x;` is where the declared-arm lane lives
//! and where every shape the issue names sits. A returned call (`return g();`,
//! `return $o->m();`) reaches it through the virtual binding of `return_call`: it is
//! judged as `$v = g(); return $v;` would be, so the two spellings cannot disagree.
//! The offset carrier (`return $a['k'];`) is still named, not shipped (issue #421).
//!
//! [`is_type_error`]: crate::arg_check::is_type_error
//! [`native_rejects_base`]: crate::arg_check::native_rejects_base

use std::collections::HashMap;
use std::slice;

use steins_contract::ContractTy;
use steins_domain::{Fact, Val};
use steins_syntax::{ArgValue, NativeType};

use crate::arg_check::{
    MaybeVerdict, arm_base_set, arm_lane_premise, maybe_fact_verdict, native_rejects_base,
    possibly_null_rejected, spell_arm,
};
use crate::cx::Cx;
use crate::dump::render_contract_arms;
use crate::finite_verdict::check_oneof_return;
use crate::env::{ContractArm, Known, Store, Stratum};
use crate::project::Diagnostic;
use crate::{PHPDOC_MAYBE_RETURN_MISMATCH_ID, TYPE_MAYBE_RETURN_MISMATCH_ID, describe_fact};

/// The premise this judgment consults, and the lane it came from — the two lanes
/// spell their arms differently, and the message says whichever the reader wrote.
enum Premise {
    /// The value lane's abstract fact for the returned variable, at its own
    /// stratum. Scalar-only by construction, so this lane never carries a class arm.
    Value(Fact, Stratum),
    /// The declared-arm lane ([`Store::contract_arms`]), at the arms' own minimum
    /// stratum. The only lane an object union can arrive on.
    Arms(Vec<ContractArm>, Stratum),
}

impl Premise {
    fn stratum(&self) -> Stratum {
        match self {
            Self::Value(_, s) | Self::Arms(_, s) => *s,
        }
    }
}

/// The abstract premise available for a returned value, in the argument side's own
/// lane order: the value lane where it has a fact, else the declared-arm lane.
///
/// A finite fact declines on either lane: a `Singleton` is the concrete lane's, which
/// [`is_type_error`] has already judged exactly one rung above this one, and a `OneOf`
/// is read by `oneof_premise` before this function is asked.
fn return_premise(
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Option<Premise> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    if let Some(k) = env.get(name)
        && let Some(f) = &k.fact
    {
        return f.finite_members().is_none().then(|| Premise::Value(f.clone(), k.stratum));
    }
    let arms = store.contract_arms(name)?;
    let stratum = arms.iter().fold(Stratum::Verified, |acc, a| acc.min(a.stratum));
    Some(Premise::Arms(arms.to_vec(), stratum))
}

/// The members of a returned variable's `OneOf` value-lane fact, at its stratum; an
/// `Asserted` `Singleton` reads as a one-member set, since the exact rung judges only
/// a proven one (#1099).
fn oneof_premise<'e>(
    value: &ArgValue,
    env: &'e HashMap<String, Known>,
    poisoned: bool,
) -> Option<(&'e [Val], Stratum, bool)> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    let k = env.get(name)?;
    match k.fact.as_ref()? {
        Fact::OneOf(members) => Some((members, k.stratum, k.thinned)),
        Fact::Singleton(v) if k.stratum == Stratum::Asserted => {
            Some((slice::from_ref(v), k.stratum, k.thinned))
        }
        _ => None,
    }
}

/// The members of a returned variable's declared-arm lane when it lowers to an
/// `Asserted` finite fact and the value lane holds nothing for the variable.
fn arm_lane_finite(
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Option<(Vec<Val>, Stratum)> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    if env.get(name).is_some_and(|k| k.fact.is_some()) {
        return None;
    }
    let (fact, stratum, _) = arm_lane_premise(store.contract_arms(name)?.to_vec())?;
    Some((fact.finite_members()?.to_vec(), stratum))
}

/// The class an object-world declared arm denotes, or `None` for an arm with a
/// value-lattice reading. An enum case is an object of its enum, which is
/// implicitly final — so the exactness gate below passes it by construction.
fn arm_object_class(ty: &ContractTy) -> Option<&str> {
    match ty {
        ContractTy::Class(fqn) => Some(fqn),
        ContractTy::EnumCase { enum_fqn, .. } => Some(enum_fqn),
        _ => None,
    }
}

/// What a native return type has to say about **one** declared arm.
#[derive(PartialEq, Eq)]
enum ArmVerdict {
    /// Every value the arm denotes raises a `TypeError`.
    Rejected,
    /// At least one value the arm denotes binds.
    Accepted,
    /// The arm has no reading this judgment can decide either way.
    Undecided,
}

/// Judge one declared arm against the native return type.
///
/// All-or-nothing on the reject side, the same rule the argument side's arm speller
/// applies: an arm that denotes several bases is rejected only when each of them is,
/// so a lane's own spelling can never name an arm the type partly accepts.
///
/// [`ArmVerdict::Undecided`] is where every uncertainty goes, and it is not the same
/// answer as [`ArmVerdict::Accepted`] — one arm of it silences the whole judgment
/// (see [`verdict_spelling`]). A **non-final class** arm is the case that matters:
/// [`Cx::object_is_type_error`] decides an object of an exact class, and a subclass
/// may implement an interface the return type accepts. A `float` arm is the other:
/// the value domain has no faithful `float` (its `float` accepts ints), so
/// `steins_contract::to_fact` refuses the spelling and this judgment must too rather
/// than read the refusal as acceptance.
fn judge_arm(cx: &Cx, ret: &NativeType, ty: &ContractTy) -> ArmVerdict {
    if let Some(fqn) = arm_object_class(ty) {
        if !cx.class_has_no_subclass(fqn) {
            return ArmVerdict::Undecided;
        }
        return if cx.object_is_type_error(ret, fqn) {
            ArmVerdict::Rejected
        } else {
            ArmVerdict::Accepted
        };
    }
    match arm_base_set(ty) {
        Some((bases, nullable)) if !bases.is_empty() || nullable => {
            let rejected = bases.iter().all(|b| native_rejects_base(cx, ret, *b))
                && (!nullable || possibly_null_rejected(cx, ret));
            if rejected { ArmVerdict::Rejected } else { ArmVerdict::Accepted }
        }
        _ => ArmVerdict::Undecided,
    }
}

/// The subject spelling, the rejected-arm names and whether **every** arm is rejected
/// for one premise, or `None` where the verdict is not the possibly grade's: nothing
/// rejected, or everything on a `Verified` premise (issue #291's ruling; an `Asserted`
/// one reports, #1099).
fn verdict_spelling(
    cx: &Cx,
    ret: &NativeType,
    premise: &Premise,
) -> Option<(String, Vec<String>, bool)> {
    let asserted = premise.stratum() == Stratum::Asserted;
    match premise {
        Premise::Value(fact, _) => {
            // No return position has an implicit-nullable default, and the internal
            // carve-out is an argument-boundary rule — both flags are `false` here.
            let (verdict, rejected, null_rejected) =
                maybe_fact_verdict(cx, ret, fact, false, false);
            let every = verdict == MaybeVerdict::Every && asserted;
            if verdict != MaybeVerdict::Partial && !every {
                return None;
            }
            let mut named: Vec<String> = rejected.iter().map(spell_arm).collect();
            if null_rejected {
                named.push("null".to_owned());
            }
            let subject =
                describe_fact(fact).trim_start_matches("a value of type ").to_owned();
            Some((subject, named, every))
        }
        Premise::Arms(arms, _) => {
            let judged: Vec<(&ContractArm, ArmVerdict)> =
                arms.iter().map(|a| (a, judge_arm(cx, ret, &a.ty))).collect();
            // One arm this judgment cannot read collapses the whole position to
            // silence — the discipline `lower_hint` applies to a native type with an
            // unmodeled member, for the same reason: "some arm rejected, some
            // accepted" is a claim about the whole arm list, so a list with a hole
            // in it supports neither half of it.
            if judged.iter().any(|(_, v)| *v == ArmVerdict::Undecided) {
                return None;
            }
            let rejected: Vec<&ContractArm> = judged
                .iter()
                .filter(|(_, v)| *v == ArmVerdict::Rejected)
                .map(|(a, _)| *a)
                .collect();
            let every = rejected.len() == arms.len() && asserted;
            if rejected.is_empty() || (rejected.len() == arms.len() && !every) {
                return None;
            }
            // A lane with an arm this speller has no faithful rendering for declines
            // whole rather than naming the rest: the message states what the variable
            // is, and half a union is not that.
            let subject = render_contract_arms(cx, arms)?;
            let named: Vec<String> =
                rejected.iter().filter_map(|a| render_contract_arms(cx, slice::from_ref(a))).collect();
            (named.len() == rejected.len()).then_some((subject, named, every))
        }
    }
}

/// Emit the possibly-grade return finding for one `return` statement, at the point
/// the native proof (`type.return-mismatch`) did **not** fire — so a definite No is
/// never shadowed by its own weaker sibling.
///
/// The plain per-scope pass only: a descent rebinds the callee's parameters, not its
/// return, so the caller's gate (`descent.is_none()`) is the same one `ret_info`
/// already carries.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_maybe_return_mismatch(
    cx: &Cx,
    ret: &NativeType,
    display: &str,
    value: &ArgValue,
    shown: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    span_start: u32,
    out: &mut Vec<Diagnostic>,
) {
    // A `OneOf` is judged member by member, by `finite_verdict`'s table; it is the
    // one finite fact that reaches here (a `Singleton` is the exact rung's).
    if let Some((members, stratum, thinned)) = oneof_premise(value, env, poisoned) {
        check_oneof_return(cx, ret, display, shown, members, stratum, thinned, span_start, out);
        return;
    }
    // An `Asserted` declared literal union (`@param 'a'|'b' $x`) is a finite fact on the
    // arm lane: the same table, read member by member (#1099).
    if let Some((members, stratum)) = arm_lane_finite(value, env, store, poisoned) {
        check_oneof_return(cx, ret, display, shown, &members, stratum, false, span_start, out);
        return;
    }
    let Some(premise) = return_premise(value, env, store, poisoned) else { return };
    let Some((subject, named, every)) = verdict_spelling(cx, ret, &premise) else { return };
    let id = if premise.stratum() == Stratum::Verified {
        TYPE_MAYBE_RETURN_MISMATCH_ID
    } else {
        PHPDOC_MAYBE_RETURN_MISMATCH_ID
    };
    let subject_name = shown.render();
    let arms = if every {
        format!("every one of its arms ({}) raises a TypeError", named.join(", "))
    } else if named.len() == 1 {
        format!("its {} arm raises a TypeError", named[0])
    } else {
        format!("its {} arms raise a TypeError", named.join(" and "))
    };
    let pos = cx.tree().position(span_start);
    out.push(Diagnostic {
        id,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "return value {subject_name} may not become {} (return type of {display}()) — {subject_name} is {subject}, and {arms} ({} mode)",
            ret.render(),
            if cx.strict() { "strict" } else { "coercive" },
        ),
        facet: None,
        fix: None,
    });
}
