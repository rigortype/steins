//! String refinement predicates as a closed bitset (ADR-0035).
//!
//! Predicate sets are closed under the implications documented on
//! [`StrPreds::close`], so subset tests include entailed facts. Casing is
//! otherwise orthogonal to the length and numeric predicates.
//!
//! **The set is a conjunction of predicates, not a satisfiable-by-construction
//! one.** `DecimalInt` and `NonDecimalInt` are complementary, so the set that
//! carries both denotes ∅ — a legitimate value of the type (`union` builds it),
//! and the one place a `StrWith` contract admits no string at all. What the
//! representation cannot do is *reason* about that exclusion abstractly: a
//! subset test over positive literals can conclude "every string with these
//! predicates also has those", never "no string has both". See
//! `complementary_bits_are_a_conjunction_not_a_contradiction`.
//!
//! ## Extensional and contextual predicates (issue #236)
//!
//! Every predicate here is a property of the *value*, never of its provenance —
//! that is what keeps the Refined layer extensional, and it is why ADR-0038
//! keeps `literal-string` out (two identical strings differ in literal-string
//! status, so no function of the value decides it).
//!
//! [`StrPreds::CLASS_STRING`] is a value property in exactly that sense — two
//! identical strings are both class-strings or neither — but it is decided
//! against the program's **class table**, which [`StrPreds::of`] does not have.
//! So the bitset splits in two:
//!
//! - the **extensional** predicates ([`StrPreds::extensional`]), which
//!   [`StrPreds::of`] computes from the string alone; and
//! - the **contextual** ones, which the set can *record* (a producer that knows
//!   the class table puts them there) but no member test can *decide* here.
//!
//! Every membership query in this crate therefore reads a set through its
//! extensional projection, which over-approximates γ — the sound direction for
//! the join contract, and the value-domain face of the honest `Maybe` the
//! contract layer answers for the same question.

use crate::php::{
    php_is_numeric, php_str_is_decimal_int, php_str_is_falsy, php_str_is_lowercase,
    php_str_is_uppercase,
};

/// A set of string predicates, canonically closed under implication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct StrPreds(u8);

impl StrPreds {
    /// `non-empty-string`: the value is not `""`.
    pub const NON_EMPTY: StrPreds = StrPreds(1 << 0);
    /// `non-falsy-string`: the value is neither `""` nor `"0"`.
    pub const NON_FALSY: StrPreds = StrPreds(1 << 1);
    /// `numeric-string`: `is_numeric()` holds.
    pub const NUMERIC: StrPreds = StrPreds(1 << 2);
    /// `lowercase-string`: `strtolower()` leaves the value unchanged.
    ///
    /// Casing is orthogonal to the length and numeric predicates (`"1e5"` is
    /// lowercase, `"1E5"` is not, both are numeric; `""` is lowercase).
    /// `Lowercase` and `Uppercase` hold together exactly when the string has
    /// no cased character (`""`, `"123"`).
    pub const LOWERCASE: StrPreds = StrPreds(1 << 3);
    /// `uppercase-string`: `strtoupper()` leaves the value unchanged.
    pub const UPPERCASE: StrPreds = StrPreds(1 << 4);
    /// `decimal-int-string`: the string spells an integer the way PHP writes
    /// one back, so an array key made of it is cast to `int`
    /// ([`php_str_is_decimal_int`]).
    ///
    /// The one predicate here that entails others (see [`StrPreds::close`]).
    /// It does **not** entail `NonFalsy`: `"0"` is a decimal-int-string and is
    /// falsy.
    pub const DECIMAL_INT: StrPreds = StrPreds(1 << 5);
    /// `non-decimal-int-string`: the complement of `DECIMAL_INT` within
    /// `string` — every string that keeps its identity as an array key, which
    /// is wider than the name suggests (`"+1"`, `"00"`, `"18E+3"`, `"1.2"`,
    /// `"foo"` and `""` all qualify).
    ///
    /// Entails nothing and is entailed by nothing: its members range over the
    /// whole lattice. Being the *complement* of a sibling bit is not something
    /// the closure can express (see [`StrPreds`] module docs).
    pub const NON_DECIMAL_INT: StrPreds = StrPreds(1 << 6);
    /// `class-string`: the value names a class-like — a class, interface, trait
    /// or enum the program declares (issue #236).
    ///
    /// The one **contextual** predicate (see the module docs): a property of the
    /// value, but decided against the class table rather than against the
    /// characters, so [`StrPreds::of`] never sets it and no member test here can
    /// discharge it. It is set by a producer that holds the evidence — a
    /// declared `class-string` contract, or `self`/`parent`/`static::class`,
    /// where the class-like is resolved but its *spelling* is not (ADR-0043's
    /// casing deferral).
    ///
    /// Its implications are the ones every class-like name satisfies by PHP's
    /// own identifier grammar: a name is never `""` and never `"0"`
    /// (`NON_FALSY`, hence `NON_EMPTY`), and never a canonical decimal integer
    /// (`NON_DECIMAL_INT`) — an identifier cannot start with a digit. Those are
    /// extensional, so they *are* decidable, which is what lets a `class-string`
    /// contract refute `""` instead of shrugging at it.
    pub const CLASS_STRING: StrPreds = StrPreds(1 << 7);

