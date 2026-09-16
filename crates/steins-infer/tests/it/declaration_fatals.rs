//! ADR-0078 / issue #183: the declaration-incompatibility fatals
//! `class.abstract-unimplemented` and `class.extends-final` — the member-kind
//! port's P5 tracer. Both read the declaration graph only (no flow, no value
//! domain, no receiver) and claim a fatal PHP raises **at class load**.
//!
//! Every runtime claim is `php -r`-witnessed on PHP 8.5.9, condensed to one
//! line per fixture. Harness mirrors `tests/class_undefined.rs` minus its
//! `Boot` mock: these ids consult no sidecar (positive claims about resolved
//! declarations, not absence-of-symbol), pinned by `fires_without_a_sidecar`
//! — `check` runs with `NoFold`, whose absence family is unavailable.

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

// `class.abstract-unimplemented` — firing fixtures.

#[test]
fn fires_on_abstract_parent_method() {
    // witness: Class C contains 1 abstract method … must implement it (B::m)
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
    // Harness runs on `NoFold` (no boot surface) — every firing fixture above is
    // also this leg's evidence: no `absence_family_available` gate, unlike ADR-0049.
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
    // witness: Class C contains 1 abstract method … (I::m)
    let d = unimplemented("<?php\ninterface I { public function m(); }\nclass C implements I {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(I::m)"), "{}", d[0].message);
}

#[test]
fn fires_on_interface_inherited_through_an_interface() {
    // witness: Class C contains 1 abstract method … (J::m) — inherited through I
    let d = unimplemented(
        "<?php\ninterface J { public function m(); }\ninterface I extends J {}\nclass C implements I {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(J::m)"), "{}", d[0].message);
}

#[test]
fn fires_on_grandparent_abstract_method() {
    // witness: Class C contains 1 abstract method … (A::m) — from the grandparent
    let d = unimplemented(
        "<?php\nabstract class A { abstract public function m(); }\nabstract class B extends A {}\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(A::m)"), "{}", d[0].message);
}

#[test]
fn fires_although_call_is_present() {
    // `__call` doesn't discharge an abstract method — witness: (B::m) still fires.
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nclass C extends B { public function __call($n, $a) {} }\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_namespaced_declaration_with_qualified_names() {
    // witness: Class App\C contains 1 abstract method … (App\B::m)
    let d = unimplemented(
        "<?php\nnamespace App;\nabstract class B { abstract public function m(); }\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("class App\\C"), "{}", d[0].message);
    assert!(d[0].message.contains("(App\\B::m)"), "{}", d[0].message);
}

#[test]
fn message_truncates_a_long_method_list() {
    // witness: 4 abstract methods … (B::a, B::b, B::c, ...) — PHP truncates too
    let d = unimplemented(
        "<?php\nabstract class B {\n  abstract public function a();\n  abstract public function b();\n  abstract public function c();\n  abstract public function d();\n}\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("leaves 4 inherited abstract methods"), "{}", d[0].message);
    assert!(d[0].message.contains("(B::a, B::b, B::c, and 1 more)"), "{}", d[0].message);
}

#[test]
fn fires_on_the_proven_requirement_beside_an_unresolvable_interface() {
    // Asymmetry: `Countable` isn't enumerable; interfaces only ADD requirements
    // (no body) so it's DROPPED — parent-proven requirement fires (PHP: 2 methods, we: 1).
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\nclass C extends B implements Countable {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("leaves 1 inherited abstract method"), "{}", d[0].message);
    assert!(d[0].message.contains("(B::m)"), "{}", d[0].message);
}

// `class.abstract-unimplemented` — the silence matrix, one fixture per leg.

#[test]
fn silent_on_an_abstract_class_declaration() {
    // witness: runs clean — an abstract class may carry the requirement forward.
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
    // Enums can't be abstract; their own fatal differs (`Enum E must implement 1
    // abstract method`), and enum members aren't lowered at all (ADR-0043).
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\nenum E implements I { case A; }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_trait_using_class() {
    // witness: runs clean — the trait is a member source Steins does not flatten.
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
    // Dropped-interface's other face: with nothing else proven, an unenumerable
    // interface alone reports nothing — a yield loss, never a false positive.
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
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\nclass P { public function m() {} }\nclass C extends P implements I {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_an_abstract_is_discharged_mid_chain() {
    let d = unimplemented(
        "<?php\nabstract class A { abstract public function m(); }\nabstract class B extends A { public function m() {} }\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_the_abstract_over_concrete_shape() {
    // witness: different fatal — "Cannot make non abstract method A::m() abstract
    // in class B", raised at B's declaration; naming C here would misname it.
    let d = unimplemented(
        "<?php\nclass A { public function m() {} }\nabstract class B extends A { abstract public function m(); }\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_private_implementation() {
    // witness: different fatal — "Access level to C::m() must be public (as in
    // class I)" — the `override.visibility-weakened` family's, not this one's.
    let d = unimplemented(
        "<?php\ninterface I { public function m(); }\nclass C implements I { private function m() {} }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_anonymous_class() {
    // witness: real fatal exists, but `new class` lowers EDGE-ONLY (ADR-0049 A4:
    // parent+implements, no members) — own definitions invisible, claim unfounded.
    let d = unimplemented(
        "<?php\nabstract class B { abstract public function m(); }\n$x = new class extends B {};\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_conditional_declaration_under_a_standing_dam() {
    // A2i: guarded declaration re-dams the claim — binding is load order's business.
    let d = unimplemented(
        "<?php\neval($code);\nif (defined('X')) {\n  abstract class B { abstract public function m(); }\n}\nclass C extends B {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn fires_on_a_conditional_declaration_with_the_dam_clear() {
    // Same fixture without the dynamism site — A2i is about the dam, not conditionality.
    let d = unimplemented(
        "<?php\nif (defined('X')) {\n  abstract class B { abstract public function m(); }\n}\nclass C extends B {}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

// `class.extends-final`.

#[test]
fn fires_on_extends_final() {
    // witness: Class C cannot extend final class F
    let d = extends_final("<?php\nfinal class F {}\nclass C extends F {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("class C cannot extend final class F"), "{}", d[0].message);
    assert!(d[0].message.contains("fatal when the class is loaded"), "{}", d[0].message);
    assert_eq!(d[0].line, 3, "positioned at the `extends` clause: {d:?}");
}

#[test]
fn fires_on_an_abstract_subject() {
    // witness: Class C cannot extend final class F (abstractness is irrelevant)
    let d = extends_final("<?php\nfinal class F {}\nabstract class C extends F {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_an_anonymous_class() {
    // witness: Class F@anonymous cannot extend final class F — no members needed
    // to prove it, so edge-only lowering suffices.
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
    // Enum lowers as implicitly final, but a different fatal: witness "Class C
    // cannot extend enum E".
    let d = extends_final("<?php\nenum E { case A; }\nclass C extends E {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_interface_extends() {
    // witness: different fatal — "I cannot implement F - it is not an interface";
    // `final interface` isn't even parseable.
    let d = extends_final("<?php\nfinal class F {}\ninterface I extends F {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_in_a_dead_branch() {
    // Live-path discipline (ADR-0002/0031): a dead-region declaration never fatals.
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
