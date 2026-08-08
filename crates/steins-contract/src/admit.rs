//! Acceptance judgments: values and facts against contract types.
//!
//! Everything is Kleene composition: conjunction (`and`) for "all conditions
//! hold", disjunction (`or`) across union members, [`Certainty::all_of`] for
//! "every possible value". The abstract-fact path uses a documented sound
//! under-approximation: a union that only *jointly* covers a base (e.g.
//! `int<min,0>|int<0,max>` over general `int`) answers `Maybe`, never a
//! wrong verdict.

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
        // Exact against a concrete value: the cut is a value predicate, and
        // `php_is_falsy` is the engine's own answer (null included).
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
        // The extensional predicates decide the question outright. A CONTEXTUAL
        // one (`class-string`, issue #236) is decidable only in the refuting
        // direction from the string itself: PHP's identifier grammar rules out
        // `''`, `'0'` and `'123'`, and nothing here can rule anything *in* —
        // whether `'App\User'` names a declared class is the class table's
        // answer, not the characters'. So `No` stays proven and `Yes` degrades
        // to `Maybe`, which is exactly the floor ADR-0038 pinned for the
        // spelling before it had a predicate.
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
                    // Phan's `associative-array`: a list realization is rejected
                    // outright, key/value membership never consulted.
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
        // The signature (if any) is not consulted here: a runtime string/array
        // value cannot be judged against a call shape, so acceptance is the same
        // as for a bare callable — a string may name a function, a pair-array a
        // method (Maybe), any other scalar No. The signature is used only by the
        // closure-argument variance check (issue #11, `steins-infer`).
        // `closure_only` (the `*-closure` spellings, ADR-0063 P3) decides the string
        // and array cases outright: a callable-string names a function and a pair-
        // array a method, and neither is ever a `Closure` **instance**. That is the
        // half of `pure-closure` that needs no purity analysis at all.
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
        // The array stratum (ADR-0062 `Fact::Shape`) has no scalar base: it is
        // judged by its own rule table (ADR-0072), which shares this arm's
        // nullable split — the denotation is the shape's members ∪ {null}, and
        // both halves must agree.
        Fact::Shape { shape, nullable } => {
            let array_part = admits_shape_fact(ty, shape);
            return if *nullable {
                Certainty::all_of([array_part, admits_val(ty, &Val::Null)])
            } else {
                array_part
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

/// For-all judgment over the (non-null) base part of an abstract fact.
///
/// Union folding is a sound under-approximation: `Yes` requires a single
/// member covering the whole base part, so jointly-covering unions answer
/// `Maybe`.
fn base_only(ty: &ContractTy, base: Base, refinement: Option<Refinement>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match ty {
        ContractTy::Mixed => Yes,
        ContractTy::Never => No,
        ContractTy::Opaque => Maybe,
        ContractTy::Null => No,
        // The base part of a fact is non-null by construction, so the null cut
        // is already satisfied here; the fact's own `nullable` half is judged by
        // the caller through `admits_val(ty, Val::Null)`, which answers `No`.
        ContractTy::MixedMinus(MixedCut::Null) => Yes,
        // The falsy cut decides only where the refinement already carries the
        // answer. Everything else is `Maybe` — the honest floor for a base part
        // that holds both falsy and truthy values.
        ContractTy::MixedMinus(MixedCut::Falsy) => match (base, refinement) {
            // `non-falsy-string` is exactly "not `''`, not `'0'`" — the whole
            // string half of the cut. Its absence is *not* a refutation (a
            // `non-empty-string` fact still holds truthy members), so `Maybe`.
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
                    // Positive predicate sets over the extensional bits always
                    // overlap ("5" satisfies every one of them), so refuting is
                    // impossible here — and a contextual bit cannot refute
                    // either, since it is never proven absent, only unrecorded.
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
                    // A non-full interval containing the literal still holds
                    // other ints — unless it is the point interval, which the
                    // finite layers own; stay honest.
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
        | ContractTy::ObjectAny => No,
        // As in [`admits_val`]: a definitely-string fact is a callable-string
        // candidate (`Maybe`) for the `callable` spellings, but never a `Closure`
        // instance, so `closure_only` decides it `No`.
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

/// The declared parts of one array shape, lane-independently — exactly what the
/// structural acceptance relation ([`shape_verdict`]) reads.
///
/// `T` is the lane's declared-type representation: [`ContractTy`] for the fact
/// lane, the phpdoc `Type` AST for `steins-infer`'s proven-value lane (ADR-0062
/// §5: **one** acceptance relation, two leaf judges).
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
/// `list{}` a positional sequence (which must also *be* a list).
///
/// This is the single implementation of the relation (ADR-0030's
/// no-second-relation discipline, ADR-0062 §5). Everything lane-specific is a
/// parameter: `judge_val` decides a declared value type against one of the
/// lane's values, `judge_key` decides the unsealed tail's key type against a
/// runtime key. The structural rules — required/optional presence, sealing, the
/// tail key *and* value obligations, list-ness, non-emptiness — live here and
/// nowhere else.
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

    // Extra entries: keys not declared by any field.
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
// The relation's third face: a contract against an abstract *array* fact
// (ADR-0072). `shape_verdict` above is type-vs-value, `normalize::subsumes_array`
// is type-vs-type, and this is type-vs-abstract-fact — the for-all judgment over
// everything a [`ShapeFact`] admits.
//
// **What `No` means here, and why it is not what ADR-0072 §3's table reads.**
// `admits_fact`'s three answers are `Yes` ⇒ every member of the fact's
// denotation is admitted (subset), `No` ⇒ *no* member is (disjoint), `Maybe` ⇒
// neither is proven. That is the documented contract at every consumer ("only a
// definite `No` — every value the fact admits is rejected — reports") and it is
// what the scalar arms above already implement: `admits_fact(LitInt(1),
// int<0,5>)` is `Maybe`, not `No`, although `0` is a member the literal rejects.
//
// ADR-0072 §2 states the `No` gate as "a member of the fact's denotation the
// contract provably rejects". That is a *necessary* condition, and §3's table
// then reads several rows as if it were sufficient ("else No (`[]` witness)",
// "a fact field at `Optional` → No"). Under this relation a single escaping
// witness proves only ¬`Yes`. Every such row is implemented at the sound
// verdict — `Maybe` — and each one says so at its site. Firing `No` there would
// report `array $a` passed to `@param non-empty-array`, which is the FP class
// ADR-0072 §4.5 calls a stop-the-line defect, and this face's consumers turn
// `No` straight into a `phpdoc.*` finding.
//
// The composition rule that makes the rest of the table work: each obligation
// below is itself a for-all over the same denotation (`Yes` = every member
// satisfies it, `No` = no member does), so Kleene `and` composes them exactly —
// one obligation no member can satisfy rejects every member, which *is* `No`.
// ---------------------------------------------------------------------------

/// Does the shape fact admit `[]`? **Lemma 1 of ADR-0072 §2.**
///
/// Taken from the domain's own extensional membership test rather than
/// restated: [`ShapeFact::admits`] *is* the concretization, so this is exact in
/// both directions — which is what lets a `Yes` below rest on it.
///
/// It is strictly sharper than the ADR's prose ("no `Required` field and
/// `non_empty` false"), which omits the two other ways a shape excludes the
/// empty array: `is_list == No` (`[]` **is** a list, so a shape none of whose
/// members is a list has no `[]` member) and a non-empty `covers` (a cover
/// demands some key be present). Both sharpenings move the same way — they make
/// "admits `[]`" *rarer*, so the `[]`-shaped rules refute less often, never
/// more. Reading `covers` only here is consistent with §3's refusal: the ADR
/// declines to *discharge obligations* with covers, which is the direction that
/// could widen toward a wrong pole.
fn admits_empty(sf: &ShapeFact) -> bool {
    sf.admits(&[])
}

/// Is every member of the denotation the empty array?
///
/// [`ShapeFact::can_be_non_empty`] is over-approximate on the permissive side,
/// so a `false` is a *proof* that no non-empty array is admitted — the gate a
/// `No` needs before it may rest on the `[]` cut alone.
fn only_empty(sf: &ShapeFact) -> bool {
    !sf.can_be_non_empty()
}

/// Is the shape fact's denotation provably empty?
///
/// ADR-0072 §3 asserts a shape fact always denotes something. [`ShapeFact::normalize`]
/// makes that nearly true (a `Sealed` tail strips `Absent` fields), but it is
/// not an invariant: `normalize(vec![], Sealed, _, non_empty = true, vec![])`
/// admits no array at all. Every verdict is vacuous on an empty denotation, so
/// this declines to decide — the stance [`Certainty::all_of`] already takes for
/// an empty iterator, and the guard ADR-0071 spells `denotes_nothing`.
fn denotes_no_array(sf: &ShapeFact) -> bool {
    !admits_empty(sf) && only_empty(sf)
}

/// The `covers_ne` column of ADR-0072 §3, in this relation's idiom.
///
/// A `non-empty-*` contract — and [`MixedCut::Falsy`], whose cut removes exactly
/// `[]` from the array world ([`php_is_falsy`]) — rejects one member and one
/// only, so lemma 1 decides it outright:
///
/// * `[]` is not in the denotation → the cut removes nothing → `Yes`;
/// * the denotation is `{[]}` → the cut removes everything → `No`;
/// * otherwise the denotation straddles the cut → `Maybe`.
///
/// **Deviation from ADR-0072 §3, deliberate** (see the module note above): the
/// table's `ArrayAny{ne}` and `MixedMinus(Falsy)` rows read "else No". Here an
/// `[]`-admitting fact that also admits non-empty arrays is `Maybe` — its
/// non-empty members are admitted, so the denotations are not disjoint.
fn ne_gate(sf: &ShapeFact) -> Certainty {
    if !admits_empty(sf) {
        Certainty::Yes
    } else if only_empty(sf) {
        Certainty::No
    } else {
        Certainty::Maybe
    }
}

/// A field or tail value slot against a value contract.
///
/// A `None` slot is the domain's "no fact" floor (A-G1a): it realizes as *any*
/// value, so it can neither refute — some value satisfies any inhabited
/// contract — nor prove, except against `mixed`, which admits every value there
/// is (the one sharpening ADR-0072 §3 names). **This is the FP-killer
/// invariant**: an unknown slot must never manufacture a refutation, so the
/// `None` arm may not reach `No` by any path.
fn slot_verdict(ty: &ContractTy, slot: &Option<Box<Fact>>) -> Certainty {
    match slot {
        Some(f) => admits_fact(ty, f),
        None if matches!(ty, ContractTy::Mixed) => Certainty::Yes,
        None => Certainty::Maybe,
    }
}

/// Does the key contract cover every key a [`KeyClass`] can supply?
///
/// A tail's key class says what an *undeclared* key may be, so the query is the
/// same for-all judgment one stratum down: `Int` asks whether the contract
/// admits every int, `Str` every string, `ArrayKey` both — folded with
/// [`Certainty::all_of`], so a contract covering one half only answers `Maybe`.
fn key_class_verdict(key_ty: &ContractTy, class: KeyClass) -> Certainty {
    let of_base = |b: Base| admits_fact(key_ty, &Fact::General { base: b, nullable: false });
    match class {
        KeyClass::Int => of_base(Base::Int),
        KeyClass::Str => of_base(Base::String),
        KeyClass::ArrayKey => Certainty::all_of([of_base(Base::Int), of_base(Base::String)]),
    }
}

/// Demote an obligation only the *realized* members carry.
///
/// A [`Presence::Optional`] field and an unsealed tail both describe entries
/// some members have and others do not. Such an obligation can still prove
/// `Yes` — every member that carries the entry satisfies it, and the ones
/// without are unconstrained by it — but never `No`, because the members
/// lacking the entry are not refuted by it. [`Presence::Required`] entries need
/// no demotion: every member carries them, which is what makes a required
/// field's key or value obligation able to refute the whole denotation.
fn conditional(c: Certainty) -> Certainty {
    if c.is_no() { Certainty::Maybe } else { c }
}

/// A **required** contract field against an entry the fact does not guarantee
/// (an `Optional` fact field, or a key only the fact's tail may supply).
///
/// Both witness families are in the denotation and `No` needs both to refute:
///
/// * the member **without** the key violates the required field outright;
/// * the member **with** the key violates only when the value obligation does.
///
/// So `No` exactly when the value obligation is `No`, and `Maybe` otherwise —
/// never `Yes`, since the key-less member always escapes. ADR-0072 §3 reads
/// "a fact field at `Optional` → No" on the first witness alone; that proves
/// ¬`Yes`, not disjointness, so the unconditional `No` is not taken here.
fn required_vs_may_have(value: Certainty) -> Certainty {
    if value.is_no() { Certainty::No } else { Certainty::Maybe }
}

/// Is the fact's tail forced on *every* member?
///
/// The witness: the shape is non-empty, so each member has at least one entry;
/// every declared field is `Absent`, so no declared key can supply it; the entry
/// is therefore undeclared and the tail governs it. When this holds the tail's
/// obligations need no [`conditional`] demotion and may refute — which is how a
/// `non-empty-list<string>` fact refutes `@param list<int>`.
fn tail_is_forced(sf: &ShapeFact) -> bool {
    sf.non_empty && sf.fields.iter().all(|(_, p, _)| matches!(p, Presence::Absent))
}

/// Is *every* array the shape fact admits also admitted by the contract?
///
/// The ADR-0072 §3 rule table, dispatched on the contract arm. `covers`
/// (disjunctive presence, A-G8) is deliberately not consulted to discharge
/// obligations (§3, §5) — ignoring it only widens toward `Maybe`.
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
        // The falsy cut removes exactly `[]` from the array world.
        ContractTy::MixedMinus(MixedCut::Falsy) => ne_gate(sf),
        // `never` admits nothing, and the vacuity guard above already proved
        // this denotation nonempty — so every member is rejected.
        ContractTy::Never => No,
        // Array-incapable arms: the denotation holds arrays only, and none of
        // these admits an array, so the two are disjoint.
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
        | ContractTy::ObjectAny => No,
        // A `callable` *value* may be a two-element method array, so the
        // question stays open; a `*-closure` spelling (ADR-0063 P3) demands a
        // `Closure` instance, which no array ever is. ADR-0072 §5 refuses the
        // pair-array-vs-signature refinement outright.
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
        // `iterable<K, V>` is `array<K, V>` without the non-emptiness and
        // list-ness cuts: the fact denotes arrays only, every one of which
        // `iterable` covers when K and V do.
        ContractTy::IterableOf { key, val } => map_of_fact(sf, key, val, false, false),
        ContractTy::Shape { list, fields, sealed, non_empty, unsealed } => {
            shape_vs_fact(sf, *list, fields, *sealed, *non_empty, unsealed)
        }
        // NO haircut here, by ratified decision (ADR-0072 as-built amendment):
        // under this relation's disjointness reading the or-fold is exact —
        // a union rejects a value iff every member does, so "every member
        // disjoint from the fact" IS "the union disjoint from the fact",
        // member-wise. ADR-0071 §2's haircut exists because *coverage* is not
        // member-wise; disjointness is. The jointly-covering case the haircut
        // protected (`list<int>|non-empty-array` over an []-admitting fact)
        // needs no protection: the `non-empty-array` member already answers
        // the honest `Maybe` from its own row, and `Maybe` survives the fold.
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(admits_shape_fact(m, sf)))
        }
        // `A ∩ B` admits a member iff both do: `and` is sound in both
        // directions here, as it is for the scalar arms.
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(admits_shape_fact(m, sf)))
        }
    }
}

