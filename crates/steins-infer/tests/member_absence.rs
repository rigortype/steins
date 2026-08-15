//! ADR-0078 / issue #197: the absence ladder over `property.undefined` (a read of a
//! property nothing declares) and `class-const.undefined` (a fetch of a constant
//! nothing provides).
//!
//! Organized as a leg table: every silence leg is paired against the negative control
//! that fires with the leg's condition removed — a silence test that would pass on a
//! check that never fires proves nothing. Every consequence is `php -r`-witnessed at
//! PHP 8.5.9, quoted at the test that consumes it.

use std::collections::BTreeMap;

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    CLASS_CONST_UNDEFINED_ID, Diagnostic, Folder, PROPERTY_UNDEFINED_ID, REGISTERED_NOT_YET_EMITTED,
    check_full, check_with,
};
use steins_syntax::SourceTree;

/// The boot-surface mock every absence suite uses: the A9 gate is open (live,
/// monkey-patch-free sidecar), so fixtures measure the ladder, never the gate.
struct Boot {
    available: bool,
}

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
        _strict: bool,
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
/// declared-receiver lane by the evidence phrasing.
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

// Negative controls — what makes every silence fixture below meaningful.

/// Witnessed, PHP 8.5.9: `Undefined property: C::$nope` then `NULL`.
const EXACT_FIRES: &str = "<?php
class C { public int $a = 1; }
$c = new C();
$x = $c->nope;
";

/// Witnessed, PHP 8.5.9: `Error: Undefined constant C::NOPE` — a fatal, not a warning.
const CONST_FIRES: &str = "<?php
class C { const K = 1; }
$x = C::NOPE;
";

#[test]
fn an_undefined_property_read_on_a_fully_enumerated_class_fires() {
    let d = exact_props(EXACT_FIRES);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("$c->nope"), "{}", d[0].message);
    assert!(
        d[0].message.contains("Undefined property: C::$nope"),
        "the message must quote the witnessed warning: {}",
        d[0].message
    );
    assert!(d[0].message.contains("evaluates to null"), "{}", d[0].message);
}

