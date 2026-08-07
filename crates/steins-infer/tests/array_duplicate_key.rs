//! `array.duplicate-key` (ADR-0078, issue #187, mechanics layer).
//!
//! A literal array expression that declares the same PHP-normalized key twice
//! silently drops the earlier value. Purely syntactic — the evidence is the
//! literal itself, not a proven runtime path — so these tests drive the plain
//! [`check`] entry point except where a specific PHP minor must be pinned for
//! the ADR-0049 A12 auto-increment edge case, which uses [`check_with`] with a
//! minor-reporting [`Folder`].
//!
//! Every coerced-equal pair and the auto-increment interplay claim below is
//! `php -r`-witnessed on PHP 8.5.9 (the sandbox's `php`), verbatim in each
//! test's comment.

use steins_infer::{ARRAY_DUPLICATE_KEY_ID, Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A folder that reports a fixed PHP `(major, minor)` and never folds — used
/// only by the ADR-0049 A12 auto-increment version-dependence tests below.
struct FixedMinor(u16, u16);

impl Folder for FixedMinor {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        Some((self.0, self.1))
    }
}

fn dups(src: &str) -> Vec<Diagnostic> {
    check(&SourceTree::parse(src), &[], "test.php")
        .into_iter()
        .filter(|d| d.id == ARRAY_DUPLICATE_KEY_ID)
        .collect()
}

fn dups_with_minor(src: &str, minor: (u16, u16)) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut FixedMinor(minor.0, minor.1))
        .into_iter()
        .filter(|d| d.id == ARRAY_DUPLICATE_KEY_ID)
        .collect()
}

// --- Firing fixtures -------------------------------------------------------

#[test]
fn fires_on_a_plain_duplicate_int_key() {
    // php -r 'var_export([1 => "a", 1 => "b"]);' → [1 => 'b'] (one element).
    let src = "<?php\n$a = [\n    1 => 'a',\n    1 => 'b',\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 4, "positioned at the LATER (winning) occurrence");
    assert!(d[0].message.contains('1'), "{}", d[0].message);
    assert!(d[0].message.contains("line 3"), "names the shadowed earlier entry's line: {}", d[0].message);
}

