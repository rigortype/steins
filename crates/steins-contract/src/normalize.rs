//! The type-side normalizer (ADR-0052 §4), extracted from the honesty
//! renderer's dedup / subsumption-collapse / precision-ladder logic rather
//! than built as a separate `TypeCombinator` layer (ADR-0030).
//!
//! Types stay syntactic **arm lists** ([`ContractTy`] members) judged arm-wise
//! through the single acceptance relation this crate owns
//! ([`admits_val`] / [`admits_fact`]); [`subsumes`] reduces one arm to that
//! query and adds no parallel judgment. Two arm families have no scalar-fact
//! denotation and carry their rules *inside* [`subsumes`] instead: object arms
//! via the reflexive is-a floor (`subsumes_class`), and the array vocabulary
//! via the structural denotation of ADR-0071 §2.1 (`subsumes_array`), whose
//! leaves recurse back through [`subsumes`]/`admits_val`. (`shape_verdict` in
//! admit.rs is the only type-vs-*value* shape relation.)
//!
//! Public surface (ADR-0052 §4, `merge_int_arms` added per the §4 note of
//! 2026-08-02): [`subsumes`], [`arm_eq`], [`dedup_arms`], [`summarize_vals`]
//! (value-set → normal-form), [`subtract`], and [`merge_int_arms`] (interval
//! absorption behind [`dedup_arms`], reusable by a stratified arm carrier such
//! as steins-infer's contract lane). Deliberately no generic `union(A, B)` or
//! `remove(T, S)` (joins stay the value domain's job, ADR-0030);
//! `merge_int_arms` answers only where the union of two arms IS a single arm,
//! mirrored on subtraction by [`subtract_arm`]'s endpoint clip.
//! `subtract`/`subtract_arm` consult a real is-a [`IsaOracle`]
//! ([`ReflexiveFloor`] default); the same seam carries
//! [`provably_uninhabited`] (issue #234) with the declared [`FinalKeyword`]
//! posture explicit, so no caller can collapse `final A ∧ B` to `never` blindly.
//!
//! **ADR-0030 registry entry 5:** semantic type equality is defined **only**
//! as mutual subsumption (Yes/Yes) over extensional arms ([`arm_eq`]).
//! Provenance-flavored types ([`ContractTy::StrOpaque`] and kin, ADR-0038) are
//! undecidable for equality by construction and barred from the arm
//! vocabulary by the type system, so [`subsumes`] never answers `Yes` about
//! one — only the honest `Maybe`.
//!
//! **ADR-0048 compliance:** every function here is pure. Arm lists are
//! declaration-ordered by their caller; [`dedup_arms`] is order-stable.

use crate::{CField, CKey, ContractTy, MixedCut, admits_fact, admits_val};
use steins_domain::{
    Base, Certainty, Fact, IntRange, Key, KeyClass, PhpStr, Presence, Refinement, ShapeFact,
    StrPreds, Tail, Val, php_is_falsy,
};

/// The set a guard's negative information removes from an arm list (ADR-0052
/// §2). Judged arm-wise by [`subtract`] / [`subtract_arm`]: an arm dies iff
/// the subtrahend subsumes it with [`Certainty::Yes`]; `Maybe` keeps it, except
/// an interval arm losing its own endpoint, which shrinks instead
/// ([`ArmFate::Narrows`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Subtrahend {
    /// `!== null`.
    Null,
    /// `!== v` — a concrete value.
    Value(Val),
    /// `!is_int($x)` and kin — deletes the base's arm and every literal it covers.
    Base(Base),
    /// `instanceof` narrowing over class arms. `polarity`: `false` is the
    /// negative branch (`!($v instanceof T)`, subtract instances of `T`);
    /// `true` the positive branch (subtract non-instances of `T`). The
    /// ADR-0052 §2 polarity asymmetry lives in the judgment, not here.
    Class {
        /// The guard class FQN (normalized on comparison).
        fqn: String,
        polarity: bool,
    },
    /// `=== Enum::Case` / `!== Enum::Case` narrowing over enum-case arms (issue
    /// #429), the [`Self::Class`] shape one rung finer: the subtrahend is a
    /// single *value*, not a class extent.
    ///
    /// `polarity`: `false` is the negative branch (`$s !== Enum::Case`, subtract
    /// that one case); `true` the positive branch (`$s === Enum::Case`, subtract
    /// **everything else**). The positive branch is a subtraction here rather
    /// than a keep-only intersection precisely so the arm lane stays what
    /// ADR-0052 §2 built it as — a carrier every mutation removes provably-dead
    /// arms from — even though the value lane cannot own this branch the way it
    /// owns `if ($x === false)` (an enum case has no `Val`).
    EnumCase {
        /// The guard enum's FQN (normalized on comparison).
        enum_fqn: String,
        /// The guard case name (compared case-sensitively).
        case: String,
        polarity: bool,
    },
    /// The truthy branch of a bare truthiness guard (`if ($x)`, `$x && …`,
    /// `$x ? … : …`, and the else of `if (!$x)`) — issue #557. The subtrahend
    /// is the **falsy set**: every arm all of whose inhabitants PHP judges
    /// falsy dies, which is what makes `string|false` under `if ($x)` a
    /// `string` on that branch.
    ///
    /// This is deliberately whole-arm deletion and nothing finer. It does not
    /// refine *within* a surviving arm — an `int` arm still spells `int` where
    /// the guard has in fact excluded `0`, and a `bool` arm still spells `bool`
    /// where the guard has excluded `false`. Both are widenings of the truth,
    /// so the lane stays sound; the finer readings are neighbouring work
    /// ([`ArmFate::Narrows`] is where they would land).
    Falsy,
}

/// The is-a oracle for class-arm subtraction (ADR-0052 §2), a trait so
/// steins-contract stays free of any steins-infer dependency: the project
/// class hierarchy, builtin catalog, and A11 version-skew demotion live in the
/// caller's implementor (`ProjectIsa`).
pub trait IsaOracle {
    /// `is_a(sub, sup)`: is every value of exact class `sub` an instance of `sup`?
    /// [`Certainty::Yes`] — proven supertype path. [`Certainty::No`] — proven
    /// non-membership under a **fully enumerated** hierarchy. [`Certainty::Maybe`]
    /// — incomplete enumeration, unresolvable name, or an A11 demotion.
    ///
    /// **Argument order is (arm-class, guard-class)** in both branches —
    /// reversing it is the implementation drift the ADR warns about.
    fn is_a(&self, sub: &str, sup: &str) -> Certainty;

    /// Whether `fqn` is `final` (no subclass exists, so proven non-membership
    /// licenses positive-branch deletion); non-final always survives (unseen
    /// descendant could implement `T`).
    fn is_final(&self, fqn: &str) -> bool;
}

/// The reflexive is-a floor: without a class hierarchy, `is_a` decides `Yes`
/// only for the same normalized class name and otherwise `Maybe`; nothing is
/// `final`, so every open class survives the positive branch.
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

/// The `[runtime] final-keyword` posture (issue #234): what the project's
/// runtime *does* with the `final` keyword. ADR-0037 §2 pseudo-constant
/// family (same shelf as `warning-handler`): a boot truth no source reading
/// settles, so the project declares it. [`Self::Enforced`] is the
/// [`Default`], so a silent project keeps today's semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FinalKeyword {
    /// `final-keyword = "enforced"` — the language's own rule and the absence
    /// default: a `final` class (or enum) admits no subtype, so its instances
    /// all have that exact class.
    #[default]
    Enforced,
    /// `final-keyword = "stripped"` — the project declares its runtime strips
    /// `final` before compiling the class (motivating case: `dg/bypass-finals`),
    /// so a mock subclass of a `final` class genuinely exists and
    /// `FinalClass&MockObject` is inhabited.
    ///
    /// Does NOT license: `readonly.reassigned` widening (separate, untouched
    /// knob); silencing the `final` diagnostics (issue #234 "out of scope" —
    /// only intersection inhabitance is at stake); or inferring a
    /// final-stripping loader (issue #205 — this posture is declared or absent).
    ///
    /// v1 boundary: project-wide, not path-scoped like the real call site
    /// (`denyPaths([…])`) — honest (only ever withdraws an emptiness proof),
    /// and a scoped answer has nowhere to live until ADR-0047's
    /// `[transform.partitions]` regions reach the check lane.
    Stripped,
}

/// Whether `t`'s denotation is **provably empty** under the class hierarchy
/// `oracle` answers for and the declared `final_keyword` posture.
///
/// Issue #234 plants this guard ahead of its consumer (intersection
/// consumption, issue #238); the required `final_keyword` argument makes the
/// posture impossible to skip, since computing `final A ∧ B` as empty
/// unconditionally is a false claim under a final-stripping loader.
///
/// `true` is a **proof**, so every leg is conservative: algebraic emptiness
/// (`denotes_nothing`, posture-independent), plus the **sealed-class
/// conflict** — under [`FinalKeyword::Enforced`], a `final` class arm `F` has
/// no subtype, so every intersection value has exact class `F` and one proven
/// `is_a(F, T) = No` empties it; `Unknown` or a non-final arm keeps it alive
/// (FP-safe). Under [`FinalKeyword::Stripped`] this leg does not run.
///
/// Emptiness for an unmodeled reason (`int&string`, an abstract class with no
/// concrete descendant) answers `false` here too — "not proven empty", never
/// "proven inhabited".
#[must_use]
pub fn provably_uninhabited(
    t: &ContractTy,
    oracle: &dyn IsaOracle,
    final_keyword: FinalKeyword,
) -> bool {
    if denotes_nothing(t) {
        return true;
    }
    match t {
        // A union is empty only when every member is.
        ContractTy::Union(members) => {
            members.iter().all(|m| provably_uninhabited(m, oracle, final_keyword))
        }
        // An intersection is empty as soon as one member is, plus the finality leg.
        ContractTy::Inter(members) => {
            members.iter().any(|m| provably_uninhabited(m, oracle, final_keyword))
                || sealed_class_conflict(members, oracle, final_keyword)
        }
        _ => false,
    }
}

/// The finality leg of [`provably_uninhabited`]: two class arms an *enforced*
/// `final` keyword cannot let one value satisfy at once.
///
/// `is_a(F, F)` is reflexively `Yes`, so a lone final arm never conflicts with
/// itself; a non-class member is not consulted here (its own emptiness is the
/// recursive leg's business; issue #238 leaves its object-ness unaddressed).
fn sealed_class_conflict(
    members: &[ContractTy],
    oracle: &dyn IsaOracle,
    final_keyword: FinalKeyword,
) -> bool {
    if final_keyword == FinalKeyword::Stripped {
        return false;
    }
    let classes: Vec<&str> = members
        .iter()
        .filter_map(|m| match m {
            ContractTy::Class(fqn) => Some(fqn.as_str()),
            _ => None,
        })
        .collect();
    classes.iter().any(|&f| {
        oracle.is_final(f) && classes.iter().any(|&t| oracle.is_a(f, t) == Certainty::No)
    })
}

/// Pairwise arm subsumption: the [`Certainty`] that every value in `b`'s
/// denotation is admitted by `a` (i.e. `a ⊇ b`, the `isSuperTypeOf` shape).
///
/// Reuses the single acceptance relation: `b` reduces to the value or abstract
/// fact that denotes it, queried against `a` via [`admits_val`] /
/// [`admits_fact`]. Object arms (`Class`, `object`) have no scalar-fact
/// denotation and are judged by the reflexive is-a floor (`subsumes_class`);
/// the array vocabulary is judged *structurally* by `subsumes_array`
/// (ADR-0071 §2.1); everything else undecidable falls to the honest `Maybe`.
#[must_use]
pub fn subsumes(a: &ContractTy, b: &ContractTy) -> Certainty {
    use Certainty::{Maybe, Yes};
    // Conjunction-vs-conjunction, judged ARM-WISE (issue #238): asked whole,
    // `A&B ⊇ A&B` would fold through the `b`-dispatch to the unprovable `A&B ⊇ A`.
    // Rule: `(A₁∩…∩Aₙ) ⊇ (B₁∩…∩Bₘ)` when every `Aᵢ` subsumes some `Bⱼ` — sound by
    // construction (each `x ∈ ∩Bⱼ` has a witness `Bⱼ ⊆ Aᵢ` for every `Aᵢ`). Only
    // the proven `Yes` is claimed; short of it falls through, so this can widen a
    // `Maybe` to `Yes` but never manufacture a `No`.
    if let (ContractTy::Inter(am), ContractTy::Inter(bm)) = (a, b)
        && !am.is_empty()
        && !bm.is_empty()
        && am.iter().all(|x| bm.iter().any(|y| subsumes(x, y).is_yes()))
    {
        return Yes;
    }
    match b {
        ContractTy::Never => Yes,
        ContractTy::Union(members) => Certainty::all_of(members.iter().map(|m| subsumes(a, m))),
        // `a ⊇ (m1 ∩ m2)` if `a` subsumes any member; otherwise stay honest.
        ContractTy::Inter(members) => {
            if members.iter().any(|m| subsumes(a, m).is_yes()) { Yes } else { Maybe }
        }

        // `b` a single concrete value: ask the acceptance relation.
        ContractTy::Null => admits_val(a, &Val::Null),
        ContractTy::LitInt(i) => admits_val(a, &Val::Int(*i)),
        ContractTy::LitFloat(f) => admits_val(a, &Val::Float(*f)),
        ContractTy::LitStr(s) => admits_val(a, &Val::Str(s.clone())),
        ContractTy::LitBool(x) => admits_val(a, &Val::Bool(*x)),

        // `b` an abstract scalar fact: ask the for-all acceptance.
        ContractTy::Base(base) => admits_fact(a, &Fact::General { base: *base, nullable: false }),
        ContractTy::StrWith(p) => {
            admits_fact(a, &Fact::refined(Base::String, Refinement::Str(*p), false))
        }
        ContractTy::IntIn(r) => admits_fact(a, &Fact::refined(Base::Int, Refinement::Int(*r), false)),

        // Object arms: no scalar-fact denotation; reflexive is-a floor.
        ContractTy::Class(name) => subsumes_class(a, name),
        ContractTy::EnumCase { enum_fqn, case } => subsumes_enum_case(a, enum_fqn, case),
        ContractTy::ObjectAny => subsumes_object(a),
        // The resource leaf: no scalar-fact denotation, but no hierarchy to be
        // unsure about either, so the answer is exact both ways (ADR-0056 §8).
        ContractTy::Resource => subsumes_resource(a),

        // `a` covers everything only if `a` is `mixed` itself (`Opaque` → `Maybe`).
        ContractTy::Mixed => match a {
            ContractTy::Mixed => Yes,
            ContractTy::Opaque => Maybe,
            _ => Certainty::No,
        },

        // The array vocabulary: a structural denotation (ADR-0071 §2.1's rule
        // table). `Yes` needs coverage over `b`'s whole denotation (`[]` and the
        // #14939 order-agnostic keyed realizations included); `No` needs a
        // witness `a` provably rejects; `Maybe` is the floor elsewhere.
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. } => subsumes_array(a, b),

        // Callable / provenance / opaque / `unset` `b`: outside the scalar-fact
        // vocabulary. Only `mixed` provably covers them (a cut of `mixed` still
        // spans every base, so it has no scalar-fact denotation either);
        // otherwise the honest `Maybe`, never a wrong `Yes`. `unset` is here and
        // not beside `Never` above: its empty *value* denotation would make
        // `a ⊇ unset` a free `Yes`, a claim about a member no value inhabits, so
        // the floor stays undecided (ADR-0087) — arm builders drop it anyway.
        ContractTy::CallableTy { .. }
        | ContractTy::StrOpaque
        | ContractTy::MixedMinus(_)
        | ContractTy::Unset
        | ContractTy::Opaque => match a {
            ContractTy::Mixed => Yes,
            _ => Maybe,
        },
    }
}

