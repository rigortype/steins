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

/// The ordinal of `f64::INFINITY` — the largest a non-`NAN` double maps to.
const ORD_INF: i64 = 0x7FF0_0000_0000_0000;

/// A non-`NAN` double's position in PHP's comparison order, as an `i64`: a
/// positive double's bits read as an integer grow with its magnitude, a negative
/// one is the negated ordinal of its magnitude, and **both zeros are `0`** (PHP
/// compares `-0.0 == 0.0`). The next and previous float are the next and
/// previous ordinal. PHPStan's `FloatRangeType::toOrdinal`, the same map.
///
/// `None` for `NAN`, which has no place in the order.
#[must_use]
pub fn float_ordinal(v: f64) -> Option<i64> {
    if v.is_nan() {
        return None;
    }
    if v == 0.0 {
        return Some(0);
    }
    // The magnitude's bits fit in 63 bits (the sign bit is cleared).
    #[allow(clippy::cast_possible_wrap)]
    let mag = (v.to_bits() & !(1u64 << 63)) as i64;
    Some(if v.is_sign_negative() { -mag } else { mag })
}

/// The double at an ordinal: [`float_ordinal`]'s inverse on `[-ORD_INF,
/// ORD_INF]`, with `0` the positive zero.
fn from_ordinal(o: i64) -> f64 {
    let mag = f64::from_bits(o.unsigned_abs());
    if o < 0 { -mag } else { mag }
}

/// A float interval — the denotation of phpdoc `float<lo, hi>` and the named
/// `positive-float` family — held as an inclusive interval of
/// [`float_ordinal`]s, PHPStan's `FloatRangeType` form.
///
/// The ordinal form is what makes it canonical. The non-`NAN` doubles are a
/// finite ordered set, so every open end is a closed one at the neighbouring
/// double: `positive-float` (`> 0`) **is** `float<5.0E-324, max>`, one value,
/// not two spellings that fail to contain each other. Hull, intersection and
/// containment are then [`IntRange`]'s, on ordinals.
///
/// `min`/`max` are `-INF`/`INF`, included. **`NAN` is in no interval** —
/// `float<min, max>` included, which PHPStan documents as "float without NAN".
/// `-0.0` and `0.0` are one point, so no interval holds one zero without the
/// other. Invariant: `-ORD_INF <= lo <= hi <= ORD_INF`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FloatRange {
    lo: i64,
    hi: i64,
}

impl std::fmt::Debug for FloatRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FloatRange[{:?}, {:?}]", self.lo(), self.hi())
    }
}

impl FloatRange {
    /// `float<min, max>` — every float but `NAN`.
    pub const ALL: FloatRange = FloatRange { lo: -ORD_INF, hi: ORD_INF };
    /// `positive-float` — `> 0`, `INF` included.
    pub const POSITIVE: FloatRange = FloatRange { lo: 1, hi: ORD_INF };
    /// `negative-float` — `< 0`, `-INF` included.
    pub const NEGATIVE: FloatRange = FloatRange { lo: -ORD_INF, hi: -1 };
    /// `non-negative-float` — `>= 0`.
    pub const NON_NEGATIVE: FloatRange = FloatRange { lo: 0, hi: ORD_INF };
    /// `non-positive-float` — `<= 0`.
    pub const NON_POSITIVE: FloatRange = FloatRange { lo: -ORD_INF, hi: 0 };
    /// `finite-float` — `[-PHP_FLOAT_MAX, PHP_FLOAT_MAX]`: every float except
    /// `INF`, `-INF` and `NAN`. (`f64::MAX` is the double just below `INF`.)
    pub const FINITE: FloatRange = FloatRange { lo: -(ORD_INF - 1), hi: ORD_INF - 1 };

    /// The closed interval `[lo, hi]`; `None` for a `NAN` bound or `lo > hi`.
    #[must_use]
    pub fn new(lo: f64, hi: f64) -> Option<Self> {
        let (lo, hi) = (float_ordinal(lo)?, float_ordinal(hi)?);
        (lo <= hi).then_some(FloatRange { lo, hi })
    }

    /// The single-point interval at `v`; `None` for `NAN`.
    #[must_use]
    pub fn point(v: f64) -> Option<Self> {
        Self::new(v, v)
    }

    /// Lower bound (inclusive). A zero bound is `0.0`, never `-0.0`.
    #[must_use]
    pub fn lo(self) -> f64 {
        from_ordinal(self.lo)
    }

    /// Upper bound (inclusive). A zero bound is `0.0`, never `-0.0`.
    #[must_use]
    pub fn hi(self) -> f64 {
        from_ordinal(self.hi)
    }

    /// Membership of a float; `NAN` is in no interval.
    #[must_use]
    pub fn contains(self, v: f64) -> bool {
        float_ordinal(v).is_some_and(|o| self.lo <= o && o <= self.hi)
    }

