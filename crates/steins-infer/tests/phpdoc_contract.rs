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

// 1. Scalar / refinement contract strictness (no coercion).

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

/// The casing pair (`phpdoc_advanced_fallback_{,non_empty_}{lower,upper}case_string`):
/// refinement is `strtolower($s) === $s`, so an uncased literal satisfies both,
/// and `non-empty-` fails independently of the casing half.
#[test]
fn casing_predicates_on_proven_string_literals() {
    let lc = "<?php /** @param lowercase-string $s */ function f($s): void {}\n";
    assert_eq!(param_count(&format!("{lc}f('abc');")), 0);
    assert_eq!(param_count(&format!("{lc}f('123');")), 0, "nothing to lowercase");
    assert_eq!(param_count(&format!("{lc}f('');")), 0, "'' is a lowercase-string");
    assert_eq!(param_count(&format!("{lc}f('ABC');")), 1, "'ABC' is not lowercase");
    assert_eq!(param_count(&format!("{lc}f('abC');")), 1, "one character decides it");

    let uc = "<?php /** @param uppercase-string $s */ function f($s): void {}\n";
    assert_eq!(param_count(&format!("{uc}f('ABC');")), 0);
    assert_eq!(param_count(&format!("{uc}f('123');")), 0, "nothing to uppercase");
    assert_eq!(param_count(&format!("{uc}f('');")), 0, "'' is an uppercase-string");
    assert_eq!(param_count(&format!("{uc}f('abc');")), 1, "'abc' is not uppercase");
    assert_eq!(param_count(&format!("{uc}f('ABc');")), 1, "one character decides it");

    let nelc = "<?php /** @param non-empty-lowercase-string $s */ function f($s): void {}\n";
    assert_eq!(param_count(&format!("{nelc}f('abc');")), 0);
    assert_eq!(param_count(&format!("{nelc}f('123');")), 0);
    assert_eq!(param_count(&format!("{nelc}f('');")), 1, "lowercase, but empty");
    assert_eq!(param_count(&format!("{nelc}f('ABC');")), 1, "non-empty, but not lowercase");

    let neuc = "<?php /** @param non-empty-uppercase-string $s */ function f($s): void {}\n";
    assert_eq!(param_count(&format!("{neuc}f('ABC');")), 0);
    assert_eq!(param_count(&format!("{neuc}f('123');")), 0);
    assert_eq!(param_count(&format!("{neuc}f('');")), 1, "uppercase, but empty");
    assert_eq!(param_count(&format!("{neuc}f('abc');")), 1, "non-empty, but not uppercase");

    // The other direction of the fixtures: a casing-refined *value* satisfies a
    // native `string` parameter.
    let flow = "<?php /** @return lowercase-string */ function g() { return 'abc'; }\n\
                function h(string $s): void {}\n";
    assert_eq!(param_count(&format!("{flow}h(g());")), 0, "lowercase-string is a string");
}

// 2. list<T> / array<K,V> / non-empty per phpstan#14939.

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

// 3. Shapes per #14939: order-agnostic array{} vs positional list{}.

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

/// ADR-0062 §5 — acceptance-convergence, proven-value side: the unsealed tail
/// carries a KEY contract too, and the proven path now judges both via
/// `steins-contract`'s one shape relation (previously the int key `9` wrongly
/// passed `...<string, int>` here while the fact path rejected it).
#[test]
fn unsealed_tail_key_contract_is_checked() {
    let f = "<?php /** @param array{a: int, ...<string, int>} $s */ function f(array $s): void {}\n";
    assert_eq!(
        param_count(&format!("{f}f(['a' => 1, 9 => 2]);")),
        1,
        "int key 9 violates the <string, …> tail key contract"
    );
    assert_eq!(
        param_count(&format!("{f}f(['a' => 1, 'b' => 2]);")),
        0,
        "string tail key with an int value satisfies ...<string, int>"
    );
    assert_eq!(
        param_count(&format!("{f}f(['a' => 1, 'b' => 'x']);")),
        1,
        "the tail VALUE contract is still checked"
    );

    // An int-keyed tail contract admits the same value.
    let g = "<?php /** @param array{a: int, ...<int, int>} $s */ function g(array $s): void {}\n";
    assert_eq!(
        param_count(&format!("{g}g(['a' => 1, 9 => 2]);")),
        0,
        "int key 9 satisfies the <int, …> tail key contract"
    );

    // An untyped unsealed tail admits any extra key/value.
    let h = "<?php /** @param array{a: int, ...} $s */ function h(array $s): void {}\n";
    assert_eq!(
        param_count(&format!("{h}h(['a' => 1, 9 => 2]);")),
        0,
        "untyped `...` tail admits anything"
    );
}

