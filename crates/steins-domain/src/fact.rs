//! The [`Fact`] — what the analyzer knows about one value — and its algebra.
//!
//! Soundness contract (property-tested in `tests/lattice.rs`, proved for every
//! value in `spike/lean-domain` — ADR-0059):
//! `γ(a) ∪ γ(b) ⊆ γ(join(a, b))` whenever the join is representable; a
//! `None` join means the caller must drop the fact (γ = everything), which
//! is always safe. Widening from the finite layers is *computed*: the
//! summary a value set widens to is derived by evaluating predicates on
//! every member, so precision loss is measured, never guessed (ADR-0035).

use crate::certainty::Certainty;
use crate::php::php_is_falsy;
use crate::preds::StrPreds;
use crate::range::IntRange;
use crate::shape::{
    KeyClass, Presence, ShapeFact, Tail, array_is_list, SHAPE_WIDTH_LIMIT,
};
use crate::value::{Base, Key, Val};

/// Maximum cardinality of the [`Fact::OneOf`] layer.
pub const CAP: usize = 8;

/// A refinement on a scalar base (the third layer's content).
///
/// Invariants (enforced by [`Fact::refined`]): a `Str` refinement carries a
/// non-empty predicate set, an `Int` refinement a non-full interval —
/// otherwise the fact *is* the General form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refinement {
    /// String predicates (implication-closed bitset).
    Str(StrPreds),
    /// Integer interval.
    Int(IntRange),
}

/// What is known about a single value, in one of the four layers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fact {
    /// Layer 1: exactly this value.
    Singleton(Val),
    /// Layer 2: one of these values (sorted, deduped, `2..=CAP`).
    OneOf(Vec<Val>),
    /// Layer 3: a scalar base constrained by a refinement; `nullable` adds
    /// the null value to the denotation.
    Refined {
        /// The scalar base.
        base: Base,
        /// The refinement (see invariants on [`Refinement`]).
        refinement: Refinement,
        /// Whether `null` is also admitted.
        nullable: bool,
    },
    /// Layer 4: just a scalar base (plus optionally null).
    General {
        /// The scalar base.
        base: Base,
        /// Whether `null` is also admitted.
        nullable: bool,
    },
    /// The abstract array stratum (ADR-0062 §3, A-G2): one canonical
    /// [`ShapeFact`] plus the same `nullable` side-flag the other abstract
    /// layers carry.
    ///
    /// There is no array-`General` variant: the degenerate shape
    /// ([`ShapeFact::plain_array`]) *is* plain `array`. Field-value
    /// nullability lives in that field's own slot fact, never here — one
    /// representation per claim.
    Shape {
        /// The array shape.
        shape: Box<ShapeFact>,
        /// Whether `null` is also admitted.
        nullable: bool,
    },
}

impl Fact {
    /// The Singleton layer.
    #[must_use]
    pub fn singleton(v: Val) -> Fact {
        Fact::Singleton(v)
    }

    /// Build a finite fact from values: deduped and sorted; one value is a
    /// Singleton, up to [`CAP`] a OneOf, beyond that the **computed
    /// widening** to a Refined/General summary. `None` for an empty input
    /// or an unsummarizable overflow (e.g. mixed scalar bases, arrays).
    #[must_use]
    pub fn from_vals(mut vals: Vec<Val>) -> Option<Fact> {
        vals.sort();
        vals.dedup();
        match vals.len() {
            0 => None,
            1 => Some(Fact::Singleton(vals.pop().expect("len checked"))),
            n if n <= CAP => Some(Fact::OneOf(vals)),
            _ => summarize(&vals),
        }
    }

    /// Normalizing Refined constructor: contentless refinements collapse to
    /// the General layer.
    #[must_use]
    pub fn refined(base: Base, refinement: Refinement, nullable: bool) -> Fact {
        let empty = match refinement {
            Refinement::Str(p) => p.is_empty(),
            Refinement::Int(r) => r.is_full(),
        };
        if empty { Fact::General { base, nullable } } else { Fact::Refined { base, refinement, nullable } }
    }

