//! ADR-0049 A14 / issue #195: `@method` / `@property*` / `@mixin` and the
//! `@phpstan-type` pair are read as **silence obstacles**, never as member
//! sources.
//!
//! A class-like carrying one of these anywhere in its resolved reach — parents,
//! interfaces, `@mixin` targets followed transitively — is not enumerable for an
//! absence proof, so both method-absence ladders (S2 `call.undefined-method` and
//! S6 `phpdoc.undefined-method`) go silent on it exactly as they do for `__call`.
//! This can only *remove* findings, so every fixture below is paired against the
//! negative control: the same shape with the tag removed still fires.

use steins_infer::{
    CALL_UNDEFINED_METHOD_ID, Diagnostic, FileUnit, Folder, MagicObstacle,
    PHPDOC_UNDEFINED_METHOD_ID, check_with, magic_obstacles, magic_obstacles_reaching,
};
use steins_phpdoc::MagicTagKind;
use steins_syntax::SourceTree;

/// The boot-surface mock the S2/S6 suites use: the absence family is available and
/// no project class is a boot-surface homonym, so every other ladder leg holds and
/// what these fixtures measure is the A14 leg alone.
struct Boot;

impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[steins_syntax::ArgValue]) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

fn run(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot).into_iter().filter(|d| d.id == id).collect()
}

/// The S2 findings (exact `new`-typed receiver).
fn s2(src: &str) -> Vec<Diagnostic> {
    run(src, CALL_UNDEFINED_METHOD_ID)
}

/// The S6 findings (declared receiver).
fn s6(src: &str) -> Vec<Diagnostic> {
    run(src, PHPDOC_UNDEFINED_METHOD_ID)
}

fn reach(src: &str, fqn: &str) -> Vec<MagicObstacle> {
    let tree = SourceTree::parse(src);
    let units = [FileUnit { path: "test.php", tree: &tree }];
    magic_obstacles_reaching(&units, fqn)
}

// ---------------------------------------------------------------------------
// The negative control comes first: without a tag, every fixture below fires.
// ---------------------------------------------------------------------------

#[test]
fn a_tagless_class_still_fires() {
    // The exact shape every silence fixture uses, minus the docblock. If this ever
    // stops firing, the obstacle leg has over-silenced and the rest of this file
    // proves nothing.
    let d = s2("<?php\nclass Order {}\n(new Order())->anything();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined method Order::anything()"), "{}", d[0].message);
    // A docblock carrying only tags Steins already reads is not an obstacle.
    let d = s2("<?php\n/**\n * A plain class.\n * @template T\n */\nclass Order {}\n(new Order())->anything();\n");
    assert_eq!(d.len(), 1, "a non-magic docblock must not silence: {d:?}");
}

#[test]
fn the_fired_evidence_string_names_the_leg() {
    // A fired sibling asserts the obstacle leg held, so it must say so — otherwise
    // the evidence string claims a closure it never checked.
    let d = s2("<?php\nclass Order {}\n(new Order())->anything();\n");
    assert!(d[0].message.contains("no __call"), "{}", d[0].message);
    assert!(d[0].message.contains("no @method/@property/@mixin"), "{}", d[0].message);
}

// ---------------------------------------------------------------------------
// One fixture per tag kind, on the S2 ladder.
// ---------------------------------------------------------------------------

