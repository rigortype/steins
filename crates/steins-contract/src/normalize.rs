//! The type-side normalizer (ADR-0052 §4), extracted from the honesty
//! renderer's dedup / subsumption-collapse / precision-ladder logic rather than
//! built as a separate `TypeCombinator` layer (ADR-0030).
//!
//! Types stay syntactic **arm lists** ([`ContractTy`] members) judged arm-wise
//! through the *single* acceptance relation this crate already owns
//! ([`admits_val`] / [`admits_fact`]). This module adds no parallel judgment:
//! [`subsumes`] reduces one arm to the denotation query the acceptance relation
//! already answers. Two arm families have no scalar-fact denotation to reduce
//! to and carry their rules *inside* [`subsumes`] instead — object arms through
//! the reflexive is-a floor (`subsumes_class`), and the array vocabulary through
//! the structural denotation ADR-0071 §2.1 specifies (`subsumes_array`), whose
//! leaf questions recurse straight back through [`subsumes`] and `admits_val`.
//! (`shape_verdict` in admit.rs is the only type-vs-*value* shape relation.)
//!
//! The public surface (ADR-0052 §4) provides pairwise [`subsumes`], [`arm_eq`],
//! [`dedup_arms`], the value-set → normal-form
//! [`summarize_vals`], and arm-wise [`subtract`] — plus [`merge_int_arms`],
//! the one addition the §4 note of 2026-08-02 records. It is the pairwise
//! primitive behind [`dedup_arms`]' interval absorption, published so a
//! *stratified* arm carrier (steins-infer's contract lane) can reuse the rule
//! instead of reimplementing it. There is deliberately still **no**
//! `union(A, B)` and no generic `remove(T, S)`: joins stay the value domain's
//! job (ADR-0030), and [`merge_int_arms`] is not one — it answers only where
//! the union of two arms IS a single arm, and refuses everywhere else. Its
//! subtraction mirror is [`subtract_arm`]'s endpoint clip: an arm is partially
//! deleted only where the remainder IS a single arm (an interval less its own
//! endpoint), and an interior point is refused everywhere else. [`subtract`]
//! (and the public per-arm judgment [`subtract_arm`] / [`subtrahend_covers`])
//! consult a real is-a [`IsaOracle`]; the caller wires the project hierarchy
//! through that seam, and [`ReflexiveFloor`] is the default. The same seam
//! carries the **inhabitance** judgment [`provably_uninhabited`] (issue #234) —
//! whether a type's denotation is provably empty — because the one rule that
//! makes an intersection empty is a statement about class finality, and finality
//! is what the oracle already answers. It takes the declared [`FinalKeyword`]
//! posture explicitly, so no caller can collapse `final A ∧ B` to `never` without
//! having decided what the project's runtime does with the keyword.
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

use crate::{CField, CKey, ContractTy, MixedCut, admits_fact, admits_val};
use steins_domain::{
    Base, Certainty, Fact, IntRange, Key, KeyClass, Presence, Refinement, ShapeFact, StrPreds,
    Tail, Val,
};

/// The set a guard's negative information removes from an arm list (ADR-0052
/// §2). Judged arm-wise by [`subtract`] / [`subtract_arm`]: an arm dies iff
/// the subtrahend subsumes it with [`Certainty::Yes`]; `Maybe` keeps it (the
/// silence side) — except an interval arm losing its own endpoint, which
/// shrinks instead ([`ArmFate::Narrows`]).
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

/// The is-a oracle for class-arm subtraction (ADR-0052 §2). It is a trait so
/// steins-contract stays **free of any steins-infer dependency**: the project
/// class hierarchy, builtin catalog, and amendment-A11 version-skew demotion live
/// in the caller's implementor (`ProjectIsa`). The hierarchy crosses this seam
/// without moving the polarity law out of this crate.
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
    /// positive branch asks the same order. Reversing it is the implementation
    /// drift the ADR warns about.
    fn is_a(&self, sub: &str, sup: &str) -> Certainty;

    /// Whether `fqn` is `final` (or an enum) — no subclass can exist, so a proven
    /// non-membership (`is_a(fqn, T) = No`) is **exhaustive** and licenses the
    /// positive-branch deletion of the arm. A non-final class always survives the
    /// positive branch (an unseen descendant could implement `T`).
    fn is_final(&self, fqn: &str) -> bool;
}

/// The reflexive is-a floor: without a class hierarchy, `is_a` decides `Yes`
/// only for the same normalized class name and otherwise returns `Maybe`;
/// nothing is `final`, so every open class survives the positive branch.
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

/// The `[runtime] final-keyword` posture (issue #234): what the runtime the
/// project actually runs under *does* with the `final` keyword.
///
/// It belongs to the ADR-0037 §2 pseudo-constant family — the same shelf as
/// `warning-handler` (ADR-0049 §7 amendment) — and for the same reason: it is a
/// boot truth no amount of reading source settles, so the project declares it and
/// Steins reasons under the declaration instead of guessing. The key names the
/// language facility and the value names what the runtime does to it, exactly as
/// `warning-handler = "abort" | "null"` does.
///
/// [`Self::Enforced`] is the default and the [`Default`] impl, so a project that
/// declares nothing gets today's semantics unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FinalKeyword {
    /// `final-keyword = "enforced"` — the language's own rule, and the absence
    /// default: a `final` class (or an enum) admits no subtype, so its instances
    /// all have that exact class.
    #[default]
    Enforced,
    /// `final-keyword = "stripped"` — the project declares that the runtime it is
    /// analyzed for removes the keyword before the engine ever compiles the class.
    /// `dg/bypass-finals` is the motivating implementation: it installs a stream
    /// wrapper that rewrites `final` out of the source as it loads, so under the
    /// test harness a mock subclass of a `final` class genuinely exists and
    /// `FinalClass&MockObject` is a type test code legitimately holds.
    ///
    /// # What this posture deliberately does NOT license
    ///
    /// * **`readonly` is untouched.** `dg/bypass-finals` strips `readonly` only
    ///   when explicitly asked — `enable(bypassReadOnly: true)` — and the project
    ///   that motivated this passes `false`. The two knobs are separate in the
    ///   library, so they stay separate here: a declared `"stripped"` never widens
    ///   to `readonly.reassigned`, whose proof rests on the property modifier and
    ///   not on class finality at all.
    /// * **The `final` diagnostics stay as they are** (issue #234, "out of
    ///   scope"): `class.extends-final` still fires on a declaration that extends a
    ///   final class, `override.final` still fires, and the ADR-0049 §8
    ///   descendant-closure `Immune` leg is unchanged. Source that *spells*
    ///   `extends FinalClass` is broken under a plain runtime whatever the test
    ///   harness rewrites at load time; only the *inhabitance* of an intersection
    ///   type is at stake here.
    /// * **Nothing is inferred.** Detecting `uopz`/`runkit7` or sniffing a
    ///   final-stripping loader out of the dependency graph is issue #205; this
    ///   posture is declared or it is absent.
    ///
    /// # Calibration boundary: the posture is project-wide in v1
    ///
    /// The real call site is path-scoped — `dg/bypass-finals` takes a
    /// `denyPaths([…])` list, so production code is rewritten and the test tree is
    /// not (or the reverse) — and this posture is not: declaring it declares it for
    /// the whole run. That is the honest v1 boundary rather than a defect, because
    /// the widening is in the *silent* direction (it only ever withdraws an
    /// emptiness proof, never adds a claim), and because Steins has nowhere to put a
    /// scoped answer yet: region assignment is ADR-0047's `[transform.partitions]`
    /// machinery, whose observer/partition split is exactly the `denyPaths()` shape,
    /// and it is unimplemented outside `steins transform`. A path-scoped
    /// `final-keyword` belongs there when regions reach the check lane, keyed on the
    /// same declared regions rather than on a second, parallel path vocabulary.
    Stripped,
}

