//! `array.duplicate-key` (ADR-0078, issue #187, mechanics layer).
//!
//! A literal array expression that declares the same PHP-normalized key twice
//! silently drops the earlier value. Purely syntactic, so tests use plain
//! [`check`] except for the ADR-0049 A12 auto-increment edge case, which
//! pins a PHP minor via [`check_with`] and a minor-reporting [`Folder`].
//!
//! Every coerced-equal pair and auto-increment claim below is
//! `php -r`-witnessed on PHP 8.5.9 (the sandbox's `php`).

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

// Firing fixtures

#[test]
fn fires_on_a_plain_duplicate_int_key() {
    // Result: [1 => 'b'] (one element).
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

// PHP key-coercion pairs (ADR-0062's A12 coercion, reused verbatim)

#[test]
fn fires_on_int_vs_int_like_string() {
    // Result: [1 => 'b'].
    let d = dups("<?php\n$a = [\n    1 => 'a',\n    '1' => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_bool_true_vs_int_one() {
    // Result: [1 => 'b'].
    let d = dups("<?php\n$a = [\n    true => 'a',\n    1 => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_null_vs_empty_string() {
    // Result: ['' => 'b'] (with a "null as array offset" deprecation notice).
    let d = dups("<?php\n$a = [\n    null => 'a',\n    '' => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_truncated_float_vs_int() {
    // Result: [1 => 'b'] (with an "implicit conversion from float 1.7"
    // deprecation notice; truncation toward zero lands on the same key).
    let d = dups("<?php\n$a = [\n    1.7 => 'a',\n    1 => 'b',\n];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

// Non-folded keys: silence, never a guess

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

// '01' is a distinct string key, not int 1

#[test]
fn zero_padded_string_stays_distinct_from_int_one() {
    // Result: ['01' => 'a', 1 => 'c'] — '1'/1 collapse, '01' stays distinct.
    // Exactly one finding (the '1'/1 pair), none naming '01'.
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

// Byte-string keys (issue #187's false positive; ADR-0080 fixed it: literal
// string keys are byte strings, so distinct invalid-UTF-8 keys stay distinct
// instead of collapsing through `String::from_utf8_lossy`).

#[test]
fn the_symfony_console_shape_is_silent() {
    // corpus/symfony__console/Helper/QuestionHelper.php:356 (the measured FP):
    // four distinct single-byte keys indexed by `$c & "\xF0"` — silent because
    // they genuinely differ, a stronger claim than the pre-ADR-0080 silence.
    let src = "<?php\n$a = [\"\\xC0\" => 1, \"\\xD0\" => 1, \"\\xE0\" => 2, \"\\xF0\" => 3];\n";
    let d = dups(src);
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn a_byte_string_key_does_not_hide_a_genuine_duplicate_beside_it() {
    // A byte-string key participates like any other key in the same literal.
    let src = "<?php\n$a = [\n    \"\\xC0\" => 1,\n    2 => 'a',\n    2 => 'b',\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains('2'), "{}", d[0].message);
    assert_eq!(d[0].line, 5, "the genuine 2/2 pair, not the lossy key");
}

#[test]
fn a_byte_string_key_does_not_poison_auto_increment() {
    // Result: ["\xC0" => 'x', 0 => 'b'] — invalid-UTF-8 bytes never form a
    // canonical integer string, so the key never bumps the auto-index counter;
    // unlike an unresolvable (`None`) key, it does not poison later `Auto`
    // positions.
    let src = "<?php\n$a = [\n    \"\\xC0\" => 'x',\n    'a',\n    0 => 'b',\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 5);
    assert!(d[0].message.contains('0'), "{}", d[0].message);
}

// Auto-increment interplay (ADR-0049 A12)

#[test]
fn auto_increment_zero_collides_with_explicit_zero() {
    // Result: [0 => 'b'] — the bare first element auto-assigns key 0, which
    // the explicit `0 => 'b'` then shadows.
    let src = "<?php\n$a = [\n    'a',\n    0 => 'b',\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 4);
    assert!(d[0].message.contains('0'), "{}", d[0].message);
}

#[test]
fn an_unresolvable_key_poisons_every_later_auto_position() {
    // php -r '$x=5; var_export([$x=>"a","b"]);' → [5=>'a', 6=>'b'] — a
    // non-folded key shifts where a later bare element lands, so the bare
    // element's key is unprovable and the whole pair stays silent (even
    // though position 0 might look free, read naively).
    let src = "<?php\nfunction f($x) {\n    $a = [$x => 'a', 0 => 'b', 'c'];\n}\n";
    let d = dups(src);
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn version_dependent_auto_index_is_silent_with_no_known_minor() {
    // Straddles the PHP 8.3 next-int rule change (ADR-0049 A12): MaxPlusOne
    // (8.3+) lands the bare element at -4 (colliding with `-4 => 'c'`);
    // FloorAtZero (pre-8.3) lands it at 0 (no collision). Unknown minor ⇒ silence.
    let src = "<?php\n$a = [-5 => 'a', 'b', -4 => 'c'];\n";
    let d = dups(src);
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn version_dependent_auto_index_fires_once_the_minor_is_known_post_8_3() {
    // Result on PHP 8.5.9: [-5 => 'a', -4 => 'c'] — verified MaxPlusOne.
    let src = "<?php\n$a = [-5 => 'a', 'b', -4 => 'c'];\n";
    let d = dups_with_minor(src, (8, 5));
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn version_dependent_auto_index_is_silent_on_a_pre_8_3_minor() {
    // Under FloorAtZero the bare element lands at 0, not -4 — no collision.
    let src = "<?php\n$a = [-5 => 'a', 'b', -4 => 'c'];\n";
    let d = dups_with_minor(src, (8, 1));
    assert!(d.is_empty(), "{d:#?}");
}

// Multiple shadowing, nesting, and legacy array() syntax

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

// Mechanics semantics: works regardless of whether the array is used

#[test]
fn fires_even_in_a_proven_dead_region() {
    // Purely syntactic — no dead-region gate, unlike proof-layer passes that
    // skip a call/read proven unreachable. The literal's own text is
    // unaffected by whether the statement after `return` ever executes.
    let src = "<?php\nfunction f() {\n    return;\n    $a = [1 => 'a', 1 => 'b'];\n}\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
}

/// The cost the issue #187 guard charged, repaid: a literal that repeats a
/// **genuine** `"\u{FFFD}"` was indistinguishable from the decoding artifact
/// and had to be passed over. It is an ordinary duplicate now.
#[test]
fn a_repeated_real_replacement_character_is_a_duplicate_again() {
    let src = "<?php\n$a = [\n    \"\\u{FFFD}\" => 1,\n    \"\\u{FFFD}\" => 2,\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 4);
}

/// The byte keys of the symfony shape are distinct *values*, so a literal that
/// really does repeat one of them still reports.
#[test]
fn a_repeated_byte_string_key_is_a_duplicate() {
    let src = "<?php\n$a = [\n    \"\\xC0\" => 1,\n    \"\\xD0\" => 2,\n    \"\\xC0\" => 3,\n];\n";
    let d = dups(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 5, "the repeated \\xC0, not the distinct \\xD0");
}
