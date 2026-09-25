//! The fact of a `??` chain (ADR-0052 §6, ADR-0062 A-G11 / S5):
//! [`eval_coalesce_fact`], and the projection and cover predicates the offset
//! family reads too, so that a premise and a cover cannot disagree.

use std::collections::HashMap;

use steins_domain::{CoverFlavor, Fact, ShapeFact, Val, Key as VKey};
use steins_syntax::{ArgValue, Span};

use crate::by_value::value_stratum;
use crate::fold::Folder;
use crate::cond::coalesce_lhs_proven_present;
use crate::env::{Known, Store, Stratum, singleton_fact};
use crate::offsets::{ShapeRead, offset_key_of, offset_operand_fact, shape_read};
use crate::refine::clear_null;
use crate::walk::{WalkCx, mark_dead_span};

/// The fact of a `??` chain (ADR-0052 §6, extended by ADR-0062 A-G11 / S5).
///
/// The join law is unchanged: `clear_null(fact($a)) join fact($b)`, folded
/// right-to-left over the whole spine (`??` is right-associative, so `$a ?? $b ??
/// $c` is one chain, not two nested pairs). An operand the domain cannot spell
/// yields `None` for the whole expression; every arm but the last contributes only
/// its non-null part.
///
/// What S5 adds is the premise ladder (A-G11). A `??` arm is reached only when
/// every arm to its left failed `isset`, so a pure depth-1 projection arm
/// `$x['k']` contributes the premise `¬isset($x['k'])` to everything after it.
/// [`ShapeFact::cover_proves`] consumes those: a KeyCover from
/// `isset($x['a']) || isset($x['b'])` plus `¬isset($x['a'])` proves `$x['b']`
/// present, turning an otherwise undischarged optional read into a value.
///
/// Any other arm form invalidates the ladder (A-G11's conservatism): a call may
/// write through a reference and make an earlier `¬isset` stale, so a non-projection
/// arm contributes no premise and drops every accumulated one — why
/// `$x['a'] ?? f() ?? $x['b']` discharges nothing.
///
/// # A non-projection arm settles the chain too (issue #630)
///
/// `settled` ends the spine at an arm PHP proves is the value, because nothing to
/// its right is evaluated. It used to be computed **only** inside the projection
/// branch, and the other branch hardcoded `false` — so a left arm that is a literal
/// or a proven-non-null variable never settled and the join added an arm PHP never
/// reaches: `'foo' ?? null` answered `'foo'|null`, `$scalar = 3; $scalar ?? 4`
/// answered `3|4`. The predicate that branch wants is
/// [`coalesce_lhs_proven_present`], which already stood next door in the assignment
/// seam deciding the same question for deadness alone.
///
/// **This evaluator now owns the `mark_dead_span` record**, the choice issue #625's
/// leg 1 made for [`eval_ternary_fact`]: one predicate decides the value and the
/// deadness together, so no seam can answer `??` with a fact while disagreeing about
/// which arms PHP ran. The assignment seam's separate call is gone. `rest` is the
/// source extent to the right of each arm, threaded by
/// [`flatten_coalesce_spans`].
///
/// The projection branch's `settled` deliberately does **not** mark. Its presence
/// claim comes from a shape fact, which is `Asserted` — and reachability stays
/// proof-only, the same line `coalesce_lhs_proven_present`'s own third refusal
/// draws. A shape may say a key is `Required` on a docblock's word; that is enough
/// to pick a value, never enough to prove code unreached.
///
/// [`eval_ternary_fact`]: crate::cond::eval_ternary_fact
/// [`coalesce_lhs_proven_present`]: crate::cond::coalesce_lhs_proven_present
pub(crate) fn eval_coalesce_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    a: &ArgValue,
    b: &ArgValue,
    rhs_span: Span,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<(Fact, Stratum)> {
    let (poisoned, php_minor) = (w.scope.poisoned, w.cx.php_minor);
    let mut arms: Vec<(&ArgValue, Option<Span>)> = Vec::new();
    flatten_coalesce_spans(a, Some(rhs_span), &mut arms);
    flatten_coalesce_spans(b, None, &mut arms);
    let last = arms.len() - 1;

    let mut premises: Vec<(String, VKey)> = Vec::new();
    // `Option<(Option<Fact>, Stratum)>`: the outer `None` is "the domain cannot
    // spell this arm" and ends the whole expression; the INNER `None` is "PHP
    // proves this arm falls through", which contributes no value and still
    // contributes its stratum.
    let mut parts: Vec<(Option<Fact>, Stratum)> = Vec::with_capacity(arms.len());
    for (i, (arm, rest)) in arms.iter().enumerate() {
        let projection = coalesce_projection(arm, env, poisoned, php_minor);
        let (part, settled) = match &projection {
            Some((var, key)) => {
                let (f, s, settled) = coalesce_arm_fact(var, key, env, &premises, i == last)?;
                (Some((f, s)), settled)
            }
            None => {
                // Marked before the fact is demanded, so an arm proven present that
                // the domain still cannot spell records the deadness it proves —
                // exactly what the assignment seam did with its own call.
                let settled = store
                    .is_some_and(|s| coalesce_lhs_proven_present(w, folder, arm, env, s));
                if settled && let Some(span) = rest {
                    mark_dead_span(w, *span);
                }
                (
                    arg_value_fact(w, folder, arm, env)
                        .map(|f| (Some(f), value_stratum(w.cx, arm, env, store))),
                    settled,
                )
            }
        };
        parts.push(part?);
        // An arm proven present and non-null is the value: `??` never evaluates
        // anything to its right, so the chain ends here.
        if settled {
            break;
        }
        match projection {
            Some(p) => premises.push(p),
            None => premises.clear(),
        }
    }

    // Derivation clause (ADR-0052 §5): result is no stronger than the weakest arm.
    // The last arm is the value whenever everything left of it fell through, so it
    // is the one arm that must carry a fact.
    let (last_fact, mut stratum) = parts.pop()?;
    let mut acc = last_fact?;
    while let Some((fact, s)) = parts.pop() {
        stratum = stratum.min(s);
        // A provably-absent arm (no fact) and a provably-null one (nothing survives
        // `clear_null`) are the same law: PHP falls through both, so neither
        // contributes a value — and both still contribute their stratum.
        if let Some(nonnull) = fact.as_ref().and_then(clear_null) {
            acc = nonnull.join(&acc)?;
        }
    }
    Some((acc, stratum))
}