/// Whether `t`'s denotation is **provably empty** — no value of any shape can
/// inhabit it — under the class hierarchy `oracle` answers for and the declared
/// `final_keyword` posture.
///
/// This is the guard issue #234 plants ahead of its consumer. Intersections are
/// consumed nowhere today, so nothing in the binary calls this yet; whoever lands
/// intersection consumption (issue #238) reaches for exactly this question, and
/// the signature makes the posture impossible to skip — there is no argument-free
/// way to ask it. Computing `final A ∧ B` as empty *unconditionally* is the
/// natural implementation and would be a false claim on the **default** surface
/// the moment a project runs its tests under a final-stripping loader (the
/// declared-receiver lane is proof-layer at the `Default` floor, ADR-0049 A13).
///
/// # The rule
///
/// `true` is a **proof**, so every leg is conservative:
///
/// * the algebraic emptiness `denotes_nothing` already decides (`never` and its
///   closures) — posture-independent, and unchanged;
/// * the **sealed-class conflict**: under [`FinalKeyword::Enforced`] a `final`
///   class arm `F` has no subtype, so every value of the intersection has exact
///   class `F` and must therefore already be an instance of every other class arm
///   `T`. One proven `is_a(F, T) = No` and no value satisfies both arms at once.
///   An `Unknown` is-a keeps the intersection alive (the FP-safe side), and so
///   does a non-final arm — an unseen descendant of it could implement `T`.
///
/// Under [`FinalKeyword::Stripped`] the sealed-class leg does not run at all: the
/// subtype the rule assumed away does exist there, so `FinalClass&MockObject` is
/// inhabited and member lookup over it is the union of the arms.
///
/// # Residuals
///
/// Emptiness for a reason this vocabulary does not model — `int&string`, an
/// abstract class with no concrete descendant, a private constructor — answers
/// `false` here exactly as it does in `denotes_nothing`. `false` means "not proven
/// empty", never "proven inhabited"; only the `true` side is actionable.
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
        // A union is empty only when every member is (an empty member list is
        // already `denotes_nothing`).
        ContractTy::Union(members) => {
            members.iter().all(|m| provably_uninhabited(m, oracle, final_keyword))
        }
        // An intersection is empty as soon as ONE member is, plus the finality leg.
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
/// itself; a non-class member (`object`, a scalar, a shape) is not consulted here
/// — its own emptiness is the recursive leg's business, and its *object-ness* is
/// the recall question issue #234 leaves to #238.
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
/// This reuses the single acceptance relation: `b` is reduced to the value or
/// abstract fact that denotes it, and `a` is queried through
/// [`admits_val`] / [`admits_fact`]. Object arms (`Class`, `object`) have no
/// scalar-fact denotation and are judged by the reflexive is-a floor
/// (`subsumes_class`); the array vocabulary (`array`, `list`, `array<K, V>`,
/// `iterable`, `array{…}`) is judged *structurally* by `subsumes_array`
/// (ADR-0071 §2.1); everything else the acceptance relation cannot decide falls
/// to the honest `Maybe`.
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

        // The array vocabulary: a structural denotation, judged by ADR-0071
        // §2.1's rule table. `Yes` needs a coverage argument over `b`'s whole
        // denotation (`[]` and the #14939 order-agnostic keyed realizations
        // included); `No` needs a witness in that denotation `a` provably
        // rejects; `Maybe` remains the floor everywhere else.
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. } => subsumes_array(a, b),

        // Callable / provenance / opaque `b`: outside the scalar-fact
        // vocabulary. `mixed` covers them; otherwise the honest `Maybe` (never
        // a wrong `Yes`, so [`dedup_arms`]/[`subtract`] never collapse them
        // unsoundly).
        ContractTy::CallableTy { .. }
        | ContractTy::StrOpaque
        // A cut of `mixed` still spans every base — objects included — so it has
        // no scalar-fact denotation to ask the acceptance relation about, and
        // only `mixed` itself provably covers it. (`a` being the *same* cut is
        // the reflexive case a future `subsumes` refinement could decide; the
        // honest `Maybe` here never collapses an arm unsoundly.)
        | ContractTy::MixedMinus(_)
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

// ---------------------------------------------------------------------------
// The array vocabulary's structural denotation (ADR-0071).
//
// `subsumes_array` is to the five array arms what `subsumes_class` is to the
// object arms: a rule set *inside* [`subsumes`], not a second relation. Leaf
// questions (element types, key types, field types) recurse through [`subsumes`]
// itself, so a scalar element is judged by exactly the rules a scalar arm is.
// This is the type-vs-type face of the array vocabulary (see the module docs).
//
// Two soundness gates apply to every rule below, and each rule's doc comment
// names which side it argues:
//
//   * **`Yes` only when provable** — every value in `b`'s denotation is admitted
//     by `a`, with `[]` and the #14939 order-agnostic keyed realizations
//     (`array{0: int, 1: string}` admits `[1 => 's', 0 => 1]`, which is *not* a
//     list) explicitly considered.
//   * **`No` only when refutable** — a concrete witness family inside `b`'s
//     denotation that `a` rejects. A `No` fired on an *empty* denotation would
//     be a wrong `No` (vacuously `a ⊇ ∅`), so every entry-shaped witness is
//     gated on the entry being realizable at all (`denotes_nothing`).
// ---------------------------------------------------------------------------

/// The empty array — the witness ADR-0071 §2 names as the most common one, and
/// the probe both `[]`-laws below are written in terms of.
fn empty_array() -> Val {
    Val::Array(Vec::new())
}

