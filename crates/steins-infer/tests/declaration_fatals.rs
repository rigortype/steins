//! ADR-0078 / issue #183: the declaration-incompatibility fatals
//! `class.abstract-unimplemented` and `class.extends-final` — the member-kind
//! port's P5 tracer. Both read the declaration graph only (no flow analysis, no
//! value domain, no receiver), and both claim a fatal PHP raises **at class load**.
//!
//! Every runtime claim below is `php -r`-witnessed on PHP 8.5.9 and the witness is
//! quoted at the fixture that rests on it. The harness mirrors
//! `tests/class_undefined.rs`, minus its `Boot` mock: these ids consult no sidecar
//! (they are positive claims about resolved declarations, not absence-of-symbol
//! claims), which `fires_without_a_sidecar` pins — `check` runs with `NoFold`, the
//! folder whose absence family is unavailable.

use steins_infer::{
    CLASS_ABSTRACT_UNIMPLEMENTED_ID, CLASS_EXTENDS_FINAL_ID, Diagnostic, check,
};
use steins_syntax::SourceTree;

fn run(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "test.php").into_iter().filter(|d| d.id == id).collect()
}

fn unimplemented(src: &str) -> Vec<Diagnostic> {
    run(src, CLASS_ABSTRACT_UNIMPLEMENTED_ID)
}

fn extends_final(src: &str) -> Vec<Diagnostic> {
    run(src, CLASS_EXTENDS_FINAL_ID)
}

// ---------------------------------------------------------------------------
// `class.abstract-unimplemented` — firing fixtures.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_abstract_parent_method() {
    // php -r 'abstract class B { abstract public function m(); } class C extends B {}'
    // → Fatal error: Class C contains 1 abstract method and must therefore be
    //   declared abstract or implement the remaining method (B::m)
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("class C leaves 1"), "{}", d[0].message);
    assert!(d[0].message.contains("(B::m)"), "{}", d[0].message);
    assert!(d[0].message.contains("fatal when the class is loaded"), "{}", d[0].message);
    assert_eq!(d[0].line, 3, "positioned at the class declaration: {d:?}");
}

#[test]
fn fires_without_a_sidecar() {
    // The whole harness runs on `NoFold` (no boot surface), so every other firing
    // fixture is also this leg's evidence: the id has no `absence_family_available`
    // gate, unlike the ADR-0049 existence family.
    assert_eq!(
        unimplemented(
            "<?php\nabstract class B { abstract public function m(); }\nclass C extends B {}\n"
        )
        .len(),
        1
    );
}

#[test]
fn fires_on_unimplemented_interface_method() {
    // php -r 'interface I { public function m(); } class C implements I {}'
    // → Fatal error: Class C contains 1 abstract method … (I::m)
    let d = unimplemented("<?php\ninterface I { public function m(); }\nclass C implements I {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(I::m)"), "{}", d[0].message);
}

#[test]
fn fires_on_interface_inherited_through_an_interface() {
    // php -r 'interface J { public function m(); } interface I extends J {} class C implements I {}'
    // → Fatal error: Class C contains 1 abstract method … (J::m)
    let d = unimplemented(
        "<?php\ninterface J { public function m(); }\ninterface I extends J {}\nclass C implements I {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(J::m)"), "{}", d[0].message);
}

#[test]
fn fires_on_grandparent_abstract_method() {
    // php -r 'abstract class A { abstract public function m(); } abstract class B extends A {} class C extends B {}'
    // → Fatal error: Class C contains 1 abstract method … (A::m)
    let d = unimplemented(
        "<?php\nabstract class A { abstract public function m(); }\nabstract class B extends A {}\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(A::m)"), "{}", d[0].message);
}

