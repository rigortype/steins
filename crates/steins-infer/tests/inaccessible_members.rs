//! The inaccessible-member family — `call.inaccessible-method`,
//! `property.inaccessible`, `class-const.inaccessible` (ADR-0078, issue #185).
//!
//! The positive twin of the absence family: the member is *there*, and its declared
//! visibility hides it from the site's scope. The predicate already existed —
//! `private_blocked` — and was used only to suppress; these ids are its consumer.
//!
//! Every runtime claim below is `php -r`-witnessed on PHP 8.5.9 (the sandbox's
//! `php`) and quoted at the test that consumes it. The load-bearing ones:
//!
//! ```text
//! $ php -r 'class C { private function m(){} } (new C)->m();'
//!       Uncaught Error: Call to private method C::m() from global scope
//! $ php -r 'class A { private function m(){} } class B extends A { function go(){ $this->m(); } } (new B)->go();'
//!       Uncaught Error: Call to private method A::m() from scope B     (private is NOT inherited)
//! $ php -r 'class A { protected function m(){} } class U { function go(A $a){ $a->m(); } } (new U)->go(new A);'
//!       Uncaught Error: Call to protected method A::m() from scope U
//! $ php -r 'class A { protected function m(){ echo "ok"; } } class B extends A { function go(){ $this->m(); } } (new B)->go();'
//!       ok                                                              (protected IS inherited)
//! $ php -r 'class C { private static function m(){} } C::m();'
//!       Uncaught Error: Call to private method C::m() from global scope
//! $ php -r 'class C { private function __construct(){} } new C();'
//!       Uncaught Error: Call to private C::__construct() from global scope
//! $ php -r 'class C { private function __construct(){} public function __call($n,$a){} } new C();'
//!       … the same fatal — the magic fallback does NOT rescue a constructor
//! $ php -r 'class C { private $p = 1; } $c = new C; echo $c->p;'
//!       Uncaught Error: Cannot access private property C::$p
//! $ php -r 'class C { private $p = 1; } $c = new C; $c->p = 2;'
//!       Uncaught Error: Cannot access private property C::$p           (writes too)
//! $ php -r 'class A { private $p = 1; } class B extends A {} echo (new B)->p;'
//!       Warning: Undefined property: B::$p                             (absence, NOT this id)
//! $ php -r 'class A { protected $p = 1; } class B extends A {} echo (new B)->p;'
//!       Uncaught Error: Cannot access protected property B::$p         (protected IS inherited)
//! $ php -r 'class C { private const K = 1; } echo C::K;'
//!       Uncaught Error: Cannot access private constant C::K
//! $ php -r 'class A { private const K = 1; } class B extends A {} echo B::K;'
//!       Uncaught Error: Undefined constant B::K                        (absence, NOT this id)
//! $ php -r 'class A { protected const K = 1; } class B extends A {} echo B::K;'
//!       Uncaught Error: Cannot access protected constant B::K
//! ```
//!
//! And the magic-fallback witnesses, which are the whole reason this slice is not a
//! formality — PHP routes an *inaccessible* member through the very same fallback it
//! routes an undefined one through:
//!
//! ```text
//! $ php -r 'class C { private function m(){ echo "private"; } public function __call($n,$a){ echo "__call:$n"; } } (new C)->m();'
//!       __call:m                                                       (no error at all)
//! $ php -r 'class C { private static function m(){} public static function __callStatic($n,$a){ echo "__callStatic:$n"; } } C::m();'
//!       __callStatic:m
//! $ php -r 'class C { private $p = 1; public function __get($n){ echo "__get:$n"; return 9; } } echo (new C)->p;'
//!       __get:p9
//! $ php -r 'class C { private $p = 1; public function __set($n,$v){ echo "__set:$n=$v"; } } $c = new C; $c->p = 5;'
//!       __set:p=5
//! $ php -r 'class P { public function __call($n,$a){ echo "__call"; } } class C extends P { private function m(){} } (new C)->m();'
//!       __call                                                         (anywhere in the chain)
//! $ php -r 'class C { private const K = 1; public function __get($n){ return 7; } } echo C::K;'
//!       Uncaught Error: Cannot access private constant C::K             (constants have NO magic leg)
//! ```
//!
//! Two witnesses bound the reach, and both are about a receiver that is only a
//! *lower* bound — the reason `$this` / `self::` / `static::` / `parent::` are
//! silent here:
//!
//! ```text
//! $ php -r 'class A { private function m(){ echo "A"; } } class B extends A { function go(){ $this->m(); } } class C extends B { public function m(){ echo "C"; } } (new C)->go();'
//!       C            — a descendant's public override rescues the very same site
//! $ php -r '… class C extends B { public function __call($n,$a){ echo "__call"; } } (new C)->go();'
//!       __call       — and so does a descendant's magic fallback
//! ```

