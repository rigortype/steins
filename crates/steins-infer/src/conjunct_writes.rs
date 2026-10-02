//! What a later conjunct's write makes stale (issue #654): the names a condition
//! rebinds, and the condition a branch may apply once the refinements those
//! writes outlived are masked out.
//!
//! PHP evaluates `a && b` left to right, so `b` runs after `a` has been tested.
//! A refinement `a` implies holds of the value `a` tested; when `b` then rebinds
//! the name, the branch sees `b`'s value and the refinement describes one that
//! is gone:
//!
//! ```php
//! $x = null;
//! if ($x === null && ($x = fetch()) !== null) {
//!     $x->foo();   // `$x` is what fetch() returned, never the null tested
//! }
//! ```
//!
//! The walk forgets every name a condition may write before a branch clones, and
//! then applies the branch's refinements — which re-minted `$x` as `null` from
//! the left conjunct. The mask sits where every application meets: an `if`'s
//! two sides, a loop body's entry and a break-free loop's exit all go through
//! [`apply_cond_side`], and `assert()`'s fall-through calls it in the walk. A
//! fix in `walk_if`'s own invalidation step would have missed the loops, whose
//! entry never runs that step at all.
//!
//! [`apply_cond_side`]: crate::branch::apply_cond_side

use std::borrow::Cow;

use steins_syntax::{ArgValue, CallExpr, Callee, CondExpr, CondOperand, OperandSpan};

use crate::by_value::arg_is_by_ref;
use crate::cx::Cx;

/// `cond` with every conjunct masked whose names a LATER conjunct rebinds —
/// what a branch the condition decides may apply (issue #654).
///
/// Both connectives mask, on either polarity. `a && b` on its true side and
/// `a || b` on its false side ran `b` after `a`, which is the case the issue is
/// about; on the other two sides no vocabulary distributes over the operands,
/// and a mask there removes nothing that was applied.
///
/// A masked conjunct becomes an `Opaque` with nothing to forget, which every
/// vocabulary reads as refining nothing. The whole conjunct goes rather than
/// only its refinement of the rebound name — a leaf is one test, and the tests
/// that mention two names (`in_array($x, $y)`) are rare where a later conjunct
/// assigns one of them. The walk has already forgotten the rebound name before
/// the branch, so the name enters the branch unrefined, as its rebinding left
/// it. A conjunct that rebinds nothing leaves the condition borrowed, and the
/// order the vocabularies apply in (the DR2 type predicates first) is the
/// caller's and unchanged.
pub(crate) fn mask_stale_conjuncts<'c>(cx: &Cx, cond: &'c CondExpr) -> Cow<'c, CondExpr> {
    match cond {
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            let later = cond_rebinds(cx, b);
            let left = mask_stale_conjuncts(cx, a);
            let left = match mask_mentions(&left, &later) {
                Some(masked) => Cow::Owned(masked),
                None => left,
            };
            let right = mask_stale_conjuncts(cx, b);
            if matches!((&left, &right), (Cow::Borrowed(_), Cow::Borrowed(_))) {
                return Cow::Borrowed(cond);
            }
            Cow::Owned(rebuild(cond, left.into_owned(), right.into_owned()))
        }
        CondExpr::Not(c) => match mask_stale_conjuncts(cx, c) {
            Cow::Borrowed(_) => Cow::Borrowed(cond),
            Cow::Owned(masked) => Cow::Owned(CondExpr::Not(Box::new(masked))),
        },
        _ => Cow::Borrowed(cond),
    }
}

/// Every name `cond` may rebind, in first-occurrence order: the targets an
/// assignment or an increment writes, as the lowering recorded them, and every
/// variable a call hands to a parameter its callee declares by reference.
///
/// A callee this walk cannot resolve — a method, a dynamic call, an ambiguous
/// name — rebinds nothing here. That is the forget-and-re-derive behaviour the
/// guard walk had before this rule existed, kept for the calls it cannot see
/// into, and not a claim that they write nothing.
pub(crate) fn cond_rebinds(cx: &Cx, cond: &CondExpr) -> Vec<String> {
    let mut out = Vec::new();
    collect_rebinds(cx, cond, &mut out);
    out
}

fn collect_rebinds(cx: &Cx, cond: &CondExpr, out: &mut Vec<String>) {
    match cond {
        CondExpr::Cmp { lhs, rhs, .. } => {
            operand_rebinds(cx, lhs, out);
            operand_rebinds(cx, rhs, out);
        }
        CondExpr::Truthy(op) | CondExpr::Instanceof { operand: op, .. } => {
            operand_rebinds(cx, op, out);
        }
        CondExpr::InstanceofDyn { operand, class, .. } => {
            operand_rebinds(cx, operand, out);
            operand_rebinds(cx, class, out);
        }
        CondExpr::Call { call, writes, .. } => {
            push_all(writes, out);
            by_ref_arguments(cx, call, out);
        }
        CondExpr::Opaque { writes, .. } => push_all(writes, out),
        CondExpr::Not(c) => collect_rebinds(cx, c, out),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            collect_rebinds(cx, a, out);
            collect_rebinds(cx, b, out);
        }
        CondExpr::Isset { .. } | CondExpr::IssetVar { .. } => {}
    }
}

