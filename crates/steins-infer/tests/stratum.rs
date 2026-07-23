//! Stratum-discipline tests (ADR-0052 §5 slice N2): the checked trust bit that
//! stops a docblock claim from forging a proof-layer finding.
//!
//! The through-line: an `Asserted` fact (an `@phpstan-assert` family claim, or an
//! `assert($expr)` narrowing) buys **silence** — narrowing away a would-be proof
//! report is always safe — but never *premises* a proof-layer id (`type.*`,
//! `call.on-null`). A `Verified` fact (a native runtime test, a native seed) does.
//! Every "SILENT" test below is paired, where practical, with a `Verified` control
//! that fires, so the test proves the narrowing happened *and* was gated — not that
//! nothing narrowed at all.

use steins_infer::{CALL_ON_NULL_ID, Diagnostic, ID, PARAM_MISMATCH_ID, check, check_runtime};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn findings_zend(src: &str, zend: bool) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_runtime(&tree, &functions, "test.php", zend)
}

fn arg_mismatch(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == ID).count()
}

fn on_null(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == CALL_ON_NULL_ID).count()
}

// ==========================================================================
// Control: a Verified null premises the proof layer exactly as before.
// ==========================================================================

#[test]
fn verified_null_into_int_fires_proof() {
    // Baseline: a *proven* null flowing into `int` is a proof-layer
    // `type.argument-mismatch`. This is the control the Asserted cases silence.
    let src = "<?php
function takesInt(int $n): void {}
function f(): void { $x = null; takesInt($x); }
";
    assert_eq!(arg_mismatch(src), 1, "a Verified null must still premise the proof layer");
}

// ==========================================================================
// Assert tags (@phpstan-assert null) never premise the proof layer.
// ==========================================================================

#[test]
fn asserted_null_cannot_premise_argument_mismatch() {
    // A lying `@phpstan-assert null $x` narrows `$x` to null at the Asserted
    // stratum; the downstream `takesInt($x)` would be a proof-layer
    // `type.argument-mismatch` — but a claim cannot forge a proof, so SILENT.
    let src = "<?php
/** @phpstan-assert null $x */
function claimNull($x): void {}
function takesInt(int $n): void {}
function f(mixed $x): void { claimNull($x); takesInt($x); }
";
    assert_eq!(arg_mismatch(src), 0, "an Asserted null must NOT premise type.argument-mismatch");
}

#[test]
fn asserted_null_cannot_premise_call_on_null() {
    // The `call.on-null` proof: a lying `@phpstan-assert null $x` then `$x->m()`
    // must stay silent (the receiver is only *claimed* null).
    let src = "<?php
/** @phpstan-assert null $x */
function claimNull($x): void {}
function f(mixed $x): void { claimNull($x); $x->method(); }
";
    assert_eq!(on_null(src), 0, "an Asserted null receiver must NOT premise call.on-null");
}

#[test]
fn asserted_int_may_fire_contract_but_not_proof() {
    // Item 7(b): `@phpstan-assert int $x` on a value flowing into a `@param string`
    // contract fires the *contract* layer (phpdoc.param-mismatch accepts Asserted),
    // while no *proof* id fires. The claim is coherent end-to-end at its own layer.
    let src = "<?php
/** @phpstan-assert int $x */
function claimInt($x): void {}
/** @param string $s */
function takesString($s): void {}
function f(mixed $x): void { claimInt($x); takesString($x); }
";
    let f = findings(src);
    assert_eq!(f.iter().filter(|d| d.id == ID).count(), 0, "no proof-layer finding on an Asserted premise");
    assert_eq!(
        f.iter().filter(|d| d.id == PARAM_MISMATCH_ID).count(),
        1,
        "the contract layer MAY fire on the same Asserted fact"
    );
}

// ==========================================================================
// The derivation clause: min-stratum through copy/array composition and joins.
// ==========================================================================

#[test]
fn derivation_copy_carries_asserted() {
    // `$y = $x` where `$x` is Asserted-null: `$y` inherits the Asserted stratum,
    // so `takesInt($y)` stays silent (min-stratum through the assignment copy).
    let src = "<?php
/** @phpstan-assert null $x */
function claimNull($x): void {}
function takesInt(int $n): void {}
function f(mixed $x): void { claimNull($x); $y = $x; takesInt($y); }
";
    assert_eq!(arg_mismatch(src), 0, "a copy of an Asserted fact stays Asserted");
}

#[test]
fn derivation_array_composition_stays_silent() {
    // The audit's laundering shape: `$pair = [$x, 99]` composed from an Asserted
    // `$x` must not let a proof-layer finding fire on the composed value. (Offset
    // consumption is S3; this pins the composition-stratum invariant meanwhile.)
    let src = "<?php
/** @phpstan-assert null $x */
function claimNull($x): void {}
function takesInt(int $n): void {}
function f(mixed $x): void { claimNull($x); $pair = [$x, 99]; takesInt($pair[0]); }
";
    assert_eq!(arg_mismatch(src), 0, "array composition over an Asserted element launders nothing");
}

#[test]
fn join_of_verified_and_asserted_is_asserted() {
    // A branch join of a Verified-null arm and an Asserted-null arm yields an
    // Asserted null (min-stratum), so the post-if `takesInt($x)` stays silent.
    let src = "<?php
/** @phpstan-assert null $x */
function claimNull($x): void {}
function takesInt(int $n): void {}
function f(mixed $x, bool $c): void {
    if ($c) { claimNull($x); } else { $x = null; }
    takesInt($x);
}
";
    assert_eq!(arg_mismatch(src), 0, "Verified ⊔ Asserted ⇒ Asserted (join min-stratum)");
}

