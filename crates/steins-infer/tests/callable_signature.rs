//! Acceptance tests for signature-carrying callable contracts (issue #11):
//! `callable(P1, P2=): R` phpdoc types judged against a bound closure / arrow /
//! first-class-callable argument.
//!
//! The relation is the **declared-contract** one (ADR-0030 divergence #1 —
//! envelope checking, no runtime coercion; PHP does not enforce a
//! `callable(int): string` docblock at runtime). Variance: parameters are
//! CONTRAvariant (a closure accepting wider than the contract is fine; requiring
//! narrower is the violation), the return is COvariant (returning narrower/equal
//! is fine; a provably-disjoint return is the violation). Only a definite `No`
//! reports — an undeclared native type, a template, a bare `callable`, or a
//! cross-class comparison is `Maybe` and stays silent (zero-FP).

use steins_infer::{Diagnostic, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn param_count(src: &str) -> usize {
    findings(src).into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).count()
}

// The two conformance-fixture callee shapes.
const INT_TO_STRING: &str =
    "<?php /** @param callable(int): string $cb */ function takes(callable $cb): void {}\n";

// ==========================================================================
// 1. Parameter contravariance — both directions.
// ==========================================================================

#[test]
fn param_narrower_than_contract_is_violation() {
    // The `callables_docblock_signature` fixture: closure param `string` cannot
    // accept the `int` the contract supplies. Contravariance broken.
    let src = format!("{INT_TO_STRING}takes(static fn (string $v): string => $v);");
    assert_eq!(param_count(&src), 1, "string param cannot accept supplied int");
}

#[test]
fn param_exact_match_is_ok() {
    let src = format!("{INT_TO_STRING}takes(static fn (int $v): string => (string) $v);");
    assert_eq!(param_count(&src), 0, "int param matches contract int");
}

#[test]
fn param_wider_than_contract_is_ok() {
    // Contravariance: a closure accepting WIDER than the contract supplies is
    // fine (`int|string` accepts every `int`).
    let src = format!("{INT_TO_STRING}takes(static fn (int|string $v): string => (string) $v);");
    assert_eq!(param_count(&src), 0, "int|string param accepts supplied int");
}

// ==========================================================================
// 2. Return covariance — both directions.
// ==========================================================================

#[test]
fn return_disjoint_from_contract_is_violation() {
    // The `callables_return_type_mismatch` fixture: closure returns `int` where
    // the contract promises `string`. Covariance broken.
    let src = format!("{INT_TO_STRING}takes(static fn (int $v): int => $v);");
    assert_eq!(param_count(&src), 1, "int return incompatible with string contract");
}

#[test]
fn return_exact_match_is_ok() {
    let src = format!("{INT_TO_STRING}takes(static fn (int $v): string => (string) $v);");
    assert_eq!(param_count(&src), 0, "string return matches contract string");
}

#[test]
fn return_narrower_than_contract_is_ok() {
    // Covariance: a closure returning NARROWER than the contract is fine.
    let callee =
        "<?php /** @param callable(int): (int|string) $cb */ function takes(callable $cb): void {}\n";
    let src = format!("{callee}takes(static fn (int $v): int => $v);");
    assert_eq!(param_count(&src), 0, "int return is narrower than int|string, fine");
}

// ==========================================================================
// 3. Arity — a closure requiring more params than the contract supplies.
// ==========================================================================

#[test]
fn closure_requiring_more_params_is_violation() {
    // The callee will invoke `$cb($int)` with one argument; a closure REQUIRING
    // two would `ArgumentCountError` (verified against PHP 8.5).
    let src = format!("{INT_TO_STRING}takes(static fn (int $a, int $b): string => (string) $a);");
    assert_eq!(param_count(&src), 1, "2 required params > 1 supplied → arity violation");
}

