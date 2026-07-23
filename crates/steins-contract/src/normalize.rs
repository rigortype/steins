//! The type-side normalizer (ADR-0052 §4), extracted from the honesty
//! renderer's dedup / subsumption-collapse / precision-ladder logic — not
//! built as a fresh `TypeCombinator` layer (the ADR-0030 amendment, discharged
//! by slice N1).
//!
//! Types stay syntactic **arm lists** ([`ContractTy`] members) judged arm-wise
//! through the *single* acceptance relation this crate already owns
//! ([`admits_val`] / [`admits_fact`]). This module adds no parallel judgment:
//! [`subsumes`] reduces one arm to the denotation query the acceptance relation
//! already answers.
//!
//! The public surface is complete and final (ADR-0052 §4): pairwise
//! [`subsumes`], [`arm_eq`], [`dedup_arms`], the value-set → normal-form
//! [`summarize_vals`], and arm-wise [`subtract`]. There is deliberately **no**
//! `union(A, B)` and no generic `remove(T, S)`: joins stay the value domain's
//! job (ADR-0030). [`subtract`] (and the public per-arm judgment
//! [`subtrahend_covers`]) consult a real is-a [`IsaOracle`]; N4 wires the project
//! hierarchy through that seam, N1 shipped the [`ReflexiveFloor`] default.
//!
//! ### ADR-0030 registry entry 5 (semantic type equality)
//! Semantic type equality is defined **only** as mutual subsumption (Yes/Yes)
//! over extensional arms ([`arm_eq`]). Provenance-flavored types
//! ([`ContractTy::StrOpaque`] and kin, ADR-0038) are undecidable for equality
//! by construction and are barred from the normalizer's arm vocabulary — the
//! `ContractTy` arm type carries no provenance slot, so the bar is enforced by
//! the type system, not by review. Consistently, [`subsumes`] never answers
//! `Yes` about a provenance-flavored arm; it can only fall to the honest
//! `Maybe`.
//!
//! ### ADR-0048 compliance
//! Every function here is a **pure** function of its arguments: no inference,
//! no cross-scope coupling, no whole-project ordering dependence. Arm lists are
//! declaration-ordered by their caller; [`dedup_arms`] is order-stable.

use crate::{ContractTy, admits_fact, admits_val};
use steins_domain::{Base, Certainty, Fact, Refinement, StrPreds, Val};

/// The set a guard's negative information removes from an arm list (ADR-0052
/// §2). Judged arm-wise by [`subtract`]: an arm dies iff the subtrahend
/// subsumes it with [`Certainty::Yes`]; `Maybe` keeps it (the silence side).
#[derive(Debug, Clone, PartialEq)]
pub enum Subtrahend {
    /// `!== null` — the nullable bit / the `null` arm.
    Null,
    /// `!== v` — a concrete value.
    Value(Val),
    /// `!is_int($x)` and kin — a whole scalar base (deletes the base's arm and
    /// every literal arm it covers).
    Base(Base),
    /// `instanceof` narrowing over class arms. `polarity` is the guard branch:
    /// `false` is the negative branch (`!($v instanceof T)` — subtract the
    /// instances of `T`), `true` the positive branch (`$v instanceof T` —
    /// subtract the non-instances of `T`). The polarity asymmetry of ADR-0052
    /// §2 (is-a is inherited on the negative side, finality-gated on the
    /// positive side) lives in the judgment.
    Class {
        /// The guard class FQN (normalized on comparison).
        fqn: String,
        /// The guard branch (see above).
        polarity: bool,
    },
}

/// The real is-a oracle for class-arm subtraction (ADR-0052 §2, slice N4). Kept
/// as a trait so steins-contract stays **free of any steins-infer dependency**:
/// the project class hierarchy, the builtin catalog, and the amendment-A11
/// version-skew demotion all live in the *caller's* implementor (steins-infer's
/// `ProjectIsa`). N1 shipped only the reflexive floor ([`ReflexiveFloor`]); N4
/// wires the real hierarchy through this seam without moving the polarity law out
/// of this crate.
pub trait IsaOracle {
    /// `is_a(sub, sup)`: is every value of exact class `sub` an instance of `sup`?
    ///
    /// - [`Certainty::Yes`] — a supertype path is proven (`sub` == `sup`, or `sup`
    ///   is a transitive parent/interface of `sub`).
    /// - [`Certainty::No`] — proven non-membership under a **fully enumerated**
    ///   hierarchy (every ancestor edge resolved and `sup` is absent).
    /// - [`Certainty::Maybe`] — Unknown: the enumeration is incomplete, a name is
    ///   unresolvable, **or** an A11 version-skew demotion applied.
    ///
    /// **Argument order is (arm-class, guard-class)** — the arm `M` is `sub`, the
    /// guard target `T` is `sup`. The negative-branch law asks `is_a(M, T)`; the
    /// positive branch asks the same order. Reversing it is the C7 implementation
    /// drift the ADR warns about.
    fn is_a(&self, sub: &str, sup: &str) -> Certainty;

