//! Property tests for the domain's soundness contract (ADR-0035):
//! joins and widenings may lose precision, never members.

use proptest::prelude::*;
use steins_domain::{Base, Certainty, Fact, IntRange, Refinement, StrPreds, Val};

fn arb_scalar() -> impl Strategy<Value = Val> {
    prop_oneof![
        any::<i64>().prop_map(Val::Int),
        (-1000i64..1000).prop_map(Val::Int),
        prop_oneof![Just(0.0f64), Just(-0.0), Just(1.5), Just(-3.25), Just(1e10)].prop_map(Val::Float),
        prop_oneof![
            Just(String::new()),
            Just("0".to_owned()),
            Just("5".to_owned()),
            Just("abc".to_owned()),
            Just(" 5 ".to_owned()),
            Just("1.5e3".to_owned()),
            Just("00".to_owned()),
            "[a-z0-9]{0,4}",
        ]
        .prop_map(Val::Str),
        any::<bool>().prop_map(Val::Bool),
        Just(Val::Null),
    ]
}

/// A fact together with witness values it must admit.
fn arb_fact_with_witnesses() -> impl Strategy<Value = (Fact, Vec<Val>)> {
    prop::collection::vec(arb_scalar(), 1..14).prop_filter_map(
        "representable value sets only",
        |vals| Fact::from_vals(vals.clone()).map(|f| (f, vals)),
    )
}

proptest! {
    #[test]
    fn from_vals_admits_every_input((fact, witnesses) in arb_fact_with_witnesses()) {
        for w in &witnesses {
            prop_assert!(fact.admits(w), "{fact:?} must admit {w:?}");
        }
    }

    #[test]
    fn join_is_commutative(
        (a, _) in arb_fact_with_witnesses(),
        (b, _) in arb_fact_with_witnesses(),
    ) {
        prop_assert_eq!(a.join(&b), b.join(&a));
    }

    #[test]
    fn join_never_loses_members(
        (a, wa) in arb_fact_with_witnesses(),
        (b, wb) in arb_fact_with_witnesses(),
    ) {
        if let Some(j) = a.join(&b) {
            for w in wa.iter().chain(&wb) {
                prop_assert!(j.admits(w), "join {j:?} must admit {w:?}");
            }
        }
        // None is always a legal (maximally widened) outcome.
    }

    #[test]
    fn join_with_self_is_identity_on_finite((a, _) in arb_fact_with_witnesses()) {
        if a.finite_members().is_some() {
            prop_assert_eq!(a.join(&a), Some(a));
        } else if let Some(j) = a.join(&a) {
            // Abstract self-join may only stay equal (no precision loss).
            prop_assert_eq!(j, a);
        }
    }

    #[test]
    fn queries_agree_with_witnesses((fact, witnesses) in arb_fact_with_witnesses()) {
        match fact.truthy() {
            Certainty::Yes => {
                for w in &witnesses {
                    prop_assert!(!steins_domain::php_is_falsy(w));
                }
            }
            Certainty::No => {
                for w in &witnesses {
                    prop_assert!(steins_domain::php_is_falsy(w));
                }
            }
            Certainty::Maybe => {}
        }
        let pred = StrPreds::NON_FALSY.close();
        match fact.satisfies_str(pred) {
            Certainty::Yes => {
                for w in &witnesses {
                    match w {
                        Val::Str(s) => prop_assert!(StrPreds::of(s).contains_all(pred)),
                        other => prop_assert!(false, "Yes but non-string witness {other:?}"),
                    }
                }
            }
            Certainty::No => {
                for w in &witnesses {
                    if let Val::Str(s) = w {
                        prop_assert!(!StrPreds::of(s).contains_all(pred));
                    }
                }
            }
            Certainty::Maybe => {}
        }
    }

    #[test]
    fn range_hull_and_intersection_laws(
        a in any::<i64>(), b in any::<i64>(), c in any::<i64>(), d in any::<i64>(),
    ) {
        let r1 = IntRange::new(a.min(b), a.max(b)).expect("ordered");
        let r2 = IntRange::new(c.min(d), c.max(d)).expect("ordered");
        let hull = r1.hull(r2);
        prop_assert!(hull.contains_range(r1) && hull.contains_range(r2));
        if let Some(meet) = r1.intersect(r2) {
            prop_assert!(r1.contains_range(meet) && r2.contains_range(meet));
        } else {
            // Disjoint: no point may be in both.
            prop_assert!(r1.hi() < r2.lo() || r2.hi() < r1.lo());
        }
    }
}

#[test]
fn abstract_join_examples_stay_exact() {
    // Refined ∪ Refined with the same knowledge must not widen.
    let f = Fact::refined(Base::String, Refinement::Str(StrPreds::NUMERIC.close()), false);
    assert_eq!(f.join(&f), Some(f));
    let r = Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), true);
    assert_eq!(r.join(&r), Some(r));
}
