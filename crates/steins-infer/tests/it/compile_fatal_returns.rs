//! `return.compile-fatal` (ADR-0078 amendment 2026-10-11, issues #952, #1051): the
//! `return` statements PHP's compiler refuses outright. One test per row of the rule
//! table on `check_compile_fatal_returns`; every row was run on `php -l` 8.5.11, and the
//! quoted sentence is PHP's own.
//!
//! The "fire" rows fail on the base (the id does not exist there); the "silent" rows are
//! the controls that keep the rule from over-reaching.

use steins_infer::{Diagnostic, Floor, Layer, NoFold, RETURN_COMPILE_FATAL_ID, check_full, layer};
use steins_syntax::SourceTree;

fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == RETURN_COMPILE_FATAL_ID)
        .collect()
}

/// Exactly one finding, quoting `php_message`.
fn fires(src: &str, php_message: &str) -> Diagnostic {
    let d = diags(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains(&format!("\"{php_message}\"")), "{}", d[0].message);
    d.into_iter().next().unwrap()
}

fn silent(src: &str) {
    let d = diags(src);
    assert!(d.is_empty(), "{d:#?}");
}

const VOID_FN: &str = "A void function must not return a value";
const VOID_METHOD: &str = "A void method must not return a value";
const NEVER_FN: &str = "A never-returning function must not return";
const NEVER_METHOD: &str = "A never-returning method must not return";
const MISSING_FN: &str = "A function with return type must return a value";
const MISSING_METHOD: &str = "A method with return type must return a value";

#[test]
fn the_id_is_proof_layer_default_floor() {
    assert_eq!(layer(RETURN_COMPILE_FATAL_ID), Some(Layer::Proof));
    assert_eq!(steins_infer::surface_floor(RETURN_COMPILE_FATAL_ID), Some(Floor::Default));
}

// --- `: void` -----------------------------------------------------------------------

#[test]
fn void_function_returning_a_value_fires() {
    let d = fires("<?php\nfunction f(): void {\n    return 5;\n}\n", VOID_FN);
    assert_eq!((d.line, d.column), (3, 5));
}

#[test]
fn void_function_returning_null_fires() {
    fires("<?php\nfunction f(): void { return null; }\n", VOID_FN);
}

#[test]
fn void_method_says_method() {
    fires("<?php\nclass A { function f(): void { return 1; } }\n", VOID_METHOD);
}

#[test]
fn void_closure_says_function() {
    fires("<?php\n$g = function (): void { return 1; };\n", VOID_FN);
}

#[test]
fn void_arrow_function_fires() {
    fires("<?php\n$g = fn(): void => 5;\n", VOID_FN);
    fires("<?php\n$g = fn(): void => null;\n", VOID_FN);
}

#[test]
fn void_return_in_dead_code_fires() {
    fires("<?php\nfunction f(): void { if (false) { return 5; } }\n", VOID_FN);
}

#[test]
fn void_return_in_a_switch_and_a_try_fires() {
    fires("<?php\nfunction f(): void { switch (1) { case 1: return 2; } }\n", VOID_FN);
    fires("<?php\nfunction f(): void { try { return 1; } finally {} }\n", VOID_FN);
}

#[test]
fn void_function_with_a_bare_return_is_silent() {
    silent("<?php\nfunction f(): void { return; }\n");
}

#[test]
fn every_valued_return_of_a_void_body_is_its_own_finding() {
    let d = diags("<?php\nfunction f(): void { if (rand()) { return 1; } return 2; }\n");
    assert_eq!(d.len(), 2, "{d:#?}");
}

// --- `: never` ----------------------------------------------------------------------

#[test]
fn never_function_with_a_value_fires() {
    fires("<?php\nfunction f(): never { return 5; }\n", NEVER_FN);
}

#[test]
fn never_function_with_a_bare_return_fires() {
    fires("<?php\nfunction f(): never { return; }\n", NEVER_FN);
}

#[test]
fn never_method_says_method() {
    fires("<?php\nclass A { function f(): never { return; } }\n", NEVER_METHOD);
}

#[test]
fn never_closure_fires() {
    fires("<?php\n$g = function (): never { return 1; };\n", NEVER_FN);
}

