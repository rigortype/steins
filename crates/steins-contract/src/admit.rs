//! Acceptance judgments: values and facts against contract types.
//!
//! Kleene composition throughout (`and`, `or`, [`Certainty::all_of`]). Sound
//! under-approximation: a union only *jointly* covering a base (e.g.
//! `int<min,0>|int<0,max>` over `int`) answers `Maybe`, never wrong.

use crate::{CField, CKey, ContractTy, MixedCut, ckey_to_domain};
use steins_domain::Key as VKey;
use steins_domain::{
    Base, Certainty, Fact, KeyClass, Presence, Refinement, ShapeFact, StrPreds, Tail, Val,
    php_is_falsy,
};

/// Is the concrete value admitted by the contract?
#[must_use]
pub fn admits_val(ty: &ContractTy, v: &Val) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match ty {
        ContractTy::Mixed => Yes,
        ContractTy::Never => No,
        ContractTy::Opaque => Maybe,
        ContractTy::Null => Certainty::from_bool(*v == Val::Null),
        // `php_is_falsy` is the engine's own falsy judgment (null included).
        ContractTy::MixedMinus(MixedCut::Null) => Certainty::from_bool(*v != Val::Null),
        ContractTy::MixedMinus(MixedCut::Falsy) => Certainty::from_bool(!php_is_falsy(v)),
        ContractTy::Base(b) => match (b, v) {
            // int is accepted where float is expected (PHPStan core).
            (Base::Float, Val::Int(_)) => Yes,
            _ => Certainty::from_bool(v.base() == Some(*b)),
        },
        ContractTy::IntIn(r) => match v {
            Val::Int(i) => Certainty::from_bool(r.contains(*i)),
            _ => No,
        },
        // Extensional predicates decide outright. `class-string` (#236) only
        // refutes via grammar (rules out `''`,`'0'`,`'123'`); it never proves
        // `Yes` (needs the class table), so `Yes` degrades to `Maybe` (ADR-0038).
        ContractTy::StrWith(p) => match v {
            Val::Str(s) => {
                if !StrPreds::of(s).contains_all(p.extensional()) {
                    No
                } else if p.is_extensional() {
                    Yes
                } else {
                    Maybe
                }
            }
            _ => No,
        },
        ContractTy::StrOpaque => match v {
            Val::Str(_) => Maybe,
            _ => No,
        },
        ContractTy::LitInt(want) => Certainty::from_bool(matches!(v, Val::Int(i) if i == want)),
        ContractTy::LitFloat(want) => match v {
            // PHP value equality: 5 satisfies 5.0 (IEEE ==, not set equality).
            #[allow(clippy::float_cmp)]
            Val::Float(f) => Certainty::from_bool(*f == *want),
            #[allow(clippy::cast_precision_loss)]
            Val::Int(i) => Certainty::from_bool(*i as f64 == *want),
            _ => No,
        },
        ContractTy::LitStr(want) => Certainty::from_bool(matches!(v, Val::Str(s) if s == want)),
        ContractTy::LitBool(want) => Certainty::from_bool(matches!(v, Val::Bool(b) if b == want)),
        ContractTy::ArrayAny { non_empty } => match v {
            Val::Array(items) => Certainty::from_bool(!(*non_empty && items.is_empty())),
            _ => No,
        },
        ContractTy::ListOf { elem, non_empty } => match v {
            Val::Array(items) => admits_list(elem, *non_empty, items),
            _ => No,
        },
        ContractTy::MapOf { key, val, non_empty, not_list } => match v {
            Val::Array(items) => {
                if *non_empty && items.is_empty() {
                    No
                } else if *not_list && is_list(items) {
                    // Phan's `associative-array`: list realizations rejected outright.
                    No
                } else {
                    admits_entries(key, val, items)
                }
            }
            _ => No,
        },
        ContractTy::IterableOf { key, val } => match v {
            Val::Array(items) => admits_entries(key, val, items),
            _ => No,
        },
        ContractTy::Shape { list, fields, sealed, non_empty, unsealed } => match v {
            Val::Array(items) => admits_shape(*list, fields, *sealed, *non_empty, unsealed, items),
            _ => No,
        },
        ContractTy::Class(_) | ContractTy::ObjectAny => No,
        // No coercion to resource, even weakly (probed 8.5.9): `No`, not the
        // old `KNOWN_UNENFORCED` floor (ADR-0056 §8).
        ContractTy::Resource => No,
        // Signature unused (only for the closure-argument variance check, #11,
        // `steins-infer`); `closure_only` (ADR-0063 P3) decides string/array `No`.
        ContractTy::CallableTy { obl, .. } => match v {
            Val::Str(_) | Val::Array(_) if !obl.closure_only => Maybe,
            _ => No,
        },
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(admits_val(m, v)))
        }
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(admits_val(m, v)))
        }
    }
}

/// Is *every* value the fact admits also admitted by the contract?
#[must_use]
pub fn admits_fact(ty: &ContractTy, fact: &Fact) -> Certainty {
    if let Some(vals) = fact.finite_members() {
        return Certainty::all_of(vals.iter().map(|v| admits_val(ty, v)));
    }
    let (base, refinement, nullable) = match fact {
        Fact::Refined { base, refinement, nullable } => (*base, Some(*refinement), *nullable),
        Fact::General { base, nullable } => (*base, None, *nullable),
        // Array stratum (ADR-0062): no scalar base, own rule table (ADR-0072).
        Fact::Shape { shape, nullable } => {
            let array_part = admits_shape_fact(ty, shape);
            return if *nullable {
                Certainty::all_of([array_part, admits_val(ty, &Val::Null)])
            } else {
                array_part
            };
        }
        // For-all over the arms (#339); `null` checked once, as elsewhere.
        Fact::Union { arms, nullable } => {
            let arm_parts = arms.iter().map(|(b, r)| base_only(ty, *b, *r));
            let all_arms = Certainty::all_of(arm_parts);
            return if *nullable {
                Certainty::all_of([all_arms, admits_val(ty, &Val::Null)])
            } else {
                all_arms
            };
        }
        Fact::Singleton(_) | Fact::OneOf(_) => unreachable!("finite handled above"),
    };
    let base_part = base_only(ty, base, refinement);
    if nullable {
        // The denotation is base-part ∪ {null}: both parts must agree.
        Certainty::all_of([base_part, admits_val(ty, &Val::Null)])
    } else {
        base_part
    }
}