/// `list<T>` / `non-empty-list<T>` against a shape fact.
///
/// `is_list` is consumed as the denotational trinary it is (lemma 2, RFC #14939)
/// and never recomputed from the key set — the ADR-0062 A-G lesson. `No` there
/// means no member is a list while the contract admits lists only, which is
/// disjointness; `Yes` discharges the list obligation; `Maybe` leaves it open
/// and the `and` below can then only reach `Maybe` or a `No` some *other*
/// obligation proved.
///
/// `list<T>` types values and not keys, so only the value slots are read.
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
/// `iterable<K, V>` against a shape fact.
///
/// A declared field's key is a literal, so it goes through [`admits_val`]; the
/// tail's key is a class, so it goes through [`key_class_verdict`]. Phan's
/// `not_list` is the mirror of the `list<T>` gate: the contract rejects list
/// realizations, so `is_list == Yes` makes every member rejected and
/// `is_list == No` discharges the obligation — exactly [`Certainty::not`].
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

/// A declared `array{…}` / `list{…}` contract against a shape fact — ADR-0072
/// §3's structural heart.
///
/// Three obligation families, `and`-composed with the list-ness and
/// non-emptiness gates:
///
/// 1. every **contract field** must be satisfied by every member
///    ([`contract_field_vs_fact`]);
/// 2. every **fact entry** the contract does not declare must land in the
///    contract's tail, or the contract must be unsealed;
/// 3. the **fact's own tail** must land in the contract's extra surface.
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
        // A key the contract declares is obligation family 1's business.
        if fields.iter().any(|f| key_eq(&f.key, k)) {
            continue;
        }
        let entry = extra_key_vs_contract(sealed, unsealed, k, slot);
        verdict = verdict.and(if presence.is_required() { entry } else { conditional(entry) });
    }

    if let Tail::Unsealed { key: class, value } = &sf.tail {
        let entry = fact_tail_vs_contract(sealed, unsealed, *class, value);
        // [`tail_is_forced`] proves every member carries an entry the **fact**
        // does not declare — which is not enough here, because the *contract*
        // may declare that very key: a `non-empty-array` fact's forced entry can
        // be the `a` of `array{a: int}`, and that member is admitted. The
        // obligation is unconditional only when the contract declares no field
        // for the entry to land in.
        let unconditional = tail_is_forced(sf) && fields.is_empty();
        verdict = verdict.and(if unconditional { entry } else { conditional(entry) });
    }
    verdict
}