// 4. Class-name envelopes — only New-exact facts checked.

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
    // An unrelated New fact stays silent too — interfaces etc. are untracked, so
    // we never manufacture a class violation without proof of non-membership.
    let g = "<?php class Bar {}\n/** @param Foo $a */ function g($a): void {}\n";
    assert_eq!(param_count(&format!("{g}g(new Bar());")), 0, "unresolved/unrelated → silent");
}

// 5. Native + phpdoc interplay: no double-report.

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

// 6. Value propagation through env, and return checks.

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

// 7. Registry / suppressibility.

// 8. Effective-nullability and phpstan-tag precedence (FP avoidance, ADR-0029).

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

// N. Assertion-helper exemption (ADR-0030): a `@…-assert` on parameter `$x` makes
//    `@param $x` a POST-condition, so arguments aren't checked against it — but
//    sibling params, `@return`, and the native gate stay checked.

#[test]
fn assert_target_param_is_exempt() {
    // `@param int $x` would normally reject "5", but the assert marks $x a
    // post-condition target — no phpdoc.param-mismatch.
    let src = "<?php\n\
        /**\n * @param int $x\n * @phpstan-assert int $x\n */\n\
        function assertInt($x): void {}\n\
        assertInt(\"5\");";
    assert_eq!(param_count(src), 0, "assert-target param must be exempt");
    // Control: identical minus the assert tag → the @param fires.
    let control = "<?php /** @param int $x */ function assertInt($x): void {}\n\
        assertInt(\"5\");";
    assert_eq!(param_count(control), 1, "without the assert, @param int rejects \"5\"");
}

#[test]
fn if_true_if_false_and_negated_all_exempt() {
    for tag in ["@phpstan-assert-if-true int $x", "@phpstan-assert-if-false int $x", "@phpstan-assert !string $x"] {
        let src = format!(
            "<?php\n/**\n * @param int $x\n * {tag}\n */\nfunction f($x) {{ return true; }}\nf(\"5\");"
        );
        assert_eq!(param_count(&src), 0, "exempt under {tag}");
    }
}

#[test]
fn sibling_param_still_checked() {
    // Assert targets $x only; the sibling $y with @param int still rejects "5".
    let src = "<?php\n\
        /**\n * @param int $x\n * @param int $y\n * @phpstan-assert int $x\n */\n\
        function f($x, $y): void {}\n\
        f(\"5\", \"5\");";
    let ds = param_findings(src);
    assert_eq!(ds.len(), 1, "only the sibling $y should be reported, got: {ds:?}");
}

#[test]
fn return_still_checked_under_assert() {
    // The assert exempts the param, but the `@return` relation is untouched: a
    // proven `return []` still violates `@return non-empty-list<int>`. (Native
    // return is `: array`, so the phpdoc return check — not the native one — fires.)
    let src = "<?php\n\
        /**\n * @param int $x\n * @phpstan-assert int $x\n * @return non-empty-list<int>\n */\n\
        function f($x): array { return []; }\n\
        f(\"5\");";
    assert_eq!(param_count(src), 0, "param exempt");
    assert_eq!(return_count(src), 1, "@return contract still checked under the assert");
}