#[test]
fn method_tag_silences_the_ladder() {
    // `@method` names ONE method; the obstacle is the whole class-like, because
    // enumerating members is exactly what the tag defeats. Subject-granular
    // discharge is the plugin lane's job (A14), not this slice's.
    let d = s2("<?php\n/** @method int foo() */\nclass Order {}\n(new Order())->anything();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn method_tag_with_a_complex_signature_silences_the_ladder() {
    // The tail is never parsed, so generics and a parenthesized callable type must
    // not make the tag unreadable — silence must not depend on the signature.
    let d = s2(
        "<?php\n/** @method Collection<int, string> map(callable(int): string $cb) */\nclass Order {}\n(new Order())->anything();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn every_property_tag_silences_the_ladder() {
    for tag in ["@property int $count", "@property-read ?Foo $foo", "@property-write string $name"] {
        let src = format!("<?php\n/** {tag} */\nclass Order {{}}\n(new Order())->anything();\n");
        assert!(s2(&src).is_empty(), "{tag}");
    }
}

#[test]
fn mixin_tag_silences_the_ladder() {
    let d = s2(
        "<?php\nclass Builder { public function where(): void {} }\n/** @mixin Builder */\nclass Order {}\n(new Order())->anything();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn the_type_alias_pair_silences_the_ladder() {
    for tag in ["@phpstan-type Row array{id: int}", "@psalm-import-type Row from Repo"] {
        let src = format!("<?php\n/** {tag} */\nclass Order {{}}\n(new Order())->anything();\n");
        assert!(s2(&src).is_empty(), "{tag}");
    }
}

// ---------------------------------------------------------------------------
// Reach: parents, interfaces, and `@mixin` targets.
// ---------------------------------------------------------------------------

#[test]
fn a_parent_property_tag_silences_the_child_ladder() {
    // The child spells nothing; the obstacle is inherited through `extends`.
    let d = s2(
        "<?php\n/** @property int $count */\nclass Base {}\nclass Order extends Base {}\n(new Order())->anything();\n",
    );
    assert!(d.is_empty(), "{d:?}");
    // Control: the same two-class chain without the tag fires.
    let d = s2("<?php\nclass Base {}\nclass Order extends Base {}\n(new Order())->anything();\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn an_interface_method_tag_silences_the_implementor_ladder() {
    // The method-chain walk does not enumerate interfaces (an interface defines
    // nothing), but a `@method` tag on one still says the implementors answer
    // names the index cannot list — so the obstacle reach does walk them.
    let d = s2(
        "<?php\n/** @method int foo() */\ninterface Sugared {}\nclass Order implements Sugared {}\n(new Order())->anything();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn mixin_reach_is_transitive() {
    // A `@mixin` target that is itself a `@mixin` chains further: the record set
    // reaching `a` includes the `@method` declared two hops away on `C`.
    let src = "<?php\n/** @mixin B */\nclass A {}\n/** @mixin C */\nclass B {}\n/** @method int zap() */\nclass C {}\n";
    let recs = reach(src, "A");
    assert!(
        recs.iter().any(|r| r.kind == MagicTagKind::Method && r.subject == "zap" && r.class == "c"),
        "{recs:?}"
    );
    // All three hops are recorded, each against its own declaring class-like.
    assert_eq!(recs.len(), 3, "{recs:?}");
    // …and the ladder is silent on the head of that chain.
    let d = s2(&format!("{src}(new A())->anything();\n"));
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_mixin_cycle_terminates_and_silences() {
    // `A` mixes in `B`, `B` mixes back into `A`. The visited set is the guard: the
    // walk must terminate, record both, and silence the ladder.
    let src = "<?php\n/** @mixin B */\nclass A {}\n/** @mixin A */\nclass B {}\n";
    let recs = reach(src, "A");
    assert_eq!(recs.len(), 2, "each class-like is visited exactly once: {recs:?}");
    assert!(recs.iter().all(|r| r.kind == MagicTagKind::Mixin), "{recs:?}");
    let d = s2(&format!("{src}(new A())->anything();\n"));
    assert!(d.is_empty(), "{d:?}");
    // A self-mixin is the degenerate cycle and must behave the same.
    let selfish = "<?php\n/** @mixin A */\nclass A {}\n";
    assert_eq!(reach(selfish, "A").len(), 1);
    assert!(s2(&format!("{selfish}(new A())->anything();\n")).is_empty());
}

#[test]
fn an_unresolvable_mixin_target_is_an_obstacle_not_a_finding() {
    // `@mixin` naming a class the project does not declare is the *most* obstructed
    // case, not the least: nothing can be enumerated about where those members are.
    let d = s2("<?php\n/** @mixin \\Vendor\\Absent\\Builder */\nclass Order {}\n(new Order())->anything();\n");
    assert!(d.is_empty(), "{d:?}");
    let recs = reach("<?php\n/** @mixin \\Vendor\\Absent\\Builder */\nclass Order {}\n", "Order");
    assert_eq!(recs.len(), 1, "{recs:?}");
    assert_eq!(recs[0].mixin_target.as_deref(), Some("Vendor\\Absent\\Builder"));
}

// ---------------------------------------------------------------------------
// The declared-receiver lane (ADR-0049 §8 / S6) honours the same obstacle.
// ---------------------------------------------------------------------------

#[test]
fn the_declared_receiver_lane_honours_the_obstacle() {
    let tagged = "<?php\n/** @method int foo() */\nfinal class Guest { public function guestId(): int { return 1; } }\nfunction f(Guest $g): void { $g->name(); }\n";
    assert!(s6(tagged).is_empty(), "{:?}", s6(tagged));
    // Control: the identical shape without the tag is S6's conformance fixture.
    let plain = "<?php\nfinal class Guest { public function guestId(): int { return 1; } }\nfunction f(Guest $g): void { $g->name(); }\n";
    assert_eq!(s6(plain).len(), 1, "{:?}", s6(plain));
}

#[test]
fn a_descendant_carrying_a_tag_silences_the_declared_arm() {
    // §8's descendant closure: the arm itself is clean, but a subclass the runtime
    // value may actually be carries `@mixin` — the object may answer the call.
    let tagged = "<?php\nclass Guest { public function guestId(): int { return 1; } }\n/** @mixin Sugar */\nclass Sugared extends Guest {}\nclass Sugar { public function name(): string { return 'x'; } }\nfunction f(Guest $g): void { $g->name(); }\n";
    assert!(s6(tagged).is_empty(), "{:?}", s6(tagged));
    // Control: the same hierarchy with a tag-free descendant fires.
    let plain = "<?php\nclass Guest { public function guestId(): int { return 1; } }\nclass Sugared extends Guest {}\nfunction f(Guest $g): void { $g->name(); }\n";
    assert_eq!(s6(plain).len(), 1, "{:?}", s6(plain));
}

// ---------------------------------------------------------------------------
// The record shape itself (A14: per site, with its subject — never a boolean).
// ---------------------------------------------------------------------------

#[test]
fn records_are_per_site_and_carry_their_subject() {
    let tree = SourceTree::parse(
        "<?php\nnamespace App;\nuse Vendor\\Query\\Builder;\n/**\n * @method int foo()\n * @method static self make()\n * @property-read string $name\n * @mixin Builder\n */\nclass Order {}\n",
    );
    let units = [FileUnit { path: "test.php", tree: &tree }];
    let recs = magic_obstacles(&units);
    assert_eq!(recs.len(), 4, "one record per tag site, not one flag per class: {recs:?}");
    assert!(recs.iter().all(|r| r.class == "app\\order"), "{recs:?}");
    let subjects: Vec<(&MagicTagKind, &str)> =
        recs.iter().map(|r| (&r.kind, r.subject.as_str())).collect();
    assert_eq!(
        subjects,
        [
            (&MagicTagKind::Method, "foo"),
            (&MagicTagKind::Method, "make"),
            (&MagicTagKind::PropertyRead, "name"),
            (&MagicTagKind::Mixin, "Builder"),
        ]
    );
    // The `@mixin` subject resolves through the file's `use` imports, exactly as a
    // written `extends` would.
    assert_eq!(recs[3].mixin_target.as_deref(), Some("Vendor\\Query\\Builder"));
}