/// For-all judgment over the (non-null) base part of an abstract fact. Union
/// folding needs one member covering the whole base, so jointly-covering
/// unions answer `Maybe`.
fn base_only(ty: &ContractTy, base: Base, refinement: Option<Refinement>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match ty {
        ContractTy::Mixed => Yes,
        ContractTy::Never => No,
        ContractTy::Opaque => Maybe,
        ContractTy::Null => No,
        // Base part is non-null by construction; caller judges `nullable` separately.
        ContractTy::MixedMinus(MixedCut::Null) => Yes,
        // Decided only where the refinement carries the answer; otherwise `Maybe`.
        ContractTy::MixedMinus(MixedCut::Falsy) => match (base, refinement) {
            // `non-falsy-string` = not `''`/`'0'`; its absence isn't refutation.
            (Base::String, Some(Refinement::Str(have))) => {
                if have.contains_all(StrPreds::NON_FALSY) { Yes } else { Maybe }
            }
            (Base::Int, Some(Refinement::Int(have))) => {
                if !have.contains(0) {
                    Yes
                } else if have.lo() == 0 && have.hi() == 0 {
                    No
                } else {
                    Maybe
                }
            }
            _ => Maybe,
        },
        ContractTy::Base(b) => match (b, base) {
            (b, base) if *b == base => Yes,
            (Base::Float, Base::Int) => Yes,
            _ => No,
        },
        ContractTy::IntIn(r) => match (base, refinement) {
            (Base::Int, Some(Refinement::Int(have))) => {
                if r.contains_range(have) {
                    Yes
                } else if r.intersect(have).is_none() {
                    No
                } else {
                    Maybe
                }
            }
            (Base::Int, _) => Maybe,
            _ => No,
        },
        ContractTy::StrWith(p) => match (base, refinement) {
            (Base::String, Some(Refinement::Str(have))) => {
                if have.contains_all(*p) {
                    Yes
                } else {
                    // Positive predicates over extensional bits always overlap.
                    Maybe
                }
            }
            (Base::String, _) => Maybe,
            _ => No,
        },
        ContractTy::StrOpaque => {
            if base == Base::String { Maybe } else { No }
        }
        ContractTy::LitInt(want) => match (base, refinement) {
            (Base::Int, Some(Refinement::Int(have))) => {
                if !have.contains(*want) {
                    No
                } else {
                    // A non-point interval containing the literal still holds other ints.
                    Maybe
                }
            }
            (Base::Int, _) => Maybe,
            _ => No,
        },
        ContractTy::LitFloat(_) => {
            if matches!(base, Base::Float | Base::Int) { Maybe } else { No }
        }
        ContractTy::LitStr(want) => match (base, refinement) {
            (Base::String, Some(Refinement::Str(have))) => {
                if StrPreds::of(want).contains_all(have) { Maybe } else { No }
            }
            (Base::String, _) => Maybe,
            _ => No,
        },
        ContractTy::LitBool(_) => {
            if base == Base::Bool { Maybe } else { No }
        }
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. }
        | ContractTy::Class(_)
        | ContractTy::ObjectAny
        // Scalars only; no scalar is a resource ([`admits_val`]'s disjointness).
        | ContractTy::Resource => No,
        // As in [`admits_val`]: string is a `callable`-candidate but never `Closure`.
        ContractTy::CallableTy { obl, .. } => {
            if base == Base::String && !obl.closure_only { Maybe } else { No }
        }
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(base_only(m, base, refinement)))
        }
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(base_only(m, base, refinement)))
        }
    }
}

fn key_as_val(k: &VKey) -> Val {
    match k {
        VKey::Int(i) => Val::Int(*i),
        VKey::Str(s) => Val::Str(s.clone()),
    }
}

fn is_list<V>(items: &[(VKey, V)]) -> bool {
    items.iter().enumerate().all(|(i, (k, _))| matches!(k, VKey::Int(v) if *v == i as i64))
}

fn key_eq(declared: &CKey, actual: &VKey) -> bool {
    match (declared, actual) {
        (CKey::Int(a), VKey::Int(b)) => a == b,
        (CKey::Str(a), VKey::Str(b)) => a == b,
        _ => false,
    }
}

fn admits_list(elem: &ContractTy, non_empty: bool, items: &[(VKey, Val)]) -> Certainty {
    if !is_list(items) {
        return Certainty::No;
    }
    if non_empty && items.is_empty() {
        return Certainty::No;
    }
    items
        .iter()
        .fold(Certainty::Yes, |acc, (_, v)| acc.and(admits_val(elem, v)))
}

fn admits_entries(key: &ContractTy, val: &ContractTy, items: &[(VKey, Val)]) -> Certainty {
    items.iter().fold(Certainty::Yes, |acc, (k, v)| {
        acc.and(admits_val(key, &key_as_val(k))).and(admits_val(val, v))
    })
}

/// The declared parts of one array shape, lane-independently — what
/// [`shape_verdict`] reads. `T` is the lane's declared-type: [`ContractTy`]
/// for the fact lane, the phpdoc `Type` AST for `steins-infer`'s
/// proven-value lane (one relation, two leaf judges — ADR-0062 §5).
#[derive(Debug)]
pub struct ShapeSpec<'a, T> {
    /// `list{…}` (positional) vs `array{…}` (keyed set).
    pub list: bool,
    /// Sealed shapes reject undeclared keys.
    pub sealed: bool,
    /// Reject the empty array (`non-empty-array{…}` forms).
    pub non_empty: bool,
    /// The declared fields as `(normalized key, optional, value type)`.
    pub fields: Vec<(CKey, bool, &'a T)>,
    /// The unsealed tail `...<K, V>`: the optional **key** contract and the
    /// value contract. `None` for a sealed shape *and* for an untyped `...`.
    pub tail: Option<(Option<&'a T>, &'a T)>,
}

/// Shape acceptance per #14939: `array{}` is an order-agnostic key set,
/// `list{}` a positional sequence (which must also *be* a list). Single
/// implementation of the relation (ADR-0030 no-second-relation, ADR-0062 §5);
/// `judge_val`/`judge_key` are the lane-specific leaf judges, everything
/// structural lives here only.
pub fn shape_verdict<T, V>(
    spec: &ShapeSpec<'_, T>,
    items: &[(VKey, V)],
    judge_val: &mut dyn FnMut(&T, &V) -> Certainty,
    judge_key: &mut dyn FnMut(&T, &VKey) -> Certainty,
) -> Certainty {
    use Certainty::{No, Yes};

    if spec.non_empty && items.is_empty() {
        return No;
    }
    if spec.list && !is_list(items) {
        return No;
    }

    let mut verdict = Yes;
    for (key, optional, ty) in &spec.fields {
        match items.iter().find_map(|(k, v)| key_eq(key, k).then_some(v)) {
            Some(v) => verdict = verdict.and(judge_val(ty, v)),
            None if *optional => {}
            None => return No,
        }
    }

    for (k, v) in items {
        if spec.fields.iter().any(|(key, _, _)| key_eq(key, k)) {
            continue;
        }
        match spec.tail {
            Some((key_ty, val_ty)) => {
                if let Some(kt) = key_ty {
                    verdict = verdict.and(judge_key(kt, k));
                }
                verdict = verdict.and(judge_val(val_ty, v));
            }
            None => {
                if spec.sealed {
                    return No;
                }
                // Unsealed without a declared tail type: anything goes.
            }
        }
    }

    verdict
}