/// One declared contract field against the fact's knowledge of that key.
fn contract_field_vs_fact(sf: &ShapeFact, f: &CField) -> Certainty {
    let key = ckey_to_domain(&f.key);
    match sf.field(&key) {
        // Present in every member: the value obligation is the whole story, and
        // it may refute (a required `array{a: int}` field against a fact whose
        // `a` is a string rejects every member).
        Some((_, Presence::Required { .. }, slot)) => slot_verdict(&f.ty, slot),
        // The subtle row. Members both with and without the key are in the
        // denotation, so an *optional* contract field only constrains the ones
        // with it, and a *required* one is refuted only when both witnesses land
        // (see [`required_vs_may_have`]).
        Some((_, Presence::Optional, slot)) => {
            let value = slot_verdict(&f.ty, slot);
            if f.optional { conditional(value) } else { required_vs_may_have(value) }
        }
        // Proven absent (post-`unset`, the false branch of `isset`): no member
        // carries the key, so a required contract field rejects every one of
        // them and an optional field is satisfied by all.
        Some((_, Presence::Absent, _)) => Certainty::from_bool(f.optional),
        None => match &sf.tail {
            // Sealed: no member carries an undeclared key — same verdict as
            // proven absence, on the same witness.
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
                    // The tail's key class excludes this key outright, so no
                    // member can carry it: proven absence again.
                    Certainty::from_bool(f.optional)
                }
            }
        },
    }
}

