//! The inaccessible-member family — `call.inaccessible-method`,
//! `property.inaccessible`, `class-const.inaccessible` (ADR-0078, issue #185).
//! Positive twin of the absence family: the member is *there*, visibility hides
//! it (`private_blocked` only suppressed; these ids consume it). Every claim
//! below is `php -r`-witnessed on PHP 8.5.9. Load-bearing ones:
//!
//! ```text
//! 1. private method, out-of-scope call        -> private method C::m(), global scope
//! 2. private method, subclass call            -> A::m() from scope B (NOT inherited)
//! 3. protected method, unrelated scope        -> protected method A::m() from scope U
//! 4. protected method, subclass call          -> ok (protected IS inherited)
//! 5. private static method, global call       -> private method C::m(), global scope
//! 6. private constructor, global `new`        -> private C::__construct(), global scope
//! 7. private ctor + __call, global `new`      -> same fatal (ctor has no magic fallback)
//! 8. private property, out-of-scope read      -> private property C::$p
//! 9. private property, out-of-scope write     -> private property C::$p (writes too)
//! 10. private property, inherited access      -> Undefined B::$p (absence, not this id)
//! 11. protected property, inherited access    -> protected property B::$p (inherited)
//! 12. private constant, global fetch          -> private constant C::K
//! 13. private constant, inherited fetch       -> Undefined B::K (absence, not this id)
//! 14. protected constant, inherited fetch     -> protected constant B::K
//! ```
//! Magic-fallback witnesses (routed the same as an undefined member):
//!
//! ```text
//! private method + own __call        -> __call:m (no error at all)
//! private static + __callStatic      -> __callStatic:m
//! private property + __get (read)    -> __get:p9
//! private property + __set (write)   -> __set:p=5
//! private method + ancestor __call   -> __call (fallback counts anywhere in the chain)
//! private constant + __get           -> still fatal (constants have NO magic leg)
//! ```
//! A receiver that is only a *lower* bound is why `$this`/`self::`/`static::`/
//! `parent::` are silent here:
//!
//! ```text
//! $this + descendant's public override  -> rescues the very same site
//! $this + descendant's magic fallback   -> rescues it too
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

#[test]
fn private_instance_method_from_global_scope_fires() {
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
    let src = "<?php
class C { private function m(): void {} }
function f(): void { (new C())->m(); }
";
    assert_eq!(method(src).len(), 1);
}

#[test]
fn private_static_method_fires() {
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
    // Checked at FCC CREATION (true positive); the form just doesn't lower to a call.
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
    // `?->` short-circuits on `null` only; this receiver is never null (allocation-proven).
    let src = "<?php
class C { private function m(): void {} }
function f(): void { $c = new C(); $c?->m(); }
";
    assert_eq!(method(src).len(), 1);
}

#[test]
fn private_method_from_its_own_class_is_silent() {
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
    let src = "<?php
class A { protected function m(): void {} }
class B extends A { public function go(): void { $b = new B(); $b->m(); } }
";
    assert!(method(src).is_empty(), "a subclass scope sees a protected member");
}

#[test]
fn protected_method_from_a_superclass_scope_is_silent() {
    let src = "<?php
class A { public function go(): void { $b = new B(); $b->m(); } }
class B extends A { protected function m(): void {} }
";
    assert!(method(src).is_empty(), "an ancestor scope sees a descendant's protected member");
}

#[test]
fn protected_method_from_a_sibling_scope_is_silent() {
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
    let own = "<?php
class C {
    private function m(): void {}
    public function __call(string $n, array $a): void {}
}
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(own).is_empty(), "__call on the class itself swallows the call");

    let inherited = "<?php
class P { public function __call(string $n, array $a): void {} }
class C extends P { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(inherited).is_empty(), "an inherited __call swallows it too");
}

#[test]
fn magic_call_static_silences_the_static_lane() {
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
    // A constructor IS looked up through the chain (property/constant would be absence).
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
    let src = "<?php
class C {
    private function __construct() {}
    public static function make(): C { return new C(); }
}
";
    assert!(method(src).is_empty());

    let inherited = "<?php
class A { protected function __construct() {} }
class B extends A { public static function make(): B { return new B(); } }
";
    assert!(method(inherited).is_empty());
}