#[test]
fn fires_although_call_is_present() {
    // `__call` does NOT discharge an abstract method — deliberately not an obstacle:
    // php -r 'abstract class B { abstract public function m(); } class C extends B { public function __call($n,$a) {} }'
    // → Fatal error: Class C contains 1 abstract method … (B::m)
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nclass C extends B { public function __call($n, $a) {} }\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_namespaced_declaration_with_qualified_names() {
    // php -r 'namespace App; abstract class B { abstract public function m(); } class C extends B {}'
    // → Fatal error: Class App\C contains 1 abstract method … (App\B::m)
    let d = unimplemented(
        "<?php\nnamespace App;\nabstract class B { abstract public function m(); }\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("class App\\C"), "{}", d[0].message);
    assert!(d[0].message.contains("(App\\B::m)"), "{}", d[0].message);
}

#[test]
fn message_truncates_a_long_method_list() {
    // php -r 'abstract class B { abstract function a(); abstract function b(); abstract function c(); abstract function d(); } class C extends B {}'
    // → …contains 4 abstract methods… (B::a, B::b, B::c, ...)
    let d = unimplemented(
        "<?php\nabstract class B {\n  abstract public function a();\n  abstract public function b();\n  abstract public function c();\n  abstract public function d();\n}\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("leaves 4 inherited abstract methods"), "{}", d[0].message);
    assert!(d[0].message.contains("(B::a, B::b, B::c, and 1 more)"), "{}", d[0].message);
}

#[test]
fn fires_on_the_proven_requirement_beside_an_unresolvable_interface() {
    // The asymmetry, pinned: `Countable` is not enumerable (no project declaration),
    // and an interface can only ADD requirements — never define a body — so it is
    // DROPPED, and the requirement proven from the enumerable parent still fires.
    // php -r 'abstract class B { abstract public function m(); } class C extends B implements Countable {}'
    // → Fatal error: Class C contains 2 abstract methods … (B::m, Countable::count)
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nclass C extends B implements Countable {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("leaves 1 inherited abstract method"), "{}", d[0].message);
    assert!(d[0].message.contains("(B::m)"), "{}", d[0].message);
}

// ---------------------------------------------------------------------------
// `class.abstract-unimplemented` — the silence matrix, one fixture per leg.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_an_abstract_class_declaration() {
    // php -r 'abstract class B { abstract public function m(); } abstract class C extends B {}'
    // → runs clean: an abstract class may carry the requirement forward.
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nabstract class C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_interface_declaration() {
    let d = unimplemented("<?php\ninterface J { public function m(); }\ninterface I extends J {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_enum_declaration() {
    // An enum cannot be abstract and its own fatal reads differently
    // (php -r 'interface I { public function m(); } enum E implements I { case A; }'
    // → Fatal error: Enum E must implement 1 abstract method (I::m)); enum members
    // are not lowered at all (ADR-0043), so the requirement question is unanswerable.
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\nenum E implements I { case A; }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_trait_using_class() {
    // php -r 'interface I { public function m(); } trait T { public function m() {} } class C implements I { use T; }'
    // → runs clean: the trait is a member source Steins does not flatten.
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\ntrait T { public function m() {} }\nclass C implements I { use T; }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_an_ancestor_uses_a_trait() {
    // The obstacle is per-node over the WHOLE chain, not just the subject.
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\ntrait T { public function m() {} }\nabstract class B implements I { use T; }\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_unresolvable_parent() {
    // The parent is a definition source: `Vendor\Base` could implement `m` itself.
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\nclass C extends \\Vendor\\Base implements I {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_unresolvable_interface_alone() {
    // The dropped-interface decision's other face: with nothing else proven, a class
    // implementing only an unenumerable interface reports nothing (a yield loss the
    // asymmetry accepts — never a false positive).
    let d = unimplemented("<?php\nclass C implements \\Countable {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_ambiguous_parent() {
    // Two declarations of the same FQN: which one binds is load order's business.
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nabstract class B { public function m() {} }\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_the_subject_class_is_implemented() {
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nclass C extends B { public function m() {} }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_an_ancestor_implements_the_requirement() {
    // php -r 'interface I { public function m(); } class P { public function m() {} } class C extends P implements I {}'
    // → runs clean.
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\nclass P { public function m() {} }\nclass C extends P implements I {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_an_abstract_is_discharged_mid_chain() {
    // php -r 'abstract class A { abstract public function m(); } abstract class B extends A { public function m() {} } class C extends B {}'
    // → runs clean.
    let d = unimplemented(
        "<?php\nabstract class A { abstract public function m(); }\nabstract class B extends A { public function m() {} }\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_the_abstract_over_concrete_shape() {
    // php -r 'class A { public function m() {} } abstract class B extends A { abstract public function m(); } class C extends B {}'
    // → Fatal error: Cannot make non abstract method A::m() abstract in class B —
    // a DIFFERENT fatal, raised at B's declaration. Naming C would misname it.
    let d = unimplemented(
        "<?php\nclass A { public function m() {} }\nabstract class B extends A { abstract public function m(); }\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_private_implementation() {
    // php -r 'interface I { public function m(); } class C implements I { private function m() {} }'
    // → Fatal error: Access level to C::m() must be public (as in class I) — the
    // `override.visibility-weakened` family's fatal, not this one's.
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\nclass C implements I { private function m() {} }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_anonymous_class() {
    // php -r 'abstract class B { abstract public function m(); } $x = new class extends B {};'
    // → Fatal error: Class B@anonymous must implement 1 abstract method (B::m) —
    // real, but `new class` lowers EDGE-ONLY (ADR-0049 A4: parent + implements refs,
    // no members), so its own definitions are invisible and the claim is unfounded.
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\n$x = new class extends B {};\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_conditional_declaration_under_a_standing_dam() {
    // A2i: a guarded declaration leaves which definition binds to load order, so a
    // conditional node in the consulted set re-dams the claim.
    let d = unimplemented(
        "<?php\neval($code);\nif (defined('X')) {\n  abstract class B { abstract public function m(); }\n}\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn fires_on_a_conditional_declaration_with_the_dam_clear() {
    // The same fixture without the dynamism site: the A2i leg is about the dam, not
    // about conditionality by itself.
    let d = unimplemented(
        "<?php\nif (defined('X')) {\n  abstract class B { abstract public function m(); }\n}\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

// ---------------------------------------------------------------------------
// `class.extends-final`.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_extends_final() {
    // php -r 'final class F {} class C extends F {}'
    // → Fatal error: Class C cannot extend final class F
    let d = extends_final("<?php\nfinal class F {}\nclass C extends F {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("class C cannot extend final class F"), "{}", d[0].message);
    assert!(d[0].message.contains("fatal when the class is loaded"), "{}", d[0].message);
    assert_eq!(d[0].line, 3, "positioned at the `extends` clause: {d:?}");
}

#[test]
fn fires_on_an_abstract_subject() {
    // php -r 'final class F {} abstract class C extends F {}'
    // → Fatal error: Class C cannot extend final class F (abstractness is irrelevant)
    let d = extends_final("<?php\nfinal class F {}\nabstract class C extends F {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_an_anonymous_class() {
    // php -r 'final class F {} $x = new class extends F {};'
    // → Fatal error: Class F@anonymous cannot extend final class F. No members are
    // needed to prove it, so the edge-only lowering suffices here.
    let d = extends_final("<?php\nfinal class F {}\n$x = new class extends F {};\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("anonymous class cannot extend"), "{}", d[0].message);
}

#[test]
fn fires_on_a_namespaced_final_parent() {
    let d = extends_final("<?php\nnamespace App;\nfinal class F {}\nclass C extends F {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("final class App\\F"), "{}", d[0].message);
}

#[test]
fn silent_on_a_non_final_parent() {
    assert!(extends_final("<?php\nclass F {}\nclass C extends F {}\n").is_empty());
}

#[test]
fn silent_on_an_absent_parent() {
    // Issue #182's `class.undefined` territory; finality is unproven either way.
    assert!(extends_final("<?php\nclass C extends \\Vendor\\Missing {}\n").is_empty());
}

#[test]
fn silent_on_an_ambiguous_parent_name() {
    // Two declarations, one final: which binds is load order's business.
    let d = extends_final("<?php\nfinal class F {}\nclass F {}\nclass C extends F {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_extends_enum() {
    // An enum lowers as implicitly final, but the fatal is a different one:
    // php -r 'enum E { case A; } class C extends E {}'
    // → Fatal error: Class C cannot extend enum E
    let d = extends_final("<?php\nenum E { case A; }\nclass C extends E {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_interface_extends() {
    // php -r 'final class F {} interface I extends F {}'
    // → Fatal error: I cannot implement F - it is not an interface — again a
    // different fatal, and `final interface` is not even parseable.
    let d = extends_final("<?php\nfinal class F {}\ninterface I extends F {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_in_a_dead_branch() {
    // Live-path discipline (ADR-0002/0031): a declaration inside a proven-dead
    // region never loads, so it never fatals.
    let d = extends_final("<?php\nfinal class F {}\nif (false) {\n  class C extends F {}\n}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_conditional_final_parent_under_a_standing_dam() {
    let d = extends_final(
        "<?php\neval($code);\nif (defined('X')) {\n  final class F {}\n}\nclass C extends F {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}