    /// Extensional membership: is `v` in this fact's denotation?
    #[must_use]
    pub fn admits(&self, v: &Val) -> bool {
        match self {
            Fact::Singleton(s) => s == v,
            Fact::OneOf(vals) => vals.binary_search(v).is_ok(),
            Fact::Refined { base, refinement, nullable } => match v {
                Val::Null => *nullable,
                _ => {
                    v.base() == Some(*base)
                        && match (refinement, v) {
                            // Read through the extensional projection: a
                            // contextual predicate (`class-string`) has no
                            // member test here, so γ is over-approximated —
                            // the sound direction for the join contract.
                            (Refinement::Str(p), Val::Str(s)) => {
                                StrPreds::of(s).contains_all(p.extensional())
                            }
                            (Refinement::Int(r), Val::Int(i)) => r.contains(*i),
                            // Base matched but refinement kind cannot apply:
                            // unreachable by construction (Str↔String,
                            // Int↔Int); be safe extensionally.
                            _ => false,
                        }
                }
            },
            Fact::General { base, nullable } => match v {
                Val::Null => *nullable,
                _ => v.base() == Some(*base),
            },
            Fact::Shape { shape, nullable } => match v {
                Val::Null => *nullable,
                Val::Array(entries) => shape.admits(entries),
                _ => false,
            },
        }
    }

    /// Join: the least representable fact admitting both denotations.
    /// `None` = unrepresentable; the caller drops the fact (safe).
    #[must_use]
    pub fn join(&self, other: &Fact) -> Option<Fact> {
        // The array stratum has its own algebra (ADR-0062 A-G5); the scalar
        // layering below cannot express it.
        if matches!(self, Fact::Shape { .. }) || matches!(other, Fact::Shape { .. }) {
            return join_shape(self, other);
        }
        match (self.finite_members(), other.finite_members()) {
            (Some(a), Some(b)) => {
                let mut all = a.to_vec();
                all.extend_from_slice(b);
                Fact::from_vals(all)
            }
            (Some(finite), None) => join_finite_abstract(finite, other),
            (None, Some(finite)) => join_finite_abstract(finite, self),
            (None, None) => join_abstract(self, other),
        }
    }

    /// Certainty that the value is truthy under PHP semantics.
    #[must_use]
    pub fn truthy(&self) -> Certainty {
        match self.finite_members() {
            Some(vals) => Certainty::all_of(vals.iter().map(|v| Certainty::from_bool(!php_is_falsy(v)))),
            None => {
                let (can_be_falsy, can_be_truthy) = self.abstract_falsy_truthy();
                match (can_be_falsy, can_be_truthy) {
                    (false, true) => Certainty::Yes,
                    (true, false) => Certainty::No,
                    _ => Certainty::Maybe,
                }
            }
        }
    }

    /// Certainty that the value is `null`.
    #[must_use]
    pub fn is_null(&self) -> Certainty {
        match self {
            Fact::Singleton(v) => Certainty::from_bool(*v == Val::Null),
            Fact::OneOf(vals) => {
                Certainty::all_of(vals.iter().map(|v| Certainty::from_bool(*v == Val::Null)))
            }
            Fact::Refined { nullable, .. }
            | Fact::General { nullable, .. }
            | Fact::Shape { nullable, .. } => {
                if *nullable { Certainty::Maybe } else { Certainty::No }
            }
        }
    }

