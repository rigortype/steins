use steins_domain::Certainty;
use steins_domain::{Base, Fact};
use steins_syntax::{NativeType, RetHintKind, ScalarType, Scope, TypeMember};

use crate::coerce::coerce_fact_to_native;
use crate::cx::Cx;
use crate::env::{
    ExitContribution, HeapObj, HeapSummary, PropFact, ReturnSummary, Stratum, SummaryValue,
};
use crate::return_arms::enforced_top_arms;

/// Build the [`ReturnSummary`] from a callee's collected returning-exit contributions:
/// the value component (T0 — join the value facts (A1), a factless exit contributing
/// the declared value floor (A3), the stratum `min` over exits (A4)) and, **beside**
/// it and never inside it, the heap component (T1 — [`join_heap_exits`], §2.4).
///
/// The two are independent (T1 amendment B3): each refusal below is the value
/// component's own, so a callee whose value summary dies for want of a representable
/// floor — which is EVERY object-returning factory — still crosses its allocation.
///
/// The **`$this`** component (the 2026-08-17 amendment, D3) is a third, joined from
/// its own exit list by the same `join_heap_exits` and independent of both: a
/// constructor reads it where its `new` site mints the object, and every other
/// seeded walk reads it as the copy-back into the caller's own.
///
/// `None` only when no component survived.
pub(super) fn join_summary(
    cx: &Cx,
    callee_scope: &Scope,
    exits: &[ExitContribution],
    this_exits: &[ExitContribution],
) -> Option<ReturnSummary> {
    // Generators: the call result is a Generator, not the value of `return` after
    // `yield` (ADR-0057 §5) — refuse EVERY component. The one refusal the heap
    // component shares, and for the heap the reason is even plainer: the returned
    // allocation is not what the call evaluates to. For the `$this` component it is
    // plainer still — the body does not run at the call at all, so there is no exit
    // state to copy back (D5).
    if callee_scope.is_generator {
        return None;
    }
    let heap = join_heap_exits(exits);
    let this = join_heap_exits(this_exits);
    let value = join_value_component(cx, callee_scope, exits);
    (value.is_some() || heap.is_some() || this.is_some())
        .then_some(ReturnSummary { value, heap, this })
}

