//! End-to-end: real phpdoc type strings through parse → lower → admit,
//! including the #14939 shape semantics and the abstract-fact judgments.

use proptest::prelude::*;
use steins_contract::{admits_fact, admits_val, lower_str};
use steins_domain::{Base, Certainty, Fact, IntRange, Key, Refinement, StrPreds, Val};
use Certainty::{Maybe, No, Yes};

fn ty(s: &str) -> steins_contract::ContractTy {
    lower_str(s).unwrap_or_else(|| panic!("must lower: {s}"))
}

fn s(v: &str) -> Val {
    Val::Str(v.to_owned())
}

fn arr(items: Vec<(Key, Val)>) -> Val {
    Val::Array(items)
}

fn list(vals: Vec<Val>) -> Val {
    arr(vals.into_iter().enumerate().map(|(i, v)| (Key::Int(i as i64), v)).collect())
}

#[test]
fn scalar_contracts_have_no_coercion() {
    // ADR-0030: "5" fails an int *contract* even though it coerces at runtime.
    assert_eq!(admits_val(&ty("int"), &s("5")), No);
    assert_eq!(admits_val(&ty("int"), &Val::Int(5)), Yes);
    // int is accepted where float is expected (PHPStan core).
    assert_eq!(admits_val(&ty("float"), &Val::Int(5)), Yes);
    assert_eq!(admits_val(&ty("string"), &Val::Float(1.5)), No);
    assert_eq!(admits_val(&ty("?int"), &Val::Null), Yes);
    assert_eq!(admits_val(&ty("int|string"), &s("x")), Yes);
    assert_eq!(admits_val(&ty("int|string"), &Val::Float(1.5)), No);
}

#[test]
fn refinement_keywords() {
    assert_eq!(admits_val(&ty("numeric-string"), &s("5.5e3")), Yes);
    assert_eq!(admits_val(&ty("numeric-string"), &s("abc")), No);
    assert_eq!(admits_val(&ty("non-empty-string"), &s("")), No);
    assert_eq!(admits_val(&ty("non-falsy-string"), &s("0")), No);
    assert_eq!(admits_val(&ty("positive-int"), &Val::Int(0)), No);
    assert_eq!(admits_val(&ty("int<0, 10>"), &Val::Int(10)), Yes);
    assert_eq!(admits_val(&ty("int<0, 10>"), &Val::Int(11)), No);
    assert_eq!(admits_val(&ty("int<min, 0>"), &Val::Int(i64::MIN)), Yes);
    // Literal types.
    assert_eq!(admits_val(&ty("'a'|'b'"), &s("a")), Yes);
    assert_eq!(admits_val(&ty("'a'|'b'"), &s("c")), No);
    assert_eq!(admits_val(&ty("5.0"), &Val::Int(5)), Yes); // PHP value equality
}

#[test]
fn provenance_strings_never_decide_yes() {
    // ADR-0038: class-string / literal-string are non-extensional.
    assert_eq!(admits_val(&ty("class-string"), &s("App\\User")), Maybe);
    assert_eq!(admits_val(&ty("literal-string"), &s("abc")), Maybe);
    assert_eq!(admits_val(&ty("class-string"), &Val::Int(1)), No);
}

#[test]
fn lists_and_maps() {
    let l = ty("list<int>");
    assert_eq!(admits_val(&l, &list(vec![Val::Int(1), Val::Int(2)])), Yes);
    assert_eq!(admits_val(&l, &list(vec![Val::Int(1), s("x")])), No);
    // Keys 0..n-1 required (#14939): a keyed map is not a list.
    assert_eq!(admits_val(&l, &arr(vec![(Key::Int(1), Val::Int(1))])), No);
    assert_eq!(admits_val(&ty("non-empty-list<int>"), &list(vec![])), No);

    let m = ty("array<string, int>");
    assert_eq!(admits_val(&m, &arr(vec![(Key::Str("a".into()), Val::Int(1))])), Yes);
    assert_eq!(admits_val(&m, &arr(vec![(Key::Int(0), Val::Int(1))])), No);
    assert_eq!(admits_val(&ty("int[]"), &list(vec![Val::Int(1)])), Yes);
}

