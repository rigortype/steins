//! What a `foreach` header binds at the body entry (issue #652, completing the
//! ADR-0027 amendment of 2026-09-10).
//!
//! `foreach ($xs as $k => $v)` binds rather than tests, and what it binds is
//! already in hand: the subject's own element type. `list<int>` makes `$v` an
//! `int` and `$k` an `int<0, max>`; `array<string, Foo>` makes `$k` a `string`
//! and `$v` a `Foo`; a sealed `array{a: int, b: string}` makes `$k` one of its
//! keys and `$v` one of its value types; a proven constant array answers from the
//! entries themselves. No new inference happens here — every answer is a
//! projection of a fact the subject already carries.
//!
//! **The two lanes, in ADR-0037's trust order.** A proven array (the value lane)
//! answers first and answers exactly, at the subject's own stratum. Failing that,
//! the subject's declared contract arms answer, at the arms' stratum — which for a
//! docblock-declared `@param list<int>` is `Asserted`, so the element fact can
//! never premise a proof-layer finding (ADR-0052 §5, the A-G9 corollary). A bare
//! native `array` states no element type and therefore binds nothing: this module
//! never invents the fact `untyped.iterable-value` exists to demand.
//!
//! **What it refuses.** A by-reference target (`as &$v`) — the loop writes through
//! the subject and the trace models neither the write nor the aliasing (issue
//! #677), so the element `$v` names on iteration 2 is one this module has no fact
//! about. A destructuring or property target — there is no single name to bind, so
//! it stays a write alone. Both refusals are silent: the target keeps the
//! defined-but-untyped binding the entry forgetting leaves it with.
//!
//! `Traversable`/`Generator` subjects are out of scope by the issue's own ruling:
//! their value type rides `@implements`/`@extends` template arguments, a lane of
//! its own. A declared `iterable<K, V>` is not that case — it states its key and
//! value outright, and is read here like the `array<K, V>` it spells.

use steins_contract::{CKey, ContractTy};
use steins_domain::{Fact, IntRange, Key, Val};

use crate::env::{ContractArm, Known, Stratum};
use crate::refine::flatten_arms;

/// The `Known::bound` provenance a `foreach`-bound target carries, so a reader of
/// a dump or a finding is told which construct minted the fact.
const FOREACH_ELEMENT: &str = "bound by foreach from the subject's element type";
const FOREACH_KEY: &str = "bound by foreach from the subject's key type";

/// What one `foreach` header binds at the body entry — either half may be absent,
/// and an absent half leaves its target defined but untyped.
#[derive(Default)]
pub(crate) struct ElementBinding {
    /// The key fact and its stratum.
    pub(crate) key: Option<(Fact, Stratum)>,
    /// The value fact and its stratum.
    pub(crate) value: Option<(Fact, Stratum)>,
    /// The element's declared arms — the lane a **class** element lives in, since
    /// the value domain has no object inhabitant (ADR-0035/0043) and `Foo` has no
    /// `Fact` to be. Carried beside `value` rather than instead of it: a scalar
    /// element populates both, exactly as a declared parameter does.
    pub(crate) value_arms: Option<Vec<ContractArm>>,
}

impl ElementBinding {
    /// Whether this binding states anything at all.
    fn is_empty(&self) -> bool {
        self.key.is_none() && self.value.is_none() && self.value_arms.is_none()
    }

    /// The [`Known`] to bind the value target to, at `line`.
    pub(crate) fn value_known(&self, line: u32) -> Option<Known> {
        let (fact, stratum) = self.value.clone()?;
        Some(Known::value_strat(fact, line, Some(FOREACH_ELEMENT.to_owned()), stratum))
    }

    /// The [`Known`] to bind the key target to, at `line`.
    pub(crate) fn key_known(&self, line: u32) -> Option<Known> {
        let (fact, stratum) = self.key.clone()?;
        Some(Known::value_strat(fact, line, Some(FOREACH_KEY.to_owned()), stratum))
    }
}

/// The element binding a subject's entry-env state supports, or an empty one when
/// it supports none.
///
/// `known` and `arms` are the subject's two lanes read from the **body entry** env
/// — the iteration-count-agnostic one, after the entry forgetting. That is what
/// makes the binding re-establish identically at every entry: a subject the body
/// rebinds is not in that env at all, so nothing is bound from it, and a subject
/// the body cannot touch reads the same on iteration 40 as on iteration 1.
pub(crate) fn element_binding(
    known: Option<&Known>,
    arms: Option<&[ContractArm]>,
) -> ElementBinding {
    // The value lane first (ADR-0037: proven beats declared). A fully-known array
    // answers both halves exactly, and at its own stratum — `Verified` for a
    // literal, so `foreach ([1, 2] as $v)` may premise a proof.
    if let Some(k) = known
        && let Some(Fact::Singleton(Val::Array(entries))) = &k.fact
    {
        let b = from_entries(entries, k.stratum);
        if !b.is_empty() {
            return b;
        }
    }
    arms.map(from_arms).unwrap_or_default()
}

/// The binding a **proven** array supports: the join of its keys and the join of
/// its values, both at the subject's own stratum.
///
/// An empty array supports neither, and declines — there is no element, and a
/// zero-iteration loop's body needs no invented one.
fn from_entries(entries: &[(Key, Val)], stratum: Stratum) -> ElementBinding {
    let key = join_vals(entries.iter().map(|(k, _)| key_val(k)));
    let value = join_vals(entries.iter().map(|(_, v)| v.clone()));
    ElementBinding {
        key: key.map(|f| (f, stratum)),
        value: value.map(|f| (f, stratum)),
        // A proven element has a value fact or nothing; the declared lane is the
        // other producer's business.
        value_arms: None,
    }
}

