//! Acceptance tests for array-literal lowering into the trace IR (ADR-0001):
//! key normalization, next-int auto keys, nested arrays, spread/unrepresentable → `Other`.

use steins_domain::PhpStr;
use steins_syntax::{ArgValue, ArrayKey, NormKey, SourceTree, normalize_array};

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

/// Normalize a literal whose every key is spelled and every omitted key has a next index.
fn norm(it: &[(ArrayKey, ArgValue)]) -> Vec<(NormKey, ArgValue)> {
    normalize_array(it).expect("the literal resolves")
}

fn keys(it: &[(ArrayKey, ArgValue)]) -> Vec<NormKey> {
    norm(it).into_iter().map(|(k, _)| k).collect()
}

#[test]
fn plain_list_uses_auto_keys() {
    let v = first_arg("<?php f(['a', 'b', 'c']);");
    let it = items(&v);
    assert_eq!(it.len(), 3);
    assert!(it.iter().all(|(k, _)| matches!(k, ArrayKey::Auto)));
    assert_eq!(it[0].1, ArgValue::Str("a".into()));
    let norm = norm(it);
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
    let norm = norm(items(&v));
    assert_eq!(norm[0].0, NormKey::Int(5));
    assert_eq!(norm[1].0, NormKey::Int(6));
}

// Negative keys (ADR-0049 A22). An omitted key takes one past the largest
// integer key seen, negative or not, on every supported minor: the
// negative-index RFC landed in PHP 8.0, and every row below prints the same keys
// through `php -r` on 8.1.32, 8.2.33 and 8.5.10. There is no pre-8.3 column to
// assert — A12's floor at `0` is PHP 7.4 (7.4.33 prints `-5, 0` for the first row).

#[test]
fn an_omitted_key_after_a_negative_key_counts_it() {
    // `[-5 => 'a', 'b']` → -5, -4 on 8.0.28 through 8.5.10.
    let v = first_arg("<?php f([-5 => 'a', 'b']);");
    assert_eq!(keys(items(&v)), vec![NormKey::Int(-5), NormKey::Int(-4)]);

    // The same keys through `array(...)`: the spelling builds the same literal.
    let v = first_arg("<?php f(array(-5 => 'a', 'b'));");
    assert_eq!(keys(items(&v)), vec![NormKey::Int(-5), NormKey::Int(-4)]);
}

#[test]
fn next_int_tracks_the_running_max_not_the_last_key() {
    // `[3 => 'a', -5 => 'b', 'c']` → 3, -5, 4: the index is one past the largest
    // key seen, and never moves backwards.
    let v = first_arg("<?php f([3 => 'a', -5 => 'b', 'c']);");
    assert_eq!(keys(items(&v))[2], NormKey::Int(4));

    // `[-5 => 'a', -10 => 'b', 'c']` → -5, -10, -4: max, not last.
    let v = first_arg("<?php f([-5 => 'a', -10 => 'b', 'c']);");
    assert_eq!(keys(items(&v))[2], NormKey::Int(-4));
}

#[test]
fn duplicate_negative_key_still_advances_the_index() {
    // `[-5 => 'a', -5 => 'b', 'c']` → -5 => 'b', -4 => 'c'. Last-wins folds the
    // value; the key still counted toward the next index.
    let v = first_arg("<?php f([-5 => 'a', -5 => 'b', 'c']);");
    let norm = norm(items(&v));
    assert_eq!(norm.len(), 2);
    assert_eq!(norm[0].0, NormKey::Int(-5));
    assert_eq!(norm[0].1, ArgValue::Str("b".into()));
    assert_eq!(norm[1].0, NormKey::Int(-4));
}

#[test]
fn auto_keys_climb_out_of_the_negatives() {
    // `[-5 => 'a', 'b', -1 => 'z', 'c']` → -5 => 'a', -4 => 'b', -1 => 'z', 0 => 'c'.
    let v = first_arg("<?php f([-5 => 'a', 'b', -1 => 'z', 'c']);");
    assert_eq!(
        keys(items(&v)),
        vec![NormKey::Int(-5), NormKey::Int(-4), NormKey::Int(-1), NormKey::Int(0)]
    );
}

