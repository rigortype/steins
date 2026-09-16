//! `@phpstan-assert !int $x` subtracts on the declared-arm lane (issue #391).
//!
//! The class-typed negated spec has taken the arm-lane road since ADR-0052 §3(d)
//! landed (`assert_tag_class_lane.rs`); the scalar one established nothing, so an
//! `int|string` lane reached a `string` parameter with its `int` arm intact. The
//! judgment is ADR-0052 §2's, unchanged: an arm dies iff the subtrahend covers it
//! with `Yes`, so an arm with an interior point the tag says nothing about keeps.
//!
//! Arm lane only. The value lane has no "this base is gone" operator, and
//! inventing one for a single tag would be a second narrowing relation.

use steins_infer::{DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

fn dumps(src: &str, id: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php")
        .into_iter()
        .filter(|d| d.id == id)
        .map(|d: Diagnostic| d.message)
        .collect()
}

fn one_type(src: &str) -> String {
    let d = dumps(src, DEBUG_TYPE_ID);
    assert_eq!(d.len(), 1, "expected one debug.type dump, got {d:?}");
    d[0].clone()
}

fn one_phpdoc_type(src: &str) -> String {
    let d = dumps(src, DEBUG_PHPDOC_TYPE_ID);
    assert_eq!(d.len(), 1, "expected one debug.phpdoc-type dump, got {d:?}");
    d[0].clone()
}

const PRELUDE: &str = "<?php
declare(strict_types=1);
/** @phpstan-assert !int $value */
function assertNotInt(int|string|bool $value): void {}
function takesString(string $value): void {}
";

#[test]
fn a_negated_scalar_spec_deletes_that_bases_arm() {
    let src = format!(
        "{PRELUDE}
function f(int|string $value): void {{
    assertNotInt($value);
    \\PHPStan\\dumpType($value);
}}
"
    );
    assert_eq!(one_type(&src), "dumped type: string");
}

#[test]
fn the_remaining_arms_are_untouched() {
    // Three arms in, one named, two out — the subtraction is per arm, not a reset.
    let src = format!(
        "{PRELUDE}
/** @param int|string|bool $x */
function f($x): void {{
    assertNotInt($x);
    \\PHPStan\\dumpPhpDocType($x);
}}
"
    );
    assert_eq!(one_phpdoc_type(&src), "dumped phpdoc type: string|bool (asserted)");
}

#[test]
fn an_arm_the_subtrahend_does_not_cover_survives() {
    // ADR-0052 §2's rule, one carrier up: an arm dies iff the subtrahend covers it
    // with `Yes`. `!bool` covers no arm of an `int|string` lane, so the lane is
    // untouched — the tag is a subtraction, never a reset.
    let src = "<?php
declare(strict_types=1);
/** @phpstan-assert !bool $value */
function assertNotBool(mixed $value): void {}
function f(int|string $value): void {
    assertNotBool($value);
    \\PHPStan\\dumpType($value);
}
";
    assert_eq!(one_type(src), "dumped type: int|string");
}

#[test]
fn the_float_subtrahend_is_refused_outright() {
    // The one base whose ACCEPTANCE relation widens across bases: a `float`
    // parameter takes an int, so `subsumes(float, int) = Yes` — but `is_float(1)`
    // is false, and reading acceptance as identity here would delete a live `int`
    // arm. `!float` therefore subtracts nothing at all.
    let src = "<?php
declare(strict_types=1);
/** @phpstan-assert !float $value */
function assertNotFloat(mixed $value): void {}
function f(int|string $value): void {
    assertNotFloat($value);
    \\PHPStan\\dumpType($value);
}
";
    assert_eq!(one_type(src), "dumped type: int|string");
}

#[test]
fn the_positive_spelling_is_unchanged() {
    // The control: `@phpstan-assert int` was already a positive narrowing on the
    // value lane and stays exactly that.
    let src = "<?php
declare(strict_types=1);
/** @phpstan-assert int $value */
function mustInt(int|string $value): void {}
function f(int|string $value): void {
    mustInt($value);
    \\PHPStan\\dumpType($value);
}
";
    assert_eq!(one_type(src), "dumped type: int (asserted)");
}

#[test]
fn a_subject_without_a_declared_lane_learns_nothing() {
    // No lane to subtract from, and the tag mints no carrier of its own — the same
    // bound the class road carries.
    let src = format!(
        "{PRELUDE}
function f($x): void {{
    assertNotInt($x);
    \\PHPStan\\dumpType($x);
}}
"
    );
    assert_eq!(one_type(&src), "dumped type: unknown");
}