/// The value half of [`join_summary`] (ADR-0057 amendment T0), unchanged in content
/// by T1 — lifted into its own function only so its several refusals stop being
/// refusals of the whole summary.
fn join_value_component(
    cx: &Cx,
    callee_scope: &Scope,
    exits: &[ExitContribution],
) -> Option<SummaryValue> {
    let ret = cx.scope_return(callee_scope).map(|(ty, _)| ty);
    // A written return hint Steins cannot lower (`: void`, `: never`, a DNF union, …)
    // leaves `scope_return` as `None`, so the A2 native-oracle arms are empty and
    // `native_violates` cannot drop boundary TypeErrors (`return null` under such a
    // hint). Refuse rather than rebind an uncheckable exit as a Singleton premise
    // (ADR-0075 review).
    //
    // An **enforced top** is exempt (ADR-0057 note, issue #603): `: array`, `: object`
    // and `: iterable` lower to no `NativeType` but DO seed the A2 oracle, from
    // `scope_return_top`, so the arms this refusal exists to require are there. The
    // value component is then A1's as usual — `return [1, 2]` under `: array` crosses
    // the shape it proved — and where A1 has nothing the envelope alone is the answer,
    // which it gives in the arm lane (`return_envelope_arms`), not here: `floor` stays
    // `None` for want of a single-base value floor, so a factless exit still floors the
    // value summary out (A3), exactly as it does under `: mixed`.
    //
    // `: mixed` is exempt (issue #364): it is the TOTAL envelope, so the empty
    // oracle has nothing to drop — no value violates `mixed`, and no conversion
    // happens at the boundary — and the exit that crosses is the exit the body
    // proved. It reads as NO hint here and only here: `floor` stays `None` (a total
    // envelope has no single-base value floor), so a factless exit still floors the
    // whole summary out (A3), and everything outside this function keeps treating
    // it as the written hint it is.
    if ret.is_none()
        && callee_scope
            .ret_hint
            .is_some_and(|h| !matches!(h.kind, RetHintKind::Mixed | RetHintKind::Top(_)))
    {
        return None;
    }
    let floor = ret.and_then(native_value_floor);
    // The declared return type is a CONVERSION boundary, not just an envelope
    // (the #48 family, return edition): PHP hands the caller what the boundary
    // converts, so `return 1` under `: float` crosses as `1.0`, never the callee's
    // raw int. A2 already dropped the *violating* exits at collection; this
    // converts the admitted ones, and an admitted-but-unconvertible fact degrades
    // to the declared floor (A3 — wider, never wrong).
    let coerced: Vec<ExitContribution> = exits
        .iter()
        .map(|e| match (e, ret) {
            (ExitContribution::Fact(f, s), Some(ty)) => {
                match coerce_fact_to_native(ty, f.clone()) {
                    Some(cf) => ExitContribution::Fact(cf, *s),
                    None => ExitContribution::Floor,
                }
            }
            // Under an enforced top there is no conversion, only the boundary: a
            // fact the top admits WHOLE crosses as it is; a fact it admits only in
            // part (`null|list{5}` under `: array` — a finite or nullable fact with
            // one violating member, which A2 keeps because `admits_fact` says
            // `Maybe`) may not cross, or the caller would hold a Verified member no
            // call can return. It degrades to `Floor`, and the top has no value
            // floor, so the value component declines and the arm lane answers
            // `array` (A3, wider never wrong) — the `: int` twin's degrade to the
            // native floor, one lane over.
            (ExitContribution::Fact(f, s), None) => match callee_scope.ret_hint.map(|h| h.kind) {
                Some(RetHintKind::Top(top))
                    if !enforced_top_arms(top)
                        .iter()
                        .any(|ty| steins_contract::admits_fact(ty, f) == Certainty::Yes) =>
                {
                    ExitContribution::Floor
                }
                _ => ExitContribution::Fact(f.clone(), *s),
            },
            // An object exit is a `Floor` on this side and always has been (T1's
            // `Heap` variant only names what the OTHER side reads): a value floor is
            // the widest thing the value domain can say about it, and for an object
            // return that floor is `None`, which is what ends the value summary.
            (ExitContribution::Floor | ExitContribution::Heap(_), _) => ExitContribution::Floor,
        })
        .collect();
    let (fact, stratum) = join_exits(&coerced, floor.as_ref())?;
    Some(SummaryValue { fact, stratum })
}

