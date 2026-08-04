//! Acceptance tests for the refined callable spellings' obligations (ADR-0063 P3):
//! `pure-callable`, `pure-closure`, `static-closure`, `static-pure-closure` lowered
//! to `CallableTy` plus a purity / static-binding / closure-ness obligation on the
//! bound argument.
//!
//! Three independent halves, each with its own decider:
//!
//! * **purity** — the bound callable's *inferred* effect envelope, read from the
//!   whole-project effect fixpoint (ADR-0055 `Pure` semantics: an empty label set
//!   tolerates no label). Never a declaration flag — the metadata-only purity flag
//!   is the import ADR-0063 §3 declines.
//! * **static binding** — a syntactic check on the closure's `static` keyword.
//! * **closure-ness** — a value-domain check (`admits_val`): a callable-string is
//!   callable but is not a `Closure` instance.
//!
//! Zero-FP throughout: only a *proven* violation reports. An opaque callable value,
//! an unresolvable name, or a builtin stays silent.

use steins_infer::{check, Diagnostic, PARAM_MISMATCH_ID};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == PARAM_MISMATCH_ID)
        .collect()
}

fn count(src: &str) -> usize {
    findings(src).len()
}

fn one(src: &str) -> Diagnostic {
    let f = findings(src);
    assert_eq!(f.len(), 1, "expected exactly one param-mismatch, got: {f:#?}");
    f.into_iter().next().unwrap()
}

const PURE_CALLABLE: &str =
    "<?php /** @param pure-callable $cb */ function takes($cb): void {}\n";
const PURE_CLOSURE: &str = "<?php /** @param pure-closure $cb */ function takes($cb): void {}\n";
const STATIC_CLOSURE: &str =
    "<?php /** @param static-closure $cb */ function takes($cb): void {}\n";
const STATIC_PURE_CLOSURE: &str =
    "<?php /** @param static-pure-closure $cb */ function takes($cb): void {}\n";

// 1. Purity — the semantic half.

#[test]
fn echoing_closure_literal_violates_pure_callable() {
    let src = format!("{PURE_CALLABLE}takes(static function (): void {{ echo 'x'; }});");
    let d = one(&src);
    assert!(d.message.contains("not pure"), "names the purity obligation: {}", d.message);
    assert!(d.message.contains("pure-callable"), "renders the spelling: {}", d.message);
}

#[test]
fn computing_closure_literal_satisfies_pure_callable() {
    let src = format!("{PURE_CALLABLE}takes(static fn (int $v): int => $v + 1);");
    assert_eq!(count(&src), 0, "a closure that only computes is pure");
}

#[test]
fn echoing_closure_through_a_variable_violates_pure_callable() {
    // The propagation-pass lane: the variable carries a PROVEN closure value and no
    // scalar fact at all, so neither value lane can see it.
    let src = format!(
        "{PURE_CALLABLE}$cb = static function (): void {{ echo 'x'; }};\ntakes($cb);"
    );
    assert_eq!(count(&src), 1, "a variable-bound impure closure is judged too");
}

#[test]
fn impure_named_function_first_class_callable_violates_pure_callable() {
    let src = format!(
        "{PURE_CALLABLE}function impure(): int {{ echo 'x'; return 1; }}\ntakes(impure(...));"
    );
    assert_eq!(count(&src), 1, "an impure user function is not a pure-callable");
}

#[test]
fn pure_named_function_first_class_callable_is_silent() {
    let src =
        format!("{PURE_CALLABLE}function calc(int $v): int {{ return $v + 1; }}\ntakes(calc(...));");
    assert_eq!(count(&src), 0, "a computing user function satisfies the obligation");
}

#[test]
fn builtin_first_class_callable_stays_silent() {
    // A builtin has no body the fixpoint reads — `Maybe`, so silent (zero-FP).
    let src = format!("{PURE_CALLABLE}takes(strlen(...));");
    assert_eq!(count(&src), 0, "an unread callable body never reports");
}

#[test]
fn opaque_callable_parameter_stays_silent() {
    // A `callable` value forwarded from an enclosing parameter is opaque: no
    // definition in scope, so the obligation cannot be decided.
    let src = format!(
        "{PURE_CALLABLE}function fwd(callable $x): void {{ takes($x); }}\n"
    );
    assert_eq!(count(&src), 0, "an opaque callable value stays Maybe");
}

#[test]
fn plain_callable_spelling_imposes_no_purity() {
    // The obligation must come from the *refined* spelling: a bare `callable`
    // accepts an echoing closure exactly as it always did.
    let src = "<?php /** @param callable $cb */ function takes($cb): void {}\n\
               takes(static function (): void { echo 'x'; });";
    assert_eq!(count(src), 0, "bare callable carries no purity obligation");
}

// 2. Static binding — the syntactic half.

