//! An assignment or increment nested inside an expression statement is a write of
//! that statement (issue #694).
//!
//! The walk applies no nested assignment expression: `$x = ($s = 'bar')` binds `$x`
//! and says nothing of `$s`. `echo` and the offset writes (#641) already named such
//! a write, as an opaque invalidation entry; every other expression-statement arm
//! handed only its by-ref call arguments to the walk, so `$s` kept the value it had
//! before the statement — a wrong fact, not a missing one. PHP at 8.5.10:
//!
//! ```text
//! $s = 'foo'; $x = ($s = 'bar'); var_dump($s);   // string(3) "bar"
//! $s = 1;     $x = $s++;         var_dump($s);   // int(2)
//! ```
//!
//! The pins below answer `unknown` where the walk used to answer the pre-statement
//! value. That is the over-approximation the entry buys: the name is forgotten,
//! never modeled.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, ID, check};
use steins_syntax::SourceTree;

/// Every `debug.type` body a source produces, in source order.
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// A statement body wrapped in a function scope, dumping `$s` at the end.
fn dump_after(body: &str) -> String {
    let got =
        types(&format!("<?php\nfunction f(\\stdClass $o): void {{ {body} \\PHPStan\\dumpType($s); }}\n"));
    assert_eq!(got.len(), 1, "expected exactly one dump, got {got:?}");
    got.into_iter().next().unwrap()
}

#[test]
fn a_plain_assignment_right_hand_side_writes_what_it_assigns() {
    // The issue's two lines.
    assert_eq!(dump_after("$s = 'foo'; $x = ($s = 'bar');"), "dumped type: unknown");
    assert_eq!(dump_after("$s = 1; $x = $s++;"), "dumped type: unknown");
    // A compound assignment's right-hand side is the same position.
    assert_eq!(dump_after("$s = 1; $x = 'a'; $x .= ++$s;"), "dumped type: unknown");
}

#[test]
fn a_chained_assignment_forgets_the_inner_target() {
    // `$a = $b = 3` used to leave `$b` at `2`. It is forgotten now, not modeled:
    // the outer target is what the statement binds.
    let src = "<?php
function f(): void {
    $a = 1; $b = 2;
    $a = $b = 3;
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(types(src), vec!["dumped type: unknown".to_owned()]);
}

#[test]
fn a_call_argument_writes_what_it_assigns() {
    // The owner's widening on the issue: the call arm named only its bare-variable
    // arguments. A certified by-value callee, an unknown one, and a method call
    // all answer the same — the write, not the callee, is the reason.
    assert_eq!(dump_after("$s = 'abc'; strlen($s = 'zz');"), "dumped type: unknown");
    assert_eq!(dump_after("$s = 'abc'; my_helper($s = 'zz');"), "dumped type: unknown");
    assert_eq!(dump_after("$s = 'abc'; $o->m($s = 'zz');"), "dumped type: unknown");
    assert_eq!(dump_after("$s = 1; strlen((string) $s++);"), "dumped type: unknown");
}

#[test]
fn a_property_assignment_and_an_assert_write_what_they_assign() {
    assert_eq!(dump_after("$s = 'abc'; $o->p = ($s = 'zz');"), "dumped type: unknown");
    // `assert` keeps its narrowing and takes no call invalidation, but its
    // argument's own write is still a write.
    assert_eq!(dump_after("$s = 'abc'; assert(($s = 'zz') !== '');"), "dumped type: unknown");
}

#[test]
fn a_name_the_statement_does_not_write_keeps_its_fact() {
    // The control: the entry is per written name, so a neighbour survives.
    let src = "<?php
function f(): void {
    $t = 'k'; $s = 'a';
    $x = ($s = 'bar');
    \\PHPStan\\dumpType($t);
    strlen($s = 'zz');
    \\PHPStan\\dumpType($t);
}
";
    assert_eq!(types(src), vec!["dumped type: 'k'".to_owned(), "dumped type: 'k'".to_owned()]);
}

/// The proof-layer false positive the stale fact manufactured on the default
/// surface (confirmed on the v0.1.8 binary with `intdiv($z, 1)`): `$z` is `5`
/// when it is passed, and the walk said `null`.
#[test]
fn an_embedded_write_no_longer_proves_a_stale_argument_mismatch() {
    let src = "<?php
declare(strict_types=1);
function need_int(int $n): void {}
function t(): void { $z = null; $w = ($z = 5); need_int($z); }
function u(): void { $z = null; need_int($z = 5); need_int($z); }
";
    let tree = SourceTree::parse(src);
    let found: Vec<Diagnostic> =
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == ID).collect();
    assert!(found.is_empty(), "a written `$z` is not the null before it: {found:?}");
    // The control: without the embedded write the null is still proven.
    let control = "<?php
declare(strict_types=1);
function need_int(int $n): void {}
function t(): void { $z = null; $w = 5; need_int($z); }
";
    let tree = SourceTree::parse(control);
    let lines: Vec<u32> =
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == ID).map(|d| d.line).collect();
    assert_eq!(lines, vec![4], "the unwritten null still reports");
}
