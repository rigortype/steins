//! The trait composition rule table (ADR-0105), one test per row, each fixture run
//! on PHP 8.5.11 with the verdict quoted. A row that declines asserts silence for a
//! shape whose class-level twin (no trait) fires, so the silence is the decline and
//! not a lane that never ran. The #940 and #952 l3 witnesses close the file.

use steins_infer::{
    CALL_INACCESSIBLE_METHOD_ID, CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_FUNCTION_ID,
    CALL_UNDEFINED_METHOD_ID, CLASS_ABSTRACT_UNIMPLEMENTED_ID, CLASS_CONST_UNDEFINED_ID,
    DEBUG_TYPE_ID, Diagnostic, OVERRIDE_FINAL_ID, PROP_MISMATCH_ID, PROPERTY_UNDEFINED_ID,
    check_with,
};
use steins_syntax::SourceTree;

use crate::arity::Boot;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot::ready())
}

fn of(src: &str, id: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == id).collect()
}

fn dumped(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// C1: the importer's own member outranks the trait's (PHP: "Too few arguments to
/// function C::m(), 1 passed").
#[test]
fn c1_the_importers_own_member_wins() {
    let src = "<?php\ntrait T { function m() {} }\nclass C { use T; function m($a, $b) {} }\n(new C)->m(1);\n";
    let d = of(src, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("C::m()"), "{d:?}");
}

/// C2: a trait method outranks an inherited one (PHP runs `T::m`; the parent's
/// envelope `int|string` would answer otherwise).
#[test]
fn c2_a_trait_method_outranks_the_parents() {
    let src = r#"<?php
class P { public function m(): int|string { return 1; } }
trait T { public function m(): int { return 1; } }
class C extends P { use T; }
function f(C $c): void { \PHPStan\dumpType($c->m()); }
"#;
    assert_eq!(dumped(src), ["int"]);
}

/// C3: two traits give one concrete name from different declarations — PHP fatals
/// ("Trait method B::m has not been applied as C::m, because of collision with
/// A::m"); the method table declines, so the class twin's `call.undefined-method`
/// stays silent.
#[test]
fn c3_a_collision_declines_the_method_table() {
    let src = "<?php\ntrait A { function m() {} }\ntrait B { function m() {} }\nclass C { use A, B; }\n(new C)->tyop();\n";
    assert!(of(src, CALL_UNDEFINED_METHOD_ID).is_empty());
    let twin = "<?php\ntrait A { function m() {} }\nclass C { use A; }\n(new C)->tyop();\n";
    assert_eq!(of(twin, CALL_UNDEFINED_METHOD_ID).len(), 1);
}

/// C3a: an abstract and a concrete declaration compose to the concrete one (PHP:
/// "Too few arguments to function C::m(), 0 passed").
#[test]
fn c3a_the_concrete_one_displaces_an_abstract_one() {
    let src = "<?php\ntrait A { abstract function m($a); }\ntrait B { function m($a) {} }\nclass C { use A, B; }\n(new C)->m();\n";
    assert_eq!(of(src, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1);
}

/// C3b: two abstract declarations compose to one requirement (PHP: "Class C contains
/// 1 abstract method … (C::m)").
#[test]
fn c3b_two_abstract_declarations_are_one_requirement() {
    let src = "<?php\ntrait A { abstract function m(); }\ntrait B { abstract function m(); }\nclass C { use A, B; }\n";
    let d = of(src, CLASS_ABSTRACT_UNIMPLEMENTED_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(C::m)"), "{d:?}");
}

/// C4: one trait declaration reached twice is one member, no collision (PHP: "Too
/// few arguments to function C::m(), 0 passed").
#[test]
fn c4_a_diamond_is_no_collision() {
    let src = "<?php\ntrait U { function m($a) {} }\ntrait A { use U; }\ntrait B { use U; }\nclass C { use A, B; }\n(new C)->m();\n";
    assert_eq!(of(src, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1);
    let twice = "<?php\ntrait U { function m($a) {} }\nclass C { use U, U; }\n(new C)->m();\n";
    assert_eq!(of(twice, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1);
}

/// C5: an `insteadof` or `as` adaptation declines every table (v1) — PHP runs
/// `B::m` here and the alias `am`.
#[test]
fn c5_an_adaptation_declines() {
    let src = "<?php\ntrait A { function m($a) {} }\ntrait B { function m($a) {} }\nclass C { use A, B { B::m insteadof A; A::m as am; } }\n(new C)->m();\n(new C)->tyop();\n";
    assert!(of(src, CALL_TOO_FEW_ARGUMENTS_ID).is_empty());
    assert!(of(src, CALL_UNDEFINED_METHOD_ID).is_empty());
}

/// C6: a trait's own member shadows what it imports (PHP: "Too few arguments to
/// function C::m(), 0 passed" — `A::m($a)`, not `U::m()`); a `use` cycle declines
/// (PHP: "Trait "B" not found").
#[test]
fn c6_trait_of_trait_composes_recursively() {
    let src = "<?php\ntrait U { function m() {} }\ntrait A { use U; function m($a) {} }\nclass C { use A; }\n(new C)->m();\n";
    assert_eq!(of(src, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1);
    let cycle = "<?php\ntrait A { use B; }\ntrait B { use A; }\nclass C { use A; }\n(new C)->tyop();\n";
    assert!(of(cycle, CALL_UNDEFINED_METHOD_ID).is_empty());
}

/// C7: a used name that is no unique, unconditional trait declines: absent ("Trait
/// "Nope" not found"), under a condition (whichever binds), a class, or declared twice.
#[test]
fn c7_an_unresolvable_trait_declines() {
    for src in [
        "<?php\nclass C { use Nope; }\n(new C)->tyop();\n",
        "<?php\nif (rand(0, 1) < 2) { trait T {} }\nclass C { use T; }\n(new C)->tyop();\n",
        "<?php\nclass T {}\nclass C { use T; }\n(new C)->tyop();\n",
        "<?php\ntrait T {}\ntrait T {}\nclass C { use T; }\n(new C)->tyop();\n",
    ] {
        assert!(of(src, CALL_UNDEFINED_METHOD_ID).is_empty(), "{src}");
    }
}

/// C8: an abstract trait method nothing implements is the importer's requirement,
/// named `Importer::m` — also through a trait of a trait (PHP: "(U::p)").
#[test]
fn c8_an_abstract_trait_method_is_the_importers_requirement() {
    let direct = "<?php\ntrait T { abstract public function p(): int; }\nclass U { use T; }\n";
    let d = of(direct, CLASS_ABSTRACT_UNIMPLEMENTED_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(U::p)"), "{d:?}");
    let nested = "<?php\ntrait V { abstract function p(); }\ntrait T { use V; }\nclass U { use T; }\n";
    assert_eq!(of(nested, CLASS_ABSTRACT_UNIMPLEMENTED_ID).len(), 1);
    let abstract_importer = "<?php\ntrait T { abstract public function p(): int; }\nabstract class U { use T; }\n";
    assert!(of(abstract_importer, CLASS_ABSTRACT_UNIMPLEMENTED_ID).is_empty());
}

/// C8a: an inherited concrete method implements an abstract trait method (PHP
/// loads); with an inherited abstract one, PHP names the parent's (`(P::m)`).
#[test]
fn c8a_an_inherited_method_implements_or_names_the_requirement() {
    let src = "<?php\nclass P { public function p(): int { return 1; } }\ntrait T { abstract public function p(): int; }\nclass U extends P { use T; }\n";
    assert!(of(src, CLASS_ABSTRACT_UNIMPLEMENTED_ID).is_empty());
    let named = "<?php\nabstract class P { abstract public function m(); }\ntrait T { abstract public function m(); }\nclass C extends P { use T; }\n";
    let d = of(named, CLASS_ABSTRACT_UNIMPLEMENTED_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("(P::m)"), "{d:?}");
}

/// C9: a trait method over a `final` parent method — a private one too — is the
/// importer's override (PHP: "Cannot override final method Base::foo()").
#[test]
fn c9_a_trait_method_over_a_final_parent_method() {
    for vis in ["public", "private"] {
        let src = format!(
            "<?php\nclass Base {{ final public function foo(): void {{}} }}\ntrait T {{ {vis} function foo(): void {{}} }}\nclass Child extends Base {{ use T; }}\n"
        );
        let d = of(&src, OVERRIDE_FINAL_ID);
        assert_eq!(d.len(), 1, "{vis}: {d:?}");
        assert!(d[0].message.contains("Child::foo()") && d[0].message.contains("Base::foo()"));
    }
    // A method a trait gives an ancestor is that ancestor's (PHP: "Cannot override
    // final method P::m()").
    let ancestor = "<?php\ntrait T { final public function m() {} }\nclass P { use T; }\nclass C extends P { public function m() {} }\n";
    let d = of(ancestor, OVERRIDE_FINAL_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("P::m()"), "{d:?}");
}

/// C9a: an abstract trait method over a `final` parent method binds to it (PHP loads).
#[test]
fn c9a_an_abstract_trait_method_is_no_override() {
    let src = "<?php\nclass Base { final public function foo(): void {} }\ntrait T { abstract public function foo(): void; }\nclass Child extends Base { use T; }\n";
    assert!(of(src, OVERRIDE_FINAL_ID).is_empty());
}

/// C10: a static member composes as an instance one does (PHP: "Too few arguments
/// to function C::s(), 0 passed").
#[test]
fn c10_static_members_compose_alike() {
    let src = "<?php\ntrait T { static function s(int $a) {} }\nclass C { use T; }\nC::s();\n";
    assert_eq!(of(src, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1);
}

/// C11: a trait property is the importer's (#952 l3, below); two traits declaring
/// one property decline the property table (PHP loads the compatible pair), and so
/// does a trait hooked property (C11a).
#[test]
fn c11_properties_compose_and_a_conflict_declines() {
    let held = "<?php\ntrait T { public int $t = 0; }\nclass Q { use T; }\n$q = new Q();\n$x = $q->nope;\n";
    assert_eq!(of(held, PROPERTY_UNDEFINED_ID).len(), 1);
    let pair = "<?php\ntrait A { public int $t = 0; }\ntrait B { public int $t = 0; }\nclass Q { use A, B; }\n$q = new Q();\n$x = $q->nope;\n";
    assert!(of(pair, PROPERTY_UNDEFINED_ID).is_empty());
    let hooked = "<?php\ntrait T { public int $h { get => 1; } }\nclass Q { use T; }\n$q = new Q();\n$x = $q->nope;\n";
    assert!(of(hooked, PROPERTY_UNDEFINED_ID).is_empty());
}

/// C12: a trait constant is the importer's (PHP 8.2); two traits declaring one
/// decline the constant table (PHP loads the compatible pair: `C::K` is 1).
#[test]
fn c12_constants_compose_and_a_conflict_declines() {
    let held = "<?php\ntrait T { const K = 1; }\nclass C { use T; }\n$a = C::K;\n$b = C::NOPE;\n";
    assert_eq!(of(held, CLASS_CONST_UNDEFINED_ID).len(), 1);
    let pair = "<?php\ntrait A { const K = 1; }\ntrait B { const K = 1; }\nclass C { use A, B; }\n$b = C::NOPE;\n";
    assert!(of(pair, CLASS_CONST_UNDEFINED_ID).is_empty());
}

/// #940 d: a trait method body runs the whole walk (PHP: "Call to undefined
/// function missing()").
#[test]
fn a_trait_method_body_runs_the_walk() {
    let src = "<?php\ntrait T { public function go(): void { missing(); } }\nclass K { use T; }\n(new K)->go();\n";
    assert_eq!(of(src, CALL_UNDEFINED_FUNCTION_ID).len(), 1);
}

/// An enum's and an anonymous class's method bodies run the walk too (PHP: "Call to
/// undefined function missing()" when called).
#[test]
fn enum_and_anonymous_method_bodies_run_the_walk() {
    let src = "<?php\nenum E { case A; public function go(): void { missing(); } }\n\
               $o = new class { public function go(): void { missing(); } };\n";
    assert_eq!(of(src, CALL_UNDEFINED_FUNCTION_ID).len(), 2);
}

/// The open receiver: in a trait method `$this`, `self` and the calling scope are
/// the importer's, which the one walk does not know — every receiver-dependent
/// claim declines (PHP runs each of these clean).
#[test]
fn a_trait_method_body_judges_no_receiver_dependent_claim() {
    for src in [
        "<?php\ntrait T { function f() { $c = new C(); return $c->priv(); } }\nclass C { use T; private function priv() { return 1; } }\n",
        "<?php\ntrait T { function f() { return $this->g(); } }\nclass C { use T; function g() { return 1; } }\n",
        "<?php\ntrait T { function f() { return self::g(); } }\nclass C { use T; static function g() { return 1; } }\n",
    ] {
        let d = findings(src);
        assert!(
            !d.iter().any(|d| d.id == CALL_INACCESSIBLE_METHOD_ID || d.id == CALL_UNDEFINED_METHOD_ID),
            "{src}: {d:?}"
        );
    }
}

/// §2.5: a finding in a trait body names the importer PHP names (`C::f(): Return value
/// must be of type int, string returned`) when one class imports the trait, and the
/// trait when several do.
#[test]
fn a_trait_body_finding_names_its_importer() {
    let one = "<?php\ndeclare(strict_types=1);\ntrait T { public function f(): int { return \"x\"; } }\nclass C { use T; }\n";
    let d = findings(one);
    assert!(d.iter().any(|d| d.message.contains("C::f()")), "{d:#?}");
    let two = "<?php\ndeclare(strict_types=1);\ntrait T { public function f(): int { return \"x\"; } }\nclass C { use T; }\nclass D { use T; }\n";
    let d = findings(two);
    assert!(d.iter().any(|d| d.message.contains("T::f()")), "{d:#?}");
}

/// The effect lane reads a trait-using class's own method as it reads a plain class's
/// (C1): a wrapper over `DateTimeImmutable::createFromFormat` reaches the engine row
/// (ADR-0101 §3.17, the zone and the clock as the upper bound a concrete call keeps) the
/// same way whether or not its class uses a trait, so both grade definite, as the
/// plain-class twin did before this ADR. A string conversion of a `mixed` operand
/// names no receiver and stays the possibly-grade gap in both.
#[test]
fn a_trait_using_wrapper_reaches_the_effect_its_plain_twin_does() {
    let src = "<?php\n\
        class W extends DateTimeImmutable { public static function cff(string $f, string $t): static|false { return parent::createFromFormat($f, $t); } }\n\
        trait Fmt { public function label(): string { return 'w'; } }\n\
        class WT extends DateTimeImmutable { use Fmt; public static function cff(string $f, string $t): static|false { return parent::createFromFormat($f, $t); } }\n\
        class Stamp { use Fmt; public function __toString(): string { return date('Y'); } }\n\
        /** @phpstan-all-methods-pure */\n\
        final class R {\n\
          public static function plain(string $s) { return W::cff('Y-m-d', $s); }\n\
          public static function traited(string $s) { return WT::cff('Y-m-d', $s); }\n\
          public static function text(mixed $s) { return \"{$s}\"; }\n\
        }\n";
    let d = findings(src);
    let line = |id: &str, n: u32| d.iter().filter(|f| f.id == id && f.line == n).count();
    assert_eq!(line(steins_infer::EFFECT_ID, 8), 2, "{d:#?}");
    assert_eq!(line(steins_infer::EFFECT_ID, 9), 2, "{d:#?}");
    assert_eq!(line(steins_infer::EFFECT_ID, 10), 0, "{d:#?}");
    assert_eq!(line(steins_infer::EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID, 10), 1, "{d:#?}");
}

/// #952 l3: a typed property declared in a trait (PHP: "Cannot assign string to
/// property Q::$t of type int").
#[test]
fn a_trait_typed_property_is_judged_on_write() {
    let src = "<?php\ndeclare(strict_types=1);\ntrait T { public int $t = 0; }\nclass Q { use T; }\nfunction l3(): void { $q = new Q(); $q->t = \"x\"; }\n";
    assert_eq!(of(src, PROP_MISMATCH_ID).len(), 1);
}

/// C1 under a declining composition: the class's own typed property is still its own
/// (PHP: "Cannot assign string to property Q::$p of type int").
#[test]
fn an_own_property_survives_a_declining_composition() {
    let src = "<?php\ndeclare(strict_types=1);\ntrait T { function t() {} }\nclass Q { use T { t as u; } public int $p = 0; }\nfunction f(): void { $q = new Q(); $q->p = \"x\"; }\n";
    assert_eq!(of(src, PROP_MISMATCH_ID).len(), 1);
}

/// #940 c: an empty `use` no longer hides the class's own constructor (PHP: "Too
/// few arguments to function C::__construct(), 0 passed"); a constructor a trait
/// gives the class is the class's (C2).
#[test]
fn a_trait_use_leaves_the_constructor_judged() {
    let own = "<?php\ntrait T {}\nclass C { use T; public function __construct(int $a) {} }\nnew C();\n";
    assert_eq!(of(own, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1);
    let imported = "<?php\nclass P { function __construct() {} }\ntrait T { function __construct(int $a) {} }\nclass C extends P { use T; }\nnew C();\n";
    let d = of(imported, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("C::__construct()"), "{d:?}");
}