    /// Whether `fqn` is `final` (or an enum) — no subclass can exist, so a proven
    /// non-membership (`is_a(fqn, T) = No`) is **exhaustive** and licenses the
    /// positive-branch deletion of the arm. A non-final class always survives the
    /// positive branch (an unseen descendant could implement `T`).
    fn is_final(&self, fqn: &str) -> bool;
}

/// The reflexive is-a floor N1 shipped: no class hierarchy, so `is_a` decides
/// `Yes` only reflexively (same normalized class name) and is otherwise honest
/// `Maybe`; nothing is `final` (every open class survives the positive branch).
/// This reproduces N1's exact `subtract` behavior when no real oracle is supplied.
#[derive(Debug, Clone, Copy)]
pub struct ReflexiveFloor;

impl IsaOracle for ReflexiveFloor {
    fn is_a(&self, sub: &str, sup: &str) -> Certainty {
        if class_eq(sub, sup) { Certainty::Yes } else { Certainty::Maybe }
    }
    fn is_final(&self, _fqn: &str) -> bool {
        false
    }
}

/// Pairwise arm subsumption: the [`Certainty`] that every value in `b`'s
/// denotation is admitted by `a` (i.e. `a ⊇ b`, the `isSuperTypeOf` shape).
///
/// This reuses the single acceptance relation: `b` is reduced to the value or
/// abstract fact that denotes it, and `a` is queried through
/// [`admits_val`] / [`admits_fact`]. Object arms (`Class`, `object`) have no
/// scalar-fact denotation and are judged by the reflexive is-a floor
/// ([`subsumes_class`]); everything else the acceptance relation cannot decide
/// falls to the honest `Maybe`.
#[must_use]
pub fn subsumes(a: &ContractTy, b: &ContractTy) -> Certainty {
    use Certainty::{Maybe, Yes};
    match b {
        // The empty type is subsumed by everything.
        ContractTy::Never => Yes,
        // `a` must subsume every arm of a union `b`.
        ContractTy::Union(members) => Certainty::all_of(members.iter().map(|m| subsumes(a, m))),
        // `a ⊇ (m1 ∩ m2)` holds if `a` subsumes any member (the intersection
        // is a subset of each); otherwise stay honest.
        ContractTy::Inter(members) => {
            if members.iter().any(|m| subsumes(a, m).is_yes()) { Yes } else { Maybe }
        }

        // `b` denotes a single concrete value — ask the acceptance relation.
        ContractTy::Null => admits_val(a, &Val::Null),
        ContractTy::LitInt(i) => admits_val(a, &Val::Int(*i)),
        ContractTy::LitFloat(f) => admits_val(a, &Val::Float(*f)),
        ContractTy::LitStr(s) => admits_val(a, &Val::Str(s.clone())),
        ContractTy::LitBool(x) => admits_val(a, &Val::Bool(*x)),

        // `b` denotes an abstract scalar fact — ask the for-all acceptance.
        ContractTy::Base(base) => admits_fact(a, &Fact::General { base: *base, nullable: false }),
        ContractTy::StrWith(p) => {
            admits_fact(a, &Fact::refined(Base::String, Refinement::Str(*p), false))
        }
        ContractTy::IntIn(r) => admits_fact(a, &Fact::refined(Base::Int, Refinement::Int(*r), false)),

        // Object arms: no scalar-fact denotation; reflexive is-a floor.
        ContractTy::Class(name) => subsumes_class(a, name),
        ContractTy::ObjectAny => subsumes_object(a),

        // `a` covers everything only if `a` is itself `mixed` (or the unknown
        // `Opaque`, honestly `Maybe`).
        ContractTy::Mixed => match a {
            ContractTy::Mixed => Yes,
            ContractTy::Opaque => Maybe,
            _ => Certainty::No,
        },

        // Array / shape / callable / opaque `b`: outside the scalar-fact
        // vocabulary. `mixed` covers them; otherwise the honest `Maybe` (never
        // a wrong `Yes`, so [`dedup_arms`]/[`subtract`] never collapse them
        // unsoundly).
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. }
        | ContractTy::CallableTy
        | ContractTy::StrOpaque
        | ContractTy::Opaque => match a {
            ContractTy::Mixed => Yes,
            _ => Maybe,
        },
    }
}

