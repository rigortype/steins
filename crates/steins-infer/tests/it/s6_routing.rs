//! ADR-0049 A13: the declared-receiver lane routes by **minimum stratum**.
//!
//! One ladder (`tests/phpdoc_undefined_method.rs` pins every leg of it), two ids.
//! All-`Verified` arms — a native `C $o` parameter PHP enforces at the call
//! boundary — emit `call.undefined-method`, the proof layer at the `Default` floor,
//! the same id S2 emits (ADR-0022 decouples emitter from id). Any `Asserted` arm —
//! a `@param`/`@var` claim, or a docblock refinement inside a native envelope —
//! keeps `phpdoc.undefined-method` on the contract layer.
//!
//! What this file pins: the headline (a native-param receiver's undefined method
//! is a default-surface finding, missing per the cross-check note's §3 probe);
//! the min-stratum rule (one `Asserted` arm drags the whole finding to the
//! contract id); that no docblock claim can forge the proof id; that the
//! promoted path still stops at the A14 magic-tag obstacle, the ADR-0046 dam,
//! and the A9 sidecar gate; and the piece-2 receiver-kind widening (a declared
//! param copied to a variable), with two shapes still deferred.

use std::collections::BTreeMap;

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    CALL_UNDEFINED_METHOD_ID, Diagnostic, Folder, PHPDOC_UNDEFINED_METHOD_ID, check_with,
};
use steins_syntax::SourceTree;

/// The S6 test harness's boot-surface mock: the A9 gate is open, no builtin
/// homonyms, no PHP-minor skew.
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

fn check(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot { available: true })
}

/// The declared-receiver lane's emissions, whichever id A13 routed them to. The
/// evidence phrasing is what tells this lane apart from S2 on the shared proof id —
/// the same discriminator a human reader uses.
fn lane(src: &str) -> Vec<Diagnostic> {
    check(src)
        .into_iter()
        .filter(|d| {
            (d.id == PHPDOC_UNDEFINED_METHOD_ID || d.id == CALL_UNDEFINED_METHOD_ID)
                && d.message.contains("declared receiver")
        })
        .collect()
}

/// The lane's single finding and the id it was routed to, or `None` for silence.
fn routed(src: &str) -> Option<(&'static str, Diagnostic)> {
    let mut d = lane(src);
    assert!(d.len() <= 1, "fixtures are single-site: {d:?}");
    let d = d.pop()?;
    let id = if d.id == CALL_UNDEFINED_METHOD_ID {
        CALL_UNDEFINED_METHOD_ID
    } else {
        PHPDOC_UNDEFINED_METHOD_ID
    };
    Some((id, d))
}

/// Whether the built-in profile `name` surfaces `d`.
fn surfaced(name: &str, d: &Diagnostic) -> bool {
    ProfileConfigs(BTreeMap::new()).resolve(Some(name)).unwrap().is_surfaced(d)
}

/// A native `Guest $g` receiver: `Verified`, so the promoted half.
const NATIVE_PARAM: &str = "<?php
final class Guest { public function guestId(): int { return 1; } }
function viaParam(Guest $g): void { $g->nope(); }
";

/// The same shape declared only in a docblock: `Asserted`, so the contract half.
const DOC_PARAM: &str = "<?php
final class Guest { public function guestId(): int { return 1; } }
/** @param Guest $g */
function viaDocParam($g): void { $g->nope(); }
";


// The headline: the promoted half is a DEFAULT-surface finding.


#[test]
fn a_native_param_receiver_carries_the_proof_id() {
    // Cross-check note §3's `viaParam` probe: a bare `steins check` printed nothing
    // here, since the only emittable id sat at the `Contracts` floor. The arm is a
    // native declaration — `Verified` — so A13 routes it to the proof-layer id.
    let (id, d) = routed(NATIVE_PARAM).expect("the native-param receiver must fire");
    assert_eq!(id, CALL_UNDEFINED_METHOD_ID, "{}", d.message);
    assert!(d.message.contains("Guest::nope()"), "{}", d.message);
    assert!(d.message.contains("declared receiver $g"), "{}", d.message);
}

#[test]
fn the_promoted_finding_is_on_the_default_surface() {
    // The floor follows the layer, and the layer follows the id: nothing profile-side
    // is special-cased here.
    let (_, d) = routed(NATIVE_PARAM).unwrap();
    assert!(surfaced("default", &d), "a bare `steins check` must print it");
    assert!(surfaced("contracts", &d), "the ladder is cumulative");
    assert!(surfaced("strict", &d));
}

#[test]
fn a_docblock_param_receiver_stays_on_the_contract_id_at_contracts() {
    let (id, d) = routed(DOC_PARAM).expect("the docblock-param receiver must fire");
    assert_eq!(id, PHPDOC_UNDEFINED_METHOD_ID, "{}", d.message);
    assert!(!surfaced("default", &d), "an Asserted premise never reaches the default surface");
    assert!(surfaced("contracts", &d), "it is a contract-layer finding");
}

