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

/// One arm of a [`Fact::Union`]: a scalar base and what is known about the
/// values of that base, `None` being that base's `General` (issue #339).
pub type UnionArm = (Base, Option<Refinement>);

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
    /// **Layer 3½: an abstract union across bases** (issue #339).
    ///
    /// The finite layers have always carried a mixed-base union exactly —
    /// `$c ? 1 : 'x'` is `Fact::OneOf([Int(1), Str("x")])`, whatever the bases.
    /// The abstract layers could not, so a value that started as `1|'x'` and
    /// widened past [`CAP`] became **nothing** rather than `int|string`, and
    /// every rule whose answer spanned two bases — `int|false` returns,
    /// `json_decode`'s envelope, `in_array`'s `int|string|false`, `min`/`max` —
    /// declined for want of a form to say it in.
    ///
    /// This is that form: one arm per [`Base`], each carrying the same
    /// `Option<Refinement>` the single-base layers do (`None` being that base's
    /// General). PHP has four scalar bases, so the union is **bounded at four
    /// arms** — it is a small map, not an open lattice, which is what makes
    /// [`Fact::join`] total over the abstract layers instead of partial.
    ///
    /// # Invariants (established by [`Fact::union`], which is the only way in)
    ///
    /// * arms sorted by [`Base`], **one entry per base**;
    /// * **at least two** arms — one collapses to `Refined`/`General`, none to
    ///   nothing, so a `Union` always says something neither of those can;
    /// * `nullable` carries `null` for the whole union, exactly as it does for
    ///   one base.
    ///
    /// # What is not an arm
    ///
    /// The array stratum. `array|string` is a real PHP type and this is not the
    /// form for it: [`ShapeFact`] is recursive and joining it into a scalar
    /// union would make the layer mutually recursive with the shape algebra for
    /// one spelling. A union with an array in it declines, as it does today.
    Union {
        /// One [`UnionArm`] per base, sorted, `2..=4` of them.
        arms: Vec<UnionArm>,
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
    /// **The array-key cast, at the type level** (issue #336): the fact
    /// describing `$a[$v]`'s key when all that is known about `$v` is this
    /// fact.
    ///
    /// PHP casts an array key eagerly, and the interesting half is the string
    /// one: a string that spells an integer *the way PHP writes one back*
    /// becomes that integer, and every other string keeps its identity. Those
    /// two classes are exactly [`StrPreds::DECIMAL_INT`] and
    /// [`StrPreds::NON_DECIMAL_INT`], which is why this function can be written
    /// at all — the vocabulary to name the halves already exists.
    ///
    /// # The grid, probed at PHP 8.5.9
    ///
    /// | input | key | witness |
    /// | --- | --- | --- |
    /// | `int` | `int` | identity |
    /// | `decimal-int-string` | `int` | `'0'`, `'-1'`, `'9223372036854775807'` all cast |
    /// | `non-decimal-int-string` | `non-decimal-int-string` | `''`, `'00'`, `'+1'`, `' 1'`, `'1e3'`, `'-0'` all keep their identity |
    /// | `numeric-string` | `int \| numeric-string&non-decimal-int-string` | `'1'` casts, `'1.5'`/`' 1'`/`'1e3'` stay and stay numeric |
    /// | `string` | `int \| non-decimal-int-string` | the two halves, and nothing else |
    /// | `bool` | `int` | `true` is `1`, `false` is `0` |
    /// | `float` | — | declines; see below |
    ///
    /// The `numeric-string` and plain `string` rows are **sharper than
    /// `array-key`**: a string that survives the cast is by definition one PHP
    /// does not rewrite, which is a predicate this domain carries. They are
    /// two-base unions, which [`Fact::Union`] (issue #339) is the form for —
    /// before it, both declined.
    ///
    /// # What it declines, and why
    ///
    /// * **`float`** — PHP renders a float to string under the `precision` ini
    ///   directive at some of these seams, so the key would depend on the
    ///   runtime's configuration. (And the seams disagree: `$a[1.5]` truncates
    ///   to `1` while `array_fill_keys([1.5], v)` writes `'1.5'`.) A key this
    ///   crate cannot state without knowing a setting is a key it does not
    ///   state.
    /// * **`array`** — an illegal offset type, a `TypeError` rather than a key.
    /// * Anything whose base is not a scalar, and any fact whose own layer this
    ///   cannot express.
    ///
    /// A `Singleton`/`OneOf` of proven values is **not** handled here: those
    /// have exact keys, and the callers compute them value-by-value with the
    /// per-seam casts (which, as the float row shows, are not all this one).
    /// This is the abstract rung, for when nothing exact is available.
    #[must_use]
    pub fn array_key_cast(&self) -> Option<Fact> {
        let int = || Fact::General { base: Base::Int, nullable: false };
        let str_with = |p: StrPreds| Fact::refined(Base::String, Refinement::Str(p), false);
        let (base, preds) = match self {
            Fact::General { base, nullable: false } => (*base, StrPreds::empty()),
            Fact::Refined { base, refinement: Refinement::Str(p), nullable: false } => {
                (*base, *p)
            }
            // An int refinement survives the cast untouched — the key IS the
            // integer — so the fact passes straight through.
            Fact::Refined { base: Base::Int, refinement: Refinement::Int(_), nullable: false } => {
                return Some(self.clone());
            }
            _ => return None,
        };
        match base {
            Base::Int => Some(int()),
            // `true`/`false` cast to `1`/`0` (a `null` VALUE is a `Singleton`,
            // which this abstract rung does not handle — see the doc comment).
            Base::Bool => Some(int()),
            Base::String => {
                // A string already known to spell an integer casts whole; one
                // already known not to keeps its identity, predicates and all.
                if preds.contains_all(StrPreds::DECIMAL_INT) {
                    return Some(int());
                }
                if preds.contains_all(StrPreds::NON_DECIMAL_INT) {
                    return Some(str_with(preds));
                }
                // Otherwise both halves are live and the answer is a union:
                // `int` for the strings PHP rewrites, and — for the ones it
                // does not — every predicate the input had plus the fact that
                // it survived, which IS `non-decimal-int-string`. So a plain
                // `string` keys as `int|non-decimal-int-string` and a
                // `numeric-string` as `int|numeric-string&non-decimal-int-string`,
                // both strictly sharper than `array-key`.
                //
                // Before issue #339 this had nowhere to go — a `Fact` carried
                // one `Base` — and declined.
                int().join(&str_with(preds.union(StrPreds::NON_DECIMAL_INT).close()))
            }
            Base::Float => None,
        }
    }

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

    /// **The normalising union constructor** (issue #339) — the only way to
    /// build [`Fact::Union`], and what establishes its invariants.
    ///
    /// Arms are sorted by base and merged one-per-base: two arms of the same
    /// base join into one (their refinements joined, which is the *widening*
    /// join the single-base layers already use, so nothing here is a new
    /// semantics). A union that ends up with one arm collapses to
    /// `Refined`/`General`, and one with none is `None` — a fact must say
    /// something.
    #[must_use]
    pub fn union(arms: Vec<UnionArm>, nullable: bool) -> Option<Fact> {
        let mut merged: Vec<UnionArm> = Vec::with_capacity(arms.len());
        for (base, refinement) in arms {
            // A contentless refinement IS that base's General, and storing it
            // as `Some` would be a second spelling of one fact. `Fact::refined`
            // enforces this for the single-base layers; an arm is the same
            // layer one level in, so it gets the same rule.
            //
            // This is not tidiness: without it `join` is **not associative**.
            // `Singleton(1) ⊔ (Singleton('a') ⊔ numeric-string)` reaches the
            // string arm as `None` while `(Singleton(1) ⊔ Singleton('a')) ⊔
            // numeric-string` reaches it as `Some(<empty preds>)` — the same
            // denotation, two structures, and the vector universe finds 35698
            // of them.
            let refinement = refinement.filter(|r| !refinement_is_empty(*r));
            match merged.iter_mut().find(|(b, _)| *b == base) {
                Some(slot) => {
                    slot.1 = join_refinements(slot.1, refinement).filter(|r| !refinement_is_empty(*r));
                }
                None => merged.push((base, refinement)),
            }
        }
        merged.sort_by_key(|(b, _)| *b);
        match merged.len() {
            0 => None,
            1 => {
                let (base, refinement) = merged.pop().expect("len checked");
                Some(match refinement {
                    Some(r) => Fact::refined(base, r, nullable),
                    None => Fact::General { base, nullable },
                })
            }
            _ => Some(Fact::Union { arms: merged, nullable }),
        }
    }

    /// This fact's abstract arms — one entry for a single-base layer, several
    /// for a union — or `None` for a finite or array fact.
    fn abstract_arms(&self) -> Option<(Vec<UnionArm>, bool)> {
        match self {
            Fact::Refined { base, refinement, nullable } => {
                Some((vec![(*base, Some(*refinement))], *nullable))
            }
            Fact::General { base, nullable } => Some((vec![(*base, None)], *nullable)),
            Fact::Union { arms, nullable } => Some((arms.clone(), *nullable)),
            Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => None,
        }
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
            // Any arm admitting the value admits it — the arms are disjoint by
            // base, so at most one can even apply.
            Fact::Union { arms, nullable } => match v {
                Val::Null => *nullable,
                _ => arms.iter().any(|(base, refinement)| match refinement {
                    Some(r) => Fact::refined(*base, *r, false).admits(v),
                    None => v.base() == Some(*base),
                }),
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
            | Fact::Union { nullable, .. }
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
            // A union answers for every arm at once: `Yes` only where each
            // arm is, `No` only where none can be. A non-string arm is a
            // definite `No` on its own, so a mixed union is at best `Maybe`.
            Fact::Union { arms, nullable } => Certainty::all_of(arms.iter().map(|(base, refinement)| {
                match refinement {
                    Some(r) => Fact::refined(*base, *r, *nullable).satisfies_str(pred),
                    None => Fact::General { base: *base, nullable: *nullable }.satisfies_str(pred),
                }
            })),
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
            // A union answers for every arm at once: `Yes` only where each
            // arm is, `No` only where none can be. A non-int arm is a
            // definite `No` on its own, so a mixed union is at best `Maybe`.
            Fact::Union { arms, nullable } => Certainty::all_of(arms.iter().map(|(base, refinement)| {
                match refinement {
                    Some(r) => Fact::refined(*base, *r, *nullable).int_in(range),
                    None => Fact::General { base: *base, nullable: *nullable }.int_in(range),
                }
            })),
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
            // A union can be falsy if any arm can, and truthy if any arm can —
            // the arms are alternatives, so each side is a disjunction.
            Fact::Union { arms, nullable } => {
                let mut falsy = *nullable;
                let mut truthy = false;
                for (base, refinement) in arms {
                    let arm = match refinement {
                        Some(r) => Fact::refined(*base, *r, false),
                        None => Fact::General { base: *base, nullable: false },
                    };
                    let (f, t) = arm.abstract_falsy_truthy();
                    falsy |= f;
                    truthy |= t;
                }
                (falsy, truthy)
            }
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
    // **A mixed-base overflow becomes a union** (issue #339), where it used to
    // become nothing: `1|'x'` widening past `CAP` is `int|string`, not the
    // absence of a fact. Each base is summarized on its own members — the same
    // per-base computation as below — and the arms are then merged.
    if scalars.iter().any(|v| v.base() != Some(base)) {
        // …but only when every member HAS a scalar base. An array has none
        // (`Val::base()` is `None` for it) and is not an arm of this layer, so
        // a set mixing arrays with scalars still drops the fact whole. Keeping
        // the scalar members and silently losing the array would build a fact
        // that does not admit a value the set contained — the one thing the
        // widening must never do, and what the vector universe caught.
        if scalars.iter().any(|v| v.base().is_none()) {
            return None;
        }
        let mut arms: Vec<(Base, Option<Refinement>)> = Vec::new();
        for b in [Base::Int, Base::Float, Base::String, Base::Bool] {
            let members: Vec<Val> =
                scalars.iter().filter(|v| v.base() == Some(b)).map(|v| (*v).clone()).collect();
            if members.is_empty() {
                continue;
            }
            match summarize(&members) {
                Some(Fact::Refined { refinement, .. }) => arms.push((b, Some(refinement))),
                Some(Fact::General { .. }) => arms.push((b, None)),
                // A base whose own summary is unrepresentable widens to that
                // base's General, which is sound and keeps the arm.
                _ => arms.push((b, None)),
            }
        }
        return Fact::union(arms, nullable);
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
            // A union takes the nullability the same way — beside its arms
            // (issue #339). Before the union layer existed, `abs` could only be
            // one of the two above, which is what the old `unreachable!` said.
            Fact::Union { arms, .. } => Fact::union(arms.clone(), true),
            Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => {
                unreachable!("abs is abstract by caller contract")
            }
        },
        None => join_abstract(&summary, abs),
    }
}

/// The join of two abstract facts (issue #339).
///
/// It used to return `None` the moment the bases differed, which is where
/// `int|string` died: the finite layers carried `1|'x'` exactly and the moment
/// it widened past [`CAP`] there was no form left to hold it. With
/// [`Fact::Union`] there is, so this **concatenates the arms** and lets the
/// union constructor merge them per base — the same-base case is the old
/// refinement join, unchanged, and the different-base case is now a union
/// instead of a dropped fact.
///
/// It is total over the abstract layers: two facts with arms always join,
/// because a union of at most four bases always exists. `None` survives for one
/// reason only — a fact with no abstract arms at all (a finite or array fact),
/// which the caller routes elsewhere.
fn join_abstract(a: &Fact, b: &Fact) -> Option<Fact> {
    let (mut arms, anull) = a.abstract_arms()?;
    let (brms, bnull) = b.abstract_arms()?;
    arms.extend(brms);
    Fact::union(arms, anull || bnull)
}

/// Is this refinement contentless — the whole base rather than a part of it?
/// The predicate [`Fact::refined`] already applies when it collapses a `Refined`
/// to a `General`, lifted out so union arms can be held to the same invariant.
fn refinement_is_empty(r: Refinement) -> bool {
    match r {
        Refinement::Str(p) => p.is_empty(),
        Refinement::Int(q) => q.is_full(),
    }
}

/// Join two refinements **of the same base** (issue #339): the widening join
/// the single-base layers already use, lifted out so the union constructor and
/// `join_abstract` share one definition. `None` on either side is that base's
/// General, which absorbs.
fn join_refinements(a: Option<Refinement>, b: Option<Refinement>) -> Option<Refinement> {
    match (a, b) {
        (Some(Refinement::Str(p)), Some(Refinement::Str(q))) => Some(Refinement::Str(p.intersect(q))),
        (Some(Refinement::Int(r)), Some(Refinement::Int(s))) => Some(Refinement::Int(r.hull(s))),
        _ => None,
    }
}


#[cfg(test)]
mod tests {

    // ------------------------------------------------------------------
    // the array-key cast (issue #336)
    // ------------------------------------------------------------------

    /// Every expectation here is a probe at PHP 8.5.9: the value was used as an
    /// array key and the resulting key's type and identity read back.
    #[test]
    fn the_two_string_halves_are_the_whole_cast() {
        let s = |p: StrPreds| Fact::refined(Base::String, Refinement::Str(p), false);
        let int = Fact::General { base: Base::Int, nullable: false };

        // `decimal-int-string` always casts: '0', '-1', '9223372036854775807'.
        assert_eq!(s(StrPreds::DECIMAL_INT.close()).array_key_cast(), Some(int.clone()));
        // `non-decimal-int-string` never does, and keeps its identity — so it
        // keeps its predicates too: '', '00', '+1', ' 1', '1e3', '-0'.
        assert_eq!(
            s(StrPreds::NON_DECIMAL_INT).array_key_cast(),
            Some(s(StrPreds::NON_DECIMAL_INT))
        );
    }

    #[test]
    fn the_two_base_unions_are_now_expressible() {
        // A plain `string` keys as `int | non-decimal-int-string` and a
        // `numeric-string` as `int | numeric-string&non-decimal-int-string`.
        // Both used to decline for want of a form; `Fact::Union` is the form.
        let plain = Fact::General { base: Base::String, nullable: false }
            .array_key_cast()
            .expect("a string keys");
        assert!(plain.admits(&Val::Int(10)), "the int half: {plain:?}");
        assert!(plain.admits(&Val::Str("foo".into())), "the string half: {plain:?}");
        // `'10'` is NOT admitted as a string — PHP rewrote it to the int.
        assert!(!plain.admits(&Val::Str("10".into())), "too wide: {plain:?}");

        let numeric = Fact::refined(Base::String, Refinement::Str(StrPreds::NUMERIC), false)
            .array_key_cast()
            .expect("a numeric string keys");
        assert!(numeric.admits(&Val::Int(10)));
        assert!(numeric.admits(&Val::Str("1.5".into())), "numeric survivor: {numeric:?}");
        // A non-numeric string was never in the input, so it is not in the key.
        assert!(!numeric.admits(&Val::Str("foo".into())), "too wide: {numeric:?}");
    }

    #[test]
    fn an_int_passes_through_with_its_refinement() {
        let r = Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false);
        assert_eq!(r.array_key_cast(), Some(r.clone()));
        let g = Fact::General { base: Base::Int, nullable: false };
        assert_eq!(g.array_key_cast(), Some(g));
    }

    #[test]
    fn a_bool_casts_to_int() {
        assert_eq!(
            Fact::General { base: Base::Bool, nullable: false }.array_key_cast(),
            Some(Fact::General { base: Base::Int, nullable: false })
        );
    }

    #[test]
    fn a_float_declines_because_its_key_is_a_setting() {
        // PHP renders a float to string under the `precision` ini directive at
        // some of these seams, and the seams disagree with each other
        // (`$a[1.5]` truncates to 1, `array_fill_keys([1.5], v)` writes '1.5').
        assert_eq!(Fact::General { base: Base::Float, nullable: false }.array_key_cast(), None);
    }

    #[test]
    fn a_nullable_fact_declines() {
        // `null` is a value, not an abstract class this rung handles; a
        // nullable fact mixes the two and is left to the caller.
        assert_eq!(Fact::General { base: Base::String, nullable: true }.array_key_cast(), None);
    }
    use super::*;

    fn s(v: &str) -> Val {
        Val::Str(v.into())
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
    fn mixed_bases_join_into_a_union() {
        // This test used to pin `None` — `mixed_bases_are_unrepresentable` —
        // and that was the wall issue #339 removes. The join is now total over
        // the abstract layers, and each arm keeps its own refinement rather
        // than being flattened into the other's.
        let a = Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false);
        let b = Fact::refined(Base::String, Refinement::Str(StrPreds::NON_EMPTY), false);
        let j = a.join(&b).expect("mixed bases now join");
        assert!(j.admits(&Val::Int(5)) && j.admits(&s("x")));
        // …and neither arm was widened on the way: the int stays positive and
        // the string stays non-empty.
        assert!(!j.admits(&Val::Int(-5)), "the int refinement survived: {j:?}");
        assert!(!j.admits(&s("")), "the string refinement survived: {j:?}");
        assert!(!j.admits(&Val::Bool(true)), "no arm was invented: {j:?}");
        assert_eq!(j.is_null(), Certainty::No);
    }

    #[test]
    fn the_union_constructor_establishes_its_invariants() {
        let int = (Base::Int, None);
        let string = (Base::String, None);
        // Sorted by base, one entry per base.
        let Some(Fact::Union { arms, .. }) = Fact::union(vec![string, int], false) else {
            panic!("two bases make a union");
        };
        assert_eq!(arms, vec![(Base::Int, None), (Base::String, None)]);
        // One arm is not a union — it collapses to the single-base layer.
        assert_eq!(Fact::union(vec![int], false), Some(Fact::General { base: Base::Int, nullable: false }));
        // None is not a fact at all.
        assert_eq!(Fact::union(Vec::new(), false), None);
        // Duplicate bases merge through the refinement join rather than both
        // being kept: `int<1,max>` ⊔ `int<-5,-1>` is the hull.
        let pos = (Base::Int, Some(Refinement::Int(IntRange::POSITIVE)));
        let neg = (Base::Int, Some(Refinement::Int(IntRange::NEGATIVE)));
        let merged = Fact::union(vec![pos, neg, string], false).expect("a union");
        assert!(merged.admits(&Val::Int(5)) && merged.admits(&Val::Int(-5)));
    }

    #[test]
    fn a_mixed_overflow_summarizes_to_a_union() {
        // The everyday cliff: a finite mixed union that outgrows `CAP` used to
        // become NOTHING rather than `int|string`.
        let mut vals: Vec<Val> = (0..6).map(Val::Int).collect();
        vals.extend((0..6).map(|i| s(&format!("v{i}"))));
        let f = Fact::from_vals(vals).expect("an over-CAP mixed set summarizes");
        assert!(f.admits(&Val::Int(3)) && f.admits(&s("v3")));
        assert!(!f.admits(&Val::Bool(true)), "no arm was invented: {f:?}");
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