#[test]
fn never_arrow_function_compiles_and_is_silent() {
    silent("<?php\n$g = fn(): never => 5;\n");
}

#[test]
fn never_function_that_throws_is_silent() {
    silent("<?php\nfunction f(): never { throw new Exception(); }\n");
}

// --- any other written hint, bare `return;` -----------------------------------------

#[test]
fn bare_return_under_each_other_hint_fires() {
    for hint in [
        "int", "?int", "int|string", "int|null", "mixed", "array", "iterable", "object",
        "string", "bool", "float", "callable", "Foo", "?Foo", "null", "false", "true",
    ] {
        let src = format!("<?php\nfunction f(): {hint} {{ return; }}\n");
        fires(&src, MISSING_FN);
    }
}

#[test]
fn bare_return_under_static_and_self_fires_in_a_method() {
    fires("<?php\nclass A { function f(): static { return; } }\n", MISSING_METHOD);
    fires("<?php\nclass A { function f(): self { return; } }\n", MISSING_METHOD);
}

#[test]
fn bare_return_in_a_typed_closure_and_deep_in_a_loop_fires() {
    fires("<?php\n$g = function (): int { return; };\n", MISSING_FN);
    fires(
        "<?php\nfunction f(): int { foreach ([1] as $x) { if ($x) { return; } } return 1; }\n",
        MISSING_FN,
    );
}

#[test]
fn bare_return_in_a_destructor_with_a_hint_fires() {
    fires("<?php\nclass A { function __destruct(): int { return; } }\n", MISSING_METHOD);
}

// --- the controls: compiles -----------------------------------------------------------

#[test]
fn int_function_returning_null_is_not_this_id() {
    silent("<?php\nfunction f(): int { return null; }\n");
}

#[test]
fn typed_functions_returning_values_are_silent() {
    silent("<?php\nfunction f(): int { return 5; }\nfunction g(): ?int { return null; }\n");
    silent("<?php\nfunction f(): null { return null; }\n");
    silent("<?php\n$g = fn(): int => 5;\n");
}

#[test]
fn constructor_and_destructor_returning_a_value_are_silent() {
    silent("<?php\nclass A { function __construct() { return 5; } function __destruct() { return 5; } }\n");
}

#[test]
fn untyped_functions_are_silent() {
    silent("<?php\nfunction f() { return; }\nfunction g() { return 5; }\n$h = fn() => 1;\n");
}

#[test]
fn the_rule_is_per_scope() {
    // A closure returning a value inside a `: void` body, and an anonymous-class
    // method doing the same, are judged by their own (absent) hints.
    silent("<?php\nfunction f(): void { $g = function () { return 5; }; }\n");
    silent("<?php\nfunction f(): void { $g = fn() => 5; }\n");
    silent("<?php\nfunction f(): void { new class { function g() { return 5; } }; }\n");
    // …and the closure's own hint still applies, with the outer one irrelevant.
    fires("<?php\nfunction f(): int { $g = function (): void { return 5; }; return 1; }\n", VOID_FN);
}

#[test]
fn a_void_body_whose_nested_closure_has_its_own_fatal_reports_once() {
    let d = diags("<?php\nfunction f(): void { $g = function (): int { return; }; }\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains(MISSING_FN), "{}", d[0].message);
}

#[test]
fn abstract_and_interface_methods_have_no_body() {
    silent(
        "<?php\ninterface I { function f(): void; function g(): int; }\n\
         abstract class B { abstract function h(): never; }\n",
    );
}

// --- declined shapes ----------------------------------------------------------------

#[test]
fn nullable_and_union_void_are_a_different_fatal_and_declined() {
    silent("<?php\nfunction f(): ?void { return 5; }\n");
    silent("<?php\nfunction f(): ?void { return; }\n");
    silent("<?php\nfunction f(): int|void { return; }\n");
}

#[test]
fn generator_bodies_are_declined() {
    silent("<?php\nfunction f(): int { yield 1; return; }\n");
    silent("<?php\nfunction f(): Generator { yield 1; return; }\n");
    silent("<?php\nfunction f(): void { yield 1; return 5; }\n");
}

#[test]
fn property_hook_bodies_are_declined() {
    silent("<?php\nclass A { public int $v { get { return; } } }\n");
}