/// The fact lane's driver of [`shape_verdict`]: both leaf judges are
/// [`admits_val`] (a runtime key is itself a value).
fn admits_shape(
    list: bool,
    fields: &[CField],
    sealed: bool,
    non_empty: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
    items: &[(VKey, Val)],
) -> Certainty {
    let spec = ShapeSpec {
        list,
        sealed,
        non_empty,
        fields: fields.iter().map(|f| (f.key.clone(), f.optional, &f.ty)).collect(),
        tail: unsealed.as_ref().map(|(k, v)| (k.as_deref(), &**v)),
    };
    shape_verdict(
        &spec,
        items,
        &mut |ty, v| admits_val(ty, v),
        &mut |ty, k| admits_val(ty, &key_as_val(k)),
    )
}

// ---------------------------------------------------------------------------
// The relation's third face: contract vs abstract *array* fact (ADR-0072),
// the for-all over everything a [`ShapeFact`] admits (`shape_verdict` above is
// type-vs-value; `normalize::subsumes_array` type-vs-type). `Yes` = subset,
// `No` = disjoint, `Maybe` = neither proven, same as the scalar arms.
//
// Deviates from ADR-0072 §3's table, which reads a single escaping witness as
// disjointness though it only proves ¬`Yes`; those rows answer `Maybe` here
// instead — firing `No` would wrongly report `array $a` against
// `@param non-empty-array`, the stop-the-line FP class §4.5 names. Each
// obligation below is itself a for-all, so `and` still composes them exactly.
// ---------------------------------------------------------------------------

/// Does the shape fact admit `[]`? **Lemma 1 of ADR-0072 §2.** Delegates to
/// [`ShapeFact::admits`] (exact both ways) — sharper than the ADR's prose,
/// which omits `is_list == No` and non-empty `covers` as further ways `[]`
/// is excluded (only makes "admits `[]`" rarer).
fn admits_empty(sf: &ShapeFact) -> bool {
    sf.admits(&[])
}

/// Is every member of the denotation the empty array?
/// [`ShapeFact::can_be_non_empty`] over-approximates permissively, so
/// `false` proves no non-empty array is admitted — the gate `No` needs.
fn only_empty(sf: &ShapeFact) -> bool {
    !sf.can_be_non_empty()
}

/// Is the shape fact's denotation provably empty? ADR-0072 §3 assumes it
/// always denotes something; not invariant (`normalize(vec![], Sealed, _,
/// non_empty=true, vec![])` admits nothing) — declines to decide there, per
/// ADR-0071's `denotes_nothing` guard.
fn denotes_no_array(sf: &ShapeFact) -> bool {
    !admits_empty(sf) && only_empty(sf)
}

/// The `covers_ne` column of ADR-0072 §3. `non-empty-*` and
/// [`MixedCut::Falsy`] ([`php_is_falsy`]) both reject only `[]`, so lemma 1
/// decides it: `[]` absent → `Yes`; denotation is `{[]}` → `No`; straddles →
/// `Maybe`. Deviates from §3's `ArrayAny{ne}`/`MixedMinus(Falsy)` rows ("else
/// No"): a fact admitting `[]` *and* non-empty arrays is `Maybe` here.
fn ne_gate(sf: &ShapeFact) -> Certainty {
    if !admits_empty(sf) {
        Certainty::Yes
    } else if only_empty(sf) {
        Certainty::No
    } else {
        Certainty::Maybe
    }
}

/// A field or tail value slot against a value contract. `None` is the
/// domain's "no fact" floor (A-G1a): realizes as *any* value, so it proves
/// only against `mixed`. **FP-killer invariant**: never manufactures `No`.
fn slot_verdict(ty: &ContractTy, slot: &Option<Box<Fact>>) -> Certainty {
    match slot {
        Some(f) => admits_fact(ty, f),
        None if matches!(ty, ContractTy::Mixed) => Certainty::Yes,
        None => Certainty::Maybe,
    }
}

/// Does the key contract cover every key a [`KeyClass`] can supply? Same
/// for-all one stratum down: `ArrayKey` needs both int and string coverage
/// ([`Certainty::all_of`]) — covering one half only answers `Maybe`.
fn key_class_verdict(key_ty: &ContractTy, class: KeyClass) -> Certainty {
    let of_base = |b: Base| admits_fact(key_ty, &Fact::General { base: b, nullable: false });
    match class {
        KeyClass::Int => of_base(Base::Int),
        KeyClass::Str => of_base(Base::String),
        KeyClass::ArrayKey => Certainty::all_of([of_base(Base::Int), of_base(Base::String)]),
    }
}

/// Demote an obligation only the *realized* members carry: [`Presence::Optional`]
/// fields/tails can prove `Yes` (carriers satisfy it) but never `No`
/// (non-carriers aren't refuted). `Required` entries need no demotion.
fn conditional(c: Certainty) -> Certainty {
    if c.is_no() { Certainty::Maybe } else { c }
}

/// A **required** contract field against an entry the fact doesn't guarantee:
/// `No` iff the value obligation is `No`, else `Maybe`, never `Yes` (the
/// key-less member always escapes). ADR-0072 §3's "`Optional` → No" proves
/// only ¬`Yes` from that one witness.
fn required_vs_may_have(value: Certainty) -> Certainty {
    if value.is_no() { Certainty::No } else { Certainty::Maybe }
}

/// Is the fact's tail forced on *every* member? Non-empty + all declared
/// fields `Absent` ⇒ the tail governs every entry, so its obligations need no
/// [`conditional`] demotion — how `non-empty-list<string>` refutes
/// `@param list<int>`.
fn tail_is_forced(sf: &ShapeFact) -> bool {
    sf.non_empty && sf.fields.iter().all(|(_, p, _)| matches!(p, Presence::Absent))
}

