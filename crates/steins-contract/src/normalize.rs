//! The type-side normalizer (ADR-0052 §4), extracted from the honesty
//! renderer's dedup / subsumption-collapse / precision-ladder logic — not
//! built as a fresh `TypeCombinator` layer (the ADR-0030 amendment, discharged
//! by slice N1).
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
//! `shape_verdict` remains the only type-vs-*value* shape relation.
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

use crate::{CField, CKey, ContractTy, MixedCut, admits_fact, admits_val};
use steins_domain::{Base, Certainty, Fact, IntRange, Refinement, StrPreds, Val};

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
// `shape_verdict` (admit.rs) stays the only type-vs-*value* shape relation; this
// is its type-vs-type face.
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
/// `Yes`: a keyed shape whose fields are all optional and all `⊇ mixed`, over an
/// extra-entry surface that accepts every remaining key and value.
fn shape_vs_array_any(v: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    if v.list || v.fields.iter().any(|f| !f.optional) {
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
/// `Yes` only for a **positional** `b` (`list{…}`): a keyed `b` with keys
/// `0..n-1` still has order-agnostic realizations (#14939) that are not lists —
/// `array{0: int, 1: string}` admits `[1 => 's', 0 => 1]` — so it stays `Maybe`.
/// `No` when a required string key makes every `b`-member a non-list.
fn list_vs_shape(elem: &ContractTy, bv: &ShapeView<'_>) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    if bv.fields.iter().any(|f| !f.optional && matches!(f.key, CKey::Str(_))) {
        return No;
    }
    if !bv.list {
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
///    order-agnostic realizations (#14939) need not be lists — unless a required
///    string key proves none of them is. Non-emptiness was law 2's.
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

    // (4) Flags.
    if av.list && !bv.list {
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