#[test]
fn native_hint_still_fires_under_assert() {
    // A native `int` hint is a real runtime gate; the assert does not silence it.
    // Under strict_types, passing "5" to `int` is a runtime TypeError, reported as
    // `type.argument-mismatch` (not phpdoc.*), regardless of the assert tag.
    let src = "<?php declare(strict_types=1);\n\
        /**\n * @param int $x\n * @phpstan-assert int $x\n */\n\
        function f(int $x): void {}\n\
        f(\"5\");";
    let native = findings(src)
        .into_iter()
        .filter(|d| d.id == steins_infer::ID)
        .count();
    assert_eq!(native, 1, "native int hint still gates regardless of the assert");
    assert_eq!(param_count(src), 0, "no phpdoc.param-mismatch (native fired / param exempt)");
}

#[test]
fn property_assert_target_does_not_exempt() {
    // `@phpstan-assert int $this->x` is a property target: no exemption effect, so
    // the sibling `@param int $x` still rejects a bad argument.
    let src = "<?php\n\
        /**\n * @param int $x\n * @phpstan-assert int $this->x\n */\n\
        function f($x): void {}\n\
        f(\"5\");";
    assert_eq!(param_count(src), 1, "property assert target must NOT exempt the param");
}

// 8. Named arguments bind in the contract lane (Gap A): `f(n: <expr>)` binds by
//    name (case-sensitive) and is judged like a positional argument — previously
//    the positional-only guards skipped named/mixed calls entirely (fired NOTHING).

#[test]
fn named_arg_wrong_literal_fires_on_plain_function() {
    let f = "<?php /** @param positive-int $n */ function f($n): void {}\n";
    assert_eq!(param_count(&format!("{f}f(n: 0);")), 1, "named 0 violates positive-int");
    assert_eq!(param_count(&format!("{f}f(n: -5);")), 1, "named -5 violates positive-int");
    assert_eq!(param_count(&format!("{f}f(n: 5);")), 0, "named 5 satisfies positive-int");
}

#[test]
fn named_arg_wrong_literal_fires_on_constructor() {
    // The headline reproduction: `new Foo(n: 0)` used to fire nothing.
    let c = "<?php class Foo { /** @param positive-int $n */ \
        public function __construct(public int $n) {} }\n";
    assert_eq!(param_count(&format!("{c}new Foo(n: 0);")), 1, "named 0 violates positive-int");
    assert_eq!(param_count(&format!("{c}new Foo(n: 5);")), 0, "named 5 satisfies positive-int");
    // Positional still fires (regression guard for the reordering).
    assert_eq!(param_count(&format!("{c}new Foo(-5);")), 1, "positional -5 still fires");
}

#[test]
fn named_arg_wrong_literal_fires_on_method() {
    let c = "<?php class C { /** @param positive-int $n */ public function m($n): void {} }\n";
    let call = "$c = new C(); $c->m(n: 0);";
    assert_eq!(param_count(&format!("{c}{call}")), 1, "named 0 violates positive-int on method");
    let ok = "$c = new C(); $c->m(n: 5);";
    assert_eq!(param_count(&format!("{c}{ok}")), 0, "named 5 satisfies positive-int on method");
}

#[test]
fn mixed_positional_and_named() {
    let f = "<?php /** @param int $a\n * @param positive-int $b */ function f($a, $b): void {}\n";
    // Positional `a` ok, named `b` violates.
    assert_eq!(param_count(&format!("{f}f(1, b: 0);")), 1, "named b=0 violates positive-int");
    // Positional `a` violates (contract int, float given), named `b` ok.
    assert_eq!(param_count(&format!("{f}f(1.5, b: 3);")), 1, "positional a=1.5 violates int");
    // Both ok.
    assert_eq!(param_count(&format!("{f}f(1, b: 3);")), 0, "both satisfy");
    // Both violate → two findings.
    assert_eq!(param_count(&format!("{f}f(1.5, b: 0);")), 2, "both violate");
}