/// Whether `a` subsumes all instances of class `name`. The reflexive is-a
/// floor: `object`/`mixed` and the same class cover it (`Yes`); any other
/// relationship is Unknown (this module carries no class hierarchy), so `Maybe`
/// (ADR-0052 §2 "Unknown is-a keeps the arm").
fn subsumes_class(a: &ContractTy, name: &str) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::Mixed | ContractTy::ObjectAny => Yes,
        ContractTy::Opaque => Maybe,
        ContractTy::Class(n) => {
            if class_eq(n, name) { Yes } else { Maybe }
        }
        // One case never covers a whole class — but it is not disjoint from one
        // either (a single-case enum's one case IS the enum, and any class arm
        // this module cannot place in the hierarchy may be a supertype). The
        // honest floor, never the `No` the catch-all below would claim.
        ContractTy::EnumCase { .. } => Maybe,
        // Some union member covering the class suffices (instances share a class).
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(subsumes_class(m, name)))
        }
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(subsumes_class(m, name)))
        }
        _ => No,
    }
}

/// Whether `a` subsumes the single value `enum_fqn::case` (issue #429). Sharper
/// than [`subsumes_class`] on both ends, because an enum case is one *value*
/// rather than a class extent: the enum's own name covers it outright, a
/// different case of the same enum is provably disjoint from it, and both cuts
/// of `mixed` keep it (an object is neither null nor falsy — the argument
/// [`subsumes_resource`] makes for the resource leaf).
///
/// The one place the floor stays `Maybe` is a foreign class arm: an enum may
/// implement interfaces, and this module carries no hierarchy to rule that in
/// or out (ADR-0052 §2 "Unknown is-a keeps the arm").
fn subsumes_enum_case(a: &ContractTy, enum_fqn: &str, case: &str) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::Mixed | ContractTy::MixedMinus(_) | ContractTy::ObjectAny => Yes,
        ContractTy::Opaque => Maybe,
        ContractTy::EnumCase { enum_fqn: e, case: c } => {
            // Case names are class constants: PHP compares them case-sensitively.
            Certainty::from_bool(class_eq(e, enum_fqn) && c == case)
        }
        ContractTy::Class(n) => {
            if class_eq(n, enum_fqn) { Yes } else { Maybe }
        }
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(subsumes_enum_case(m, enum_fqn, case)))
        }
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(subsumes_enum_case(m, enum_fqn, case)))
        }
        _ => No,
    }
}

/// Whether `a` subsumes every resource. Exact, because a resource is a **leaf**
/// with no hierarchy to be unsure about — the only `Maybe` is what
/// [`ContractTy::Opaque`] forces.
///
/// Both cuts of `mixed` keep every resource: no resource is null, and every
/// resource is truthy — even a *closed* one (`fclose($h); (bool) $h === true`
/// at 8.5.9) — so `non-empty-mixed` covers the leaf exactly as `non-null-mixed`
/// does.
fn subsumes_resource(a: &ContractTy) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::Mixed | ContractTy::MixedMinus(_) | ContractTy::Resource => Yes,
        ContractTy::Opaque => Maybe,
        ContractTy::Union(members) => {
            members.iter().fold(No, |acc, m| acc.or(subsumes_resource(m)))
        }
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(subsumes_resource(m)))
        }
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

// ---------------------------------------------------------------------------
// The array vocabulary's structural denotation (ADR-0071). `subsumes_array` is
// to the five array arms what `subsumes_class` is to object arms: a rule set
// *inside* `subsumes`, whose leaf questions recurse through `subsumes` itself.
//
// Two soundness gates apply below: `Yes` only when provable (coverage over
// `b`'s whole denotation, `[]` and #14939 order-agnostic keyed realizations
// included); `No` only when refutable by a concrete witness, gated on the
// witness being realizable (`denotes_nothing`) since a `No` on an empty
// denotation is vacuous.
// ---------------------------------------------------------------------------

/// The empty array — ADR-0071 §2's most common witness, and the probe below.
fn empty_array() -> Val {
    Val::Array(Vec::new())
}

/// `a ⊇ b` for an array-vocabulary `b` (ADR-0071 §2.1). Two exact laws run
/// before the `a`-side dispatch, discharging every non-emptiness rule in the
/// ADR's tables (`covers_ne`) plus the degenerate-denotation guard:
///
/// 1. **`b ⊆ {[]}`** — `b`'s entry-bearing members provably don't exist
///    (`list<never>`, `array{a?: never}`), so the question collapses to "does
///    `a` admit `[]`"; uninhabited `b` is subsumed by everything.
/// 2. **the `[]` witness** — `[]` is in `b`, `a` provably rejects it
///    (`admits_val(·, [])` is exact both sides here), exactly ADR-0071's
///    `covers_ne` negated. Refutes `non-empty-array ⊇ array{a?: int}`,
///    `array{a: int} ⊇ array{a?: int}`, `associative-array<K,V> ⊇ array<K,V>`.
fn subsumes_array(a: &ContractTy, b: &ContractTy) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    let empty = empty_array();
    let b_empty = admits_val(b, &empty);

    // Law 1 — degenerate denotation.
    if at_most_empty(b) {
        return if b_empty.is_yes() { admits_val(a, &empty) } else { Yes };
    }
    // Law 2 — the `[]` witness.
    if b_empty.is_yes() && admits_val(a, &empty).is_no() {
        return No;
    }

    match a {
        // `mixed` covers everything; the null cut removes no array or Traversable.
        ContractTy::Mixed | ContractTy::MixedMinus(MixedCut::Null) => Yes,
        // Falsy cut removes only `[]`; law 2 already proved `b` non-empty here.
        ContractTy::MixedMinus(MixedCut::Falsy) => Yes,
        // `unset` admits no array, but claims nothing either (ADR-0087).
        ContractTy::Unset | ContractTy::Opaque => Maybe,
        // `callable` may be a method array (undecided); `*-closure` (ADR-0063 P3)
        // demands a `Closure` instance, which no array is — proven `No`.
        ContractTy::CallableTy { obl, .. } => {
            if obl.closure_only { No } else { Maybe }
        }
        // Joint-cover haircut (ADR-0071 §2): `list|non-empty-array` covers `array`
        // although neither member does, so an or-fold `No` needs every member
        // refusing for the same base reason (no array admitted at all).
        ContractTy::Union(members) => {
            let folded = members.iter().fold(No, |acc, m| acc.or(subsumes_array(m, b)));
            if folded.is_no() && !members.iter().all(array_incapable) { Maybe } else { folded }
        }
        // `and` is sound both ways: a witness one member rejects, the intersection rejects.
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(subsumes_array(m, b)))
        }
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. } => array_vs_array(a, b),
        // Array-incapable, and law 1 guarantees `b` holds one; keeps ADR-0038's bar.
        ContractTy::Never
        | ContractTy::Null
        | ContractTy::Base(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::StrOpaque
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_)
        | ContractTy::Class(_)
        | ContractTy::EnumCase { .. }
        | ContractTy::ObjectAny
        | ContractTy::Resource => No,
    }
}

/// Whether `t` admits **no array value at all** — the "base reason" ADR-0071 §2
/// requires before an `a`-side union fold may end at `No` (a union with even
/// one array-capable member may cover `b` jointly, e.g.
/// `list|non-empty-array ⊇ array`, so its fold's `No` is degraded to `Maybe`).
///
/// Shared with `admit.rs`: ADR-0072 §3 imports this verbatim for the
/// shape-fact face, one definition rather than two.
pub(crate) fn array_incapable(t: &ContractTy) -> bool {
    match t {
        ContractTy::Never
        | ContractTy::Null
        | ContractTy::Base(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::StrOpaque
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_)
        | ContractTy::Class(_)
        | ContractTy::EnumCase { .. }
        | ContractTy::ObjectAny
        | ContractTy::Resource => true,
        // Only the `*-closure` spellings refuse an array outright.
        ContractTy::CallableTy { obl, .. } => obl.closure_only,
        ContractTy::Union(m) => m.iter().all(array_incapable),
        ContractTy::Inter(m) => m.iter().any(array_incapable),
        _ => false,
    }
}

/// Whether some member of `t` is not provably absent — an over-approximation
/// of inhabitedness, gating every witness that needs a *value* to exist.
///
/// Exact for `never` and its algebraic closures; an intersection empty for an
/// unmodeled reason (`int&string`) is not detected (the one residual this
/// module accepts). The posture-independent core of [`provably_uninhabited`];
/// the array laws below need no oracle or posture, so they call this directly.
fn denotes_nothing(t: &ContractTy) -> bool {
    match t {
        ContractTy::Never => true,
        ContractTy::Union(m) => m.iter().all(denotes_nothing),
        ContractTy::Inter(m) => m.iter().any(denotes_nothing),
        _ => false,
    }
}

/// Whether `b`'s denotation provably holds **no value carrying an entry**, so
/// `b ⊆ {[]}` (law 1 of [`subsumes_array`]).
///
/// `array`/`non-empty-array` never qualify (`[0 => 0]` is always a member), and
/// neither does `iterable` — its `Traversable` members exist whatever the key
/// and value types say.
fn at_most_empty(b: &ContractTy) -> bool {
    match b {
        ContractTy::ListOf { elem, .. } => denotes_nothing(elem),
        ContractTy::MapOf { key, val, .. } => denotes_nothing(key) || denotes_nothing(val),
        ContractTy::Shape { .. } => shape_view(b).is_some_and(|v| {
            // A required field nothing can fill leaves the shape uninhabited.
            v.fields.iter().any(|f| !f.optional && denotes_nothing(&f.ty))
                // Otherwise: no field and no tail can ever contribute an entry.
                || (v.fields.iter().all(|f| denotes_nothing(&f.ty)) && !v.tail.carries_entries())
        }),
        _ => false,
    }
}

/// The extra-entry surface of a shape — what `shape_verdict` consults for a key
/// no field declares, as the three cases the type-vs-type rules branch on. A
/// typed tail whose value (or key) type denotes nothing admits no extra entry
/// and is therefore *sealed* — hence a computed view, not the raw `sealed` flag.
#[derive(Clone, Copy)]
enum Extras<'a> {
    /// No extra entry is ever admitted.
    Sealed,
    /// `...` with no type: any key, any value.
    Open,
    /// `...<K, V>` — `None` key means "any key".
    Typed(Option<&'a ContractTy>, &'a ContractTy),
}

impl Extras<'_> {
    /// Whether a member of this shape may carry an entry no field declares.
    fn carries_entries(self) -> bool {
        !matches!(self, Extras::Sealed)
    }
}

/// A shape arm read the way [`crate::shape_verdict`] reads it: fields, the
/// list/non-empty flags, and the computed [`Extras`] surface. `None` for a
/// non-shape arm, so no rule here can panic on a mis-dispatch.
struct ShapeView<'a> {
    list: bool,
    non_empty: bool,
    fields: &'a [CField],
    tail: Extras<'a>,
}

fn shape_view(ty: &ContractTy) -> Option<ShapeView<'_>> {
    let ContractTy::Shape { list, fields, sealed, non_empty, unsealed } = ty else {
        return None;
    };
    // Resolves tail before sealed, same order as `shape_verdict` — one reading.
    let tail = match unsealed {
        Some((k, v)) => {
            if denotes_nothing(v) || k.as_deref().is_some_and(denotes_nothing) {
                Extras::Sealed
            } else {
                Extras::Typed(k.as_deref(), v)
            }
        }
        None if *sealed => Extras::Sealed,
        None => Extras::Open,
    };
    Some(ShapeView { list: *list, non_empty: *non_empty, fields, tail })
}

/// `a ⊇ b` with both sides in the array vocabulary. Non-emptiness (`covers_ne`)
/// was already decided by [`subsumes_array`]'s two laws, so the rules here are
/// purely about keys, values and structure.
fn array_vs_array(a: &ContractTy, b: &ContractTy) -> Certainty {
    match b {
        ContractTy::ArrayAny { .. } => vs_array_any(a),
        ContractTy::ListOf { elem, .. } => vs_list(a, elem),
        ContractTy::MapOf { key, val, not_list, .. } => vs_map(a, key, val, *not_list),
        ContractTy::IterableOf { key, val } => vs_iterable(a, key, val),
        ContractTy::Shape { .. } => match shape_view(b) {
            Some(v) => vs_shape(a, &v),
            None => Certainty::Maybe,
        },
        _ => Certainty::Maybe,
    }
}

