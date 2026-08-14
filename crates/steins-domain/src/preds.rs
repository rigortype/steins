//! String refinement predicates as a closed bitset (ADR-0035).
//!
//! Predicate sets are closed under the implications on [`StrPreds::close`], so subset tests
//! include entailed facts. Casing is orthogonal to the length and numeric predicates.
//!
//! **A conjunction of predicates, not satisfiable-by-construction.** `DecimalInt` and
//! `NonDecimalInt` are complementary, so a set carrying both denotes ∅ — reachable via
//! `union`, and the one way a `StrWith` contract admits no string. The representation cannot
//! reason about that exclusion abstractly: a subset test over positive literals proves
//! entailment, never refutation.
//!
//! ## Extensional and contextual predicates (issue #236)
//!
//! Every predicate is a property of the *value*, never its provenance — why the Refined
//! layer stays extensional, and why ADR-0038 excludes `literal-string` (identical strings
//! can differ in that status).
//!
//! [`StrPreds::CLASS_STRING`] is a value property too, but decided against the program's
//! class table, which [`StrPreds::of`] lacks. So the bitset splits: **extensional**
//! predicates ([`StrPreds::extensional`]), computed from the string alone, and
//! **contextual** ones, which a set can record but no member test here can decide.
//!
//! Every membership query reads a set through its extensional projection, which
//! over-approximates γ — sound for the join contract, and this crate's face of the contract
//! layer's honest `Maybe`.

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
    /// `Lowercase` and `Uppercase` hold together exactly when the string has no cased
    /// character (`""`, `"123"`); e.g. `"1e5"` is lowercase-numeric, `"1E5"` is
    /// uppercase-numeric.
    pub const LOWERCASE: StrPreds = StrPreds(1 << 3);
    /// `uppercase-string`: `strtoupper()` leaves the value unchanged.
    pub const UPPERCASE: StrPreds = StrPreds(1 << 4);
    /// `decimal-int-string`: the string spells an integer the way PHP writes one back, so an
    /// array key made of it casts to `int` ([`php_str_is_decimal_int`]).
    ///
    /// Entails others (see [`StrPreds::close`]) but not `NonFalsy`: `"0"` is a
    /// decimal-int-string and falsy.
    pub const DECIMAL_INT: StrPreds = StrPreds(1 << 5);
    /// `non-decimal-int-string`: the complement of `DECIMAL_INT` within `string` — every
    /// string keeping its identity as an array key, wider than the name suggests (`"+1"`,
    /// `"00"`, `"18E+3"`, `"1.2"`, `"foo"`, `""` all qualify).
    ///
    /// Entails and is entailed by nothing (the closure can't express a complement — see
    /// module docs).
    pub const NON_DECIMAL_INT: StrPreds = StrPreds(1 << 6);
    /// `class-string`: names a class-like — class, interface, trait, or enum the program
    /// declares (issue #236).
    ///
    /// The one **contextual** predicate (module docs): decided against the class table, not
    /// the characters, so [`StrPreds::of`] never sets it; a producer holding the evidence
    /// sets it (a declared contract, or `self`/`parent`/`static::class` — resolved class,
    /// unresolved spelling, ADR-0043's casing deferral).
    ///
    /// Implications follow PHP's identifier grammar (never `""`/`"0"`, never a canonical
    /// decimal integer since identifiers can't start with a digit) and are extensional, so a
    /// `class-string` contract can refute `""`.
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

    /// Set intersection — closure-preserving (implications are Horn clauses over positive
    /// literals).
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        StrPreds(self.0 & other.0)
    }

    /// Whether every predicate in `other` is present in `self`.
    #[must_use]
    pub const fn contains_all(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Implication closure: `DecimalInt ⇒ Numeric`, `DecimalInt ⇒ Lowercase ∧ Uppercase`,
    /// `ClassString ⇒ NonFalsy ∧ NonDecimalInt`, `NonFalsy ⇒ NonEmpty`, `Numeric ⇒
    /// NonEmpty`.
    ///
    /// One pass reaches the fixpoint: `DecimalInt`/`ClassString` consequents are discharged
    /// first, feeding the two `⇒ NonEmpty` clauses that run after.
    #[must_use]
    pub const fn close(self) -> Self {
        let mut bits = self.0;
        if bits & StrPreds::DECIMAL_INT.0 != 0 {
            bits |= StrPreds::NUMERIC.0 | StrPreds::LOWERCASE.0 | StrPreds::UPPERCASE.0;
        }
        if bits & StrPreds::CLASS_STRING.0 != 0 {
            bits |= StrPreds::NON_FALSY.0 | StrPreds::NON_DECIMAL_INT.0;
        }
        if bits & (StrPreds::NON_FALSY.0 | StrPreds::NUMERIC.0) != 0 {
            bits |= StrPreds::NON_EMPTY.0;
        }
        StrPreds(bits)
    }

    /// This set with contextual predicates dropped — what [`StrPreds::of`] can decide, and
    /// the only part a membership test may consult.
    ///
    /// Dropping predicates *widens* the denotation, over-approximating γ — the sound
    /// direction (see module docs).
    #[must_use]
    pub const fn extensional(self) -> Self {
        StrPreds(self.0 & StrPreds::EXTENSIONAL.0)
    }

    /// True when nothing here needs the class table, so a membership test is *exact* rather
    /// than an over-approximation.
    #[must_use]
    pub const fn is_extensional(self) -> bool {
        self.0 & !StrPreds::EXTENSIONAL.0 == 0
    }

    /// The full predicate summary of a concrete string — the computed widening seed
    /// (ADR-0035: precision loss is measured, not guessed).
    ///
    /// Extensional only: [`StrPreds::CLASS_STRING`] is never set here (no class table in
    /// scope), so a `Foo::class` literal widens to a set that's forgotten it names a class —
    /// never a lie, which is why the class-string producer records the bit at its evidence
    /// site instead of re-deriving it.
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

    /// Evaluate a single predicate (one of the constants) on a concrete string.
    ///
    /// A contextual predicate reads through [`StrPreds::extensional`], so `true` means "not
    /// refuted", not "proven" — check [`StrPreds::is_extensional`] when the difference
    /// matters.
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
        assert_eq!(
            StrPreds::of(""),
            StrPreds::LOWERCASE.union(StrPreds::UPPERCASE).union(StrPreds::NON_DECIMAL_INT)
        );
        let zero = StrPreds::of("0");
        assert!(zero.contains_all(StrPreds::NON_EMPTY));
        assert!(zero.contains_all(StrPreds::NUMERIC));
        assert!(!zero.contains_all(StrPreds::NON_FALSY));
        let abc = StrPreds::of("abc");
        assert!(abc.contains_all(StrPreds::NON_FALSY));
        assert!(!abc.contains_all(StrPreds::NUMERIC));
    }

    #[test]
    fn casing_summaries_follow_the_fixtures() {
        let abc = StrPreds::of("abc");
        assert!(abc.contains_all(StrPreds::LOWERCASE));
        assert!(!abc.contains_all(StrPreds::UPPERCASE));
        let upper = StrPreds::of("ABC");
        assert!(upper.contains_all(StrPreds::UPPERCASE));
        assert!(!upper.contains_all(StrPreds::LOWERCASE));
        assert!(!StrPreds::of("abC").contains_all(StrPreds::LOWERCASE));
        assert!(!StrPreds::of("ABc").contains_all(StrPreds::UPPERCASE));
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
        // `DecimalInt` entails casing (its alphabet has no cased character); nothing else
        // does, in either direction.
        assert_eq!(StrPreds::LOWERCASE.close(), StrPreds::LOWERCASE);
        assert_eq!(StrPreds::UPPERCASE.close(), StrPreds::UPPERCASE);
        assert!(!StrPreds::LOWERCASE.contains_all(StrPreds::NON_EMPTY));
        assert!(StrPreds::of("1e5").contains_all(StrPreds::NUMERIC.union(StrPreds::LOWERCASE)));
        assert!(!StrPreds::of("1e5").contains_all(StrPreds::UPPERCASE));
        assert!(StrPreds::of("1E5").contains_all(StrPreds::NUMERIC.union(StrPreds::UPPERCASE)));
        assert!(!StrPreds::of("1E5").contains_all(StrPreds::LOWERCASE));
        // `"5"` witnesses joint satisfiability of every predicate but the complementary pair
        // — why `admits`'s abstract-fact leg can't refute a `StrWith` drawn from these.
        let all = StrPreds::NON_FALSY
            .union(StrPreds::NUMERIC)
            .union(StrPreds::LOWERCASE)
            .union(StrPreds::UPPERCASE)
            .union(StrPreds::DECIMAL_INT);
        assert!(StrPreds::of("5").contains_all(all));
    }

    #[test]
    fn decimal_int_closure_and_the_fixture_summaries() {
        let d = StrPreds::DECIMAL_INT.close();
        assert!(d.contains_all(StrPreds::NUMERIC));
        assert!(d.contains_all(StrPreds::NON_EMPTY));
        assert!(d.contains_all(StrPreds::LOWERCASE));
        assert!(d.contains_all(StrPreds::UPPERCASE));
        // NOT entailed: `"0"` is decimal-int and falsy.
        assert!(!d.contains_all(StrPreds::NON_FALSY));
        assert!(StrPreds::of("0").contains_all(StrPreds::DECIMAL_INT));
        assert!(!StrPreds::of("0").contains_all(StrPreds::NON_FALSY));
        // Nothing entails `DecimalInt`; `"007"` is the numeric-but-not-decimal near miss.
        assert!(!StrPreds::NUMERIC.close().contains_all(StrPreds::DECIMAL_INT));
        assert!(StrPreds::of("007").contains_all(StrPreds::NUMERIC));
        assert!(!StrPreds::of("007").contains_all(StrPreds::DECIMAL_INT));
        assert_eq!(StrPreds::NON_DECIMAL_INT.close(), StrPreds::NON_DECIMAL_INT);
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
        // Reachable via `union`, denotes ∅: no string satisfies it.
        let bottom = StrPreds::DECIMAL_INT.union(StrPreds::NON_DECIMAL_INT);
        assert_eq!(bottom, bottom.close());
        for s in ["", "0", "007", "5", "foo", "-1"] {
            assert!(!StrPreds::of(s).contains_all(bottom), "{s:?}");
        }
        // The negation ceiling: knowing `DecimalInt` doesn't "contain" `NonDecimalInt` —
        // reads as "not proven", not "refuted", which is why `admits`'s abstract-fact leg
        // answers `Maybe` here.
        assert!(!StrPreds::DECIMAL_INT.close().contains_all(StrPreds::NON_DECIMAL_INT));
        assert!(!StrPreds::NON_DECIMAL_INT.contains_all(StrPreds::DECIMAL_INT));
    }

    #[test]
    fn class_string_is_contextual_and_carries_the_identifier_grammar() {
        let cs = StrPreds::CLASS_STRING.close();
        assert!(cs.contains_all(StrPreds::NON_FALSY));
        assert!(cs.contains_all(StrPreds::NON_EMPTY));
        assert!(cs.contains_all(StrPreds::NON_DECIMAL_INT));
        // Nothing extensional entails it back — `"Foo"` looks like any non-falsy
        // identifier-shaped string.
        assert!(!StrPreds::of("Foo").contains_all(StrPreds::CLASS_STRING));
        assert!(!StrPreds::NON_FALSY.close().contains_all(StrPreds::CLASS_STRING));
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
        for refuted in ["", "0", "7", "123"] {
            assert!(!StrPreds::eval(StrPreds::CLASS_STRING.close(), refuted), "{refuted:?}");
        }
        for open in ["Foo", "App\\User", "stdClass"] {
            assert!(StrPreds::eval(StrPreds::CLASS_STRING.close(), open), "{open:?}");
        }
    }

    #[test]
    fn class_string_widens_away_under_intersection() {
        let joined = StrPreds::CLASS_STRING.close().intersect(StrPreds::NON_EMPTY);
        assert!(!joined.contains_all(StrPreds::CLASS_STRING));
        assert!(joined.contains_all(StrPreds::NON_EMPTY));
    }

    #[test]
    fn intersection_of_closed_sets_stays_closed() {
        let a = StrPreds::of("5");
        let b = StrPreds::of("0");
        let i = a.intersect(b);
        assert_eq!(i, i.close());
        assert!(i.contains_all(StrPreds::NUMERIC));
        assert!(i.contains_all(StrPreds::NON_EMPTY));
        assert!(!i.contains_all(StrPreds::NON_FALSY));
    }
}
