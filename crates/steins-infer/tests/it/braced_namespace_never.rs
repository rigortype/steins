//! A braced namespace body is a declaration scope exactly as an unbraced
//! `namespace A;` is (ADR-0049 A2i, issue #973).
//!
//! The braced body is a `Block` node. Before the fix, every declaration under it
//! was lowered as conditional, so the never-returning veto declined and a call
//! to a `: never` function left its successor live: the `null` the throw path
//! cannot reach was reported as a `call.on-null` after the call. The pairs below
//! are the witness and its unbraced control; the conditional case pins that the
//! fix admits the body and not the statements inside it. The arity lane's braced
//! pair lives in `arity.rs`, where its boot-surface mock is.

use steins_infer::{CALL_ON_NULL_ID, Diagnostic, TYPE_MAYBE_RETURN_MISMATCH_ID, check};
use steins_syntax::SourceTree;

/// Every diagnostic `src` produces under the default profile.
fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

#[test]
fn a_never_call_in_a_braced_namespace_kills_its_successor() {
    // The witness (w973e). `boom()` never returns, so `$x->bar()` is unreachable
    // and there is no null dereference to report.
    let d = run(
        "<?php\n\
         namespace A {\n\
         \x20   function boom(): never { throw new \\Exception(); }\n\
         \x20   function k(): void { $x = null; boom(); $x->bar(); }\n\
         }\n",
    );
    assert!(d.iter().all(|d| d.id != CALL_ON_NULL_ID), "{d:?}");
}

#[test]
fn a_never_call_in_an_unbraced_namespace_kills_its_successor() {
    // The control (w973f): the same code, unbraced. It was already silent and
    // must stay silent, so the braced result is measured against a known shape.
    let d = run(
        "<?php\n\
         namespace A;\n\
         function boom(): never { throw new \\Exception(); }\n\
         function k(): void { $x = null; boom(); $x->bar(); }\n",
    );
    assert!(d.iter().all(|d| d.id != CALL_ON_NULL_ID), "{d:?}");
}

#[test]
fn a_never_declared_under_an_if_in_a_braced_namespace_does_not_prune() {
    // The fix admits the body, not what is inside it: a `never` declared under an
    // `if` in the braces is still conditional, so its call does not prune. The
    // `string|false` guard then leaves the maybe-grade return finding the
    // conditional twin of `a_conditionally_declared_never_does_not_prune`.
    let src = "<?php\n\
         declare(strict_types=1);\n\
         namespace A {\n\
         \x20   if (!function_exists('boom')) {\n\
         \x20       function boom(): never { throw new \\Exception(); }\n\
         \x20   }\n\
         \x20   function k(string|false $v): string {\n\
         \x20       if ($v === false) { boom(); }\n\
         \x20       return $v;\n\
         \x20   }\n\
         }\n";
    let maybes = run(src)
        .into_iter()
        .filter(|d| d.id == TYPE_MAYBE_RETURN_MISMATCH_ID)
        .count();
    assert_eq!(maybes, 1);
}