#[test]
fn silence_on_distinct_keys() {
    let d = dups("<?php\n$a = ['a' => 1, 'b' => 2];\n");
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn silence_on_an_empty_array() {
    let d = dups("<?php\n$a = [];\n");
    assert!(d.is_empty(), "{d:#?}");
}

// --- PHP key-coercion pairs (ADR-0062's A12 coercion, reused verbatim) -----

#[test]
fn fires_on_int_vs_int_like_string() {
    // php -r 'var_export([1 => "a", "1" => "b"]);' → [1 => 'b'].
    let d = dups("<?php\n$a = [\n    1 => 'a',\n    '1' => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_bool_true_vs_int_one() {
    // php -r 'var_export([true => "a", 1 => "b"]);' → [1 => 'b'].
    let d = dups("<?php\n$a = [\n    true => 'a',\n    1 => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_null_vs_empty_string() {
    // php -r 'var_export([null => "a", "" => "b"]);' → ['' => 'b'] (with a
    // "null as array offset" deprecation notice — the value still collapses).
    let d = dups("<?php\n$a = [\n    null => 'a',\n    '' => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_truncated_float_vs_int() {
    // php -r 'var_export([1.7 => "a", 1 => "b"]);' → [1 => 'b'] (with an
    // "implicit conversion from float 1.7" deprecation notice — truncation
    // toward zero still lands on the same key as the explicit int).
    let d = dups("<?php\n$a = [\n    1.7 => 'a',\n    1 => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

// --- Non-folded keys: silence, never a guess --------------------------------

#[test]
fn silence_on_a_variable_key() {
    let d = dups("<?php\nfunction f($x) {\n    $a = [$x => 'a', $x => 'b'];\n}\n");
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn silence_on_a_call_key() {
    let d = dups("<?php\nfunction k() { return 1; }\n$a = [k() => 'a', k() => 'b'];\n");
    assert!(d.is_empty(), "{d:#?}");
}

// --- `'01'` is a distinct string key, not int 1 -----------------------------

#[test]
fn zero_padded_string_stays_distinct_from_int_one() {
    // php -r 'var_export(["01" => "a", "1" => "b", 1 => "c"]);'
    //   → ['01' => 'a', 1 => 'c'] — '1' and 1 collapse together, '01' stays its
    //     own entry untouched. Exactly one finding (the '1'/1 pair), none
    //     naming '01'.
    let src = "<?php\n$a = [\n    '01' => 'a',\n    '1' => 'b',\n    1 => 'c',\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 5, "the '1'/1 pair, not '01'");
    assert!(!d[0].message.contains("'01'"), "{}", d[0].message);
}

#[test]
fn zero_padded_string_alone_against_int_one_is_silent() {
    let d = dups("<?php\n$a = ['01' => 'a', 1 => 'b'];\n");
    assert!(d.is_empty(), "{d:#?}");
}

// --- Auto-increment interplay (ADR-0049 A12) --------------------------------

#[test]
fn auto_increment_zero_collides_with_explicit_zero() {
    // php -r 'var_export(["a", 0 => "b"]);' → [0 => 'b'] — the bare first
    // element auto-assigns key 0, which the explicit `0 => 'b'` then shadows.
    let src = "<?php\n$a = [\n    'a',\n    0 => 'b',\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 4);
    assert!(d[0].message.contains('0'), "{}", d[0].message);
}

#[test]
fn an_unresolvable_key_poisons_every_later_auto_position() {
    // php -r '$x = 5; var_export([$x => "a", "b"]);' → [5 => 'a', 6 => 'b']:
    // a non-folded key genuinely shifts where a later bare element lands, so
    // the bare element's key is unprovable and the whole pair is silent —
    // even though, read naively, position 0 might look free.
    let src = "<?php\nfunction f($x) {\n    $a = [$x => 'a', 0 => 'b', 'c'];\n}\n";
    let d = dups(src);
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn version_dependent_auto_index_is_silent_with_no_known_minor() {
    // Straddles the PHP 8.3 next-int rule change (ADR-0049 A12): under
    // MaxPlusOne (8.3+) the bare element lands at -4, colliding with the
    // explicit `-4 => 'c'`; under FloorAtZero (pre-8.3) it lands at 0, no
    // collision. With no minor known, the answer would be a guess — silence.
    let src = "<?php\n$a = [-5 => 'a', 'b', -4 => 'c'];\n";
    let d = dups(src);
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn version_dependent_auto_index_fires_once_the_minor_is_known_post_8_3() {
    // php -r 'var_export([-5 => "a", "b", -4 => "c"]);' on PHP 8.5.9 →
    // [-5 => 'a', -4 => 'c'] — verified MaxPlusOne behavior on this sandbox.
    let src = "<?php\n$a = [-5 => 'a', 'b', -4 => 'c'];\n";
    let d = dups_with_minor(src, (8, 5));
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn version_dependent_auto_index_is_silent_on_a_pre_8_3_minor() {
    // Under FloorAtZero the bare element lands at 0, not -4, so there is no
    // collision with the explicit `-4 => 'c'` at all.
    let src = "<?php\n$a = [-5 => 'a', 'b', -4 => 'c'];\n";
    let d = dups_with_minor(src, (8, 1));
    assert!(d.is_empty(), "{d:#?}");
}

// --- Multiple shadowing, nesting, and legacy `array()` syntax ---------------

#[test]
fn a_key_reused_three_times_yields_two_findings_positioned_at_each_winner() {
    let src = "<?php\n$a = [\n    1 => 'a',\n    1 => 'b',\n    1 => 'c',\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 2, "{d:#?}");
    assert_eq!(d[0].line, 4, "'b' shadows 'a'");
    assert!(d[0].message.contains("line 3"), "{}", d[0].message);
    assert_eq!(d[1].line, 5, "'c' shadows 'b', not 'a'");
    assert!(d[1].message.contains("line 4"), "{}", d[1].message);
}

#[test]
fn fires_inside_a_nested_array() {
    let src = "<?php\n$a = [\n    'outer' => [\n        1 => 'x',\n        1 => 'y',\n    ],\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 5);
}

#[test]
fn fires_on_legacy_array_syntax() {
    let d = dups("<?php\n$a = array(\n    1 => 'a',\n    1 => 'b',\n);\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

// --- Mechanics semantics: works regardless of whether the array is used ----

#[test]
fn fires_even_in_a_proven_dead_region() {
    // Purely syntactic — no dead-region gate, unlike the proof-layer passes
    // (which skip a call/read the propagation pass proves unreachable). The
    // statement after an unconditional `return` is exactly such a region, and
    // the literal's own text is unaffected by whether it ever executes.
    let src = "<?php\nfunction f() {\n    return;\n    $a = [1 => 'a', 1 => 'b'];\n}\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
}