#[test]
fn non_static_closure_violates_static_closure() {
    let src = format!("{STATIC_CLOSURE}takes(fn (): int => 1);");
    let d = one(&src);
    assert!(d.message.contains("not declared static"), "names the half: {}", d.message);
    assert!(d.message.contains("static-closure"), "renders the spelling: {}", d.message);
}

#[test]
fn static_closure_satisfies_static_closure() {
    let src = format!("{STATIC_CLOSURE}takes(static fn (): int => 1);");
    assert_eq!(count(&src), 0, "the keyword satisfies the binding obligation");
}

#[test]
fn static_keyword_on_a_full_closure_is_recorded() {
    let src = format!("{STATIC_CLOSURE}takes(static function (): int {{ return 1; }});");
    assert_eq!(count(&src), 0, "`static function` is recorded, not only `static fn`");
}

#[test]
fn non_static_full_closure_violates_static_closure() {
    let src = format!("{STATIC_CLOSURE}takes(function (): int {{ return 1; }});");
    assert_eq!(count(&src), 1, "a bindable closure is not a static-closure");
}

#[test]
fn first_class_callable_satisfies_static_closure() {
    // `f(...)` has no bound `$this`, so it satisfies the binding obligation the way
    // `static function () {}` does.
    let src = format!("{STATIC_CLOSURE}function calc(): int {{ return 1; }}\ntakes(calc(...));");
    assert_eq!(count(&src), 0, "a free-function first-class callable is unbound");
}

// 3. Closure-ness — the value-domain half, which fails independently.

#[test]
fn callable_string_is_not_a_closure() {
    let src = format!("{PURE_CLOSURE}takes('strlen');");
    assert_eq!(count(&src), 1, "a callable-string fails the Closure half");
}

#[test]
fn callable_string_is_not_a_static_closure_either() {
    let src = format!("{STATIC_CLOSURE}takes('strlen');");
    assert_eq!(count(&src), 1, "the Closure half needs no purity analysis");
}

#[test]
fn callable_string_still_satisfies_pure_callable() {
    // `pure-callable` is not closure-only: a string may name a pure function, and
    // its purity is not decidable from the value, so this stays `Maybe`.
    let src = format!("{PURE_CALLABLE}takes('strlen');");
    assert_eq!(count(&src), 0, "pure-callable admits a callable-string as Maybe");
}

#[test]
fn non_callable_scalar_fails_every_spelling() {
    for callee in [PURE_CALLABLE, PURE_CLOSURE, STATIC_CLOSURE, STATIC_PURE_CLOSURE] {
        let src = format!("{callee}takes(1);");
        assert_eq!(count(&src), 1, "1 is not callable at all");
    }
}

// 4. Composition — `static-pure-closure` fails each half on its own.

#[test]
fn pure_but_not_static_violates_static_pure_closure() {
    let src = format!("{STATIC_PURE_CLOSURE}takes(fn (int $v): int => $v + 1);");
    let d = one(&src);
    assert!(d.message.contains("not declared static"), "the static half: {}", d.message);
}

#[test]
fn static_but_not_pure_violates_static_pure_closure() {
    let src = format!(
        "{STATIC_PURE_CLOSURE}$cb = static function (): void {{ echo 'x'; }};\ntakes($cb);"
    );
    let d = one(&src);
    assert!(d.message.contains("not pure"), "the purity half: {}", d.message);
}

#[test]
fn static_and_pure_satisfies_both_halves() {
    let src = format!("{STATIC_PURE_CLOSURE}takes(static fn (int $v): int => $v + 1);");
    assert_eq!(count(&src), 0, "both halves hold");
}

// 5. The obligation composes with the signature half (issue #11).

#[test]
fn signature_bearing_pure_callable_still_judges_variance() {
    let src = "<?php /** @param pure-callable(int): string $cb */ function takes($cb): void {}\n\
               takes(static fn (string $v): string => $v);";
    assert_eq!(count(src), 1, "the parenthesized form keeps the variance check");
}

#[test]
fn signature_bearing_pure_callable_also_judges_purity() {
    let src = "<?php /** @param pure-callable(int): string $cb */ function takes($cb): void {}\n\
               takes(static function (int $v): string { echo $v; return (string) $v; });";
    let d = one(src);
    assert!(d.message.contains("not pure"), "the obligation rides the signature: {}", d.message);
}

// 6. The `@return` side is NOT judged (the fixtures' explicit non-expectation).

#[test]
fn return_position_carries_no_obligation() {
    // An analyzer that cannot *construct* a `pure-closure` would report the valid
    // probe and the `@return` body. Steins judges the argument lane only.
    let src = "<?php /** @return pure-closure */ function mk() { return static fn (int $v): int => $v + 1; }\n";
    assert_eq!(count(src), 0, "no obligation is imposed on a returned callable");
}
