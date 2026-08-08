//! ADR-0078 / issue #197: the absence ladder over the remaining member kinds —
//! `property.undefined` (a read of a property nothing declares) and
//! `class-const.undefined` (a fetch of a constant nothing provides).
//!
//! One ladder, applied per member kind. What differs per kind is *what counts as a
//! member source* and *what obstacle hides one*, so this file is organized as a leg
//! table: every silence leg is paired against the negative control that fires with
//! the leg's condition removed, because a silence test that would pass on a check
//! that never fires proves nothing.
//!
//! Every consequence asserted here is `php -r`-witnessed at PHP 8.5.9 and quoted at
//! the test that consumes it.

use std::collections::BTreeMap;

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    CLASS_CONST_UNDEFINED_ID, Diagnostic, Folder, PROPERTY_UNDEFINED_ID, REGISTERED_NOT_YET_EMITTED,
    check_full, check_with,
};
use steins_syntax::SourceTree;

/// The boot-surface mock every absence suite uses: the A9 gate is open (a live,
/// monkey-patch-free sidecar), no project class is a boot-surface homonym, and no
/// PHP-minor skew. So what these fixtures measure is the ladder, never the gate.
struct Boot {
    available: bool,
}

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        None
    }
}

fn findings(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot { available: true })
        .into_iter()
        .filter(|d| d.id == id)
        .collect()
}

/// The `property.undefined` findings under the default `warning-handler = "abort"`
/// posture.
fn props(src: &str) -> Vec<Diagnostic> {
    findings(src, PROPERTY_UNDEFINED_ID)
}

/// The `class-const.undefined` findings.
fn consts(src: &str) -> Vec<Diagnostic> {
    findings(src, CLASS_CONST_UNDEFINED_ID)
}

/// The exact-receiver property findings — the S2 lane, told apart from the
/// declared-receiver lane by the evidence phrasing, the same discriminator a human
/// reader uses.
fn exact_props(src: &str) -> Vec<Diagnostic> {
    props(src).into_iter().filter(|d| !d.message.contains("declared receiver")).collect()
}

/// The declared-receiver property findings — the S6 lane, promoted by A13.
fn declared_props(src: &str) -> Vec<Diagnostic> {
    props(src).into_iter().filter(|d| d.message.contains("declared receiver")).collect()
}

/// Whether the built-in profile `name` surfaces `d`.
fn surfaced(name: &str, d: &Diagnostic) -> bool {
    ProfileConfigs(BTreeMap::new()).resolve(Some(name)).unwrap().is_surfaced(d)
}

// ---------------------------------------------------------------------------
// The negative controls come first. Every silence fixture below is the same shape
// with one leg's condition added, so these two are what make the rest mean
// anything.
// ---------------------------------------------------------------------------

/// `php -r` witness, PHP 8.5.9:
///
/// ```text
/// class C { public int $a = 1; } $c = new C; var_dump($c->nope);
///   Warning: Undefined property: C::$nope
///   NULL
/// ```
const EXACT_FIRES: &str = "<?php
class C { public int $a = 1; }
$c = new C();
$x = $c->nope;
";

/// `php -r` witness, PHP 8.5.9: `class C { const K = 1; } echo C::NOPE;` →
/// `Error: Undefined constant C::NOPE` — a fatal, not a warning.
const CONST_FIRES: &str = "<?php
class C { const K = 1; }
$x = C::NOPE;
";

#[test]
fn an_undefined_property_read_on_a_fully_enumerated_class_fires() {
    let d = exact_props(EXACT_FIRES);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("$c->nope"), "{}", d[0].message);
    // The evidence string quotes PHP's own consequence, verbatim and including the
    // value the program then carries.
    assert!(
        d[0].message.contains("Undefined property: C::$nope"),
        "the message must quote the witnessed warning: {}",
        d[0].message
    );
    assert!(d[0].message.contains("evaluates to null"), "{}", d[0].message);
}