#[test]
fn shapes_follow_14939() {
    let shape = ty("array{id: int, name?: string}");
    let ok = arr(vec![(Key::Str("id".into()), Val::Int(1))]);
    let with_name =
        arr(vec![(Key::Str("name".into()), s("n")), (Key::Str("id".into()), Val::Int(1))]);
    let missing = arr(vec![(Key::Str("name".into()), s("n"))]);
    let extra = arr(vec![(Key::Str("id".into()), Val::Int(1)), (Key::Str("x".into()), s("y"))]);

    assert_eq!(admits_val(&shape, &ok), Yes);
    // array{} is an order-agnostic key SET — declaration order is irrelevant.
    assert_eq!(admits_val(&shape, &with_name), Yes);
    assert_eq!(admits_val(&shape, &missing), No); // required key absent
    assert_eq!(admits_val(&shape, &extra), No); // sealed

    let unsealed = ty("array{id: int, ...<string, mixed>}");
    assert_eq!(admits_val(&unsealed, &extra), Yes);
    let bad_tail_key = arr(vec![(Key::Str("id".into()), Val::Int(1)), (Key::Int(9), s("y"))]);
    assert_eq!(admits_val(&unsealed, &bad_tail_key), No);

    // list{} is positional.
    let pair = ty("list{int, string}");
    assert_eq!(admits_val(&pair, &list(vec![Val::Int(1), s("a")])), Yes);
    assert_eq!(admits_val(&pair, &list(vec![s("a"), Val::Int(1)])), No);
    // Reversed-key literal is NOT a list (#14939 — the registered divergence).
    let reversed = arr(vec![(Key::Int(1), s("x")), (Key::Int(0), s("y"))]);
    assert_eq!(admits_val(&pair, &reversed), No);
}

#[test]
fn abstract_facts_judged_soundly() {
    let numeric =
        Fact::refined(Base::String, Refinement::Str(StrPreds::NUMERIC.close()), false);
    assert_eq!(admits_fact(&ty("numeric-string"), &numeric), Yes);
    assert_eq!(admits_fact(&ty("non-empty-string"), &numeric), Yes); // implied
    assert_eq!(admits_fact(&ty("non-falsy-string"), &numeric), Maybe); // "0"
    assert_eq!(admits_fact(&ty("string"), &numeric), Yes);
    assert_eq!(admits_fact(&ty("int"), &numeric), No); // contract: no coercion

    let pos = Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false);
    assert_eq!(admits_fact(&ty("int<0, max>"), &pos), Yes);
    assert_eq!(admits_fact(&ty("int<min, 0>"), &pos), No); // disjoint
    assert_eq!(admits_fact(&ty("float"), &pos), Yes); // int ⊆ float contract
    assert_eq!(admits_fact(&ty("int|string"), &pos), Yes);

    let nullable_pos = Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), true);
    assert_eq!(admits_fact(&ty("int"), &nullable_pos), Maybe); // null escapes
    assert_eq!(admits_fact(&ty("?int"), &nullable_pos), Yes);

    // Jointly-covering unions under-approximate to Maybe (documented).
    let general = Fact::General { base: Base::Int, nullable: false };
    assert_eq!(admits_fact(&ty("int<min, 0>|int<0, max>"), &general), Maybe);
    assert_eq!(admits_fact(&ty("mixed"), &general), Yes);
}

#[test]
fn opaque_constructs_stay_maybe() {
    for t in ["Foo::BAR", "($x is int ? string : bool)", "self"] {
        let lowered = ty(t);
        assert_eq!(admits_val(&lowered, &Val::Int(1)), Maybe, "{t}");
    }
    // Class names: scalars are never instances. NOTE: a bare template name
    // (`T`) is indistinguishable from a class name in context-free lowering —
    // callers must substitute declared templates to Opaque first (ADR-0032).
    assert_eq!(admits_val(&ty("\\App\\User"), &Val::Int(1)), No);
    assert_eq!(admits_val(&ty("T"), &Val::Int(1)), No);
}

fn arb_scalar() -> impl Strategy<Value = Val> {
    prop_oneof![
        any::<i64>().prop_map(Val::Int),
        prop_oneof![Just(0.0f64), Just(1.5), Just(-3.25)].prop_map(Val::Float),
        prop_oneof![
            Just(String::new()),
            Just("0".to_owned()),
            Just("5".to_owned()),
            Just("abc".to_owned()),
            "[a-z0-9]{0,3}",
        ]
        .prop_map(Val::Str),
        any::<bool>().prop_map(Val::Bool),
        Just(Val::Null),
    ]
}

proptest! {
    /// Fact-level judgment must agree with value-level judgment on every
    /// witness: Yes ⇒ all witnesses admitted, No ⇒ none (soundness of the
    /// abstract path across summarization).
    #[test]
    fn fact_judgment_consistent_with_witnesses(
        vals in prop::collection::vec(arb_scalar(), 1..14),
        tystr in prop_oneof![
            Just("int"), Just("string"), Just("float"), Just("bool"),
            Just("?int"), Just("int|string"), Just("numeric-string"),
            Just("non-empty-string"), Just("positive-int"), Just("mixed"),
            Just("int<0, 100>"), Just("'5'|'abc'"), Just("scalar"),
        ],
    ) {
        let Some(fact) = Fact::from_vals(vals.clone()) else { return Ok(()) };
        let contract = ty(tystr);
        match admits_fact(&contract, &fact) {
            Certainty::Yes => {
                for v in &vals {
                    prop_assert_eq!(admits_val(&contract, v), Yes, "{:?} under {}", v, tystr);
                }
            }
            Certainty::No => {
                for v in &vals {
                    prop_assert_eq!(admits_val(&contract, v), No, "{:?} under {}", v, tystr);
                }
            }
            Certainty::Maybe => {}
        }
    }
}
