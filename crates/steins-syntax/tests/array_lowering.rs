//! Acceptance tests for array-literal lowering into the trace IR (ADR-0001):
//! key normalization, next-int auto keys, nested arrays, spread/unrepresentable → `Other`.

use steins_domain::PhpStr;
use steins_syntax::{
    ArgValue, ArrayKey, NextIntRule, NormKey, SourceTree, next_int_is_version_dependent,
    normalize_array, normalize_array_with,
};

/// The `ArgValue` of the first positional argument of the first function call.
fn first_arg(src: &str) -> ArgValue {
    let tree = SourceTree::parse(src);
    tree.calls()[0].args[0].value.clone()
}

fn items(v: &ArgValue) -> &[(ArrayKey, ArgValue)] {
    match v {
        ArgValue::Array(items) => items,
        other => panic!("expected Array, got {other:?}"),
    }
}

/// Normalize with an unknown minor: asserts the literal is version-independent (rules agree).
fn norm_unknown(it: &[(ArrayKey, ArgValue)]) -> Vec<(NormKey, ArgValue)> {
    assert!(!next_int_is_version_dependent(it), "fixture is version-dependent");
    normalize_array(it, None).expect("version-independent literal resolves without a minor")
}

#[test]
fn plain_list_uses_auto_keys() {
    let v = first_arg("<?php f(['a', 'b', 'c']);");
    let it = items(&v);
    assert_eq!(it.len(), 3);
    assert!(it.iter().all(|(k, _)| matches!(k, ArrayKey::Auto)));
    assert_eq!(it[0].1, ArgValue::Str("a".into()));
    let norm = norm_unknown(it);
    assert_eq!(norm[0].0, NormKey::Int(0));
    assert_eq!(norm[2].0, NormKey::Int(2));
}

#[test]
fn legacy_array_syntax_lowers_the_same() {
    let v = first_arg("<?php f(array(1, 2));");
    let it = items(&v);
    assert_eq!(it.len(), 2);
    assert_eq!(it[1].1, ArgValue::Int(2));
}

#[test]
fn integer_like_string_key_normalizes_to_int() {
    // "5" is a canonical integer string → Int(5); "05" and "+5" stay strings.
    let v = first_arg("<?php f(['5' => 'a', '05' => 'b', '+5' => 'c']);");
    let it = items(&v);
    assert_eq!(it[0].0, ArrayKey::Int(5));
    assert_eq!(it[1].0, ArrayKey::Str("05".into()));
    assert_eq!(it[2].0, ArrayKey::Str("+5".into()));
}

#[test]
fn bool_float_null_keys_normalize_php_faithfully() {
    // true→1, false→0, 1.9→1 (truncate), null→"".
    let v = first_arg("<?php f([true => 'a', false => 'b', 1.9 => 'c', null => 'd']);");
    let it = items(&v);
    assert_eq!(it[0].0, ArrayKey::Int(1));
    assert_eq!(it[1].0, ArrayKey::Int(0));
    assert_eq!(it[2].0, ArrayKey::Int(1));
    assert_eq!(it[3].0, ArrayKey::Str(PhpStr::new()));
}

#[test]
fn next_int_follows_largest_explicit_int_key() {
    // [5 => 'a', 'b'] → 'b' gets key 6 (one past the largest int key seen).
    let v = first_arg("<?php f([5 => 'a', 'b']);");
    let norm = norm_unknown(items(&v));
    assert_eq!(norm[0].0, NormKey::Int(5));
    assert_eq!(norm[1].0, NormKey::Int(6));
}

// Negative keys: the PHP 8.3 next-auto-index change (ADR-0049 A12)
// Every expectation below is a `php -r 'var_export(...)'` witness on PHP 8.5.8,
// never recall; the pre-8.3 column is what PHP < 8.3 documents (floor at 0;
// Steins's floor is 8.1, ADR-0011).

#[test]
fn negative_key_next_int_splits_on_the_83_rule() {
    // php -r 'var_export([-5=>"a","b"]);' on 8.5.8 → -5, -4.
    let v = first_arg("<?php f([-5 => 'a', 'b']);");
    let it = items(&v);
    assert!(next_int_is_version_dependent(it));

    let post = normalize_array_with(it, NextIntRule::MaxPlusOne);
    assert_eq!(post[0].0, NormKey::Int(-5));
    assert_eq!(post[1].0, NormKey::Int(-4));

    // PHP < 8.3 floors the next auto-index at 0.
    let pre = normalize_array_with(it, NextIntRule::FloorAtZero);
    assert_eq!(pre[0].0, NormKey::Int(-5));
    assert_eq!(pre[1].0, NormKey::Int(0));
}