#[test]
fn the_fired_property_evidence_names_every_leg_it_checked() {
    // A fired finding asserts each obstacle leg held, so it has to say which — an
    // evidence string claiming a closure it never checked is the manufactured-FP
    // shape ADR-0002 forbids.
    let d = exact_props(EXACT_FIRES);
    let m = &d[0].message;
    assert!(m.contains("hierarchy fully enumerated (C)"), "{m}");
    assert!(m.contains("no __get/__set/__isset"), "{m}");
    assert!(m.contains("no #[AllowDynamicProperties]"), "{m}");
    assert!(m.contains("no @property/@method/@mixin"), "{m}");
    assert!(m.contains("no dynamic write of `nope` anywhere"), "{m}");
}

#[test]
fn an_undefined_class_constant_fetch_fires() {
    let d = consts(CONST_FIRES);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("C::NOPE"), "{}", d[0].message);
    assert!(
        d[0].message.contains("proven Error (Undefined constant C::NOPE)"),
        "the message must quote the witnessed fatal: {}",
        d[0].message
    );
    assert!(
        d[0].message.contains("constants have no magic fallback"),
        "{}",
        d[0].message
    );
}

// ---------------------------------------------------------------------------
// property.undefined — the member sources. A declared property is silence however
// it is spelled.
// ---------------------------------------------------------------------------

#[test]
fn a_declared_property_is_silent() {
    assert!(props("<?php\nclass C { public int $a = 1; }\n$c = new C();\n$x = $c->a;\n").is_empty());
}