/// An operand's rebinds: its written targets, plus every name its call sites
/// hand to a reference parameter. The sites are the lowering's own ADR-0070
/// evidence, and an operand holding an assignment carries none, so the call the
/// operand *is* is read directly as well.
fn operand_rebinds(cx: &Cx, op: &CondOperand, out: &mut Vec<String>) {
    let CondOperand::Other { call, sites, writes, .. } = op else { return };
    push_all(writes, out);
    for entry in sites {
        if entry.sites.iter().any(|(callee, position)| arg_is_by_ref(cx, callee, *position))
            && !out.contains(&entry.name)
        {
            out.push(entry.name.clone());
        }
    }
    if let Some(call) = call {
        by_ref_arguments(cx, call, out);
    }
}

/// The variables a function call hands to its callee's reference parameters —
/// a bare `$v`, or the root of an offset chain (`f($v['k'])` writes through
/// into `$v`).
fn by_ref_arguments(cx: &Cx, call: &CallExpr, out: &mut Vec<String>) {
    let (Callee::Function(_), Some(callee)) = (&call.receiver, &call.callee_ref) else { return };
    if !call.positional_only || call.has_spread {
        return;
    }
    for (position, arg) in (0u32..).zip(&call.args) {
        if let Some(root) = offset_root(&arg.value)
            && !out.iter().any(|n| n == root)
            && arg_is_by_ref(cx, callee, position)
        {
            out.push(root.to_owned());
        }
    }
}

fn offset_root(v: &ArgValue) -> Option<&str> {
    match v {
        ArgValue::Var(name) => Some(name),
        ArgValue::OffsetRead { base, .. } => offset_root(base),
        _ => None,
    }
}

fn push_all(names: &[String], out: &mut Vec<String>) {
    for n in names {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
}

/// `cond` with every leaf that mentions one of `names` replaced by a test that
/// refines nothing, or `None` when no leaf does.
fn mask_mentions(cond: &CondExpr, names: &[String]) -> Option<CondExpr> {
    if names.is_empty() {
        return None;
    }
    match cond {
        CondExpr::Not(c) => mask_mentions(c, names).map(|m| CondExpr::Not(Box::new(m))),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            let (ma, mb) = (mask_mentions(a, names), mask_mentions(b, names));
            if ma.is_none() && mb.is_none() {
                return None;
            }
            let left = ma.unwrap_or_else(|| (**a).clone());
            let right = mb.unwrap_or_else(|| (**b).clone());
            Some(rebuild(cond, left, right))
        }
        leaf => leaf_mentions(leaf, names)
            .then(|| CondExpr::Opaque { reads: Vec::new(), writes: Vec::new() }),
    }
}

/// Whether a leaf test mentions one of `names` anywhere a vocabulary could
/// narrow it from: a bare variable or offset base operand, the arguments of a
/// call operand (its `invalidates`, which is the operand's whole read set once
/// it holds a call), or a guard call's read set. An operand that neither is nor
/// holds a call and writes nothing (`$o->p`) narrows nothing and mentions
/// nothing here.
fn leaf_mentions(leaf: &CondExpr, names: &[String]) -> bool {
    let named = |n: &String| names.contains(n);
    match leaf {
        CondExpr::Cmp { lhs, rhs, .. } => {
            operand_mentions(lhs, names) || operand_mentions(rhs, names)
        }
        CondExpr::Truthy(op) | CondExpr::Instanceof { operand: op, .. } => {
            operand_mentions(op, names)
        }
        CondExpr::InstanceofDyn { operand, class, reads } => {
            operand_mentions(operand, names)
                || operand_mentions(class, names)
                || reads.iter().any(named)
        }
        CondExpr::Call { reads, .. } | CondExpr::Opaque { reads, .. } => reads.iter().any(named),
        CondExpr::Isset { var, .. } | CondExpr::IssetVar { var } => named(var),
        CondExpr::Not(_) | CondExpr::And(..) | CondExpr::Or(..) => false,
    }
}

fn operand_mentions(op: &CondOperand, names: &[String]) -> bool {
    match op {
        CondOperand::Var(v) | CondOperand::Offset { var: v, .. } => names.contains(v),
        CondOperand::Other { invalidates, .. } => invalidates.iter().any(|n| names.contains(n)),
        CondOperand::Literal(_) | CondOperand::Const(_) | CondOperand::ClassConst(..) => false,
    }
}

/// The same connective as `cond` over new operands.
fn rebuild(cond: &CondExpr, left: CondExpr, right: CondExpr) -> CondExpr {
    let (left, right) = (Box::new(left), Box::new(right));
    match cond {
        CondExpr::Or(_, _, span) => CondExpr::Or(left, right, *span),
        CondExpr::And(_, _, span) => CondExpr::And(left, right, *span),
        _ => CondExpr::And(left, right, OperandSpan::NONE),
    }
}
