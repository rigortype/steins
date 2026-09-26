//! `new C($x)` is a call site like any other: a constructor that takes `&$x`
//! writes the caller's binding (issue #678).
//!
//! The call-argument walkers matched the four call nodes and skipped
//! `Node::Instantiation`, so a variable handed to a constructor stayed at its
//! pre-statement value — in straight-line code, through a `while` body's `reads`
//! (#659) and through a `for`'s `carried` set (#650). With `null` there, the walk
//! proved a `call.on-null` on a variable the constructor had just assigned. PHP at
//! 8.5.10 runs every fixture below without an error.
//!
//! A constructor argument is an opaque entry, as a method-call argument is: the
//! by-value gate (ADR-0070) resolves functions only, so the name is forgotten
//! whether or not the parameter is by reference.

use steins_infer::{CALL_ON_NULL_ID, DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

const SETTER: &str = "\
final class Foo { public function bar(): void {} }
final class Setter { public function __construct(?Foo &$slot) { $slot = new Foo(); } }
";

/// The lines carrying a `call.on-null` finding in `SETTER` followed by `body`.
fn on_null_lines(body: &str) -> Vec<u32> {
    let tree = SourceTree::parse(&format!("<?php\n{SETTER}{body}"));
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == CALL_ON_NULL_ID)
        .map(|d| d.line)
        .collect()
}

#[test]
fn a_by_ref_constructor_argument_is_written_in_straight_line_code() {
    let statement = "function f(): void {
    $x = null;
    new Setter($x);
    $x->bar();
}
";
    assert!(on_null_lines(statement).is_empty(), "the constructor assigned `$x`");
    // The same call as an assignment's right-hand side.
    let assigned = "function f(): void {
    $x = null;
    $s = new Setter($x);
    $x->bar();
}
";
    assert!(on_null_lines(assigned).is_empty(), "the right-hand side assigned `$x`");
}

#[test]
fn a_by_ref_constructor_argument_is_a_write_of_the_loop() {
    // The issue's `for` shape: `$x` was in `carried`, so the second iteration's
    // entry read the `init` null.
    let for_loop = "function f(): void {
    for ($x = null, $i = 0; $i < 2; $i++) {
        if ($i > 0) { $x->bar(); }
        new Setter($x);
    }
}
";
    assert!(on_null_lines(for_loop).is_empty(), "`for`: `$x` is a write of the loop");
    // The `while` twin: `$x` was in `reads`, kept at the body entry.
    let while_loop = "function f(): void {
    $x = null;
    $i = 0;
    while ($i < 2) {
        if ($i > 0) { $x->bar(); }
        new Setter($x);
        $i++;
    }
}
";
    assert!(on_null_lines(while_loop).is_empty(), "`while`: `$x` is a write of the loop");
}

#[test]
fn a_null_the_constructor_never_saw_still_reports() {
    // The control: the finding is the constructor's to silence, not the class's.
    let src = "function f(): void {
    $x = null;
    $y = null;
    new Setter($y);
    $x->bar();
}
";
    assert_eq!(on_null_lines(src), vec![8], "`$x` was not handed to the constructor");
}

#[test]
fn a_constructor_argument_is_forgotten_after_the_statement() {
    let src = "<?php
final class Box { public function __construct(string $s) {} }
function f(): void {
    $s = 'abc';
    new Box($s);
    \\PHPStan\\dumpType($s);
}
";
    let tree = SourceTree::parse(src);
    let dumps: Vec<String> = check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(dumps, vec!["dumped type: unknown".to_owned()], "no site certifies a constructor");
}