#[test]
fn closure_with_fewer_params_is_ok() {
    // A closure that ignores an argument the contract supplies is fine.
    let callee =
        "<?php /** @param callable(int, int): string $cb */ function takes(callable $cb): void {}\n";
    let src = format!("{callee}takes(static fn (int $a): string => (string) $a);");
    assert_eq!(param_count(&src), 0, "1 param ≤ 2 supplied, and it matches");
}

#[test]
fn closure_extra_optional_param_is_ok() {
    // Surplus OPTIONAL parameters do not fatal (PHP ignores surplus args).
    let src = format!("{INT_TO_STRING}takes(static fn (int $a, int $b = 0): string => (string) $a);");
    assert_eq!(param_count(&src), 0, "extra optional param is fine");
}

// ==========================================================================
// 4. Silence matrix — every undecidable shape stays silent (zero-FP).
// ==========================================================================

#[test]
fn bare_callable_contract_never_fires() {
    // No signature on the contract → any callable is accepted.
    let callee = "<?php /** @param callable $cb */ function takes(callable $cb): void {}\n";
    let src = format!("{callee}takes(static fn (string $v): float => 1.0);");
    assert_eq!(param_count(&src), 0, "bare callable accepts any signature");
}

#[test]
fn undeclared_closure_param_is_silent() {
    // No native type on the closure param → Maybe → silent.
    let src = format!("{INT_TO_STRING}takes(static fn ($v): string => (string) $v);");
    assert_eq!(param_count(&src), 0, "undeclared closure param is not judged");
}

#[test]
fn undeclared_closure_return_is_silent() {
    // No native return hint on the closure → return covariance is Maybe → silent.
    let src = format!("{INT_TO_STRING}takes(static fn (int $v) => $v);");
    assert_eq!(param_count(&src), 0, "undeclared closure return is not judged");
}

#[test]
fn template_callable_contract_is_silent() {
    // A template-bearing callable type lowers to a bare callable (ADR-0032/0051 —
    // no call-site template solver), so it never judges.
    let callee =
        "<?php /** @param callable(T): T $cb */ function takes(callable $cb): void {}\n";
    let src = format!("{callee}takes(static fn (int $v): string => (string) $v);");
    assert_eq!(param_count(&src), 0, "template callable stays silent");
}

#[test]
fn object_typed_positions_stay_silent() {
    // Cross-class comparisons are only reflexively decidable; a different class is
    // Maybe, never a definite No.
    let callee =
        "<?php /** @param callable(A): B $cb */ function takes(callable $cb): void {}\nclass A {}\nclass B {}\nclass C {}\nclass D {}\n";
    let src = format!("{callee}takes(static fn (C $v): D => new D());");
    assert_eq!(param_count(&src), 0, "unrelated class params/returns stay silent");
}

#[test]
fn byref_closure_param_is_silent() {
    // By-reference callable semantics are unverified — stay silent.
    let src = format!("{INT_TO_STRING}takes(static fn (string &$v): string => $v);");
    assert_eq!(param_count(&src), 0, "by-ref param position is skipped");
}

// ==========================================================================
// 5. First-class callables — judged only where a user target resolves uniquely.
// ==========================================================================

#[test]
fn first_class_callable_to_user_fn_is_judged() {
    // `wants(...)` names a user function whose declared signature violates the
    // contract return: it returns `int` where `string` is promised.
    let src = format!(
        "{INT_TO_STRING}function wants(int $x): int {{ return $x; }}\ntakes(wants(...));"
    );
    assert_eq!(param_count(&src), 1, "resolved user fn return int violates string");
}

#[test]
fn first_class_callable_to_user_fn_ok() {
    let src = format!(
        "{INT_TO_STRING}function wants(int $x): string {{ return (string) $x; }}\ntakes(wants(...));"
    );
    assert_eq!(param_count(&src), 0, "resolved user fn matches the contract");
}

#[test]
fn first_class_callable_to_builtin_is_silent() {
    // A builtin has no ground-truth Steins signature → Maybe → silent.
    let src = format!("{INT_TO_STRING}takes(strlen(...));");
    assert_eq!(param_count(&src), 0, "builtin first-class callable stays silent");
}
