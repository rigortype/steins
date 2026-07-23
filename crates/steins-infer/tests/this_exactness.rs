//! Audit G1 — the `$this` exactness bit: membership is not exactness in the heap.
//!
//! `seed_this_object` binds `$this` to `HeapObj { class: <enclosing class>, … }`,
//! but the enclosing class is only a **lower bound**: any subclass instance runs the
//! method. Before the `class_exact` bit, every heap-class consumer read that lower
//! bound as an *exact* class, so a No-side conclusion (`is_a(enclosing, T) = No`)
//! manufactured a false positive — the runtime object may be a descendant that *is*
//! a `T`. The bit gates every No-side consumer: acceptance definite-No,
//! `eval_instanceof`'s No verdict, exact-dispatch, and phpdoc `CVal::Object`.
//!
//! Yes-side conclusions (`is_a(lower, T) = Yes`) stay valid for a lower bound (every
//! descendant is still a `T`) and must NOT be demoted — the retention tests pin that.

use steins_infer::{Diagnostic, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

fn ids(src: &str) -> Vec<String> {
    findings(src).into_iter().map(|d| d.id.to_owned()).collect()
}

// ==========================================================================
// 1. The two live FP shapes from the audit — SILENT after the fix.
//    (Both FIRED a definite-No `type.argument-mismatch` / a wrong dead branch
//     before the `class_exact` bit landed.)
// ==========================================================================

#[test]
fn fp_shape1_this_argument_lower_bound_is_silent() {
    // FIRED BEFORE: `$this` "holds a Node2", `add_leaf` wants a `Leaf`,
    // is_a(Node2, Leaf) = No under complete enumeration → definite-No FP.
    // Runtime `(new Leaf())->register()` is fine — `$this` IS a Leaf.
    let src = "<?php declare(strict_types=1);
class Node2 {
    public int $depth = 0;
    public function register(): void { add_leaf($this); }
}
class Leaf extends Node2 {}
function add_leaf(Leaf $l): void {}
(new Leaf())->register();";
    assert_eq!(n(src), 0, "$this is a lower bound → No-side acceptance gated → silent");
}

#[test]
fn fp_shape2_this_instanceof_subclass_branch_not_dead() {
    // FIRED BEFORE: `$this instanceof Sub2` answered a definite No, the then-branch
    // was treated dead, so `$v` stayed Singleton(1) and `takes_string($v)` fired a
    // definite `type.argument-mismatch`. Runtime `(new Sub2())->m()` sets $v="hello".
    let src = "<?php declare(strict_types=1);
class Base2 {
    public int $x = 0;
    public function m(): void {
        $v = 1;
        if ($this instanceof Sub2) { $v = \"hello\"; }
        takes_string($v);
    }
}
class Sub2 extends Base2 {}
function takes_string(string $s): void {}
(new Sub2())->m();";
    assert_eq!(n(src), 0, "instanceof No on a lower bound → Maybe → then-branch live → silent");
}

#[test]
fn fp_shape3_laundered_alias_dispatch_is_guarded() {
    // FIRED BEFORE: `$u = $this` laundered the enclosing class into an "exact"
    // `Receiver::Var`, so `$u->m("abc")` resolved EXACTLY to Base3::m and checked
    // "abc" against its int param → FP. A subclass may override `m` with a widened
    // (string-accepting) signature — the runtime is not bound to Base3::m — so an
    // overridable method on a lower bound must route through the final/private guard.
    let src = "<?php declare(strict_types=1);
class Base3 {
    public int $x = 0;
    public function go(): void { $u = $this; $u->m(\"abc\"); }
    public function m(int $w): void {}
}
(new Base3())->go();";
    assert_eq!(n(src), 0, "aliased $this is not exact → guarded dispatch → overridable m silent");
}

// ==========================================================================
// 2. Retention — TRUE positives must still fire.
// ==========================================================================

#[test]
fn retention_new_object_incompatible_param_still_fires() {
    // (a) `new Foo()` is allocation-proven exact — the No-side stays live.
    let src = "<?php declare(strict_types=1);
final class User {}
final class Robot {}
function f(User $u): void {}
$r = new Robot();
f($r);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "exact new Robot rejected by User");
}

#[test]
fn retention_this_in_final_class_still_fires() {
    // (b) A `final` class has no subclass, so its `$this` IS exact — the definite-No
    // acceptance is sound and must still fire.
    let src = "<?php declare(strict_types=1);
final class Widget {
    public int $x = 0;
    public function reg(): void { takes_gadget($this); }
}
class Gadget {}
function takes_gadget(Gadget $g): void {}
(new Widget())->reg();";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "final-class $this is exact → No fires");
}

#[test]
fn retention_this_instanceof_yes_side_kills_else_branch() {
    // (c) `is_a(Base4, Iface) = Yes` holds for a lower bound too (every descendant
    // implements Iface), so the else-branch is dead and the wrong-typed literal
    // call inside it stays silent — the Yes-side branch-death precision is retained.
    let live = "<?php declare(strict_types=1);
interface Iface {}
class Base4 implements Iface {
    public int $x = 0;
    public function go(): void {
        if ($this instanceof Iface) {} else { takes_string(1); }
    }
}
function takes_string(string $s): void {}
(new Base4())->go();";
    assert_eq!(n(live), 0, "instanceof Yes on a lower bound retained → else dead → silent");

    // Control: drop the `implements` so is_a(Base4, Iface) is a *No* — but Base4 is
    // a lower bound, so the No is demoted to Maybe → the else-branch stays LIVE and
    // the wrong-typed call fires. (This is exactly the FP the No-side gate prevents
    // when the else is the *reported* side; here it proves the branch is not killed.)
    let control = "<?php declare(strict_types=1);
interface Iface {}
class Base4 {
    public int $x = 0;
    public function go(): void {
        if ($this instanceof Iface) {} else { takes_string(1); }
    }
}
function takes_string(string $s): void {}
(new Base4())->go();";
    assert_eq!(n(control), 1, "instanceof No on a lower bound → Maybe → else live → call flagged");
}

#[test]
fn retention_final_method_on_aliased_this_still_fires() {
    // The guarded-dispatch fallback still resolves a `final` method (it cannot be
    // overridden), so a genuine arg error through a laundered alias still fires.
    let src = "<?php declare(strict_types=1);
class Base5 {
    public int $x = 0;
    public function go(): void { $u = $this; $u->m(\"abc\"); }
    final public function m(int $w): void {}
}
(new Base5())->go();";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "final m resolves under the guard → fires");
}

// ==========================================================================
// 3. Alias shares the bit; clone copies the bit.
// ==========================================================================

#[test]
fn alias_shares_inexact_bit_silent() {
    // `$u = $this` shares the heap object → the lower-bound bit rides along →
    // No-side acceptance on `$u` is gated → silent.
    let src = "<?php declare(strict_types=1);
class Node6 {
    public int $depth = 0;
    public function register(): void { $u = $this; add_leaf($u); }
}
class Leaf6 extends Node6 {}
function add_leaf(Leaf6 $l): void {}
(new Node6())->register();";
    assert_eq!(n(src), 0, "aliased lower-bound $this stays inexact → silent");
}

#[test]
fn alias_shares_exact_bit_fires() {
    // `$u = $x` where `$x = new Robot()` shares the EXACT bit → No-side fires.
    let src = "<?php declare(strict_types=1);
final class User7 {}
final class Robot7 {}
function f(User7 $u): void {}
$x = new Robot7();
$u = $x;
f($u);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "aliased exact object stays exact → fires");
}

#[test]
fn clone_copies_inexact_bit_silent() {
    // `clone $this` copies the object (and its lower-bound bit) into a fresh id.
    let src = "<?php declare(strict_types=1);
class Node8 {
    public int $depth = 0;
    public function register(): void { $u = clone $this; add_leaf($u); }
}
class Leaf8 extends Node8 {}
function add_leaf(Leaf8 $l): void {}
(new Node8())->register();";
    assert_eq!(n(src), 0, "clone of a lower-bound $this stays inexact → silent");
}

#[test]
fn clone_copies_exact_bit_fires() {
    // `clone $x` of an allocation-proven object stays exact → No-side fires.
    let src = "<?php declare(strict_types=1);
final class User9 {}
final class Robot9 {}
function f(User9 $u): void {}
$x = new Robot9();
$u = clone $x;
f($u);";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "clone of an exact object stays exact → fires");
}