#[test]
fn reported_minor_picks_the_rule_and_unknown_declines() {
    let v = first_arg("<?php f([-5 => 'a', 'b']);");
    let it = items(&v);

    // A known minor resolves exactly — on either side of the 8.3 boundary.
    for (minor, want) in [((8, 1), 0), ((8, 2), 0), ((8, 3), -4), ((8, 5), -4)] {
        let norm = normalize_array(it, Some(minor)).expect("a known minor always resolves");
        assert_eq!(norm[1].0, NormKey::Int(want), "PHP {minor:?}");
    }

    // Unknown minor + version-dependent literal → unproven; drop the fact, don't guess.
    assert_eq!(normalize_array(it, None), None);
}

#[test]
fn version_independent_literals_resolve_without_a_minor() {
    // No negative key anywhere → the two rules agree, so an unknown minor still answers.
    for src in ["<?php f(['a', 'b']);", "<?php f([5 => 'a', 'b']);", "<?php f(['k' => 1, 'b']);"] {
        let v = first_arg(src);
        let it = items(&v);
        assert!(!next_int_is_version_dependent(it), "{src}");
        assert!(normalize_array(it, None).is_some(), "{src}");
    }

    // A negative key with no later omitted key is version-independent too.
    let v = first_arg("<?php f([-5 => 'a', 3 => 'b']);");
    assert!(!next_int_is_version_dependent(items(&v)));
}

#[test]
fn next_int_tracks_the_running_max_not_the_last_key() {
    // php -r 'var_export([3=>"a",-5=>"b","c"]);' on 8.5.8 → 3, -5, 4: the index
    // is one past the largest key seen, and never moves backwards.
    let v = first_arg("<?php f([3 => 'a', -5 => 'b', 'c']);");
    let it = items(&v);
    // The running max is already 3 (≥ 0), so both rules agree here.
    assert!(!next_int_is_version_dependent(it));
    let norm = norm_unknown(it);
    assert_eq!(norm[2].0, NormKey::Int(4));

    // php -r 'var_export([-5=>"a",-10=>"b","c"]);' → -5, -10, -4: max, not last.
    let v = first_arg("<?php f([-5 => 'a', -10 => 'b', 'c']);");
    let post = normalize_array_with(items(&v), NextIntRule::MaxPlusOne);
    assert_eq!(post[2].0, NormKey::Int(-4));
}

#[test]
fn duplicate_negative_key_still_advances_the_index() {
    // php -r 'var_export([-5=>"a",-5=>"b","c"]);' on 8.5.8 → -5 => 'b', -4 => 'c'.
    // Last-wins folds the value; the key still counted toward the next index.
    let v = first_arg("<?php f([-5 => 'a', -5 => 'b', 'c']);");
    let norm = normalize_array_with(items(&v), NextIntRule::MaxPlusOne);
    assert_eq!(norm.len(), 2);
    assert_eq!(norm[0].0, NormKey::Int(-5));
    assert_eq!(norm[0].1, ArgValue::Str("b".into()));
    assert_eq!(norm[1].0, NormKey::Int(-4));
}

#[test]
fn auto_keys_climb_out_of_the_negatives() {
    // php -r 'var_export([-5=>"a","b",-1=>"z","c"]);' on 8.5.8
    //   → -5 => 'a', -4 => 'b', -1 => 'z', 0 => 'c'.
    let v = first_arg("<?php f([-5 => 'a', 'b', -1 => 'z', 'c']);");
    let norm = normalize_array_with(items(&v), NextIntRule::MaxPlusOne);
    let keys: Vec<_> = norm.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(
        keys,
        vec![NormKey::Int(-5), NormKey::Int(-4), NormKey::Int(-1), NormKey::Int(0)]
    );
}