#[test]
fn an_uninstantiable_class_keeps_its_own_fatal() {
    // "Cannot instantiate abstract class A" fires BEFORE any visibility check (#183).
    let src = "<?php
abstract class A { private function __construct() {} }
function f(): void { $a = new A(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn magic_call_does_not_silence_a_private_constructor() {
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
    // ADR-0049 A14 (#195): `@method` says members live where the index can't enumerate them.
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
    // A trait's members aren't flattened in, so the walk can't see a public `m()` or `__call`.
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
    let src = "<?php
class C extends \\Vendor\\Base { private function m(): void {} }
function f(): void { $c = new C(); $c->m(); }
";
    assert!(method(src).is_empty(), "an unenumerable hierarchy declines the claim");
}

#[test]
fn a_this_receiver_is_silent() {
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
    // `C::m()` on non-static is an instance call on `$this`, never this id's fatal.
    let src = "<?php
class C { private function m(): void {} }
function f(): void { C::m(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn an_inexact_receiver_is_silent() {
    // A declared receiver has no exact class — a descendant could override publicly (#196).
    let src = "<?php
class C { private function m(): void {} }
function f(C $c): void { $c->m(); }
";
    assert!(method(src).is_empty());
}

#[test]
fn private_property_read_fires() {
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
    let src = "<?php
class C { protected int $p = 1; }
function f(): int { $c = new C(); $v = $c->p; return $v; }
";
    assert_eq!(property(src).len(), 1);
}

#[test]
fn inherited_protected_property_fires() {
    let src = "<?php
class A { protected int $p = 1; }
class B extends A {}
function f(): int { $b = new B(); $v = $b->p; return $v; }
";
    assert_eq!(property(src).len(), 1);
}

#[test]
fn promoted_private_property_fires() {
    // A promoted ctor param is a property like any other — same fatal.
    let src = "<?php
class C { public function __construct(private readonly int $x = 1) {} }
function f(): int { $c = new C(); $v = $c->x; return $v; }
";
    assert_eq!(property(src).len(), 1);
}

#[test]
fn inherited_private_property_is_silent() {
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
    let src = "<?php
class A { protected int $p = 1; }
class B extends A { public function go(): int { $b = new B(); $v = $b->p; return $v; } }
";
    assert!(property(src).is_empty());
}

#[test]
fn magic_get_is_silent_on_a_read() {
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
    // A hooked override can't be convicted from an unlowered declaration.
    let overriding = "<?php
class A { protected int $p = 1; }
class B extends A { public int $p { get => 42; } }
function f(): int { $b = new B(); $v = $b->p; return $v; }
";
    assert!(property(overriding).is_empty(), "a hooked override declines the claim");

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

#[test]
fn private_class_constant_fires() {
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
    // Visibility records EVERY constant, even ones the value list drops (ADR-0043 §2).
    let src = "<?php
class C { private const K = [1, 2][0] + 1; }
function f(): int { $v = C::K; return $v; }
";
    assert_eq!(class_const(src).len(), 1);
}

#[test]
fn protected_class_constant_fires_from_an_unrelated_scope() {
    let src = "<?php
class A { protected const K = 1; }
class U { public function go(): int { $v = A::K; return $v; } }
";
    assert_eq!(class_const(src).len(), 1);
}

#[test]
fn inherited_protected_class_constant_fires() {
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
    let src = "<?php
class A { private const K = 1; }
class B extends A {}
function f(): int { $v = B::K; return $v; }
";
    assert!(class_const(src).is_empty());
}

#[test]
fn the_declaring_class_named_directly_fires_from_a_subclass_scope() {
    // Naming the declaring class directly is inaccessible, unlike naming the subclass.
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
    // `Suit::Hearts` is syntactically a constant fetch, but enum cases are always public.
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
    // `static::K` is late-bound (ADR-0043 §1); `self`/`parent` are related by construction.
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
    // A closure runs in its defining scope, but the walk leaves it `None`, never "global".
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

#[test]
fn each_sentence_quotes_phps_own_wording() {
    // The parenthetical is PHP's verbatim message (ctor lacks `method`, others lack scope).
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
    // A2i: a guarded declaration beside a dynamism site leaves load order to decide binding.
    let dammed = "<?php
if (!class_exists('C')) {
    class C { private function m(): void {} }
}
function f(string $n): void { eval($n); $c = new C(); $c->m(); }
";
    assert!(method(dammed).is_empty(), "a conditional declaration under a live dam is silent");

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