/// `b = array` / `non-empty-array`: **every** array (law 2 settled `[]`).
///
/// `No` witnesses: `['a' => 0]` is an array that is not a list, so no `list<T>`
/// and no `associative-array` — which rejects every list — and no positional
/// `list{…}` shape covers `b`. `Yes` needs `a` to accept every key *and* every
/// value, which for a keyed `a` means `array-key → mixed` with no `not_list` cut.
fn vs_array_any(a: &ContractTy) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::ArrayAny { .. } => Yes,
        ContractTy::ListOf { .. } => No,
        // `associative-array` rejects `[0 => 0]`, which `array` holds.
        ContractTy::MapOf { not_list: true, .. } => No,
        ContractTy::MapOf { key, val, .. } | ContractTy::IterableOf { key, val } => {
            if covers(key, &crate::array_key()) && covers(val, &ContractTy::Mixed) {
                Yes
            } else {
                Maybe
            }
        }
        ContractTy::Shape { .. } => match shape_view(a) {
            Some(v) => shape_vs_array_any(&v),
            None => Maybe,
        },
        _ => Maybe,
    }
}

/// A shape `a` against `b = array` (ADR-0071 §2.1, `ArrayAny`'s `Shape` row).
///
/// `No`: a positional `a` rejects `['a' => 0]`; a required field is missing
/// from the fresh-keyed `b`-member (`array` holds infinitely many keys `a`
/// doesn't require); `keys_prove_list` (issue #169) falls to the same witness.
/// `Yes`: a keyed shape with all-optional, all-`⊇ mixed` fields over an
/// extra-entry surface accepting every remaining key and value.
fn shape_vs_array_any(v: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    // Third disjunct is issue #169's No-sharpening: `['a' => 0]` is a member of
    // `b` (string-keyed, non-empty) that a `keys_prove_list` `a` (sealed tail,
    // possible keys ⊆ {0}) provably rejects.
    if v.list || v.fields.iter().any(|f| !f.optional) || keys_prove_list(v) {
        return No;
    }
    let fields_open = v.fields.iter().all(|f| covers(&f.ty, &ContractTy::Mixed));
    let tail_open = match v.tail {
        Extras::Open => true,
        Extras::Typed(k, val) => {
            k.is_none_or(|k| covers(k, &crate::array_key())) && covers(val, &ContractTy::Mixed)
        }
        Extras::Sealed => false,
    };
    if fields_open && tail_open { Yes } else { Maybe }
}

/// `b = list<T>` / `non-empty-list<T>`: keys `0..n-1`, unbounded length, `T`
/// inhabited (law 1) so a non-empty member is always available as a witness.
///
/// `Yes`: `array` covers every list; `list<T'>` iff `T' ⊇ T`; keyed `a` iff
/// its key contract holds every `int<0, max>` and value contract holds `T`.
/// `No`: `not_list` `a` rejects every member; a refusing key/value contract is
/// refuted by one member.
fn vs_list(a: &ContractTy, elem: &ContractTy) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::ArrayAny { .. } => Yes,
        ContractTy::ListOf { elem: e, .. } => subsumes(e, elem),
        // Every member of `b` is a list and `a` rejects every list.
        ContractTy::MapOf { not_list: true, .. } => No,
        ContractTy::MapOf { key, val, .. } | ContractTy::IterableOf { key, val } => {
            subsumes(key, &nonneg_int()).and(subsumes(val, elem))
        }
        ContractTy::Shape { .. } => match shape_view(a) {
            Some(v) => shape_vs_list(&v),
            None => Maybe,
        },
        _ => Maybe,
    }
}

/// A shape `a` against `b = list<T>` (ADR-0071 §2.1: `Maybe` in general).
///
/// `No`: a required **string** key admits no list; a *sealed* `a` bounds the
/// length while `b` holds longer lists. No `Yes` rule: a shape covers only
/// bounded lengths, never an unbounded `list<T>`.
fn shape_vs_list(v: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, No};
    if v.fields.iter().any(|f| !f.optional && matches!(f.key, CKey::Str(_))) {
        return No;
    }
    if matches!(v.tail, Extras::Sealed) {
        return No;
    }
    Maybe
}

/// `b = array<K', V'>` / `associative-array<K', V'>`, with `K'`/`V'` inhabited
/// (law 1).
///
/// `Yes`: key/value contracts each cover `b`'s, plus `not_list` — an
/// `associative-array` `a` covers `b` only when `b` itself has no list
/// realization. `No`: `list<T>` `a` refuted as soon as `K'` admits a
/// non-list-starting key; `not_list` `a` refuted by `[0 => v]` when `b` admits it.
fn vs_map(a: &ContractTy, key2: &ContractTy, val2: &ContractTy, not_list2: bool) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::ArrayAny { .. } => Yes,
        ContractTy::ListOf { .. } => {
            // A key neither `0` nor a list continuation makes it a non-list.
            let non_list_key = [Val::Int(1), Val::Str(PhpStr::from("k"))]
                .iter()
                .any(|k| admits_val(key2, k).is_yes());
            if non_list_key { No } else { Maybe }
        }
        ContractTy::MapOf { key, val, not_list, .. } => {
            let core = subsumes(key, key2).and(subsumes(val, val2));
            if *not_list && !not_list2 {
                match admits_val(key2, &Val::Int(0)) {
                    No => core, // `b` has no list realization: `a`'s cut is vacuous.
                    Yes => No,  // Witness `[0 => v]`: a member of `b`, rejected by `a`.
                    Maybe => core.and(Maybe),
                }
            } else {
                core
            }
        }
        ContractTy::IterableOf { key, val } => subsumes(key, key2).and(subsumes(val, val2)),
        // A shape bounds the key set; `array<K, V>` does not — the honest floor.
        _ => Maybe,
    }
}

/// `b = iterable<K', V'>` — arrays **plus** `Traversable` objects.
///
/// The `Traversable` member (ADR-0071 §2's witness) is admitted by no array
/// arm or shape, so all are proven `No`. Only another `iterable` can say
/// `Yes`, gated by an element-witness guard: `iterable<never, never>` still
/// denotes `[]` and objects, so element types alone give no `No` witness.
fn vs_iterable(a: &ContractTy, key2: &ContractTy, val2: &ContractTy) -> Certainty {
    use Certainty::{Maybe, No};
    match a {
        ContractTy::IterableOf { key, val } => {
            let c = subsumes(key, key2).and(subsumes(val, val2));
            let entry_witness = !denotes_nothing(key2) && !denotes_nothing(val2);
            if c.is_no() && !entry_witness { Maybe } else { c }
        }
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::Shape { .. } => No,
        _ => Maybe,
    }
}

/// `b = array{…}` / `list{…}` — the mining workhorse's right-hand side.
fn vs_shape(a: &ContractTy, bv: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, Yes};
    match a {
        // `array ⊇ array{…}`: every shape realization is an array (law 2 already
        // required `a` to admit `[]` wherever `b` does) — the row the 388 shaped
        // functionMap rows ride (ADR-0071 §1).
        ContractTy::ArrayAny { .. } => Yes,
        ContractTy::ListOf { elem, .. } => list_vs_shape(elem, bv),
        ContractTy::MapOf { key, val, not_list, .. } => {
            entries_vs_shape(key, val, *not_list, bv)
        }
        ContractTy::IterableOf { key, val } => entries_vs_shape(key, val, false, bv),
        ContractTy::Shape { .. } => match shape_view(a) {
            Some(av) => shape_vs_shape(&av, bv),
            None => Maybe,
        },
        _ => Maybe,
    }
}

/// `list<T> ⊇ array{…}` (ADR-0071 §2.1, `Shape`'s `ListOf` row).
///
/// `Yes`-eligibility is **denotational** (issue #161): `b` is positional
/// (`list{…}`), or its key structure alone proves every realization a list
/// ([`keys_prove_list`]) — keys decide, not the keyword, so `array{null}` and
/// `list{null}` get the same answer.
///
/// A keyed `b` that can hold two keys stays `Maybe`: `array{…}` keys are
/// order-agnostic (#14939), so `array{0: int, 1: string}` admits
/// `[1 => 's', 0 => 1]`, not a list. `No` when a required string key makes
/// every `b`-member a non-list.
fn list_vs_shape(elem: &ContractTy, bv: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    if bv.fields.iter().any(|f| !f.optional && matches!(f.key, CKey::Str(_))) {
        return No;
    }
    if !bv.list && !keys_prove_list(bv) {
        return Maybe;
    }
    let fields_ok = bv.fields.iter().all(|f| covers(elem, &f.ty));
    let tail_ok = match bv.tail {
        Extras::Sealed => true,
        Extras::Typed(_, v) => covers(elem, v),
        Extras::Open => false,
    };
    if fields_ok && tail_ok { Yes } else { Maybe }
}

/// Does the key structure alone prove every realization of the shape a list?
/// **Routed to the domain rather than re-derived** (issue #161): the key
/// skeleton goes through [`ShapeFact::normalize`], one definition of
/// list-ness. An unsealed tail is passed as the widest key class it could
/// admit; callers require a sealed tail for `Yes`, so widening it cannot
/// manufacture one.
fn keys_prove_list(bv: &ShapeView<'_>) -> bool {
    let fields = bv
        .fields
        .iter()
        .map(|f| {
            let key = match &f.key {
                CKey::Int(i) => Key::Int(*i),
                CKey::Str(s) => Key::Str(s.clone()),
            };
            let presence = if f.optional {
                Presence::Optional
            } else {
                Presence::Required { witnessed: false }
            };
            (key, presence, None)
        })
        .collect();
    let tail = match bv.tail {
        Extras::Sealed => Tail::Sealed,
        Extras::Open | Extras::Typed(..) => {
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None }
        }
    };
    ShapeFact::normalize(fields, tail, Certainty::Maybe, bv.non_empty, Vec::new())
        .is_list
        .is_yes()
}

/// `array<K, V>` / `iterable<K, V>` `⊇ array{…}` (ADR-0071 §2.1, `Shape`'s
/// `MapOf`/`IterableOf` row). Every declared key literal must be inside `K`,
/// every field type inside `V`, and `b`'s extra-entry surface covered: sealed
/// (nothing to cover), a typed tail `K`/`V` covers, or untyped-unsealed `b`
/// demands `K ⊇ array-key` and `V ⊇ mixed`. `not_list` needs a required
/// **string** key in `b`. `No` witnesses: a field whose key/type `a` refuses,
/// or (untyped-unsealed `b`) one concrete extra entry `a` refuses
/// ([`entry_refuted`]).
fn entries_vs_shape(
    key: &ContractTy,
    val: &ContractTy,
    not_list: bool,
    bv: &ShapeView<'_>,
) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    let realizable = all_fields_inhabited(bv);
    let mut verdict = Yes;
    for f in bv.fields {
        // A field nothing can fill is carried by no member, so it obliges nothing.
        if denotes_nothing(&f.ty) {
            continue;
        }
        verdict = verdict.and(subsumes(key, &key_ty(&f.key))).and(subsumes(val, &f.ty));
    }
    match bv.tail {
        Extras::Sealed => {}
        Extras::Typed(tk, tv) => {
            let tkey = tk.cloned().unwrap_or_else(crate::array_key);
            let c = subsumes(key, &tkey).and(subsumes(val, tv));
            // A tail key confined to declared fields carries no extra entry.
            let escapes = tk.is_none() || tail_key_escapes_fields(&tkey, bv.fields);
            verdict = verdict.and(if c.is_no() && !escapes { Maybe } else { c });
        }
        Extras::Open => {
            if covers(key, &crate::array_key()) && covers(val, &ContractTy::Mixed) {
                // Every conceivable extra entry is inside `a`.
            } else if !bv.list && entry_refuted(key, val, bv.fields) {
                return No;
            } else {
                verdict = verdict.and(Maybe);
            }
        }
    }
    if not_list {
        // `associative-array` rejects lists; only a required string key rules them out.
        let string_keyed = bv.fields.iter().any(|f| !f.optional && matches!(f.key, CKey::Str(_)));
        if !string_keyed {
            verdict = verdict.and(Maybe);
        }
    }
    // An unfillable field can make the refuting member unbuildable; stay honest.
    if verdict.is_no() && !realizable { Maybe } else { verdict }
}

/// The first integer key, and first `kN` string key, `fields` does not declare
/// — always exist (fields are finite) — the "fresh key" every extra-entry
/// witness in this module is built on.
fn free_keys(fields: &[CField]) -> (i64, PhpStr) {
    let mut i = 0i64;
    while fields.iter().any(|f| f.key == CKey::Int(i)) {
        i += 1;
    }
    let mut n = 0usize;
    let mut s = format!("k{n}");
    while fields.iter().any(|f| matches!(&f.key, CKey::Str(k) if *k == *s)) {
        n += 1;
        s = format!("k{n}");
    }
    (i, PhpStr::from(s))
}

/// Whether a typed tail can carry an entry whose key the shape's own fields do
/// **not** declare. A tail key wholly inside the declared keys contributes no
/// extra entry, so a `No` read off it would be vacuous. Probing two fresh keys
/// is a sound under-approximation: `false` only ever costs precision
/// (`Maybe` instead of `No`).
fn tail_key_escapes_fields(tk: &ContractTy, fields: &[CField]) -> bool {
    let (i, s) = free_keys(fields);
    admits_val(tk, &Val::Int(i)).is_yes() || admits_val(tk, &Val::Str(s)).is_yes()
}