#[test]
fn named_only_call_zero_positional() {
    let f = "<?php /** @param int $a\n * @param positive-int $b */ function f($a, $b): void {}\n";
    // Named-only, out of source order — binding is by name, so `b` still checks.
    assert_eq!(param_count(&format!("{f}f(b: 0, a: 1);")), 1, "named b=0 violates regardless of order");
    assert_eq!(param_count(&format!("{f}f(b: 3, a: 1);")), 0, "named-only, both satisfy");
}

#[test]
fn named_arg_to_variadic_stays_silent() {
    // A named argument collected by a variadic parameter is a keyed element, not a
    // scalar contract — the collector semantics keep it silent.
    let f = "<?php /** @param positive-int ...$rest */ function f(int ...$rest): void {}\n";
    assert_eq!(param_count(&format!("{f}f(rest: 0);")), 0, "named into variadic collector is silent");
}

#[test]
fn named_arg_case_sensitive_binding() {
    // PHP named-argument names are case-SENSITIVE: `N:` does not bind to `$n`, so the
    // contract lane binds nothing (the arity lane owns the resulting Error). No FP.
    let f = "<?php /** @param positive-int $n */ function f($n): void {}\n";
    assert_eq!(param_count(&format!("{f}f(N: 0);")), 0, "wrong-case name binds nothing → silent");
    assert_eq!(param_count(&format!("{f}f(n: 0);")), 1, "exact-case name binds and fires");
}

#[test]
fn named_arg_native_nullable_accepts_null() {
    // The nullable-default acceptance rule holds for named binding too (no FP).
    let f = "<?php /** @param int $n */ function f($n = null): void {}\n";
    assert_eq!(param_count(&format!("{f}f(n: null);")), 0, "null accepted via nullable default");
}

// Conformance slice C1 — the one identifier table. Each test below mirrors one
// `php-typing-conformance` case, assertions read off that fixture's `E?:` probe
// lines and its silent (accepting) sites. These spellings were already known to
// `steins-contract::lower_identifier` (the abstract-fact lane's table) but were
// silent on the proven-value lane, which kept a hand-maintained sibling match —
// the two lanes now share one table.

/// `phpdoc_advanced_param_typehint_boolean_synonym`: `boolean` is `bool`, and is
/// still *enforced* as one.
#[test]
fn boolean_synonym_is_enforced_as_bool() {
    let f = "<?php /** @param boolean $flag */ function f($flag): void {}\n";
    assert_eq!(param_count(&format!("{f}f(true);")), 0, "a native bool satisfies @param boolean");
    assert_eq!(param_count(&format!("{f}f(false);")), 0, "false satisfies @param boolean");
    assert_eq!(
        param_count(&format!("{f}f('not a bool');")),
        1,
        "a string is rejected where @param boolean is required"
    );
    // The `@return boolean` half of the fixture must stay silent on a real bool.
    let g = "<?php /** @return boolean */ function g() { return true; }\n";
    assert_eq!(return_count(g), 0, "true satisfies @return boolean");
}

/// `phpdoc_advanced_param_typehint_integer_synonym`.
#[test]
fn integer_synonym_is_enforced_as_int() {
    let f = "<?php /** @param integer $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(1);")), 0, "an int satisfies @param integer");
    assert_eq!(
        param_count(&format!("{f}f('not an int');")),
        1,
        "a string is rejected where @param integer is required"
    );
    let g = "<?php /** @return integer */ function g() { return 1; }\n";
    assert_eq!(return_count(g), 0, "1 satisfies @return integer");
}

/// `phpdoc_advanced_param_typehint_double_synonym`.
#[test]
fn double_synonym_is_enforced_as_float() {
    let f = "<?php /** @param double $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(1.5);")), 0, "a float satisfies @param double");
    assert_eq!(
        param_count(&format!("{f}f(1);")),
        0,
        "an int satisfies float/double (PHPStan core semantics)"
    );
    assert_eq!(
        param_count(&format!("{f}f('not a float');")),
        1,
        "a string is rejected where @param double is required"
    );
    let g = "<?php /** @return double */ function g() { return 1.5; }\n";
    assert_eq!(return_count(g), 0, "1.5 satisfies @return double");
}