/// Whether `a` subsumes all instances of class `name`. The reflexive is-a
/// floor: `object`/`mixed` cover every instance (`Yes`); the same class covers
/// itself (`Yes`); any other class relationship is Unknown here — steins-
/// contract carries no class hierarchy — so it stays `Maybe`, keeping the arm
/// FP-safe (ADR-0052 §2 "Unknown is-a keeps the arm").
fn subsumes_class(a: &ContractTy, name: &str) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::Mixed | ContractTy::ObjectAny => Yes,
        ContractTy::Opaque => Maybe,
        ContractTy::Class(n) => {
            if class_eq(n, name) { Yes } else { Maybe }
        }
        // Some union member covering the class suffices (instances share a
        // class, so one covering arm covers them all).
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(subsumes_class(m, name)))
        }
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(subsumes_class(m, name)))
        }
        // Scalars / arrays / null / literals never cover object instances.
        _ => No,
    }
}

/// Whether `a` subsumes every object (`object`). Only `mixed`/`object` cover
/// the open universe of objects; a single class does not (there are objects of
/// other classes), so it is `No`; `Opaque` is `Maybe`.
fn subsumes_object(a: &ContractTy) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::Mixed | ContractTy::ObjectAny => Yes,
        ContractTy::Opaque => Maybe,
        ContractTy::Union(members) => members.iter().fold(No, |acc, m| acc.or(subsumes_object(m))),
        ContractTy::Inter(members) => members.iter().fold(Yes, |acc, m| acc.and(subsumes_object(m))),
        _ => No,
    }
}

/// Semantic type equality (ADR-0030 registry entry 5): mutual subsumption
/// (Yes/Yes) over extensional arms. Two provenance-flavored arms can never be
/// judged equal (neither subsumes the other with `Yes`), which is the intended
/// undecidability.
#[must_use]
pub fn arm_eq(a: &ContractTy, b: &ContractTy) -> bool {
    subsumes(a, b).is_yes() && subsumes(b, a).is_yes()
}

/// Remove arms that another surviving arm subsumes with [`Certainty::Yes`],
/// preserving the stable order of the survivors. Mutually-subsuming
/// (`arm_eq`) duplicates keep their **first** occurrence.
pub fn dedup_arms(arms: &mut Vec<ContractTy>) {
    let mut kept: Vec<ContractTy> = Vec::with_capacity(arms.len());
    for arm in arms.drain(..) {
        // An arm already covered (Yes) by something kept adds nothing.
        if kept.iter().any(|k| subsumes(k, &arm).is_yes()) {
            continue;
        }
        // This arm survives; it may in turn subsume earlier-kept arms — drop
        // those (the survivor is the wider, more canonical spelling).
        kept.retain(|k| !subsumes(&arm, k).is_yes());
        kept.push(arm);
    }
    *arms = kept;
}

/// The value-set → canonical normal-form (arm list) half of the extraction
/// (ADR-0052 §4). Sorts, dedups, and applies the **computed** collapse of
/// literal groups into their predicate class (numeric literals →
/// `numeric-string`, the bool pair → `bool`, null-fold) — every rung judged by
/// the predicate summary, never guessed.
///
/// Returns `None` on a non-scalar-bearing set (an array member, or an empty
/// set), matching today's `render_value_domain` refusal.
///
/// **Seam (ADR-0052 §4):** this produces the *semantic* arm list only. The
/// docblock literal-safety fallback, the CAP-bounded literal-union spelling
/// decision, quoting/escaping, and member spelling order are rendering policy
/// and stay in `steins-edit`. Concretely: a string group that is *all numeric*
/// with ≥ 2 distinct members is the canonical `numeric-string` class (ADR-0037
/// PDO story) and collapses to a single [`ContractTy::StrWith`] arm here; every
/// other string group is returned as its distinct-sorted [`ContractTy::LitStr`]
/// arms, and the renderer decides how to spell them (a literal, a literal
/// union, or — when a literal cannot be embedded in a docblock — the tightest
/// predicate keyword).
#[must_use]
pub fn summarize_vals(vals: &[Val]) -> Option<Vec<ContractTy>> {
    // Any non-scalar member has no faithful scalar spelling (today's refusal).
    if vals.iter().any(|v| matches!(v, Val::Array(_))) {
        return None;
    }

    // Sort + dedup the whole set once (canonical, order-stable).
    let mut sorted: Vec<Val> = vals.to_vec();
    sorted.sort();
    sorted.dedup();

    let mut has_int = false;
    let mut has_float = false;
    let mut has_true = false;
    let mut has_false = false;
    let mut has_null = false;
    let mut strings: Vec<&str> = Vec::new();
    for v in &sorted {
        match v {
            Val::Int(_) => has_int = true,
            Val::Float(_) => has_float = true,
            Val::Bool(true) => has_true = true,
            Val::Bool(false) => has_false = true,
            Val::Null => has_null = true,
            Val::Str(s) => strings.push(s),
            Val::Array(_) => unreachable!("arrays refused above"),
        }
    }

    // Canonical spelling order: int, float, string(s), bool, null. The renderer
    // re-imposes this order as policy; producing it here keeps the arm list
    // readable and the two orders identical by construction.
    let mut arms: Vec<ContractTy> = Vec::new();
    if has_int {
        arms.push(ContractTy::Base(Base::Int));
    }
    if has_float {
        arms.push(ContractTy::Base(Base::Float));
    }
    arms.extend(summarize_string_group(&strings));
    match (has_true, has_false) {
        (true, true) => arms.push(ContractTy::Base(Base::Bool)),
        (true, false) => arms.push(ContractTy::LitBool(true)),
        (false, true) => arms.push(ContractTy::LitBool(false)),
        (false, false) => {}
    }
    if has_null {
        arms.push(ContractTy::Null);
    }

    // Empty ⟺ the input was empty (a null-only set already yields `[Null]`);
    // that is today's `nullable.then(|| "null")` / empty-proof `None` split.
    if arms.is_empty() { None } else { Some(arms) }
}

