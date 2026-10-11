//! Issue #297 — a call that is the **source** of a destructure is a checkable call.
//!
//! `[$a, $b] = f($bad);` carried the call in `StmtKind::Destructure.call`, and the
//! source read was judged, but `checkable_calls` did not list it, so the existence,
//! arity and receiver families never saw it. The same call in an assignment RHS
//! (`$t = f($bad);`) got every family. Each witness below is a PHP fatal or a
//! warning the destructure form hides, and each pairs with its `$t = …` spelling:
//! the two must report the same ids.
//!
//! # Behavioral witnesses at PHP 8.5.11 (`php -r`)
//!
//! ```text
//! [$a, $b] = nope_fn();          → Fatal error: Uncaught Error: Call to undefined function nope_fn()
//! function f() {} [$a] = f(1, 2);→ no finding (extra arguments to a userland function run clean)
//! function f($x) {} [$a] = f();  → Fatal error: Uncaught ArgumentCountError: Too few arguments
//! ['k' => $x] = $c->nope();      → Fatal error: Uncaught Error: Call to undefined method C::nope()
//! ```
//!
//! The arity witness is too few, not too many: PHP is silent on extra positional
//! arguments to a non-variadic userland function, so an extra-argument form is the
//! control for "a valid call stays silent" below.

use steins_infer::{
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_FUNCTION_ID, CALL_UNDEFINED_METHOD_ID, Diagnostic,
    Folder, check_with,
};
use steins_syntax::SourceTree;

/// The boot surface reports nothing resident and answers cleanly, so every
/// existence and arity leg can prove its finding (as in `arity.rs`).
#[derive(Default)]
struct Boot;

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_function(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

/// Every call-family finding a source produces, as `(id, message)`, in emission
/// order. The `untyped.*` advisories are the fixtures' missing annotations, not
/// the subject, so they are dropped.
fn diagnostics(src: &str) -> Vec<(&'static str, String)> {
    let tree = SourceTree::parse(src);
    let found: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot);
    found
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .map(|d| (d.id, d.message))
        .collect()
}

fn ids(src: &str) -> Vec<&'static str> {
    diagnostics(src).into_iter().map(|(id, _)| id).collect()
}

const UNDEFINED_FN: &str = "<?php\n[$a, $b] = nope_fn();\n";
const UNDEFINED_FN_ASSIGN: &str = "<?php\n$t = nope_fn();\n";

const TOO_FEW: &str = "<?php\nfunction f($x) {}\n[$a] = f();\n";
const TOO_FEW_ASSIGN: &str = "<?php\nfunction f($x) {}\n$t = f();\n";

const UNDEFINED_METHOD: &str = "<?php\nclass C {}\n$c = new C();\n['k' => $x] = $c->nope();\n";
const UNDEFINED_METHOD_ASSIGN: &str = "<?php\nclass C {}\n$c = new C();\n$t = $c->nope();\n";

// It fires: each destructure witness reports, exactly as its assignment spelling does.

#[test]
fn undefined_function_source_is_reported() {
    assert_eq!(ids(UNDEFINED_FN), [CALL_UNDEFINED_FUNCTION_ID]);
}

#[test]
fn undefined_function_destructure_matches_assignment() {
    assert_eq!(ids(UNDEFINED_FN), ids(UNDEFINED_FN_ASSIGN));
}

#[test]
fn too_few_arguments_source_is_reported() {
    assert_eq!(ids(TOO_FEW), [CALL_TOO_FEW_ARGUMENTS_ID]);
}

#[test]
fn too_few_arguments_destructure_matches_assignment() {
    assert_eq!(ids(TOO_FEW), ids(TOO_FEW_ASSIGN));
}

#[test]
fn undefined_method_source_is_reported() {
    assert_eq!(ids(UNDEFINED_METHOD), [CALL_UNDEFINED_METHOD_ID]);
}

#[test]
fn undefined_method_destructure_matches_assignment() {
    assert_eq!(ids(UNDEFINED_METHOD), ids(UNDEFINED_METHOD_ASSIGN));
}

// It stays silent: a valid call, and extra positional arguments (PHP runs clean).

#[test]
fn valid_destructure_source_is_silent() {
    let src = "<?php\nfunction f($x) { return [1, 2]; }\n[$a, $b] = f(1);\n";
    assert_eq!(ids(src), Vec::<&str>::new());
}

#[test]
fn valid_method_destructure_source_is_silent() {
    let src = "<?php\nclass C { public function ok(): array { return ['k' => 1]; } }\n\
               $c = new C();\n['k' => $x] = $c->ok();\n";
    assert_eq!(ids(src), Vec::<&str>::new());
}

#[test]
fn extra_positional_arguments_to_a_userland_function_are_silent() {
    let src = "<?php\nfunction f() { return [1]; }\n[$a] = f(1, 2);\n";
    assert_eq!(ids(src), Vec::<&str>::new());
}