    /// Membership of an int, by PHP's int-to-float widening (`5` satisfies
    /// `float<5, 6>`): the same value-equality reading as a float literal type.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn contains_int(self, v: i64) -> bool {
        self.contains(v as f64)
    }

    /// The floats an int interval widens to, as one interval. Exact as a hull:
    /// the int-to-float conversion is monotone, so `[lo, hi]` lands inside
    /// `[(float) lo, (float) hi]`, and both endpoints are members.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn widened(r: IntRange) -> Self {
        Self::new(r.lo() as f64, r.hi() as f64).expect("int bounds are ordered and not NAN")
    }

    /// Whether this is exactly the point `0.0` (which is also `-0.0`).
    #[must_use]
    pub const fn is_zero_point(self) -> bool {
        self.lo == 0 && self.hi == 0
    }

    /// Whether `self` holds every member of `other`.
    #[must_use]
    pub const fn contains_range(self, other: Self) -> bool {
        self.lo <= other.lo && other.hi <= self.hi
    }

    /// Convex hull — the join. May over-approximate a union with a gap.
    #[must_use]
    pub const fn hull(self, other: Self) -> Self {
        FloatRange {
            lo: if self.lo < other.lo { self.lo } else { other.lo },
            hi: if self.hi > other.hi { self.hi } else { other.hi },
        }
    }

    /// Intersection — the meet; `None` when disjoint.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Option<Self> {
        let lo = if self.lo > other.lo { self.lo } else { other.lo };
        let hi = if self.hi < other.hi { self.hi } else { other.hi };
        if lo <= hi { Some(FloatRange { lo, hi }) } else { None }
    }

    /// Whether the two share no float.
    #[must_use]
    pub const fn is_disjoint(self, other: Self) -> bool {
        self.intersect(other).is_none()
    }

    /// Whether the two overlap or sit side by side with no double between
    /// them — exactly when their union is one interval, their [`hull`](Self::hull).
    #[must_use]
    pub const fn touches(self, other: Self) -> bool {
        // `hi + 1` cannot overflow: `hi <= ORD_INF`.
        self.lo <= other.hi + 1 && other.lo <= self.hi + 1
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
        assert_eq!(all, FloatRange::ALL);
        let fin = FloatRange::FINITE;
        assert!(fin.contains(f64::MAX) && !fin.contains(f64::INFINITY) && !fin.contains(f64::NAN));
        assert!(FloatRange::NON_NEGATIVE.contains_range(FloatRange::POSITIVE));
        assert!(!FloatRange::POSITIVE.contains_range(FloatRange::NON_NEGATIVE));
        assert!(all.contains_range(fin) && !fin.contains_range(all) && all.contains_range(all));
    }

    /// The ordinal map is PHPStan's: order-preserving, both zeros at `0`, the
    /// next double the next ordinal, and the named bounds land where it puts
    /// them (`positive-float` is `float<5.0E-324, max>`).
    #[test]
    fn ordinals_follow_the_double_order() {
        let tiny = f64::from_bits(1); // 5.0E-324, the smallest positive double
        assert_eq!(float_ordinal(0.0), Some(0));
        assert_eq!(float_ordinal(-0.0), Some(0));
        assert_eq!(float_ordinal(tiny), Some(1));
        assert_eq!(float_ordinal(-tiny), Some(-1));
        assert_eq!(float_ordinal(f64::INFINITY), Some(ORD_INF));
        assert_eq!(float_ordinal(f64::NEG_INFINITY), Some(-ORD_INF));
        assert_eq!(float_ordinal(f64::MAX), Some(ORD_INF - 1));
        assert_eq!(float_ordinal(f64::NAN), None);
        assert_eq!(float_ordinal(1.0).map(|o| o + 1), float_ordinal(1.0f64.next_up()));
        assert_eq!(FloatRange::new(tiny, f64::INFINITY), Some(FloatRange::POSITIVE));
        assert_eq!(FloatRange::new(f64::NEG_INFINITY, -tiny), Some(FloatRange::NEGATIVE));
        assert_eq!(FloatRange::new(-f64::MAX, f64::MAX), Some(FloatRange::FINITE));
        assert_eq!(FloatRange::new(-0.0, -0.0), FloatRange::new(0.0, 0.0));
        assert!(FloatRange::point(-0.0).unwrap().is_zero_point());
        assert_eq!(FloatRange::point(-0.0).unwrap().lo().to_bits(), 0.0f64.to_bits());
        for v in [0.0, tiny, 1.5, -2.25, f64::MAX, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(FloatRange::point(v).unwrap().lo(), v);
        }
    }

    #[test]
    fn float_interval_algebra() {
        let a = FloatRange::new(0.0, 10.0).unwrap();
        let b = FloatRange::new(5.0, 20.0).unwrap();
        assert_eq!(a.hull(b), FloatRange::new(0.0, 20.0).unwrap());
        assert_eq!(a.intersect(b), FloatRange::new(5.0, 10.0));
        assert!(a.is_disjoint(FloatRange::new(10.5, 11.0).unwrap()));
        // Positive and non-positive floats meet with no double between them.
        assert!(FloatRange::POSITIVE.touches(FloatRange::NON_POSITIVE));
        assert!(FloatRange::POSITIVE.is_disjoint(FloatRange::NON_POSITIVE));
        assert_eq!(FloatRange::POSITIVE.hull(FloatRange::NON_POSITIVE), FloatRange::ALL);
        assert!(!FloatRange::POSITIVE.touches(FloatRange::NEGATIVE));
        let ints = FloatRange::widened(IntRange::new(1, 3).unwrap());
        assert_eq!(ints, FloatRange::new(1.0, 3.0).unwrap());
        let all_ints = FloatRange::widened(IntRange::FULL);
        assert!(all_ints.contains_int(i64::MIN) && all_ints.contains_int(i64::MAX));
    }

    #[test]
    fn hull_is_sound_for_members() {
        let a = IntRange::point(-3);
        let b = IntRange::point(7);
        let h = a.hull(b);
        assert!(h.contains(-3) && h.contains(7) && h.contains(0));
    }
}