#[test]
fn the_two_halves_of_the_same_shape_differ_only_in_their_id() {
    // The evidence string is the lane's, not the id's: a reader can tell which
    // ladder proved a `call.undefined-method` without consulting the profile.
    let (_, native) = routed(NATIVE_PARAM).unwrap();
    let (_, doc) = routed(DOC_PARAM).unwrap();
    for d in [&native, &doc] {
        assert!(d.message.contains("call to undefined method Guest::nope()"), "{}", d.message);
        assert!(d.message.contains("hierarchy and descendants fully enumerated"), "{}", d.message);
    }
    assert_ne!(native.id, doc.id);
}


// The minimum-stratum rule at its boundary.


#[test]
fn a_lane_mixing_a_native_and_a_refined_arm_routes_asserted() {
    // `refine_contract_arms`: `A` covers a native member exactly (`Verified`), while
    // `SubB` is a strict refinement inside native `B` (`Asserted`). The minimum is
    // `Asserted` — one unverified arm is enough, since the runtime receiver may be
    // the arm the docblock invented.
    let src = "<?php
final class A {}
class B {}
final class SubB extends B {}
/** @param A|SubB $v */
function f(A|B $v): void { $v->nope(); }
";
    let (id, d) = routed(src).expect("both arms provably lack the method");
    assert_eq!(id, PHPDOC_UNDEFINED_METHOD_ID, "{}", d.message);
    assert!(d.message.contains("A|SubB"), "{}", d.message);
}

#[test]
fn a_docblock_restating_the_native_type_stays_verified() {
    // A `@param` that only repeats the native arm adds no unverified premise
    // (`arm_eq` ⇒ `Verified`); a redundant docblock must not cost the default finding.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
/** @param Guest $g */
function f(Guest $g): void { $g->nope(); }
";
    let (id, d) = routed(src).expect("must fire");
    assert_eq!(id, CALL_UNDEFINED_METHOD_ID, "{}", d.message);
}

#[test]
fn a_native_union_of_two_verified_arms_stays_verified() {
    // Min over a two-arm native lane is `Verified`: PHP enforces the whole union at
    // the boundary, so every arm is runtime-guaranteed.
    let src = "<?php
final class A {}
final class B {}
function f(A|B $v): void { $v->nope(); }
";
    let (id, d) = routed(src).expect("must fire");
    assert_eq!(id, CALL_UNDEFINED_METHOD_ID, "{}", d.message);
    assert!(d.message.contains("A|B"), "{}", d.message);
}


// No docblock claim can forge the proof id (ADR-0037, ADR-0052 N2).


#[test]
fn a_lying_param_cannot_forge_the_proof_id() {
    // `@param Guest $v` over an untyped parameter: the runtime value may be anything,
    // so the claim is `Asserted` — correct as a contract violation (§8), never a proof.
    let (id, _) = routed(DOC_PARAM).unwrap();
    assert_eq!(id, PHPDOC_UNDEFINED_METHOD_ID);
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
/** @param Guest $v */
function f(mixed $v): void { $v->nope(); }
";
    assert_eq!(routed(src).map(|(id, _)| id), Some(PHPDOC_UNDEFINED_METHOD_ID));
}

#[test]
fn an_inline_var_cast_cannot_forge_the_proof_id() {
    // ADR-0073's statement-level `/** @var T $x */` seeds every arm `Asserted` (no
    // native envelope backs it), so the narrowed receiver stops at the contract id.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
function f(object $x): void {
    /** @var Guest $x */
    $x->nope();
}
";
    let (id, d) = routed(src).expect("the cast narrows the receiver onto Guest");
    assert_eq!(id, PHPDOC_UNDEFINED_METHOD_ID, "{}", d.message);
}

#[test]
fn a_lying_phpstan_assert_cannot_forge_the_proof_id() {
    // Adversarial shape: without the tag the lane is `{Base}`, `Verified` and SILENT
    // (descendant `Sub` answers `nope()`). A tag claiming the method-less `Guest`
    // would, if it could mint a `Verified` class arm, turn that silence into a
    // proof-layer finding — but the assert lane narrows only at `Asserted` and
    // never seeds a declared class arm, so the shape stays silent on BOTH ids.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
class Base {}
class Sub extends Base { public function nope(): void {} }
/** @phpstan-assert Guest $o */
function assertGuest(object $o): void {}
function f(Base $b): void { assertGuest($b); $b->nope(); }
";
    let d = lane(src);
    assert!(d.is_empty(), "a tag must not manufacture a declared-receiver claim: {d:?}");
}


// The promoted path still climbs the whole ladder (spot-pins).


#[test]
fn the_promoted_path_still_stops_at_a_magic_tag_obstacle() {
    // A14 / issue #195, on the proof-layer half: an `@method` tag in the receiver's
    // reach silences the lane, exactly as it does for the contract half.
    let tagged = "<?php
/** @method int foo() */
final class Guest { public function guestId(): int { return 1; } }
function viaParam(Guest $g): void { $g->nope(); }
";
    assert!(lane(tagged).is_empty(), "{:?}", lane(tagged));
    // Control: the untagged twin is the promoted headline fixture.
    assert_eq!(routed(NATIVE_PARAM).map(|(id, _)| id), Some(CALL_UNDEFINED_METHOD_ID));
}