/// Canonicalize a string value group into arms. The only *computed collapse*
/// that is semantic (not spelling policy) is the numeric-string class: a group
/// whose members are all numeric and number ≥ 2 distinct is the canonical
/// `numeric-string` predicate class (ADR-0037), collapsing to one
/// [`ContractTy::StrWith`] arm. Every other group is returned as its
/// distinct-sorted literal arms — the renderer owns the literal-vs-keyword
/// spelling decision (safety, CAP).
fn summarize_string_group(strings: &[&str]) -> Vec<ContractTy> {
    if strings.is_empty() {
        return Vec::new();
    }
    let mut distinct: Vec<&str> = strings.to_vec();
    distinct.sort_unstable();
    distinct.dedup();

    // The predicate class every value shares (implication-closed).
    let mut preds = StrPreds::of(distinct[0]);
    for s in &distinct[1..] {
        preds = preds.intersect(StrPreds::of(s));
    }

    if distinct.len() >= 2 && preds.contains_all(StrPreds::NUMERIC) {
        // ≥ 2 all-numeric literals are the `numeric-string` class, not an enum
        // union. A single numeric literal stays precise (`'123'`) — the
        // renderer keeps it, or (when unsafe to embed) widens it itself.
        return vec![ContractTy::StrWith(StrPreds::NUMERIC.close())];
    }
    distinct.into_iter().map(|s| ContractTy::LitStr(s.to_owned())).collect()
}

/// Subtract a guard's negative information from an arm list, arm-wise
/// (ADR-0052 §2). An arm dies iff the subtrahend subsumes it with
/// [`Certainty::Yes`]; `Maybe` keeps it (the silence side). An arm list that
/// this empties is left empty — the caller drops it to no-fact (never a death
/// signal; the verdict owns death, ADR-0052 §2).
pub fn subtract(arms: &mut Vec<ContractTy>, sub: &Subtrahend, oracle: &dyn IsaOracle) {
    arms.retain(|arm| !subtrahend_covers(sub, arm, oracle).is_yes());
}

/// The [`Certainty`] that the subtrahend's denotation covers (subsumes) the whole
/// arm — an arm dies iff this is [`Certainty::Yes`]. `Null`/`Value`/`Base` reduce
/// to a [`ContractTy`] and reuse [`subsumes`]; the class subtrahend carries the
/// polarity asymmetry and consults the real is-a `oracle`.
///
/// Public so a caller carrying a **parallel** per-arm structure (steins-infer's
/// stratified contract lane, `Vec<(ContractTy, Stratum)>`) can `retain` in lockstep
/// with the exact same judgment [`subtract`] uses — the single deletion oracle, no
/// second copy of the polarity law.
#[must_use]
pub fn subtrahend_covers(sub: &Subtrahend, arm: &ContractTy, oracle: &dyn IsaOracle) -> Certainty {
    match sub {
        Subtrahend::Null => subsumes(&ContractTy::Null, arm),
        Subtrahend::Value(v) => subsumes(&val_contract(v), arm),
        Subtrahend::Base(b) => subsumes(&ContractTy::Base(*b), arm),
        Subtrahend::Class { fqn, polarity } => class_covers(fqn, *polarity, arm, oracle),
    }
}