/// Flatten a `??` spine into its arms, left to right. Both sides are walked: `??`
/// is right-associative, so the nesting is normally on the right, but explicit
/// parentheses (`($a ?? $b) ?? $c`) nest left and mean the same chain.
pub(crate) fn flatten_coalesce<'a>(v: &'a ArgValue, out: &mut Vec<&'a ArgValue>) {
    match v {
        ArgValue::Coalesce(a, b, _) => {
            flatten_coalesce(a, out);
            flatten_coalesce(b, out);
        }
        _ => out.push(v),
    }
}

/// [`flatten_coalesce`] carrying, per arm, the source extent of everything to its
/// **right** in the chain — the region `mark_dead_span` records when that arm
/// settles (issue #630). `rest` is what lies to the right of `v` itself.
///
/// `ArgValue::Coalesce(a, b, rhs_span)` spans `b` with `rhs_span`, so on the
/// ordinary right-associative nesting each arm gets exactly the extent the
/// assignment seam used to mark for the head arm, and the arms after it get their
/// own tighter ones. A left-nested chain `($x ?? $y) ?? $z` gives `$x` the extent of
/// `$y` alone: `$z` is dead too, but recording a sub-extent only *under*-suppresses,
/// which is the FP-safe side and the only side deadness may err on.
fn flatten_coalesce_spans<'a>(
    v: &'a ArgValue,
    rest: Option<Span>,
    out: &mut Vec<(&'a ArgValue, Option<Span>)>,
) {
    match v {
        ArgValue::Coalesce(a, b, rhs_span) => {
            flatten_coalesce_spans(a, Some(*rhs_span), out);
            flatten_coalesce_spans(b, rest, out);
        }
        _ => out.push((v, rest)),
    }
}

/// Is this `??` arm a pure depth-1 projection `$x[k]` with a resolvable constant
/// key (A-G11's premise carrier)? Key resolution is the offset family's own
/// ([`offset_operand_fact`] + [`offset_key_of`]), so a premise and a cover can
/// never disagree.
///
/// Depth is exactly one and the base is exactly a binding: `$x['a']['b']` and
/// `$this->x['a']` are not premise carriers and invalidate the ladder rather than
/// extending it.
pub(crate) fn coalesce_projection(
    arm: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    php_minor: Option<(u16, u16)>,
) -> Option<(String, VKey)> {
    let ArgValue::OffsetRead { base, key } = arm else { return None };
    let ArgValue::Var(name) = base.as_ref() else { return None };
    let Some(Fact::Singleton(key_val)) = offset_operand_fact(key, env, poisoned, php_minor) else {
        return None;
    };
    Some((name.clone(), offset_key_of(&key_val)?))
}

