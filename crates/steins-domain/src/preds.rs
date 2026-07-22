//! String refinement predicates as a closed bitset (ADR-0035).
//!
//! The set is deliberately closed: adding a predicate is one constant plus
//! its evaluator, and every interaction stays exhaustively checkable. The
//! implication closure (`Numeric ⇒ NonEmpty`, `NonFalsy ⇒ NonEmpty`) is
//! applied at construction so subset tests never miss an entailed fact.

use crate::php::{php_is_numeric, php_str_is_falsy};

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
        assert_eq!(StrPreds::of(""), StrPreds::empty());
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