/// The class-arm polarity asymmetry (ADR-0052 §2), judged against the real is-a
/// `oracle` (the reflexive floor still closes the reflexive cases; the project
/// hierarchy + A11 demotion arrive through the caller's implementor).
///
/// - **Negative branch** (`polarity == false`, subtrahend = *instances of T*):
///   a class arm `M` dies iff `is_a(M, T) = Yes` — is-a is inherited, so every
///   possible value of `M` (any descendant) is a `T` and none survives `!instanceof`.
///   `No`/`Unknown` keeps the arm (`Maybe`/`No` — never `Yes`). A non-object arm
///   (a scalar / null / array) is never a `T` instance and survives.
/// - **Positive branch** (`polarity == true`, subtrahend = *non-instances of T*):
///   a class arm `M` dies **only** when `M` is `final`/enum (`oracle.is_final`)
///   **and** `is_a(M, T) = No` — an open class could have a descendant that also
///   implements `T`, so a non-final arm survives (`Maybe`), and `Unknown` keeps it
///   in both polarities. A scalar / null / array arm is definitely a non-instance
///   and dies; a bare `object`/`Opaque`/`mixed` arm survives (`Maybe`).
fn class_covers(fqn: &str, polarity: bool, arm: &ContractTy, oracle: &dyn IsaOracle) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    if polarity {
        // Subtrahend = non-instances of T. Argument order: is_a(M, T).
        match arm {
            ContractTy::Class(m) => {
                if oracle.is_final(m) && oracle.is_a(m, fqn) == No { Yes } else { Maybe }
            }
            ContractTy::ObjectAny | ContractTy::Opaque | ContractTy::Mixed => Maybe,
            _ => Yes,
        }
    } else {
        // Subtrahend = instances of T. Argument order: is_a(M, T) — the arm class
        // is `sub`, the guard target `T` is `sup`. Yes deletes; No/Maybe keep.
        match arm {
            ContractTy::Class(m) => oracle.is_a(m, fqn),
            ContractTy::ObjectAny | ContractTy::Opaque | ContractTy::Mixed => Maybe,
            _ => No,
        }
    }
}

/// The literal contract that denotes exactly one value (for the `Value`
/// subtrahend). An array value has no scalar-literal arm, so it lowers to the
/// unknown `Opaque` — subtracting it covers nothing (sound: N1 subtracts no
/// arrays).
fn val_contract(v: &Val) -> ContractTy {
    match v {
        Val::Int(i) => ContractTy::LitInt(*i),
        Val::Float(f) => ContractTy::LitFloat(*f),
        Val::Str(s) => ContractTy::LitStr(s.clone()),
        Val::Bool(b) => ContractTy::LitBool(*b),
        Val::Null => ContractTy::Null,
        Val::Array(_) => ContractTy::Opaque,
    }
}