/// Is *every* array the shape fact admits also admitted by the contract?
/// ADR-0072 §3's rule table, dispatched on the contract arm. `covers`
/// (disjunctive presence, A-G8) is deliberately unused to discharge
/// obligations — only widens toward `Maybe`.
fn admits_shape_fact(ty: &ContractTy, sf: &ShapeFact) -> Certainty {
    use Certainty::{Maybe, No, Yes};

    // Vacuity guard: nothing is provable about an empty denotation.
    if denotes_no_array(sf) {
        return Maybe;
    }

    match ty {
        // `mixed` covers everything; the null cut removes no array.
        ContractTy::Mixed | ContractTy::MixedMinus(MixedCut::Null) => Yes,
        ContractTy::Opaque => Maybe,
        ContractTy::MixedMinus(MixedCut::Falsy) => ne_gate(sf),
        // `never` admits nothing, and the fact is already proven nonempty above.
        ContractTy::Never => No,
        // Array-incapable arms: none admits an array, so the two are disjoint.
        ContractTy::Null
        | ContractTy::Base(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::StrOpaque
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_)
        | ContractTy::Class(_)
        | ContractTy::ObjectAny
        // An array is never a resource.
        | ContractTy::Resource => No,
        // Pair-array may be `callable`; `*-closure` (ADR-0063 P3) never is.
        // ADR-0072 §5 refuses the pair-array-vs-signature refinement outright.
        ContractTy::CallableTy { obl, .. } => {
            if obl.closure_only { No } else { Maybe }
        }
        ContractTy::ArrayAny { non_empty } => {
            if *non_empty { ne_gate(sf) } else { Yes }
        }
        ContractTy::ListOf { elem, non_empty } => list_of_fact(sf, elem, *non_empty),
        ContractTy::MapOf { key, val, non_empty, not_list } => {
            map_of_fact(sf, key, val, *non_empty, *not_list)
        }
        // `iterable<K, V>` = `array<K, V>` without the non-emptiness/list-ness cuts.
        ContractTy::IterableOf { key, val } => map_of_fact(sf, key, val, false, false),
        ContractTy::Shape { list, fields, sealed, non_empty, unsealed } => {
            shape_vs_fact(sf, *list, fields, *sealed, *non_empty, unsealed)
        }
        // NO haircut (ADR-0072 as-built amendment): the or-fold is exact for
        // disjointness, member-wise, unlike ADR-0071 §2's coverage haircut.
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(admits_shape_fact(m, sf)))
        }
        // `A ∩ B` admits a member iff both do — sound in both directions.
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(admits_shape_fact(m, sf)))
        }
    }
}

/// `list<T>` / `non-empty-list<T>` against a shape fact. `is_list` is read as
/// the denotational trinary (lemma 2, RFC #14939), never recomputed from the
/// key set (ADR-0062 A-G lesson). Types values only.
fn list_of_fact(sf: &ShapeFact, elem: &ContractTy, non_empty: bool) -> Certainty {
    let mut verdict = if non_empty { ne_gate(sf) } else { Certainty::Yes };
    verdict = verdict.and(sf.is_list);
    for (_, presence, slot) in &sf.fields {
        if matches!(presence, Presence::Absent) {
            continue;
        }
        let value = slot_verdict(elem, slot);
        verdict = verdict.and(if presence.is_required() { value } else { conditional(value) });
    }
    if let Tail::Unsealed { value, .. } = &sf.tail {
        let tail_value = slot_verdict(elem, value);
        verdict =
            verdict.and(if tail_is_forced(sf) { tail_value } else { conditional(tail_value) });
    }
    verdict
}

/// `array<K, V>` / `non-empty-array<K, V>` / `associative-array<K, V>` /
/// `iterable<K, V>` against a shape fact. `not_list` mirrors `list<T>`:
/// `is_list == Yes` rejects every member, `No` discharges it ([`Certainty::not`]).
fn map_of_fact(
    sf: &ShapeFact,
    key: &ContractTy,
    val: &ContractTy,
    non_empty: bool,
    not_list: bool,
) -> Certainty {
    let mut verdict = if non_empty { ne_gate(sf) } else { Certainty::Yes };
    if not_list {
        verdict = verdict.and(sf.is_list.not());
    }
    for (k, presence, slot) in &sf.fields {
        if matches!(presence, Presence::Absent) {
            continue;
        }
        let entry = admits_val(key, &key_as_val(k)).and(slot_verdict(val, slot));
        verdict = verdict.and(if presence.is_required() { entry } else { conditional(entry) });
    }
    if let Tail::Unsealed { key: class, value } = &sf.tail {
        let entry = key_class_verdict(key, *class).and(slot_verdict(val, value));
        verdict = verdict.and(if tail_is_forced(sf) { entry } else { conditional(entry) });
    }
    verdict
}

/// A declared `array{…}` / `list{…}` contract against a shape fact —
/// ADR-0072 §3's structural heart: (1) every contract field satisfied by
/// every member ([`contract_field_vs_fact`]); (2) every undeclared fact
/// entry lands in the tail, or the contract is unsealed; (3) the fact's own
/// tail lands in the contract's extra surface.
fn shape_vs_fact(
    sf: &ShapeFact,
    list: bool,
    fields: &[CField],
    sealed: bool,
    non_empty: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
) -> Certainty {
    let mut verdict = if non_empty { ne_gate(sf) } else { Certainty::Yes };
    if list {
        verdict = verdict.and(sf.is_list);
    }

    for f in fields {
        verdict = verdict.and(contract_field_vs_fact(sf, f));
    }

    for (k, presence, slot) in &sf.fields {
        if matches!(presence, Presence::Absent) {
            continue;
        }
        // Declared key: family 1's business.
        if fields.iter().any(|f| key_eq(&f.key, k)) {
            continue;
        }
        let entry = extra_key_vs_contract(sealed, unsealed, k, slot);
        verdict = verdict.and(if presence.is_required() { entry } else { conditional(entry) });
    }

    if let Tail::Unsealed { key: class, value } = &sf.tail {
        let entry = fact_tail_vs_contract(sealed, unsealed, *class, value);
        // `tail_is_forced` isn't enough here: the *contract* may declare that
        // key. Unconditional only when it declares no field at all.
        let unconditional = tail_is_forced(sf) && fields.is_empty();
        verdict = verdict.and(if unconditional { entry } else { conditional(entry) });
    }
    verdict
}

/// One declared contract field against the fact's knowledge of that key.
fn contract_field_vs_fact(sf: &ShapeFact, f: &CField) -> Certainty {
    let key = ckey_to_domain(&f.key);
    match sf.field(&key) {
        // Present in every member: the value obligation may refute directly.
        Some((_, Presence::Required { .. }, slot)) => slot_verdict(&f.ty, slot),
        // With/without the key are both in the denotation: optional
        // constrains only the ones with it ([`required_vs_may_have`]).
        Some((_, Presence::Optional, slot)) => {
            let value = slot_verdict(&f.ty, slot);
            if f.optional { conditional(value) } else { required_vs_may_have(value) }
        }
        // Proven absent (post-`unset`): required rejects all, optional passes all.
        Some((_, Presence::Absent, _)) => Certainty::from_bool(f.optional),
        None => match &sf.tail {
            // Sealed: same witness as proven absence.
            Tail::Sealed => Certainty::from_bool(f.optional),
            Tail::Unsealed { key: class, value } => {
                if class.admits_key(&key) {
                    // The tail says *may*, not *must*.
                    let tail_value = slot_verdict(&f.ty, value);
                    if f.optional {
                        conditional(tail_value)
                    } else {
                        required_vs_may_have(tail_value)
                    }
                } else {
                    // Key class excludes this key outright: proven absence again.
                    Certainty::from_bool(f.optional)
                }
            }
        },
    }
}