/// An array key as the value PHP hands the loop variable — a key IS a value, and
/// this is the whole of the conversion (keys are already normalized, ADR-0062).
fn key_val(k: &Key) -> Val {
    match k {
        Key::Int(i) => Val::Int(*i),
        Key::Str(s) => Val::Str(s.clone()),
    }
}

/// The single fact a value sequence joins to, through the domain's own
/// [`Fact::join`] — so the `OneOf` cap and every widening rung are the ones the
/// rest of the engine uses, not a second reading of them. `None` for an empty
/// sequence or a join the domain declines.
fn join_vals(vals: impl Iterator<Item = Val>) -> Option<Fact> {
    let mut acc: Option<Fact> = None;
    for v in vals {
        let f = Fact::Singleton(v);
        acc = Some(match acc {
            None => f,
            Some(prev) => prev.join(&f)?,
        });
    }
    acc
}

/// The binding a **declared** array lane supports: the key and element contracts
/// the one array arm states, lowered through [`steins_contract::to_fact`].
///
/// One array arm, exactly. A `null` arm is skipped (a nullable subject is a
/// `foreach.non-iterable` question, not an element-type one); two or more array
/// arms decline for [`crate::env::Store`]'s own A-G3 reason — the arms carry a
/// discrimination a union fold would blur, and the element of a blur is not the
/// element of either arm.
fn from_arms(arms: &[ContractArm]) -> ElementBinding {
    let mut only: Option<&ContractArm> = None;
    for arm in arms {
        if matches!(arm.ty, ContractTy::Null) {
            continue;
        }
        if only.is_some() {
            return ElementBinding::default();
        }
        only = Some(arm);
    }
    let Some(arm) = only else { return ElementBinding::default() };
    let Some((key_ty, val_ty)) = element_contracts(&arm.ty) else {
        return ElementBinding::default();
    };
    // The stratum is the arm's, unchanged: a projection of a declared fact is that
    // fact's grade, never a better one (ADR-0052's derivation clause).
    let stratum = arm.stratum;
    let value_arms: Vec<ContractArm> = flatten_arms(val_ty.clone())
        .into_iter()
        .map(|ty| ContractArm { ty, stratum })
        .collect();
    ElementBinding {
        key: key_ty.and_then(|t| steins_contract::to_fact(&t)).map(|f| (f, stratum)),
        value: steins_contract::to_fact(&val_ty).map(|f| (f, stratum)),
        value_arms: (!value_arms.is_empty()).then_some(value_arms),
    }
}

/// The `(key, value)` contracts one array-ish declared type states, or `None` when
/// it states no element type at all.
///
/// The key half is optional on its own: a `list<T>`'s keys are the non-negative
/// ints by definition, an `array<K, V>` says its own `K`, and an unsealed shape
/// with an untyped tail key says nothing while still typing its values.
fn element_contracts(ty: &ContractTy) -> Option<(Option<ContractTy>, ContractTy)> {
    match ty {
        // `list<T>`: keys are exactly `0..n-1` (#14939), so the key type is the
        // non-negative ints — sharper than `array<int, T>`'s bare `int`, and the
        // one thing the list spelling buys a reader of the body.
        ContractTy::ListOf { elem, .. } => {
            Some((Some(ContractTy::IntIn(IntRange::NON_NEGATIVE)), (**elem).clone()))
        }
        // `array<K, V>`, `T[]`, and `iterable<K, V>` — the same two contracts, and
        // `iterable`'s array realization iterates exactly like the map one.
        ContractTy::MapOf { key, val, .. } | ContractTy::IterableOf { key, val } => {
            Some((Some((**key).clone()), (**val).clone()))
        }
        // A shape: the union of what its fields declare. Sealed only — an unsealed
        // tail admits keys and values the field list does not mention, so the union
        // of the declared fields is not the element type, it is a subset of it.
        // A typed tail (`array{a: int, ...<string, int>}`) joins in as one more
        // alternative, which is exactly what it admits.
        ContractTy::Shape { fields, sealed, unsealed, .. } => {
            let mut keys: Vec<ContractTy> = Vec::new();
            let mut vals: Vec<ContractTy> = Vec::new();
            for f in fields {
                keys.push(match &f.key {
                    CKey::Int(i) => ContractTy::LitInt(*i),
                    CKey::Str(s) => ContractTy::LitStr(s.clone()),
                });
                vals.push(f.ty.clone());
            }
            match (sealed, unsealed) {
                (true, _) => {}
                (false, Some((tail_key, tail_val))) => {
                    // A tail with no declared key admits `array-key`; saying so
                    // would be a claim about keys the shape does not make, so the
                    // key half drops out entirely instead.
                    match tail_key {
                        Some(k) => keys.push((**k).clone()),
                        None => keys.clear(),
                    }
                    vals.push((**tail_val).clone());
                }
                // An unsealed shape with an untyped tail states nothing about what
                // else is in there.
                (false, None) => return None,
            }
            if vals.is_empty() {
                return None;
            }
            let key = (!keys.is_empty()).then_some(ContractTy::Union(keys));
            Some((key, ContractTy::Union(vals)))
        }
        // `array` / `non-empty-array` without parameters: the acceptance criterion
        // this arm exists for — no element type is stated, so none is invented.
        _ => None,
    }
}