/// The fact a projection arm `$var[key]` contributes to its `??` chain, the
/// stratum it inherits from the base's shape fact (derivation clause: never
/// stronger than the base — always `Asserted`), and whether the arm settles the
/// chain (proven to be the value, so `??` never evaluates further).
///
/// A non-final arm is used only when `isset` holds of it, so presence needn't be
/// proved — missing means fall-through, yielding its declared slot
/// ([`ShapeRead::taken_fact`]). The final arm is the value whenever everything to
/// its left fell through, so it must be *proved* present: a `Required` field
/// proves it outright; an optional one needs A-G11's cover discharge, with
/// `absent` the accumulated `¬isset` ladder over this base.
///
/// # The base may be an order-witnessed VALUE, not only an abstract shape
///
/// A fully literal array binds `Fact::Singleton(Val::Array)`, never `Fact::Shape`,
/// so `$array = [1, 2, 3]; $array['string'] ?? 0` used to decline on the base test
/// alone — while `isset($array['string'])` on the very same binding answered
/// `false`, because the `isset` lane reads a literal array directly. The base is
/// **lifted** here for the same reason and by the same call the offset-write barrier
/// uses ([`ShapeFact::lift`], issue #327): a witnessed value is strictly more
/// precise than the shape it lifts to, so reading it through the shape law can only
/// lose precision, never invent it.
///
/// # A provably-absent arm falls through; it does not silence the chain
///
/// `DeclaredAbsent` is a *proof* — a `Sealed` shape's non-field, or a field the
/// declaration marks `Absent`. PHP's `??` skips such an arm and evaluates the next
/// one, so the arm contributes **no fact** and the chain goes on. Returning `None`
/// for it, which is what `taken_fact()` alone did, conflated "this arm is proven
/// not to be the value" with "the domain cannot spell this arm" and let the first
/// kill the whole expression. That is the same law the join loop already applies
/// one level up to a provably-null arm.
///
/// The absent arm still contributes its **stratum**: the absence rests on the
/// base's fact, so an `Asserted` shape's word about a missing key cannot buy a
/// `Verified` answer.
fn coalesce_arm_fact(
    var: &str,
    key: &VKey,
    env: &HashMap<String, Known>,
    premises: &[(String, VKey)],
    final_arm: bool,
) -> Option<(Option<Fact>, Stratum, bool)> {
    let known = env.get(var)?;
    let lifted;
    let shape: &ShapeFact = match &known.fact {
        Some(Fact::Shape { shape, nullable: false }) => shape.as_ref(),
        Some(Fact::Singleton(Val::Array(entries))) => {
            lifted = ShapeFact::lift(entries);
            &lifted
        }
        _ => return None,
    };
    let read = shape_read(shape, key);
    // Present and non-null is exactly PHP's `isset` — same test in both positions,
    // harmless on the last arm and a short-circuit before it.
    let settled = matches!(&read, ShapeRead::Present(Some(f)) if f.is_null().is_no());
    if !final_arm {
        // Proven absent: PHP falls through, so no fact and no silence.
        if matches!(read, ShapeRead::DeclaredAbsent) {
            return Some((None, known.stratum, false));
        }
        return read.taken_fact().map(|f| (Some(f), known.stratum, settled));
    }
    if let ShapeRead::Present(Some(f)) = read {
        return Some((Some(f), known.stratum, settled));
    }
    let absent: Vec<VKey> =
        premises.iter().filter(|(v, _)| v == var).map(|(_, k)| k.clone()).collect();
    let flavor = cover_discharges(shape, key, &absent)?;
    let slot = shape.field(key).and_then(|(_, _, s)| s.as_deref().cloned())?;
    match flavor {
        // "at least one covered key is present AND non-null": this one carries it.
        CoverFlavor::Isset => clear_null(&slot).map(|f| (Some(f), known.stratum, true)),
        // "at least one covered key EXISTS", value possibly null (A-G11's table).
        CoverFlavor::KeyExists => Some((Some(slot), known.stratum, false)),
    }
}

/// The S5 discharge, asked as a presence question (A-G11's table): does a
/// recorded KeyCover plus the accumulated `¬isset(absent)` ladder prove `key`
/// present? `Some(flavor)` when it does, `None` otherwise.
///
/// Split out of [`coalesce_arm_fact`] so the value lane and S6's finding lane
/// consult one predicate: a discharged key with an unrepresentable value slot
/// yields no fact but is still proven present, so the finding lane must stay
/// silent — folding the two would emit a false positive on an unspellable slot.
pub(crate) fn cover_discharges(shape: &ShapeFact, key: &VKey, absent: &[VKey]) -> Option<CoverFlavor> {
    match shape.cover_proves(key, absent)? {
        CoverFlavor::Isset => Some(CoverFlavor::Isset),
        // A present-*null* earlier key satisfies a KeyExists claim while `??` still
        // falls through it, so the discharge is sound only when every premise key's
        // declared value is provably non-nullable ("fell through" == "absent").
        CoverFlavor::KeyExists => absent
            .iter()
            .all(|k| {
                shape
                    .field(k)
                    .and_then(|(_, _, s)| s.as_deref())
                    .is_some_and(|f| f.is_null().is_no())
            })
            .then_some(CoverFlavor::KeyExists),
    }
}

/// The value-domain fact of an rvalue operand for the `??` join: a bare variable's
/// env fact, or a literal/foldable value's `Singleton`. Non-representable operands
/// (calls, offsets → `Other`, objects) yield `None`.
fn arg_value_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    arg: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<Fact> {
    match arg {
        ArgValue::Var(name) if !w.scope.poisoned => env.get(name)?.fact.clone(),
        _ => {
            let lit = w.cx.resolve_literal(arg, env, w.scope.poisoned, folder)?;
            singleton_fact(&lit, w.cx.php_minor)
        }
    }
}
