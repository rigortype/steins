//! Acceptance tests for the phpdoc declared-contract acceptance relation
//! (ADR-0030 relation #1): `phpdoc.param-mismatch` / `phpdoc.return-mismatch`.
//!
//! This relation is **pure set membership, no coercion** — the differentiator
//! from the runtime relation. A numeric string `"5"` does NOT satisfy `int` here.
//! Judgments are trinary; only a proven `No` is reported (`maybe` is silent).

use steins_infer::{
    DIAGNOSTIC_IDS, Diagnostic, PARAM_MISMATCH_ID, RETURN_MISMATCH_ID, check, pattern_is_known,
};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn param_findings(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).collect()
}

fn param_count(src: &str) -> usize {
    param_findings(src).len()
}

fn return_count(src: &str) -> usize {
    findings(src).into_iter().filter(|d| d.id == RETURN_MISMATCH_ID).count()
}

// ==========================================================================
// 1. Scalar / refinement contract strictness (no coercion).
// ==========================================================================

#[test]
fn numeric_string_does_not_satisfy_int_contract() {
    // The headline divergence from the runtime relation: "5" fails `int` here,
    // even though it coerces fine at runtime.
    let f = "<?php /** @param int $n */ function f($n): void {}\n";
    assert_eq!(param_count(&format!("{f}f(\"5\");")), 1, "\"5\" violates int contract");
    assert_eq!(param_count(&format!("{f}f(5);")), 0, "5 satisfies int");
    assert_eq!(param_count(&format!("{f}f(1.5);")), 1, "1.5 (float) violates int");
}

#[test]
fn int_is_accepted_by_float() {
    let f = "<?php /** @param float $n */ function f($n): void {}\n";
    assert_eq!(param_count(&format!("{f}f(5);")), 0, "int accepted by float (PHPStan core)");
    assert_eq!(param_count(&format!("{f}f(\"5\");")), 1, "numeric string still violates float");
}

#[test]
fn refinement_predicates_on_proven_scalars() {
    let pos = "<?php /** @param positive-int $n */ function f($n): void {}\n";
    assert_eq!(param_count(&format!("{pos}f(5);")), 0);
    assert_eq!(param_count(&format!("{pos}f(-5);")), 1, "-5 not positive-int");
    assert_eq!(param_count(&format!("{pos}f(0);")), 1, "0 not positive-int");

    let nes = "<?php /** @param non-empty-string $s */ function f($s): void {}\n";
    assert_eq!(param_count(&format!("{nes}f(\"x\");")), 0);
    assert_eq!(param_count(&format!("{nes}f(\"\");")), 1, "empty string violates non-empty-string");

    let num = "<?php /** @param numeric-string $s */ function f($s): void {}\n";
    assert_eq!(param_count(&format!("{num}f(\"5\");")), 0, "\"5\" is a numeric-string");
    assert_eq!(param_count(&format!("{num}f(\"abc\");")), 1, "\"abc\" not numeric-string");

    let nf = "<?php /** @param non-falsy-string $s */ function f($s): void {}\n";
    assert_eq!(param_count(&format!("{nf}f(\"0\");")), 1, "\"0\" is falsy");
    assert_eq!(param_count(&format!("{nf}f(\"1\");")), 0);
}

// ==========================================================================
// 2. list<T> / array<K,V> / non-empty per phpstan#14939.
// ==========================================================================

#[test]
fn list_membership_key_order_and_elements() {
    let f = "<?php /** @param list<int> $xs */ function f(array $xs): void {}\n";
    assert_eq!(param_count(&format!("{f}f([1, 2, 3]);")), 0, "0..n-1 ints is a list<int>");
    assert_eq!(param_count(&format!("{f}f([]);")), 0, "empty is a valid list");
    assert_eq!(param_count(&format!("{f}f(['a']);")), 1, "string element violates list<int>");
    assert_eq!(param_count(&format!("{f}f(['k' => 1]);")), 1, "string key → not a list");
    assert_eq!(param_count(&format!("{f}f([1 => 1, 0 => 2]);")), 1, "keys out of order → not a list");
}

