//! Assert-tag consumption for **class-typed** specs (ADR-0052 §5 + §3(d), issue
//! #266 slice 2).
//!
//! Closes the gap where `@phpstan-assert Guest $v` established *nothing*: the
//! value lane is object-free by construction (ADR-0035), so every `Class` arm
//! was declined and the tag family was a silent no-op on object subjects, unlike
//! the equivalent `instanceof` guard.
//!
//! What lands: a class-typed spec narrows the **contract arm lane**, arm-wise,
//! through the same judgment the `instanceof` guard uses, at the `Asserted`
//! stratum. It **adds** contract-layer findings (`phpdoc.*`) where a docblock
//! claim narrows a declared union, and no proof-layer finding — the two stratum
//! pins at the bottom are the fixtures that would catch a violation.

use steins_infer::{
    CALL_ON_NULL_ID, CALL_UNDEFINED_METHOD_ID, Diagnostic, Folder, ID, check, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

/// A boot surface that makes the absence family available with an empty homonym
/// surface — the environment in which `call.undefined-method` (the proof-layer,
/// exactness-requiring id) *would* fire on a proven-exact receiver. A silence
/// under it therefore proves the receiver was never treated as exact.
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

fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot)
}

fn ids(src: &str, id: &str) -> usize {
    diags(src).iter().filter(|d| d.id == id).count()
}

fn plain(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

const PHPDOC_UNDEFINED_METHOD: &str = "phpdoc.undefined-method";

const PRELUDE: &str = "<?php
declare(strict_types=1);
final class User { public function name(): string { return 'u'; } }
final class Guest { public function guestId(): int { return 1; } }
/** @phpstan-assert Guest $value */
function mustGuest(object $value): void {}
/** @phpstan-assert !Guest $value */
function mustNotGuest(object $value): void {}
/** @phpstan-assert-if-true Guest $value */
function isGuest(object $value): bool { return $value instanceof Guest; }
/** @phpstan-assert-if-false Guest $value */
function notGuest(object $value): bool { return !($value instanceof Guest); }
";

// The reference point: the same narrowing spelled as a guard.

#[test]
fn instanceof_guard_is_the_reference_narrowing() {
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    if ($value instanceof User) {{ echo $value->name(); return; }}
    $value->name();
}}
"
    );
    assert_eq!(
        ids(&src, PHPDOC_UNDEFINED_METHOD),
        1,
        "the guard leaves {{Guest}} on the fall-through and S6 reports Guest::name()"
    );
}

// `@phpstan-assert` (Always), statement position.

#[test]
fn always_assert_narrows_the_declared_arm_lane() {
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    mustGuest($value);
    $value->name();
}}
"
    );
    assert_eq!(
        ids(&src, PHPDOC_UNDEFINED_METHOD),
        1,
        "the Always tag deletes the User arm exactly as the guard does"
    );
}

#[test]
fn always_assert_control_without_the_tag_is_silent() {
    // The control: drop the tag and the lane keeps both arms, so `name()` — present
    // on User — is a Maybe, and Maybe is silence.
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    $value->name();
}}
"
    );
    assert_eq!(ids(&src, PHPDOC_UNDEFINED_METHOD), 0, "an un-narrowed union is a Maybe → silent");
}

// The negated form — the other polarity of the same subtraction.

#[test]
fn negated_assert_deletes_the_named_arm() {
    // `@phpstan-assert !Guest` is the `!($v instanceof Guest)` rule: an arm dies iff
    // it IS-A Guest. `{User, Guest}` minus Guest is `{User}`, and `guestId()` is
    // absent on User.
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    mustNotGuest($value);
    $value->guestId();
}}
"
    );
    assert_eq!(ids(&src, PHPDOC_UNDEFINED_METHOD), 1, "the negated tag deletes the Guest arm");
}

// Guard position, both polarities.

#[test]
fn assert_if_true_narrows_on_the_true_branch() {
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    if (isGuest($value)) {{ $value->name(); }}
}}
"
    );
    assert_eq!(ids(&src, PHPDOC_UNDEFINED_METHOD), 1, "-if-true applies on the then-branch");
}