/// A fact entry whose key the contract's fields do not declare. Same
/// three-way structure [`shape_verdict`] uses: typed tail judges key and
/// value, untyped `...` admits anything, sealed contract rejects outright —
/// refuting every member when `Required` ([`conditional`] handles `Optional`).
fn extra_key_vs_contract(
    sealed: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
    k: &VKey,
    slot: &Option<Box<Fact>>,
) -> Certainty {
    match unsealed {
        Some((key_ty, val_ty)) => {
            let key_ok =
                key_ty.as_deref().map_or(Certainty::Yes, |t| admits_val(t, &key_as_val(k)));
            key_ok.and(slot_verdict(val_ty, slot))
        }
        None if sealed => Certainty::No,
        None => Certainty::Yes,
    }
}

/// The fact's unsealed tail against the contract's extra surface.
/// Conservative: the fact's tail can cover keys the *contract* declares as
/// fields too, so this demands slightly more than necessary for `Yes` (safe
/// side); its `No` is demoted by the caller unless [`tail_is_forced`].
fn fact_tail_vs_contract(
    sealed: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
    class: KeyClass,
    value: &Option<Box<Fact>>,
) -> Certainty {
    match unsealed {
        Some((key_ty, val_ty)) => {
            let key_ok = key_ty.as_deref().map_or(Certainty::Yes, |t| key_class_verdict(t, class));
            key_ok.and(slot_verdict(val_ty, value))
        }
        None if sealed => Certainty::No,
        None => Certainty::Yes,
    }
}

/// ADR-0072 — the shape-fact face of the relation, one test per rule row.
#[cfg(test)]
mod shape_fact_tests {
    use super::*;
    use crate::lower_str;
    use steins_domain::{IntRange, Key};

    fn ty(src: &str) -> ContractTy {
        lower_str(src).unwrap_or_else(|| panic!("`{src}` must lower"))
    }

    fn fact(sf: ShapeFact) -> Fact {
        Fact::Shape { shape: Box::new(sf), nullable: false }
    }

    fn judge(src: &str, sf: &ShapeFact) -> Certainty {
        admits_fact(&ty(src), &fact(sf.clone()))
    }

    fn req(k: Key, v: Option<Fact>) -> (Key, Presence, Option<Box<Fact>>) {
        (k, Presence::Required { witnessed: true }, v.map(Box::new))
    }

    fn opt(k: Key, v: Option<Fact>) -> (Key, Presence, Option<Box<Fact>>) {
        (k, Presence::Optional, v.map(Box::new))
    }

    fn absent(k: Key) -> (Key, Presence, Option<Box<Fact>>) {
        (k, Presence::Absent, None)
    }

    fn ikey(n: i64) -> Key {
        Key::Int(n)
    }

    fn skey(s: &str) -> Key {
        Key::Str(s.into())
    }

    fn int_fact() -> Fact {
        Fact::General { base: Base::Int, nullable: false }
    }

    fn str_fact() -> Fact {
        Fact::General { base: Base::String, nullable: false }
    }

    fn open(key: KeyClass, value: Option<Fact>) -> Tail {
        Tail::Unsealed { key, value: value.map(Box::new) }
    }

    /// `array{0: int, 1: int}`-flavored fact: a sealed two-entry int list.
    fn int_pair() -> ShapeFact {
        ShapeFact::normalize(
            vec![req(ikey(0), Some(int_fact())), req(ikey(1), Some(int_fact()))],
            Tail::Sealed,
            Certainty::Yes,
            true,
            Vec::new(),
        )
    }