#[test]
fn join_of_two_verified_still_fires() {
    // Control for the join test: both arms Verified-null ⇒ the join is Verified, so
    // the proof layer fires (the join min-stratum did not needlessly downgrade).
    let src = "<?php
function takesInt(int $n): void {}
function f(bool $c): void {
    if ($c) { $x = null; } else { $x = null; }
    takesInt($x);
}
";
    assert_eq!(arg_mismatch(src), 1, "Verified ⊔ Verified ⇒ Verified (still premises proof)");
}

// ==========================================================================
// Guard-position -if-true / -if-false consumption (Asserted stratum).
// ==========================================================================

#[test]
fn guard_if_true_narrows_then_branch_silently() {
    // `@phpstan-assert-if-true null` consumed on the TRUE branch: `$x` narrows to
    // null (Asserted), so the guarded `takesInt($x)` stays silent.
    let src = "<?php
/** @phpstan-assert-if-true null $x */
function isNull($x): bool { return true; }
function takesInt(int $n): void {}
function f(mixed $x): void { if (isNull($x)) { takesInt($x); } }
";
    assert_eq!(arg_mismatch(src), 0, "-if-true narrows the true branch at the Asserted stratum");
}

#[test]
fn guard_verified_equality_control_fires() {
    // Control: the same shape with a *native* `=== null` guard narrows at the
    // Verified stratum, so the guarded `takesInt($x)` DOES premise the proof layer.
    let src = "<?php
function takesInt(int $n): void {}
function f(mixed $x): void { if ($x === null) { takesInt($x); } }
";
    assert_eq!(arg_mismatch(src), 1, "a native === null guard is Verified → fires");
}

#[test]
fn guard_if_false_narrows_else_branch_silently() {
    // `@phpstan-assert-if-false null` consumed on the FALSE branch (else): `$x`
    // narrows to null (Asserted) there, so `takesInt($x)` stays silent.
    let src = "<?php
/** @phpstan-assert-if-false null $x */
function notNull($x): bool { return true; }
function takesInt(int $n): void {}
function f(mixed $x): void { if (notNull($x)) {} else { takesInt($x); } }
";
    assert_eq!(arg_mismatch(src), 0, "-if-false narrows the else branch at the Asserted stratum");
}

#[test]
fn guard_negated_if_true_flips_polarity() {
    // `if (!isNull($x))` — the `@phpstan-assert-if-true null` spec applies on the
    // ELSE branch (the call was true there). The guarded else `takesInt($x)` stays
    // silent; the then branch is unnarrowed.
    let src = "<?php
/** @phpstan-assert-if-true null $x */
function isNull($x): bool { return true; }
function takesInt(int $n): void {}
function f(mixed $x): void { if (!isNull($x)) {} else { takesInt($x); } }
";
    assert_eq!(arg_mismatch(src), 0, "a negated guard flips the polarity to the else branch");
}

#[test]
fn bare_assert_if_true_is_not_recognized() {
    // Regression pin (conformance `regressions_string_narrowing_assert_if_true`):
    // the BARE `@assert-if-true` (no vendor prefix, ADR-0029) is NOT a recognized
    // tag, so it narrows nothing — the guard-call carrier consumes no envelope and
    // the case stays exactly as silent as before this slice.
    let src = "<?php
/** @assert-if-true null $x */
function isNull($x): bool { return true; }
function takesInt(int $n): void {}
function f(mixed $x): void { if (isNull($x)) { takesInt($x); } }
";
    // No recognized envelope ⇒ no narrowing ⇒ $x stays `mixed` ⇒ the native check
    // has no proven value to fire on. Silent, but for the "unrecognized" reason.
    assert_eq!(arg_mismatch(src), 0, "a bare @assert-if-true narrows nothing (unchanged behavior)");
}

// ==========================================================================
// assert($expr) statement narrowing and the [runtime] zend-assertions knob.
// ==========================================================================

#[test]
fn assert_stmt_narrows_at_asserted_by_default() {
    // `assert($x === null)` narrows `$x` to null — but at the Asserted stratum by
    // default (under zend.assertions=-1 the expression is never evaluated), so the
    // downstream `takesInt($x)` stays silent.
    let src = "<?php
function takesInt(int $n): void {}
function f(mixed $x): void { assert($x === null); takesInt($x); }
";
    assert_eq!(arg_mismatch(src), 0, "assert() narrowing is Asserted by default → silent proof layer");
}

#[test]
fn assert_stmt_promoted_to_verified_fires() {
    // With `[runtime] zend-assertions = "enabled"`, the assert expression runs, so
    // the narrowing rises to Verified and the downstream proof-layer check fires.
    let src = "<?php
function takesInt(int $n): void {}
function f(mixed $x): void { assert($x === null); takesInt($x); }
";
    assert_eq!(
        findings_zend(src, true).iter().filter(|d| d.id == ID).count(),
        1,
        "zend-assertions=enabled promotes assert() narrowing to Verified → fires"
    );
    assert_eq!(
        findings_zend(src, false).iter().filter(|d| d.id == ID).count(),
        0,
        "the same source with the default (disabled) stays silent"
    );
}
