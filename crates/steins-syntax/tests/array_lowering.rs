//! Acceptance tests for array-literal lowering into the trace IR (ADR-0001):
//! key normalization, auto (next-int) keys, nested arrays, and the spread /
//! unrepresentable-element → `Other` fallback.

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

#[test]
fn plain_list_uses_auto_keys() {
    let v = first_arg("<?php f(['a', 'b', 'c']);");
    let it = items(&v);
    assert_eq!(it.len(), 3);
    assert!(it.iter().all(|(k, _)| matches!(k, ArrayKey::Auto)));
    assert_eq!(it[0].1, ArgValue::Str("a".into()));
    // Normalization assigns 0, 1, 2.
    let norm = normalize_array(it);
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
    assert_eq!(it[3].0, ArrayKey::Str(String::new()));
}

#[test]
fn next_int_follows_largest_explicit_int_key() {
    // [5 => 'a', 'b'] → 'b' gets key 6 (one past the largest int key seen).
    let v = first_arg("<?php f([5 => 'a', 'b']);");
    let norm = normalize_array(items(&v));
    assert_eq!(norm[0].0, NormKey::Int(5));
    assert_eq!(norm[1].0, NormKey::Int(6));
}

#[test]
fn duplicate_keys_resolve_last_wins() {
    // [0 => 'a', 0 => 'b'] → one entry, value 'b', at the first position.
    let v = first_arg("<?php f([0 => 'a', 0 => 'b']);");
    let norm = normalize_array(items(&v));
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
fn non_literal_key_collapses_to_other() {
    let v = first_arg("<?php f([$k => 1]);");
    assert_eq!(v, ArgValue::Other);
}

#[test]
fn variable_element_stays_representable() {
    // A bare `$x` element is a representable carrier (resolved later against env).
    let v = first_arg("<?php f([$x, 2]);");
    let it = items(&v);
    assert_eq!(it[0].1, ArgValue::Var("x".into()));
}