#[test]
fn the_fired_property_evidence_names_every_leg_it_checked() {
    // Each finding must name every leg it checked (ADR-0002: no manufactured-FP evidence).
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

// property.undefined — member sources: any declared property silences, however spelled.

#[test]
fn a_declared_property_is_silent() {
    assert!(props("<?php\nclass C { public int $a = 1; }\n$c = new C();\n$x = $c->a;\n").is_empty());
}

#[test]
fn an_inherited_property_is_silent() {
    // Witnessed: inherited property reads clean (`int(9)`).
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
    // Witnessed: promoted ctor property reads clean (`int(3)`).
    let src = "<?php
class Promo { public function __construct(public int $q = 3) {} }
$p = new Promo();
$x = $p->q;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn a_hooked_property_is_declared_and_silent() {
    // #185: a hooked property isn't lowered to PropertyDecl — only
    // ClassDecl::hooked_properties names it; witnessed: reads 42 clean.
    let src = "<?php
class Hooked { public int $h { get => 42; } }
$o = new Hooked();
$x = $o->h;
";
    assert!(props(src).is_empty(), "a hooked property IS declared: {:#?}", props(src));
}

#[test]
fn a_private_ancestor_property_is_silence_not_a_finding() {
    // PHP mangles a private property into the declaring class's slot, so the child truly
    // lacks it (witnessed: `Undefined property: B1::$p`) — keeps this id disjoint from
    // `property.inaccessible`, whose reach is where the name IS declared somewhere.
    let src = "<?php
class A1 { private $p = 1; }
class B1 extends A1 {}
$b = new B1();
$x = $b->p;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

// property.undefined — obstacle legs.

#[test]
fn a_get_anywhere_in_the_chain_is_silence() {
    // Witnessed: a class with `__get` prints `__get:nope` and raises nothing.
    let own = "<?php
class C { public function __get($n) { return 1; } }
$c = new C();
$x = $c->nope;
";
    assert!(props(own).is_empty(), "{:#?}", props(own));
    // __get is inherited too, so the leg is asked at every chain node (witnessed: `p:nope`).
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
    // Deliberate over-silence: `__set`/`__isset` alone still warn `Undefined property:
    // C::$nope` on read (PHP would agree with a finding), but any of the three magic
    // methods runs the magic-property protocol, and one enumerability rule beats two.
    for magic in ["public function __set($n, $v) {}", "public function __isset($n) { return true; }"]
    {
        let src = format!("<?php\nclass C {{ {magic} }}\n$c = new C();\n$x = $c->nope;\n");
        assert!(props(&src).is_empty(), "`{magic}` must be an obstacle: {:#?}", props(&src));
    }
}

#[test]
fn allow_dynamic_properties_is_silence() {
    // Deliberate over-silence: the never-written read still warns.
    // #[AllowDynamicProperties] re-licenses the write PHP 8.2 deprecated elsewhere
    // (witnessed: silent here vs `Deprecated: Creation of dynamic property Plain::$dyn
    // is deprecated`), leaving the property set open for good.
    let src = "<?php
#[AllowDynamicProperties]
class Adp { public int $a = 1; }
$o = new Adp();
$x = $o->nope;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
    let child = "<?php
#[AllowDynamicProperties]
class Adp { public int $a = 1; }
class Kid extends Adp {}
$o = new Kid();
$x = $o->nope;
";
    assert!(props(child).is_empty(), "{:#?}", props(child));
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
    // Witnessed: `(new stdClass)->nope` warns too — a true positive v1 declines anyway,
    // since stdClass is the language's own property bag and a write anywhere answers
    // clean. No dedicated code needed: it isn't a project declaration, so the chain
    // walk's resolve-or-silence edge covers it and every descendant.
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
    // #195 machinery reused verbatim — the reach walk that keeps the Eloquent/facade
    // shape (PHPStan's 15,554 sites) silent.
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
    // Trait members aren't flattened into the using class (S1 / leg (e)), and a trait
    // CAN declare a property — witnessed: `(new UT)->tp` is 4.
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
    // Vendor isn't a project declaration, so the chain never closes — nothing proves
    // absence up there.
    let src = "<?php
class C extends Vendor { public int $a = 1; }
$c = new C();
$x = $c->nope;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn an_enum_receiver_is_silence() {
    // name/value are engine-provided, not declared, so an enum reads property-empty
    // otherwise (witnessed: `$e->name`/`$e->value` answer; `$e->nope` warns).
    let src = "<?php
enum Suit: string { case H = 'h'; }
$e = Suit::H;
$x = $e->nope;
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn a_project_wide_dynamic_write_of_the_name_is_silence() {
    // No method-ladder analogue: a dynamic write on a plain class is a DEPRECATION, not
    // an error (witnessed: writes then reads back clean), so a write anywhere could apply.
    let src = "<?php
class C { public int $a = 1; }
function seed(C $c): void { $c->nope = 1; }
$c = new C();
$x = $c->nope;
";
    assert!(props(src).is_empty(), "a write of the same name anywhere silences: {:#?}", props(src));
    // A DIFFERENT name written leaves the claim standing — the obstacle is keyed by name.
    let other = "<?php
class C { public int $a = 1; }
function seed(C $c): void { $c->other = 1; }
$c = new C();
$x = $c->nope;
";
    assert_eq!(exact_props(other).len(), 1, "{:#?}", props(other));
    // A COMPUTED-name write could create any name, so it silences the id everywhere.
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
    // A2i: which body binds is load order's business, so the claim fires only with the
    // whole-universe dam clear; eval is a dam site.
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
    // No live sidecar (ADR-0049 A9) ⇒ absence family can't close ⇒ id silent regardless
    // of the ladder.
    let tree = SourceTree::parse(EXACT_FIRES);
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot { available: false })
        .into_iter()
        .filter(|d| d.id == PROPERTY_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn a_lower_bound_receiver_is_not_this_lane() {
    // $this is membership only, never exactness (A1): a descendant may declare the
    // property or carry __get.
    let src = "<?php
class C { public int $a = 1; public function go() { $x = $this->nope; return $x; } }
";
    assert!(props(src).is_empty(), "{:#?}", props(src));
}

// property.undefined — warning-handler gate (ADR-0049 §7).

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
    // A declared warning-handler = "null" tolerates Undefined property, so this
    // warning-grade finding leaves the proof surface (ADR-0049 §7, same flag as `offset.missing`).
    let tree = SourceTree::parse(EXACT_FIRES);
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Boot { available: true }, false)
        .into_iter()
        .filter(|d| d.id == PROPERTY_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "the \"null\" posture demotes: {d:#?}");
}

#[test]
fn the_class_constant_id_has_no_gate() {
    // The gate boundary IS the id boundary (ADR-0078 §1.4): undefined constant is a
    // fatal Error, no posture survives it.
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

// property.undefined — declared-receiver lane (A13 routing).

#[test]
fn a_native_declared_receiver_fires_the_proof_id() {
    // S6 lane, promoted by A13: every arm is Verified (native param), so the finding
    // carries the proof id under chain closure PLUS descendant closure (a subclass could declare it).
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
    // §8 leg: the runtime object may be any subclass of the contract type, and a
    // subclass declaring the property answers the read.
    let src = "<?php
class C { public int $a = 1; }
class Sub extends C { public int $nope = 2; }
function viaParam(C $c): void { $x = $c->nope; }
";
    assert!(declared_props(src).is_empty(), "{:#?}", props(src));
}

#[test]
fn an_asserted_arm_is_the_calibration_boundary_not_a_finding() {
    // A13 routes an Asserted method claim to phpdoc.undefined-method, but the property
    // family has no phpdoc twin registered (ADR-0078's floor table), so a docblock-premised
    // absence gets NO id in v1 rather than being laundered onto the proof surface.
    let src = "<?php
final class C { public int $a = 1; }
/** @param C $c */
function viaDocParam($c): void { $x = $c->nope; }
";
    assert!(props(src).is_empty(), "an Asserted arm is silence: {:#?}", props(src));
    // Negative control: the same shape natively declared DOES fire, so the silence
    // above is the stratum gate, not a broken fixture.
    let native = "<?php
final class C { public int $a = 1; }
function viaParam(C $c): void { $x = $c->nope; }
";
    assert_eq!(declared_props(native).len(), 1, "{:#?}", props(native));
}

#[test]
fn one_property_read_is_judged_by_exactly_one_lane() {
    // Disjointness is over SITES, not ids: the exact lane owns class_exact receivers,
    // the declared lane requires non-exact — never judged twice.
    for src in [
        EXACT_FIRES,
        "<?php\nfinal class C { public int $a = 1; }\nfunction viaParam(C $c): void { $x = $c->nope; }\n",
    ] {
        assert_eq!(props(src).len(), 1, "exactly one finding per site: {:#?}", props(src));
    }
}

// class-const.undefined — member sources (wider reach than a method's).

#[test]
fn a_declared_constant_is_silent() {
    assert!(consts("<?php\nclass C { const K = 1; }\n$x = C::K;\n").is_empty());
    // …including a non-literal initializer: const_visibility records every declared
    // name, so absence here really means none exists.
    let computed = "<?php
class C { const J = 'x'; const K = self::J . 'y'; }
$x = C::K;
";
    assert!(consts(computed).is_empty(), "{:#?}", consts(computed));
}

#[test]
fn an_inherited_constant_is_silent() {
    // Witnessed: `CChi::PK` is "pk".
    let src = "<?php
class CPar { const PK = 'pk'; }
class CChi extends CPar {}
$x = CChi::PK;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn an_interface_constant_is_a_member_source() {
    // Reach a method walk lacks. Witnessed: `CImpl::IK` is "ik".
    let src = "<?php
interface I1 { const IK = 'ik'; }
class CImpl implements I1 {}
$x = CImpl::IK;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
    // …transitively through an interface extending another. Witnessed: both `CB::AK`
    // and `IB::AK` are "ak".
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
    // Witnessed: `Suit::Hearts` resolves; `Suit::Nope` is `Error: Undefined constant
    // Suit::Nope`.
    let cases = "<?php
enum Suit { case Hearts; case Spades; }
$x = Suit::Hearts;
";
    assert!(consts(cases).is_empty(), "{:#?}", consts(cases));
    // An enum's own constants resolve too — enumerable for this member kind, unlike
    // methods (leg (j)/A3, unlowered).
    let konst = "<?php
enum E2 { const EK = 'ek'; case A; }
$x = E2::EK;
";
    assert!(consts(konst).is_empty(), "{:#?}", consts(konst));
    // …and the undefined case DOES fire, so the silence above is the member source,
    // not a blanket enum obstacle.
    let missing = "<?php
enum Suit { case Hearts; }
$x = Suit::Nope;
";
    assert_eq!(consts(missing).len(), 1, "{:#?}", consts(missing));
}

// class-const.undefined — obstacle legs.

#[test]
fn a_trait_using_class_is_silence_for_constants() {
    // Trait constants (8.2+) answer through the using class (witnessed: `CT::TK` is
    // "tk"), so a trait-using node is an obstacle, not a node to skip.
    let src = "<?php
trait T1 { const TK = 'tk'; }
class CT { use T1; }
$x = CT::NOPE;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn an_unresolvable_interface_is_silence() {
    // The interface could hold the constant, so the reach never closes.
    let src = "<?php
class C implements VendorContract { const K = 1; }
$x = C::NOPE;
";
    assert!(consts(src).is_empty(), "{:#?}", consts(src));
}

#[test]
fn the_class_magic_constant_is_excluded_at_the_site() {
    // X::class is a plain string since PHP 8.0 (witnessed even for a nonexistent
    // class) — never a member fetch.
    assert!(consts("<?php\nclass C {}\n$x = C::class;\n").is_empty());
}

#[test]
fn self_static_and_parent_are_not_subjects() {
    // Reach belongs to class-const.inaccessible: self::/parent:: resolve in a lexically
    // fixed scope this walk doesn't thread; static::K is late-bound and unproven (ADR-0043 §1).
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
    // Pure over-silence, deliberate: constants have no magic channel at all (witnessed:
    // `__get`+`__callStatic` still `Error: Undefined constant Magic::NOPE`) — reused anyway
    // so the codebase keeps ONE enumerability rule (`string.non-stringable` precedent).
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
    // The other direction: __get/__callStatic are NOT an obstacle here since constants
    // have no magic channel — fires, making this id the cleanest member in the family.
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

// The `maybe-` sibling (ADR-0078 §1.3).

#[test]
fn the_maybe_sibling_never_doubles_up_on_a_definite_finding() {
    // A definite leg never ships without its possibly-grade twin NAMED; the twin now
    // emits (ADR-0081 §7, #267). Registry checks live in tests/registry.rs, the twin's
    // fixtures in tests/property_maybe_undefined.rs — here: the two legs partition sites, never overlap.
    assert!(
        !REGISTERED_NOT_YET_EMITTED.contains(&"property.maybe-undefined"),
        "the maybe- sibling emits, so it left the registered-ahead-of-emission list"
    );
    for src in [EXACT_FIRES, CONST_FIRES] {
        assert!(
            findings(src, "property.maybe-undefined").is_empty(),
            "a site the definite leg owns is never also the possibly leg's"
        );
    }
}