    /// `array{'a': int}`-flavored fact: one required string key.
    fn keyed_int() -> ShapeFact {
        ShapeFact::normalize(
            vec![req(skey("a"), Some(int_fact()))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        )
    }

    #[test]
    fn lemma_one_is_exact_both_ways() {
        assert!(admits_empty(&ShapeFact::plain_array()));
        // A required field forces an entry.
        assert!(!admits_empty(&keyed_int()));
        let flagged = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::ArrayKey, None),
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        // The flag alone forces one.
        assert!(!admits_empty(&flagged));
        // Sharper than the ADR: `[]` IS a list, so `is_list == No` excludes it.
        let not_a_list = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Str, None),
            Certainty::No,
            false,
            Vec::new(),
        );
        assert!(!admits_empty(&not_a_list));
    }

    #[test]
    fn mixed_and_the_null_cut_cover_every_array() {
        assert_eq!(judge("mixed", &ShapeFact::plain_array()), Certainty::Yes);
        assert_eq!(judge("non-null-mixed", &ShapeFact::plain_array()), Certainty::Yes);
        assert_eq!(judge("mixed", &int_pair()), Certainty::Yes);
    }

    #[test]
    fn opaque_is_the_floor() {
        assert_eq!(judge("int-mask<1, 2>", &int_pair()), Certainty::Maybe);
    }

    /// See [`ne_gate`]'s deviation note (ADR reads `No` where this is `Maybe`).
    #[test]
    fn falsy_cut_is_decided_by_lemma_one() {
        assert_eq!(judge("non-empty-mixed", &int_pair()), Certainty::Yes);
        assert_eq!(judge("non-empty-mixed", &ShapeFact::plain_array()), Certainty::Maybe);
        // Sealed, fieldless: `can_be_non_empty` provably false, `{[]}` only.
        let only_empty_shape =
            ShapeFact::normalize(Vec::new(), Tail::Sealed, Certainty::Yes, false, Vec::new());
        assert_eq!(judge("non-empty-mixed", &only_empty_shape), Certainty::No);
    }

    #[test]
    fn never_refutes_every_shape_fact() {
        assert_eq!(judge("never", &int_pair()), Certainty::No);
        assert_eq!(judge("never", &ShapeFact::plain_array()), Certainty::No);
    }

    /// The row that turns an array literal against `@param string` into a finding.
    #[test]
    fn array_incapable_arms_refute_on_every_member() {
        for src in [
            "string",
            "int",
            "float",
            "bool",
            "null",
            "positive-int",
            "numeric-string",
            "class-string",
            "'lit'",
            "5",
            "true",
            "SomeClass",
            "object",
        ] {
            assert_eq!(judge(src, &int_pair()), Certainty::No, "{src} admits no array");
            assert_eq!(
                judge(src, &ShapeFact::plain_array()),
                Certainty::No,
                "{src} admits no array"
            );
        }
    }

    /// `*-closure` demands a `Closure` instance (ADR-0063 P3); no array is one.
    #[test]
    fn callable_is_open_unless_closure_only() {
        assert_eq!(judge("callable", &int_pair()), Certainty::Maybe);
        assert_eq!(judge("pure-callable", &int_pair()), Certainty::Maybe);
        assert_eq!(judge("Closure", &int_pair()), Certainty::Maybe);
        assert_eq!(judge("pure-closure", &int_pair()), Certainty::No);
        assert_eq!(judge("static-closure", &int_pair()), Certainty::No);
    }

    #[test]
    fn array_any_covers_everything_and_the_ne_form_reads_lemma_one() {
        assert_eq!(judge("array", &ShapeFact::plain_array()), Certainty::Yes);
        assert_eq!(judge("array", &int_pair()), Certainty::Yes);
        assert_eq!(judge("non-empty-array", &int_pair()), Certainty::Yes);
        assert_eq!(judge("non-empty-array", &ShapeFact::plain_array()), Certainty::Maybe);
    }

    #[test]
    fn list_of_reads_the_is_list_trinary_as_given() {
        assert_eq!(judge("list<int>", &int_pair()), Certainty::Yes);
        // `No`: no member is a list.
        assert_eq!(judge("list<int>", &keyed_int()), Certainty::No);
        let unknown_listness = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::ArrayKey, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("list<int>", &unknown_listness), Certainty::Maybe);
    }

    #[test]
    fn list_of_refutes_on_a_required_slot_whose_values_it_rejects() {
        let str_list = ShapeFact::normalize(
            vec![req(ikey(0), Some(str_fact()))],
            Tail::Sealed,
            Certainty::Yes,
            true,
            Vec::new(),
        );
        assert_eq!(judge("list<int>", &str_list), Certainty::No);
        assert_eq!(judge("list<string>", &str_list), Certainty::Yes);
    }

    /// `non-empty-list<string>`: forced tail, see [`tail_is_forced`].
    #[test]
    fn a_forced_tail_refutes_where_a_conditional_one_could_not() {
        let ne_str_list = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Int, Some(str_fact())),
            Certainty::Yes,
            true,
            Vec::new(),
        );
        assert_eq!(judge("list<int>", &ne_str_list), Certainty::No);
        assert_eq!(judge("list<string>", &ne_str_list), Certainty::Yes);
        // Without non-emptiness, nothing refutes.
        let maybe_empty = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Int, Some(str_fact())),
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(judge("list<int>", &maybe_empty), Certainty::Maybe);
    }

    #[test]
    fn non_empty_list_needs_the_ne_gate() {
        assert_eq!(judge("non-empty-list<int>", &int_pair()), Certainty::Yes);
        let maybe_empty_int_list = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Int, Some(int_fact())),
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(judge("list<int>", &maybe_empty_int_list), Certainty::Yes);
        assert_eq!(judge("non-empty-list<int>", &maybe_empty_int_list), Certainty::Maybe);
    }

    /// List fact vs `@param array<string, int>`: the key 0/1 refutes it.
    #[test]
    fn map_key_contract_refutes_through_a_required_key() {
        assert_eq!(judge("array<string, int>", &int_pair()), Certainty::No);
        assert_eq!(judge("array<int, int>", &int_pair()), Certainty::Yes);
        assert_eq!(judge("array<string, int>", &keyed_int()), Certainty::Yes);
    }

    #[test]
    fn map_value_contract_refutes_through_a_required_slot() {
        assert_eq!(judge("array<string, string>", &keyed_int()), Certainty::No);
        assert_eq!(judge("array<array-key, int>", &keyed_int()), Certainty::Yes);
    }

    /// `ArrayKey` needs both int and string coverage; never `No`.
    #[test]
    fn map_tail_key_class_coverage() {
        let open_int_vals = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Int, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("array<int, int>", &open_int_vals), Certainty::Yes);
        assert_eq!(judge("array<array-key, int>", &open_int_vals), Certainty::Yes);
        assert_eq!(judge("array<string, int>", &open_int_vals), Certainty::Maybe);
        let open_any = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::ArrayKey, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("array<int, int>", &open_any), Certainty::Maybe);
        assert_eq!(judge("array<array-key, int>", &open_any), Certainty::Yes);
    }

    /// Phan's `associative-array`: rejects list realizations (`not_list` mirror).
    #[test]
    fn associative_array_is_the_not_list_mirror() {
        assert_eq!(judge("associative-array<array-key, int>", &int_pair()), Certainty::No);
        assert_eq!(judge("associative-array<array-key, int>", &keyed_int()), Certainty::Yes);
        let unknown = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::ArrayKey, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("associative-array<array-key, int>", &unknown), Certainty::Maybe);
    }

    /// `iterable<K, V>` = `array<K, V>` without the cuts: covers all when K, V do.
    #[test]
    fn iterable_covers_a_matching_array_fact() {
        assert_eq!(judge("iterable<int, int>", &int_pair()), Certainty::Yes);
        assert_eq!(judge("iterable<string, int>", &keyed_int()), Certainty::Yes);
        assert_eq!(judge("iterable<string, int>", &int_pair()), Certainty::No);
    }

    /// ADR-0072 §3's two witness families: each alone proves ¬`Yes` only.
    #[test]
    fn optional_fact_field_vs_required_contract_field() {
        let maybe_a = ShapeFact::normalize(
            vec![opt(skey("a"), Some(int_fact()))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        // Witness 1 only.
        assert_eq!(judge("array{a: int}", &maybe_a), Certainty::Maybe);
        // Both witnesses land.
        assert_eq!(judge("array{a: string}", &maybe_a), Certainty::No);
        assert_eq!(judge("array{a?: int}", &maybe_a), Certainty::Yes);
    }

    /// Witness 2 alone (sealed contract, undeclared key) is not disjoint either.
    #[test]
    fn optional_fact_field_vs_sealed_contract_is_not_disjoint() {
        let maybe_b = ShapeFact::normalize(
            vec![req(skey("a"), Some(int_fact())), opt(skey("b"), Some(int_fact()))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &maybe_b), Certainty::Maybe);
        let always_b = ShapeFact::normalize(
            vec![req(skey("a"), Some(int_fact())), req(skey("b"), Some(int_fact()))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &always_b), Certainty::No);
        assert_eq!(judge("array{a: int, ...}", &always_b), Certainty::Yes);
        assert_eq!(judge("array{a: int, ...<string, int>}", &always_b), Certainty::Yes);
        assert_eq!(judge("array{a: int, ...<int, int>}", &always_b), Certainty::No);
    }

    /// Proven-absent key and sealed fact tail are the same witness.
    #[test]
    fn required_contract_field_vs_proven_absence() {
        let a_absent = ShapeFact::normalize(
            vec![req(skey("x"), Some(int_fact())), absent(skey("a"))],
            open(KeyClass::Str, Some(int_fact())),
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &a_absent), Certainty::No);
        assert_eq!(judge("array{a?: int, ...}", &a_absent), Certainty::Yes);
        // Sealed, undeclared.
        assert_eq!(judge("array{q: int}", &keyed_int()), Certainty::No);
        let int_keys_only = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Int, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &int_keys_only), Certainty::No);
    }

    /// Unsealed fact tail says *may*, not *must*: neither pole reachable.
    #[test]
    fn unsealed_fact_tail_leaves_a_required_contract_field_open() {
        let open_str = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Str, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &open_str), Certainty::Maybe);
        // Sealed: still just *may*.
        assert_eq!(judge("array{}", &open_str), Certainty::Maybe);
    }

    #[test]
    fn contract_shape_typed_tail_covers_the_fact_tail() {
        let str_keyed_ints = ShapeFact::normalize(
            vec![req(skey("a"), Some(int_fact()))],
            open(KeyClass::Str, Some(int_fact())),
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int, ...<string, int>}", &str_keyed_ints), Certainty::Yes);
        assert_eq!(judge("array{a: int, ...}", &str_keyed_ints), Certainty::Yes);
        assert_eq!(judge("array{a: int, ...<string, string>}", &str_keyed_ints), Certainty::Maybe);
    }

    #[test]
    fn contract_list_shape_demands_the_is_list_trinary() {
        assert_eq!(judge("list{int, int}", &int_pair()), Certainty::Yes);
        assert_eq!(judge("list{int}", &keyed_int()), Certainty::No);
    }

    /// Only a fieldless sealed contract refutes a forced fact tail.
    #[test]
    fn a_forced_fact_tail_refutes_only_a_fieldless_sealed_contract() {
        let ne_any = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::ArrayKey, None),
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &ne_any), Certainty::Maybe);
        assert_eq!(judge("array{a?: int}", &ne_any), Certainty::Maybe);
        // Fieldless: every entry is out.
        assert_eq!(judge("array{}", &ne_any), Certainty::No);
    }

    #[test]
    fn contract_shape_non_emptiness_uses_the_same_gate() {
        let empty_ok = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::ArrayKey, None),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("non-empty-array{a?: int, ...}", &empty_ok), Certainty::Maybe);
    }

    /// **Inverted-hazard pin.** A `No` here could only come from an unknown slot.
    fn unknown_slots() -> ShapeFact {
        ShapeFact::normalize(
            vec![req(skey("a"), None), opt(skey("b"), None)],
            open(KeyClass::Str, None),
            Certainty::Maybe,
            true,
            Vec::new(),
        )
    }

    #[test]
    fn a_none_value_slot_never_manufactures_a_refutation() {
        let sf = unknown_slots();
        for src in [
            "array",
            "non-empty-array",
            "array<string, int>",
            "array<string, string>",
            "array<array-key, mixed>",
            "associative-array<string, int>",
            "iterable<string, int>",
            "iterable",
            "array{a: int, ...<string, string>}",
            "array{a: string, ...}",
            "array{a?: int, ...}",
            "non-empty-array{a: int, ...}",
            "mixed",
            "non-empty-mixed",
        ] {
            assert_ne!(
                judge(src, &sf),
                Certainty::No,
                "{src} must not refute a fact whose slots are unknown"
            );
        }
    }

    /// Teeth on the pin above: a *known*, rejected fact does refute.
    #[test]
    fn the_same_shape_with_a_known_slot_does_refute() {
        let known = ShapeFact::normalize(
            vec![req(skey("a"), Some(str_fact())), opt(skey("b"), None)],
            open(KeyClass::Str, None),
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array<string, int>", &known), Certainty::No);
        assert_eq!(judge("array{a: int, ...<string, string>}", &known), Certainty::No);
        assert_eq!(judge("array{a: int, ...<string, string>}", &unknown_slots()), Certainty::Maybe);
    }

    /// From the other end: plain `array` knows nothing, refutes nothing.
    #[test]
    fn the_degenerate_shape_refutes_no_array_contract() {
        for src in [
            "array",
            "non-empty-array",
            "list<int>",
            "non-empty-list<string>",
            "array<string, int>",
            "associative-array<array-key, int>",
            "iterable<int, SomeClass>",
            "array{a: int}",
            "array{a?: int, ...}",
            "list{int, string}",
            "array{}",
        ] {
            assert_ne!(
                judge(src, &ShapeFact::plain_array()),
                Certainty::No,
                "plain `array` knows nothing and must refute {src} not at all"
            );
        }
    }

    /// `mixed` is the one contract a `None` slot can prove `Yes` against.
    #[test]
    fn a_none_slot_proves_yes_against_mixed_only() {
        let unknown = ShapeFact::normalize(
            vec![req(skey("a"), None)],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{a: mixed}", &unknown), Certainty::Yes);
        assert_eq!(judge("array{a: int}", &unknown), Certainty::Maybe);
    }

    /// No haircut needed (ADR-0072 as-built amendment): each member is `Maybe`.
    #[test]
    fn a_jointly_covering_union_stays_undecided_without_a_haircut() {
        let ne_ints = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Int, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_ne!(judge("list<int>|non-empty-array", &ne_ints), Certainty::No);
        // Covers outright.
        assert_eq!(judge("string|array", &ne_ints), Certainty::Yes);
    }

    /// All-scalar union: any array witnesses disjointness for the whole union.
    #[test]
    fn all_scalar_union_refutes_genuinely() {
        assert_eq!(judge("string|int", &int_pair()), Certainty::No);
        assert_eq!(judge("string|int|null", &ShapeFact::plain_array()), Certainty::No);
        assert_eq!(judge("SomeClass|pure-closure", &int_pair()), Certainty::No);
        // Array-capable member also genuinely refutes: no haircut cost here.
        assert_eq!(judge("string|list<int>", &keyed_int()), Certainty::No);
    }

    #[test]
    fn intersection_is_an_and_fold() {
        assert_eq!(judge("array&iterable<int, int>", &int_pair()), Certainty::Yes);
    }

    /// Denotes shape members ∪ `{null}`; same split the scalar arms use.
    #[test]
    fn nullable_shape_fact_splits_like_every_other_fact() {
        let nullable = Fact::Shape { shape: Box::new(int_pair()), nullable: true };
        assert_eq!(admits_fact(&ty("array"), &nullable), Certainty::Maybe);
        assert_eq!(admits_fact(&ty("array|null"), &nullable), Certainty::Yes);
        assert_eq!(admits_fact(&ty("string"), &nullable), Certainty::No);
        assert_eq!(admits_fact(&ty("?string"), &nullable), Certainty::Maybe);
    }

    /// Provably-empty denotation decides nothing ([`Certainty::all_of`]'s stance).
    #[test]
    fn an_uninhabited_shape_decides_nothing() {
        let nothing =
            ShapeFact::normalize(Vec::new(), Tail::Sealed, Certainty::Maybe, true, Vec::new());
        assert!(denotes_no_array(&nothing));
        assert_eq!(judge("string", &nothing), Certainty::Maybe);
        assert_eq!(judge("never", &nothing), Certainty::Maybe);
    }

    const ARRAY_SPELLINGS: [&str; 18] = [
        "array",
        "non-empty-array",
        "list<int>",
        "list<string>",
        "non-empty-list<int>",
        "array<int, int>",
        "array<string, int>",
        "array<array-key, int>",
        "array<string, string>",
        "associative-array<string, int>",
        "array{a: int}",
        "array{a?: int}",
        "array{a: int, b: string}",
        "array{a: int, ...}",
        "array{a: int, ...<string, int>}",
        "array{}",
        "list{int}",
        "list{int, string}",
    ];

    /// **FP oracle.** ADR-0071's `subsumes` is type-vs-type; this is
    /// type-vs-fact: where `b ⊇ a` is proven, `b` must never refute `a`'s fact.
    #[test]
    fn a_proven_subsumption_is_never_refuted_by_the_fact_face() {
        for a_src in ARRAY_SPELLINGS {
            let a = ty(a_src);
            let Some(sf) = crate::to_shape_fact(&a) else { continue };
            for b_src in ARRAY_SPELLINGS {
                let b = ty(b_src);
                if crate::normalize::subsumes(&b, &a) != Certainty::Yes {
                    continue;
                }
                assert_ne!(
                    admits_fact(&b, &fact(sf.clone())),
                    Certainty::No,
                    "`{b_src}` provably subsumes `{a_src}`, so it cannot refute its fact"
                );
            }
        }
    }

    /// Same oracle, `b` drawn from outside the array world.
    #[test]
    fn a_proven_subsumption_from_outside_the_array_world_is_not_refuted() {
        for a_src in ARRAY_SPELLINGS {
            let a = ty(a_src);
            let Some(sf) = crate::to_shape_fact(&a) else { continue };
            for b_src in ["mixed", "iterable", "iterable<array-key, mixed>", "array|null", "?array"]
            {
                let b = ty(b_src);
                if crate::normalize::subsumes(&b, &a) != Certainty::Yes {
                    continue;
                }
                assert_ne!(
                    admits_fact(&b, &fact(sf.clone())),
                    Certainty::No,
                    "`{b_src}` provably subsumes `{a_src}`, so it cannot refute its fact"
                );
            }
        }
    }

    fn witness_pool() -> Vec<Vec<(Key, Val)>> {
        let i = |n: i64| Val::Int(n);
        let s = |t: &str| Val::Str(t.into());
        vec![
            vec![],
            vec![(ikey(0), i(1))],
            vec![(ikey(0), i(1)), (ikey(1), i(2))],
            vec![(ikey(0), s("x"))],
            vec![(ikey(0), i(1)), (ikey(1), s("x"))],
            vec![(ikey(1), i(1))],
            vec![(skey("a"), i(1))],
            vec![(skey("a"), s("x"))],
            vec![(skey("a"), i(1)), (skey("b"), s("x"))],
            vec![(skey("a"), i(1)), (skey("b"), i(2))],
            vec![(skey("b"), i(1))],
            vec![(skey("a"), i(1)), (ikey(0), i(2))],
        ]
    }

    /// **Definitional oracle.** `Yes`/`No` must hold for every witness the
    /// fact admits, checked against [`admits_val`] directly.
    #[test]
    fn every_verdict_agrees_with_the_values_the_fact_admits() {
        let pool = witness_pool();
        let contracts: Vec<&str> = ARRAY_SPELLINGS
            .iter()
            .copied()
            .chain(["mixed", "non-empty-mixed", "string", "iterable", "iterable<int, int>"])
            .collect();
        // Plus hand-built facts reaching shapes the lowering can't spell.
        let mut facts: Vec<(String, ShapeFact)> = ARRAY_SPELLINGS
            .iter()
            .filter_map(|src| crate::to_shape_fact(&ty(src)).map(|sf| ((*src).to_owned(), sf)))
            .collect();
        facts.push(("absent-a".to_owned(), {
            ShapeFact::normalize(
                vec![req(skey("b"), Some(int_fact())), absent(skey("a"))],
                open(KeyClass::Str, Some(int_fact())),
                Certainty::Maybe,
                true,
                Vec::new(),
            )
        }));
        facts.push(("unknown-slots".to_owned(), unknown_slots()));
        facts.push(("forced-str-tail".to_owned(), {
            ShapeFact::normalize(
                Vec::new(),
                open(KeyClass::Int, Some(str_fact())),
                Certainty::Yes,
                true,
                Vec::new(),
            )
        }));
        facts.push(("optional-only".to_owned(), {
            ShapeFact::normalize(
                vec![opt(skey("a"), Some(int_fact()))],
                Tail::Sealed,
                Certainty::Maybe,
                false,
                Vec::new(),
            )
        }));
        for (a_src, sf) in &facts {
            let members: Vec<&Vec<(Key, Val)>> = pool.iter().filter(|w| sf.admits(w)).collect();
            if members.is_empty() {
                continue;
            }
            for b_src in &contracts {
                let b = ty(b_src);
                let verdict = admits_fact(&b, &fact(sf.clone()));
                for w in &members {
                    let concrete = admits_val(&b, &Val::Array((*w).clone()));
                    if verdict.is_no() {
                        assert!(
                            !concrete.is_yes(),
                            "`{b_src}` answered No for the fact of `{a_src}` yet admits a member of it: {w:?}"
                        );
                    }
                    if verdict.is_yes() {
                        assert!(
                            !concrete.is_no(),
                            "`{b_src}` answered Yes for the fact of `{a_src}` yet rejects a member of it: {w:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn nested_shape_slots_recurse_through_admits_fact() {
        let inner = ShapeFact::normalize(
            vec![req(ikey(0), Some(str_fact()))],
            Tail::Sealed,
            Certainty::Yes,
            true,
            Vec::new(),
        );
        let outer = ShapeFact::normalize(
            vec![req(skey("rows"), Some(fact(inner)))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{rows: list<string>}", &outer), Certainty::Yes);
        assert_eq!(judge("array{rows: list<int>}", &outer), Certainty::No);
        assert_eq!(judge("array{rows: string}", &outer), Certainty::No);
    }

    #[test]
    fn refined_slots_recurse_too() {
        let positive = ShapeFact::normalize(
            vec![req(
                skey("n"),
                Some(Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false)),
            )],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{n: positive-int}", &positive), Certainty::Yes);
        assert_eq!(judge("array{n: int}", &positive), Certainty::Yes);
        assert_eq!(judge("array{n: int<min, 0>}", &positive), Certainty::No);
        assert_eq!(judge("array{n: string}", &positive), Certainty::No);
    }
}
