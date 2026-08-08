//! End-to-end: real phpdoc type strings through parse → lower → admit,
//! including the #14939 shape semantics and the abstract-fact judgments.

use proptest::prelude::*;
use steins_contract::normalize::subsumes;
use steins_contract::{admits_fact, admits_val, lower_str};
use steins_domain::{Base, Certainty, Fact, IntRange, Key, Refinement, StrPreds, Val};
use Certainty::{Maybe, No, Yes};

fn ty(s: &str) -> steins_contract::ContractTy {
    lower_str(s).unwrap_or_else(|| panic!("must lower: {s}"))
}

fn s(v: &str) -> Val {
    Val::Str(v.into())
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
    assert_eq!(admits_val(&ty("'a'|'b'"), &s("a")), Yes);
    assert_eq!(admits_val(&ty("'a'|'b'"), &s("c")), No);
    assert_eq!(admits_val(&ty("5.0"), &Val::Int(5)), Yes); // PHP value equality
}

#[test]
fn provenance_strings_never_decide_yes() {
    // ADR-0038: `literal-string` is provenance, so no value decides it.
    assert_eq!(admits_val(&ty("literal-string"), &s("abc")), Maybe);
    assert_eq!(admits_val(&ty("literal-string"), &Val::Int(1)), No);
    // `class-string` is a *contextual* predicate (issue #236), not provenance:
    // no `Yes` without the class table, but the identifier grammar refutes.
    assert_eq!(admits_val(&ty("class-string"), &s("App\\User")), Maybe);
    assert_eq!(admits_val(&ty("class-string"), &Val::Int(1)), No);
    assert_eq!(admits_val(&ty("class-string"), &s("")), No);
    assert_eq!(admits_val(&ty("class-string"), &s("0")), No);
    assert_eq!(admits_val(&ty("class-string"), &s("123")), No);
    // …and it takes its place on the refined-string ladder in both directions.
    assert_eq!(subsumes(&ty("non-falsy-string"), &ty("class-string")), Yes);
    assert_eq!(subsumes(&ty("string"), &ty("class-string")), Yes);
    assert_eq!(subsumes(&ty("class-string"), &ty("class-string")), Yes);
    assert_ne!(subsumes(&ty("class-string"), &ty("non-empty-string")), Yes);
    // The spelling round-trips: the speller's word lowers back to the same set.
    assert_eq!(steins_contract::spell::spell_arms(&[ty("class-string")]).as_deref(), Some("class-string"));
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

/// Phan's `associative-array<K, V>` / `non-empty-associative-array<K, V>`
/// (census bucket ix): `array<K, V>` plus a refusal of list realizations — the
/// ADR-0062 `is_list` trinary seeded `No` instead of `Maybe`.
#[test]
fn associative_array_rejects_list_realizations() {
    let assoc = ty("associative-array<int, string>");
    // A non-sequential int-keyed array is associative everywhere.
    assert_eq!(
        admits_val(&assoc, &arr(vec![(Key::Int(5), s("a")), (Key::Int(9), s("b"))])),
        Yes
    );
    // Same element types, but the keys ARE a list — Phan rejects it.
    assert_eq!(admits_val(&assoc, &list(vec![s("a"), s("b"), s("c")])), No);
    // Non-array values never satisfied it in the first place.
    assert_eq!(admits_val(&assoc, &Val::Int(1)), No);

    let non_empty_assoc = ty("non-empty-associative-array<string, int>");
    assert_eq!(
        admits_val(&non_empty_assoc, &arr(vec![(Key::Str("a".into()), Val::Int(1))])),
        Yes
    );
    assert_eq!(admits_val(&non_empty_assoc, &arr(vec![])), No);
    // A list violates the associative part.
    assert_eq!(
        admits_val(&non_empty_assoc, &list(vec![Val::Int(1), Val::Int(2), Val::Int(3)])),
        No
    );

    // The bare (unparameterized) spellings lower too, same not-list refusal.
    assert_eq!(admits_val(&ty("associative-array"), &list(vec![Val::Int(1)])), No);
    assert_eq!(
        admits_val(&ty("associative-array"), &arr(vec![(Key::Int(1), Val::Int(1))])),
        Yes
    );
}

/// `key-of<T>` / `value-of<T>` (census bucket vi, inline tier): the projection
/// over an operand this crate can enumerate on its own.
#[test]
fn key_of_and_value_of_project_enumerable_operands() {
    let k = ty("key-of<array{name: string, age: int}>");
    assert_eq!(admits_val(&k, &s("name")), Yes);
    assert_eq!(admits_val(&k, &s("age")), Yes);
    assert_eq!(admits_val(&k, &s("missing")), No);
    // A *value* of the shape is not a key of it — the mistake the spelling invites.
    assert_eq!(admits_val(&k, &Val::Int(0)), No);

    let v = ty("value-of<array{a: int, b: int}>");
    assert_eq!(admits_val(&v, &Val::Int(1)), Yes);
    assert_eq!(admits_val(&v, &s("x")), No);
    // `'a'` is a key of the shape, not a value of it.
    assert_eq!(admits_val(&v, &s("a")), No);

    // Duplicate member types collapse: `int|int` is `int`, so any int qualifies.
    assert_eq!(admits_val(&v, &Val::Int(9999)), Yes);

    // A heterogeneous shape keeps both value members.
    let hv = ty("value-of<array{a: int, b: string}>");
    assert_eq!(admits_val(&hv, &Val::Int(1)), Yes);
    assert_eq!(admits_val(&hv, &s("x")), Yes);
    assert_eq!(admits_val(&hv, &Val::Bool(true)), No);

    // A `list{…}` shape's keys are its positions.
    let lk = ty("key-of<list{int, string}>");
    assert_eq!(admits_val(&lk, &Val::Int(0)), Yes);
    assert_eq!(admits_val(&lk, &Val::Int(1)), Yes);
    assert_eq!(admits_val(&lk, &Val::Int(2)), No);
    assert_eq!(admits_val(&lk, &s("0")), No);

    // `list<T>` keys are `int<0, max>`; its values are `T`.
    assert_eq!(admits_val(&ty("key-of<list<string>>"), &Val::Int(7)), Yes);
    assert_eq!(admits_val(&ty("key-of<list<string>>"), &Val::Int(-1)), No);
    assert_eq!(admits_val(&ty("value-of<list<string>>"), &s("x")), Yes);
    assert_eq!(admits_val(&ty("value-of<list<string>>"), &Val::Int(1)), No);

    // `array<K, V>` projects to its own key/value contracts.
    assert_eq!(admits_val(&ty("key-of<array<string, int>>"), &s("k")), Yes);
    assert_eq!(admits_val(&ty("key-of<array<string, int>>"), &Val::Int(1)), No);
    assert_eq!(admits_val(&ty("value-of<array<string, int>>"), &Val::Int(1)), Yes);
}

/// The optional-key rule, and the operands the projection refuses.
#[test]
fn key_of_keeps_optional_keys_and_floors_open_operands() {
    // PHPStan's `Type::getKeysArray()` includes an optional field's key: a `b?:`
    // field is still a key the array MAY carry. No conformance fixture probes
    // this — the rule is taken from PHPStan's semantics, not derived from a test.
    let k = ty("key-of<array{a: int, b?: string}>");
    assert_eq!(admits_val(&k, &s("a")), Yes);
    assert_eq!(admits_val(&k, &s("b")), Yes);
    assert_eq!(admits_val(&k, &s("c")), No);
    let v = ty("value-of<array{a: int, b?: string}>");
    assert_eq!(admits_val(&v, &s("x")), Yes);
    assert_eq!(admits_val(&v, &Val::Int(1)), Yes);
    assert_eq!(admits_val(&v, &Val::Bool(false)), No);

    // An UNSEALED shape's key set is open — the declaration names a prefix, not
    // the whole set — so the projection declines rather than enumerate a lie.
    assert_eq!(admits_val(&ty("key-of<array{a: int, ...}>"), &s("zzz")), Maybe);
    assert_eq!(admits_val(&ty("value-of<array{a: int, ...}>"), &s("zzz")), Maybe);

    // Operands with no enumerable key/value set stay at the Opaque floor: a
    // template parameter, a class-constant fetch (the const tier resolves that in
    // `steins-infer`, which has the project index this crate does not), a class.
    assert_eq!(admits_val(&ty("key-of<T>"), &s("anything")), Maybe);
    assert_eq!(admits_val(&ty("value-of<T>"), &s("anything")), Maybe);
    assert_eq!(admits_val(&ty("key-of<Config::MAP>"), &s("anything")), Maybe);
    assert_eq!(admits_val(&ty("value-of<Suit>"), &s("anything")), Maybe);
    assert_eq!(admits_val(&ty("value-of<int>"), &s("anything")), Maybe);

    // `array` states nothing about its values, but its keys are still `array-key`.
    assert_eq!(admits_val(&ty("key-of<array>"), &s("k")), Yes);
    assert_eq!(admits_val(&ty("key-of<array>"), &Val::Bool(true)), No);
    assert_eq!(admits_val(&ty("value-of<array>"), &Val::Bool(true)), Maybe);

    // The empty shape has no keys and no values at all — `never`, not silence.
    assert_eq!(admits_val(&ty("key-of<array{}>"), &s("a")), No);
    assert_eq!(admits_val(&ty("value-of<array{}>"), &Val::Int(1)), No);

    // The bare, unparameterized keywords keep their Opaque floor.
    assert_eq!(admits_val(&ty("key-of"), &s("a")), Maybe);
    assert_eq!(admits_val(&ty("value-of"), &s("a")), Maybe);
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

/// ADR-0062 §5 — the acceptance-convergence fixture, fact-path side. The
/// proven-value path in `steins-infer` judges through this very relation
/// (`shape_verdict`), so its twin fixture — `unsealed_tail_key_contract_is_checked`
/// in `steins-infer/tests/phpdoc_contract.rs` — must agree verdict for verdict.
#[test]
fn unsealed_tail_key_contract_is_checked() {
    let tail = ty("array{a: int, ...<string, int>}");
    let int_tail_key = arr(vec![(Key::Str("a".into()), Val::Int(1)), (Key::Int(9), Val::Int(2))]);
    let str_tail_key =
        arr(vec![(Key::Str("a".into()), Val::Int(1)), (Key::Str("b".into()), Val::Int(2))]);
    let bad_tail_val =
        arr(vec![(Key::Str("a".into()), Val::Int(1)), (Key::Str("b".into()), s("x"))]);

    assert_eq!(admits_val(&tail, &int_tail_key), No, "int key 9 violates <string, …>");
    assert_eq!(admits_val(&tail, &str_tail_key), Yes);
    assert_eq!(admits_val(&tail, &bad_tail_val), No, "the tail VALUE contract too");
    assert_eq!(admits_val(&ty("array{a: int, ...<int, int>}"), &int_tail_key), Yes);
    assert_eq!(admits_val(&ty("array{a: int, ...}"), &int_tail_key), Yes, "untyped tail");
}

/// PHP array-key normalization is part of the one shape relation: a shape key
/// spelled as an integer-like string declares the *int* key it denotes, so it
/// matches the value `[9 => …]` instead of counting as an undeclared extra.
#[test]
fn integer_like_string_shape_keys_normalize() {
    let shape = ty("array{'9': int}");
    assert_eq!(admits_val(&shape, &arr(vec![(Key::Int(9), Val::Int(1))])), Yes);
    assert_eq!(admits_val(&shape, &arr(vec![(Key::Int(9), s("x"))])), No);
    // A non-canonical spelling stays a string key (PHP does not fold "09").
    assert_eq!(
        admits_val(&ty("array{'09': int}"), &arr(vec![(Key::Str("09".into()), Val::Int(1))])),
        Yes
    );
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
        .prop_map(|s| Val::Str(s.into())),
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

// ==========================================================================
// Conformance vocabulary in the identifier/generic table.
// ==========================================================================

#[test]
fn number_is_int_or_float_but_not_a_numeric_string() {
    // `number` is `numeric` minus its string member — the whole distinction.
    assert_eq!(admits_val(&ty("number"), &Val::Int(1)), Yes);
    assert_eq!(admits_val(&ty("number"), &Val::Float(1.5)), Yes);
    assert_eq!(admits_val(&ty("number"), &s("1")), No);
    assert_eq!(admits_val(&ty("number"), &Val::Bool(true)), No);
    assert_eq!(admits_val(&ty("number"), &Val::Null), No);
    // The contrast, on the same values.
    assert_eq!(admits_val(&ty("numeric"), &s("1")), Yes);
    assert_eq!(admits_val(&ty("numeric"), &s("abc")), No);
    // Abstract facts agree: a general int is wholly inside `number`.
    assert_eq!(
        admits_fact(&ty("number"), &Fact::General { base: Base::Int, nullable: false }),
        Yes
    );
    assert_eq!(
        admits_fact(&ty("number"), &Fact::General { base: Base::String, nullable: false }),
        No
    );
}

#[test]
fn non_zero_int_keeps_the_hole_at_zero() {
    // The union must not flatten: `int<min, -1>|int<1, max>`, not `int<min, max>`.
    assert_eq!(admits_val(&ty("non-zero-int"), &Val::Int(1)), Yes);
    assert_eq!(admits_val(&ty("non-zero-int"), &Val::Int(-1)), Yes);
    assert_eq!(admits_val(&ty("non-zero-int"), &Val::Int(0)), No);
    assert_eq!(admits_val(&ty("non-zero-int"), &s("1")), No);
    // A general `int` only *jointly* inhabits the two arms, so the documented
    // under-approximation answers `Maybe` — never a wrong `Yes`.
    assert_eq!(
        admits_fact(&ty("non-zero-int"), &Fact::General { base: Base::Int, nullable: false }),
        Maybe
    );
    // A refined int wholly on one side of the hole is decided.
    assert_eq!(
        admits_fact(
            &ty("non-zero-int"),
            &Fact::Refined {
                base: Base::Int,
                refinement: Refinement::Int(IntRange::new(1, 10).unwrap()),
                nullable: false,
            }
        ),
        Yes
    );
}

#[test]
fn int_range_keyword_is_the_int_range() {
    // Phan's `int-range<lo, hi>` and PHPStan's `int<lo, hi>` are one lowering.
    assert_eq!(ty("int-range<0, 255>"), ty("int<0, 255>"));
    assert_eq!(admits_val(&ty("int-range<0, 255>"), &Val::Int(200)), Yes);
    assert_eq!(admits_val(&ty("int-range<0, 255>"), &Val::Int(256)), No);
    assert_eq!(admits_val(&ty("int-range<0, 255>"), &Val::Int(-1)), No);
    assert_eq!(admits_val(&ty("int-range<0, 255>"), &s("200")), No);
    // The bound grammar comes along with it (`min`/`max`).
    assert_eq!(ty("int-range<min, 0>"), ty("non-positive-int"));
}

#[test]
fn non_positive_int_covers_zero() {
    assert_eq!(admits_val(&ty("non-positive-int"), &Val::Int(0)), Yes);
    assert_eq!(admits_val(&ty("non-positive-int"), &Val::Int(-1)), Yes);
    assert_eq!(admits_val(&ty("non-positive-int"), &Val::Int(1)), No);
    // Exactly one value apart from `negative-int` — which is the point.
    assert_eq!(admits_val(&ty("negative-int"), &Val::Int(0)), No);
}

// ---------------------------------------------------------------------------
// C5 — the array-key-cast pair.
// ---------------------------------------------------------------------------

#[test]
fn decimal_int_string_is_the_array_key_cast_not_is_numeric() {
    let d = ty("decimal-int-string");
    for yes in ["0", "1", "1234", "-1", "9223372036854775807", "-9223372036854775808"] {
        assert_eq!(admits_val(&d, &s(yes)), Yes, "{yes:?}");
    }
    // Numeric, but not canonical — every one keeps its string identity.
    for no in ["007", "+1", "00", "-0", "1.2", "18E+3", "9223372036854775808", "", "abc"] {
        assert_eq!(admits_val(&d, &s(no)), No, "{no:?}");
    }
    // A `decimal-int-string` is a `numeric-string` and a `non-empty-string`
    // (the closure), but NOT a `non-falsy-string` — `'0'` is one and is falsy.
    assert_eq!(admits_val(&ty("numeric-string"), &s("0")), Yes);
    assert_eq!(admits_val(&ty("non-falsy-string"), &s("0")), No);
    assert_eq!(admits_val(&d, &s("0")), Yes);
    // Not a string at all: out, without consulting the predicate.
    assert_eq!(admits_val(&d, &Val::Int(123)), No);
}

#[test]
fn non_decimal_int_string_is_the_complement_within_string() {
    let n = ty("non-decimal-int-string");
    for yes in ["+1", "00", "18E+3", "1.2", "1,3", "foo", "", "-0", "007"] {
        assert_eq!(admits_val(&n, &s(yes)), Yes, "{yes:?}");
    }
    for no in ["123", "-1", "0"] {
        assert_eq!(admits_val(&n, &s(no)), No, "{no:?}");
    }
    assert_eq!(admits_val(&n, &Val::Int(1)), No, "not a string");
}

/// The negation ceiling. `StrPreds` is a conjunction over positive literals, so
/// the abstract leg can conclude "every string with these predicates also has
/// those" but never "no string has both". The pair is therefore decided exactly
/// against a **value** and only one-directionally against a **fact**.
#[test]
fn the_complementary_pair_cannot_be_refuted_abstractly() {
    let decimal_fact = Fact::Refined {
        base: Base::String,
        refinement: Refinement::Str(StrPreds::DECIMAL_INT.close()),
        nullable: false,
    };
    // The entailed direction is decided: a decimal-int-string IS numeric,
    // non-empty, and (having no cased character) both lowercase and uppercase.
    assert_eq!(admits_fact(&ty("numeric-string"), &decimal_fact), Yes);
    assert_eq!(admits_fact(&ty("non-empty-string"), &decimal_fact), Yes);
    assert_eq!(admits_fact(&ty("lowercase-string"), &decimal_fact), Yes);
    assert_eq!(admits_fact(&ty("uppercase-string"), &decimal_fact), Yes);
    assert_eq!(admits_fact(&ty("decimal-int-string"), &decimal_fact), Yes);
    // The un-entailed one is honestly undecided rather than wrong.
    assert_eq!(admits_fact(&ty("non-falsy-string"), &decimal_fact), Maybe);
    // And the ceiling itself: the exclusion is real but inexpressible, so the
    // complement answers `Maybe` where an exclusion-aware lattice would say No.
    assert_eq!(admits_fact(&ty("non-decimal-int-string"), &decimal_fact), Maybe);
    // With the value in hand there is no ceiling at all.
    assert_eq!(admits_val(&ty("non-decimal-int-string"), &s("123")), No);
}

// ---------------------------------------------------------------------------
// C6 — the subtraction spellings.
// ---------------------------------------------------------------------------

#[test]
fn non_null_mixed_removes_exactly_null() {
    let t = ty("non-null-mixed");
    for v in [Val::Int(0), Val::Bool(false), s(""), Val::Float(0.0), arr(vec![])] {
        assert_eq!(admits_val(&t, &v), Yes, "{v:?} is not null");
    }
    assert_eq!(admits_val(&t, &Val::Null), No);
    // Against a fact: the base part is non-null by construction, so a
    // non-nullable fact is admitted outright. A *nullable* fact is the
    // crate-wide "some members yes, some no" case, which `all_of` reports as
    // `Maybe` — a `?int` argument really is sometimes null and sometimes not,
    // and only a proven `No` is ever reported.
    assert_eq!(admits_fact(&t, &Fact::General { base: Base::Int, nullable: false }), Yes);
    assert_eq!(admits_fact(&t, &Fact::General { base: Base::Int, nullable: true }), Maybe);
    // Compare: the same shape for any other null-excluding contract.
    assert_eq!(admits_fact(&ty("int"), &Fact::General { base: Base::Int, nullable: true }), Maybe);
}

#[test]
fn non_empty_mixed_removes_every_falsy_value() {
    let t = ty("non-empty-mixed");
    for truthy in [Val::Int(1), Val::Float(1.5), s("x"), s("0.0"), s("00"), Val::Bool(true)] {
        assert_eq!(admits_val(&t, &truthy), Yes, "{truthy:?}");
    }
    for falsy in [
        Val::Int(0),
        Val::Float(0.0),
        s(""),
        s("0"),
        Val::Bool(false),
        Val::Null,
        arr(vec![]),
    ] {
        assert_eq!(admits_val(&t, &falsy), No, "{falsy:?}");
    }
    assert_eq!(admits_val(&t, &list(vec![Val::Int(1)])), Yes, "a non-empty array is truthy");
}

#[test]
fn the_falsy_cut_decides_a_fact_only_where_the_refinement_answers() {
    let t = ty("non-empty-mixed");
    // `non-falsy-string` IS the string half of the cut.
    let non_falsy = Fact::Refined {
        base: Base::String,
        refinement: Refinement::Str(StrPreds::NON_FALSY.close()),
        nullable: false,
    };
    assert_eq!(admits_fact(&t, &non_falsy), Yes);
    // A general string holds both `''` and `'x'` — undecided, not refuted.
    assert_eq!(admits_fact(&t, &Fact::General { base: Base::String, nullable: false }), Maybe);
    // An int range missing zero is decided; one straddling it is not; the point
    // range at zero is refuted.
    let int_in = |lo, hi| Fact::Refined {
        base: Base::Int,
        refinement: Refinement::Int(IntRange::new(lo, hi).unwrap()),
        nullable: false,
    };
    assert_eq!(admits_fact(&t, &int_in(1, 10)), Yes);
    assert_eq!(admits_fact(&t, &int_in(-1, 10)), Maybe);
    assert_eq!(admits_fact(&t, &int_in(0, 0)), No);
    // A nullable fact's null half is refuted, but its base half is not, so
    // for-all returns the crate-wide mixed answer.
    assert_eq!(admits_fact(&t, &Fact::General { base: Base::String, nullable: true }), Maybe);
    let non_falsy_nullable = Fact::Refined {
        base: Base::String,
        refinement: Refinement::Str(StrPreds::NON_FALSY.close()),
        nullable: true,
    };
    assert_eq!(admits_fact(&t, &non_falsy_nullable), Maybe, "Yes base half, No null half");
}

#[test]
fn non_empty_scalar_is_the_cut_intersected_with_scalar() {
    let t = ty("non-empty-scalar");
    for ok in [Val::Int(1), Val::Int(-1), Val::Float(1.5), s("x"), Val::Bool(true)] {
        assert_eq!(admits_val(&t, &ok), Yes, "{ok:?}");
    }
    // All five of the fixture's probes, including the two PHPStan stays silent
    // on (`0`/`0.0`, which its un-narrowed `float` member swallows). Steins
    // spells the subtraction, so all five are decided.
    for falsy in [Val::Int(0), Val::Float(0.0), s(""), Val::Bool(false), s("0")] {
        assert_eq!(admits_val(&t, &falsy), No, "{falsy:?}");
    }
    // The `scalar` half holds independently of truthiness.
    assert_eq!(admits_val(&t, &list(vec![Val::Int(1)])), No, "an array is not a scalar");
    assert_eq!(admits_val(&t, &Val::Null), No, "null is not a scalar");
}

// ---------------------------------------------------------------------------
// C9 — the refined callable spellings (ADR-0063 P3). The vocabulary lowers to
// `CallableTy` plus an obligation triple, the obligation round-trips through the
// speller, and the closure-only half is decided in the value domain.
// ---------------------------------------------------------------------------

/// The obligation triple a spelling lowers to, as `(pure, is_static, closure_only)`.
fn obl(spelling: &str) -> (bool, bool, bool) {
    match ty(spelling) {
        steins_contract::ContractTy::CallableTy { obl, .. } => {
            (obl.pure, obl.is_static, obl.closure_only)
        }
        other => panic!("{spelling} must lower to a callable, got {other:?}"),
    }
}

#[test]
fn refined_callable_spellings_lower_to_their_obligations() {
    assert_eq!(obl("callable"), (false, false, false), "the bare spelling is unchanged");
    assert_eq!(obl("Closure"), (false, false, false), "bare Closure is untightened this slice");
    assert_eq!(obl("callable-object"), (false, false, false), "wider than Closure");
    assert_eq!(obl("pure-callable"), (true, false, false));
    assert_eq!(obl("pure-closure"), (true, false, true));
    assert_eq!(obl("static-closure"), (false, true, true));
    assert_eq!(obl("static-pure-closure"), (true, true, true));
}

#[test]
fn a_parenthesized_signature_keeps_the_obligation() {
    // `pure-callable(int): int` is both a call shape and a purity obligation.
    let steins_contract::ContractTy::CallableTy { sig, obl } = ty("pure-callable(int): int") else {
        panic!("must lower to a callable");
    };
    assert!(sig.is_some(), "the signature survives");
    assert!(obl.pure, "and so does the obligation");
    assert!(!obl.is_bare());
}

#[test]
fn refined_callable_spellings_round_trip_through_the_speller() {
    // The speller is reached through a nested slot (a bare callable arm has no
    // faithful scalar spelling and is refused at top level, as before).
    for spelling in
        ["callable", "pure-callable", "pure-closure", "static-closure", "static-pure-closure"]
    {
        let shape = ty(&format!("array{{cb: {spelling}}}"));
        let spelled = steins_contract::spell::spell_arms(std::slice::from_ref(&shape))
            .unwrap_or_else(|| panic!("shape must spell: {spelling}"));
        assert_eq!(spelled, format!("array{{cb: {spelling}}}"), "faithful round trip");
    }
}

#[test]
fn the_closure_only_half_is_decided_in_the_value_domain() {
    // A callable-string / callable-array names a function or a method; neither is
    // ever a `Closure` instance, and that half needs no purity analysis.
    for closure_only in ["pure-closure", "static-closure", "static-pure-closure"] {
        let t = ty(closure_only);
        assert_eq!(admits_val(&t, &s("strlen")), No, "{closure_only}: a string is not a Closure");
        assert_eq!(
            admits_val(&t, &list(vec![s("Foo"), s("bar")])),
            No,
            "{closure_only}: an array is not a Closure",
        );
        assert_eq!(admits_val(&t, &Val::Int(1)), No, "{closure_only}: not callable at all");
        assert_eq!(
            admits_fact(&t, &Fact::General { base: Base::String, nullable: false }),
            No,
            "{closure_only}: a definitely-string fact is not a Closure",
        );
    }
    // The `callable` spellings keep the historical `Maybe` — a string may name a
    // pure function, which the value alone cannot decide.
    for wide in ["callable", "pure-callable"] {
        let t = ty(wide);
        assert_eq!(admits_val(&t, &s("strlen")), Maybe, "{wide}: callable-string candidate");
        assert_eq!(admits_val(&t, &Val::Int(1)), No, "{wide}: not callable at all");
        assert_eq!(admits_fact(&t, &Fact::General { base: Base::String, nullable: false }), Maybe, "{wide}");
    }
}
