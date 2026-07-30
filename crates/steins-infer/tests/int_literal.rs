//! Integer-literal magnitude and the `int` → `float` promotion (issue #62).
//!
//! PHP's lexer promotes an integer literal that does not fit `int` to `float`. Before
//! this was implemented the literal wrapped: `9223372036854775808` lowered to
//! `i64::MIN` and the analyzer reported `-9223372036854775808` — a wrong *value*,
//! which under the zero-FP charter (ADR-0002) is strictly worse than `unknown`,
//! because it seeds an env fact, crosses return boundaries, and reaches the fold gate
//! as an argument the source never contained.
//!
//! The promotion is base-blind, so the fixtures walk every spelling PHP accepts, and
//! `oracle_agrees_on_every_spelling` checks each against the real engine rather than
//! against an assumption about it.

use std::process::Command;

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// The `debug.type` body for `$x = <expr>;`.
fn dumped(expr: &str) -> String {
    let src = format!("<?php\n$x = {expr};\n\\PHPStan\\dumpType($x);\n");
    let tree = SourceTree::parse(&src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one dump for `{expr}`, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

// ==========================================================================
// (i) The defect itself.
// ==========================================================================

#[test]
fn a_literal_above_int_max_promotes_to_float() {
    // Was `-9223372036854775808` (the silent `u64 as i64` wrap).
    assert_eq!(dumped("9223372036854775808"), "9223372036854775808.0");
}

#[test]
fn the_unary_minus_spelling_promotes_too() {
    // `-9223372036854775808` is unary minus over an ALREADY-overflowed literal, so it
    // is a float in PHP as well. It previously reached the same wrong `i64::MIN` by a
    // second route: `wrapping_neg` is a no-op on `i64::MIN`.
    assert_eq!(dumped("-9223372036854775808"), "-9223372036854775808.0");
    // `PHP_INT_MIN` has no integer-literal spelling at all — the nearest int literal
    // is `-(PHP_INT_MAX)`, which stays an int.
    assert_eq!(dumped("-9223372036854775807"), "-9223372036854775807");
}

#[test]
fn the_boundary_is_checked_on_both_sides() {
    assert_eq!(dumped("9223372036854775806"), "9223372036854775806");
    assert_eq!(dumped("9223372036854775807"), "9223372036854775807");
    assert_eq!(dumped("9223372036854775808"), "9223372036854775808.0");
}

// ==========================================================================
// (ii) Every base follows the same rule.
// ==========================================================================

#[test]
fn ordinary_literals_are_unaffected_in_every_base() {
    assert_eq!(dumped("0"), "0");
    assert_eq!(dumped("5"), "5");
    assert_eq!(dumped("0777"), "511"); // legacy octal
    assert_eq!(dumped("0o17"), "15"); // PHP 8.1 octal
    assert_eq!(dumped("0b101"), "5");
    assert_eq!(dumped("0x1A"), "26");
    assert_eq!(dumped("1_000"), "1000");
}

#[test]
fn overflow_promotes_in_every_base() {
    assert_eq!(dumped("0x8000000000000000"), "9223372036854775808.0");
    assert_eq!(dumped("0b1000000000000000000000000000000000000000000000000000000000000000"), "9223372036854775808.0");
    assert_eq!(dumped("01000000000000000000000"), "9223372036854775808.0");
    assert_eq!(dumped("0o1000000000000000000000"), "9223372036854775808.0");
    assert_eq!(dumped("9_223_372_036_854_775_808"), "9223372036854775808.0");
}

#[test]
fn beyond_u64_decimal_still_converts_other_bases_decline() {
    // A decimal digit string rounds to the nearest double identically in Rust and
    // PHP, so this is exact, not approximate.
    assert_eq!(dumped("99999999999999999999"), "100000000000000000000.0");
    // The documented ceiling: a >64-bit hex literal would need big-integer
    // arithmetic to convert, so it declines rather than guessing. Silence, not a
    // wrong value — and NOT the parser's saturated `u64::MAX`, which is the trap
    // this whole fix exists to avoid.
    assert_eq!(dumped("0x10000000000000000"), "unknown");
}

// ==========================================================================
// (iii) The array-key path the promotion newly routes through.
// ==========================================================================

#[test]
fn an_out_of_range_float_key_is_not_folded() {
    // PHP warns ("The float … is not representable as an int, cast occurred") and
    // takes the C wraparound; Rust's `as` saturates instead, so folding here would
    // produce a key PHP never makes. The key is left unproven.
    let src = "<?php\n$a = [9223372036854775808 => 'x'];\n\\PHPStan\\dumpType($a);\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].message.replace("dumped type: ", ""), "unknown");
    // An in-range float key still truncates toward zero, unchanged: `1.9` is the
    // EXPLICIT key 1, not an auto key (which would spell `array{'y'}`).
    assert_eq!(dumped("[1.9 => 'y']"), "array{1: 'y'}");
}

// ==========================================================================
// (iv) The oracle.
// ==========================================================================

/// Every spelling checked against the engine. Kept as PHP source so the literal the
/// engine sees is byte-for-byte the literal lowering sees.
const SPELLINGS: &[&str] = &[
    "0",
    "5",
    "-5",
    "0777",
    "0o17",
    "0b101",
    "0x1A",
    "1_000",
    "9223372036854775806",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775807",
    "-9223372036854775808",
    "0x7FFFFFFFFFFFFFFF",
    "0x8000000000000000",
    "0xFFFFFFFFFFFFFFFF",
    "01000000000000000000000",
    "9_223_372_036_854_775_808",
    "99999999999999999999",
];

#[test]
fn oracle_agrees_on_every_spelling() {
    if Command::new("php").arg("--version").output().is_err() {
        eprintln!("SKIP: php not on PATH; oracle comparison not run");
        return;
    }

    // `var_export` renders an int as digits and a float with a `.0` when integral,
    // which is the same shape the dump surface spells.
    let script = SPELLINGS
        .iter()
        .map(|s| format!("var_export({s}); echo \"\\n\";"))
        .collect::<Vec<_>>()
        .join("");
    let out = Command::new("php")
        .args(["-d", "serialize_precision=-1", "-d", "display_errors=stderr", "-r", &script])
        .output()
        .expect("run php");
    assert!(out.status.success(), "php failed: {}", String::from_utf8_lossy(&out.stderr));
    let engine: Vec<&str> = std::str::from_utf8(&out.stdout).expect("utf8").lines().collect();
    assert_eq!(engine.len(), SPELLINGS.len(), "answer count mismatch");

    for (spelling, expected) in SPELLINGS.iter().zip(engine) {
        let ours = dumped(spelling);
        assert_ne!(ours, "unknown", "spelling `{spelling}` failed to resolve");
        // Compare numerically: both sides agree on the value, and only the rendering
        // register differs (`1.0E+20` vs `100000000000000000000.0`). An int/float
        // MISMATCH still fails, because a float rendering always carries the `.0`.
        let ours_is_float = ours.contains('.');
        let engine_is_float = expected.contains('.') || expected.contains('E');
        assert_eq!(
            ours_is_float, engine_is_float,
            "spelling `{spelling}`: int/float disagreement — engine {expected:?}, we said {ours:?}"
        );
        let ours_n: f64 = ours.parse().expect("our value parses");
        let engine_n: f64 = expected.parse().expect("engine value parses");
        assert_eq!(
            ours_n, engine_n,
            "spelling `{spelling}`: engine said {expected:?}, we said {ours:?}"
        );
    }
}