/// A concrete extra entry that an untyped-unsealed `b` admits and that a
/// `array<K, V>`-shaped `a` provably refuses — the witness behind
/// `array<string, int> ⊉ array{a: int, ...}` (`['a' => 1, 0 => 1]` is a member
/// of `b` whose `int` key `a` refuses).
///
/// Probe key is the first integer no field declares (so the entry really is
/// extra); probe values are one member of each falsy corner of the value
/// domain, enough to refute any contract that does not accept `mixed`
/// outright. Only consulted for a keyed (`¬list`) `b`, whose extra entries are
/// unconstrained — a `list{…}` tail must continue the sequence.
fn entry_refuted(key: &ContractTy, val: &ContractTy, fields: &[CField]) -> bool {
    let (probe, _) = free_keys(fields);
    if admits_val(key, &Val::Int(probe)).is_no() {
        return true;
    }
    [Val::Null, Val::Int(0), Val::Str(PhpStr::new()), Val::Bool(false), Val::Array(Vec::new())]
        .iter()
        .any(|v| admits_val(val, v).is_no())
}

/// `array{…} ⊇ array{…}` — the four obligations of ADR-0071 §2.1's `Shape`
/// bullet:
/// 1. Every required `a` field is guaranteed by `b` (same-key required field,
///    `b.ty ⊆ a.ty`; else a lacking `b`-member refutes, [`shape_member_lacking`]).
/// 2. Every `b` field lands in `a` (same-key, typed tail, or untyped-unsealed);
///    sealed `a` refutes.
/// 3. `b`'s extra-entry surface is covered by `a`'s; untyped-unsealed `b`
///    against sealed `a` refutes, other mismatches stay `Maybe`.
/// 4. Flags: positional `a` over keyed `b` stays `Maybe` (#14939) unless `b`'s
///    keys prove a list ([`keys_prove_list`], issue #169) or a required
///    string key proves none is (`No`). Non-emptiness was law 2's.
fn shape_vs_shape(av: &ShapeView<'_>, bv: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    let realizable = all_fields_inhabited(bv);
    let mut verdict = Yes;

    // (1) `a`'s required fields.
    for af in av.fields.iter().filter(|f| !f.optional) {
        match bv.fields.iter().find(|bf| bf.key == af.key) {
            Some(bf) if !bf.optional => verdict = verdict.and(subsumes(&af.ty, &bf.ty)),
            _ => {
                if shape_member_lacking(bv, &af.key) {
                    return No;
                }
                verdict = verdict.and(Maybe);
            }
        }
    }

    // (2) `b`'s fields.
    for bf in bv.fields {
        if denotes_nothing(&bf.ty) {
            continue;
        }
        if let Some(af) = av.fields.iter().find(|af| af.key == bf.key) {
            verdict = verdict.and(subsumes(&af.ty, &bf.ty));
            continue;
        }
        match av.tail {
            Extras::Open => {}
            Extras::Typed(tk, tv) => {
                let tk = tk.cloned().unwrap_or_else(crate::array_key);
                verdict =
                    verdict.and(subsumes(&tk, &key_ty(&bf.key))).and(subsumes(tv, &bf.ty));
            }
            Extras::Sealed => {
                if all_fields_inhabited(bv) {
                    return No;
                }
                verdict = verdict.and(Maybe);
            }
        }
    }

    // (3) `b`'s extra-entry surface.
    match (bv.tail, av.tail) {
        (Extras::Sealed, _) | (_, Extras::Open) => {}
        (Extras::Typed(bk, bvt), Extras::Typed(ak, avt)) => {
            let ak = ak.cloned().unwrap_or_else(crate::array_key);
            let bk = bk.cloned().unwrap_or_else(crate::array_key);
            if !(covers(&ak, &bk) && covers(avt, bvt)) {
                verdict = verdict.and(Maybe);
            }
        }
        (Extras::Open, Extras::Typed(ak, avt)) => {
            let ak = ak.cloned().unwrap_or_else(crate::array_key);
            if !(covers(&ak, &crate::array_key()) && covers(avt, &ContractTy::Mixed)) {
                verdict = verdict.and(Maybe);
            }
        }
        (Extras::Open, Extras::Sealed) => {
            if all_fields_inhabited(bv) {
                return No;
            }
            verdict = verdict.and(Maybe);
        }
        (Extras::Typed(..), Extras::Sealed) => verdict = verdict.and(Maybe),
    }

    // (4) Flags. `keys_prove_list` (issue #169): keys alone proving a list satisfies `a`.
    if av.list && !bv.list && !keys_prove_list(bv) {
        if bv.fields.iter().any(|f| !f.optional && matches!(f.key, CKey::Str(_))) {
            return No;
        }
        verdict = verdict.and(Maybe);
    }

    // An unfillable field can make the refuting member unbuildable; stay honest.
    if verdict.is_no() && !realizable { Maybe } else { verdict }
}

/// Whether every declared field of a shape can be filled at all — gates any
/// witness that has to *build* a member of `b` (a `list{…}` member holding key
/// `n` needs `0..n` filled, so one uninhabited field can make it unbuildable).
fn all_fields_inhabited(v: &ShapeView<'_>) -> bool {
    v.fields.iter().all(|f| !denotes_nothing(&f.ty))
}

/// Whether `b` provably has a member that does **not** carry `key` — the
/// witness behind obligation 1 of [`shape_vs_shape`]. A required `b` field of
/// that key means every member carries it (no witness); otherwise the
/// required-fields-only member lacks it and exists, unless `b` is `non-empty`
/// and no other field/tail can supply an entry (then stay honest).
fn shape_member_lacking(v: &ShapeView<'_>, key: &CKey) -> bool {
    if v.fields.iter().any(|f| !f.optional && f.key == *key) {
        return false;
    }
    if !v.non_empty {
        return true;
    }
    v.fields.iter().any(|f| !f.optional)
        || v.tail.carries_entries()
        || v.fields.iter().any(|f| f.optional && f.key != *key && !denotes_nothing(&f.ty))
}

/// The one-value contract a declared shape key denotes, so recurses through [`subsumes`].
fn key_ty(k: &CKey) -> ContractTy {
    match k {
        CKey::Int(i) => ContractTy::LitInt(*i),
        CKey::Str(s) => ContractTy::LitStr(s.clone()),
    }
}

/// `int<0, max>` — every key a list can have.
fn nonneg_int() -> ContractTy {
    ContractTy::IntIn(IntRange::NON_NEGATIVE)
}

/// The proof-strength half of [`subsumes`], for rules needing "provably covers".
fn covers(outer: &ContractTy, inner: &ContractTy) -> bool {
    subsumes(outer, inner).is_yes()
}

/// Semantic type equality (ADR-0030 registry entry 5): mutual subsumption
/// (Yes/Yes) over extensional arms. Two provenance-flavored arms can never be
/// judged equal — the intended undecidability.
#[must_use]
pub fn arm_eq(a: &ContractTy, b: &ContractTy) -> bool {
    subsumes(a, b).is_yes() && subsumes(b, a).is_yes()
}

/// Remove arms that another surviving arm subsumes with [`Certainty::Yes`],
/// preserving stable order; mutually-subsuming (`arm_eq`) duplicates keep
/// their **first** occurrence. Survivors then run to an interval-absorption
/// fixpoint ([`merge_int_arms`]): `int<1, max>|0` and `int<0, max>` are one
/// denotation spelled two ways that subsumption dedup alone cannot collapse
/// (neither arm covers the other) — this pass picks the interval (issue #90).
pub fn dedup_arms(arms: &mut Vec<ContractTy>) {
    let mut kept: Vec<ContractTy> = Vec::with_capacity(arms.len());
    for arm in arms.drain(..) {
        if kept.iter().any(|k| subsumes(k, &arm).is_yes()) {
            continue;
        }
        // The wider survivor may subsume earlier-kept arms: eliminate both ways.
        kept.retain(|k| !subsumes(&arm, k).is_yes());
        kept.push(arm);
    }
    absorb_int_arms(&mut kept);
    *arms = kept;
}

/// Run an arm list to the [`merge_int_arms`] fixpoint in place; a merged pair
/// takes the **earlier** arm's slot, keeping declaration order. Iterating
/// matters (`int<2, max>`, `1`, `0` merges twice, to `int<0, max>`) and
/// terminates since every merge removes an arm.
fn absorb_int_arms(arms: &mut Vec<ContractTy>) {
    loop {
        let mut merged_at: Option<(usize, usize, ContractTy)> = None;
        'outer: for i in 0..arms.len() {
            for j in (i + 1)..arms.len() {
                if let Some(m) = merge_int_arms(&arms[i], &arms[j]) {
                    merged_at = Some((i, j, m));
                    break 'outer;
                }
            }
        }
        let Some((i, j, m)) = merged_at else { return };
        arms[i] = m;
        arms.remove(j);
    }
}

/// The one **denotation-preserving** int-arm merge (issue #90): when the union
/// of two int-flavored arms IS an interval, that interval; `None` otherwise.
/// Exactly three shapes qualify, symmetric in the arguments:
/// * `LitInt(n)` + `IntIn(lo, hi)` with `n == lo - 1` → `IntIn(n, hi)`;
/// * `LitInt(n)` + `IntIn(lo, hi)` with `n == hi + 1` → `IntIn(lo, n)`;
/// * `IntIn(a, b)` + `IntIn(c, d)` that overlap **or touch** → their hull.
///
/// Every other pair is refused: a **gap** is never bridged (`1|int<3, max>`
/// stays two arms), and an **interior** literal never reaches here (already
/// dropped by [`dedup_arms`]'s subsumption pass) but would be refused anyway —
/// the trap is closed twice over. Boundary arithmetic is checked, not
/// wrapped: `hi + 1` at `i64::MAX` / `lo - 1` at `i64::MIN` return `None`.
#[must_use]
pub fn merge_int_arms(a: &ContractTy, b: &ContractTy) -> Option<ContractTy> {
    match (a, b) {
        (ContractTy::LitInt(n), ContractTy::IntIn(r))
        | (ContractTy::IntIn(r), ContractTy::LitInt(n)) => {
            let extended = if r.lo().checked_sub(1) == Some(*n) {
                IntRange::new(*n, r.hi())
            } else if r.hi().checked_add(1) == Some(*n) {
                IntRange::new(r.lo(), *n)
            } else {
                None
            };
            extended.map(ContractTy::IntIn)
        }
        (ContractTy::IntIn(x), ContractTy::IntIn(y)) => {
            // Touching/overlapping ⟺ neither sits beyond the other's successor
            // (an open `max` end is never below another arm's `lo`).
            let touches = |p: IntRange, q: IntRange| match p.hi().checked_add(1) {
                Some(next) => q.lo() <= next,
                None => true,
            };
            (touches(*x, *y) && touches(*y, *x)).then(|| ContractTy::IntIn(x.hull(*y)))
        }
        _ => None,
    }
}

/// The value-set → canonical normal-form (arm list) half of the extraction
/// (ADR-0052 §4). Sorts, dedups, and collapses literal groups into their
/// predicate class (numeric → `numeric-string`, bool pair → `bool`,
/// null-fold). `None` on a non-scalar-bearing set, matching
/// `render_value_domain`'s refusal.
///
/// **Seam:** produces the *semantic* arm list only — literal-safety fallback,
/// CAP-bounded spelling, quoting, and member order stay rendering policy in
/// `steins-edit`. An all-numeric string group with ≥ 2 distinct members is
/// `numeric-string` (ADR-0037), one [`ContractTy::StrWith`] arm; every other
/// group returns as sorted [`ContractTy::LitStr`] arms.
#[must_use]
pub fn summarize_vals(vals: &[Val]) -> Option<Vec<ContractTy>> {
    // A non-scalar member has no faithful scalar spelling (today's refusal).
    if vals.iter().any(|v| matches!(v, Val::Array(_))) {
        return None;
    }

    let mut sorted: Vec<Val> = vals.to_vec();
    sorted.sort();
    sorted.dedup();

    let mut has_int = false;
    let mut has_float = false;
    let mut has_true = false;
    let mut has_false = false;
    let mut has_null = false;
    let mut strings: Vec<&PhpStr> = Vec::new();
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

    // Canonical order (int, float, string(s), bool, null) matches the renderer's.
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

    // Empty ⟺ input was empty; matches `nullable.then(|| "null")` / `None` split.
    if arms.is_empty() { None } else { Some(arms) }
}

/// Canonicalize a string value group into arms. The only *semantic* computed
/// collapse (not spelling policy) is the numeric-string class: ≥ 2 distinct
/// all-numeric members are the canonical `numeric-string` predicate class
/// (ADR-0037), collapsing to one [`ContractTy::StrWith`] arm. Every other group
/// returns as distinct-sorted literal arms — the renderer owns spelling
/// (safety, CAP).
fn summarize_string_group(strings: &[&PhpStr]) -> Vec<ContractTy> {
    if strings.is_empty() {
        return Vec::new();
    }
    let mut distinct: Vec<&PhpStr> = strings.to_vec();
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
    distinct.into_iter().map(|s| ContractTy::LitStr((*s).clone())).collect()
}

/// Subtract a guard's negative information from an arm list, arm-wise
/// (ADR-0052 §2), by each arm's [`subtract_arm`] fate: an arm dies iff the
/// subtrahend subsumes it with [`Certainty::Yes`] (`Maybe` keeps it), except
/// for the two partial deletions [`subtract_arm`] documents — a
/// [`ContractTy::IntIn`] arm minus one of its own **endpoints** shrinks by one,
/// and a [`ContractTy::Base`]`(`[`Base::Bool`]`)` arm minus one of its two
/// **literals** narrows to the other (issue #443) — instead of surviving
/// whole. An emptied list is left empty — the caller drops it to no-fact (the
/// verdict owns death, ADR-0052 §2).
pub fn subtract(arms: &mut Vec<ContractTy>, sub: &Subtrahend, oracle: &dyn IsaOracle) {
    arms.retain_mut(|arm| match subtract_arm(sub, arm, oracle) {
        ArmFate::Survives => true,
        ArmFate::Dies => false,
        ArmFate::Narrows(narrowed) => {
            *arm = narrowed;
            true
        }
    });
}