    /// Certainty that the value is a string satisfying every predicate in
    /// `pred`.
    ///
    /// A contextual predicate in `pred` (`class-string`) can be *refuted* by a
    /// concrete string — `""` names no class — but never proven here, so a
    /// value that survives the extensional part answers `Maybe`.
    #[must_use]
    pub fn satisfies_str(&self, pred: StrPreds) -> Certainty {
        let eval_one = |v: &Val| match v {
            Val::Str(s) => {
                if !StrPreds::of(s).contains_all(pred.extensional()) {
                    Certainty::No
                } else if pred.is_extensional() {
                    Certainty::Yes
                } else {
                    Certainty::Maybe
                }
            }
            _ => Certainty::No,
        };
        match self {
            Fact::Singleton(v) => eval_one(v),
            Fact::OneOf(vals) => Certainty::all_of(vals.iter().map(eval_one)),
            Fact::Refined { base, refinement, nullable } => {
                if *base != Base::String {
                    return Certainty::No;
                }
                match refinement {
                    Refinement::Str(p) if p.contains_all(pred) && !nullable => Certainty::Yes,
                    _ => Certainty::Maybe,
                }
            }
            Fact::General { base, .. } => {
                if *base == Base::String { Certainty::Maybe } else { Certainty::No }
            }
            // An array is never a string, and neither is null.
            Fact::Shape { .. } => Certainty::No,
        }
    }

    /// Certainty that the value is an int within `range`.
    #[must_use]
    pub fn int_in(&self, range: IntRange) -> Certainty {
        let eval_one = |v: &Val| match v {
            Val::Int(i) => Certainty::from_bool(range.contains(*i)),
            _ => Certainty::No,
        };
        match self {
            Fact::Singleton(v) => eval_one(v),
            Fact::OneOf(vals) => Certainty::all_of(vals.iter().map(eval_one)),
            Fact::Refined { base, refinement, nullable } => {
                if *base != Base::Int {
                    return Certainty::No;
                }
                match refinement {
                    Refinement::Int(r) if range.contains_range(*r) && !nullable => Certainty::Yes,
                    Refinement::Int(r) if r.intersect(range).is_none() => Certainty::No,
                    _ => Certainty::Maybe,
                }
            }
            Fact::General { base, .. } => {
                if *base == Base::Int { Certainty::Maybe } else { Certainty::No }
            }
            // An array is never an int, and neither is null.
            Fact::Shape { .. } => Certainty::No,
        }
    }

    /// Finite members when this fact is in a finite layer.
    #[must_use]
    pub fn finite_members(&self) -> Option<&[Val]> {
        match self {
            Fact::Singleton(v) => Some(std::slice::from_ref(v)),
            Fact::OneOf(vals) => Some(vals),
            _ => None,
        }
    }

    /// (can_be_falsy, can_be_truthy) for the abstract layers.
    fn abstract_falsy_truthy(&self) -> (bool, bool) {
        match self {
            Fact::Singleton(_) | Fact::OneOf(_) => unreachable!("finite layers handled by caller"),
            Fact::Refined { base, refinement, nullable } => {
                let (f, t) = match (base, refinement) {
                    (Base::String, Refinement::Str(p)) => {
                        // Some truthy string satisfies any predicate set
                        // ("5" satisfies all three); falsy strings ("", "0")
                        // are excluded exactly by NON_FALSY.
                        (!p.contains_all(StrPreds::NON_FALSY), true)
                    }
                    (Base::Int, Refinement::Int(r)) => {
                        (r.contains(0), *r != IntRange::point(0))
                    }
                    // Refinement kinds only exist for their own base.
                    _ => (true, true),
                };
                (f || *nullable, t)
            }
            Fact::General { .. } => (true, true),
            Fact::Shape { shape, nullable } => {
                // An array is falsy exactly when empty; `null` is falsy too.
                // The truthy side is over-approximated to `true`: deciding that
                // a shape admits *only* `[]` buys nothing until a consumer asks
                // (`ShapeFact::can_be_non_empty` is the query when one does),
                // and over-approximating pushes the verdict toward `Maybe`,
                // which is the honest direction for a trinary.
                (!shape.non_empty || *nullable, true)
            }
        }
    }
}

