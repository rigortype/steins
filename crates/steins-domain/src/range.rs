//! Integer intervals — the canonical int refinement (ADR-0035).
//!
//! `positive-int`, `non-negative-int`, and phpdoc `int<lo, hi>` are all
//! spellings of an inclusive [`IntRange`] over PHP's 64-bit ints; `min`/`max`
//! are the domain bounds. Interval algebra (hull/intersection) is total and
//! canonical — no normalization pass exists because no non-canonical form
//! can be constructed.

/// An inclusive integer interval. Invariant: `lo <= hi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntRange {
    lo: i64,
    hi: i64,
}

impl IntRange {
    /// The full `int` domain.
    pub const FULL: IntRange = IntRange { lo: i64::MIN, hi: i64::MAX };
    /// `positive-int` (`int<1, max>`).
    pub const POSITIVE: IntRange = IntRange { lo: 1, hi: i64::MAX };
    /// `negative-int` (`int<min, -1>`).
    pub const NEGATIVE: IntRange = IntRange { lo: i64::MIN, hi: -1 };
    /// `non-negative-int` (`int<0, max>`).
    pub const NON_NEGATIVE: IntRange = IntRange { lo: 0, hi: i64::MAX };

    /// Construct an interval; returns `None` when `lo > hi` (empty — the
    /// domain has no empty fact; callers treat it as contradiction).
    #[must_use]
    pub const fn new(lo: i64, hi: i64) -> Option<Self> {
        if lo <= hi { Some(IntRange { lo, hi }) } else { None }
    }

    /// The single-point interval.
    #[must_use]
    pub const fn point(v: i64) -> Self {
        IntRange { lo: v, hi: v }
    }

    /// Lower bound (inclusive).
    #[must_use]
    pub const fn lo(self) -> i64 {
        self.lo
    }

    /// Upper bound (inclusive).
    #[must_use]
    pub const fn hi(self) -> i64 {
        self.hi
    }

    /// Whether this is the whole `int` domain (i.e. no knowledge).
    #[must_use]
    pub const fn is_full(self) -> bool {
        self.lo == i64::MIN && self.hi == i64::MAX
    }

    /// Membership.
    #[must_use]
    pub const fn contains(self, v: i64) -> bool {
        self.lo <= v && v <= self.hi
    }

    /// Whether `self` contains every point of `other`.
    #[must_use]
    pub const fn contains_range(self, other: Self) -> bool {
        self.lo <= other.lo && other.hi <= self.hi
    }

    /// Convex hull — the join for value-set union (may over-approximate a
    /// union with gaps; that is the measured widening, sound by
    /// construction).
    #[must_use]
    pub const fn hull(self, other: Self) -> Self {
        IntRange {
            lo: if self.lo < other.lo { self.lo } else { other.lo },
            hi: if self.hi > other.hi { self.hi } else { other.hi },
        }
    }

    /// Intersection — the meet; `None` when disjoint.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Option<Self> {
        let lo = if self.lo > other.lo { self.lo } else { other.lo };
        let hi = if self.hi < other.hi { self.hi } else { other.hi };
        IntRange::new(lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn algebra() {
        let a = IntRange::new(0, 10).unwrap();
        let b = IntRange::new(5, 20).unwrap();
        assert_eq!(a.hull(b), IntRange::new(0, 20).unwrap());
        assert_eq!(a.intersect(b), IntRange::new(5, 10));
        assert_eq!(a.intersect(IntRange::new(11, 12).unwrap()), None);
        assert!(IntRange::FULL.contains_range(a));
        assert!(IntRange::POSITIVE.contains(1) && !IntRange::POSITIVE.contains(0));
    }

    #[test]
    fn hull_is_sound_for_members() {
        let a = IntRange::point(-3);
        let b = IntRange::point(7);
        let h = a.hull(b);
        assert!(h.contains(-3) && h.contains(7) && h.contains(0));
    }
}
