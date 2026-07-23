//! ADR-0043 stage 2: `instanceof` migrated onto the trinary is-a oracle.
//!
//! A proven-false `instanceof` (the operand's exact class is-a-`No` against the
//! target, under a completely enumerated hierarchy) now yields `Certainty::No`,
//! making the guarded branch dead — the same dead-region behavior as any other
//! proven-false condition. An incomplete hierarchy stays `Maybe` (branch live),
//! and a proven supertype stays `Yes`. Instanceof still binds no exactness fact.

use steins_infer::{Diagnostic, check};
use steins_syntax::SourceTree;

fn n(src: &str) -> usize {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let out: Vec<Diagnostic> = check(&tree, &functions, "test.php");
    out.len()
}

#[test]
fn definite_no_instanceof_kills_then_branch() {
    // `$x` is exactly `Foo`; `Foo` has no ancestors, so its supertype closure is
    // `{Foo}` — `Foo instanceof Bar` is a definite `No`. The then-branch is dead,
    // so the wrong-typed call inside it is never analyzed → silent.
    let src = "<?php
class Foo { public function m(int $w): void {} }
class Bar {}
$x = new Foo();
if ($x instanceof Bar) { $x->m(\"abc\"); }
";
    assert_eq!(n(src), 0, "provably-false instanceof → dead then-branch → silent");
}

#[test]
fn unknown_instanceof_keeps_branch_live() {
    // Same shape, but `Foo` extends an uncatalogued external `Ext`, so the is-a
    // enumeration is incomplete → `Maybe`. The branch stays live and the
    // wrong-typed call inside it is flagged (the FP-safe side, unchanged).
    let src = "<?php
class Foo extends \\Vendor\\Ext { public function m(int $w): void {} }
$x = new Foo();
if ($x instanceof Bar) { $x->m(\"abc\"); }
";
    assert_eq!(n(src), 1, "incomplete hierarchy → Maybe → live branch → flagged");
}

#[test]
fn definite_yes_instanceof_kills_else_branch() {
    // `Foo extends Base`, so `Foo instanceof Base` is a definite `Yes`: the
    // condition holds, the `else` is dead, and the wrong-typed call there is
    // silent.
    let src = "<?php
class Base {}
class Foo extends Base { public function m(int $w): void {} }
$x = new Foo();
if ($x instanceof Base) {} else { $x->m(\"abc\"); }
";
    assert_eq!(n(src), 0, "provably-true instanceof → dead else-branch → silent");
}

#[test]
fn control_unrelated_call_without_guard_is_flagged() {
    // Control for the two `0`-expecting tests: with no dead branch, the same
    // wrong-typed call IS flagged — proving the silence above is branch death,
    // not a broken class fact.
    let src = "<?php
class Foo { public function m(int $w): void {} }
$x = new Foo();
$x->m(\"abc\");
";
    assert_eq!(n(src), 1, "no guard → live call → flagged");
}

#[test]
fn transitive_implements_yes_kills_else_branch() {
    // The `Yes` path through the transitive `implements` closure: `Foo` implements
    // `J` (via `Base`), and `J extends I`, so `Foo instanceof I` is a definite
    // `Yes` → the `else` is dead.
    let src = "<?php
interface I {}
interface J extends I {}
class Base implements J {}
class Foo extends Base { public function m(int $w): void {} }
$x = new Foo();
if ($x instanceof I) {} else { $x->m(\"abc\"); }
";
    assert_eq!(n(src), 0, "transitive-implements Yes → dead else-branch → silent");
}

#[test]
fn exception_instanceof_stringable_is_not_dead() {
    // PHP 8.0+: `Throwable extends Stringable`, so every Exception/Error instance
    // IS a `Stringable` (verified against PHP 8.5: `$e instanceof Stringable` is
    // `true`). The `instanceof \Stringable` branch is therefore reachable and the
    // wrong-typed call inside it MUST be flagged. A wrong `No` here would silently
    // kill a live branch — the cardinal sin. Correct verdict is `Yes` (branch
    // holds) or at worst `Unknown` (branch live); either way the call is analyzed.
    let src = "<?php
class MyEx extends \\Exception { public function m(int $w): void {} }
$x = new MyEx();
if ($x instanceof \\Stringable) { $x->m(\"abc\"); }
";
    assert_eq!(n(src), 1, "Exception IS-A Stringable → live branch → wrong-typed call flagged");
}