/// Join where at least one operand is a [`Fact::Shape`].
///
/// `None` — drop the fact — for anything outside `array|null`: mixed-base
/// unions stay un-facted, exactly as for scalars (A-G2).
fn join_shape(a: &Fact, b: &Fact) -> Option<Fact> {
    let (sa, na) = shape_view(a)?;
    let (sb, nb) = shape_view(b)?;
    let nullable = na || nb;
    let shape = match (sa, sb) {
        (Some(x), Some(y)) => x.join(&y),
        (Some(x), None) | (None, Some(x)) => x,
        // Both operands were null-only, so neither was a Shape — unreachable
        // through `join`, and dropping the fact is the safe answer anyway.
        (None, None) => return None,
    };
    Some(Fact::Shape { shape: Box::new(shape), nullable })
}

/// `(the array shape, nullability)` for an array-shaped fact; `None` when the
/// fact denotes anything outside `array|null`. A concrete array lifts (A-G5),
/// which is where order-witnessed-ness is honestly lost.
fn shape_view(f: &Fact) -> Option<(Option<ShapeFact>, bool)> {
    match f {
        Fact::Shape { shape, nullable } => Some((Some((**shape).clone()), *nullable)),
        Fact::Singleton(Val::Array(entries)) => Some((Some(ShapeFact::lift(entries)), false)),
        Fact::Singleton(Val::Null) => Some((None, true)),
        Fact::OneOf(vals) => {
            let mut acc: Option<ShapeFact> = None;
            let mut nullable = false;
            for v in vals {
                match v {
                    Val::Null => nullable = true,
                    Val::Array(entries) => {
                        let lifted = ShapeFact::lift(entries);
                        acc = Some(acc.map_or(lifted.clone(), |prev| prev.join(&lifted)));
                    }
                    _ => return None,
                }
            }
            Some((acc, nullable))
        }
        _ => None,
    }
}