/// `phpdoc_advanced_fallback_non_positive_int`: `int<min, 0>` — zero is what the
/// spelling exists for, `1` is one past the boundary.
#[test]
fn non_positive_int_is_enforced() {
    let f = "<?php /** @param non-positive-int $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(0);")), 0, "zero is inside non-positive-int");
    assert_eq!(param_count(&format!("{f}f(-1);")), 0, "-1 is inside non-positive-int");
    assert_eq!(param_count(&format!("{f}f(1);")), 1, "1 is not a non-positive-int");
}

/// `phpdoc_advanced_fallback_non_zero_int`: `int<min, -1>|int<1, max>` — the union
/// must keep the hole at zero rather than flatten to one range.
#[test]
fn non_zero_int_keeps_the_hole_at_zero() {
    let f = "<?php /** @param non-zero-int $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(1);")), 0, "1 is on one side of the hole");
    assert_eq!(param_count(&format!("{f}f(-1);")), 0, "-1 is on the other side");
    assert_eq!(param_count(&format!("{f}f(0);")), 1, "0 is the hole non-zero-int excludes");
}

/// `phpdoc_advanced_fallback_numeric`: `int|float|numeric-string`.
#[test]
fn numeric_is_enforced() {
    let f = "<?php /** @param numeric $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(1);")), 0, "int is numeric");
    assert_eq!(param_count(&format!("{f}f(1.5);")), 0, "float is numeric");
    assert_eq!(param_count(&format!("{f}f('123');")), 0, "a numeric string is numeric");
    assert_eq!(param_count(&format!("{f}f('1.5e3');")), 0, "exponent form is numeric");
    assert_eq!(param_count(&format!("{f}f('abc');")), 1, "'abc' is not numeric");
    assert_eq!(param_count(&format!("{f}f(true);")), 1, "true is not numeric");
}

/// `phpdoc_advanced_fallback_number`: `int|float` and nothing else — the whole
/// distinction from `numeric` is that a numeric string is *not* a `number`.
#[test]
fn number_excludes_numeric_strings() {
    let f = "<?php /** @param number $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(1);")), 0, "int is a number");
    assert_eq!(param_count(&format!("{f}f(1.5);")), 0, "float is a number");
    assert_eq!(param_count(&format!("{f}f('1');")), 1, "a numeric string is not a number");
    assert_eq!(param_count(&format!("{f}f(true);")), 1, "true is not a number");
    let g = "<?php /** @return number */ function g() { return 1.5; }\n";
    assert_eq!(return_count(g), 0, "1.5 satisfies @return number");
}

/// `phpdoc_advanced_int_range_keyword`: Phan's `int-range<0, 255>` is PHPStan's
/// `int<0, 255>` under a second base name — one lowering, both spellings.
#[test]
fn int_range_keyword_is_the_int_range() {
    let f = "<?php /** @param int-range<0, 255> $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(200);")), 0, "200 is inside int-range<0, 255>");
    assert_eq!(param_count(&format!("{f}f(0);")), 0, "the lower bound is inclusive");
    assert_eq!(param_count(&format!("{f}f(255);")), 0, "the upper bound is inclusive");
    assert_eq!(param_count(&format!("{f}f(256);")), 1, "256 is above the bounds");
    assert_eq!(param_count(&format!("{f}f(-1);")), 1, "-1 is below the bounds");
    // The `int<…>` spelling keeps its existing meaning, bounds grammar included.
    let g = "<?php /** @param int<min, 0> $value */ function g($value): void {}\n";
    assert_eq!(param_count(&format!("{g}g(0);")), 0, "min/max bounds still resolve");
    assert_eq!(param_count(&format!("{g}g(1);")), 1, "int<min, 0> still rejects 1");
}