/// Adversarial counterexamples #46 names by hand: the running **max** (not the
/// last key, not its sign) drives the next index.
#[test]
fn adversarial_negative_key_shapes() {
    // Mixed negative and positive explicit keys: `[-5 => a, 3 => b, c]` → -5, 3, 4.
    let v = first_arg("<?php f([-5 => 'a', 3 => 'b', 'c']);");
    assert_eq!(keys(items(&v))[2], NormKey::Int(4));

    // A negative key *after* a larger auto key: `['a', -5 => b, c]` → 0, -5, 1 —
    // the auto key already pushed the max to 0; negatives can't pull it back.
    let v = first_arg("<?php f(['a', -5 => 'b', 'c']);");
    assert_eq!(keys(items(&v)), vec![NormKey::Int(0), NormKey::Int(-5), NormKey::Int(1)]);

    // String keys interleaved, before and around the negative key. A string key
    // never touches the integer index: both give [_, -5, -4].
    for src in ["<?php f(['k' => 'a', -5 => 'b', 'c']);", "<?php f([-5 => 'a', 'k' => 'b', 'c']);"] {
        let v = first_arg(src);
        assert_eq!(keys(items(&v))[2], NormKey::Int(-4), "{src}");
    }

    // Negative, then positive, then negative again, with autos throughout:
    // `[-5 => a, b, 10 => c, d, -1 => e, f]` → -5, -4, 10, 11, -1, 12.
    let v = first_arg("<?php f([-5 => 'a', 'b', 10 => 'c', 'd', -1 => 'e', 'f']);");
    assert_eq!(
        keys(items(&v)),
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

/// One past `-1` is `0` and one past `-2` is `-1`: no special case sits at zero.
/// `[-1 => a, b, c]` → -1, 0, 1 and `[-2 => a, b]` → -2, -1.
#[test]
fn the_index_crosses_zero_without_a_floor() {
    let v = first_arg("<?php f([-1 => 'a', 'b', 'c']);");
    assert_eq!(keys(items(&v)), vec![NormKey::Int(-1), NormKey::Int(0), NormKey::Int(1)]);

    let v = first_arg("<?php f([-2 => 'a', 'b']);");
    assert_eq!(keys(items(&v)), vec![NormKey::Int(-2), NormKey::Int(-1)]);
}

/// `render_array` resolves keys exactly as the proof layer does.
#[test]
fn rendering_places_negative_keys_like_normalization() {
    let v = first_arg("<?php f([-5 => 'a', 'b']);");
    assert_eq!(v.render(), "[-5 => 'a', -4 => 'b']");

    assert_eq!(first_arg("<?php f(['a', 'b']);").render(), "['a', 'b']");

    // PHP throws on this literal, so it has no normalized form: it renders as written.
    let v = first_arg("<?php f([9223372036854775807 => 1, 'a' => 2, 3]);");
    assert_eq!(v.render(), "[9223372036854775807 => 1, 'a' => 2, 3]");
}

/// PHP has no next key past `PHP_INT_MAX`: `[9223372036854775807 => 1, 2]`
/// throws "Cannot add element to the array as the next element is already
/// occupied" (`php -r` on 8.5.10 and 8.2.33, a variable item too), so the
/// literal builds no array, and normalization declines rather than fold `2` onto
/// a clamped key. phpstan-src's `bug-15248.php` drops the item instead
/// (`array{9223372036854775807: 1}`), which PHP never produces either.
#[test]
fn an_omitted_key_past_php_int_max_declines() {
    for src in [
        "<?php f([9223372036854775807 => 1, 2]);",
        "<?php f([9223372036854775807 => 1, $x]);",
        "<?php f([9223372036854775806 => 1, 2, 3]);",
        "<?php f([-2 => 1, 9223372036854775807 => 2, 3]);",
    ] {
        let v = first_arg(src);
        assert_eq!(normalize_array(items(&v)), None, "{src}");
    }
}

/// `PHP_INT_MAX` is still a key an omitted one can take, and a written key never
/// needs a next one. Both `php -r`-witnessed on 8.5.10.
#[test]
fn php_int_max_resolves_as_the_last_free_key_and_as_a_written_key() {
    // → [9223372036854775806 => 1, 9223372036854775807 => 2]
    let v = first_arg("<?php f([9223372036854775806 => 1, 2]);");
    assert_eq!(keys(items(&v)), vec![NormKey::Int(i64::MAX - 1), NormKey::Int(i64::MAX)]);

    // → [9223372036854775807 => 3, 'a' => 2]
    let v = first_arg("<?php f([9223372036854775807 => 1, 'a' => 2, 9223372036854775807 => 3]);");
    let norm = norm(items(&v));
    assert_eq!(norm.len(), 2);
    assert_eq!(norm[0], (NormKey::Int(i64::MAX), ArgValue::Int(3)));
    assert_eq!(norm[1].0, NormKey::Str("a".into()));
}

#[test]
fn duplicate_keys_resolve_last_wins() {
    // [0 => 'a', 0 => 'b'] → one entry, value 'b', at the first position.
    let v = first_arg("<?php f([0 => 'a', 0 => 'b']);");
    let norm = norm(items(&v));
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
    // A *dynamic* method call as an element value lowers to `Other` → whole array
    // Other. The method name is a variable, so no `Callee` names it (issue #386
    // gave the STATICALLY named form a carrier — see the test below).
    let v = first_arg("<?php f([$obj->$m(), 2]);");
    assert_eq!(v, ArgValue::Other);
}

#[test]
fn a_method_call_element_no_longer_collapses_the_array() {
    // Issue #386: `[$obj->m(), 2]` used to drop the whole literal, so a
    // `count()`/`foreach` over it knew nothing about the sibling `2` either. The
    // element is a carrier now — unproven on its own, exactly like a `$x` element.
    let v = first_arg("<?php f([$obj->m(), 2]);");
    let it = items(&v);
    assert_eq!(it.len(), 2);
    assert!(matches!(&it[0].1, ArgValue::MethodCall { .. }), "the call is carried: {:?}", it[0].1);
    assert_eq!(it[1].1, ArgValue::Int(2));
    // Carried is not proven: the array is not a self-evident value (issue #39).
    assert!(!v.is_concrete_value());
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
    assert_eq!(normalize_array(it), None);
}

#[test]
fn an_unrepresentable_key_expression_still_collapses() {
    // Carrying needs a key to carry; an `Other`-lowering expression leaves nothing
    // to carry. A dynamic method name is such an expression — the statically named
    // `$obj->m()` became a carrier in issue #386 and is carried as a key like any
    // other unproven one.
    assert_eq!(first_arg("<?php f([$obj->$m() => 1]);"), ArgValue::Other);
    let carried = first_arg("<?php f([$obj->m() => 1]);");
    let it = items(&carried);
    assert!(matches!(&it[0].0, ArrayKey::Expr(k) if matches!(**k, ArgValue::MethodCall { .. })));
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