/// The **computed descent** for an over-`CAP` set of arrays (ADR-0062 §3):
/// keys present in every member are `Required { witnessed: true }`, keys in
/// some are `Optional`, and the tail seals because the members enumerate every
/// key there is. Value slots are the members' own widening, so the summary is
/// derived member by member — never a threshold heuristic (ADR-0035).
fn shape_descent(members: &[&Val], nullable: bool) -> Fact {
    let entries: Vec<&[(Key, Val)]> = members
        .iter()
        .filter_map(|v| match v {
            Val::Array(e) => Some(e.as_slice()),
            _ => None,
        })
        .collect();
    let non_empty = entries.iter().all(|e| !e.is_empty());
    let is_list =
        Certainty::all_of(entries.iter().map(|e| Certainty::from_bool(array_is_list(e))));

    let mut keys: Vec<Key> = Vec::new();
    for e in &entries {
        for (k, _) in *e {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
    }
    keys.sort();

    // A-G6: the field-width bound applies to a seeded shape as much as a
    // lifted one.
    if keys.len() > SHAPE_WIDTH_LIMIT {
        let mut key_class: Option<KeyClass> = None;
        let mut all_vals: Vec<Val> = Vec::new();
        for e in &entries {
            for (k, v) in *e {
                let c = KeyClass::of_key(k);
                key_class = Some(key_class.map_or(c, |acc| acc.join(c)));
                all_vals.push(v.clone());
            }
        }
        let tail = Tail::Unsealed {
            key: key_class.unwrap_or(KeyClass::ArrayKey),
            value: Fact::from_vals(all_vals).map(Box::new),
        };
        let shape = ShapeFact::normalize(Vec::new(), tail, is_list, non_empty, Vec::new());
        return Fact::Shape { shape: Box::new(shape), nullable };
    }

    let mut fields = Vec::with_capacity(keys.len());
    for k in keys {
        let mut vals: Vec<Val> = Vec::new();
        let mut in_every = true;
        for e in &entries {
            match e.iter().find(|(ek, _)| *ek == k) {
                Some((_, v)) => vals.push(v.clone()),
                None => in_every = false,
            }
        }
        let presence =
            if in_every { Presence::Required { witnessed: true } } else { Presence::Optional };
        fields.push((k, presence, Fact::from_vals(vals).map(Box::new)));
    }
    let shape = ShapeFact::normalize(fields, Tail::Sealed, is_list, non_empty, Vec::new());
    Fact::Shape { shape: Box::new(shape), nullable }
}

/// Widen a non-empty, deduped value list to an abstract summary. `None`
/// when unsummarizable (mixed scalar bases, arrays present).
fn summarize(vals: &[Val]) -> Option<Fact> {
    let nullable = vals.contains(&Val::Null);
    let scalars: Vec<&Val> = vals.iter().filter(|v| **v != Val::Null).collect();
    let Some(first) = scalars.first() else {
        // All members were null; the finite layer already represents this.
        return Some(Fact::Singleton(Val::Null));
    };
    // An all-array overflow descends to the abstract array stratum rather than
    // being dropped (ADR-0062 §3). A *mixed* array/non-array overflow is
    // unsummarizable.
    if scalars.iter().all(|v| matches!(v, Val::Array(_))) {
        return Some(shape_descent(&scalars, nullable));
    }
    let base = first.base()?;
    if scalars.iter().any(|v| v.base() != Some(base)) {
        return None;
    }
    let fact = match base {
        Base::Int => {
            let mut range: Option<IntRange> = None;
            for v in &scalars {
                if let Val::Int(i) = v {
                    let p = IntRange::point(*i);
                    range = Some(range.map_or(p, |r| r.hull(p)));
                }
            }
            Fact::refined(base, Refinement::Int(range.expect("nonempty ints")), nullable)
        }
        Base::String => {
            let mut preds: Option<StrPreds> = None;
            for v in &scalars {
                if let Val::Str(s) = v {
                    let p = StrPreds::of(s);
                    preds = Some(preds.map_or(p, |acc| acc.intersect(p)));
                }
            }
            Fact::refined(base, Refinement::Str(preds.expect("nonempty strs")), nullable)
        }
        Base::Float | Base::Bool => Fact::General { base, nullable },
    };
    Some(fact)
}

fn join_finite_abstract(finite: &[Val], abs: &Fact) -> Option<Fact> {
    let summary = summarize(finite)?;
    match summary.finite_members() {
        // The finite side was all-null: fold it in as nullability.
        Some(_) => match abs {
            Fact::Refined { base, refinement, .. } => {
                Some(Fact::refined(*base, *refinement, true))
            }
            Fact::General { base, .. } => Some(Fact::General { base: *base, nullable: true }),
            _ => unreachable!("abs is abstract by caller contract"),
        },
        None => join_abstract(&summary, abs),
    }
}

fn join_abstract(a: &Fact, b: &Fact) -> Option<Fact> {
    let (abase, aref, anull) = abstract_parts(a)?;
    let (bbase, bref, bnull) = abstract_parts(b)?;
    if abase != bbase {
        return None;
    }
    let nullable = anull || bnull;
    let fact = match (aref, bref) {
        (Some(Refinement::Str(p)), Some(Refinement::Str(q))) => {
            Fact::refined(abase, Refinement::Str(p.intersect(q)), nullable)
        }
        (Some(Refinement::Int(r)), Some(Refinement::Int(s))) => {
            Fact::refined(abase, Refinement::Int(r.hull(s)), nullable)
        }
        // A refinement joined with no-knowledge (or mismatched kinds, which
        // cannot occur for one base) widens to General.
        _ => Fact::General { base: abase, nullable },
    };
    Some(fact)
}

fn abstract_parts(f: &Fact) -> Option<(Base, Option<Refinement>, bool)> {
    match f {
        Fact::Refined { base, refinement, nullable } => Some((*base, Some(*refinement), *nullable)),
        Fact::General { base, nullable } => Some((*base, None, *nullable)),
        // The array stratum has no scalar base; `join` routes it away from
        // here before this is ever reached.
        Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Val {
        Val::Str(v.to_owned())
    }

    #[test]
    fn from_vals_layers() {
        assert_eq!(Fact::from_vals(vec![]), None);
        assert_eq!(Fact::from_vals(vec![Val::Int(1), Val::Int(1)]), Some(Fact::Singleton(Val::Int(1))));
        let two = Fact::from_vals(vec![Val::Int(2), Val::Int(1)]).unwrap();
        assert_eq!(two, Fact::OneOf(vec![Val::Int(1), Val::Int(2)]));
    }

    #[test]
    fn overflow_widens_to_computed_summary() {
        let vals: Vec<Val> = (0..=(CAP as i64)).map(Val::Int).collect();
        let f = Fact::from_vals(vals).unwrap();
        assert_eq!(
            f,
            Fact::refined(Base::Int, Refinement::Int(IntRange::new(0, CAP as i64).unwrap()), false)
        );
        assert!(f.admits(&Val::Int(3)));
        assert!(!f.admits(&Val::Int(-1)));
    }

    #[test]
    fn string_summary_is_predicate_intersection() {
        let vals: Vec<Val> =
            ["5", "12", "3.4", "007", " 8 ", "9e2", "44", "0", "17"].iter().map(|v| s(v)).collect();
        let f = Fact::from_vals(vals).unwrap();
        // All numeric (hence non-empty), but "0" kills NON_FALSY. All lowercase
        // too — `"9e2"` has a cased character and it is the lowercase one, so
        // only UPPERCASE falls out of the intersection.
        let expected =
            StrPreds::NUMERIC.union(StrPreds::NON_EMPTY).union(StrPreds::LOWERCASE);
        assert_eq!(f, Fact::refined(Base::String, Refinement::Str(expected), false));
    }

    #[test]
    fn join_mixes_layers_soundly() {
        let lit = Fact::singleton(s("abc"));
        let refined = Fact::refined(Base::String, Refinement::Str(StrPreds::of("xy")), false);
        let j = lit.join(&refined).unwrap();
        assert!(j.admits(&s("abc")) && j.admits(&s("xy")));
        // "abc" and "xy" are both non-falsy and non-numeric → NON_FALSY
        // (with implied NON_EMPTY) survives the intersection.
        assert_eq!(
            j,
            Fact::refined(Base::String, Refinement::Str(StrPreds::of("xy").intersect(StrPreds::of("abc"))), false)
        );
    }

    #[test]
    fn null_folds_into_nullability() {
        let null = Fact::singleton(Val::Null);
        let ints = Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false);
        let j = null.join(&ints).unwrap();
        assert!(j.admits(&Val::Null) && j.admits(&Val::Int(5)));
        assert_eq!(j.is_null(), Certainty::Maybe);
    }

    #[test]
    fn mixed_bases_are_unrepresentable() {
        let a = Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false);
        let b = Fact::refined(Base::String, Refinement::Str(StrPreds::NON_EMPTY), false);
        assert_eq!(a.join(&b), None);
    }

    #[test]
    fn truthiness_queries() {
        assert_eq!(Fact::singleton(s("0")).truthy(), Certainty::No);
        assert_eq!(
            Fact::refined(Base::String, Refinement::Str(StrPreds::NON_FALSY.close()), false).truthy(),
            Certainty::Yes
        );
        assert_eq!(
            Fact::refined(Base::String, Refinement::Str(StrPreds::NON_EMPTY), false).truthy(),
            Certainty::Maybe // "0" is non-empty yet falsy
        );
        assert_eq!(
            Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false).truthy(),
            Certainty::Yes
        );
        assert_eq!(
            Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), true).truthy(),
            Certainty::Maybe // null
        );
    }

    #[test]
    fn refinement_queries() {
        let numeric = Fact::refined(Base::String, Refinement::Str(StrPreds::NUMERIC.close()), false);
        assert_eq!(numeric.satisfies_str(StrPreds::NON_EMPTY), Certainty::Yes); // implied
        assert_eq!(numeric.satisfies_str(StrPreds::NON_FALSY), Certainty::Maybe); // "0"
        assert_eq!(
            Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false)
                .int_in(IntRange::NON_NEGATIVE),
            Certainty::Yes
        );
        assert_eq!(
            Fact::refined(Base::Int, Refinement::Int(IntRange::NEGATIVE), false)
                .int_in(IntRange::POSITIVE),
            Certainty::No // disjoint
        );
    }
}