#[test]
fn array_generic_is_key_order_agnostic() {
    let f = "<?php /** @param array<string, int> $m */ function f(array $m): void {}\n";
    assert_eq!(param_count(&format!("{f}f(['a' => 1, 'b' => 2]);")), 0);
    assert_eq!(param_count(&format!("{f}f(['a' => 'x']);")), 1, "value 'x' violates int");
    assert_eq!(param_count(&format!("{f}f([0 => 1]);")), 1, "int key violates string key type");
}

#[test]
fn non_empty_variants_reject_empty() {
    let f = "<?php /** @param non-empty-list<int> $xs */ function f(array $xs): void {}\n";
    assert_eq!(param_count(&format!("{f}f([1]);")), 0);
    assert_eq!(param_count(&format!("{f}f([]);")), 1, "empty violates non-empty-list");
}

// ==========================================================================
// 3. Shapes per #14939: order-agnostic array{} vs positional list{}.
// ==========================================================================

#[test]
fn array_shape_is_order_agnostic_and_sealed() {
    let f = "<?php /** @param array{a: int, b: string} $s */ function f(array $s): void {}\n";
    assert_eq!(param_count(&format!("{f}f(['a' => 1, 'b' => 'x']);")), 0);
    assert_eq!(param_count(&format!("{f}f(['b' => 'x', 'a' => 1]);")), 0, "order-agnostic");
    assert_eq!(param_count(&format!("{f}f(['a' => 1]);")), 1, "missing required key b");
    assert_eq!(param_count(&format!("{f}f(['a' => 1, 'b' => 'x', 'c' => 9]);")), 1, "extra key (sealed)");
    assert_eq!(param_count(&format!("{f}f(['a' => 'no', 'b' => 'x']);")), 1, "wrong element type");
}

#[test]
fn optional_shape_key_may_be_absent() {
    let f = "<?php /** @param array{a: int, b?: string} $s */ function f(array $s): void {}\n";
    assert_eq!(param_count(&format!("{f}f(['a' => 1]);")), 0, "optional b may be absent");
    assert_eq!(param_count(&format!("{f}f(['a' => 1, 'b' => 2]);")), 1, "present b must match");
}

#[test]
fn list_shape_is_positional() {
    let f = "<?php /** @param list{int, string} $s */ function f(array $s): void {}\n";
    assert_eq!(param_count(&format!("{f}f([1, 'x']);")), 0);
    assert_eq!(param_count(&format!("{f}f(['x', 1]);")), 1, "positional type mismatch");
}

// ==========================================================================
// 4. Class-name envelopes — only New-exact facts checked.
// ==========================================================================

#[test]
fn class_name_matches_exact_and_subclass() {
    let base = "<?php class Animal {} class Dog extends Animal {}\n\
        /** @param Animal $a */ function f($a): void {}\n";
    assert_eq!(param_count(&format!("{base}f(new Animal());")), 0, "exact class match");
    assert_eq!(param_count(&format!("{base}f(new Dog());")), 0, "subclass acceptable");
}

#[test]
fn class_name_unresolved_or_non_object_is_silent() {
    // A scalar into a class-name type is silent (only New-exact facts are checked).
    let f = "<?php /** @param Foo $a */ function f($a): void {}\n";
    assert_eq!(param_count(&format!("{f}f(5);")), 0, "scalar vs class name → silent");
    // An unrelated New fact stays silent too (no proof of non-membership; interfaces
    // etc. are untracked, so we never manufacture a class violation).
    let g = "<?php class Bar {}\n/** @param Foo $a */ function g($a): void {}\n";
    assert_eq!(param_count(&format!("{g}g(new Bar());")), 0, "unresolved/unrelated → silent");
}

// ==========================================================================
// 5. Native + phpdoc interplay: no double-report.
// ==========================================================================

#[test]
fn native_and_phpdoc_do_not_double_report() {
    // Native `int` + phpdoc `positive-int`: "abc" fires the NATIVE check only
    // (proven runtime TypeError); the phpdoc check is skipped at that site.
    let src = "<?php declare(strict_types=1);\n\
        /** @param positive-int $n */ function f(int $n): void {}\n\
        f(\"abc\");";
    let all = findings(src);
    assert_eq!(all.len(), 1, "exactly one finding, not two");
    assert_eq!(all[0].id, "type.argument-mismatch", "the native runtime finding wins");
}