/// `phpdoc_advanced_associative_array`: Phan treats `associative-array<K, V>` as
/// an array that is specifically not a list, so a plain list argument is
/// rejected even though its element types match (census bucket ix, ADR-0062's
/// `is_list` trinary).
#[test]
fn associative_array_rejects_a_list_argument() {
    let f = "<?php /** @param associative-array<int, string> $map */ \
             function f($map): void {}\n";
    assert_eq!(
        param_count(&format!("{f}f([5 => 'a', 9 => 'b']);")),
        0,
        "a non-sequential int-keyed array is associative everywhere"
    );
    assert_eq!(
        param_count(&format!("{f}f(['a', 'b', 'c']);")),
        1,
        "a plain list is not an associative-array"
    );
}

/// `phpdoc_advanced_phan_non_empty_associative_array`: combines the not-a-list
/// refusal with `non-empty` — both violations must be caught independently.
#[test]
fn non_empty_associative_array_rejects_empty_and_list_arguments() {
    let f = "<?php /** @param non-empty-associative-array<string, int> $map */ \
             function f($map): void {}\n";
    assert_eq!(
        param_count(&format!("{f}f(['a' => 1]);")),
        0,
        "a non-empty string-keyed array satisfies the parameter"
    );
    assert_eq!(
        param_count(&format!("{f}f([]);")),
        1,
        "an empty array is not a non-empty-associative-array"
    );
    assert_eq!(
        param_count(&format!("{f}f([1, 2, 3]);")),
        1,
        "a list violates the associative part"
    );
}

/// The convergence itself: a class name isn't keyword vocabulary, so it must
/// still ride the is-a oracle and the `is_known_class` gate rather than be judged
/// as a contract atom. An unresolved identifier (`@template`, `@phpstan-type`)
/// must stay silent — the FP the one table's `Class` catch-all would manufacture.
#[test]
fn unknown_identifier_stays_silent_after_convergence() {
    let f = "<?php /** @template T\n * @param T $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(1);")), 0, "a template param denotes anything → silent");
    let g = "<?php /** @param Undefined_Alias $value */ function g($value): void {}\n";
    assert_eq!(param_count(&format!("{g}g(1);")), 0, "an unresolved alias stays silent");
    let k = "<?php class K {}\n/** @param K $value */ function k($value): void {}\n";
    assert_eq!(param_count(&format!("{k}k(1);")), 1, "a scalar is never an instance of a known class");
}

/// `phpdoc_advanced_pseudotype_class_precedence`: a same-named class in scope
/// takes precedence over a phpdoc **pseudo-type** keyword (PHPStan's
/// `TypeNodeResolver::tryResolvePseudoTypeClassType`) — PHP doesn't reserve
/// `Integer`/`Boolean`/`Double`/`Number`, so resolving `@param Integer` to `int`
/// would miss the real violation and manufacture one against a real instance.
#[test]
fn a_same_named_class_shadows_a_pseudo_type_keyword() {
    let f = "<?php final class Integer {}\n/** @param Integer $value */ function f($value): void {}\n";
    assert_eq!(
        param_count(&format!("{f}f(new Integer());")),
        0,
        "an Integer instance satisfies the class-resolved parameter"
    );
    assert_eq!(
        param_count(&format!("{f}f(5);")),
        1,
        "a plain int is not an Integer instance"
    );
    // Without such a class in scope the keyword still means `int`.
    let g = "<?php /** @param Integer $value */ function g($value): void {}\n";
    assert_eq!(param_count(&format!("{g}g(5);")), 0, "unshadowed, `Integer` is the int keyword");
    assert_eq!(param_count(&format!("{g}g('x');")), 1, "unshadowed, a string still violates it");
}

/// The other half of the precedence rule: PHP **reserves** its native type words,
/// so no class can be named `int`/`string`/`bool`/`mixed` and the keyword can never
/// be shadowed. (The declaration below is not legal PHP; the point is that the
/// keyword wins regardless of what the class table happens to hold.)
#[test]
fn a_reserved_type_word_is_never_shadowed() {
    let f = "<?php /** @param int $value */ function f($value): void {}\n";
    assert_eq!(param_count(&format!("{f}f(5);")), 0, "int is int");
    assert_eq!(param_count(&format!("{f}f('5');")), 1, "and still rejects a numeric string");
}