/// Normalized class-name equality (leading `\` stripped, ASCII-case-folded) —
/// the normalization [`ContractTy::Class`] arms already carry, applied to the
/// (possibly raw) subtrahend FQN too.
fn class_eq(a: &str, b: &str) -> bool {
    a.trim_start_matches('\\').eq_ignore_ascii_case(b.trim_start_matches('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit_i(n: i64) -> ContractTy {
        ContractTy::LitInt(n)
    }
    fn lit_s(s: &str) -> ContractTy {
        ContractTy::LitStr(s.to_owned())
    }
    fn class(s: &str) -> ContractTy {
        ContractTy::Class(s.to_owned())
    }

    // ---- subsumes -----------------------------------------------------------

    #[test]
    fn base_subsumes_its_literals_and_refinements() {
        assert_eq!(subsumes(&ContractTy::Base(Base::Int), &lit_i(5)), Certainty::Yes);
        assert_eq!(
            subsumes(&ContractTy::Base(Base::String), &lit_s("x")),
            Certainty::Yes
        );
        // string ⊇ numeric-string.
        assert_eq!(
            subsumes(
                &ContractTy::Base(Base::String),
                &ContractTy::StrWith(StrPreds::NUMERIC.close())
            ),
            Certainty::Yes
        );
    }

    #[test]
    fn literal_does_not_subsume_its_base() {
        // A single literal cannot cover the whole base — honest Maybe, never Yes.
        assert_ne!(
            subsumes(&lit_i(5), &ContractTy::Base(Base::Int)),
            Certainty::Yes
        );
    }

    #[test]
    fn disjoint_bases_are_no() {
        assert_eq!(
            subsumes(&ContractTy::Base(Base::Int), &ContractTy::Base(Base::String)),
            Certainty::No
        );
        assert_eq!(subsumes(&ContractTy::Base(Base::Int), &lit_s("x")), Certainty::No);
    }

    #[test]
    fn refined_string_subsumption_follows_predicate_containment() {
        let non_empty = ContractTy::StrWith(StrPreds::NON_EMPTY);
        let numeric = ContractTy::StrWith(StrPreds::NUMERIC.close());
        // non-empty-string ⊇ numeric-string (numeric ⇒ non-empty).
        assert_eq!(subsumes(&non_empty, &numeric), Certainty::Yes);
        // numeric-string does NOT subsume '' — '' is not numeric.
        assert_eq!(subsumes(&numeric, &lit_s("")), Certainty::No);
        // numeric-string ⊇ '123'.
        assert_eq!(subsumes(&numeric, &lit_s("123")), Certainty::Yes);
    }

    #[test]
    fn union_subsumes_each_member() {
        let u = ContractTy::Union(vec![ContractTy::Base(Base::Int), ContractTy::Base(Base::String)]);
        assert_eq!(subsumes(&u, &lit_i(1)), Certainty::Yes);
        assert_eq!(subsumes(&u, &lit_s("x")), Certainty::Yes);
        // `a` must subsume EVERY arm of a union `b`.
        assert_eq!(
            subsumes(
                &u,
                &ContractTy::Union(vec![lit_i(1), ContractTy::Base(Base::Bool)])
            ),
            Certainty::Maybe // bool arm not covered → all_of([Yes, No]) = Maybe
        );
    }

    #[test]
    fn never_is_subsumed_by_anything() {
        assert_eq!(subsumes(&lit_i(1), &ContractTy::Never), Certainty::Yes);
    }

    #[test]
    fn class_subsumption_is_reflexive_else_maybe() {
        assert_eq!(subsumes(&class("user"), &class("user")), Certainty::Yes);
        // Leading `\` and case are normalized.
        assert_eq!(subsumes(&class("user"), &class("\\User")), Certainty::Yes);
        // Unknown hierarchy → Maybe (FP-safe), never a wrong Yes/No.
        assert_eq!(subsumes(&class("user"), &class("guest")), Certainty::Maybe);
        // object ⊇ any instance.
        assert_eq!(subsumes(&ContractTy::ObjectAny, &class("user")), Certainty::Yes);
        // A scalar never subsumes an instance.
        assert_eq!(subsumes(&ContractTy::Base(Base::Int), &class("user")), Certainty::No);
    }

    #[test]
    fn provenance_arm_never_decides_yes() {
        // `StrOpaque` (literal-string and kin) is barred from Yes on either side.
        assert_ne!(subsumes(&ContractTy::StrOpaque, &lit_s("x")), Certainty::Yes);
        assert_ne!(subsumes(&lit_s("x"), &ContractTy::StrOpaque), Certainty::Yes);
        assert!(!arm_eq(&ContractTy::StrOpaque, &ContractTy::StrOpaque));
    }

    // ---- arm_eq -------------------------------------------------------------

    #[test]
    fn arm_eq_is_mutual_subsumption() {
        assert!(arm_eq(&lit_i(5), &lit_i(5)));
        assert!(arm_eq(&ContractTy::Base(Base::Int), &ContractTy::Base(Base::Int)));
        // string ⊋ numeric-string: subsumes one way only → not equal.
        assert!(!arm_eq(
            &ContractTy::Base(Base::String),
            &ContractTy::StrWith(StrPreds::NUMERIC.close())
        ));
    }

    // ---- dedup_arms ---------------------------------------------------------

    #[test]
    fn dedup_drops_subsumed_literal_keeps_base() {
        let mut arms = vec![ContractTy::Base(Base::Int), lit_i(5)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![ContractTy::Base(Base::Int)]);
    }

    #[test]
    fn dedup_survivor_absorbs_earlier_kept_arm() {
        // Literal first, then its base: the base survives, the literal is dropped,
        // and the surviving list is the single base.
        let mut arms = vec![lit_i(5), ContractTy::Base(Base::Int)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![ContractTy::Base(Base::Int)]);
    }

    #[test]
    fn dedup_preserves_stable_order_of_disjoint_arms() {
        let mut arms =
            vec![ContractTy::Base(Base::Int), ContractTy::Base(Base::String), ContractTy::Null];
        let before = arms.clone();
        dedup_arms(&mut arms);
        assert_eq!(arms, before);
    }

    #[test]
    fn dedup_collapses_arm_eq_duplicates_keeping_first() {
        let mut arms = vec![lit_s("a"), lit_s("a")];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![lit_s("a")]);
    }

    // ---- summarize_vals -----------------------------------------------------

    fn i(n: i64) -> Val {
        Val::Int(n)
    }
    fn s(v: &str) -> Val {
        Val::Str(v.to_owned())
    }

    #[test]
    fn summarize_ints_dedup_to_base_int() {
        assert_eq!(summarize_vals(&[i(1), i(2), i(1)]), Some(vec![ContractTy::Base(Base::Int)]));
    }

    #[test]
    fn summarize_single_string_is_a_literal_arm() {
        assert_eq!(summarize_vals(&[s("123")]), Some(vec![lit_s("123")]));
    }

    #[test]
    fn summarize_numeric_group_collapses_to_numeric_string() {
        assert_eq!(
            summarize_vals(&[s("12"), s("34")]),
            Some(vec![ContractTy::StrWith(StrPreds::NUMERIC.close())])
        );
    }

    #[test]
    fn summarize_enum_like_strings_stay_literal_arms_sorted() {
        assert_eq!(
            summarize_vals(&[s("POST"), s("GET"), s("GET")]),
            Some(vec![lit_s("GET"), lit_s("POST")])
        );
    }

    #[test]
    fn summarize_int_and_numeric_strings_is_canonical_union() {
        assert_eq!(
            summarize_vals(&[i(1), s("12"), s("34")]),
            Some(vec![ContractTy::Base(Base::Int), ContractTy::StrWith(StrPreds::NUMERIC.close())])
        );
    }

    #[test]
    fn summarize_bool_pair_and_single() {
        assert_eq!(
            summarize_vals(&[Val::Bool(true), Val::Bool(false)]),
            Some(vec![ContractTy::Base(Base::Bool)])
        );
        assert_eq!(summarize_vals(&[Val::Bool(true)]), Some(vec![ContractTy::LitBool(true)]));
    }

    #[test]
    fn summarize_folds_null_as_an_arm() {
        assert_eq!(
            summarize_vals(&[i(1), Val::Null]),
            Some(vec![ContractTy::Base(Base::Int), ContractTy::Null])
        );
        assert_eq!(summarize_vals(&[Val::Null]), Some(vec![ContractTy::Null]));
    }

    #[test]
    fn summarize_refuses_arrays_and_empty() {
        assert_eq!(summarize_vals(&[Val::Array(vec![])]), None);
        assert_eq!(summarize_vals(&[i(1), Val::Array(vec![])]), None);
        assert_eq!(summarize_vals(&[]), None);
    }

    // ---- subtract -----------------------------------------------------------

    #[test]
    fn subtract_null_removes_only_the_null_arm() {
        let mut arms = vec![ContractTy::Base(Base::Int), ContractTy::Null];
        subtract(&mut arms, &Subtrahend::Null, &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::Base(Base::Int)]);
    }

    #[test]
    fn subtract_value_removes_the_matching_literal_only() {
        let mut arms = vec![lit_i(5), lit_i(6), ContractTy::Base(Base::String)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(5)), &ReflexiveFloor);
        assert_eq!(arms, vec![lit_i(6), ContractTy::Base(Base::String)]);
    }

    #[test]
    fn subtract_value_does_not_touch_the_covering_base() {
        // `!== 5` on a general `int` arm is a no-op (interior point) — the base
        // arm is not subsumed by the single literal.
        let mut arms = vec![ContractTy::Base(Base::Int)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(5)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::Base(Base::Int)]);
    }

    #[test]
    fn subtract_base_deletes_the_arm_and_its_literals() {
        // `!is_int($x)` over `int|string`: the int arm (and any int literal) dies,
        // the string arm survives.
        let mut arms = vec![ContractTy::Base(Base::Int), lit_i(7), ContractTy::Base(Base::String)];
        subtract(&mut arms, &Subtrahend::Base(Base::Int), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::Base(Base::String)]);
    }

    #[test]
    fn subtract_class_negative_branch_reflexive_deletion() {
        // else-branch of `$v instanceof User` over `User|Guest`: User dies
        // (is_a(User,User)=Yes), Guest survives (Unknown is-a keeps it).
        let mut arms = vec![class("user"), class("guest")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "User".to_owned(), polarity: false }, &ReflexiveFloor);
        assert_eq!(arms, vec![class("guest")]);
    }

    #[test]
    fn subtract_class_negative_branch_keeps_scalars() {
        // `!($v instanceof T)` does not remove the possibility of a scalar.
        let mut arms = vec![ContractTy::Base(Base::Int), class("user")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Guest".to_owned(), polarity: false }, &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::Base(Base::Int), class("user")]);
    }

    #[test]
    fn subtract_class_positive_branch_kills_scalars_keeps_classes() {
        // then-branch of `$v instanceof T` over `int|User`: int dies (a proven
        // instance is not a scalar), the class arm survives (finality unknown).
        let mut arms = vec![ContractTy::Base(Base::Int), class("user"), ContractTy::Null];
        subtract(&mut arms, &Subtrahend::Class { fqn: "User".to_owned(), polarity: true }, &ReflexiveFloor);
        assert_eq!(arms, vec![class("user")]);
    }

    #[test]
    fn subtract_can_empty_the_arm_list() {
        let mut arms = vec![ContractTy::Null];
        subtract(&mut arms, &Subtrahend::Null, &ReflexiveFloor);
        assert!(arms.is_empty());
    }

    // ---- subtract with a REAL is-a oracle (N4) ------------------------------

    /// A fixed-hierarchy mock: `edges[sub]` lists `sub`'s proven supertypes
    /// (transitively closed by the mock), `finals` the final/enum classes. Any
    /// class named here is "fully enumerated", so a target absent from its closure
    /// is a definite `No`; a class NOT named at all answers `Unknown` (`Maybe`).
    struct MockIsa {
        edges: std::collections::HashMap<&'static str, Vec<&'static str>>,
        finals: Vec<&'static str>,
        known: Vec<&'static str>,
    }
    impl IsaOracle for MockIsa {
        fn is_a(&self, sub: &str, sup: &str) -> Certainty {
            if class_eq(sub, sup) {
                return Certainty::Yes;
            }
            if !self.known.iter().any(|k| class_eq(k, sub)) {
                return Certainty::Maybe; // unknown class → incomplete enumeration
            }
            if self
                .edges
                .iter()
                .find(|(k, _)| class_eq(k, sub))
                .is_some_and(|(_, sups)| sups.iter().any(|s| class_eq(s, sup)))
            {
                Certainty::Yes
            } else {
                Certainty::No // fully enumerated, target absent
            }
        }
        fn is_final(&self, fqn: &str) -> bool {
            self.finals.iter().any(|f| class_eq(f, fqn))
        }
    }

    fn mock() -> MockIsa {
        // Dog is-a Animal; Cat is-a Animal. Animal, Dog, Cat all known; Dog final.
        MockIsa {
            edges: [("dog", vec!["animal"]), ("cat", vec!["animal"]), ("animal", vec![])]
                .into_iter()
                .collect(),
            finals: vec!["dog"],
            known: vec!["dog", "cat", "animal"],
        }
    }

    #[test]
    fn subtract_negative_branch_deletes_real_subclass_arm() {
        // else of `$v instanceof Animal` over `Dog|Cat|string`: is_a(Dog,Animal)=Yes
        // and is_a(Cat,Animal)=Yes both die; the scalar arm survives.
        let mut arms = vec![class("dog"), class("cat"), ContractTy::Base(Base::String)];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Animal".to_owned(), polarity: false }, &mock());
        assert_eq!(arms, vec![ContractTy::Base(Base::String)]);
    }

    #[test]
    fn subtract_negative_branch_argument_order_is_m_then_t() {
        // Guard `instanceof Dog` over arm `Animal`: the ADR asks is_a(Animal, Dog)
        // = No (Animal is NOT a Dog) → the Animal arm SURVIVES the negation. A
        // reversed is_a(Dog, Animal)=Yes would wrongly delete it — the C7 drift.
        let mut arms = vec![class("animal")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Dog".to_owned(), polarity: false }, &mock());
        assert_eq!(arms, vec![class("animal")], "is_a(M,T) order: Animal is not a Dog, arm kept");
    }

    #[test]
    fn subtract_negative_branch_unknown_keeps_arm() {
        // `Mystery` is not in the mock's known set → is_a Unknown → arm kept both
        // polarities (FP-safe).
        let mut neg = vec![class("mystery")];
        subtract(&mut neg, &Subtrahend::Class { fqn: "Animal".to_owned(), polarity: false }, &mock());
        assert_eq!(neg, vec![class("mystery")]);
        let mut pos = vec![class("mystery")];
        subtract(&mut pos, &Subtrahend::Class { fqn: "Animal".to_owned(), polarity: true }, &mock());
        assert_eq!(pos, vec![class("mystery")]);
    }

    #[test]
    fn subtract_positive_branch_deletes_final_nonmember_only() {
        // then of `$v instanceof Cat` over `Dog|Cat`: Dog is final AND is_a(Dog,Cat)
        // = No → Dog dies; Cat is is_a(Cat,Cat)=Yes so it is NOT a non-instance →
        // survives (Maybe).
        let mut arms = vec![class("dog"), class("cat")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Cat".to_owned(), polarity: true }, &mock());
        assert_eq!(arms, vec![class("cat")]);
    }

    #[test]
    fn subtract_positive_branch_keeps_nonfinal_nonmember() {
        // `Animal` is NOT final, so even though is_a(Animal, Cat)=No, the positive
        // branch keeps it — an unseen Animal subclass could be a Cat. The drift
        // "positive-branch deleting a non-final arm" is guarded here.
        let mut arms = vec![class("animal")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Cat".to_owned(), polarity: true }, &mock());
        assert_eq!(arms, vec![class("animal")]);
    }
}