/// A fact entry whose key the contract's fields do not declare.
///
/// The same three-way structure [`shape_verdict`] uses on the value lane: the
/// contract's typed tail judges key and value, an untyped `...` admits
/// anything, and a sealed contract rejects the entry outright — which refutes
/// every member when the entry is `Required` (the caller's [`conditional`]
/// demotion handles the `Optional` case).
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
        // Unsealed without a declared tail type: anything goes.
        None => Certainty::Yes,
    }
}

/// The fact's unsealed tail against the contract's extra surface.
///
/// Deliberately conservative in one place: the fact's tail governs keys the
/// *fact* does not declare, which can include keys the *contract* declares as
/// fields. Judging the whole tail against the contract's tail therefore demands
/// slightly more than necessary for `Yes` — the safe side — and its `No` is
/// demoted by the caller unless [`tail_is_forced`].
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

/// ADR-0072 — the shape-fact face of the acceptance relation, one vector per
/// rule row, each naming the witness (for a `No`) or the coverage argument (for
/// a `Yes`) that licenses it.
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
        Key::Str(s.to_owned())
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

    // ---- lemma 1: the `[]` membership test ---------------------------------

    #[test]
    fn lemma_one_is_exact_both_ways() {
        // No required field and not flagged non-empty → `[]` is a member.
        assert!(admits_empty(&ShapeFact::plain_array()));
        // A required field forces an entry.
        assert!(!admits_empty(&keyed_int()));
        // The flag alone forces one.
        let flagged = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::ArrayKey, None),
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert!(!admits_empty(&flagged));
        // Sharper than the ADR's prose: `[]` IS a list, so `is_list == No`
        // excludes it too.
        let not_a_list = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Str, None),
            Certainty::No,
            false,
            Vec::new(),
        );
        assert!(!admits_empty(&not_a_list));
    }

    // ---- `mixed`, the cuts, `never`, the scalar arms -----------------------

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

    /// The falsy cut removes exactly `[]`, so lemma 1 decides it — `Yes` when no
    /// member is `[]`, `Maybe` when the denotation straddles the cut (the ADR's
    /// table reads `No` there; see [`ne_gate`]'s deviation note).
    #[test]
    fn falsy_cut_is_decided_by_lemma_one() {
        assert_eq!(judge("non-empty-mixed", &int_pair()), Certainty::Yes);
        assert_eq!(judge("non-empty-mixed", &ShapeFact::plain_array()), Certainty::Maybe);
        // The one denotation the cut removes entirely: `{[]}` — sealed, no
        // fields, so `can_be_non_empty` is provably false.
        let only_empty_shape =
            ShapeFact::normalize(Vec::new(), Tail::Sealed, Certainty::Yes, false, Vec::new());
        assert_eq!(judge("non-empty-mixed", &only_empty_shape), Certainty::No);
    }

    /// `never` admits nothing while the fact's denotation holds an array — the
    /// nonemptiness the vacuity guard establishes first.
    #[test]
    fn never_refutes_every_shape_fact() {
        assert_eq!(judge("never", &int_pair()), Certainty::No);
        assert_eq!(judge("never", &ShapeFact::plain_array()), Certainty::No);
    }

    /// The witness is any member at all: an array is not a scalar, an object or
    /// `null`, so the denotations are disjoint. This is the row that turns an
    /// array literal passed to `@param string` into a finding.
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

    /// A pair-array may be a `[$obj, 'method']` callable, so a bare `callable`
    /// stays open; a `*-closure` spelling demands a `Closure` **instance**,
    /// which no array is (ADR-0063 P3). Bare `Closure` carries the default
    /// obligation, so it is open too — the lowering's own call.
    #[test]
    fn callable_is_open_unless_closure_only() {
        assert_eq!(judge("callable", &int_pair()), Certainty::Maybe);
        assert_eq!(judge("pure-callable", &int_pair()), Certainty::Maybe);
        assert_eq!(judge("Closure", &int_pair()), Certainty::Maybe);
        assert_eq!(judge("pure-closure", &int_pair()), Certainty::No);
        assert_eq!(judge("static-closure", &int_pair()), Certainty::No);
    }

    // ---- `ArrayAny` --------------------------------------------------------

    #[test]
    fn array_any_covers_everything_and_the_ne_form_reads_lemma_one() {
        assert_eq!(judge("array", &ShapeFact::plain_array()), Certainty::Yes);
        assert_eq!(judge("array", &int_pair()), Certainty::Yes);
        // Coverage: no member is `[]`, so the non-emptiness cut removes nothing.
        assert_eq!(judge("non-empty-array", &int_pair()), Certainty::Yes);
        // Straddles the cut — the members that are not `[]` are admitted.
        assert_eq!(judge("non-empty-array", &ShapeFact::plain_array()), Certainty::Maybe);
    }

    // ---- `ListOf`: the three `is_list` cases -------------------------------

    #[test]
    fn list_of_reads_the_is_list_trinary_as_given() {
        // `Yes` + every value slot ⊆ int → coverage proven.
        assert_eq!(judge("list<int>", &int_pair()), Certainty::Yes);
        // `No` — the witness is that no member is a list at all, and `list<T>`
        // admits lists only. `['a' => 1]` carries exactly this fact.
        assert_eq!(judge("list<int>", &keyed_int()), Certainty::No);
        // `Maybe` — nothing proven either way, and the value slots agree.
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
        // Witness: every member carries key 0 with a string, which `list<int>`
        // rejects — so every member is rejected.
        assert_eq!(judge("list<int>", &str_list), Certainty::No);
        assert_eq!(judge("list<string>", &str_list), Certainty::Yes);
    }

    /// A `non-empty-list<string>` fact: the tail is forced on every member (the
    /// shape is non-empty and declares no field), so its value obligation may
    /// refute — the [`tail_is_forced`] row.
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
        // The same tail without the non-emptiness flag: a member may carry no
        // undeclared entry at all, so nothing is refuted.
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

    // ---- `MapOf` / `IterableOf` -------------------------------------------

    /// A required field's key literal is in *every* member, so a key contract
    /// that rejects it rejects the whole denotation — this is what refutes a
    /// list fact against `@param array<string, int>`.
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

    /// The tail's key *class* is judged by coverage of the class's whole key
    /// world: `ArrayKey` needs both halves, so a `string` key contract answers
    /// `Maybe` — and never `No`, because a member need not carry an undeclared
    /// key at all.
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

    /// Phan's `associative-array` rejects list realizations, so `is_list == Yes`
    /// rejects every member and `is_list == No` discharges the obligation.
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

    /// `iterable<K, V>` is `array<K, V>` without the cuts: the fact denotes
    /// arrays only, all of which `iterable` covers when K and V do.
    #[test]
    fn iterable_covers_a_matching_array_fact() {
        assert_eq!(judge("iterable<int, int>", &int_pair()), Certainty::Yes);
        assert_eq!(judge("iterable<string, int>", &keyed_int()), Certainty::Yes);
        assert_eq!(judge("iterable<string, int>", &int_pair()), Certainty::No);
    }

    // ---- contract `Shape` vs fact shape ------------------------------------

    /// The two witness families ADR-0072 §3 names, both realized. A fact field
    /// at `Optional` puts members with AND without the key in the denotation: a
    /// required contract field is violated by the member *without* it, and a
    /// sealed contract is violated by the member *with* it. Each proves ¬`Yes`;
    /// neither alone proves disjointness, which is what `No` means here.
    #[test]
    fn optional_fact_field_vs_required_contract_field() {
        let maybe_a = ShapeFact::normalize(
            vec![opt(skey("a"), Some(int_fact()))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        // Witness 1 (the member without 'a') refutes `Yes` only.
        assert_eq!(judge("array{a: int}", &maybe_a), Certainty::Maybe);
        // Both witnesses land: the member without 'a' misses the required field,
        // and the member with it carries an int where a string is declared. Now
        // every member is rejected.
        assert_eq!(judge("array{a: string}", &maybe_a), Certainty::No);
        // The optional contract field is satisfied by every member.
        assert_eq!(judge("array{a?: int}", &maybe_a), Certainty::Yes);
    }

    /// Witness 2 alone, on a *sealed* contract: the member carrying the
    /// undeclared key is rejected, the member without it is not.
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
        // A *required* extra key is a witness every member carries → disjoint.
        let always_b = ShapeFact::normalize(
            vec![req(skey("a"), Some(int_fact())), req(skey("b"), Some(int_fact()))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &always_b), Certainty::No);
        // Unsealing the contract, or typing its tail, covers the extra key.
        assert_eq!(judge("array{a: int, ...}", &always_b), Certainty::Yes);
        assert_eq!(judge("array{a: int, ...<string, int>}", &always_b), Certainty::Yes);
        assert_eq!(judge("array{a: int, ...<int, int>}", &always_b), Certainty::No);
    }

    /// A proven-absent key, and a sealed fact tail, are the same witness: no
    /// member carries the key, so a required contract field rejects them all.
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
        // Sealed and undeclared: same witness.
        assert_eq!(judge("array{q: int}", &keyed_int()), Certainty::No);
        // A tail whose key class cannot supply the key is the witness too.
        let int_keys_only = ShapeFact::normalize(
            Vec::new(),
            open(KeyClass::Int, Some(int_fact())),
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(judge("array{a: int}", &int_keys_only), Certainty::No);
    }

    /// An unsealed *fact* tail against a required contract field: the tail says
    /// *may*, not *must*, so neither pole is reachable.
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
        // …and against a sealed contract the fact's tail is likewise only a
        // *may*, so the sealing refutes nothing.
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
        // The tail's value type is not covered — but a member may carry no
        // undeclared entry, so this is `Maybe`, not `No`.
        assert_eq!(judge("array{a: int, ...<string, string>}", &str_keyed_ints), Certainty::Maybe);
    }

    #[test]
    fn contract_list_shape_demands_the_is_list_trinary() {
        assert_eq!(judge("list{int, int}", &int_pair()), Certainty::Yes);
        assert_eq!(judge("list{int}", &keyed_int()), Certainty::No);
    }

    /// A `non-empty-array` fact against a sealed contract shape. Every member
    /// carries an entry the *fact* does not declare — but the *contract* may
    /// declare its key, and `['a' => 1]` is then admitted, so the sealing
    /// refutes nothing. Only a contract with no fields at all turns the forced
    /// entry into a refutation.
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
        // `array{}` declares nothing and seals: every non-empty array is out.
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

    // ---- `None` value slots: the FP-killer pins ----------------------------

    /// **The inverted-hazard pin.** A `None` value slot realizes as any value,
    /// so it can neither prove nor refute. Every contract below agrees with this
    /// fact on the parts the fact *does* know — its string keys, its non-empty
    /// flag, its `is_list == No` — so a `No` could only have been manufactured
    /// by an unknown slot, and none is.
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

    /// The teeth on the pin above: fill the same slot with a *known* fact the
    /// contract rejects and the very same contracts refute. The silence is the
    /// unknown slot's doing, not a dead code path.
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

    /// The same pin from the other end: the *degenerate* shape (plain `array`)
    /// knows nothing at all and must refute no array contract whatsoever.
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

    /// A `mixed` value contract is the one sharpening a `None` slot may prove:
    /// it admits every value there is.
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

    // ---- unions and intersections -----------------------------------------

    /// A jointly-covering union needs NO haircut under the disjointness
    /// reading (ADR-0072 as-built amendment): the `non-empty-array` member
    /// answers `Maybe` from its own row (the fact admits `[]` alongside
    /// non-empty members), and `Maybe` survives the or-fold. The fold's `No`
    /// is member-wise exact for disjointness, unlike ADR-0071's coverage.
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
        // The haircut is a floor, not a ceiling: a member that covers outright
        // still answers `Yes`.
        assert_eq!(judge("string|array", &ne_ints), Certainty::Yes);
    }

    /// An all-scalar union is the case the haircut lets through: every member
    /// admits no array at all, so every array in the denotation is a witness the
    /// whole union shares.
    #[test]
    fn all_scalar_union_refutes_genuinely() {
        assert_eq!(judge("string|int", &int_pair()), Certainty::No);
        assert_eq!(judge("string|int|null", &ShapeFact::plain_array()), Certainty::No);
        assert_eq!(judge("SomeClass|pure-closure", &int_pair()), Certainty::No);
        // One array-capable member that ALSO genuinely refutes (the fact is
        // definitely keyed, so `list<int>` is disjoint too): under the
        // disjointness reading the fold's `No` is exact and STANDS — the
        // ADR-0072 as-built amendment's ruling that this relation takes no
        // ADR-0071 haircut. This is the true positive the haircut would cost.
        assert_eq!(judge("string|list<int>", &keyed_int()), Certainty::No);
    }

    #[test]
    fn intersection_is_an_and_fold() {
        assert_eq!(judge("array&iterable<int, int>", &int_pair()), Certainty::Yes);
    }

    // ---- the nullable half -------------------------------------------------

    /// A nullable shape fact denotes the shape's members ∪ `{null}`, so both
    /// halves must agree — exactly the split the scalar arms use.
    #[test]
    fn nullable_shape_fact_splits_like_every_other_fact() {
        let nullable = Fact::Shape { shape: Box::new(int_pair()), nullable: true };
        assert_eq!(admits_fact(&ty("array"), &nullable), Certainty::Maybe);
        assert_eq!(admits_fact(&ty("array|null"), &nullable), Certainty::Yes);
        assert_eq!(admits_fact(&ty("string"), &nullable), Certainty::No);
        assert_eq!(admits_fact(&ty("?string"), &nullable), Certainty::Maybe);
    }

    // ---- vacuity -----------------------------------------------------------

    /// A shape whose denotation is provably empty decides nothing — the stance
    /// [`Certainty::all_of`] already takes for an empty iterator.
    #[test]
    fn an_uninhabited_shape_decides_nothing() {
        let nothing =
            ShapeFact::normalize(Vec::new(), Tail::Sealed, Certainty::Maybe, true, Vec::new());
        assert!(denotes_no_array(&nothing));
        assert_eq!(judge("string", &nothing), Certainty::Maybe);
        assert_eq!(judge("never", &nothing), Certainty::Maybe);
    }

    // ---- the cross-relation FP oracle --------------------------------------

    /// The array vocabulary, as the lowering sees it.
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

    /// **The FP oracle, run against the other face of the relation.** ADR-0071's
    /// `subsumes` is type-vs-type; this one is type-vs-fact. Where `b ⊇ a` is
    /// *proven*, `b` cannot be disjoint from anything inside `a` — so judging
    /// `a`'s own fact form against `b` must never answer `No`.
    ///
    /// The argument survives the lowering's widening: `to_shape_fact` may drop a
    /// slot it cannot spell (`to_fact` returns `None` for `float`, classes,
    /// intersections), which makes the fact *wider* than `a`. A `No` over a
    /// wider denotation implies `No` over the narrower one, so the property is
    /// if anything harder to satisfy, not easier.
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

    /// The same oracle with the scalar and `mixed` arms in the `b` position:
    /// anything that subsumes an array spelling must not refute its fact.
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

    /// Small concrete arrays to probe a denotation with.
    fn witness_pool() -> Vec<Vec<(Key, Val)>> {
        let i = |n: i64| Val::Int(n);
        let s = |t: &str| Val::Str(t.to_owned());
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

    /// **The definitional oracle.** `Yes` must mean every member is admitted and
    /// `No` must mean none is — so every witness the fact admits is checked
    /// against [`admits_val`], which is the extensional judge. This is the pin
    /// that would catch a rule whose verdict outruns its witness argument: the
    /// `non-empty-array`-fact-vs-`array{a: int}` wrong `No` was exactly such a
    /// rule, and this test refutes it directly.
    #[test]
    fn every_verdict_agrees_with_the_values_the_fact_admits() {
        let pool = witness_pool();
        let contracts: Vec<&str> = ARRAY_SPELLINGS
            .iter()
            .copied()
            .chain(["mixed", "non-empty-mixed", "string", "iterable", "iterable<int, int>"])
            .collect();
        // Facts the lowering produces, plus hand-built ones reaching the shapes
        // it cannot spell: proven absence, unknown slots, a forced tail.
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

    // ---- recursion through nested slots ------------------------------------

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