#[test]
fn assert_if_true_does_not_narrow_the_false_branch() {
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    if (isGuest($value)) {{ echo 1; }} else {{ $value->name(); }}
}}
"
    );
    assert_eq!(
        ids(&src, PHPDOC_UNDEFINED_METHOD),
        0,
        "-if-true says nothing about the branch where the call returned false"
    );
}

#[test]
fn assert_if_false_narrows_on_the_false_branch() {
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    if (notGuest($value)) {{ echo 1; }} else {{ $value->name(); }}
}}
"
    );
    assert_eq!(ids(&src, PHPDOC_UNDEFINED_METHOD), 1, "-if-false applies on the else-branch");
}

// The stratum pins — what an Asserted class claim must NOT buy.

#[test]
fn an_asserted_class_claim_never_premises_the_proof_layer_absence_id() {
    // `call.undefined-method` (ADR-0049 §4a) requires receiver EXACTNESS; an assert
    // tag supplies membership at best and is kept out of the `Member` carrier. Under
    // a boot surface that makes the id available, the site must stay silent.
    let src = format!(
        "{PRELUDE}
/** @param User|Guest $value */
function f(object $value): void {{
    mustGuest($value);
    $value->name();
}}
"
    );
    assert_eq!(
        ids(&src, CALL_UNDEFINED_METHOD_ID),
        0,
        "a lying tag must not forge the exactness the proof-layer id requires"
    );
}

#[test]
fn an_asserted_class_claim_does_not_overwrite_a_proven_null() {
    // Replace-if-weaker's second half, on the class road: the value lane holds a
    // Verified `null` the tag's claim does not touch, so the finding is premised
    // entirely on Verified evidence and is correct to fire.
    let src = format!(
        "{PRELUDE}
function f(): void {{
    $x = null;
    mustGuest($x);
    $x->name();
}}
"
    );
    assert_eq!(
        plain(&src).iter().filter(|d| d.id == CALL_ON_NULL_ID).count(),
        1,
        "the Verified null survives the Asserted class claim"
    );
}

#[test]
fn a_class_assert_mints_no_value_fact() {
    // The carrier boundary: a class claim writes the arm lane and nothing else. If it
    // leaked into the value lane, `takesInt($v)` — a definite-No against an object
    // fact — would fire on an Asserted premise.
    let src = format!(
        "{PRELUDE}
function takesInt(int $n): void {{}}
/** @param User|Guest $value */
function f(object $value): void {{
    mustGuest($value);
    takesInt($value);
}}
"
    );
    assert_eq!(
        plain(&src).iter().filter(|d| d.id == ID).count(),
        0,
        "no proof-layer argument mismatch may be premised on a class assert tag"
    );
}

// Boundaries that keep this narrow.

#[test]
fn an_unknown_class_name_narrows_nothing() {
    // An `Unknown` is-a keeps every arm (the FP-safe side of §2's class-arm rule).
    let src = format!(
        "{PRELUDE}
/** @phpstan-assert \\Nowhere\\Missing $value */
function mustMissing(object $value): void {{}}
/** @param User|Guest $value */
function f(object $value): void {{
    mustMissing($value);
    $value->name();
}}
"
    );
    assert_eq!(ids(&src, PHPDOC_UNDEFINED_METHOD), 0, "an unresolvable class name deletes no arm");
}

#[test]
fn a_subject_without_a_declared_lane_learns_nothing() {
    // No `@param` union means no arm lane; the tag has nothing to subtract from and
    // mints no carrier of its own.
    let src = format!(
        "{PRELUDE}
function f(object $value): void {{
    mustGuest($value);
    $value->name();
}}
"
    );
    assert_eq!(ids(&src, PHPDOC_UNDEFINED_METHOD), 0, "no declared lane, no narrowing");
}

#[test]
fn the_prefix_rule_still_gates_the_family() {
    // ADR-0029's prefix rule: a bare `@assert-if-true` is not a recognized tag. The
    // pinned regression of ADR-0052 §10, restated on the class road — an unprefixed
    // tag must consume as nothing at all.
    let src = format!(
        "{PRELUDE}
/** @assert Guest $value */
function bareAssert(object $value): void {{}}
/** @param User|Guest $value */
function f(object $value): void {{
    bareAssert($value);
    $value->name();
}}
"
    );
    assert_eq!(ids(&src, PHPDOC_UNDEFINED_METHOD), 0, "an unprefixed tag is not an assert tag");
}
