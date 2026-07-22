//! Acceptance judgments: values and facts against contract types.
//!
//! Everything is Kleene composition: conjunction (`and`) for "all conditions
//! hold", disjunction (`or`) across union members, [`Certainty::all_of`] for
//! "every possible value". The abstract-fact path uses a documented sound
//! under-approximation: a union that only *jointly* covers a base (e.g.
//! `int<min,0>|int<0,max>` over general `int`) answers `Maybe`, never a
//! wrong verdict.

use crate::{CField, CKey, ContractTy};
use steins_domain::{Base, Certainty, Fact, Refinement, StrPreds, Val};
use steins_domain::Key as VKey;

/// Is the concrete value admitted by the contract?
#[must_use]
pub fn admits_val(ty: &ContractTy, v: &Val) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match ty {
        ContractTy::Mixed => Yes,
        ContractTy::Never => No,
        ContractTy::Opaque => Maybe,
        ContractTy::Null => Certainty::from_bool(*v == Val::Null),
        ContractTy::Base(b) => match (b, v) {
            // int is accepted where float is expected (PHPStan core).
            (Base::Float, Val::Int(_)) => Yes,
            _ => Certainty::from_bool(v.base() == Some(*b)),
        },
        ContractTy::IntIn(r) => match v {
            Val::Int(i) => Certainty::from_bool(r.contains(*i)),
            _ => No,
        },
        ContractTy::StrWith(p) => match v {
            Val::Str(s) => Certainty::from_bool(StrPreds::of(s).contains_all(*p)),
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
        ContractTy::MapOf { key, val, non_empty } => match v {
            Val::Array(items) => {
                if *non_empty && items.is_empty() {
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
        ContractTy::CallableTy => match v {
            Val::Str(_) | Val::Array(_) => Maybe,
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
                    // Positive predicate sets always overlap ("5" satisfies
                    // every predicate), so refuting is impossible here.
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
        ContractTy::CallableTy => {
            if base == Base::String { Maybe } else { No }
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

fn is_list(items: &[(VKey, Val)]) -> bool {
    items.iter().enumerate().all(|(i, (k, _))| matches!(k, VKey::Int(v) if *v == i as i64))
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

/// Shape acceptance per #14939: `array{}` is an order-agnostic key set,
/// `list{}` a positional sequence (which must also *be* a list).
fn admits_shape(
    list: bool,
    fields: &[CField],
    sealed: bool,
    non_empty: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
    items: &[(VKey, Val)],
) -> Certainty {
    use Certainty::{No, Yes};

    if non_empty && items.is_empty() {
        return No;
    }
    if list && !is_list(items) {
        return No;
    }

    let lookup = |key: &CKey| -> Option<&Val> {
        items.iter().find_map(|(k, v)| {
            let matches = match (k, key) {
                (VKey::Int(a), CKey::Int(b)) => a == b,
                (VKey::Str(a), CKey::Str(b)) => a == b,
                _ => false,
            };
            matches.then_some(v)
        })
    };

    let mut verdict = Yes;
    for field in fields {
        match lookup(&field.key) {
            Some(v) => verdict = verdict.and(admits_val(&field.ty, v)),
            None if field.optional => {}
            None => return No,
        }
    }

    // Extra entries: keys not declared by any field.
    for (k, v) in items {
        let declared = fields.iter().any(|f| match (&f.key, k) {
            (CKey::Int(b), VKey::Int(a)) => a == b,
            (CKey::Str(b), VKey::Str(a)) => a == b,
            _ => false,
        });
        if declared {
            continue;
        }
        match unsealed {
            Some((key_ty, val_ty)) => {
                if let Some(kt) = key_ty {
                    verdict = verdict.and(admits_val(kt, &key_as_val(k)));
                }
                verdict = verdict.and(admits_val(val_ty, v));
            }
            None => {
                if sealed {
                    return No;
                }
                // Unsealed without a declared tail type: anything goes.
            }
        }
    }

    verdict
}
