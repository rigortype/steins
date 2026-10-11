//! Issue #1051 slice S4: `call.too-few-arguments` through a closure value, a
//! first-class callable and a string callee. One test per row of the callee table
//! in `crates/steins-infer/src/arity.rs`; every PHP verdict was run on 8.5.
//! A too-many call to a user callable is never an error in PHP, so none is judged.

use steins_infer::{CALL_TOO_FEW_ARGUMENTS_ID, Diagnostic, check_with};
use steins_syntax::SourceTree;

use crate::arity::Boot;

fn arity(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot::ready())
        .into_iter()
        .filter(|d| d.id == CALL_TOO_FEW_ARGUMENTS_ID)
        .collect()
}

fn fires_once(src: &str, to: &str) {
    let d = arity(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains(&format!("to {to}()")), "{}", d[0].message);
}

fn silent(src: &str) {
    let d = arity(src);
    assert!(d.is_empty(), "{d:?}");
}

const ADD: &str = "<?php\nfunction add(int $a, int $b) { return $a + $b; }\n";

#[test]
fn closure_variable() {
    fires_once("<?php\n$f = function (int $a, int $b) {};\n$f(1);\n", "closure");
}

#[test]
fn arrow_function_variable() {
    fires_once("<?php\n$f = fn(int $a, int $b) => $a + $b;\n$f(1);\n", "closure");
}

#[test]
fn iife() {
    fires_once("<?php\n(function (int $a, int $b) {})(1);\n", "closure");
    fires_once("<?php\n(fn(int $a, int $b) => 1)(1);\n", "closure");
}

#[test]
fn first_class_callable_variable() {
    fires_once(&format!("{ADD}$f = add(...);\n$f(1);\n"), "add");
}

#[test]
fn first_class_callable_called_in_place() {
    fires_once(&format!("{ADD}add(...)(1);\n"), "add");
}

#[test]
fn string_literal_variable() {
    fires_once(&format!("{ADD}$f = 'add';\n$f(1);\n"), "add");
}

#[test]
fn string_literal_called_in_place() {
    fires_once(&format!("{ADD}'add'(1);\n"), "add");
    fires_once(&format!("{ADD}'\\add'(1);\n"), "add");
}

#[test]
fn string_callee_is_global_never_the_namespace() {
    // PHP: `'add'` inside `namespace App` is an undefined global function (Error).
    let ns = "<?php\nnamespace App;\nfunction add(int $a, int $b) {}\n";
    silent(&format!("{ns}$f = 'add';\n$f(1);\n"));
    silent(&format!("{ns}'add'(1);\n"));
    fires_once(&format!("{ns}$f = 'App\\\\add';\n$f(1);\n"), "add");
    fires_once(&format!("{ns}'App\\\\add'(1);\n"), "add");
}

#[test]
fn named_argument_and_enough_arguments_decline_like_a_direct_call() {
    silent("<?php\n$f = function (int $a, int $b) {};\n$f(1, 2);\n");
    silent("<?php\n$f = function (int $a, int $b) {};\n$f(1, 2, 3);\n");
    silent("<?php\n$f = function (int $a, int $b) {};\n$f(1, b: 2);\n");
    silent("<?php\n$f = function (int $a, int $b) {};\n$f(...$xs);\n");
    silent("<?php\n$f = function (int $a, int ...$b) {};\n$f(1);\n");
    silent("<?php\n$f = function (int $a, int $b = 2) {};\n$f(1);\n");
}

#[test]
fn array_callable_and_invoke_object_decline() {
    silent("<?php\nclass C { function m(int $a, int $b) {} }\n$o = new C;\n[$o, 'm'](1);\n");
    silent("<?php\nclass D { function __invoke(int $a, int $b) {} }\n$o = new D;\n$o(1);\n");
}

#[test]
fn one_of_closures_declines() {
    silent(
        "<?php\n$f = rand(0, 1) ? function (int $a) {} : function (int $a, int $b) {};\n$f(1);\n",
    );
}

#[test]
fn by_reference_captured_closure_declines() {
    // PHP: `$g()` replaces `$f`, so `$f(1)` runs clean.
    silent(
        "<?php\n$f = function (int $a, int $b) {};\n$g = function () use (&$f) { $f = function () {}; };\n$g();\n$f(1);\n",
    );
}

#[test]
fn from_callable_declines() {
    silent(&format!("{ADD}$f = Closure::fromCallable('add');\n$f(1);\n"));
}

#[test]
fn builtin_and_unknown_string_callees_decline() {
    silent("<?php\n$f = 'strlen';\n$f();\n");
    silent("<?php\n$f = 'nope';\n$f(1);\n");
    silent("<?php\nclass K { static function m(int $a, int $b) {} }\n$f = 'K::m';\n$f(1);\n");
}

#[test]
fn creating_a_first_class_callable_is_not_a_call() {
    silent(&format!("{ADD}$f = add(...);\n"));
    silent(&format!("{ADD}add(...);\n"));
}

#[test]
fn a_reassigned_variable_declines() {
    silent("<?php\n$f = function (int $a, int $b) {};\n$f = function (int $a) {};\n$f(1);\n");
}