#[test]
fn the_promoted_path_still_stops_at_the_dam() {
    // ADR-0046: an `eval` site can mint a subclass carrying the method, so the
    // enumerated descendant set does not close — the dam outranks the stratum.
    let clean = "<?php
class Base {}
function f(Base $b): void { $b->nope(); }
";
    assert_eq!(routed(clean).map(|(id, _)| id), Some(CALL_UNDEFINED_METHOD_ID));
    let dammed = "<?php
eval('$x = 1;');
class Base {}
function f(Base $b): void { $b->nope(); }
";
    assert!(lane(dammed).is_empty(), "{:?}", lane(dammed));
}

#[test]
fn the_promoted_path_still_needs_a_live_sidecar() {
    // A9: with the absence family unavailable, the lane is silent on both ids —
    // promotion buys the proof half no exemption.
    let tree = SourceTree::parse(NATIVE_PARAM);
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot { available: false })
        .into_iter()
        .filter(|d| d.message.contains("declared receiver"))
        .collect();
    assert!(d.is_empty(), "{d:?}");
}


// Piece 2: the receiver-kind gate.


#[test]
fn a_declared_receiver_copied_to_a_variable_reaches_the_lane() {
    // `$c = $o;` binds the same value, so every declared possibility of `$o` holds
    // of `$c` at the same stratum — the copy's finding is a proof too.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
function f(Guest $o): void { $c = $o; $c->nope(); }
";
    let (id, d) = routed(src).expect("the copied receiver must reach the lane");
    assert_eq!(id, CALL_UNDEFINED_METHOD_ID, "{}", d.message);
    assert!(d.message.contains("declared receiver $c"), "{}", d.message);
}

#[test]
fn a_copied_docblock_receiver_keeps_the_contract_id() {
    // The copy carries the stratum with the arms — it is a transport, not a
    // laundering step.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
/** @param Guest $o */
function f($o): void { $c = $o; $c->nope(); }
";
    let (id, d) = routed(src).expect("the copied receiver must reach the lane");
    assert_eq!(id, PHPDOC_UNDEFINED_METHOD_ID, "{}", d.message);
}

#[test]
fn a_copy_narrowed_after_the_copy_still_closes() {
    // The copied lane is live: a guard on the COPY subtracts only from the copy's
    // arms, so the else-branch closes on the surviving native arm.
    let src = "<?php
final class User { public function name(): string { return 'u'; } }
final class Guest {}
function f(User|Guest $v): void {
    $c = $v;
    if ($c instanceof User) { return; }
    $c->nope();
}
";
    let (id, d) = routed(src).expect("the narrowed copy must fire");
    assert_eq!(id, CALL_UNDEFINED_METHOD_ID, "{}", d.message);
    assert!(d.message.contains("Guest::nope()"), "{}", d.message);
}

#[test]
fn a_return_typed_call_receiver_is_still_silent() {
    // Deferred: `mk()->m()` has no [`Receiver`] representation — the trace lowers
    // it to `Callee::Dynamic` — so admitting it is a syntax-crate change (a new
    // receiver variant plus every match on it), not a widening of this lane.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
function mk(): Guest { return new Guest(); }
function f(): void { mk()->nope(); }
";
    assert!(lane(src).is_empty(), "{:?}", lane(src));
}

#[test]
fn a_property_receiver_is_still_silent() {
    // ADR-0052 N5: `$this->prop->m()` is a Barrier — a property chain carries no
    // declared-arm lane in v0.1.0. Explicitly deferred there, not here.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
final class Holder {
    public function __construct(private Guest $g) {}
    public function f(): void { $this->g->nope(); }
}
";
    assert!(lane(src).is_empty(), "{:?}", lane(src));
}


// Site disjointness under one shared id.


#[test]
fn one_call_site_is_judged_by_exactly_one_emitter() {
    // A13 lets S2 and the promoted declared-receiver half share `call.undefined-method`;
    // the ADR-0049 §8 disjointness invariant restated over SITES: an exact receiver
    // has no contract lane consulted (`is_exact` bails), a lane-carrying var is never
    // `class_exact` — one finding per site, from one emitter.
    let src = "<?php
final class Guest { public function guestId(): int { return 1; } }
function f(Guest $g): void {
    $g->nope();
    $x = new Guest();
    $x->nope();
}
";
    let all: Vec<Diagnostic> =
        check(src).into_iter().filter(|d| d.id == CALL_UNDEFINED_METHOD_ID).collect();
    assert_eq!(all.len(), 2, "one per site, never two per site: {all:?}");
    let declared: Vec<&Diagnostic> =
        all.iter().filter(|d| d.message.contains("declared receiver")).collect();
    assert_eq!(declared.len(), 1, "the param site is the lane's: {all:?}");
    assert_eq!(declared[0].line, 4);
    assert!(check(src).iter().all(|d| d.id != PHPDOC_UNDEFINED_METHOD_ID), "{all:?}");
}