#[test]
fn phpdoc_fires_where_native_is_silent() {
    // -5 satisfies native `int` (no runtime error) but violates phpdoc positive-int.
    let src = "<?php /** @param positive-int $n */ function f(int $n): void {}\nf(-5);";
    let all = findings(src);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, PARAM_MISMATCH_ID);
}

// ==========================================================================
// 6. Value propagation through env, and return checks.
// ==========================================================================

#[test]
fn array_flows_through_a_variable() {
    let src = "<?php /** @param list<int> $xs */ function f(array $xs): void {}\n\
        $a = ['x'];\nf($a);";
    assert_eq!(param_count(src), 1, "array value propagates via env into the contract check");
}

#[test]
fn return_contract_is_checked() {
    let src = "<?php /** @return non-empty-list<int> */ function h(): array { return []; }";
    assert_eq!(return_count(src), 1);
    let ok = "<?php /** @return list<int> */ function h(): array { return [1, 2]; }";
    assert_eq!(return_count(ok), 0);
}

// ==========================================================================
// 7. Registry / suppressibility.
// ==========================================================================

// ==========================================================================
// 8. Effective-nullability and phpstan-tag precedence (FP avoidance, ADR-0029).
// ==========================================================================

#[test]
fn null_accepted_by_effectively_nullable_param() {
    // Explicit `?string` native: `@param string` should still accept null.
    let a = "<?php /** @param string $s */ function f(?string $s): void {}\nf(null);";
    assert_eq!(param_count(a), 0, "?string native widens the @param string contract");
    // Implicit-nullable via `= null` default (untyped): PHP/PHPStan accept null.
    let b = "<?php /** @param string $s */ function f($s = null): void {}\nf(null);";
    assert_eq!(param_count(b), 0, "= null default makes the param implicitly nullable");
    // A genuinely non-nullable string param still flags null.
    let c = "<?php /** @param string $s */ function f($s): void {}\nf(null);";
    assert_eq!(param_count(c), 1, "non-nullable string still rejects null");
}

#[test]
fn phpstan_param_overrides_plain_param() {
    // `@phpstan-param` wins: a template `T` → class-name → silent for an array,
    // suppressing the plain `@param string[]` finding (PHPStan parity).
    let src = "<?php\n\
        /**\n * @param string[] $c\n * @phpstan-param T $c\n */\n\
        function f(array $c): void {}\n\
        f([1, 2]);";
    assert_eq!(param_count(src), 0, "@phpstan-param T overrides @param string[]");
    // Without the override, the plain @param string[] fires.
    let plain = "<?php /** @param string[] $c */ function f(array $c): void {}\nf([1, 2]);";
    assert_eq!(param_count(plain), 1);
}

#[test]
fn both_ids_are_registered_and_suppressible() {
    assert!(DIAGNOSTIC_IDS.contains(&PARAM_MISMATCH_ID));
    assert!(DIAGNOSTIC_IDS.contains(&RETURN_MISMATCH_ID));
    assert!(pattern_is_known(PARAM_MISMATCH_ID));
    assert!(pattern_is_known(RETURN_MISMATCH_ID));
    assert!(pattern_is_known("phpdoc"));
    assert!(pattern_is_known("phpdoc.*"));
}

#[test]
fn inline_ignore_suppresses_param_mismatch() {
    use steins_infer::apply_inline_ignores;
    let src = "<?php /** @param int $n */ function f($n): void {}\n\
        f(\"5\"); // @steins-ignore phpdoc.param-mismatch\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let raw = check(&tree, &functions, "test.php");
    assert_eq!(raw.iter().filter(|d| d.id == PARAM_MISMATCH_ID).count(), 1);
    let outcome = apply_inline_ignores(raw, &[("test.php".to_owned(), &tree)]);
    assert_eq!(outcome.kept.iter().filter(|d| d.id == PARAM_MISMATCH_ID).count(), 0);
    assert_eq!(outcome.suppressed, 1);
}
