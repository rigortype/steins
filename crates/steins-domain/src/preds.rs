//! String refinement predicates as a closed bitset (ADR-0035).
//!
//! The set is deliberately closed: adding a predicate is one constant plus
//! its evaluator, and every interaction stays exhaustively checkable. The
//! implication closure (`Numeric ⇒ NonEmpty`, `NonFalsy ⇒ NonEmpty`) is
//! applied at construction so subset tests never miss an entailed fact. The
//! casing predicates add no implication in either direction — that is the
//! design claim `casing_is_orthogonal_to_the_closure` pins.

use crate::php::{php_is_numeric, php_str_is_falsy, php_str_is_lowercase, php_str_is_uppercase};

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
    /// Orthogonal to every predicate above, and deliberately so: `"1e5"` is
    /// numeric *and* lowercase, `"1E5"` is numeric and **not** lowercase, so
    /// `Numeric` entails no casing and no casing entails `NonEmpty` (`""` is
    /// lowercase). The one entailment worth naming is the one that is *not*
    /// exclusion: `Lowercase` and `Uppercase` hold together exactly when the
    /// string has no cased character at all (`""`, `"123"`).
    pub const LOWERCASE: StrPreds = StrPreds(1 << 3);
    /// `uppercase-string`: `strtoupper()` leaves the value unchanged.
    pub const UPPERCASE: StrPreds = StrPreds(1 << 4);

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

    /// Apply the implication closure: `NonFalsy ⇒ NonEmpty`,
    /// `Numeric ⇒ NonEmpty`.
    #[must_use]
    pub const fn close(self) -> Self {
        let mut bits = self.0;
        if bits & (StrPreds::NON_FALSY.0 | StrPreds::NUMERIC.0) != 0 {
            bits |= StrPreds::NON_EMPTY.0;
        }
        StrPreds(bits)
    }

    /// The full predicate summary of a concrete string — the computed
    /// widening seed (ADR-0035: precision loss is measured, not guessed).
    #[must_use]
    pub fn of(s: &str) -> Self {
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
        p
    }

    /// Evaluate a single predicate (one of the constants) on a concrete
    /// string.
    #[must_use]
    pub fn eval(pred: StrPreds, s: &str) -> bool {
        StrPreds::of(s).contains_all(pred)
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
        // by both case functions — the empty string satisfies both casings.
        assert_eq!(StrPreds::of(""), StrPreds::LOWERCASE.union(StrPreds::UPPERCASE));
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
        // Casing entails nothing, and nothing entails casing.
        assert_eq!(StrPreds::LOWERCASE.close(), StrPreds::LOWERCASE);
        assert_eq!(StrPreds::UPPERCASE.close(), StrPreds::UPPERCASE);
        assert!(!StrPreds::LOWERCASE.contains_all(StrPreds::NON_EMPTY));
        // `"1e5"` is numeric and lowercase; `"1E5"` is numeric and uppercase —
        // so `Numeric` cannot entail either casing.
        assert!(StrPreds::of("1e5").contains_all(StrPreds::NUMERIC.union(StrPreds::LOWERCASE)));
        assert!(!StrPreds::of("1e5").contains_all(StrPreds::UPPERCASE));
        assert!(StrPreds::of("1E5").contains_all(StrPreds::NUMERIC.union(StrPreds::UPPERCASE)));
        assert!(!StrPreds::of("1E5").contains_all(StrPreds::LOWERCASE));
        // Every predicate is jointly satisfiable — `"5"` witnesses it, which is
        // why the abstract-fact leg of `admits` can never refute a `StrWith`.
        let all = StrPreds::NON_FALSY
            .union(StrPreds::NUMERIC)
            .union(StrPreds::LOWERCASE)
            .union(StrPreds::UPPERCASE);
        assert!(StrPreds::of("5").contains_all(all));
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