    /// The predicates [`StrPreds::of`] can decide from the string alone.
    const EXTENSIONAL: StrPreds = StrPreds(0b0111_1111);

    /// The empty predicate set (no knowledge — the General form's content).
    #[must_use]
    pub const fn empty() -> Self {
        StrPreds(0)
    }

    /// True when no predicate is known.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Set union, then implication closure.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        StrPreds(self.0 | other.0).close()
    }

    /// Set intersection. (Closure is preserved by intersection of closed
    /// sets, since implications are Horn clauses over positive literals.)
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        StrPreds(self.0 & other.0)
    }

    /// Whether every predicate in `other` is present in `self`.
    #[must_use]
    pub const fn contains_all(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Apply the implication closure: `DecimalInt ⇒ Numeric`,
    /// `DecimalInt ⇒ Lowercase ∧ Uppercase`, `ClassString ⇒ NonFalsy ∧
    /// NonDecimalInt`, `NonFalsy ⇒ NonEmpty`, `Numeric ⇒ NonEmpty`.
    ///
    /// One pass reaches the fixpoint because the clauses are applied in
    /// dependency order: `DecimalInt`'s and `ClassString`'s consequents are
    /// discharged first, and the only clauses they feed (`NonFalsy ⇒ NonEmpty`,
    /// `Numeric ⇒ NonEmpty`) run after.
    #[must_use]
    pub const fn close(self) -> Self {
        let mut bits = self.0;
        if bits & StrPreds::DECIMAL_INT.0 != 0 {
            bits |= StrPreds::NUMERIC.0 | StrPreds::LOWERCASE.0 | StrPreds::UPPERCASE.0;
        }
        // PHP's identifier grammar, read as implications: a class-like name has
        // at least one character, is not `"0"`, and cannot be a canonical
        // decimal integer (it may not start with a digit).
        if bits & StrPreds::CLASS_STRING.0 != 0 {
            bits |= StrPreds::NON_FALSY.0 | StrPreds::NON_DECIMAL_INT.0;
        }
        if bits & (StrPreds::NON_FALSY.0 | StrPreds::NUMERIC.0) != 0 {
            bits |= StrPreds::NON_EMPTY.0;
        }
        StrPreds(bits)
    }

    /// This set with every contextual predicate dropped — the part
    /// [`StrPreds::of`] can decide, and the only part a membership test on a
    /// concrete string may consult.
    ///
    /// Dropping predicates *widens* the denotation, so reading a set through
    /// this projection over-approximates γ, which is the sound direction for
    /// every consumer (see the module docs).
    #[must_use]
    pub const fn extensional(self) -> Self {
        StrPreds(self.0 & StrPreds::EXTENSIONAL.0)
    }

    /// True when nothing in this set needs the class table — so a membership
    /// test on a concrete string is *exact* rather than an over-approximation.
    #[must_use]
    pub const fn is_extensional(self) -> bool {
        self.0 & !StrPreds::EXTENSIONAL.0 == 0
    }

    /// The full predicate summary of a concrete string — the computed
    /// widening seed (ADR-0035: precision loss is measured, not guessed).
    ///
    /// Extensional predicates only: [`StrPreds::CLASS_STRING`] is never set
    /// here, because the class table is not in scope. A `Foo::class` literal
    /// therefore widens to a set that has *forgotten* it names a class — a
    /// widening, never a lie, and the reason the class-string producer records
    /// the bit at the site that holds the evidence instead of re-deriving it.
    #[must_use]
    pub fn of(s: impl AsRef<[u8]>) -> Self {
        let s = s.as_ref();
        let mut p = StrPreds::empty();
        if !s.is_empty() {
            p = p.union(StrPreds::NON_EMPTY);
        }
        if !php_str_is_falsy(s) {
            p = p.union(StrPreds::NON_FALSY);
        }
        if php_is_numeric(s) {
            p = p.union(StrPreds::NUMERIC);
        }
        if php_str_is_lowercase(s) {
            p = p.union(StrPreds::LOWERCASE);
        }
        if php_str_is_uppercase(s) {
            p = p.union(StrPreds::UPPERCASE);
        }
        if php_str_is_decimal_int(s) {
            p = p.union(StrPreds::DECIMAL_INT);
        } else {
            p = p.union(StrPreds::NON_DECIMAL_INT);
        }
        p
    }

    /// Evaluate a single predicate (one of the constants) on a concrete
    /// string.
    ///
    /// A contextual predicate is read through [`StrPreds::extensional`], so a
    /// `true` here means "not refuted by the characters", not "proven" — ask
    /// [`StrPreds::is_extensional`] first when the difference matters.
    #[must_use]
    pub fn eval(pred: StrPreds, s: impl AsRef<[u8]>) -> bool {
        StrPreds::of(s).contains_all(pred.extensional())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_applies_implications() {
        assert!(StrPreds::NUMERIC.close().contains_all(StrPreds::NON_EMPTY));
        assert!(StrPreds::NON_FALSY.close().contains_all(StrPreds::NON_EMPTY));
    }

    #[test]
    fn summaries() {
        // `""` knows nothing on the length/numeric axis, but it *is* unchanged
        // by both case functions — the empty string satisfies both casings —
        // and it is not a decimal-int-string, so it carries the complement bit.
        assert_eq!(
            StrPreds::of(""),
            StrPreds::LOWERCASE.union(StrPreds::UPPERCASE).union(StrPreds::NON_DECIMAL_INT)
        );
        // "0": non-empty but falsy and numeric.
        let zero = StrPreds::of("0");
        assert!(zero.contains_all(StrPreds::NON_EMPTY));
        assert!(zero.contains_all(StrPreds::NUMERIC));
        assert!(!zero.contains_all(StrPreds::NON_FALSY));
        // "abc": non-empty, non-falsy, not numeric.
        let abc = StrPreds::of("abc");
        assert!(abc.contains_all(StrPreds::NON_FALSY));
        assert!(!abc.contains_all(StrPreds::NUMERIC));
    }

    #[test]
    fn casing_summaries_follow_the_fixtures() {
        // The four conformance fixtures, as predicate summaries.
        let abc = StrPreds::of("abc");
        assert!(abc.contains_all(StrPreds::LOWERCASE));
        assert!(!abc.contains_all(StrPreds::UPPERCASE));
        let upper = StrPreds::of("ABC");
        assert!(upper.contains_all(StrPreds::UPPERCASE));
        assert!(!upper.contains_all(StrPreds::LOWERCASE));
        // A single cased character decides the whole string.
        assert!(!StrPreds::of("abC").contains_all(StrPreds::LOWERCASE));
        assert!(!StrPreds::of("ABc").contains_all(StrPreds::UPPERCASE));
        // Nothing to case: both hold at once, and `''` is the one that also
        // fails the length half — the two halves fail independently.
        for uncased in ["", "123"] {
            let p = StrPreds::of(uncased);
            assert!(p.contains_all(StrPreds::LOWERCASE), "{uncased:?}");
            assert!(p.contains_all(StrPreds::UPPERCASE), "{uncased:?}");
        }
        assert!(!StrPreds::of("").contains_all(StrPreds::NON_EMPTY));
        assert!(StrPreds::of("123").contains_all(StrPreds::NON_EMPTY));
    }

    #[test]
    fn casing_is_orthogonal_to_the_closure() {
        // Casing entails nothing, and nothing on the length/numeric axis
        // entails casing. (`DecimalInt` does — its alphabet has no cased
        // character — which is a clause *into* casing, not out of it.)
        assert_eq!(StrPreds::LOWERCASE.close(), StrPreds::LOWERCASE);
        assert_eq!(StrPreds::UPPERCASE.close(), StrPreds::UPPERCASE);
        assert!(!StrPreds::LOWERCASE.contains_all(StrPreds::NON_EMPTY));
        // `"1e5"` is numeric and lowercase; `"1E5"` is numeric and uppercase —
        // so `Numeric` cannot entail either casing.
        assert!(StrPreds::of("1e5").contains_all(StrPreds::NUMERIC.union(StrPreds::LOWERCASE)));
        assert!(!StrPreds::of("1e5").contains_all(StrPreds::UPPERCASE));
        assert!(StrPreds::of("1E5").contains_all(StrPreds::NUMERIC.union(StrPreds::UPPERCASE)));
        assert!(!StrPreds::of("1E5").contains_all(StrPreds::LOWERCASE));
        // Every predicate *except the complementary pair* is jointly
        // satisfiable — `"5"` witnesses it, which is why the abstract-fact leg
        // of `admits` can never refute a `StrWith` drawn from these.
        let all = StrPreds::NON_FALSY
            .union(StrPreds::NUMERIC)
            .union(StrPreds::LOWERCASE)
            .union(StrPreds::UPPERCASE)
            .union(StrPreds::DECIMAL_INT);
        assert!(StrPreds::of("5").contains_all(all));
    }

    #[test]
    fn decimal_int_closure_and_the_fixture_summaries() {
        // The clause set, each direction pinned separately.
        let d = StrPreds::DECIMAL_INT.close();
        assert!(d.contains_all(StrPreds::NUMERIC));
        assert!(d.contains_all(StrPreds::NON_EMPTY));
        assert!(d.contains_all(StrPreds::LOWERCASE));
        assert!(d.contains_all(StrPreds::UPPERCASE));
        // …and the one clause that is NOT entailed: `"0"` is a
        // decimal-int-string and PHP calls it falsy.
        assert!(!d.contains_all(StrPreds::NON_FALSY));
        assert!(StrPreds::of("0").contains_all(StrPreds::DECIMAL_INT));
        assert!(!StrPreds::of("0").contains_all(StrPreds::NON_FALSY));
        // Nothing entails `DecimalInt` — numeric is the near miss the fixture
        // is built around (`"007"` is numeric, not decimal).
        assert!(!StrPreds::NUMERIC.close().contains_all(StrPreds::DECIMAL_INT));
        assert!(StrPreds::of("007").contains_all(StrPreds::NUMERIC));
        assert!(!StrPreds::of("007").contains_all(StrPreds::DECIMAL_INT));
        // `NonDecimalInt` is a closure leaf in both directions.
        assert_eq!(StrPreds::NON_DECIMAL_INT.close(), StrPreds::NON_DECIMAL_INT);
        // The two fixture accept-sets, as summaries.
        for decimal in ["123", "-1", "0"] {
            let p = StrPreds::of(decimal);
            assert!(p.contains_all(StrPreds::DECIMAL_INT), "{decimal:?}");
            assert!(!p.contains_all(StrPreds::NON_DECIMAL_INT), "{decimal:?}");
        }
        for non_decimal in ["00", "1.2", "foo", "+1", "007", "abc", ""] {
            let p = StrPreds::of(non_decimal);
            assert!(p.contains_all(StrPreds::NON_DECIMAL_INT), "{non_decimal:?}");
            assert!(!p.contains_all(StrPreds::DECIMAL_INT), "{non_decimal:?}");
        }
    }

    #[test]
    fn complementary_bits_are_a_conjunction_not_a_contradiction() {
        // The set carrying both is reachable (`union` builds it) and denotes ∅:
        // no string satisfies it, so `contains_all` refuses it for every atom.
        let bottom = StrPreds::DECIMAL_INT.union(StrPreds::NON_DECIMAL_INT);
        assert_eq!(bottom, bottom.close());
        for s in ["", "0", "007", "5", "foo", "-1"] {
            assert!(!StrPreds::of(s).contains_all(bottom), "{s:?}");
        }
        // What the representation cannot do: conclude `No` *abstractly*. A set
        // knowing `DecimalInt` does not "contain" `NonDecimalInt`, which reads
        // as "not proven", not as "refuted" — the negation ceiling, and the
        // reason the abstract-fact leg of `admits` answers `Maybe` there.
        assert!(!StrPreds::DECIMAL_INT.close().contains_all(StrPreds::NON_DECIMAL_INT));
        assert!(!StrPreds::NON_DECIMAL_INT.contains_all(StrPreds::DECIMAL_INT));
    }

    #[test]
    fn class_string_is_contextual_and_carries_the_identifier_grammar() {
        let cs = StrPreds::CLASS_STRING.close();
        // The implications PHP's identifier grammar hands us, and they are
        // extensional — which is what makes them decidable.
        assert!(cs.contains_all(StrPreds::NON_FALSY));
        assert!(cs.contains_all(StrPreds::NON_EMPTY));
        assert!(cs.contains_all(StrPreds::NON_DECIMAL_INT));
        // Nothing extensional entails it back: `"Foo"` looks exactly like every
        // other non-falsy identifier-shaped string.
        assert!(!StrPreds::of("Foo").contains_all(StrPreds::CLASS_STRING));
        assert!(!StrPreds::NON_FALSY.close().contains_all(StrPreds::CLASS_STRING));
        // The split: only this bit is contextual.
        assert!(!cs.is_extensional());
        assert!(cs.extensional().is_extensional());
        assert_eq!(cs.extensional(), StrPreds::NON_FALSY.union(StrPreds::NON_DECIMAL_INT));
        for extensional in [
            StrPreds::empty(),
            StrPreds::NON_EMPTY,
            StrPreds::NUMERIC.close(),
            StrPreds::DECIMAL_INT.close(),
            StrPreds::LOWERCASE,
        ] {
            assert!(extensional.is_extensional(), "{extensional:?}");
        }
    }

    #[test]
    fn class_string_eval_refutes_but_never_proves() {
        // Refuted by the characters — no class-like is named `""`, `"0"` or `"7"`.
        for refuted in ["", "0", "7", "123"] {
            assert!(!StrPreds::eval(StrPreds::CLASS_STRING.close(), refuted), "{refuted:?}");
        }
        // Not refuted — and `eval` says so without claiming the class exists.
        for open in ["Foo", "App\\User", "stdClass"] {
            assert!(StrPreds::eval(StrPreds::CLASS_STRING.close(), open), "{open:?}");
        }
    }

    #[test]
    fn class_string_widens_away_under_intersection() {
        // The meet with a plain non-empty string forgets the class-like claim —
        // the widening direction, which is the safe one for a join.
        let joined = StrPreds::CLASS_STRING.close().intersect(StrPreds::NON_EMPTY);
        assert!(!joined.contains_all(StrPreds::CLASS_STRING));
        assert!(joined.contains_all(StrPreds::NON_EMPTY));
    }

    #[test]
    fn intersection_of_closed_sets_stays_closed() {
        let a = StrPreds::of("5");   // numeric, non-empty, non-falsy
        let b = StrPreds::of("0");   // numeric, non-empty
        let i = a.intersect(b);
        assert_eq!(i, i.close());
        assert!(i.contains_all(StrPreds::NUMERIC));
        assert!(i.contains_all(StrPreds::NON_EMPTY));
        assert!(!i.contains_all(StrPreds::NON_FALSY));
    }
}