#[test]
fn an_inherited_property_is_silent() {
    // Witnessed: `class Par { public int $ip = 9; } class Chi extends Par {}` —
    // `(new Chi)->ip` prints `int(9)`.
    let src = "<?php
class Par { public int $ip = 9; }
class Chi extends Par {}
$c = new Chi();
$x = $c->ip;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn a_promoted_constructor_property_is_silent() {
    // Witnessed: `class Promo { public function __construct(public int $q = 3) {} }`
    // — `(new Promo)->q` prints `int(3)`.
    let src = "<?php
class Promo { public function __construct(public int $q = 3) {} }
$p = new Promo();
$x = $p->q;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn a_hooked_property_is_declared_and_silent() {
    // The #185 lowering's whole point, reused here: a class-body hooked property
    // binds no value and is NOT lowered to a `PropertyDecl`, so only
    // `ClassDecl::hooked_properties` keeps its name — and a member-absence claim
    // that could not see it would convict legal code. Witnessed:
    // `class Hooked { public int $h { get => 42; } }` — `(new Hooked)->h` prints 42.
    let src = "<?php
class Hooked { public int $h { get => 42; } }
$o = new Hooked();
$x = $o->h;
";
    assert!(props(src).is_empty(), "a hooked property IS declared: {:#?}", props(src));
}

#[test]
fn a_private_ancestor_property_is_silence_not_a_finding() {
    // PHP mangles a private property into its declaring class's slot, so the child
    // really has no such name — witnessed:
    // `class A1 { private $p = 1; } class B1 extends A1 {}` → `(new B1)->p` is
    // `Warning: Undefined property: B1::$p`. v1 under-fires here rather than reason
    // about mangling, and the choice is what keeps this id disjoint from
    // `property.inaccessible` by construction: whenever the name is declared
    // anywhere in the chain this id is silent, and that is exactly where the
    // visibility id lives.
    let src = "<?php
class A1 { private $p = 1; }
class B1 extends A1 {}
$b = new B1();
$x = $b->p;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

// ---------------------------------------------------------------------------
// property.undefined — the obstacle legs.
// ---------------------------------------------------------------------------

#[test]
fn a_get_anywhere_in_the_chain_is_silence() {
    // Witnessed: a class with `__get` prints `__get:nope` and raises nothing.
    let own = "<?php
class C { public function __get($n) { return 1; } }
$c = new C();
$x = $c->nope;
";
    assert!(props(own).is_empty(), "{:#?}", props(own));
    // …and the fallback is INHERITED, which is why the leg is asked at every node.
    // Witnessed: `class GP { public function __get($n){…} } class GC extends GP {}`
    // — `(new GC)->nope` prints `p:nope`.
    let inherited = "<?php
class GP { public function __get($n) { return 1; } }
class GC extends GP {}
$c = new GC();
$x = $c->nope;
";
    assert!(props(inherited).is_empty(), "{:#?}", props(inherited));
}

#[test]
fn set_and_isset_are_obstacles_too_though_neither_rescues_a_read() {
    // The deliberate over-silence, named. Witnessed at 8.5.9: with `__isset` alone,
    // and with `__set` alone, the read still raises
    // `Warning: Undefined property: C::$nope` — so PHP would agree with a finding
    // here. Steins declines anyway: a class declaring any of the three runs the
    // magic-property protocol, and one enumerability rule beats two.
    for magic in ["public function __set($n, $v) {}", "public function __isset($n) { return true; }"]
    {
        let src = format!("<?php\nclass C {{ {magic} }}\n$c = new C();\n$x = $c->nope;\n");
        assert!(props(&src).is_empty(), "`{magic}` must be an obstacle: {:#?}", props(&src));
    }
}

#[test]
fn allow_dynamic_properties_is_silence() {
    // Witnessed: an `#[AllowDynamicProperties]` class's never-written read still
    // warns — so this too is a true positive Steins declines. The attribute
    // re-licenses the write PHP 8.2 deprecated (witnessed: `$x->dyn = 7;` on such a
    // class is silent, while the same write on a plain class is
    // `Deprecated: Creation of dynamic property Plain::$dyn is deprecated`), which
    // leaves the property set open for good.
    let src = "<?php
#[AllowDynamicProperties]
class Adp { public int $a = 1; }
$o = new Adp();
$x = $o->nope;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
    // Inherited: the licence descends with the class.
    let child = "<?php
#[AllowDynamicProperties]
class Adp { public int $a = 1; }
class Kid extends Adp {}
$o = new Kid();
$x = $o->nope;
";
    assert!(props(child).is_empty(), "{:#?}", props(child));
    // The fully-qualified spelling matches too.
    let fq = "<?php
#[\\AllowDynamicProperties]
class Adp2 { public int $a = 1; }
$o = new Adp2();
$x = $o->nope;
";
    assert!(props(fq).is_empty(), "{:#?}", props(fq));
}

#[test]
fn stdclass_and_its_descent_are_silent_for_the_id_entirely() {
    // Witnessed: `(new stdClass)->nope` really does warn, so both of these are true
    // positives — and v1 declines both. `stdClass` is the language's own property
    // bag, and a dynamic property written anywhere would make the read clean; the
    // conservatism costs nothing but these two shapes.
    //
    // Mechanically the leg needs no code of its own: `stdClass` is not a project
    // declaration, so the chain walk's resolve-or-silence edge covers the class and
    // every descendant of it at once.
    let bare = "<?php\n$s = new stdClass();\n$x = $s->nope;\n";
    assert!(props(bare).is_empty(), "{:#?}", props(bare));
    let descent = "<?php
class Bag extends stdClass {}
$s = new Bag();
$x = $s->nope;
";
    assert!(props(descent).is_empty(), "{:#?}", props(descent));
}

#[test]
fn a_magic_tag_obstacle_in_reach_is_silence() {
    // The #195 machinery, reused verbatim — the same records, the same reach walk,
    // the same discharge channel a plugin pack will open member by member. This is
    // the leg that keeps the Eloquent/facade shape (PHPStan's 15,554 sites) silent.
    for tag in ["@property int $anything", "@property-read int $anything", "@mixin Other"] {
        let src = format!(
            "<?php\nclass Other {{}}\n/**\n * {tag}\n */\nclass C {{ public int $a = 1; }}\n$c = new C();\n$x = $c->nope;\n"
        );
        assert!(props(&src).is_empty(), "`{tag}` must silence: {:#?}", props(&src));
    }
    // …and through the parent chain, not only on the class itself.
    let via_parent = "<?php
/**
 * @property int $anything
 */
class Base {}
class C extends Base { public int $a = 1; }
$c = new C();
$x = $c->nope;
";
    assert!(props(via_parent).is_empty(), "{:#?}", props(via_parent));
}

#[test]
fn a_trait_anywhere_in_the_chain_is_silence() {
    // Trait members are not flattened into the using class (S1 / leg (e)), and a
    // trait CAN declare a property — witnessed:
    // `trait TP { public int $tp = 4; } class UT { use TP; }` → `(new UT)->tp` is 4.
    let src = "<?php
trait TP { public int $tp = 4; }
class UT { use TP; }
$u = new UT();
$x = $u->nope;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn an_unresolvable_ancestor_is_silence() {
    // The chain never closes: `Vendor` is not a project declaration, so nothing
    // proves the property is not declared up there.
    let src = "<?php
class C extends Vendor { public int $a = 1; }
$c = new C();
$x = $c->nope;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn an_enum_receiver_is_silence() {
    // `name` and `value` are engine-provided rather than declared, so an enum node
    // would otherwise read as property-empty. Witnessed: `$e->name` / `$e->value`
    // both answer on a backed enum case, while `$e->nope` warns.
    let src = "<?php
enum Suit: string { case H = 'h'; }
$e = Suit::H;
$x = $e->nope;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn a_project_wide_dynamic_write_of_the_name_is_silence() {
    // The leg the method ladder has no analogue for. A dynamic write on a plain
    // class is a DEPRECATION, not an error — witnessed:
    // `$p->dyn = 5;` prints `Deprecated: Creation of dynamic property Plain::$dyn is
    // deprecated` and then `int(5)` reads back clean. So a name written anywhere in
    // the project could have been created on this object before the read, and the
    // absence claim does not hold.
    let src = "<?php
class C { public int $a = 1; }
function seed(C $c): void { $c->nope = 1; }
$c = new C();
$x = $c->nope;
";
    assert!(props(src).is_empty(), "a write of the same name anywhere silences: {:#?}", props(src));
    // A DIFFERENT name being written leaves the claim standing — the obstacle is
    // keyed by name, so it does not collapse into "any project that writes any
    // property is silent".
    let other = "<?php
class C { public int $a = 1; }
function seed(C $c): void { $c->other = 1; }
$c = new C();
$x = $c->nope;
";
    assert_eq!(exact_props(other).len(), 1, "{:#?}", props(other));
    // A COMPUTED-name write could have created any name at all, so one anywhere
    // takes the id off the surface.
    let computed = "<?php
class C { public int $a = 1; }
function seed(C $c, string $n): void { $c->$n = 1; }
$c = new C();
$x = $c->nope;
";
    assert!(props(computed).is_empty(), "{:#?}", props(computed));
}

#[test]
fn a_conditional_declaration_re_dams_the_claim() {
    // A2i: which body binds is left to load order, so the claim fires only when the
    // whole-universe dam is clear — and an `eval` is a dam site.
    let src = "<?php
if (!class_exists('C')) { class C { public int $a = 1; } }
$c = new C();
$x = $c->nope;
eval('$q = 1;');
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn the_a9_sidecar_gate_silences_the_whole_id() {
    // Without a live, monkey-patch-free sidecar the absence family cannot close
    // (ADR-0049 A9), and the id is silent whatever the ladder would say.
    let tree = SourceTree::parse(EXACT_FIRES);
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot { available: false })
        .into_iter()
        .filter(|d| d.id == PROPERTY_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn a_lower_bound_receiver_is_not_this_lane() {
    // `$this` is a membership fact, never exactness (A1): a descendant may declare
    // the property, or carry a `__get`. Silence, and not by accident — the same
    // exactness discipline every member check applies.
    let src = "<?php
class C { public int $a = 1; public function go() { $x = $this->nope; return $x; } }
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

// ---------------------------------------------------------------------------
// property.undefined — the warning-handler gate (ADR-0049 §7).
// ---------------------------------------------------------------------------

#[test]
fn warning_handler_abort_emits_the_property_id() {
    let tree = SourceTree::parse(EXACT_FIRES);
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Boot { available: true }, true)
        .into_iter()
        .filter(|d| d.id == PROPERTY_UNDEFINED_ID)
        .collect();
    assert_eq!(d.len(), 1, "the default \"abort\" posture emits: {d:#?}");
}

#[test]
fn warning_handler_null_demotes_the_property_id() {
    // Under a declared `warning-handler = "null"` the application has said it
    // tolerates `Undefined property`, so this warning-grade finding leaves the proof
    // surface — the `offset.missing` lever, ADR-0049 §7, wired to the same flag and
    // not to a second mechanism.
    let tree = SourceTree::parse(EXACT_FIRES);
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Boot { available: true }, false)
        .into_iter()
        .filter(|d| d.id == PROPERTY_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "the \"null\" posture demotes: {d:#?}");
}

#[test]
fn the_class_constant_id_has_no_gate() {
    // The gate boundary IS the id boundary (ADR-0078 §1.4): an undefined constant is
    // a fatal `Error` and no posture makes it survivable, so it emits under both.
    let tree = SourceTree::parse(CONST_FIRES);
    for posture in [true, false] {
        let d: Vec<Diagnostic> =
            check_full(&tree, "test.php", &mut Boot { available: true }, posture)
                .into_iter()
                .filter(|d| d.id == CLASS_CONST_UNDEFINED_ID)
                .collect();
        assert_eq!(d.len(), 1, "warning_handler_abort={posture}: {d:#?}");
    }
}

#[test]
fn both_ids_are_on_the_default_surface() {
    let p = exact_props(EXACT_FIRES).pop().expect("fires");
    let c = consts(CONST_FIRES).pop().expect("fires");
    for d in [&p, &c] {
        assert!(surfaced("default", d), "proof at the Default floor: {}", d.id);
    }
}

// ---------------------------------------------------------------------------
// property.undefined — the declared-receiver lane (A13 routing).
// ---------------------------------------------------------------------------

#[test]
fn a_native_declared_receiver_fires_the_proof_id() {
    // The S6 lane, promoted by A13: every arm is `Verified` (a native `C $c`
    // parameter PHP enforces at the boundary), so the finding carries the proof id
    // under the declared-receiver ladder — chain closure PLUS descendant closure,
    // since a subclass could declare the property.
    let src = "<?php
final class C { public int $a = 1; }
function viaParam(C $c): void { $x = $c->nope; }
";
    let d = declared_props(src);
    assert_eq!(d.len(), 1, "{:#?}", props(src));
    assert!(d[0].message.contains("declared receiver $c"), "{}", d[0].message);
    assert!(d[0].message.contains("descendants fully enumerated"), "{}", d[0].message);
}

#[test]
fn a_descendant_declaring_the_property_silences_the_declared_lane() {
    // The §8 leg: the runtime object is contract-typed as the arm but may be any
    // subclass, and a subclass that declares the property answers the read.
    let src = "<?php
class C { public int $a = 1; }
class Sub extends C { public int $nope = 2; }
function viaParam(C $c): void { $x = $c->nope; }
";
    assert!(declared_props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn an_asserted_arm_is_the_calibration_boundary_not_a_finding() {
    // A13 routes an Asserted method claim to `phpdoc.undefined-method`. The property
    // family has no phpdoc twin to route to (ADR-0078's floor table registers none),
    // so a docblock-premised property absence gets NO id in v1 rather than being
    // laundered onto the proof surface. Recorded here as the boundary it is: the
    // twin, if measurement ever asks for one, is a registry addition.
    let src = "<?php
final class C { public int $a = 1; }
/** @param C $c */
function viaDocParam($c): void { $x = $c->nope; }
";
    assert!(props(src).is_empty(), "an Asserted arm is silence: {:#?}", props(src));
    // The negative control: the same shape with a NATIVE declaration does fire, so
    // the silence above is the stratum gate and not a broken fixture.
    let native = "<?php
final class C { public int $a = 1; }
function viaParam(C $c): void { $x = $c->nope; }
";
    assert_eq!(declared_props(native).len(), 1, "{:#?}", props(native));
}

#[test]
fn one_property_read_is_judged_by_exactly_one_lane() {
    // The disjointness invariant is over SITES, not ids: the exact lane owns
    // `class_exact` receivers, the declared lane requires the receiver NOT be exact.
    // A site is never judged twice.
    for src in [
        EXACT_FIRES,
        "<?php\nfinal class C { public int $a = 1; }\nfunction viaParam(C $c): void { $x = $c->nope; }\n",
    ] {
        assert_eq!(props(src).len(), 1, "exactly one finding per site: {:#?}", props(src));
    }
}

// ---------------------------------------------------------------------------
// class-const.undefined — the member sources. The reach is wider than a method's.
// ---------------------------------------------------------------------------

#[test]
fn a_declared_constant_is_silent() {
    assert!(consts("<?php\nclass C { const K = 1; }\n$x = C::K;\n").is_empty());
    // …including one whose initializer is not a literal: `const_visibility` records
    // every declared name, so absence there really does mean "no such constant".
    let computed = "<?php
class C { const J = 'x'; const K = self::J . 'y'; }
$x = C::K;
";
    assert!(consts(computed).is_empty(), "{:#?}", consts(computed));
}

#[test]
fn an_inherited_constant_is_silent() {
    // Witnessed: `class CPar { const PK = 'pk'; } class CChi extends CPar {}` —
    // `CChi::PK` is `"pk"`.
    let src = "<?php
class CPar { const PK = 'pk'; }
class CChi extends CPar {}
$x = CChi::PK;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn an_interface_constant_is_a_member_source() {
    // The reach a method walk does not have. Witnessed:
    // `interface I1 { const IK = 'ik'; } class CImpl implements I1 {}` —
    // `CImpl::IK` is `"ik"`.
    let src = "<?php
interface I1 { const IK = 'ik'; }
class CImpl implements I1 {}
$x = CImpl::IK;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
    // …transitively, through an interface that extends another. Witnessed:
    // `interface IA { const AK = 'ak'; } interface IB extends IA {} class CB
    // implements IB {}` — both `CB::AK` and `IB::AK` are `"ak"`.
    let nested = "<?php
interface IA { const AK = 'ak'; }
interface IB extends IA {}
class CB implements IB {}
$x = CB::AK;
$y = IB::AK;
";
    assert!(consts(nested).is_empty(), "{:#?}", consts(nested));
}

#[test]
fn an_enum_case_is_a_member_source() {
    // Witnessed: `enum Suit { case Hearts; }` — `Suit::Hearts` resolves, while
    // `Suit::Nope` is `Error: Undefined constant Suit::Nope`.
    let cases = "<?php
enum Suit { case Hearts; case Spades; }
$x = Suit::Hearts;
";
    assert!(consts(cases).is_empty(), "{:#?}", consts(cases));
    // An enum's own constants resolve too — an enum reach is enumerable for this
    // member kind, unlike for methods (leg (j)/A3, unlowered).
    let konst = "<?php
enum E2 { const EK = 'ek'; case A; }
$x = E2::EK;
";
    assert!(consts(konst).is_empty(), "{:#?}", consts(konst));
    // …and the undefined case DOES fire, so the silence above is the member source
    // and not a blanket enum obstacle.
    let missing = "<?php
enum Suit { case Hearts; }
$x = Suit::Nope;
";
    assert_eq!(consts(missing).len(), 1, "{:#?}", consts(missing));
}

// ---------------------------------------------------------------------------
// class-const.undefined — the obstacle legs.
// ---------------------------------------------------------------------------

#[test]
fn a_trait_using_class_is_silence_for_constants() {
    // Trait constants (8.2+) answer through the using class — witnessed:
    // `trait T1 { const TK = 'tk'; } class CT { use T1; }` → `CT::TK` is `"tk"`.
    // So a trait-using node is an obstacle, not a node to skip.
    let src = "<?php
trait T1 { const TK = 'tk'; }
class CT { use T1; }
$x = CT::NOPE;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn an_unresolvable_interface_is_silence() {
    // The interface could be where the constant lives, so the reach never closes.
    let src = "<?php
class C implements VendorContract { const K = 1; }
$x = C::NOPE;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn the_class_magic_constant_is_excluded_at_the_site() {
    // `X::class` is a plain string since PHP 8.0 and errors on nothing — witnessed
    // even for a class that does not exist (`TotallyUndefinedClass::class` returns
    // the string). It is never a member fetch.
    assert!(consts("<?php\nclass C {}\n$x = C::class;\n").is_empty());
}

#[test]
fn self_static_and_parent_are_not_subjects() {
    // The reach is `class-const.inaccessible`'s: `self::`/`parent::` resolve in a
    // lexically fixed scope this walk does not thread, and `static::K` is late-bound
    // and unproven (ADR-0043 §1).
    let src = "<?php
class Base { const K = 1; }
class C extends Base {
    public function a() { $x = self::NOPE; return $x; }
    public function b() { $x = static::NOPE; return $x; }
    public function c() { $x = parent::NOPE; return $x; }
}
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn a_magic_tag_obstacle_silences_constants_too() {
    // Pure over-silence, taken deliberately: constants have no magic channel at all
    // (witnessed — a class with both `__get` and `__callStatic` still raises
    // `Error: Undefined constant Magic::NOPE`), so a docblock tag could not make
    // `C::K` resolve. The leg is reused anyway so the codebase carries ONE
    // enumerability rule, the `string.non-stringable` precedent.
    let src = "<?php
/**
 * @property int $anything
 */
class C { const K = 1; }
$x = C::NOPE;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn magic_methods_do_not_rescue_a_constant_fetch() {
    // The other direction of the same witness: `__get`/`__callStatic` are NOT an
    // obstacle for this id, because PHP gives constants no magic channel. The
    // fixture fires, which is what makes the id "the cleanest member in the family".
    let src = "<?php
class Magic {
    public function __get($n) { return 1; }
    public static function __callStatic($n, $a) { return 1; }
}
$x = Magic::NOPE;
";
    assert_eq!(consts(src).len(), 1, "{:#?}", consts(src));
}

#[test]
fn the_a9_sidecar_gate_silences_the_constant_id() {
    let tree = SourceTree::parse(CONST_FIRES);
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot { available: false })
        .into_iter()
        .filter(|d| d.id == CLASS_CONST_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn a_conditional_declaration_re_dams_the_constant_claim() {
    let src = "<?php
if (!class_exists('C')) { class C { const K = 1; } }
$x = C::NOPE;
eval('$q = 1;');
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

// ---------------------------------------------------------------------------
// The `maybe-` sibling (ADR-0078 §1.3).
// ---------------------------------------------------------------------------

#[test]
fn the_maybe_sibling_is_registered_ahead_of_emission() {
    // The convention mechanized: a definite leg never ships without its
    // possibly-grade twin being NAMED. The registry-side assertions (layer, floor,
    // disjointness from `ALL_EMITTABLE_IDS`) live in `tests/registry.rs`; what
    // belongs here is that nothing in this slice emits it.
    assert!(
        REGISTERED_NOT_YET_EMITTED.contains(&"property.maybe-undefined"),
        "the maybe- sibling must be registered with its definite leg"
    );
    for src in [EXACT_FIRES, CONST_FIRES] {
        assert!(
            findings(src, "property.maybe-undefined").is_empty(),
            "no emitter produces the maybe- sibling yet"
        );
    }
}