/// `a ⊇ b` for an array-vocabulary `b` (ADR-0071 §2.1).
///
/// Two laws run before the `a`-side dispatch, because both are exact for this
/// vocabulary and between them they discharge every non-emptiness rule in the
/// ADR's tables (`covers_ne`) plus the degenerate-denotation guard:
///
/// 1. **`b ⊆ {[]}`** — when `b`'s entry-bearing members provably do not exist
///    (`list<never>`, `array{a?: never}`, `non-empty-list<never>`), the question
///    collapses to "does `a` admit `[]`". An *uninhabited* `b` is subsumed by
///    everything, exactly as [`ContractTy::Never`] is at the top of [`subsumes`];
///    answering anything else there would be the wrong `No` constraint 4 names.
/// 2. **the `[]` witness** — `[]` is in `b`'s denotation and `a` provably rejects
///    it. `admits_val(·, [])` is *exact* on both sides for this vocabulary (the
///    empty array reaches no element type: `shape_verdict` decides it on the
///    `non_empty` flag, the required fields and `is_list([]) == true` alone), so
///    this is a proven refutation and its negation is exactly ADR-0071's
///    `covers_ne`. It is what refutes `non-empty-array ⊇ array{a?: int}`,
///    `array{a: int} ⊇ array{a?: int}` and `associative-array<K, V> ⊇ array<K, V>`
///    (an `associative-array` rejects `[]`, which *is* a list).
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
        // `mixed` covers everything; the null cut removes no array and no
        // Traversable object.
        ContractTy::Mixed | ContractTy::MixedMinus(MixedCut::Null) => Yes,
        // The falsy cut removes exactly one array (`[]`, `php_is_falsy`) and no
        // object. Law 2 already answered `No` wherever `b` admits `[]` — an
        // `iterable` `b` always does — so everything still here is proven
        // non-empty and therefore truthy.
        ContractTy::MixedMinus(MixedCut::Falsy) => Yes,
        ContractTy::Opaque => Maybe,
        // A `callable` *value* may be a two-element method array, so the
        // question is undecided; a `*-closure` spelling (ADR-0063 P3) demands a
        // `Closure` **instance**, which no array is, and `b` has an array member
        // (law 1) — a proven `No`.
        ContractTy::CallableTy { obl, .. } => {
            if obl.closure_only { No } else { Maybe }
        }
        // The joint-cover haircut (ADR-0071 §2, second bullet): `list|non-empty-array`
        // covers `array` although *neither* member does, so an or-fold that ends
        // at `No` is only trustworthy when every member refuses for the same
        // base reason — it admits no array at all, making any array member of
        // `b` a witness shared by the whole union.
        ContractTy::Union(members) => {
            let folded = members.iter().fold(No, |acc, m| acc.or(subsumes_array(m, b)));
            if folded.is_no() && !members.iter().all(array_incapable) { Maybe } else { folded }
        }
        // `a = A ∩ B ⊇ b` iff both do: `and` is sound in both directions (a
        // witness one member rejects is rejected by the intersection).
        ContractTy::Inter(members) => {
            members.iter().fold(Yes, |acc, m| acc.and(subsumes_array(m, b)))
        }
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::IterableOf { .. }
        | ContractTy::Shape { .. } => array_vs_array(a, b),
        // Array-incapable arms — scalars, literals, refinements, `null`, the
        // provenance strings, a class and `object` admit no array at all, and
        // `b`'s denotation holds one (law 1). `Never` admits nothing whatsoever.
        // Note this keeps ADR-0038's bar: `StrOpaque` decides `No`, never `Yes`.
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
        | ContractTy::ObjectAny => No,
    }
}

/// Whether `t` admits **no array value at all** — the "base reason" ADR-0071 §2
/// requires before an `a`-side union fold may end at `No`.
///
/// A union of array-incapable members rejects every array, so any array in `b`'s
/// denotation is a witness the *whole* union refuses, and the fold's `No` is
/// proven. A union with even one array-capable member may cover `b` jointly
/// (`list|non-empty-array ⊇ array`) and its fold's `No` is degraded to `Maybe`.
///
/// Shared with `admit.rs`: ADR-0072 §3 imports this haircut verbatim for the
/// shape-fact face, so the predicate stays one definition rather than two.
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
        | ContractTy::ObjectAny => true,
        // Only the `*-closure` spellings refuse an array outright.
        ContractTy::CallableTy { obl, .. } => obl.closure_only,
        ContractTy::Union(m) => m.iter().all(array_incapable),
        ContractTy::Inter(m) => m.iter().any(array_incapable),
        _ => false,
    }
}

