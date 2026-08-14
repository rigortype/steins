//! `property.maybe-undefined` (ADR-0078's floor table, ADR-0081 §7): the declared-shape
//! possibly leg of the property family.
//!
//! The receiver is narrowed to a **union** of declared types; the §8 ladder proves the
//! property absent on some arms and declared on the rest — the arms are declared types, not
//! control-flow paths, so nothing here consults `variable.*`'s reachability foundation.
//!
//! The routing is a partition, and this file pins all three cells: every arm absent is
//! `property.undefined` at the `default` floor, some arms absent is this id at `strict`, and
//! one arm the ladder cannot close is silence on both.

use std::collections::BTreeMap;

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    Diagnostic, Floor, Folder, Layer, PROPERTY_MAYBE_UNDEFINED_ID, PROPERTY_UNDEFINED_ID,
    check_with, layer, surface_floor,
};
use steins_syntax::SourceTree;

/// The same boot-surface mock the definite leg's suite uses: A9 gate open, no homonym,
/// no PHP-minor skew — these fixtures measure the ladder, never the gate.
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

fn maybe(src: &str) -> Vec<Diagnostic> {
    findings(src, PROPERTY_MAYBE_UNDEFINED_ID)
}

fn definite(src: &str) -> Vec<Diagnostic> {
    findings(src, PROPERTY_UNDEFINED_ID)
}

/// One arm declares `$nope`, the other provably lacks it — the id's whole reason to exist.
/// Both arms are `final`, so descendant closure is immune.
const MIXED_UNION: &str = "<?php
final class Has { public int $nope = 1; }
final class Lacks { public int $other = 2; }
function f(Has|Lacks $o): void { $x = $o->nope; }
";

/// Neither arm declares it — the definite leg's cell.
const NEITHER_UNION: &str = "<?php
final class A { public int $other = 1; }
final class B { public int $other = 2; }
function f(A|B $o): void { $x = $o->nope; }
";

// The registry contract.

#[test]
fn the_id_sits_in_the_proof_layer_at_the_strict_floor() {
    assert_eq!(layer(PROPERTY_MAYBE_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(surface_floor(PROPERTY_MAYBE_UNDEFINED_ID), Some(Floor::Strict));
    assert_eq!(surface_floor(PROPERTY_UNDEFINED_ID), Some(Floor::Default));
}

#[test]
fn only_the_strict_profile_surfaces_it() {
    let d = maybe(MIXED_UNION).pop().expect("fires");
    for profile in ["default", "contracts", "throws-direct"] {
        let surface = ProfileConfigs(BTreeMap::new()).resolve(Some(profile)).unwrap();
        assert!(!surface.is_surfaced(&d), "`{profile}` must not show the weaker claim");
    }
    let strict = ProfileConfigs(BTreeMap::new()).resolve(Some("strict")).unwrap();
    assert!(strict.is_surfaced(&d), "`strict` is where the possibly leg lives");
}

// The three cells of the partition.

#[test]
fn fires_where_some_arms_declare_the_property_and_some_do_not() {
    let d = maybe(MIXED_UNION);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("only some arms"), "{}", d[0].message);
    assert!(d[0].message.contains("Has"), "the arm that declares it: {}", d[0].message);
    assert!(d[0].message.contains("Lacks"), "the arm that does not: {}", d[0].message);
    assert!(
        d[0].message.contains("Undefined property: Lacks::$nope"),
        "PHP's own sentence, on the arm it applies to: {}",
        d[0].message
    );
    // The definite leg must stay off this site.
    assert!(definite(MIXED_UNION).is_empty(), "{:#?}", definite(MIXED_UNION));
}

#[test]
fn every_arm_absent_stays_on_the_definite_leg() {
    assert_eq!(definite(NEITHER_UNION).len(), 1, "{:#?}", definite(NEITHER_UNION));
    assert!(maybe(NEITHER_UNION).is_empty(), "{:#?}", maybe(NEITHER_UNION));
}

#[test]
fn every_arm_declaring_it_is_silence() {
    let src = "<?php
final class A { public int $nope = 1; }
final class B { public int $nope = 2; }
function f(A|B $o): void { $x = $o->nope; }
";
    assert!(maybe(src).is_empty(), "{:#?}", maybe(src));
    assert!(definite(src).is_empty(), "{:#?}", definite(src));
}

#[test]
fn an_unclosable_arm_silences_both_legs() {
    // The leg that makes the possibly claim honest: a `__get` on one arm means the ladder
    // proved nothing there, and "some arms lack it" is still a claim about ALL of them.
    let src = "<?php
final class Has { public int $nope = 1; }
final class Magic { public function __get($n) { return 1; } }
function f(Has|Magic $o): void { $x = $o->nope; }
";
    assert!(maybe(src).is_empty(), "{:#?}", maybe(src));
    assert!(definite(src).is_empty(), "{:#?}", definite(src));

    // Control: the same shape without the magic method fires, so silence above is the leg.
    assert_eq!(maybe(MIXED_UNION).len(), 1, "{:#?}", maybe(MIXED_UNION));
}

#[test]
fn a_descendant_declaring_the_property_silences_the_possibly_leg_too() {
    // A descendant that declares it is an UNKNOWN arm, not a clean one — the runtime receiver may or may not be that subclass.
    let src = "<?php
final class Has { public int $nope = 1; }
class Lacks { public int $other = 2; }
class Sub extends Lacks { public int $nope = 3; }
function f(Has|Lacks $o): void { $x = $o->nope; }
";
    assert!(maybe(src).is_empty(), "{:#?}", maybe(src));
    assert!(definite(src).is_empty(), "{:#?}", definite(src));
}

// The definite leg's premises, inherited.

#[test]
fn the_a9_sidecar_gate_silences_it() {
    let tree = SourceTree::parse(MIXED_UNION);
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot { available: false })
        .into_iter()
        .filter(|d| d.id == PROPERTY_MAYBE_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn an_asserted_arm_is_the_same_calibration_boundary() {
    // A13's Verified-stratum floor is computed before the ladder runs, so a docblock-premised union gets no id on either leg.
    let src = "<?php
final class Has { public int $nope = 1; }
final class Lacks { public int $other = 2; }
/** @param Has|Lacks $o */
function f($o): void { $x = $o->nope; }
";
    assert!(maybe(src).is_empty(), "an Asserted arm is silence: {:#?}", maybe(src));
}

#[test]
fn a_project_wide_dynamic_write_silences_it() {
    // The obstacle is asked before any class work, covering both legs: a name written dynamically anywhere could have created it on the object first.
    let src = "<?php
final class Has { public int $nope = 1; }
final class Lacks { public int $other = 2; }
function f(Has|Lacks $o): void { $x = $o->nope; }
function w(object $q): void { $q->nope = 5; }
";
    assert!(maybe(src).is_empty(), "{:#?}", maybe(src));
}

#[test]
fn a_declared_null_warning_posture_takes_it_off_the_proof_surface() {
    // ADR-0049 §7: the consequence is warning-plus-`null`, exactly the definite leg's, so the same gate applies.
    let tree = SourceTree::parse(MIXED_UNION);
    let tolerant: Vec<Diagnostic> = steins_infer::check_full(
        &tree,
        "test.php",
        &mut Boot { available: true },
        false,
    )
    .into_iter()
    .filter(|d| d.id == PROPERTY_MAYBE_UNDEFINED_ID)
    .collect();
    assert!(tolerant.is_empty(), "a declared `null` posture tolerates the warning: {tolerant:#?}");
}