/// Join a callee's object-returning exits into the heap component (ADR-0057 §2.4,
/// per field in the T1 amendment's B3 table). Written beside the value join and never
/// inside it: the two components live and die independently.
///
/// `None` — no heap summary, the caller keeps the arm floor — whenever
///
/// * there are no exits at all, or
/// * **any** exit is not an allocation (§2.5): a scalar, `null`, an unresolved
///   expression, an untyped fall-through, or the declared floor an `Opaque`
///   `may_return` subtree contributes for the exits it hides. There is no heap shape
///   that truthfully covers such a path, so a partial summary would be a partial lie;
/// * the classes disagree (§2.4): a joined "one of Foo or Bar" is the `Member`-fact
///   shape, not the heap's, and the declared-return arms already carry that floor.
///
/// The declared return type is **not** consulted anywhere here (§2.6): a conflict
/// between the walk's proof and the declaration is the callee's own return-mismatch
/// finding, and claims do not edit proofs. The T0 amendment's A2 native oracle is the
/// value component's alone (T1 amendment B4).
fn join_heap_exits(exits: &[ExitContribution]) -> Option<HeapSummary> {
    let mut objs = Vec::with_capacity(exits.len());
    for e in exits {
        match e {
            ExitContribution::Heap(o) => objs.push(o.as_ref()),
            // Any non-allocation exit kills the summary (§2.5).
            ExitContribution::Fact(..) | ExitContribution::Floor => return None,
        }
    }
    let (first, rest) = objs.split_first()?;
    // Class agreement decides everything else: exactness is only meaningful under it,
    // and a prop of a `Foo` is not a prop of a `Bar`.
    if rest.iter().any(|o| o.class != first.class) {
        return None;
    }
    let mut joined = HeapObj::new(first.class.clone());
    // Copied, never promoted (§6.4 / A1): exact only where every path was.
    joined.class_exact = first.class_exact && rest.iter().all(|o| o.class_exact);
    // Escaped-before-return ORs (§2.4): a leak on any path means the caller must
    // rebind pre-escaped and sweep it like an object it leaked itself.
    joined.escaped = first.escaped || rest.iter().any(|o| o.escaped);
    // readonly INTERSECTS. The set is a function of the class, so disagreement is a
    // corner; where it happens the smaller set is the sound one, readonly being a
    // sweep-IMMUNITY claim (B3).
    joined.readonly =
        first.readonly.iter().filter(|n| rest.iter().all(|o| o.readonly.contains(*n))).cloned().collect();
    // ro_written likewise: a write proven on every path. Recording a one-path write
    // would let the caller's first assignment read as a `readonly.reassigned` second.
    joined.ro_written =
        first.ro_written.iter().filter(|n| rest.iter().all(|o| o.ro_written.contains(*n))).cloned().collect();
    // Carries survive only where every path carries them identically (B2) — the
    // `join_stores` intersection rule, order-independent (ADR-0048 §4).
    joined.targs =
        first.targs.iter().filter(|c| rest.iter().all(|o| o.targs.contains(c))).cloned().collect();
    // Props: present on EVERY object-returning path, joined by the existing
    // value-domain join at `min` stratum (ADR-0052 amendment 1 — a Verified arm joined
    // with an Asserted arm yields Asserted). An unjoinable pair drops the prop.
    for (name, p0) in &first.props {
        let mut fact = p0.fact.clone();
        let mut stratum = p0.stratum;
        let mut ok = true;
        for o in rest {
            match o.props.get(name) {
                Some(p) => match fact.join(&p.fact) {
                    Some(j) => {
                        fact = j;
                        stratum = stratum.min(p.stratum);
                    }
                    None => {
                        ok = false;
                        break;
                    }
                },
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            joined.props.insert(name.clone(), PropFact { fact, stratum });
        }
    }
    Some(HeapSummary { obj: joined })
}

/// Join a callee's returning-exit contributions into the value-domain summary fact
/// (ADR-0057 A1/A3/A4). Each `Fact` exit joins by the existing value-domain join;
/// each `Floor` exit contributes the declared value floor (a `None` floor — an object
/// or mixed-base return — kills the whole value summary, there being no representable
/// degraded top). Stratum is `min` over exits (N2). An empty exit set (no returning
/// exit, or every exit dropped by A2) or an unrepresentable join (mixed bases) yields
/// `None` — arm floor.
fn join_exits(exits: &[ExitContribution], floor: Option<&Fact>) -> Option<(Fact, Stratum)> {
    if exits.is_empty() {
        return None;
    }
    let mut acc: Option<Fact> = None;
    let mut stratum = Stratum::Verified;
    for e in exits {
        let (fact, s) = match e {
            ExitContribution::Fact(f, s) => (f.clone(), *s),
            // `Heap` never reaches here — `join_value_component` maps it to `Floor`
            // before this join runs — but it degrades the same way if it ever did.
            ExitContribution::Floor | ExitContribution::Heap(_) => {
                (floor?.clone(), Stratum::Verified)
            }
        };
        stratum = stratum.min(s);
        acc = Some(match acc {
            None => fact,
            Some(a) => a.join(&fact)?,
        });
    }
    Some((acc?, stratum))
}

/// The declared return type's value-domain FLOOR as a single [`Fact`] — the sound top
/// within the envelope a factless exit contributes (ADR-0057 A3). Representable only
/// when every native member shares ONE scalar base (`int`, `?int`); a union of bases,
/// an object, or a bool-literal return has no single-base value floor (`None`).
fn native_value_floor(ty: &NativeType) -> Option<Fact> {
    let mut base: Option<Base> = None;
    for m in &ty.members {
        let b = match m {
            TypeMember::Scalar(ScalarType::Int) => Base::Int,
            TypeMember::Scalar(ScalarType::Float) => Base::Float,
            TypeMember::Scalar(ScalarType::String) => Base::String,
            TypeMember::Scalar(ScalarType::Bool) => Base::Bool,
            _ => return None,
        };
        match base {
            None => base = Some(b),
            Some(x) if x == b => {}
            Some(_) => return None,
        }
    }
    base.map(|base| Fact::General { base, nullable: ty.nullable })
}
