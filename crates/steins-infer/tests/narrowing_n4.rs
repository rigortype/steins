//! ADR-0052 N4 — class facts and instanceof subtraction, at the walk-integration
//! level. N4 owns **no finding id** (S6 supplies `phpdoc.undefined-method`); its
//! whole observable contract here is therefore: (1) it emits nothing new, and
//! (2) the two new carriers never reach the §3 NOT-fed consumers — most sharply
//! `call.undefined-method`, whose ladder requires *exactness* a `Member` fact must
//! not supply. The carrier-level narrowing (the `{Guest}` arm list) is asserted in
//! the `n4_carrier_tests` unit module.

use steins_infer::{CALL_UNDEFINED_METHOD_ID, Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

fn n(src: &str) -> usize {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").len()
}

/// A boot-surface mock making the absence family (`call.undefined-method`, S2)
/// available with an empty homonym surface — the environment in which the id
/// *would* fire on a proven-exact receiver, so a silence here proves the receiver
/// was NOT treated as exact.
struct Boot;
impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

fn undefined_method(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| d.id == CALL_UNDEFINED_METHOD_ID)
        .collect()
}

/// The conformance deliverable's source. N4 leaves `{Guest}` on the else path but
/// emits nothing — the `phpdoc.undefined-method` at the `$value->name()` site is
/// S6's id, not landed. So the whole program is silent under N4.
const FIXTURE: &str = "<?php
declare(strict_types=1);
interface Named { public function name(): string; }
final class User implements Named { public function name(): string { return 'u'; } }
final class Guest { public function guestId(): int { return 1; } }
/** @param User|Guest $value */
function takesUserOrGuest(object $value): void {
    if ($value instanceof User) { echo $value->name(); return; }
    $value->name();
}
";

#[test]
fn conformance_fixture_emits_nothing_under_n4() {
    assert_eq!(n(FIXTURE), 0, "N4 owns no id; S6 supplies the finding — silent here");
}

#[test]
fn instanceof_narrowing_adds_no_findings() {
    // A spread of instanceof shapes over declared unions — all silent under N4.
    let src = "<?php
interface I {}
final class A implements I {}
final class B implements I {}
/** @param A|B $x */
function f(object $x): void {
    if ($x instanceof A) { $x->foo(); } else { $x->bar(); }
    if (!($x instanceof A)) { $x->baz(); }
}
";
    assert_eq!(n(src), 0);
}

#[test]
fn member_fact_is_not_exactness_no_undefined_method() {
    // NOT-fed enforcement (§3): a param object narrowed by `instanceof Order` gains a
    // `Member{yes:[Order]}` fact but NO exactness. `call.undefined-method` requires a
    // proven-exact receiver (ADR-0049 §4a), so `$x->tyop()` must stay silent even
    // though `Order` is final and defines no `tyop` — a final `Member` is deliberately
    // NOT exactness in v1 (no binding descent, no undefined-method).
    let src = "<?php
final class Order {}
function f(object $x): void {
    if ($x instanceof Order) { $x->tyop(); }
}
";
    assert!(
        undefined_method(src).is_empty(),
        "a Member-narrowed (non-exact) receiver must not satisfy the undefined-method ladder"
    );
}

#[test]
fn exact_receiver_still_fires_control() {
    // Control: the same missing call on a genuinely EXACT receiver (`new Order()`)
    // still fires — proving the silence above is the Member/exactness distinction,
    // not a broken harness.
    let src = "<?php
final class Order {}
(new Order())->tyop();
";
    assert_eq!(undefined_method(src).len(), 1, "exact receiver still fires undefined-method");
}

#[test]
fn contract_seed_does_not_disturb_scalar_findings() {
    // Seeding a contract lane for a scalar-union param must not perturb the existing
    // value-domain checks — a well-typed body stays silent.
    let src = "<?php
/** @param int|string $x */
function f(int|string $x): void {
    if ($x instanceof \\Stringable) { return; }
    echo $x;
}
";
    assert_eq!(n(src), 0);
}
