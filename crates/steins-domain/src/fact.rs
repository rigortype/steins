//! The [`Fact`] — what the analyzer knows about one value — and its algebra.
//!
//! Soundness contract (property-tested in `tests/lattice.rs`):
//! `γ(a) ∪ γ(b) ⊆ γ(join(a, b))` whenever the join is representable; a
//! `None` join means the caller must drop the fact (γ = everything), which
//! is always safe. Widening from the finite layers is *computed*: the
//! summary a value set widens to is derived by evaluating predicates on
//! every member, so precision loss is measured, never guessed (ADR-0035).

use crate::certainty::Certainty;
use crate::php::php_is_falsy;
use crate::preds::StrPreds;
use crate::range::IntRange;
use crate::value::{Base, Val};

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
                            (Refinement::Str(p), Val::Str(s)) => StrPreds::of(s).contains_all(*p),
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
        }
    }

    /// Join: the least representable fact admitting both denotations.
    /// `None` = unrepresentable; the caller drops the fact (safe).
    #[must_use]
    pub fn join(&self, other: &Fact) -> Option<Fact> {
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
            Fact::Refined { nullable, .. } | Fact::General { nullable, .. } => {
                if *nullable { Certainty::Maybe } else { Certainty::No }
            }
        }
    }

    /// Certainty that the value is a string satisfying every predicate in
    /// `pred`.
    #[must_use]
    pub fn satisfies_str(&self, pred: StrPreds) -> Certainty {
        let eval_one = |v: &Val| match v {
            Val::Str(s) => Certainty::from_bool(StrPreds::of(s).contains_all(pred)),
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
        }
    }
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
        Fact::Singleton(_) | Fact::OneOf(_) => None,
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
        // All numeric (hence non-empty), but "0" kills NON_FALSY.
        let expected = StrPreds::NUMERIC.union(StrPreds::NON_EMPTY);
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