/// Whether some member of `t` is not provably absent — an over-approximation of
/// inhabitedness, used to gate every witness that needs a *value* to exist.
///
/// Exact for the way emptiness is actually spelled (`never`, and the algebraic
/// closures of it). An intersection that is empty for an unmodeled reason
/// (`int&string`) is not detected here; a witness built on it would be vacuous,
/// which is the one residual this module accepts and the ADR does not name.
///
/// This is the posture-independent core of [`provably_uninhabited`], which adds
/// the one emptiness rule that *is* posture-dependent (the sealed-class conflict).
/// The array laws below need no oracle and no posture, so they keep calling this
/// directly.
fn denotes_nothing(t: &ContractTy) -> bool {
    match t {
        ContractTy::Never => true,
        // An empty union denotes nothing (`all` over no members is true), which
        // is also what `admits_val`'s `No`-seeded fold answers.
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
/// no field declares, restated as the three cases the type-vs-type rules branch
/// on. A typed tail whose value (or key) type denotes nothing admits no extra
/// entry at all and is therefore *sealed*, which is why this is a computed view
/// rather than the raw `sealed` flag.
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
    // `shape_verdict` consults a declared tail *before* the sealed flag, so this
    // view resolves them in the same order — one reading of the surface, not two.
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

/// `a ⊇ b` with both sides in the array vocabulary. The non-emptiness half of
/// every ADR-0071 rule (`covers_ne`) was already decided by [`subsumes_array`]'s
/// two laws, so the rules here are purely about keys, values and structure.
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
/// `No`: a positional (`list{…}`) `a` rejects `['a' => 0]`, and a required field
/// is missing from the `b`-member whose only key is a fresh one — `array` holds
/// an array for every one of the infinitely many keys `a` does not require.
/// A keys-prove-list `a` (issue #169) falls to the same member witness as the
/// positional one, whatever keyword spelled it: `['a' => 0]` is a string-keyed
/// array that no sealed key-`0`-only shape admits.
/// `Yes`: a keyed shape whose fields are all optional and all `⊇ mixed`, over an
/// extra-entry surface that accepts every remaining key and value.
fn shape_vs_array_any(v: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    // The third disjunct is issue #169's No-sharpening, and ADR-0071 demands
    // its member witness: `['a' => 0]` is a member of `b` — a string-keyed
    // array, non-empty, so both `ne` flavors of `b` hold it — and a
    // `keys_prove_list` `a` has a sealed tail and possible keys ⊆ {0}, so no
    // member of `a` carries the key `'a'` and `a` provably rejects the witness.
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

/// `b = list<T>` / `non-empty-list<T>`: keys exactly `0..n-1`, of unbounded
/// length, with `T` inhabited (law 1) so a non-empty member is always available
/// as a witness.
///
/// `Yes`: `array` covers every list; a `list<T'>` covers it iff `T' ⊇ T`; a
/// keyed `a` covers it iff its key contract holds every `int<0, max>` and its
/// value contract holds `T`. `No`: a `not_list` `a` rejects *every* member of
/// `b`; a key or value contract that provably refuses is refuted by one member.
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

/// A shape `a` against `b = list<T>` (ADR-0071 §2.1: `Maybe` in general, `No`
/// where a witness applies).
///
/// `No`: a required **string** key is absent from every list, so no member of
/// `b` is admitted; and a *sealed* `a` bounds the length, while `b` holds a list
/// longer than every key `a` declares. No `Yes` rule: a shape covers only
/// bounded lengths, so it cannot cover an unbounded `list<T>`.
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

/// `b = array<K', V'>` / `associative-array<K', V'>`, with `K'` and `V'`
/// inhabited (law 1).
///
/// `Yes`: key and value contracts that each cover `b`'s, plus the `not_list`
/// clause — an `associative-array` `a` covers `b` only when `b` itself has no
/// list realization (either `b` is `associative-array` too, or `0` is outside
/// `K'`, which makes a non-empty list impossible). `No`: a `list<T>` `a` is
/// refuted as soon as `K'` admits a key that cannot start a list; a `not_list`
/// `a` is refuted by `[0 => v]` when `b` admits it.
fn vs_map(a: &ContractTy, key2: &ContractTy, val2: &ContractTy, not_list2: bool) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    match a {
        ContractTy::ArrayAny { .. } => Yes,
        ContractTy::ListOf { .. } => {
            // A key that is neither the string-free `0` nor a list continuation
            // makes the whole realization a non-list, which no `list<T>` admits.
            let non_list_key = [Val::Int(1), Val::Str("k".to_owned())]
                .iter()
                .any(|k| admits_val(key2, k).is_yes());
            if non_list_key { No } else { Maybe }
        }
        ContractTy::MapOf { key, val, not_list, .. } => {
            let core = subsumes(key, key2).and(subsumes(val, val2));
            if *not_list && !not_list2 {
                match admits_val(key2, &Val::Int(0)) {
                    // `b` has no list realization at all, so `a`'s cut is vacuous.
                    No => core,
                    // Witness `[0 => v]`: a member of `b`, rejected by `a`.
                    Yes => No,
                    Maybe => core.and(Maybe),
                }
            } else {
                core
            }
        }
        ContractTy::IterableOf { key, val } => subsumes(key, key2).and(subsumes(val, val2)),
        // A shape bounds the key set; `array<K, V>` does not. No cheap witness
        // beyond law 2's, and no `Yes` — the honest floor.
        _ => Maybe,
    }
}

/// `b = iterable<K', V'>` — arrays **plus** `Traversable` objects.
///
/// The `Traversable` member is the witness ADR-0071 §2 names: no array arm, and
/// no shape, admits an object, so every one of them is a proven `No` however
/// well its keys and values line up. Only another `iterable` can say `Yes`, and
/// only when its key and value contracts cover `b`'s — with the element-witness
/// guard, because `iterable<never, never>` still denotes `[]` and objects, so a
/// `No` read off the element types alone would have no witness.
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
        // `array ⊇ array{…}`: every shape realization is an array, and law 2
        // already required `a` to admit `[]` wherever `b` does. This is the row
        // the 388 shaped functionMap rows ride (ADR-0071 §1).
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
/// `Yes`-eligibility is **denotational** (issue #161): either `b` is positional
/// (`list{…}` — the spelling itself asserts the sequence), or `b`'s key
/// structure alone proves every realization a list ([`keys_prove_list`], the
/// domain's own judgment). The keys decide, not the keyword: `array{null}` and
/// `list{null}` denote the same set and get the same answer.
///
/// A keyed `b` whose realizations can hold two keys still stays `Maybe`: an
/// `array{…}` key set is order-agnostic (#14939), so `array{0: int, 1: string}`
/// admits `[1 => 's', 0 => 1]`, which is not a list. `No` when a required
/// string key makes every `b`-member a non-list.
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
///
/// **Routed to the domain rather than re-derived** (issue #161): the shape's
/// key skeleton — keys, presence, sealing; value slots stay the unknown floor,
/// which the judgment never reads — goes through [`ShapeFact::normalize`], and
/// the answer is its denotational `is_list`. One definition of list-ness in
/// the codebase, so the two layers cannot drift. An unsealed tail is passed as
/// the widest key class it could admit; the `Yes` its callers consume
/// requires a sealed tail, so the widening cannot manufacture one.
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
/// `MapOf`/`IterableOf` row).
///
/// Every declared key literal must be inside `K` and every field type inside
/// `V`; then `b`'s extra-entry surface must be covered too — sealed (nothing to
/// cover), a typed tail `K`/`V` covers, or an untyped-unsealed `b`, which admits
/// an arbitrary extra entry and so demands `K ⊇ array-key` and `V ⊇ mixed`.
/// The `not_list` clause needs a required **string** key in `b` to guarantee no
/// member is a list. `No` witnesses: a field whose key or type `a` refuses (its
/// member exists — the field type is inhabited), and, for an untyped-unsealed
/// `b`, one concrete extra entry `a` provably refuses ([`entry_refuted`]).
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
        // A field nothing can fill is carried by no member: it witnesses nothing
        // and obliges nothing.
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
            // A tail whose key type never escapes the declared fields carries no
            // *extra* entry at all, so a refutation read off it has no witness.
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
        // An `associative-array` `a` rejects every list realization; only a
        // required string key rules those out of `b` for good.
        let string_keyed = bv.fields.iter().any(|f| !f.optional && matches!(f.key, CKey::Str(_)));
        if !string_keyed {
            verdict = verdict.and(Maybe);
        }
    }
    // Every refutation above names a member of `b` carrying some declared key;
    // an unfillable field can make that member unbuildable (a `list{…}` member
    // holding key `n` needs `0..n` filled), so stay honest there.
    if verdict.is_no() && !realizable { Maybe } else { verdict }
}

/// The first integer key, and the first `kN` string key, that `fields` does not
/// declare. Fields are finite, so both always exist — they are the "fresh key"
/// every extra-entry witness in this module is built on.
fn free_keys(fields: &[CField]) -> (i64, String) {
    let mut i = 0i64;
    while fields.iter().any(|f| f.key == CKey::Int(i)) {
        i += 1;
    }
    let mut n = 0usize;
    let mut s = format!("k{n}");
    while fields.iter().any(|f| matches!(&f.key, CKey::Str(k) if *k == s)) {
        n += 1;
        s = format!("k{n}");
    }
    (i, s)
}

/// Whether a typed tail can carry an entry whose key the shape's own fields do
/// **not** declare. A tail key type wholly inside the declared keys contributes
/// no extra entry, so any `No` read off that tail would be vacuous. Probing two
/// fresh keys is a sound under-approximation: a `false` answer only ever costs
/// precision (`Maybe` instead of `No`).
fn tail_key_escapes_fields(tk: &ContractTy, fields: &[CField]) -> bool {
    let (i, s) = free_keys(fields);
    admits_val(tk, &Val::Int(i)).is_yes() || admits_val(tk, &Val::Str(s)).is_yes()
}

/// A concrete extra entry that an untyped-unsealed `b` admits and that the
/// key/value contracts of a `array<K, V>`-shaped `a` provably refuse — the
/// witness behind `array<string, int> ⊉ array{a: int, ...}` (`['a' => 1, 0 => 1]`
/// is a member of `b` whose `int` key `a` refuses).
///
/// The probe key is the first integer no field declares, so the entry really is
/// *extra*; the probe values are one member of each falsy corner of the value
/// domain, which is enough to refute any contract that does not accept `mixed`
/// outright. Only ever consulted for a keyed (`¬list`) `b`, whose extra entries
/// are unconstrained — a `list{…}` tail must continue the sequence.
fn entry_refuted(key: &ContractTy, val: &ContractTy, fields: &[CField]) -> bool {
    let (probe, _) = free_keys(fields);
    if admits_val(key, &Val::Int(probe)).is_no() {
        return true;
    }
    [Val::Null, Val::Int(0), Val::Str(String::new()), Val::Bool(false), Val::Array(Vec::new())]
        .iter()
        .any(|v| admits_val(val, v).is_no())
}