// C5 — the array-key-cast pair (census bucket vii): `decimal-int-string` is the
// string PHP writes an integer back as, so it casts to `int` as an array key;
// `non-decimal-int-string` is its complement within `string`. The two fixtures
// (`phpdoc_advanced_fallback_{,non_}decimal_int_string`) probe the strings that
// separate them from `numeric-string`.

#[test]
fn decimal_int_string_rejects_the_non_canonical_numerics() {
    let f = "<?php /** @param decimal-int-string $value */ function f($value): void {}\n";
    // Canonical decimal notation, negative included.
    assert_eq!(param_count(&format!("{f}f('123');")), 0);
    assert_eq!(param_count(&format!("{f}f('-1');")), 0);
    assert_eq!(param_count(&format!("{f}f('0');")), 0, "'0' is canonical, though falsy");
    // Numeric, but not how PHP writes the integer back — the fixture's E? lines.
    assert_eq!(param_count(&format!("{f}f('007');")), 1, "leading zeros survive as a key");
    assert_eq!(param_count(&format!("{f}f('+1');")), 1, "'+1' survives as a key");
    assert_eq!(param_count(&format!("{f}f('abc');")), 1, "not an integer at all");
    // The edges the fixture does not probe but the predicate decides.
    assert_eq!(param_count(&format!("{f}f('-0');")), 1, "PHP writes zero back as '0'");
    assert_eq!(
        param_count(&format!("{f}f('9223372036854775808');")),
        1,
        "one past PHP_INT_MAX stays a string key"
    );
    assert_eq!(param_count(&format!("{f}f('9223372036854775807');")), 0, "PHP_INT_MAX casts");
    // A non-string is not in the running at all.
    assert_eq!(param_count(&format!("{f}f(123);")), 1, "an int is not a decimal-int-STRING");
}

#[test]
fn non_decimal_int_string_rejects_only_canonical_decimals() {
    let f = "<?php /** @param non-decimal-int-string $value */ function f($value): void {}\n";
    // Wider than the name suggests: anything that keeps its string identity.
    for ok in ["'00'", "'1.2'", "'foo'", "'+1'", "'18E+3'", "''", "'-0'"] {
        assert_eq!(param_count(&format!("{f}f({ok});")), 0, "{ok} keeps its key identity");
    }
    // The one thing excluded — the fixture's two E? lines.
    assert_eq!(param_count(&format!("{f}f('123');")), 1);
    assert_eq!(param_count(&format!("{f}f('-1');")), 1);
}

/// The `decimal-int-string` return value satisfies a native `string` parameter
/// (both fixtures open with this, and it is the leg that runs through the
/// *fact* lane rather than a proven value).
#[test]
fn the_decimal_pair_lowers_to_string_facts() {
    let src = "<?php\n\
        /** @return decimal-int-string */ function r() { return '123'; }\n\
        function s(string $v): void {}\n\
        s(r());\n";
    assert_eq!(findings(src).len(), 0, "a decimal-int-string is a string");
    let src2 = "<?php\n\
        /** @return non-decimal-int-string */ function r() { return '00'; }\n\
        function s(string $v): void {}\n\
        s(r());\n";
    assert_eq!(findings(src2).len(), 0, "a non-decimal-int-string is a string");
}