/// One arm's fate under a subtrahend — the per-arm judgment [`subtract`] runs.
/// Public (with [`subtract_arm`]) so a caller carrying a **parallel** per-arm
/// structure (steins-infer's stratified contract lane) can map arms in
/// lockstep with the same judgment, no second copy of the polarity/endpoint law.
#[derive(Debug, Clone, PartialEq)]
pub enum ArmFate {
    /// The subtrahend does not provably cover the arm — it survives whole.
    Survives,
    /// The subtrahend covers the whole arm — it is deleted.
    Dies,
    /// The subtrahend removes an endpoint — the arm shrinks (the one partial deletion).
    Narrows(ContractTy),
}

/// The fate of `arm` under `sub` (ADR-0052 §2): [`ArmFate::Dies`] iff
/// [`subtrahend_covers`] answers [`Certainty::Yes`], plus the two **partial**
/// deletions the arm vocabulary can spell back:
///
/// - a [`ContractTy::IntIn`] arm minus one of its own endpoints shrinks by one
///   (`int<lo, hi>` less `lo` is `int<lo+1, hi>`; a two-point interval
///   collapses to the surviving literal; the point interval dies). An
///   **interior** point must not split the interval — no arm can spell the
///   gap — so it survives whole.
/// - a [`ContractTy::Base`]`(`[`Base::Bool`]`)` arm minus one of its two
///   literals narrows to the other (issue #443): unlike an interval, `bool`
///   has no interior point to protect, so every non-covering subtrahend of
///   this arm is one of its exactly two members and the narrowing is total,
///   never a survival.
#[must_use]
pub fn subtract_arm(sub: &Subtrahend, arm: &ContractTy, oracle: &dyn IsaOracle) -> ArmFate {
    if let (Subtrahend::Value(Val::Int(n)), ContractTy::IntIn(r)) = (sub, arm) {
        return clip_int_endpoint(*n, *r);
    }
    if let (Subtrahend::Value(Val::Bool(b)), ContractTy::Base(Base::Bool)) = (sub, arm) {
        return ArmFate::Narrows(ContractTy::LitBool(!b));
    }
    if subtrahend_covers(sub, arm, oracle).is_yes() { ArmFate::Dies } else { ArmFate::Survives }
}

/// An interval minus one point: the point interval dies, an endpoint clips off,
/// an interior (or outside) point changes nothing. The point-interval case is
/// decided first, so `lo + 1` / `hi - 1` below runs only on a multi-point
/// interval and cannot leave the i64 domain; [`IntRange::new`]'s `Option`
/// still backstops the arithmetic.
fn clip_int_endpoint(n: i64, r: IntRange) -> ArmFate {
    if r.lo() == r.hi() {
        return if n == r.lo() { ArmFate::Dies } else { ArmFate::Survives };
    }
    let clipped = if n == r.lo() {
        IntRange::new(r.lo() + 1, r.hi())
    } else if n == r.hi() {
        IntRange::new(r.lo(), r.hi() - 1)
    } else {
        return ArmFate::Survives;
    };
    match clipped {
        Some(c) if c.lo() == c.hi() => ArmFate::Narrows(ContractTy::LitInt(c.lo())),
        Some(c) => ArmFate::Narrows(ContractTy::IntIn(c)),
        None => ArmFate::Dies,
    }
}

/// The [`Certainty`] that the subtrahend's denotation covers the whole arm.
/// `Null`/`Value`/`Base` reduce to a [`ContractTy`] and reuse [`subsumes`];
/// the class subtrahend carries the polarity asymmetry via the is-a `oracle`.
#[must_use]
pub fn subtrahend_covers(sub: &Subtrahend, arm: &ContractTy, oracle: &dyn IsaOracle) -> Certainty {
    match sub {
        Subtrahend::Null => subsumes(&ContractTy::Null, arm),
        Subtrahend::Value(v) => subsumes(&val_contract(v), arm),
        Subtrahend::Base(b) => subsumes(&ContractTy::Base(*b), arm),
        Subtrahend::Class { fqn, polarity } => class_covers(fqn, *polarity, arm, oracle),
        Subtrahend::EnumCase { enum_fqn, case, polarity } => {
            enum_case_covers(enum_fqn, case, *polarity, arm, oracle)
        }
        Subtrahend::Falsy => falsy_covers(arm),
    }
}

/// Whether every value the arm admits is falsy (issue #557) — the judgment
/// [`Subtrahend::Falsy`] deletes an arm on.
///
/// The question is decidable over the whole arm vocabulary without a `Maybe`,
/// and the reason is worth stating: an arm answers [`Certainty::No`] the moment
/// it admits one truthy value, and every arm here that is not one of the falsy
/// literal shapes below does. Objects are the case that looks like it should be
/// undecidable and is not — PHP judges every object truthy, whatever its class,
/// so an unresolvable class arm is still provably not covered. `Opaque` and
/// `Mixed` admit truthy values by construction.
///
/// `Never` is the one arm this answers `No` about on a technicality: it is
/// vacuously all-falsy, but it is also vacuously all-*anything*, and deleting
/// an uninhabited arm buys nothing worth a special case.
fn falsy_covers(arm: &ContractTy) -> Certainty {
    use Certainty::{No, Yes};
    let falsy = match arm {
        ContractTy::Null => true,
        ContractTy::LitBool(b) => !b,
        ContractTy::LitInt(n) => *n == 0,
        ContractTy::LitFloat(f) => *f == 0.0,
        ContractTy::LitStr(s) => php_is_falsy(&Val::Str(s.clone())),
        // A point interval at zero is `0` under another spelling. Any wider
        // interval admits a non-zero int and is therefore not covered — the
        // clipping an interval gets from a *value* subtrahend has no analogue
        // here, because the falsy set removes an interior point of every
        // interval that straddles zero and no arm can spell that gap.
        ContractTy::IntIn(r) => r.lo() == 0 && r.hi() == 0,
        _ => false,
    };
    if falsy { Yes } else { No }
}

/// The enum-case polarity asymmetry (issue #429), the [`class_covers`] mirror
/// for a subtrahend that is one *value*.
///
/// - **Negative** (`$s !== E::C`, subtrahend = the single value `E::C`): the arm
///   dies only if the arm IS that value, which [`subsumes`] decides exactly. A
///   `Class(E)` arm — an enum whose declaration never got expanded — survives,
///   because one case is not the whole enum.
/// - **Positive** (`$s === E::C`, subtrahend = every value other than `E::C`):
///   the arm dies iff it provably cannot hold `E::C`. For an enum-case arm that
///   is exact; for a class arm it is `is_a(E, M) = No` — and **no finality
///   question arises**, unlike [`class_covers`], because the subtrahend removes
///   a single value rather than a whole class extent: whether `M` has unseen
///   descendants cannot change whether `E::C` is one of `M`'s instances. A
///   scalar/null/array arm holds no object at all and dies; `object`/`mixed`/
///   `Opaque` survive.
fn enum_case_covers(
    enum_fqn: &str,
    case: &str,
    polarity: bool,
    arm: &ContractTy,
    oracle: &dyn IsaOracle,
) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    let value = ContractTy::EnumCase { enum_fqn: enum_fqn.to_owned(), case: case.to_owned() };
    if !polarity {
        return subsumes(&value, arm);
    }
    match arm {
        // Exact: the arm is one value, so it dies iff it is a DIFFERENT one.
        ContractTy::EnumCase { .. } => Certainty::from_bool(!subsumes(&value, arm).is_yes()),
        ContractTy::Class(m) => {
            if oracle.is_a(enum_fqn, m) == No { Yes } else { Maybe }
        }
        // Object-capable, and nothing here rules `E::C` in or out.
        ContractTy::Mixed
        | ContractTy::MixedMinus(_)
        | ContractTy::ObjectAny
        | ContractTy::Opaque
        // An enum may declare `__invoke`, so a `callable` arm may hold a case.
        | ContractTy::CallableTy { .. } => Maybe,
        // Object-incapable: no value of the arm is any enum case.
        ContractTy::Never
        | ContractTy::Null
        | ContractTy::Base(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::StrOpaque
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_)
        | ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. }
        | ContractTy::Resource
        | ContractTy::Unset => Yes,
        // A union is covered only if every member is; an intersection as soon as
        // one member is.
        ContractTy::Union(members) => Certainty::all_of(
            members.iter().map(|m| enum_case_covers(enum_fqn, case, polarity, m, oracle)),
        ),
        ContractTy::Inter(members) => {
            if members
                .iter()
                .any(|m| enum_case_covers(enum_fqn, case, polarity, m, oracle).is_yes())
            {
                Yes
            } else {
                Maybe
            }
        }
    }
}

/// The class-arm polarity asymmetry (ADR-0052 §2), judged against the real
/// is-a `oracle` (project hierarchy + A11 demotion arrive via the caller).
///
/// - **Negative** (subtrahend = instances of T): `M` dies iff
///   `is_a(M, T) = Yes` (is-a is inherited); `No`/`Unknown` keeps it; a
///   non-object arm survives.
/// - **Positive** (subtrahend = non-instances of T): `M` dies only when
///   `final`/enum **and** `is_a(M, T) = No` (an open class could still
///   implement `T`, so `Maybe`, as does `Unknown`); a scalar/null/array arm
///   dies; a bare `object`/`Opaque`/`mixed` arm survives.
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
        // Subtrahend = instances of T. is_a(M, T): Yes deletes; No/Maybe keep.
        match arm {
            ContractTy::Class(m) => oracle.is_a(m, fqn),
            ContractTy::ObjectAny | ContractTy::Opaque | ContractTy::Mixed => Maybe,
            _ => No,
        }
    }
}

