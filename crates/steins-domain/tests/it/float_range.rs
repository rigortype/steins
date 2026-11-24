//! Property tests for [`FloatRange`]: the ordinal form must answer every question
//! exactly as IEEE comparison does, across `NAN`, both infinities, both zeros,
//! subnormals and `f64::MAX`.

use proptest::prelude::*;
use steins_domain::{FloatRange, float_ordinal};

/// Any double, with the edge classes drawn far more often than chance would.
fn arb_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<f64>(),
        (-100i32..100).prop_map(f64::from),
        prop_oneof![
            Just(0.0f64),
            Just(-0.0),
            Just(f64::NAN),
            Just(f64::INFINITY),
            Just(f64::NEG_INFINITY),
            Just(f64::MAX),
            Just(f64::MIN),
            Just(f64::MIN_POSITIVE),
            Just(f64::from_bits(1)),
            Just(-f64::from_bits(1)),
            Just(1.5),
            Just(-1.5),
        ],
    ]
}

proptest! {
    /// Construction refuses exactly a `NAN` bound or a reversed pair, and
    /// membership is the closed IEEE comparison (so `NAN` is in nothing).
    #[test]
    fn membership_is_ieee_comparison(a in arb_f64(), b in arb_f64(), v in arb_f64()) {
        let r = FloatRange::new(a, b);
        prop_assert_eq!(r.is_some(), a <= b, "new({:?}, {:?})", a, b);
        if let Some(r) = r {
            prop_assert_eq!(r.contains(v), a <= v && v <= b, "{:?} ∋ {:?}", r, v);
        }
    }

    /// The ordinal map is order-preserving and NAN-free, and the bounds a range
    /// reports are the doubles it was built from (a zero reads as `0.0`).
    #[test]
    fn ordinals_preserve_order(a in arb_f64(), b in arb_f64()) {
        match (float_ordinal(a), float_ordinal(b)) {
            (Some(oa), Some(ob)) => {
                prop_assert_eq!(oa.cmp(&ob), a.partial_cmp(&b).expect("not NAN"));
                let p = FloatRange::point(a).expect("not NAN");
                prop_assert!(p.lo() == a && p.hi() == a);
            }
            _ => prop_assert!(a.is_nan() || b.is_nan()),
        }
    }

    /// Hull admits both; intersection admits exactly what both admit; and
    /// `touches` is exactly when the hull adds no double the union lacks.
    #[test]
    fn interval_algebra_matches_membership(
        (a, b) in (arb_f64(), arb_f64()),
        (c, d) in (arb_f64(), arb_f64()),
        v in arb_f64(),
    ) {
        let (Some(x), Some(y)) = (FloatRange::new(a, b), FloatRange::new(c, d)) else {
            return Ok(());
        };
        let either = x.contains(v) || y.contains(v);
        let both = x.contains(v) && y.contains(v);
        prop_assert!(!either || x.hull(y).contains(v));
        prop_assert_eq!(x.intersect(y).is_some_and(|m| m.contains(v)), both);
        prop_assert_eq!(x.is_disjoint(y), x.intersect(y).is_none());
        prop_assert_eq!(x.contains_range(y), x.intersect(y) == Some(y));
        if x.touches(y) {
            prop_assert_eq!(x.hull(y).contains(v), either, "{:?} ∪ {:?} at {:?}", x, y, v);
        }
    }
}