/// The negation ceiling, stated as a test: the predicate set is a conjunction
/// over positive literals, so an *abstract* fact carrying one bit can't be
/// refuted against the other — only proven values decide, so the ceiling costs
/// precision exactly when no value is in hand.
#[test]
fn the_complementary_pair_is_not_refutable_abstractly() {
    let abstract_src = "<?php\n\
        /** @return decimal-int-string */ function r() { return (string) \\rand(); }\n\
        /** @param non-decimal-int-string $v */ function s($v): void {}\n\
        s(r());\n";
    assert_eq!(
        param_findings(abstract_src).len(),
        0,
        "a decimal-int-string FACT is silently accepted by non-decimal-int-string \
         — sound (never a wrong verdict), imprecise, and the honest ceiling"
    );
    // The same relation with the value in hand: decided, because `admits_val`
    // asks `StrPreds::of` rather than reasoning about the bits.
    let proven_src = "<?php\n\
        /** @return decimal-int-string */ function r() { return '123'; }\n\
        /** @param non-decimal-int-string $v */ function s($v): void {}\n\
        s(r());\n";
    assert_eq!(param_findings(proven_src).len(), 1, "the proven value decides");
}

// C6 — the subtraction spellings (census bucket x): `non-null-mixed`,
// `non-empty-mixed`, `non-empty-scalar`.

#[test]
fn non_null_mixed_excludes_exactly_null() {
    let f = "<?php /** @param non-null-mixed $value */ function f($value): void {}\n";
    for ok in ["5", "0", "''", "'x'", "[]", "false", "new \\stdClass()"] {
        assert_eq!(param_count(&format!("{f}f({ok});")), 0, "{ok} is not null");
    }
    assert_eq!(param_count(&format!("{f}f(null);")), 1, "null is excluded");
}

#[test]
fn non_empty_mixed_subtracts_every_falsy_value_of_every_type() {
    let f = "<?php /** @param non-empty-mixed $value */ function f($value): void {}\n";
    for ok in ["1", "'x'", "[1]", "new \\stdClass()", "-1", "1.5", "true", "'0.0'", "'00'"] {
        assert_eq!(param_count(&format!("{f}f({ok});")), 0, "{ok} is truthy");
    }
    for falsy in ["''", "0", "[]", "null", "false", "0.0", "'0'"] {
        assert_eq!(param_count(&format!("{f}f({falsy});")), 1, "{falsy} is falsy");
    }
}

#[test]
fn non_empty_scalar_subtracts_the_falsy_member_of_each_base() {
    let f = "<?php /** @param non-empty-scalar $value */ function f($value): void {}\n";
    for ok in ["1", "1.5", "'x'", "true", "-1"] {
        assert_eq!(param_count(&format!("{f}f({ok});")), 0, "{ok} is a truthy scalar");
    }
    // The five E? lines. `0`/`0.0` are the two PHPStan stays silent on (its
    // `float` member is never narrowed and swallows both); Steins spells the
    // subtraction itself, so all five are decided.
    for falsy in ["0", "0.0", "''", "false", "'0'"] {
        assert_eq!(param_count(&format!("{f}f({falsy});")), 1, "{falsy} is falsy");
    }
    // The `scalar` half still holds: a non-scalar is out regardless of truth.
    assert_eq!(param_count(&format!("{f}f([1]);")), 1, "an array is not a scalar");
    assert_eq!(param_count(&format!("{f}f(null);")), 1, "null is not a scalar");
}

/// The cuts decide against a *fact* only where the fact's own refinement
/// carries the answer — everything else stays silent rather than guessing.
#[test]
fn the_falsy_cut_decides_abstractly_only_where_the_refinement_answers() {
    // `non-falsy-string` IS the string half of the cut → accepted, no finding.
    let ok = "<?php\n\
        /** @return non-falsy-string */ function r() { return 'x'; }\n\
        /** @param non-empty-mixed $v */ function f($v): void {}\n\
        f(r());\n";
    assert_eq!(param_findings(ok).len(), 0);
    // A plain `string` fact holds both `''` and `'x'` → silent, not refuted.
    let maybe = "<?php\n\
        /** @return string */ function r() { return 'x'; }\n\
        /** @param non-empty-mixed $v */ function f($v): void {}\n\
        f(r());\n";
    assert_eq!(param_findings(maybe).len(), 0, "a string fact is not refutable here");
}