/// The literal contract that denotes exactly one value (for the `Value`
/// subtrahend). An array value has no scalar-literal arm, so it lowers to the
/// unknown `Opaque` — subtracting it covers nothing (sound: no array subtracted).
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
        ContractTy::LitStr(s.into())
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
        // numeric ⇒ non-empty.
        assert_eq!(subsumes(&non_empty, &numeric), Certainty::Yes);
        // '' is not numeric.
        assert_eq!(subsumes(&numeric, &lit_s("")), Certainty::No);
        assert_eq!(subsumes(&numeric, &lit_s("123")), Certainty::Yes);
    }

    #[test]
    fn union_subsumes_each_member() {
        let u = ContractTy::Union(vec![ContractTy::Base(Base::Int), ContractTy::Base(Base::String)]);
        assert_eq!(subsumes(&u, &lit_i(1)), Certainty::Yes);
        assert_eq!(subsumes(&u, &lit_s("x")), Certainty::Yes);
        // `a` must subsume EVERY arm of a union `b`; bool arm uncovered → Maybe.
        assert_eq!(
            subsumes(
                &u,
                &ContractTy::Union(vec![lit_i(1), ContractTy::Base(Base::Bool)])
            ),
            Certainty::Maybe
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
        assert_eq!(subsumes(&ContractTy::ObjectAny, &class("user")), Certainty::Yes);
        assert_eq!(subsumes(&ContractTy::Base(Base::Int), &class("user")), Certainty::No);
    }

    #[test]
    fn provenance_arm_never_decides_yes() {
        // `StrOpaque` (literal-string and kin) is barred from Yes on either side.
        assert_ne!(subsumes(&ContractTy::StrOpaque, &lit_s("x")), Certainty::Yes);
        assert_ne!(subsumes(&lit_s("x"), &ContractTy::StrOpaque), Certainty::Yes);
        assert!(!arm_eq(&ContractTy::StrOpaque, &ContractTy::StrOpaque));
    }

    // ---- subsumes: the array vocabulary (ADR-0071) ---------------------------

    /// Array tests are written in phpdoc source and lowered, keeping
    /// `array{dirname: string, basename: string}` readable as itself. Panics
    /// on an unlowerable spelling — a test bug, never a silent skip.
    fn ty(src: &str) -> ContractTy {
        crate::lower_str(src).unwrap_or_else(|| panic!("{src:?} must lower"))
    }

    #[test]
    fn lowered_array_spellings_are_what_the_rules_assume() {
        // Sanity pins: a failing rule test below is a rule bug, not a lowering surprise.
        assert_eq!(ty("array"), ContractTy::ArrayAny { non_empty: false });
        assert_eq!(ty("non-empty-array"), ContractTy::ArrayAny { non_empty: true });
        assert_eq!(
            ty("array{a: int}"),
            ContractTy::Shape {
                list: false,
                fields: vec![CField {
                    key: CKey::Str("a".into()),
                    optional: false,
                    ty: ContractTy::Base(Base::Int),
                }],
                sealed: true,
                non_empty: false,
                unsealed: None,
            }
        );
        // `array{}` is the degenerate sealed shape: no fields, nothing extra.
        assert_eq!(
            ty("array{}"),
            ContractTy::Shape {
                list: false,
                fields: Vec::new(),
                sealed: true,
                non_empty: false,
                unsealed: None,
            }
        );
        // `...` is unsealed-untyped (no tail contract), NOT a sealed shape.
        assert!(matches!(
            ty("array{a: int, ...}"),
            ContractTy::Shape { sealed: false, unsealed: None, .. }
        ));
        assert!(matches!(
            ty("array{a: int, ...<string, int>}"),
            ContractTy::Shape { sealed: false, unsealed: Some(_), .. }
        ));
    }

    // -- Yes: the mining workhorses (ADR-0071 §1) --

    #[test]
    fn array_subsumes_every_shape() {
        // The row the 388 shaped functionMap rows ride.
        assert_eq!(
            subsumes(&ty("array"), &ty("array{dirname: string, basename: string}")),
            Certainty::Yes
        );
    }

    #[test]
    fn keyed_array_subsumes_a_sealed_shape_it_covers() {
        // `array<string, mixed>` holds every string-keyed array; sealed `b` declares less.
        assert_eq!(subsumes(&ty("array<string, mixed>"), &ty("array{a: int}")), Certainty::Yes);
    }

    #[test]
    fn iterable_and_keyed_array_subsume_a_list() {
        // A list's keys are `int<0, max>` and its values the element type.
        assert_eq!(subsumes(&ty("iterable<int, int>"), &ty("list<int>")), Certainty::Yes);
        assert_eq!(subsumes(&ty("array<int, string>"), &ty("list<string>")), Certainty::Yes);
    }

    #[test]
    fn list_subsumes_a_narrower_non_empty_list() {
        // Element verdicts compose; dropping the non-empty guarantee only widens.
        assert_eq!(
            subsumes(&ty("list<string>"), &ty("non-empty-list<non-falsy-string>")),
            Certainty::Yes
        );
    }

    // -- No: every wrong Yes has a witness that kills it --

    #[test]
    fn list_does_not_subsume_a_keyed_array() {
        // Witness `[1 => 1]`: a member of `array<int, int>` that is not a list.
        assert_eq!(subsumes(&ty("list<int>"), &ty("array<int, int>")), Certainty::No);
    }

    #[test]
    fn keyed_array_does_not_subsume_an_untyped_unsealed_shape() {
        // `array{a: int, ...}` admits `['a' => 1, 0 => 1]`; `int` key 0 is outside.
        assert_eq!(subsumes(&ty("array<string, int>"), &ty("array{a: int, ...}")), Certainty::No);
    }

    #[test]
    fn non_empty_array_does_not_subsume_an_all_optional_shape() {
        // Witness `[]`: `array{a?: int}` admits it, `non-empty-array` does not.
        assert_eq!(subsumes(&ty("non-empty-array"), &ty("array{a?: int}")), Certainty::No);
    }

    #[test]
    fn required_field_shape_does_not_subsume_its_optional_twin() {
        // The same `[]` witness, with the required field as the rejecting side.
        assert_eq!(subsumes(&ty("array{a: int}"), &ty("array{a?: int}")), Certainty::No);
    }

    #[test]
    fn no_array_arm_subsumes_iterable() {
        // `iterable` holds `Traversable` objects, which no array type admits.
        assert_eq!(subsumes(&ty("array"), &ty("iterable<int>")), Certainty::No);
        assert_eq!(subsumes(&ty("array<int, int>"), &ty("iterable<int, int>")), Certainty::No);
        assert_eq!(subsumes(&ty("array{a: int}"), &ty("iterable<string, int>")), Certainty::No);
    }

    #[test]
    fn list_does_not_subsume_a_keyed_shape_with_list_keys() {
        // #14939: `array{0: int, 1: int}` admits `[1 => 1, 0 => 1]`, not a list —
        // but not refutable either (the in-order realization IS a list).
        assert_eq!(subsumes(&ty("list<int>"), &ty("array{0: int, 1: int}")), Certainty::Maybe);
    }

    #[test]
    fn list_acceptance_does_not_depend_on_the_spelling_of_a_proven_key_set() {
        // Issue #161: a sealed shape whose only key is `0` has no order-agnostic
        // realization, so `array{null}`/`list{null}` must get the same (Yes) verdict.
        for a in ["list<string|null>", "non-empty-list<string|null>"] {
            let keyed = subsumes(&ty(a), &ty("array{null}"));
            let positional = subsumes(&ty(a), &ty("list{null}"));
            assert_eq!(keyed, positional, "{a}: the verdict read the keyword, not the keys");
            assert_eq!(keyed, Certainty::Yes, "{a} must accept the single-key-0 sealed shape");
        }
        // The optional twin realizes `[]` and `[0 => v]` — both lists.
        assert_eq!(subsumes(&ty("list<int>"), &ty("array{0?: int}")), Certainty::Yes);
    }

    #[test]
    fn list_does_not_accept_a_shape_admitting_a_gapped_key_set() {
        // Unsound direction of #161: optional keys admit gapped realizations
        // (`[0 => 1, 2 => 1]` fails `array_is_list`), so `Yes` would be wrong.
        assert_eq!(
            subsumes(&ty("list<int>"), &ty("array{0?: int, 1?: int, 2: int}")),
            Certainty::Maybe
        );
        assert_eq!(subsumes(&ty("list<int>"), &ty("array{0?: int, 2?: int}")), Certainty::Maybe);
        // Two optional keys with no gap still admit `{1}` alone (`[1 => 1]`).
        assert_eq!(subsumes(&ty("list<int>"), &ty("array{0?: int, 1?: int}")), Certainty::Maybe);
    }

    #[test]
    fn list_acceptance_yes_agrees_with_the_domain_is_list_judgment() {
        // Eligibility is ROUTED through `ShapeFact::normalize`; this matrix walks
        // keyed spellings through both ends so the conversion cannot drift.
        // `list{…}` spellings excluded by design: the keyword adds order info.
        // (spelling, [(int key, required)], sealed)
        type MatrixCase = (&'static str, &'static [(i64, bool)], bool);
        let cases: &[MatrixCase] = &[
            ("array{null}", &[(0, true)], true),
            ("array{0?: null}", &[(0, false)], true),
            ("array{0: null, 1: null}", &[(0, true), (1, true)], true),
            ("array{0?: null, 1?: null}", &[(0, false), (1, false)], true),
            ("array{0?: null, 1?: null, 2: null}", &[(0, false), (1, false), (2, true)], true),
            ("array{1: null}", &[(1, true)], true),
            ("array{null, ...}", &[(0, true)], false),
            ("array{null, ...<int, null>}", &[(0, true)], false),
        ];
        for (src, keys, sealed) in cases {
            let fields = keys
                .iter()
                .map(|(k, required)| {
                    let presence = if *required {
                        Presence::Required { witnessed: false }
                    } else {
                        Presence::Optional
                    };
                    (Key::Int(*k), presence, None)
                })
                .collect();
            let tail = if *sealed {
                Tail::Sealed
            } else {
                Tail::Unsealed { key: KeyClass::ArrayKey, value: None }
            };
            let domain =
                ShapeFact::normalize(fields, tail, Certainty::Maybe, false, Vec::new()).is_list;
            let accepted = subsumes(&ty("list<null>"), &ty(src));
            assert_eq!(
                accepted.is_yes(),
                domain.is_yes(),
                "{src}: acceptance {accepted:?} disagrees with domain is_list {domain:?}"
            );
        }
    }

    #[test]
    fn positional_shape_acceptance_does_not_depend_on_the_spelling_of_a_proven_key_set() {
        // Issue #169: a sealed subject whose only key is `0` is a sequence
        // whatever keyword introduced it — same (Yes) verdict both spellings.
        for a in ["list{string|null}", "list{0?: string|null}"] {
            let keyed = subsumes(&ty(a), &ty("array{null}"));
            let positional = subsumes(&ty(a), &ty("list{null}"));
            assert_eq!(keyed, positional, "{a}: the verdict read the keyword, not the keys");
            assert_eq!(keyed, Certainty::Yes, "{a} must accept the single-key-0 sealed shape");
        }
        // Optional twin realizes `[]`/`[0 => v]`, both admitted by an all-optional acceptor.
        assert_eq!(subsumes(&ty("list{0?: int}"), &ty("array{0?: int}")), Certainty::Yes);
    }

    #[test]
    fn positional_shape_does_not_accept_a_subject_admitting_a_gapped_key_set() {
        // Unsound direction of #169, mirroring #161: subjects admit a permuted
        // or gapped realization, so `Yes` under a positional acceptor is wrong.
        assert_eq!(
            subsumes(&ty("list{int, int}"), &ty("array{0: int, 1: int}")),
            Certainty::Maybe
        );
        assert_eq!(
            subsumes(
                &ty("list{0?: int, 1?: int, 2?: int}"),
                &ty("array{0?: int, 1?: int, 2: int}")
            ),
            Certainty::Maybe
        );
        // Two optional keys with no gap still admit `{1}` alone (`[1 => 1]`).
        assert_eq!(
            subsumes(&ty("list{0?: int, 1?: int}"), &ty("array{0?: int, 1?: int}")),
            Certainty::Maybe
        );
    }

    #[test]
    fn positional_shape_acceptance_yes_agrees_with_the_domain_is_list_judgment() {
        // Same drift-guard as #161's matrix: the acceptor's optional fields cover
        // every subject field, so the domain's list judgment is the only discriminator.
        // (spelling, [(int key, required)], sealed)
        type MatrixCase = (&'static str, &'static [(i64, bool)], bool);
        let cases: &[MatrixCase] = &[
            ("array{null}", &[(0, true)], true),
            ("array{0?: null}", &[(0, false)], true),
            ("array{0: null, 1: null}", &[(0, true), (1, true)], true),
            ("array{0?: null, 1?: null}", &[(0, false), (1, false)], true),
            ("array{0?: null, 1?: null, 2: null}", &[(0, false), (1, false), (2, true)], true),
            ("array{1: null}", &[(1, true)], true),
            ("array{null, ...}", &[(0, true)], false),
            ("array{null, ...<int, null>}", &[(0, true)], false),
        ];
        for (src, keys, sealed) in cases {
            let fields = keys
                .iter()
                .map(|(k, required)| {
                    let presence = if *required {
                        Presence::Required { witnessed: false }
                    } else {
                        Presence::Optional
                    };
                    (Key::Int(*k), presence, None)
                })
                .collect();
            let tail = if *sealed {
                Tail::Sealed
            } else {
                Tail::Unsealed { key: KeyClass::ArrayKey, value: None }
            };
            let domain =
                ShapeFact::normalize(fields, tail, Certainty::Maybe, false, Vec::new()).is_list;
            let accepted = subsumes(&ty("list{0?: null, 1?: null, 2?: null}"), &ty(src));
            assert_eq!(
                accepted.is_yes(),
                domain.is_yes(),
                "{src}: acceptance {accepted:?} disagrees with domain is_list {domain:?}"
            );
        }
    }

    #[test]
    fn proven_list_shape_rejects_the_general_array_whatever_keyword_spelled_it() {
        // Issue #169: `['a' => 0]` is a member of `b = array` that every sealed
        // key-`0`-only acceptor provably rejects (possible keys ⊆ {0}).
        let witness = Val::Array(vec![(Key::Str("a".into()), Val::Int(0))]);
        assert_eq!(admits_val(&ty("array"), &witness), Certainty::Yes);
        assert_eq!(admits_val(&ty("non-empty-array"), &witness), Certainty::Yes);
        for a in ["array{0?: int}", "list{0?: int}", "array{}"] {
            assert_eq!(admits_val(&ty(a), &witness), Certainty::No, "{a} must reject the witness");
            assert_eq!(subsumes(&ty(a), &ty("array")), Certainty::No, "{a} vs array");
            assert_eq!(
                subsumes(&ty(a), &ty("non-empty-array")),
                Certainty::No,
                "{a} vs non-empty-array"
            );
        }
        // Required twin already No under both spellings; row stays spelling-blind.
        let keyed = subsumes(&ty("array{int}"), &ty("array"));
        let positional = subsumes(&ty("list{int}"), &ty("array"));
        assert_eq!(keyed, positional, "the verdict read the keyword, not the keys");
        assert_eq!(keyed, Certainty::No);
    }

    #[test]
    fn associative_array_does_not_subsume_the_plain_one() {
        // `associative-array` rejects list realizations, and `[]` is a list.
        assert_eq!(
            subsumes(&ty("associative-array<string, int>"), &ty("array<string, int>")),
            Certainty::No
        );
    }

    // -- Shape against shape --

    #[test]
    fn optional_field_shape_subsumes_its_required_twin() {
        assert_eq!(subsumes(&ty("array{a?: int}"), &ty("array{a: int}")), Certainty::Yes);
    }

    #[test]
    fn shape_field_types_are_judged_by_subsumes_itself() {
        assert_eq!(subsumes(&ty("array{a: int}"), &ty("array{a: 1}")), Certainty::Yes);
    }

    #[test]
    fn sealed_shape_does_not_subsume_an_extra_optional_field() {
        // Witness `['a' => 1, 'b' => 1]`: refused by sealed `a`, which never declares `b`.
        assert_eq!(subsumes(&ty("array{a: int}"), &ty("array{a: int, b?: int}")), Certainty::No);
    }

    #[test]
    fn typed_tail_subsumes_a_narrower_typed_tail() {
        assert_eq!(
            subsumes(
                &ty("array{a: int, ...<string, int>}"),
                &ty("array{a: int, ...<non-empty-string, positive-int>}")
            ),
            Certainty::Yes
        );
    }

    #[test]
    fn sealed_shape_does_not_subsume_an_untyped_unsealed_one() {
        // `array{a: int, ...}` admits any extra key; sealed `a` declares finitely many.
        assert_eq!(subsumes(&ty("array{a: int}"), &ty("array{a: int, ...}")), Certainty::No);
    }

    // -- The degenerate shape and the degenerate element types --

    #[test]
    fn empty_sealed_shape_denotes_exactly_the_empty_array() {
        // `array{}` is `{[]}`: a list admits it, a non-empty array does not.
        assert_eq!(subsumes(&ty("list<int>"), &ty("array{}")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-empty-array"), &ty("array{}")), Certainty::No);
    }

    #[test]
    fn an_empty_denotation_is_subsumed_by_everything() {
        // `non-empty-list<never>` denotes nothing — a `No` here would be wrong.
        assert_eq!(subsumes(&ty("int"), &ty("non-empty-list<never>")), Certainty::Yes);
        // `list<never>` denotes exactly `{[]}` — decided by `[]` alone.
        assert_eq!(subsumes(&ty("non-empty-array"), &ty("list<never>")), Certainty::No);
        assert_eq!(subsumes(&ty("array"), &ty("list<never>")), Certainty::Yes);
    }

    #[test]
    fn a_never_valued_map_suppresses_the_entry_witness() {
        // `array<int, never>` denotes `{[]}`: the usual `[1 => v]` witness doesn't exist.
        assert_eq!(subsumes(&ty("list<int>"), &ty("array<int, never>")), Certainty::Yes);
    }

    // -- The a-side union haircut (ADR-0071 §2) --

    #[test]
    fn a_jointly_covering_union_is_never_refuted() {
        // `list|non-empty-array` covers `array` jointly although neither member
        // does; the haircut degrades the or-fold's `No` (no shared witness).
        assert_eq!(subsumes(&ty("list|non-empty-array"), &ty("array")), Certainty::Maybe);
    }

    #[test]
    fn an_all_scalar_union_is_refuted() {
        // Every member is array-incapable, so any array in `b` is a shared witness.
        assert_eq!(subsumes(&ty("int|string"), &ty("array")), Certainty::No);
        assert_eq!(subsumes(&ty("int|string"), &ty("non-empty-array")), Certainty::No);
        assert_eq!(subsumes(&ty("int|Foo"), &ty("array{a: int}")), Certainty::No);
    }

    // -- `mixed` and its cuts (ADR-0071 §2.1) --

    #[test]
    fn mixed_cuts_decide_the_array_arms_by_emptiness() {
        // `non-null-mixed` keeps every array; `non-empty-mixed` drops only `[]`
        // (falsy), refuting `iterable` (always holds `[]`) and the rest.
        assert_eq!(subsumes(&ty("non-null-mixed"), &ty("array")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-null-mixed"), &ty("iterable<int>")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("non-empty-array")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("array{a: int}")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("array")), Certainty::No);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("iterable<int>")), Certainty::No);
    }

    #[test]
    fn array_rules_keep_the_provenance_and_opaque_bars() {
        // ADR-0038: provenance never decides Yes, but a string provably rejects arrays.
        assert_eq!(subsumes(&ContractTy::StrOpaque, &ty("array")), Certainty::No);
        assert_eq!(subsumes(&ContractTy::Opaque, &ty("array")), Certainty::Maybe);
        // `*-closure` refuses every array; a bare `callable` may be a method array.
        assert_eq!(subsumes(&ty("pure-closure"), &ty("array")), Certainty::No);
        assert_eq!(subsumes(&ty("callable"), &ty("array")), Certainty::Maybe);
    }

    // ---- arm_eq -------------------------------------------------------------

    #[test]
    fn array_arms_are_arm_eq_reflexive() {
        // ADR-0071 §3: structural denotation makes arrays arm_eq-reflexive,
        // letting `dedup_arms` collapse spellings (`StrOpaque` stays non-reflexive).
        for src in [
            "array",
            "non-empty-array",
            "list<int>",
            "array<string, int>",
            "iterable<int, string>",
            "array{a: int}",
            "array{a: int, b?: list<string>, ...<string, mixed>}",
        ] {
            assert!(arm_eq(&ty(src), &ty(src)), "{src} must be arm_eq-reflexive");
        }
    }

    #[test]
    fn dedup_collapses_an_array_arm_into_the_wider_spelling() {
        let mut arms = vec![ty("array"), ty("list<int>")];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![ty("array")]);
        // Other declaration order: survivor still `array`.
        let mut arms = vec![ty("list<int>"), ty("array")];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![ty("array")]);
    }

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
        // Literal first, then its base: the base survives.
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

    // ---- interval absorption (issue #90) ------------------------------------

    fn rng(lo: i64, hi: i64) -> ContractTy {
        ContractTy::IntIn(IntRange::new(lo, hi).expect("valid range"))
    }

    #[test]
    fn the_literal_below_an_interval_is_absorbed_into_it() {
        // The headline: `positive-int|0` and `int<0, max>` are one denotation.
        let mut arms = vec![ContractTy::IntIn(IntRange::POSITIVE), lit_i(0)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![ContractTy::IntIn(IntRange::NON_NEGATIVE)]);
    }

    #[test]
    fn the_literal_above_an_interval_is_absorbed_too() {
        let mut arms = vec![lit_i(11), rng(1, 10)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![rng(1, 11)]);
    }

    #[test]
    fn absorption_runs_to_a_fixpoint_over_chained_literals() {
        // `0` reaches the interval only after `1` has already extended it.
        let mut arms = vec![rng(2, 9), lit_i(0), lit_i(1)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![rng(0, 9)]);
    }

    #[test]
    fn absorption_merges_two_touching_intervals_into_their_hull() {
        let mut arms = vec![rng(0, 4), rng(5, 9)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![rng(0, 9)]);
        // Overlapping is the same answer.
        let mut arms = vec![rng(0, 6), rng(5, 9)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![rng(0, 9)]);
    }

    #[test]
    fn absorption_never_bridges_a_gap() {
        // `2` is in neither input, so the hull is NOT the union — refuse.
        let mut arms = vec![lit_i(1), rng(3, 9)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![lit_i(1), rng(3, 9)]);
        let mut arms = vec![rng(0, 1), rng(3, 9)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![rng(0, 1), rng(3, 9)]);
    }

    #[test]
    fn an_interior_literal_is_dropped_by_subsumption_and_never_absorbed() {
        // Interior-point trap (issue #90): `5` is already covered, not extended.
        let mut arms = vec![ContractTy::IntIn(IntRange::POSITIVE), lit_i(5)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![ContractTy::IntIn(IntRange::POSITIVE)]);
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::POSITIVE), &lit_i(5)), None);
    }

    #[test]
    fn absorption_is_denotation_preserving_at_the_boundary() {
        // Never widens: covers each input with `Yes`…
        let lit = lit_i(0);
        let interval = ContractTy::IntIn(IntRange::POSITIVE);
        let merged = merge_int_arms(&lit, &interval).expect("adjacent");
        assert_eq!(subsumes(&merged, &lit), Certainty::Yes);
        assert_eq!(subsumes(&merged, &interval), Certainty::Yes);
        // …and never narrows: mutual subsumption with the hand-written spelling.
        assert!(arm_eq(&merged, &ContractTy::IntIn(IntRange::NON_NEGATIVE)));
        // Boundary honesty: the point just below the merged `lo` is refused.
        assert_eq!(subsumes(&merged, &lit_i(-1)), Certainty::No);
    }

    #[test]
    fn absorption_does_not_wrap_at_the_domain_ends() {
        // Open domain ends: no adjacent literal exists; checked arithmetic refuses.
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::POSITIVE), &lit_i(i64::MIN)), None);
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::NEGATIVE), &lit_i(i64::MAX)), None);
        // The full domain absorbs any literal by subsumption, never extension.
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::FULL), &lit_i(0)), None);
    }

    #[test]
    fn absorption_leaves_non_int_arms_alone() {
        // Only the int vocabulary merges; declaration order is stable around it.
        let mut arms =
            vec![ContractTy::Base(Base::String), ContractTy::IntIn(IntRange::POSITIVE), lit_i(0)];
        dedup_arms(&mut arms);
        assert_eq!(
            arms,
            vec![ContractTy::Base(Base::String), ContractTy::IntIn(IntRange::NON_NEGATIVE)]
        );
        assert_eq!(merge_int_arms(&lit_i(0), &lit_s("a")), None);
        assert_eq!(merge_int_arms(&lit_i(0), &lit_i(1)), None);
    }

    // ---- summarize_vals -----------------------------------------------------

    fn i(n: i64) -> Val {
        Val::Int(n)
    }
    fn s(v: &str) -> Val {
        Val::Str(v.into())
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
        // `!== 5` on a general `int` arm is a no-op (interior point).
        let mut arms = vec![ContractTy::Base(Base::Int)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(5)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::Base(Base::Int)]);
    }

    #[test]
    fn subtract_base_deletes_the_arm_and_its_literals() {
        // `!is_int($x)`: the int arm (and its literal) dies, string survives.
        let mut arms = vec![ContractTy::Base(Base::Int), lit_i(7), ContractTy::Base(Base::String)];
        subtract(&mut arms, &Subtrahend::Base(Base::Int), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::Base(Base::String)]);
    }

    #[test]
    fn subtract_class_negative_branch_reflexive_deletion() {
        // else of `instanceof User`: User dies (is_a=Yes), Guest survives (Unknown).
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
        // then of `instanceof T`: int dies (not an instance), class survives.
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

    // ---- subtract: interval endpoints (one of the two partial deletions) ----

    #[test]
    fn subtract_lo_endpoint_clips_the_interval() {
        // Issue-#90 follow-up: `int<0, max>` less `0` is `int<1, max>`.
        let mut arms = vec![ContractTy::IntIn(IntRange::NON_NEGATIVE)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(0)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::IntIn(IntRange::POSITIVE)]);
    }

    #[test]
    fn subtract_hi_endpoint_clips_the_interval() {
        let mut arms = vec![rng(0, 10)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(10)), &ReflexiveFloor);
        assert_eq!(arms, vec![rng(0, 9)]);
    }

    #[test]
    fn subtract_interior_point_keeps_the_interval_whole() {
        // An interior point would split the interval into a gap no arm can spell.
        let mut arms = vec![rng(0, 10)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(5)), &ReflexiveFloor);
        assert_eq!(arms, vec![rng(0, 10)]);
    }

    #[test]
    fn subtract_point_outside_the_interval_changes_nothing() {
        let mut arms = vec![rng(0, 10)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(42)), &ReflexiveFloor);
        assert_eq!(arms, vec![rng(0, 10)]);
    }

    #[test]
    fn subtract_two_point_interval_collapses_to_the_surviving_literal() {
        // `int<0, 1>` less `0` is the point `1`, spelled as the literal.
        let mut arms = vec![rng(0, 1)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(0)), &ReflexiveFloor);
        assert_eq!(arms, vec![lit_i(1)]);
    }

    #[test]
    fn subtract_point_interval_dies_like_its_literal() {
        // `int<5, 5>` less `5` empties the interval; empty list is no-fact (§2).
        let mut arms = vec![rng(5, 5)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(5)), &ReflexiveFloor);
        assert!(arms.is_empty());
    }

    #[test]
    fn subtract_endpoint_is_safe_at_the_i64_domain_ends() {
        // Clipping FULL at either end must not overflow the bound arithmetic.
        let mut arms = vec![ContractTy::IntIn(IntRange::FULL)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(i64::MIN)), &ReflexiveFloor);
        assert_eq!(arms, vec![rng(i64::MIN + 1, i64::MAX)]);

        let mut arms = vec![ContractTy::IntIn(IntRange::FULL)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(i64::MAX)), &ReflexiveFloor);
        assert_eq!(arms, vec![rng(i64::MIN, i64::MAX - 1)]);

        // The single-point interval at a domain end dies rather than clipping.
        let mut arms = vec![rng(i64::MAX, i64::MAX)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(i64::MAX)), &ReflexiveFloor);
        assert!(arms.is_empty());
    }

    #[test]
    fn subtract_non_int_value_leaves_an_interval_alone() {
        let mut arms = vec![ContractTy::IntIn(IntRange::NON_NEGATIVE)];
        subtract(&mut arms, &Subtrahend::Value(Val::Bool(false)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::IntIn(IntRange::NON_NEGATIVE)]);
    }

    // Regression guards: a bool subtrahend must not leak into the int endpoint
    // clip, and an int subtrahend must not leak into the bool one (issue #443)
    // — the two partial deletions are siblings, not a shared code path.

    #[test]
    fn subtract_bool_value_leaves_an_unrelated_interval_alone() {
        let mut arms = vec![rng(0, 10)];
        subtract(&mut arms, &Subtrahend::Value(Val::Bool(true)), &ReflexiveFloor);
        assert_eq!(arms, vec![rng(0, 10)]);
    }

    #[test]
    fn subtract_int_value_leaves_the_general_bool_arm_alone() {
        let mut arms = vec![ContractTy::Base(Base::Bool)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(0)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::Base(Base::Bool)]);
    }

    // ---- subtract: bool endpoints (the other partial deletion, issue #443) --

    #[test]
    fn subtract_false_narrows_the_general_bool_arm_to_true() {
        let mut arms = vec![ContractTy::Base(Base::Bool)];
        subtract(&mut arms, &Subtrahend::Value(Val::Bool(false)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::LitBool(true)]);
    }

    #[test]
    fn subtract_true_narrows_the_general_bool_arm_to_false() {
        // The domain has exactly two points, spelled either direction: unlike
        // `int<lo, hi>` there is no interior point and no endpoint asymmetry.
        let mut arms = vec![ContractTy::Base(Base::Bool)];
        subtract(&mut arms, &Subtrahend::Value(Val::Bool(true)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::LitBool(false)]);
    }

    #[test]
    fn subtract_bool_literal_from_its_own_singleton_still_empties() {
        // The narrowed arm is an ordinary `LitBool`, so the pre-existing literal
        // path (not the new endpoint clip) handles a second subtraction on it —
        // `subtract_arm`'s `Base(Bool)` guard does not fire on a `LitBool` arm.
        let mut arms = vec![ContractTy::LitBool(false)];
        subtract(&mut arms, &Subtrahend::Value(Val::Bool(false)), &ReflexiveFloor);
        assert!(arms.is_empty());
    }

    #[test]
    fn subtract_both_bool_literals_in_sequence_empties_the_lane() {
        // The exhaustive-chain shape (issue #443's reproducer, at the primitive
        // level): `bool` less `true` less `false` is no-fact, mirroring
        // `subtract_two_point_interval_collapses_to_the_surviving_literal`
        // followed by `subtract_point_interval_dies_like_its_literal` for ints.
        let mut arms = vec![ContractTy::Base(Base::Bool)];
        subtract(&mut arms, &Subtrahend::Value(Val::Bool(true)), &ReflexiveFloor);
        assert_eq!(arms, vec![ContractTy::LitBool(false)]);
        subtract(&mut arms, &Subtrahend::Value(Val::Bool(false)), &ReflexiveFloor);
        assert!(arms.is_empty());
    }

    // ---- subtract with a REAL is-a oracle -----------------------------------

    /// A fixed-hierarchy mock: `edges[sub]` lists `sub`'s proven supertypes,
    /// `finals` the final/enum classes. A class named here is "fully
    /// enumerated" (a missing target is a definite `No`); an unnamed class
    /// answers `Unknown` (`Maybe`).
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
        // else of `instanceof Animal`: Dog and Cat are is_a Animal, both die; scalar survives.
        let mut arms = vec![class("dog"), class("cat"), ContractTy::Base(Base::String)];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Animal".to_owned(), polarity: false }, &mock());
        assert_eq!(arms, vec![ContractTy::Base(Base::String)]);
    }

    #[test]
    fn subtract_negative_branch_argument_order_is_m_then_t() {
        // is_a(Animal,Dog)=No: it survives; reversed is_a(Dog,Animal) would wrongly delete.
        let mut arms = vec![class("animal")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Dog".to_owned(), polarity: false }, &mock());
        assert_eq!(arms, vec![class("animal")], "is_a(M,T) order: Animal is not a Dog, arm kept");
    }

    #[test]
    fn subtract_negative_branch_unknown_keeps_arm() {
        // `Mystery` is unknown → is_a Unknown → arm kept both polarities (FP-safe).
        let mut neg = vec![class("mystery")];
        subtract(&mut neg, &Subtrahend::Class { fqn: "Animal".to_owned(), polarity: false }, &mock());
        assert_eq!(neg, vec![class("mystery")]);
        let mut pos = vec![class("mystery")];
        subtract(&mut pos, &Subtrahend::Class { fqn: "Animal".to_owned(), polarity: true }, &mock());
        assert_eq!(pos, vec![class("mystery")]);
    }

    #[test]
    fn subtract_positive_branch_deletes_final_nonmember_only() {
        // then of `instanceof Cat`: Dog final + is_a=No → dies; Cat is_a=Yes → survives.
        let mut arms = vec![class("dog"), class("cat")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Cat".to_owned(), polarity: true }, &mock());
        assert_eq!(arms, vec![class("cat")]);
    }

    #[test]
    fn subtract_positive_branch_keeps_nonfinal_nonmember() {
        // `Animal` not final, so despite is_a=No it survives (unseen subclass could be Cat).
        let mut arms = vec![class("animal")];
        subtract(&mut arms, &Subtrahend::Class { fqn: "Cat".to_owned(), polarity: true }, &mock());
        assert_eq!(arms, vec![class("animal")]);
    }

    // ---- the enum case arm and its subtrahend (issue #429) ------------------

    fn ecase(e: &str, c: &str) -> ContractTy {
        ContractTy::EnumCase { enum_fqn: e.to_owned(), case: c.to_owned() }
    }

    fn suit(case: &str, polarity: bool) -> Subtrahend {
        Subtrahend::EnumCase {
            enum_fqn: "suit".to_owned(),
            case: case.to_owned(),
            polarity,
        }
    }

    #[test]
    fn an_enum_case_arm_subsumes_only_itself() {
        assert_eq!(subsumes(&ecase("suit", "Hearts"), &ecase("suit", "Hearts")), Certainty::Yes);
        // Two cases of one enum are disjoint values, not merely unrelated.
        assert_eq!(subsumes(&ecase("suit", "Hearts"), &ecase("suit", "Spades")), Certainty::No);
        // Case names are class constants: PHP compares them case-sensitively.
        assert_eq!(subsumes(&ecase("suit", "Hearts"), &ecase("suit", "HEARTS")), Certainty::No);
        // The declaring enum covers its own case; a foreign class stays undecided
        // (an enum may implement interfaces this module knows no hierarchy for).
        assert_eq!(subsumes(&class("suit"), &ecase("suit", "Hearts")), Certainty::Yes);
        assert_eq!(subsumes(&class("unitenum"), &ecase("suit", "Hearts")), Certainty::Maybe);
        // A case is an object: no scalar arm holds one, and both cuts of `mixed` do.
        assert_eq!(subsumes(&ContractTy::Base(Base::Int), &ecase("suit", "Hearts")), Certainty::No);
        assert_eq!(subsumes(&ContractTy::Mixed, &ecase("suit", "Hearts")), Certainty::Yes);
        assert_eq!(
            subsumes(&ContractTy::MixedMinus(MixedCut::Falsy), &ecase("suit", "Hearts")),
            Certainty::Yes
        );
    }

    #[test]
    fn an_enum_case_arm_never_claims_disjointness_from_a_class() {
        // The `subsumes_class` floor: one case does not cover a class extent, but
        // a single-case enum's case IS its enum, so `No` would be a false claim.
        assert_eq!(subsumes(&ecase("suit", "Hearts"), &class("suit")), Certainty::Maybe);
    }

    #[test]
    fn the_negative_branch_deletes_exactly_the_named_case() {
        // `$s !== Suit::Hearts`: the Hearts arm dies, its siblings survive.
        let mut arms = vec![ecase("suit", "Hearts"), ecase("suit", "Spades")];
        subtract(&mut arms, &suit("Hearts", false), &ReflexiveFloor);
        assert_eq!(arms, vec![ecase("suit", "Spades")]);
    }

    #[test]
    fn the_negative_branch_keeps_an_unexpanded_enum_arm() {
        // An enum whose declaration never resolved stays one `Class` arm, and one
        // case does not cover it — the absence discipline, read off the algebra.
        let mut arms = vec![class("suit")];
        subtract(&mut arms, &suit("Hearts", false), &ReflexiveFloor);
        assert_eq!(arms, vec![class("suit")]);
    }

    #[test]
    fn the_positive_branch_deletes_every_other_case() {
        // `$s === Suit::Hearts`: the arm lane keeps its subtraction shape — the
        // branch removes the cases it proves dead rather than intersecting.
        let mut arms =
            vec![ecase("suit", "Hearts"), ecase("suit", "Spades"), ecase("suit", "Clubs")];
        subtract(&mut arms, &suit("Hearts", true), &ReflexiveFloor);
        assert_eq!(arms, vec![ecase("suit", "Hearts")]);
    }

    #[test]
    fn the_positive_branch_deletes_a_scalar_arm_and_keeps_the_open_ones() {
        let mut arms = vec![
            ContractTy::Base(Base::String),
            ContractTy::Null,
            ecase("suit", "Hearts"),
            ContractTy::ObjectAny,
            ContractTy::Opaque,
        ];
        subtract(&mut arms, &suit("Hearts", true), &ReflexiveFloor);
        assert_eq!(
            arms,
            vec![ecase("suit", "Hearts"), ContractTy::ObjectAny, ContractTy::Opaque]
        );
    }

    #[test]
    fn the_positive_branch_asks_no_finality_question() {
        // Unlike the class subtrahend, this one removes a single VALUE, so an
        // unseen descendant of the arm cannot change whether it holds `E::C`.
        // `Animal` is open and `is_a(cat, animal) = Yes`, so the arm survives; a
        // proven non-membership deletes it whether or not the arm is final.
        let sub = Subtrahend::EnumCase {
            enum_fqn: "cat".to_owned(),
            case: "Tabby".to_owned(),
            polarity: true,
        };
        let mut kept = vec![class("animal")];
        subtract(&mut kept, &sub, &mock());
        assert_eq!(kept, vec![class("animal")]);
        let mut died = vec![class("dog")];
        subtract(&mut died, &sub, &mock());
        assert_eq!(died, Vec::<ContractTy>::new(), "is_a(cat, dog) = No deletes the arm");
    }

    #[test]
    fn an_unknown_hierarchy_keeps_the_arm_on_both_polarities() {
        // `Mystery` is outside the enumeration, so `is_a(Mystery, Animal)` is
        // Unknown and the arm survives either way (FP-safe, ADR-0052 §2).
        for polarity in [true, false] {
            let mut arms = vec![class("animal")];
            subtract(
                &mut arms,
                &Subtrahend::EnumCase {
                    enum_fqn: "mystery".to_owned(),
                    case: "Tabby".to_owned(),
                    polarity,
                },
                &mock(),
            );
            assert_eq!(arms, vec![class("animal")], "polarity {polarity}");
        }
    }

    #[test]
    fn an_enum_case_arm_spells_as_phpstan_spells_it() {
        assert_eq!(crate::spell::spell_nested_for_test(&ecase("suit", "Hearts")), "suit::Hearts");
        // The scalar speller declines it, as it declines every object arm.
        assert!(crate::spell::spell_arms(&[ecase("suit", "Hearts")]).is_none());
    }

    // ---- inhabitance under the `[runtime] final-keyword` posture (issue #234) --
    // These pin the judgment ITSELF: intersections are consumed nowhere in the
    // binary today, so the rule ships before its consumer.

    /// `Svc` a plain final service class, `Guard` a final class implementing
    /// `Mock`, `Base` an open class, `Mock` the marker interface — all four
    /// fully enumerated (missing edge = definite `No`). `Sealed` is `final`
    /// but with an unresolvable ancestor, so its is-a answers `Unknown`.
    fn mock_object_isa() -> MockIsa {
        MockIsa {
            edges: [
                ("svc", vec![]),
                ("guard", vec!["mock"]),
                ("base", vec![]),
                ("mock", vec![]),
            ]
            .into_iter()
            .collect(),
            finals: vec!["svc", "guard", "sealed"],
            known: vec!["svc", "guard", "base", "mock"],
        }
    }

    fn inter(arms: &[&str]) -> ContractTy {
        ContractTy::Inter(arms.iter().map(|a| class(a)).collect())
    }

    #[test]
    fn enforced_final_arm_makes_the_intersection_uninhabited() {
        // DEFAULT posture: `Svc` final, so every value is exact `Svc`; is_a(Svc,Mock)=No.
        assert!(provably_uninhabited(
            &inter(&["svc", "mock"]),
            &mock_object_isa(),
            FinalKeyword::Enforced
        ));
    }

    #[test]
    fn stripped_final_arm_leaves_the_intersection_inhabited() {
        // Under "stripped" a mock subclass of `Svc` exists; `Svc&Mock` must not collapse.
        assert!(!provably_uninhabited(
            &inter(&["svc", "mock"]),
            &mock_object_isa(),
            FinalKeyword::Stripped
        ));
    }

    #[test]
    fn the_absence_default_is_the_enforced_posture() {
        // A `steins.toml` with no `[runtime] final-keyword` key resolves to `Default`.
        assert_eq!(FinalKeyword::default(), FinalKeyword::Enforced);
        assert_eq!(
            provably_uninhabited(&inter(&["svc", "mock"]), &mock_object_isa(), FinalKeyword::default()),
            provably_uninhabited(&inter(&["svc", "mock"]), &mock_object_isa(), FinalKeyword::Enforced),
        );
    }

    #[test]
    fn a_final_arm_that_already_implements_the_other_is_inhabited_under_both() {
        // `Guard` is final and is_a(Guard,Mock)=Yes: inhabited under either
        // posture (the posture only ever removes an emptiness proof).
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(
                !provably_uninhabited(&inter(&["guard", "mock"]), &mock_object_isa(), posture),
                "{posture:?}"
            );
        }
    }

    #[test]
    fn an_open_class_arm_is_never_proven_empty() {
        // `Base` not final; an unseen descendant could implement `Mock` (FP-safe).
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(
                !provably_uninhabited(&inter(&["base", "mock"]), &mock_object_isa(), posture),
                "{posture:?}"
            );
        }
    }

    #[test]
    fn an_unknown_is_a_keeps_the_intersection_alive() {
        // `Sealed` is final but unenumerated: is_a=Unknown proves nothing (the
        // A11 catalog-skew demotion reaches this rule as the same `Maybe`).
        assert_eq!(mock_object_isa().is_a("sealed", "mock"), Certainty::Maybe);
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(
                !provably_uninhabited(&inter(&["sealed", "mock"]), &mock_object_isa(), posture),
                "{posture:?}"
            );
        }
    }

    #[test]
    fn two_distinct_final_arms_are_uninhabited_only_when_enforced() {
        // Neither `Svc` nor `Guard` is a subtype of the other and both are sealed.
        let t = inter(&["svc", "guard"]);
        assert!(provably_uninhabited(&t, &mock_object_isa(), FinalKeyword::Enforced));
        assert!(!provably_uninhabited(&t, &mock_object_isa(), FinalKeyword::Stripped));
    }

    #[test]
    fn a_lone_final_arm_never_conflicts_with_itself() {
        // is_a(Svc, Svc)=Yes reflexively: a one-arm intersection is not an emptiness proof.
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(!provably_uninhabited(&inter(&["svc"]), &mock_object_isa(), posture), "{posture:?}");
            assert!(!provably_uninhabited(&class("svc"), &mock_object_isa(), posture), "{posture:?}");
        }
    }

    #[test]
    fn the_never_legs_are_posture_independent() {
        // Algebraic emptiness is the language's, not the runtime's: `never` and
        // closures answer the same under both postures (issue #234 scope guard).
        let cases = [
            ContractTy::Never,
            ContractTy::Inter(vec![class("svc"), ContractTy::Never]),
            ContractTy::Union(vec![ContractTy::Never, ContractTy::Never]),
            ContractTy::Union(vec![ContractTy::Never, inter(&["svc", "mock"])]),
        ];
        for t in &cases {
            assert!(provably_uninhabited(t, &mock_object_isa(), FinalKeyword::Enforced), "{t:?}");
        }
        // Only the last (sealed-conflict) case stops being a proof when stripped.
        for t in &cases[..3] {
            assert!(provably_uninhabited(t, &mock_object_isa(), FinalKeyword::Stripped), "{t:?}");
        }
        assert!(!provably_uninhabited(&cases[3], &mock_object_isa(), FinalKeyword::Stripped));
    }

    #[test]
    fn the_reflexive_floor_proves_no_intersection_empty() {
        // Without a hierarchy nothing is final, so the floor stays "not proven empty".
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(
                !provably_uninhabited(&inter(&["svc", "mock"]), &ReflexiveFloor, posture),
                "{posture:?}"
            );
        }
    }

    #[test]
    fn the_posture_does_not_reach_the_positive_branch_subtraction() {
        // Issue #234 "out of scope": `subtract` takes no posture, so the ADR-0052
        // §2 positive-branch deletion, `class.extends-final`, `override.final`,
        // and the ADR-0049 §8 `Immune` leg are all unchanged.
        let mut arms = vec![class("svc"), class("guard")];
        subtract(
            &mut arms,
            &Subtrahend::Class { fqn: "Mock".to_owned(), polarity: true },
            &mock_object_isa(),
        );
        assert_eq!(arms, vec![class("guard")], "final Svc is not a Mock and still dies");
    }
}