use steins_infer::{
    CALL_INACCESSIBLE_METHOD_ID, CLASS_CONST_INACCESSIBLE_ID, Diagnostic,
    PROPERTY_INACCESSIBLE_ID, check,
};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn method(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == CALL_INACCESSIBLE_METHOD_ID).collect()
}

fn property(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == PROPERTY_INACCESSIBLE_ID).collect()
}

fn class_const(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == CLASS_CONST_INACCESSIBLE_ID).collect()
}

// --- call.inaccessible-method: the firing shapes ---------------------------

#[test]
fn private_instance_method_from_global_scope_fires() {
    // `php -r 'class C { private function m(){} } (new C)->m();'`
    //     → Call to private method C::m() from global scope
    let src = "<?php
class C { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    let d = method(src);
    assert_eq!(d.len(), 1, "an out-of-scope private call is a fatal: {d:#?}");
    assert!(d[0].message.contains("private method C::m()"), "{d:#?}");
    assert!(d[0].message.contains("global scope"), "the message names the site's scope: {d:#?}");
}

#[test]
fn private_method_on_new_receiver_fires() {
    // The `new`-typed receiver lane, with no intervening variable.
    let src = "<?php
class C { private function m(): void {} }
function f(): void { (new C())->m(); }
";
    assert_eq!(method(src).len(), 1);
}

#[test]
fn private_static_method_fires() {
    // `php -r 'class C { private static function m(){} } C::m();'`
    //     → Call to private method C::m() from global scope
    let src = "<?php
class C { private static function m(): void {} }
function f(): void { C::m(); }
";
    let d = method(src);
    assert_eq!(d.len(), 1, "the explicit `C::m()` lane fires too: {d:#?}");
    assert!(d[0].message.contains("private method C::m()"), "{d:#?}");
}

#[test]
fn private_static_method_named_through_a_subclass_fires() {
    // `php -r 'class C { private static function m(){} } class D extends C {} D::m();'`
    //     → Call to private method C::m() from global scope — the declaring class is
    // named in the sentence, not the written one.
    let src = "<?php
class C { private static function m(): void {} }
class D extends C {}
function f(): void { D::m(); }
";
    let d = method(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("C::m()"), "the DECLARING class is named: {d:#?}");
}

#[test]
fn protected_method_from_an_unrelated_scope_fires() {
    // `php -r 'class A { protected function m(){} } class U { function go(A $a){ $a->m(); } } (new U)->go(new A);'`
    //     → Call to protected method A::m() from scope U
    let src = "<?php
class A { protected function m(): void {} }
class U { public function go(): void { $a = new A(); $a->m(); } }
";
    let d = method(src);
    assert_eq!(d.len(), 1, "an unrelated class scope cannot see protected: {d:#?}");
    assert!(d[0].message.contains("protected method A::m()"), "{d:#?}");
    assert!(d[0].message.contains("scope U"), "{d:#?}");
}

#[test]
fn private_method_of_an_ancestor_fires_from_a_subclass_scope() {
    // Private is NOT inherited — the sharp case:
    // `php -r 'class A { private function m(){} } class B extends A { function go(){ $this->m(); } } (new B)->go();'`
    //     → Call to private method A::m() from scope B
    // Spelled here on an allocation-proven receiver, because a `$this` receiver is a
    // lower bound (see the module header's rescue witnesses) and stays silent.
    let src = "<?php
class A { private function m(): void {} }
class B extends A { public function go(): void { $b = new B(); $b->m(); } }
";
    let d = method(src);
    assert_eq!(d.len(), 1, "a subclass scope is outside a private member's scope: {d:#?}");
    assert!(d[0].message.contains("private method A::m()"), "{d:#?}");
    assert!(d[0].message.contains("scope B"), "{d:#?}");
}

#[test]
fn private_constructor_fires() {
    // `php -r 'class C { private function __construct(){} } new C();'`
    //     → Call to private C::__construct() from global scope — the singleton idiom,
    // called from outside.
    let src = "<?php
class C { private function __construct() {} }
function f(): void { $c = new C(); }
";
    let d = method(src);
    assert_eq!(d.len(), 1, "a private constructor is an inaccessible call: {d:#?}");
    assert!(d[0].message.contains("__construct"), "{d:#?}");
}

#[test]
fn first_class_callable_is_a_recorded_boundary() {
    // `php -r 'class C { private function m(){} } $c = new C; $f = $c->m(...);'`
    //     → Call to private method C::m() from global scope — PHP checks
    // accessibility when the closure is CREATED, so this IS a true positive and the
    // check would report it. It stays silent because the FCC form does not lower to
    // a method call in the trace IR at all, in either position. Pinned as a boundary,
    // not as correct behaviour; it joins the lane free when the lowering carries it.
    // (`php -r '… public function __call($n,$a){ echo "__call"; } … $f = $c->m(...); $f();'`
    // prints `__call`, so the obstacle leg would apply unchanged.)
    let stmt = "<?php
class C { private function m(): void {} }
function f(): void { $c = new C(); $c->m(...); }
";
    assert!(method(stmt).is_empty());

    let assigned = "<?php
class C { private function m(): void {} }
function f(): void { $c = new C(); $g = $c->m(...); }
";
    assert!(method(assigned).is_empty());
}

#[test]
fn nullsafe_does_not_excuse_a_private_method() {
    // `php -r 'class C { private function m(){} } $c = new C; $c?->m();'`
    //     → Call to private method C::m() from global scope. `?->` short-circuits on
    // `null` and on nothing else, and this lane's receiver is an allocation-proven
    // object, so it is never null.
    let src = "<?php
class C { private function m(): void {} }
function f(): void { $c = new C(); $c?->m(); }
";
    assert_eq!(method(src).len(), 1);
}

// --- call.inaccessible-method: the silence legs ----------------------------

#[test]
fn private_method_from_its_own_class_is_silent() {
    // `php -r 'class C { private function m(){ echo "ok"; } function go(C $o){ $o->m(); } } (new C)->go(new C);'`
    //     → ok. Visibility is per-CLASS, not per-object: another instance of the
    // same class is reachable.
    let src = "<?php
class C {
    private function m(): void {}
    public function go(): void { $o = new C(); $o->m(); }
}
";
    assert!(method(src).is_empty(), "the declaring class's own scope sees it");
}

#[test]
fn protected_method_from_a_subclass_scope_is_silent() {
    // `php -r 'class A { protected function m(){ echo "ok"; } } class B extends A { function go(){ $this->m(); } } (new B)->go();'`
    //     → ok. Protected IS inherited — this is the leg that makes the protected
    // verdict need the is-a oracle rather than a name comparison.
    let src = "<?php
class A { protected function m(): void {} }
class B extends A { public function go(): void { $b = new B(); $b->m(); } }
";
    assert!(method(src).is_empty(), "a subclass scope sees a protected member");
}

#[test]
fn protected_method_from_a_superclass_scope_is_silent() {
    // `php -r 'class A { function go(B $b){ $b->m(); } } class B extends A { protected function m(){ echo "ok"; } } (new A)->go(new B);'`
    //     → ok. The relation is checked in BOTH directions.
    let src = "<?php
class A { public function go(): void { $b = new B(); $b->m(); } }
class B extends A { protected function m(): void {} }
";
    assert!(method(src).is_empty(), "an ancestor scope sees a descendant's protected member");
}

#[test]
fn protected_method_from_a_sibling_scope_is_silent() {
    // `php -r 'class A { protected function m(){} } class S1 extends A { function go(S2 $o){ $o->m(); } } class S2 extends A {} (new S1)->go(new S2);'`
    //     → no error: `S1` is a descendant of the DECLARING class `A`, which is all
    // PHP asks.
    let src = "<?php
class A { protected function m(): void {} }
class S1 extends A { public function go(): void { $o = new S2(); $o->m(); } }
class S2 extends A {}
";
    assert!(method(src).is_empty(), "a sibling scope shares the declaring class");
}

#[test]
fn public_method_is_silent() {
    let src = "<?php
class C { public function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn magic_call_anywhere_in_the_chain_is_silent() {
    // `php -r 'class C { private function m(){ echo "private"; } public function __call($n,$a){ echo "__call:$n"; } } (new C)->m();'`
    //     → __call:m — PHP routes the INACCESSIBLE call through the magic fallback
    // exactly as it routes an undefined one. No error is raised at all.
    let own = "<?php
class C {
    private function m(): void {}
    public function __call(string $n, array $a): void {}
}
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(own).is_empty(), "__call on the class itself swallows the call");

    // `php -r 'class P { public function __call($n,$a){ echo "__call"; } } class C extends P { private function m(){} } (new C)->m();'`
    //     → __call — an ANCESTOR's fallback counts, which is why the chain must be
    // walked to the root rather than stopped at the declaring class.
    let inherited = "<?php
class P { public function __call(string $n, array $a): void {} }
class C extends P { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(inherited).is_empty(), "an inherited __call swallows it too");
}

#[test]
fn magic_call_static_silences_the_static_lane() {
    // `php -r 'class C { private static function m(){} public static function __callStatic($n,$a){ echo "__callStatic:$n"; } } C::m();'`
    //     → __callStatic:m
    let src = "<?php
class C {
    private static function m(): void {}
    public static function __callStatic(string $n, array $a): void {}
}
function f(): void { C::m(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn an_inherited_private_constructor_fires() {
    // `php -r 'class A { private function __construct(){} } class B extends A {} new B();'`
    //     → Call to private A::__construct() from global scope. A constructor IS
    // looked up through the chain — unlike a private property or constant, where the
    // same shape is absence.
    let src = "<?php
class A { private function __construct() {} }
class B extends A {}
function f(): void { $b = new B(); }
";
    let d = method(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("A::__construct()"), "{d:#?}");
}

#[test]
fn a_constructor_reachable_from_its_own_class_is_silent() {
    // `php -r 'class C { private function __construct(){} static function make(){ return new C(); } } var_dump(C::make());'`
    //     → object(C)#1 — the singleton idiom's own factory, which must stay silent.
    let src = "<?php
class C {
    private function __construct() {}
    public static function make(): C { return new C(); }
}
";
    assert!(method(src).is_empty());

    // `php -r 'class A { protected function __construct(){} } class B extends A { static function make(){ return new B(); } } var_dump(B::make());'`
    //     → object(B)#1 — protected reaches the subclass's own factory.
    let inherited = "<?php
class A { protected function __construct() {} }
class B extends A { public static function make(): B { return new B(); } }
";
    assert!(method(inherited).is_empty());
}

#[test]
fn an_uninstantiable_class_keeps_its_own_fatal() {
    // `php -r 'abstract class A { private function __construct(){} } new A();'`
    //     → Cannot instantiate abstract class A — raised BEFORE any visibility check,
    // so naming this site with this id would misname the consequence (the #183
    // discipline). The interface and enum spellings behave identically.
    let src = "<?php
abstract class A { private function __construct() {} }
function f(): void { $a = new A(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn magic_call_does_not_silence_a_private_constructor() {
    // `php -r 'class C { private function __construct(){} public function __call($n,$a){} } new C();'`
    //     → Call to private C::__construct() from global scope. The constructor has
    // no magic fallback, so the obstacle leg is deliberately absent for it.
    let src = "<?php
class C {
    private function __construct() {}
    public function __call(string $n, array $a): void {}
}
function f(): void { $c = new C(); }
";
    assert_eq!(method(src).len(), 1, "a constructor is not rescued by __call");
}

#[test]
fn magic_tag_in_reach_is_silent() {
    // ADR-0049 A14 (issue #195): a `@method` tag says members live where the index
    // cannot enumerate them — the `__call` verdict, one door earlier.
    let src = "<?php
/**
 * @method void anything()
 */
class C { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(src).is_empty(), "an A14 magic tag in reach declines the claim");
}

#[test]
fn mixin_tag_in_reach_is_silent() {
    let src = "<?php
class Helper { public function h(): void {} }
/**
 * @mixin Helper
 */
class C { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn trait_use_anywhere_in_the_chain_is_silent() {
    // A trait's members are not flattened into the using class, so the walk cannot
    // see whether a trait supplies a public `m()` — or a `__call`.
    let using = "<?php
trait T { public function t(): void {} }
class C { use T; private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(using).is_empty(), "a trait-using receiver is unresolvable");

    let ancestor = "<?php
trait T { public function t(): void {} }
class P { use T; }
class C extends P { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(ancestor).is_empty(), "a trait-using ANCESTOR is unresolvable too");
}

#[test]
fn unresolvable_chain_is_silent() {
    // The parent leaves the project (a vendor/builtin base): it is exactly where a
    // `__call` — or a nearer public declaration — could live.
    let src = "<?php
class C extends \\Vendor\\Base { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(src).is_empty(), "an unenumerable hierarchy declines the claim");
}

#[test]
fn a_this_receiver_is_silent() {
    // The lower-bound receiver, witnessed twice in the module header: a descendant
    // may override `m()` publicly, or carry a `__call`, and both rescue this very
    // site. `self::` / `static::` / `parent::` decline for the same reason.
    let src = "<?php
class A { private function m(): void {} }
class B extends A { public function go(): void { $this->m(); } }
";
    assert!(method(src).is_empty(), "a $this receiver is a lower bound, never exact");

    let statics = "<?php
class A { private function m(): void {} }
class B extends A {
    public function viaSelf(): void { self::m(); }
    public function viaParent(): void { parent::m(); }
    public function viaStatic(): void { static::m(); }
}
";
    assert!(method(statics).is_empty(), "self/parent/static are lower-bound sites too");
}

#[test]
fn a_non_static_method_named_statically_is_silent() {
    // `C::m()` on a non-static method is either an instance call on the enclosing
    // `$this` (the lower-bound case again) or a different fatal entirely
    // (`cannot be called statically`) — never this id's sentence.
    let src = "<?php
class C { private function m(): void {} }
function f(): void { C::m(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn an_inexact_receiver_is_silent() {
    // A declared (non-allocation-proven) receiver carries no exact class, so a
    // descendant could override the member publicly. Widening this lane is #196's.
    let src = "<?php
class C { private function m(): void {} }
function f(C $c): void { $c->m(); }
";
    assert!(method(src).is_empty());
}

// --- property.inaccessible -------------------------------------------------

#[test]
fn private_property_read_fires() {
    // `php -r 'class C { private $p = 1; } $c = new C; echo $c->p;'`
    //     → Cannot access private property C::$p
    let src = "<?php
class C { private int $p = 1; }
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    let d = property(src);
    assert_eq!(d.len(), 1, "an out-of-scope private property read is a fatal: {d:#?}");
    assert!(d[0].message.contains("private property $c->p"), "{d:#?}");
    assert!(d[0].message.contains("read"), "{d:#?}");
}

#[test]
fn private_property_write_fires() {
    // `php -r 'class C { private $p = 1; } $c = new C; $c->p = 2;'`
    //     → Cannot access private property C::$p — writes raise the identical fatal.
    let src = "<?php
class C { private int $p = 1; }
function f(): void { $c = new C(); $c->p = 2; }
";
    let d = property(src);
    assert_eq!(d.len(), 1, "a write is a member access too: {d:#?}");
    assert!(d[0].message.contains("write"), "{d:#?}");
}

#[test]
fn protected_property_read_fires() {
    // `php -r 'class C { protected $p = 1; } $c = new C; echo $c->p;'`
    //     → Cannot access protected property C::$p
    let src = "<?php
class C { protected int $p = 1; }
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert_eq!(property(src).len(), 1);
}

#[test]
fn inherited_protected_property_fires() {
    // `php -r 'class A { protected $p = 1; } class B extends A {} echo (new B)->p;'`
    //     → Cannot access protected property B::$p — protected IS inherited, so the
    // declaration may sit anywhere in the chain.
    let src = "<?php
class A { protected int $p = 1; }
class B extends A {}
function f(): int { $b = new B(); $v = $b->p; return $v; }
";
    assert_eq!(property(src).len(), 1);
}

#[test]
fn promoted_private_property_fires() {
    // `php -r 'class C { public function __construct(private readonly int $x = 1){} } echo (new C)->x;'`
    //     → Cannot access private property C::$x — a promoted ctor param is a
    // property like any other.
    let src = "<?php
class C { public function __construct(private readonly int $x = 1) {} }
function f(): int { $c = new C(); $v = $c->x; return $v; }
";
    assert_eq!(property(src).len(), 1);
}

#[test]
fn inherited_private_property_is_silent() {
    // `php -r 'class A { private $p = 1; } class B extends A {} echo (new B)->p;'`
    //     → Warning: Undefined property: B::$p. A private property is mangled into
    // its declaring class's own slot, so from a `B` instance the name is ABSENT, not
    // inaccessible — a different consequence and a different (future) id.
    let src = "<?php
class A { private int $p = 1; }
class B extends A {}
function f(): int { $b = new B(); $v = $b->p; return $v; }
";
    assert!(property(src).is_empty(), "an ancestor's private property is absence, not this id");
}

#[test]
fn private_property_from_its_own_class_is_silent() {
    let src = "<?php
class C {
    private int $p = 1;
    public function go(): int { $o = new C(); $v = $o->p; return $v; }
}
";
    assert!(property(src).is_empty());
}

#[test]
fn protected_property_from_a_subclass_scope_is_silent() {
    // `php -r 'class A { protected $p = 7; } class B extends A { function go(){ echo $this->p; } } (new B)->go();'`
    //     → 7
    let src = "<?php
class A { protected int $p = 1; }
class B extends A { public function go(): int { $b = new B(); $v = $b->p; return $v; } }
";
    assert!(property(src).is_empty());
}

#[test]
fn magic_get_is_silent_on_a_read() {
    // `php -r 'class C { private $p = 1; public function __get($n){ echo "__get:$n"; return 9; } } echo (new C)->p;'`
    //     → __get:p9 — the inaccessible read is routed, not refused.
    let src = "<?php
class C {
    private int $p = 1;
    public function __get(string $n): int { return 0; }
}
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert!(property(src).is_empty());
}

#[test]
fn magic_set_is_silent_on_a_write() {
    // `php -r 'class C { private $p = 1; public function __set($n,$v){ echo "__set:$n=$v"; } } $c = new C; $c->p = 5;'`
    //     → __set:p=5
    let src = "<?php
class C {
    private int $p = 1;
    public function __set(string $n, int $v): void {}
}
function f(): void { $c = new C(); $c->p = 2; }
";
    assert!(property(src).is_empty());
}

#[test]
fn magic_get_does_not_silence_a_write_and_set_does_not_silence_a_read() {
    // The two fallbacks are direction-specific: `__get` cannot intercept a write.
    let get_only = "<?php
class C {
    private int $p = 1;
    public function __get(string $n): int { return 0; }
}
function f(): void { $c = new C(); $c->p = 2; }
";
    assert_eq!(property(get_only).len(), 1, "__get does not cover a write");

    let set_only = "<?php
class C {
    private int $p = 1;
    public function __set(string $n, int $v): void {}
}
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert_eq!(property(set_only).len(), 1, "__set does not cover a read");
}

#[test]
fn public_property_is_silent() {
    let src = "<?php
class C { public int $p = 1; }
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert!(property(src).is_empty());
}

#[test]
fn a_hooked_property_anywhere_in_the_chain_is_silent() {
    // A hooked declaration OVERRIDES an inherited one:
    // `php -r 'class A { protected int $p = 1; } class B extends A { public int $p { get => 42; } } echo (new B)->p;'`
    //     → 42. Class-body hooked properties are not lowered (they bind no value), so
    // the walk cannot judge one — and must not convict the ancestor's declaration it
    // *can* see. Only the name is recorded, and it is enough to decline.
    let overriding = "<?php
class A { protected int $p = 1; }
class B extends A { public int $p { get => 42; } }
function f(): int { $b = new B(); $v = $b->p; return $v; }
";
    assert!(property(overriding).is_empty(), "a hooked override declines the claim");

    // `php -r 'class C { private int $p { get => 5; } } echo (new C)->p;'`
    //     → Cannot access private property C::$p. A true positive the slice declines
    // for the same reason — a recorded boundary, not a verdict.
    let own = "<?php
class C { private int $p { get => 5; } }
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert!(property(own).is_empty());
}

#[test]
fn property_on_a_trait_using_receiver_is_silent() {
    let src = "<?php
trait T { public function t(): void {} }
class C { use T; private int $p = 1; }
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert!(property(src).is_empty());
}

#[test]
fn property_on_an_unresolvable_chain_is_silent() {
    let src = "<?php
class C extends \\Vendor\\Base { private int $p = 1; }
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert!(property(src).is_empty());
}

#[test]
fn property_on_a_this_receiver_is_silent() {
    let src = "<?php
class A { private int $p = 1; }
class B extends A { public function go(): int { $v = $this->p; return $v; } }
";
    assert!(property(src).is_empty(), "$this is a lower bound, never exact");
}

// --- class-const.inaccessible ----------------------------------------------

#[test]
fn private_class_constant_fires() {
    // `php -r 'class C { private const K = 1; } echo C::K;'`
    //     → Cannot access private constant C::K
    let src = "<?php
class C { private const K = 1; }
function f(): int { $v = C::K; return $v; }
";
    let d = class_const(src);
    assert_eq!(d.len(), 1, "an out-of-scope private constant is a fatal: {d:#?}");
    assert!(d[0].message.contains("private"), "{d:#?}");
    assert!(d[0].message.contains("C::K"), "{d:#?}");
}

#[test]
fn private_class_constant_without_a_literal_value_fires() {
    // The visibility list records EVERY declared constant, including one whose
    // initializer is not a representable literal — the value list (ADR-0043 §2)
    // drops those, and reading visibility off it would have missed this.
    let src = "<?php
class C { private const K = [1, 2][0] + 1; }
function f(): int { $v = C::K; return $v; }
";
    assert_eq!(class_const(src).len(), 1);
}

#[test]
fn protected_class_constant_fires_from_an_unrelated_scope() {
    // `php -r 'class A { protected const K = 1; } class U { function go(){ echo A::K; } } (new U)->go();'`
    //     → Cannot access protected constant A::K
    let src = "<?php
class A { protected const K = 1; }
class U { public function go(): int { $v = A::K; return $v; } }
";
    assert_eq!(class_const(src).len(), 1);
}

#[test]
fn inherited_protected_class_constant_fires() {
    // `php -r 'class A { protected const K = 1; } class B extends A {} echo B::K;'`
    //     → Cannot access protected constant B::K — and the message names the
    // WRITTEN class, as PHP's does.
    let src = "<?php
class A { protected const K = 1; }
class B extends A {}
function f(): int { $v = B::K; return $v; }
";
    let d = class_const(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("B::K"), "the written class is named: {d:#?}");
}

#[test]
fn inherited_private_class_constant_is_silent() {
    // `php -r 'class A { private const K = 1; } class B extends A {} echo B::K;'`
    //     → Undefined constant B::K. A private constant is not inherited, so through
    // the subclass the name is ABSENT — the absence family's business, not this id's.
    let src = "<?php
class A { private const K = 1; }
class B extends A {}
function f(): int { $v = B::K; return $v; }
";
    assert!(class_const(src).is_empty());
}

#[test]
fn the_declaring_class_named_directly_fires_from_a_subclass_scope() {
    // `php -r 'class A { private const K = 1; } class B extends A { static function go(){ echo A::K; } } B::go();'`
    //     → Cannot access private constant A::K — naming the declaring class is
    // inaccessibility even where naming the subclass was absence.
    let src = "<?php
class A { private const K = 1; }
class B extends A { public function go(): int { $v = A::K; return $v; } }
";
    assert_eq!(class_const(src).len(), 1);
}

#[test]
fn private_class_constant_from_its_own_class_is_silent() {
    let src = "<?php
class C {
    private const K = 1;
    public function go(): int { $v = C::K; return $v; }
}
";
    assert!(class_const(src).is_empty());
}

#[test]
fn protected_class_constant_from_a_subclass_scope_is_silent() {
    // `php -r 'class A { protected const K = 3; } class B extends A { static function go(){ echo static::K; } } B::go();'`
    //     → 3
    let src = "<?php
class A { protected const K = 1; }
class B extends A { public function go(): int { $v = B::K; return $v; } }
";
    assert!(class_const(src).is_empty());
}

#[test]
fn public_class_constant_is_silent() {
    let src = "<?php
class C { const K = 1; public const J = 2; }
function f(): int { $v = C::K; $w = C::J; return $v + $w; }
";
    assert!(class_const(src).is_empty());
}

#[test]
fn magic_methods_do_not_silence_a_constant() {
    // `php -r 'class C { private const K = 1; public function __get($n){ return 7; } } echo C::K;'`
    //     → Cannot access private constant C::K, and the `__callStatic` spelling
    // fatals identically. Constants have no magic leg at all.
    let src = "<?php
class C {
    private const K = 1;
    public function __get(string $n): int { return 0; }
    public static function __callStatic(string $n, array $a): void {}
}
function f(): int { $v = C::K; return $v; }
";
    assert_eq!(class_const(src).len(), 1);
}

#[test]
fn an_enum_case_is_never_a_constant_fetch() {
    // `Suit::Hearts` is syntactically identical to a constant fetch; enum cases are
    // always public and live off the constant list, so nothing fires.
    let src = "<?php
enum Suit { case Hearts; case Spades; }
function f(): Suit { $v = Suit::Hearts; return $v; }
";
    assert!(class_const(src).is_empty());
}

#[test]
fn the_class_pseudo_constant_is_silent() {
    let src = "<?php
class C { private const K = 1; }
function f(): string { $v = C::class; return $v; }
";
    assert!(class_const(src).is_empty());
}

#[test]
fn class_constant_on_a_trait_using_or_unresolvable_chain_is_silent() {
    let using = "<?php
trait T { public function t(): void {} }
class C { use T; private const K = 1; }
function f(): int { $v = C::K; return $v; }
";
    assert!(class_const(using).is_empty());

    let unresolvable = "<?php
class C extends \\Vendor\\Base { private const K = 1; }
function f(): int { $v = C::K; return $v; }
";
    assert!(class_const(unresolvable).is_empty());
}

#[test]
fn a_late_bound_class_expression_is_silent() {
    // `static::K` is late-bound (ADR-0043 §1) and `self::`/`parent::` can never
    // produce an inaccessible verdict — the enclosing scope is related by
    // construction.
    let src = "<?php
class A { private const K = 1; protected const J = 2; }
class B extends A {
    public function viaStatic(): int { $v = static::J; return $v; }
    public function viaSelf(): int { $v = self::J; return $v; }
    public function viaParent(): int { $v = parent::J; return $v; }
}
";
    assert!(class_const(src).is_empty());
}

#[test]
fn a_closure_body_is_silent_for_every_id() {
    // A closure declared inside a class method RUNS IN THAT CLASS'S SCOPE:
    // `php -r 'class C { private function m(){ echo "ok"; } function go(){ $f = function(){ $o = new C(); $o->m(); }; $f(); } } (new C)->go();'`
    //     → ok, and the arrow-function and `static function` spellings print their
    // values too. The walk does not thread the enclosing class into a closure scope,
    // so its `None` there means "unknown", not "global scope" — reading it as the
    // latter would convict this legal code. Silence until the scope carries its owner.
    let src = "<?php
class C {
    private const K = 1;
    private int $p = 1;
    private function m(): void {}
    public function go(): void {
        $f = function (): int {
            $o = new C();
            $o->m();
            $v = $o->p;
            $k = C::K;
            return $v + $k;
        };
    }
}
";
    assert!(method(src).is_empty(), "a closure inherits its defining class's scope");
    assert!(property(src).is_empty());
    assert!(class_const(src).is_empty());
}

// --- the family as a whole -------------------------------------------------

#[test]
fn each_sentence_quotes_phps_own_wording() {
    // The parenthetical is PHP's message verbatim, including the two shapes that
    // differ from the obvious one: a constructor carries no `method` word, and the
    // property/constant fatals carry no scope clause.
    let m = "<?php
class C { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert_eq!(
        method(m)[0].message,
        "call to private method C::m() — hierarchy fully enumerated (C), no __call — \
         proven Error (Call to private method C::m() from global scope)"
    );

    let ctor = "<?php
class C { private function __construct() {} }
function f(): void { $c = new C(); }
";
    assert_eq!(
        method(ctor)[0].message,
        "call to private C::__construct() — hierarchy fully enumerated (C), a constructor \
         has no magic fallback — proven Error (Call to private C::__construct() from global scope)"
    );

    let p = "<?php
class C { private int $p = 1; }
function f(): void { $c = new C(); $c->p = 2; }
";
    assert_eq!(
        property(p)[0].message,
        "write of private property $c->p from global scope — declared by C, hierarchy fully \
         enumerated (C), no __set — proven Error (Cannot access private property C::$p)"
    );

    let k = "<?php
class A { protected const K = 1; }
class B extends A {}
function f(): int { $v = B::K; return $v; }
";
    assert_eq!(
        class_const(k)[0].message,
        "fetch of protected class constant B::K from global scope — declared by A, hierarchy \
         fully enumerated (B → A), constants have no magic fallback — proven Error \
         (Cannot access protected constant B::K)"
    );
}

#[test]
fn a_conditional_declaration_re_dams_the_claim() {
    // A2i: `if (!class_exists('C')) { class C { … } }` beside a standing dynamism
    // site leaves WHICH declaration binds to load order — the fallback-stub shape.
    let dammed = "<?php
if (!class_exists('C')) {
    class C { private function m(): void {} }
}
function f(string $n): void { eval($n); $c = new C(); $c->m(); }
";
    assert!(method(dammed).is_empty(), "a conditional declaration under a live dam is silent");

    // With the dam clear the same conditional declaration is the only one there is.
    let clear = "<?php
if (!class_exists('C')) {
    class C { private function m(): void {} }
}
function f(): void { $c = new C(); $c->m(); }
";
    assert_eq!(method(clear).len(), 1, "a clear dam leaves the claim standing");
}

#[test]
fn the_three_ids_are_disjoint_at_a_site() {
    // One fixture exercising all three: each site reports exactly its own id, and
    // no site reports two.
    let src = "<?php
class C {
    private const K = 1;
    private int $p = 1;
    private function m(): void {}
}
function f(): int {
    $c = new C();
    $c->m();
    $v = $c->p;
    $k = C::K;
    return $v + $k;
}
";
    assert_eq!(method(src).len(), 1, "one method site");
    assert_eq!(property(src).len(), 1, "one property site");
    assert_eq!(class_const(src).len(), 1, "one class-constant site");
}