/// `array{…} ⊇ array{…}` — the four obligations of ADR-0071 §2.1's `Shape`
/// bullet, each with its own witness:
///
/// 1. **Every required `a` field must be guaranteed by `b`.** A same-key
///    *required* `b` field with `b.ty ⊆ a.ty` discharges it. Otherwise `b` has a
///    member without the key ([`shape_member_lacking`] proves one exists) and
///    that member refutes.
/// 2. **Every `b` field must land somewhere in `a`** — a same-key `a` field, or
///    `a`'s typed tail, or `a` being untyped-unsealed. A *sealed* `a` refutes
///    (witness: the `b`-member carrying that key).
/// 3. **`b`'s extra-entry surface must be covered by `a`'s.** An untyped-unsealed
///    `b` against a sealed `a` refutes (a `b`-member with a key `a` never
///    declares). The other mismatches stay `Maybe`: their witness would have to
///    be an extra entry whose key escapes `a`'s declared fields, which the tail
///    key type alone does not prove.
/// 4. **Flags.** A positional `a` over a keyed `b` stays `Maybe` — `b`'s
///    order-agnostic realizations (#14939) need not be lists — unless `b`'s key
///    structure alone proves every realization a list ([`keys_prove_list`],
///    issue #169: the flag mismatch is then spelling, not denotation, and the
///    field/tail obligations above already decide), or a required string key
///    proves none of them is (`No`). Non-emptiness was law 2's.
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

    // (4) Flags. The `keys_prove_list` disjunct is issue #169's bridge: a `b`
    // whose keys alone prove every realization a list satisfies a positional
    // `a`'s ordering constraint whatever keyword spelled it, and every changed
    // `Yes` is still earned field by field in obligations 1–3 above.
    if av.list && !bv.list && !keys_prove_list(bv) {
        if bv.fields.iter().any(|f| !f.optional && matches!(f.key, CKey::Str(_))) {
            return No;
        }
        verdict = verdict.and(Maybe);
    }

    // Every field-derived refutation names a member of `b` carrying that key;
    // an unfillable field can make that member unbuildable. The `return No`s
    // above carry their own gate ([`all_fields_inhabited`], or a witness that
    // needs no member at all); this one covers the accumulated verdict.
    if verdict.is_no() && !realizable { Maybe } else { verdict }
}

/// Whether every declared field of a shape can be filled at all — the gate on
/// any witness that has to *build* a member of `b`. A `list{…}` member carrying
/// key `n` needs keys `0..n` filled, so one uninhabited field can make a member
/// unbuildable even when the field under discussion is fine.
fn all_fields_inhabited(v: &ShapeView<'_>) -> bool {
    v.fields.iter().all(|f| !denotes_nothing(&f.ty))
}

/// Whether `b` provably has a member that does **not** carry `key` — the witness
/// behind obligation 1 of [`shape_vs_shape`].
///
/// A required `b` field of that key means every member carries it: no witness.
/// Otherwise the required-fields-only member lacks it, and that member exists —
/// unless `b` is `non-empty`, where at least one entry has to come from
/// somewhere: another required field, the extra-entry surface, or a *different*
/// fillable optional field. When none of those is available the only member `b`
/// has may be the one keyed exactly here, so stay honest.
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

/// The one-value contract a declared shape key denotes, so key questions recurse
/// through [`subsumes`] like every other leaf.
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