/// Adversarial counterexamples #46 names by hand, each a `php -r` witness on
/// PHP 8.5.8: the running **max** (not last key, not its sign) drives the next index.
#[test]
fn adversarial_negative_key_shapes() {
    // Mixed negative and positive explicit keys. `array_keys([-5=>a,3=>b,c])`
    // → [-5, 3, 4]: the positive key lifts the max, so both rules agree.
    let v = first_arg("<?php f([-5 => 'a', 3 => 'b', 'c']);");
    let it = items(&v);
    assert!(!next_int_is_version_dependent(it));
    assert_eq!(norm_unknown(it)[2].0, NormKey::Int(4));

    // A negative key *after* a larger auto key: `array_keys(['a',-5=>b,c])` →
    // [0, -5, 1] — the auto key already pushed max to 0; negatives can't pull it back.
    let v = first_arg("<?php f(['a', -5 => 'b', 'c']);");
    let it = items(&v);
    assert!(!next_int_is_version_dependent(it));
    let norm = norm_unknown(it);
    let keys: Vec<_> = norm.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(keys, vec![NormKey::Int(0), NormKey::Int(-5), NormKey::Int(1)]);

    // String keys interleaved, before and around the negative key. A string key
    // never touches the integer index: both witness [_, -5, -4].
    for src in ["<?php f(['k' => 'a', -5 => 'b', 'c']);", "<?php f([-5 => 'a', 'k' => 'b', 'c']);"] {
        let v = first_arg(src);
        let it = items(&v);
        assert!(next_int_is_version_dependent(it), "{src}");
        let post = normalize_array_with(it, NextIntRule::MaxPlusOne);
        assert_eq!(post[2].0, NormKey::Int(-4), "{src}");
        // Pre-8.3 floors that same slot at 0.
        assert_eq!(normalize_array_with(it, NextIntRule::FloorAtZero)[2].0, NormKey::Int(0), "{src}");
    }

    // Negative, then positive, then negative again, with autos throughout.
    // `array_keys([-5=>a,b,10=>c,d,-1=>e,f])` → [-5, -4, 10, 11, -1, 12].
    let v = first_arg("<?php f([-5 => 'a', 'b', 10 => 'c', 'd', -1 => 'e', 'f']);");
    let it = items(&v);
    assert!(next_int_is_version_dependent(it));
    let post = normalize_array_with(it, NextIntRule::MaxPlusOne);
    let keys: Vec<_> = post.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(
        keys,
        vec![
            NormKey::Int(-5),
            NormKey::Int(-4),
            NormKey::Int(10),
            NormKey::Int(11),
            NormKey::Int(-1),
            NormKey::Int(12),
        ]
    );
}

/// `-1` is the exact edge of the 8.3 change: one past it is `0`, which the
/// pre-8.3 floor also yields, so a `-1` key is *not* version-dependent.
/// Witnessed: `array_keys([-1=>"a","b","c"])` → `[-1, 0, 1]` on PHP 8.5.8.
#[test]
fn minus_one_is_the_boundary_where_the_rules_reconverge() {
    let v = first_arg("<?php f([-1 => 'a', 'b', 'c']);");
    let it = items(&v);
    assert!(!next_int_is_version_dependent(it));
    let norm = norm_unknown(it);
    let keys: Vec<_> = norm.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(keys, vec![NormKey::Int(-1), NormKey::Int(0), NormKey::Int(1)]);

    // `-2` is the first key that does split them.
    let v = first_arg("<?php f([-2 => 'a', 'b']);");
    assert!(next_int_is_version_dependent(items(&v)));
}

/// `render_array` is the one consumer A12 exempts: not a proof-layer premise,
/// so it takes the pinned rule unconditionally instead of threading the minor
/// (issue #46 criterion 3) — a future change to thread it shows as a test diff.
#[test]
fn rendering_takes_the_pinned_rule_and_never_declines() {
    let v = first_arg("<?php f([-5 => 'a', 'b']);");
    // Pinned rule (8.3+) puts 'b' at -4; version-dependent, yet rendering produces a message.
    assert!(next_int_is_version_dependent(items(&v)));
    assert_eq!(v.render(), "[-5 => 'a', -4 => 'b']");

    assert_eq!(first_arg("<?php f(['a', 'b']);").render(), "['a', 'b']");
}

/// The load-bearing invariant behind A12's narrow widening: whenever
/// `next_int_is_version_dependent` says "no", the two rules agree, so an unknown
/// minor is sound. Exhaustive over key sequences ≤ length 4 (omitted/neg/zero/pos/string).
#[test]
fn version_independence_implies_the_two_rules_agree() {
    let alphabet = [
        ArrayKey::Auto,
        ArrayKey::Int(-2),
        ArrayKey::Int(-1),
        ArrayKey::Int(0),
        ArrayKey::Int(1),
        ArrayKey::Str("k".into()),
    ];
    let val = ArgValue::Int(0);
    let mut checked = 0usize;
    let mut dependent = 0usize;

    for len in 0..=4 {
        let total = alphabet.len().pow(len as u32);
        for n in 0..total {
            let mut seq = Vec::with_capacity(len);
            let mut rest = n;
            for _ in 0..len {
                seq.push((alphabet[rest % alphabet.len()].clone(), val.clone()));
                rest /= alphabet.len();
            }
            let pre = normalize_array_with(&seq, NextIntRule::FloorAtZero);
            let post = normalize_array_with(&seq, NextIntRule::MaxPlusOne);
            if next_int_is_version_dependent(&seq) {
                dependent += 1;
                // Declared dependent → an unknown minor must decline.
                assert_eq!(normalize_array(&seq, None), None, "{seq:?}");
            } else {
                assert_eq!(pre, post, "declared version-independent but rules differ: {seq:?}");
                assert_eq!(normalize_array(&seq, None).as_ref(), Some(&post), "{seq:?}");
            }
            checked += 1;
        }
    }

    assert_eq!(checked, 1 + 6 + 36 + 216 + 1296);
    // The predicate is not vacuously false — it fires on a real slice of them.
    assert!(dependent > 0, "no version-dependent sequence in the sweep");
}

