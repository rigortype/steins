//! Integer intervals — the canonical int refinement (ADR-0035).
//!
//! `positive-int`, `non-negative-int`, and phpdoc `int<lo, hi>` are all spellings of an
//! inclusive [`IntRange`] over PHP's 64-bit ints. Interval algebra (hull/intersection) is
//! total and canonical: no non-canonical form is constructible, so no normalization pass
//! exists.

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

    /// Construct an interval; `None` when `lo > hi` (empty; treated as contradiction).
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

    /// Convex hull — the join for value-set union. May over-approximate a union with gaps
    /// (measured widening, sound by construction).
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

/// A float interval — the denotation of phpdoc `float<lo, hi>` and the named
/// `positive-float` family.
///
/// Bounds are IEEE doubles, so `min`/`max` are `-INF`/`+INF` (both inclusive)
/// and `finite-float` is `[f64::MIN, f64::MAX]`. Each end is independently open
/// or closed so `positive-float` (`> 0`) needs no second variant.
///
/// Membership is the IEEE comparison itself, so **`NAN` is in no interval** —
/// `float<min, max>` included, which PHPStan's `FloatRangeType` documents as
/// "float without NAN" — and `-0.0` compares equal to `0.0`. Invariant: neither
/// bound is `NAN`, and the interval is not empty.
///
/// Unlike [`IntRange`] this has no hull/intersection algebra: no fact carries
/// it (the value lattice keeps a float as `General`), so it exists only to be
/// asked membership and containment questions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatRange {
    lo: f64,
    hi: f64,
    lo_open: bool,
    hi_open: bool,
}

impl FloatRange {
    /// `positive-float` — `> 0`, `INF` included.
    pub const POSITIVE: FloatRange =
        FloatRange { lo: 0.0, hi: f64::INFINITY, lo_open: true, hi_open: false };
    /// `negative-float` — `< 0`, `-INF` included.
    pub const NEGATIVE: FloatRange =
        FloatRange { lo: f64::NEG_INFINITY, hi: 0.0, lo_open: false, hi_open: true };
    /// `non-negative-float` — `>= 0`.
    pub const NON_NEGATIVE: FloatRange =
        FloatRange { lo: 0.0, hi: f64::INFINITY, lo_open: false, hi_open: false };
    /// `non-positive-float` — `<= 0`.
    pub const NON_POSITIVE: FloatRange =
        FloatRange { lo: f64::NEG_INFINITY, hi: 0.0, lo_open: false, hi_open: false };
    /// `finite-float` — every float except `INF`, `-INF` and `NAN`.
    pub const FINITE: FloatRange =
        FloatRange { lo: f64::MIN, hi: f64::MAX, lo_open: false, hi_open: false };

    /// The closed interval `[lo, hi]`; `None` for a `NAN` bound or `lo > hi`.
    #[must_use]
    pub fn new(lo: f64, hi: f64) -> Option<Self> {
        (lo <= hi).then_some(FloatRange { lo, hi, lo_open: false, hi_open: false })
    }

    /// Lower bound.
    #[must_use]
    pub const fn lo(self) -> f64 {
        self.lo
    }

    /// Upper bound.
    #[must_use]
    pub const fn hi(self) -> f64 {
        self.hi
    }

    /// Membership of a float; `NAN` is in no interval.
    #[must_use]
    pub fn contains(self, v: f64) -> bool {
        (if self.lo_open { self.lo < v } else { self.lo <= v })
            && (if self.hi_open { v < self.hi } else { v <= self.hi })
    }

    /// Whether `self` holds every member of `other`.
    #[must_use]
    pub fn contains_range(self, other: Self) -> bool {
        let lo_ok = self.lo < other.lo
            || (self.lo == other.lo && (!self.lo_open || other.lo_open));
        let hi_ok = other.hi < self.hi
            || (self.hi == other.hi && (!self.hi_open || other.hi_open));
        lo_ok && hi_ok
    }

    /// Membership of an int, by PHP's int-to-float widening (`5` satisfies
    /// `float<5, 6>`): the same value-equality reading as a float literal type.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn contains_int(self, v: i64) -> bool {
        self.contains(v as f64)
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
    fn float_ranges_are_ieee_intervals() {
        let r = FloatRange::new(0.0, 1.5).unwrap();
        assert!(r.contains(0.0) && r.contains(-0.0) && r.contains(1.5) && !r.contains(1.6));
        assert!(!r.contains(f64::NAN) && !r.contains(f64::INFINITY));
        assert!(r.contains_int(1) && !r.contains_int(2));
        assert!(FloatRange::new(2.0, 1.0).is_none() && FloatRange::new(f64::NAN, 1.0).is_none());
        assert!(!FloatRange::POSITIVE.contains(0.0) && FloatRange::POSITIVE.contains(f64::INFINITY));
        assert!(FloatRange::NEGATIVE.contains(f64::NEG_INFINITY) && !FloatRange::NEGATIVE.contains(-0.0));
        assert!(FloatRange::NON_NEGATIVE.contains(-0.0) && FloatRange::NON_POSITIVE.contains(0.0));
        let all = FloatRange::new(f64::NEG_INFINITY, f64::INFINITY).unwrap();
        assert!(!all.contains(f64::NAN) && all.contains(f64::INFINITY));
        let fin = FloatRange::FINITE;
        assert!(fin.contains(f64::MAX) && !fin.contains(f64::INFINITY) && !fin.contains(f64::NAN));
        assert!(FloatRange::NON_NEGATIVE.contains_range(FloatRange::POSITIVE));
        assert!(!FloatRange::POSITIVE.contains_range(FloatRange::NON_NEGATIVE));
        assert!(all.contains_range(fin) && !fin.contains_range(all) && all.contains_range(all));
    }

    #[test]
    fn hull_is_sound_for_members() {
        let a = IntRange::point(-3);
        let b = IntRange::point(7);
        let h = a.hull(b);
        assert!(h.contains(-3) && h.contains(7) && h.contains(0));
    }
}