/// The proof-strength half of [`subsumes`], for the many places a rule needs
/// "provably covers" rather than a three-valued answer.
fn covers(outer: &ContractTy, inner: &ContractTy) -> bool {
    subsumes(outer, inner).is_yes()
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
///
/// The survivors are then run to an interval-absorption fixpoint
/// ([`merge_int_arms`]): `int<1, max>|0` and `int<0, max>` are one denotation
/// spelled two ways, and this pass picks the interval (issue #90). Subsumption
/// dedup alone cannot do it — neither arm covers the other — so the collapse is
/// a *computed* one, in the [`summarize_vals`] sense, not a renderer choice.
pub fn dedup_arms(arms: &mut Vec<ContractTy>) {
    let mut kept: Vec<ContractTy> = Vec::with_capacity(arms.len());
    for arm in arms.drain(..) {
        if kept.iter().any(|k| subsumes(k, &arm).is_yes()) {
            continue;
        }
        // The survivor is the wider, more canonical spelling, so it may subsume
        // earlier-kept arms: eliminate in both directions to reach a fixpoint.
        kept.retain(|k| !subsumes(&arm, k).is_yes());
        kept.push(arm);
    }
    absorb_int_arms(&mut kept);
    *arms = kept;
}

/// Run an arm list to the [`merge_int_arms`] fixpoint in place, keeping the
/// stable order: a merged pair takes the **earlier** arm's slot, so the list
/// stays declaration-ordered around it.
///
/// Iterating matters — one merge can expose the next (`int<2, max>`, `1`, `0`
/// merges twice, to `int<0, max>`) — and it terminates because every merge
/// removes exactly one arm.
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
///
/// Exactly three shapes qualify, and the rule is symmetric in its arguments:
///
/// * `LitInt(n)` + `IntIn(lo, hi)` with `n == lo - 1` → `IntIn(n, hi)`;
/// * `LitInt(n)` + `IntIn(lo, hi)` with `n == hi + 1` → `IntIn(lo, n)`;
/// * `IntIn(a, b)` + `IntIn(c, d)` that overlap **or touch** → their hull.
///
/// Every other pair is refused, and two refusals carry the rule's whole
/// soundness argument:
///
/// * a **gap** is never bridged — `1|int<3, max>` stays two arms, because
///   `int<1, max>` would admit `2`, which neither input does;
/// * an **interior** literal (`5` beside `int<1, max>`) never reaches here at
///   all, because [`dedup_arms`]' subsumption pass already dropped it as
///   covered. Were it to reach here it would still be refused (`5` is neither
///   `lo - 1` nor `hi + 1`), so the interior-point trap is closed twice over.
///
/// Boundary arithmetic is checked, not wrapped: `hi + 1` at `i64::MAX` and
/// `lo - 1` at `i64::MIN` return `None`. Those are the `min`/`max` open ends,
/// where no adjacent literal can exist — the guard is written anyway so the
/// rule never depends on that argument holding.
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
            // Touching or overlapping ⟺ neither sits strictly beyond the
            // other's successor. `hi + 1` is checked: an open `max` end can
            // never be *below* another arm's `lo`, so the saturating fallback
            // is only ever the correct "no gap" answer.
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
/// (ADR-0052 §2), by each arm's [`subtract_arm`] fate: an arm dies iff the
/// subtrahend subsumes it with [`Certainty::Yes`] (`Maybe` keeps it — the
/// silence side), except that a [`ContractTy::IntIn`] arm minus one of its own
/// **endpoints** is partially deleted — it shrinks by one instead of surviving
/// whole. An arm list that this empties is left empty — the caller drops it to
/// no-fact (never a death signal; the verdict owns death, ADR-0052 §2).
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
///
/// Public (with [`subtract_arm`]) so a caller carrying a **parallel** per-arm
/// structure (steins-infer's stratified contract lane) can map its arms in
/// lockstep with the exact same judgment — the single deletion oracle, no
/// second copy of the polarity or endpoint law.
#[derive(Debug, Clone, PartialEq)]
pub enum ArmFate {
    /// The subtrahend does not provably cover the arm — it survives whole.
    Survives,
    /// The subtrahend covers the whole arm — it is deleted.
    Dies,
    /// The subtrahend removes an endpoint of an interval arm — the arm is
    /// replaced by the shrunk remainder (the one partial deletion).
    Narrows(ContractTy),
}

/// The fate of `arm` under `sub` (ADR-0052 §2): [`ArmFate::Dies`] iff
/// [`subtrahend_covers`] answers [`Certainty::Yes`], plus the one **partial**
/// deletion the arm vocabulary can spell back — a [`ContractTy::IntIn`] arm
/// minus one of its own endpoints shrinks by one (`int<lo, hi>` less `lo` is
/// `int<lo+1, hi>`; a two-point interval collapses to the surviving literal;
/// the point interval dies). An **interior** point must not split the interval
/// — the gap has no arm spelling — so the arm survives whole: the same
/// interior-point discipline as point 2's `Refined` clause, one carrier up.
#[must_use]
pub fn subtract_arm(sub: &Subtrahend, arm: &ContractTy, oracle: &dyn IsaOracle) -> ArmFate {
    if let (Subtrahend::Value(Val::Int(n)), ContractTy::IntIn(r)) = (sub, arm) {
        return clip_int_endpoint(*n, *r);
    }
    if subtrahend_covers(sub, arm, oracle).is_yes() { ArmFate::Dies } else { ArmFate::Survives }
}

/// An interval minus one point: the point interval dies, an endpoint is clipped
/// off, an interior (or outside) point changes nothing. The point-interval case
/// is decided first, so the `lo + 1` / `hi - 1` below runs only on a
/// multi-point interval (`lo < hi`) and cannot leave the i64 domain; the
/// [`IntRange::new`] `Option` still backstops the arithmetic.
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

/// The [`Certainty`] that the subtrahend's denotation covers (subsumes) the whole
/// arm — the whole-arm half of [`subtract_arm`]'s judgment. `Null`/`Value`/`Base`
/// reduce to a [`ContractTy`] and reuse [`subsumes`]; the class subtrahend carries
/// the polarity asymmetry and consults the real is-a `oracle`.
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
/// unknown `Opaque` — subtracting it covers nothing (sound: no array is
/// subtracted).
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

    // ---- subsumes: the array vocabulary (ADR-0071) ---------------------------

    /// The array tests are written in phpdoc source and lowered, which keeps a
    /// `array{dirname: string, basename: string}` readable as itself. Panics on a
    /// spelling this crate cannot lower — a test bug, never a silent skip.
    fn ty(src: &str) -> ContractTy {
        crate::lower_str(src).unwrap_or_else(|| panic!("{src:?} must lower"))
    }

    #[test]
    fn lowered_array_spellings_are_what_the_rules_assume() {
        // Sanity pins for the source spellings the rule tests below rely on: a
        // failing rule test is then a rule bug, not a lowering surprise.
        assert_eq!(ty("array"), ContractTy::ArrayAny { non_empty: false });
        assert_eq!(ty("non-empty-array"), ContractTy::ArrayAny { non_empty: true });
        assert_eq!(
            ty("array{a: int}"),
            ContractTy::Shape {
                list: false,
                fields: vec![CField {
                    key: CKey::Str("a".to_owned()),
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
        // The row the 388 shaped functionMap rows ride: every realization of a
        // shape is an array, `[]` included where the shape admits it.
        assert_eq!(
            subsumes(&ty("array"), &ty("array{dirname: string, basename: string}")),
            Certainty::Yes
        );
    }

    #[test]
    fn keyed_array_subsumes_a_sealed_shape_it_covers() {
        // `array<string, mixed>` holds every string-keyed array; a sealed shape
        // declares nothing outside those keys and values.
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
        // Element verdicts compose: `string ⊇ non-falsy-string`, and dropping the
        // non-empty guarantee only widens.
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
        // `array{a: int, ...}` admits `['a' => 1, 0 => 1]`; the `int` key is
        // outside `array<string, int>`.
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
        // #14939: `array{0: int, 1: int}` admits `[1 => 1, 0 => 1]`, which is NOT
        // a list. Not refutable either (the in-order realization IS a list), so
        // the honest middle.
        assert_eq!(subsumes(&ty("list<int>"), &ty("array{0: int, 1: int}")), Certainty::Maybe);
    }

    #[test]
    fn list_acceptance_does_not_depend_on_the_spelling_of_a_proven_key_set() {
        // Issue #161: a sealed shape whose only possible key is `0` has no
        // order-agnostic realization, so `array{null}` and `list{null}` denote
        // the same set — the verdict must be the same, and it must be Yes.
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
        // The unsound direction of issue #161: optional keys admit gapped
        // realizations — `[0 => 1, 2 => 1]` fails `array_is_list` (measured) —
        // so a `Yes` here would be a wrong Yes, whatever the element types say.
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
        // The denotational eligibility is ROUTED through `ShapeFact::normalize`
        // (one definition of list-ness); this matrix walks keyed spellings
        // through both ends of the route — the acceptance relation on the
        // lowered spelling, the domain on the same key skeleton — so the
        // conversion between the layers cannot drift. `list{…}` spellings are
        // excluded by design: the keyword adds order information the key set
        // alone does not carry.
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
        // Issue #169, `shape_vs_shape`'s flag row: a sealed subject whose only
        // possible key is `0` is a sequence whatever keyword introduced it, so
        // under a positional acceptor `array{null}` and `list{null}` must get
        // the same verdict — and it is Yes, earned by obligations 1–3.
        for a in ["list{string|null}", "list{0?: string|null}"] {
            let keyed = subsumes(&ty(a), &ty("array{null}"));
            let positional = subsumes(&ty(a), &ty("list{null}"));
            assert_eq!(keyed, positional, "{a}: the verdict read the keyword, not the keys");
            assert_eq!(keyed, Certainty::Yes, "{a} must accept the single-key-0 sealed shape");
        }
        // The optional twin realizes `[]` and `[0 => v]` — both admitted by an
        // all-optional positional acceptor.
        assert_eq!(subsumes(&ty("list{0?: int}"), &ty("array{0?: int}")), Certainty::Yes);
    }

    #[test]
    fn positional_shape_does_not_accept_a_subject_admitting_a_gapped_key_set() {
        // The unsound direction of issue #169's flag row, mirroring #161's
        // pins: these subjects admit a permuted (`[1 => 1, 0 => 1]`) or gapped
        // (`[0 => 1, 2 => 1]`, which fails `array_is_list`) realization, so a
        // `Yes` under a positional acceptor would be a wrong Yes.
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
        // The same drift-guard as #161's matrix, on `shape_vs_shape`'s flag
        // row: the acceptor's optional fields cover every subject's fields, so
        // the denotational list judgment is the only discriminator left, and
        // acceptance may answer Yes exactly where the domain proves every
        // realization a list.
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
        // Issue #169, `shape_vs_array_any`'s No-sharpening, with its member
        // witness measured in this run rather than asserted by analogy:
        // `['a' => 0]` is a member of `b = array` (string-keyed, non-empty, so
        // both `ne` flavors hold it) that every sealed key-`0`-only acceptor
        // provably rejects — its possible keys are ⊆ {0}, so no member carries
        // the key `'a'`.
        let witness = Val::Array(vec![(Key::Str("a".to_owned()), Val::Int(0))]);
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
        // The required twin was already No under both spellings (a required
        // field is missing from the fresh-keyed member); pinned so the row
        // stays spelling-blind end to end.
        let keyed = subsumes(&ty("array{int}"), &ty("array"));
        let positional = subsumes(&ty("list{int}"), &ty("array"));
        assert_eq!(keyed, positional, "the verdict read the keyword, not the keys");
        assert_eq!(keyed, Certainty::No);
    }

    #[test]
    fn associative_array_does_not_subsume_the_plain_one() {
        // `associative-array` rejects list realizations — and `[]` is a list, so
        // it is exactly the witness `array<string, int>` supplies.
        assert_eq!(
            subsumes(&ty("associative-array<string, int>"), &ty("array<string, int>")),
            Certainty::No
        );
    }

    // -- Shape against shape --

    #[test]
    fn optional_field_shape_subsumes_its_required_twin() {
        // Every `array{a: int}` member has `a` present with an `int`; an optional
        // declaration admits exactly that (and more).
        assert_eq!(subsumes(&ty("array{a?: int}"), &ty("array{a: int}")), Certainty::Yes);
    }

    #[test]
    fn shape_field_types_are_judged_by_subsumes_itself() {
        assert_eq!(subsumes(&ty("array{a: int}"), &ty("array{a: 1}")), Certainty::Yes);
    }

    #[test]
    fn sealed_shape_does_not_subsume_an_extra_optional_field() {
        // Witness `['a' => 1, 'b' => 1]`: a member of `b`, refused by a sealed `a`
        // that never declares `b`.
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
        // `array{a: int, ...}` admits a member with any extra key; a sealed `a`
        // declares finitely many, so one of them refutes.
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
        // `non-empty-list<never>` denotes nothing at all — answering `No` there
        // (as the `[]`/entry witnesses otherwise would) is a WRONG No.
        assert_eq!(subsumes(&ty("int"), &ty("non-empty-list<never>")), Certainty::Yes);
        // `list<never>` denotes exactly `{[]}` — decided by the `[]` question alone.
        assert_eq!(subsumes(&ty("non-empty-array"), &ty("list<never>")), Certainty::No);
        assert_eq!(subsumes(&ty("array"), &ty("list<never>")), Certainty::Yes);
    }

    #[test]
    fn a_never_valued_map_suppresses_the_entry_witness() {
        // `array<int, never>` denotes `{[]}`, so the `[1 => v]` non-list witness
        // that normally refutes `list<int> ⊇ array<int, int>` does not exist.
        assert_eq!(subsumes(&ty("list<int>"), &ty("array<int, never>")), Certainty::Yes);
    }

    // -- The a-side union haircut (ADR-0071 §2) --

    #[test]
    fn a_jointly_covering_union_is_never_refuted() {
        // `list|non-empty-array` covers `array` jointly (every empty array is a
        // list, every other array is non-empty) although NEITHER member does. An
        // or-fold would end at No; the haircut degrades it, because a member that
        // can hold an array supplies no shared witness.
        assert_eq!(subsumes(&ty("list|non-empty-array"), &ty("array")), Certainty::Maybe);
    }

    #[test]
    fn an_all_scalar_union_is_refuted() {
        // Every member is array-incapable, so any array in `b` is one witness the
        // whole union rejects — the fold's No stands.
        assert_eq!(subsumes(&ty("int|string"), &ty("array")), Certainty::No);
        assert_eq!(subsumes(&ty("int|string"), &ty("non-empty-array")), Certainty::No);
        assert_eq!(subsumes(&ty("int|Foo"), &ty("array{a: int}")), Certainty::No);
    }

    // -- `mixed` and its cuts (ADR-0071 §2.1) --

    #[test]
    fn mixed_cuts_decide_the_array_arms_by_emptiness() {
        // `non-null-mixed` keeps every array; `non-empty-mixed` drops exactly one
        // (`[]` is falsy), so it covers a guaranteed-non-empty `b` and refutes the
        // rest — `iterable` included, which always holds `[]`.
        assert_eq!(subsumes(&ty("non-null-mixed"), &ty("array")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-null-mixed"), &ty("iterable<int>")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("non-empty-array")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("array{a: int}")), Certainty::Yes);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("array")), Certainty::No);
        assert_eq!(subsumes(&ty("non-empty-mixed"), &ty("iterable<int>")), Certainty::No);
    }

    #[test]
    fn array_rules_keep_the_provenance_and_opaque_bars() {
        // ADR-0038: a provenance arm never decides Yes — but a string type does
        // provably reject an array, so No is right and Yes is barred.
        assert_eq!(subsumes(&ContractTy::StrOpaque, &ty("array")), Certainty::No);
        // `Opaque` is the honest middle on both sides.
        assert_eq!(subsumes(&ContractTy::Opaque, &ty("array")), Certainty::Maybe);
        // A `*-closure` spelling refuses every array; a bare `callable` may be a
        // two-element method array.
        assert_eq!(subsumes(&ty("pure-closure"), &ty("array")), Certainty::No);
        assert_eq!(subsumes(&ty("callable"), &ty("array")), Certainty::Maybe);
    }

    // ---- arm_eq -------------------------------------------------------------

    #[test]
    fn array_arms_are_arm_eq_reflexive() {
        // ADR-0071 §3: the structural denotation makes the array vocabulary
        // arm_eq-reflexive, which is what lets `dedup_arms` collapse duplicate
        // array spellings at all. (`StrOpaque`/`Opaque`/`callable` stay
        // deliberately non-reflexive — see `provenance_arm_never_decides_yes`.)
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
        // …and in the other declaration order the survivor is still `array`.
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
        // The interior-point trap (issue #90): `5` inside `int<1, max>` is already
        // covered, so the subsumption pass deletes it — the interval is unchanged,
        // not extended. Pinned on both halves: the list, and the merge's refusal.
        let mut arms = vec![ContractTy::IntIn(IntRange::POSITIVE), lit_i(5)];
        dedup_arms(&mut arms);
        assert_eq!(arms, vec![ContractTy::IntIn(IntRange::POSITIVE)]);
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::POSITIVE), &lit_i(5)), None);
    }

    #[test]
    fn absorption_is_denotation_preserving_at_the_boundary() {
        // Never widens: the merged arm covers each input with `Yes`…
        let lit = lit_i(0);
        let interval = ContractTy::IntIn(IntRange::POSITIVE);
        let merged = merge_int_arms(&lit, &interval).expect("adjacent");
        assert_eq!(subsumes(&merged, &lit), Certainty::Yes);
        assert_eq!(subsumes(&merged, &interval), Certainty::Yes);
        // …and never narrows: each input is still inside it (mutual subsumption
        // with the hand-written spelling of the same set).
        assert!(arm_eq(&merged, &ContractTy::IntIn(IntRange::NON_NEGATIVE)));
        // Boundary honesty: the point just below the merged `lo` is refused.
        assert_eq!(subsumes(&merged, &lit_i(-1)), Certainty::No);
    }

    #[test]
    fn absorption_does_not_wrap_at_the_domain_ends() {
        // `hi + 1` at `max` and `lo - 1` at `min` are the open ends: no literal can
        // be adjacent on the open side, and the checked arithmetic says so rather
        // than wrapping into a bogus merge.
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::POSITIVE), &lit_i(i64::MIN)), None);
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::NEGATIVE), &lit_i(i64::MAX)), None);
        // The full domain absorbs any literal by subsumption, never by extension.
        assert_eq!(merge_int_arms(&ContractTy::IntIn(IntRange::FULL), &lit_i(0)), None);
    }

    #[test]
    fn absorption_leaves_non_int_arms_alone() {
        // Only the int vocabulary merges; a string/bool neighbour is untouched, and
        // the surviving order stays declaration-stable around the merge.
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

    // ---- subtract: interval endpoints (the one partial deletion) ------------

    #[test]
    fn subtract_lo_endpoint_clips_the_interval() {
        // The issue-#90 follow-up headline: `int<0, max>` less `0` is
        // `int<1, max>` — the absorbed `positive-int|0` narrows again.
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
        // An interior point would split the interval into two arms — a gap the
        // arm vocabulary has no way to spell back — so the honest answer is the
        // unchanged arm (ADR-0052 §2's interior-point discipline, one carrier up).
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
        // `int<0, 1>` less `0` is the point `1`, spelled as the literal — the
        // canonical arm the #90 absorption would rebuild the interval from.
        let mut arms = vec![rng(0, 1)];
        subtract(&mut arms, &Subtrahend::Value(Val::Int(0)), &ReflexiveFloor);
        assert_eq!(arms, vec![lit_i(1)]);
    }

    #[test]
    fn subtract_point_interval_dies_like_its_literal() {
        // `int<5, 5>` less `5` empties the interval; the emptied arm dies and an
        // emptied list stays the caller's no-fact signal, as everywhere in §2.
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

    // ---- subtract with a REAL is-a oracle -----------------------------------

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
        // reversed is_a(Dog, Animal)=Yes would wrongly delete it — the drift.
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

    // ---- inhabitance under the `[runtime] final-keyword` posture (issue #234) --
    //
    // These pin the judgment ITSELF, not a finding: intersections are consumed
    // nowhere in the binary today, so there is no diagnostic to assert against and
    // no consumer whose arrival these tests may wait on. That is the point — the
    // rule ships before the consumer so the consumer cannot ship the collapse.

    /// The mock-object hierarchy: `Svc` is a plain final service class, `Guard` a
    /// final class that additionally implements `Mock`, `Base` an open class, and
    /// `Mock` the marker interface a mock generator would implement. Those four are
    /// fully enumerated, so a missing edge is a definite `No` — the situation a real
    /// project reaches once PHPUnit is indexed. `Sealed` is the fifth case a real
    /// project also has: `final`, but with an ancestor the index cannot resolve, so
    /// its is-a answers `Unknown`.
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
        // The DEFAULT posture, and the language's own semantics: `Svc` is final, so
        // every value of the intersection is an exact `Svc`, and is_a(Svc, Mock)=No
        // proves no such value implements `Mock`. Nothing can inhabit `Svc&Mock`.
        assert!(provably_uninhabited(
            &inter(&["svc", "mock"]),
            &mock_object_isa(),
            FinalKeyword::Enforced
        ));
    }

    #[test]
    fn stripped_final_arm_leaves_the_intersection_inhabited() {
        // The whole issue: under a declared `final-keyword = "stripped"` the loader
        // has removed the keyword, a mock subclass of `Svc` exists, and `Svc&Mock`
        // is a type the test suite genuinely holds. Whoever consumes intersections
        // must NOT collapse it.
        assert!(!provably_uninhabited(
            &inter(&["svc", "mock"]),
            &mock_object_isa(),
            FinalKeyword::Stripped
        ));
    }

    #[test]
    fn the_absence_default_is_the_enforced_posture() {
        // `Default` is what a `steins.toml` with no `[runtime] final-keyword` key
        // resolves to, so today's semantics are what a silent project gets.
        assert_eq!(FinalKeyword::default(), FinalKeyword::Enforced);
        assert_eq!(
            provably_uninhabited(&inter(&["svc", "mock"]), &mock_object_isa(), FinalKeyword::default()),
            provably_uninhabited(&inter(&["svc", "mock"]), &mock_object_isa(), FinalKeyword::Enforced),
        );
    }

    #[test]
    fn a_final_arm_that_already_implements_the_other_is_inhabited_under_both() {
        // `Guard` is final AND is_a(Guard, Mock)=Yes: the exact class already
        // satisfies both arms, so `Guard&Mock` is inhabited whatever the runtime
        // does with the keyword. The posture only ever *removes* an emptiness
        // proof; it never adds one.
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(
                !provably_uninhabited(&inter(&["guard", "mock"]), &mock_object_isa(), posture),
                "{posture:?}"
            );
        }
    }

    #[test]
    fn an_open_class_arm_is_never_proven_empty() {
        // `Base` is not final and is_a(Base, Mock)=No, but an unseen descendant of
        // `Base` could implement `Mock` — the FP-safe side, and unchanged by this
        // issue in either posture.
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(
                !provably_uninhabited(&inter(&["base", "mock"]), &mock_object_isa(), posture),
                "{posture:?}"
            );
        }
    }

    #[test]
    fn an_unknown_is_a_keeps_the_intersection_alive() {
        // `Sealed` is final but its ancestry is not fully enumerated, so
        // is_a(Sealed, Mock) answers Unknown. A sealed arm against an undecided
        // target proves nothing (ADR-0052's Unknown-keeps-the-arm discipline), in
        // either posture — and neither does the A11 catalog-skew demotion, which
        // reaches this rule as exactly the same `Maybe`.
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
        // is_a(Svc, Svc) is reflexively Yes: a one-arm intersection (and a bare
        // class) is not an emptiness proof.
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(!provably_uninhabited(&inter(&["svc"]), &mock_object_isa(), posture), "{posture:?}");
            assert!(!provably_uninhabited(&class("svc"), &mock_object_isa(), posture), "{posture:?}");
        }
    }

    #[test]
    fn the_never_legs_are_posture_independent() {
        // Algebraic emptiness is the language's, not the runtime's: `never` and its
        // closures answer the same under both postures. A posture that could silence
        // THIS would be the scope creep issue #234 forbids.
        let cases = [
            ContractTy::Never,
            ContractTy::Inter(vec![class("svc"), ContractTy::Never]),
            ContractTy::Union(vec![ContractTy::Never, ContractTy::Never]),
            ContractTy::Union(vec![ContractTy::Never, inter(&["svc", "mock"])]),
        ];
        for t in &cases {
            assert!(provably_uninhabited(t, &mock_object_isa(), FinalKeyword::Enforced), "{t:?}");
        }
        // Only the last one — whose non-`never` member is the sealed conflict —
        // stops being a proof when the keyword is stripped.
        for t in &cases[..3] {
            assert!(provably_uninhabited(t, &mock_object_isa(), FinalKeyword::Stripped), "{t:?}");
        }
        assert!(!provably_uninhabited(&cases[3], &mock_object_isa(), FinalKeyword::Stripped));
    }

    #[test]
    fn the_reflexive_floor_proves_no_intersection_empty() {
        // Without a project hierarchy nothing is final and nothing is a proven
        // non-member, so the floor answers the honest "not proven empty" in both
        // postures — the same FP-safe default `subtract` gets from it.
        for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
            assert!(
                !provably_uninhabited(&inter(&["svc", "mock"]), &ReflexiveFloor, posture),
                "{posture:?}"
            );
        }
    }

    #[test]
    fn the_posture_does_not_reach_the_positive_branch_subtraction() {
        // Issue #234's "out of scope": `final` semantics are otherwise unchanged.
        // `subtract` takes no posture at all — it cannot, by construction — so the
        // ADR-0052 §2 positive-branch deletion of a final non-member is exactly
        // what it was, and `class.extends-final` / `override.final` / the ADR-0049
        // §8 `Immune` leg are untouched for the same reason.
        let mut arms = vec![class("svc"), class("guard")];
        subtract(
            &mut arms,
            &Subtrahend::Class { fqn: "Mock".to_owned(), polarity: true },
            &mock_object_isa(),
        );
        assert_eq!(arms, vec![class("guard")], "final Svc is not a Mock and still dies");
    }
}