#[test]
fn rule_selection_brackets_the_83_boundary() {
    assert_eq!(NextIntRule::for_minor((8, 1)), NextIntRule::FloorAtZero);
    assert_eq!(NextIntRule::for_minor((8, 2)), NextIntRule::FloorAtZero);
    assert_eq!(NextIntRule::for_minor((8, 3)), NextIntRule::MaxPlusOne);
    assert_eq!(NextIntRule::for_minor((8, 5)), NextIntRule::MaxPlusOne);
    assert_eq!(NextIntRule::for_minor((9, 0)), NextIntRule::MaxPlusOne);
}

#[test]
fn duplicate_keys_resolve_last_wins() {
    // [0 => 'a', 0 => 'b'] → one entry, value 'b', at the first position.
    let v = first_arg("<?php f([0 => 'a', 0 => 'b']);");
    let norm = norm_unknown(items(&v));
    assert_eq!(norm.len(), 1);
    assert_eq!(norm[0].0, NormKey::Int(0));
    assert_eq!(norm[0].1, ArgValue::Str("b".into()));
}

#[test]
fn nested_arrays_lower_recursively() {
    let v = first_arg("<?php f([[1, 2], ['k' => 3]]);");
    let it = items(&v);
    assert_eq!(it.len(), 2);
    assert!(matches!(&it[0].1, ArgValue::Array(inner) if inner.len() == 2));
    assert!(matches!(&it[1].1, ArgValue::Array(inner) if inner[0].0 == ArrayKey::Str("k".into())));
}

#[test]
fn spread_collapses_whole_array_to_other() {
    let v = first_arg("<?php f([1, ...$rest, 2]);");
    assert_eq!(v, ArgValue::Other, "a spread makes the whole array unrepresentable");
}

#[test]
fn unrepresentable_element_collapses_to_other() {
    // A dynamic method call as an element value lowers to `Other` → whole array Other.
    let v = first_arg("<?php f([$obj->m(), 2]);");
    assert_eq!(v, ArgValue::Other);
}

#[test]
fn a_non_literal_key_is_carried_rather_than_collapsing() {
    // This pinned `ArgValue::Other` — one unspellable key dropped the WHOLE
    // literal, breaking `array_key_first`/`foreach` (issue #336). The key
    // expression is carried instead, even though which key it lands on is unknown.
    let v = first_arg("<?php f([$k => 1]);");
    let it = items(&v);
    assert_eq!(it.len(), 1);
    assert_eq!(it[0].0, ArrayKey::Expr(Box::new(ArgValue::Var("k".into()))));
    assert_eq!(it[0].1, ArgValue::Int(1));
    // Not a normalizable key set: an unknown key may be an integer, moving the auto-index.
    assert_eq!(normalize_array(it, Some((8, 5))), None);
}

#[test]
fn an_unrepresentable_key_expression_still_collapses() {
    // Carrying needs a key to carry; an `Other`-lowering expression leaves nothing to carry.
    assert_eq!(first_arg("<?php f([$obj->m() => 1]);"), ArgValue::Other);
}

#[test]
fn variable_element_stays_representable() {
    // A bare `$x` element is a representable carrier (resolved later against env).
    let v = first_arg("<?php f([$x, 2]);");
    let it = items(&v);
    assert_eq!(it[0].1, ArgValue::Var("x".into()));
}

// `is_concrete_value`: the self-evident-value predicate (issue #39)

#[test]
fn concrete_value_covers_scalars_and_literal_arrays() {
    for src in [
        "<?php f(1);",
        "<?php f('s');",
        "<?php f(null);",
        "<?php f([]);", // the empty array IS a value — `count([])` folds to 0
        "<?php f([1, 2, 3]);",
        "<?php f(['k' => 'v', 5 => 1.5, true]);",
        "<?php f([[1, 2], ['k' => [3]]]);", // nesting is represented, not widened
    ] {
        assert!(first_arg(src).is_concrete_value(), "{src} should be a concrete value");
    }
}

#[test]
fn one_unresolved_element_makes_the_whole_array_non_concrete() {
    // A carrier element (`$x`, a call) is representable but not a proven value, at any depth.
    for src in [
        "<?php f([$x]);",
        "<?php f([1, $x, 3]);",
        "<?php f([1, strtolower('A')]);",
        "<?php f([[1, [2, $x]]]);",
    ] {
        let v = first_arg(src);
        assert!(matches!(v, ArgValue::Array(_)), "{src} still lowers to an Array");
        assert!(!v.is_concrete_value(), "{src} must not be a concrete value");
    }
}
