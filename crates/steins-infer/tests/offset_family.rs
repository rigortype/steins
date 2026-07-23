//! ADR-0049 §7 / S3: the offset family — `offset.missing` / `offset.on-unsupported`.
//!
//! A value-domain absence proof: a read `$base[$key]` provably emits an `E_WARNING`
//! because the container value is a proven `Verified` `Singleton`/all-array `OneOf`
//! and the key is provably absent, or the base is a proven non-offsetable
//! scalar/null. The family is gated on the runtime boot surface (ADR-0049 A9), so
//! these tests drive a [`Ready`] folder that stands in for a live, monkey-patch-free
//! sidecar. Every ladder leg ships with a **silence fixture** (§10 silence-matrix
//! discipline) proving the id stays quiet when a leg fails.

use steins_infer::{
    Diagnostic, Folder, OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID, check_full, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

/// A boot surface that is present and monkey-patch-free (A9 available), so the
/// value-domain offset proofs may fire. The offset family consults only
/// `absence_family_available`; the folder never folds and answers no homonyms.
struct Ready;

impl Folder for Ready {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
}

/// A boot surface that is unavailable (no sidecar / a monkey-patch extension loaded):
/// the whole family is silent (A9 / the sound subset).
struct Unavailable;

impl Folder for Unavailable {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        false
    }
}

fn offset_diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Ready)
        .into_iter()
        .filter(|d| d.id == OFFSET_MISSING_ID || d.id == OFFSET_ON_UNSUPPORTED_ID)
        .collect()
}

fn missing(src: &str) -> Vec<Diagnostic> {
    offset_diags(src).into_iter().filter(|d| d.id == OFFSET_MISSING_ID).collect()
}

fn on_unsupported(src: &str) -> Vec<Diagnostic> {
    offset_diags(src).into_iter().filter(|d| d.id == OFFSET_ON_UNSUPPORTED_ID).collect()
}

// ---------------------------------------------------------------------------
// Firing fixtures — every leg holds.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_empty_list_guard_branch() {
    // The conformance shape (`assertions_assert_non_empty_list`): the `=== []` branch
    // narrows the container to `Singleton([])`, so `$v[0]` provably reads null.
    let src = "<?php\nfunction f(array $v): int {\n    if ($v === []) {\n        return $v[0];\n    }\n    return $v[0];\n}\n";
    let d = missing(src);
    assert_eq!(d.len(), 1, "exactly the guarded branch fires: {d:#?}");
    assert_eq!(d[0].line, 4, "the `=== []` branch's read: {d:#?}");
    assert!(d[0].message.contains("offset 0 provably missing"), "{}", d[0].message);
    assert!(d[0].message.contains("Undefined array key 0"), "{}", d[0].message);
}

#[test]
fn fires_on_literal_array_missing_key() {
    let d = missing("<?php\n$a = ['x' => 1];\n$b = $a[0];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("Undefined array key 0"), "{}", d[0].message);
}

#[test]
fn fires_on_missing_string_key_with_php_phrasing() {
    let d = missing("<?php\n$a = [1, 2, 3];\n$b = $a['foo'];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    // PHP's verbatim consequence double-quotes a string key.
    assert!(d[0].message.contains("Undefined array key \"foo\""), "{}", d[0].message);
}

#[test]
fn fires_via_variable_key() {
    let d = missing("<?php\n$a = ['x' => 1];\n$k = 0;\n$b = $a[$k];\n");
    assert_eq!(d.len(), 1, "a proven Singleton key resolves through the env: {d:#?}");
}

#[test]
fn fires_on_oneof_all_arrays_missing() {
    // `$a` joins to `OneOf` of two arrays; neither carries key 0 ⇒ definite absence.
    let src = "<?php\nif (rand()) {\n    $a = ['x' => 1];\n} else {\n    $a = ['y' => 2];\n}\n$b = $a[0];\n";
    let d = missing(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("none carrying the key"), "{}", d[0].message);
}

#[test]
fn fires_on_null_base() {
    let d = on_unsupported("<?php\n$a = null;\n$b = $a[0];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("Trying to access array offset on null"), "{}", d[0].message);
}

#[test]
fn fires_on_int_base() {
    let d = on_unsupported("<?php\n$a = 5;\n$b = $a[0];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("Trying to access array offset on int"), "{}", d[0].message);
}

#[test]
fn fires_on_bool_base_with_true_false_word() {
    let d = on_unsupported("<?php\n$a = true;\n$b = $a[0];\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    // PHP names the concrete bool: `true`/`false`, not `bool`.
    assert!(d[0].message.contains("array offset on true"), "{}", d[0].message);
}

#[test]
fn fires_in_return_position() {
    let d = missing("<?php\nfunction f(): int {\n    $a = ['x' => 1];\n    return $a[0];\n}\n");
    assert_eq!(d.len(), 1, "a return operand is a whitelisted read: {d:#?}");
}

// ---------------------------------------------------------------------------
// Canonicalization (A10) — the same key primitive as the write side.
// ---------------------------------------------------------------------------

#[test]
fn string_five_is_the_present_int_key() {
    // `$a = [5 => 'v']; $a["5"]` — `"5"` canonicalizes to int 5, which is present.
    let d = missing("<?php\n$a = [5 => 'v'];\n$b = $a[\"5\"];\n");
    assert!(d.is_empty(), "\"5\" is the present key 5: {d:#?}");
}

#[test]
fn leading_zero_string_stays_a_missing_string_key() {
    // `"05"` is NOT canonical-int — it stays a string key, absent from `[5 => …]`.
    let d = missing("<?php\n$a = [5 => 'v'];\n$b = $a[\"05\"];\n");
    assert_eq!(d.len(), 1, "\"05\" stays a string key ⇒ missing: {d:#?}");
    assert!(d[0].message.contains("Undefined array key \"05\""), "{}", d[0].message);
}

#[test]
fn int_key_matches_string_keyed_literal() {
    // `$a = ["5" => 'v']` normalizes the key to int 5, so `$a[5]` is present.
    let d = missing("<?php\n$a = [\"5\" => 'v'];\n$b = $a[5];\n");
    assert!(d.is_empty(), "int 5 hits the normalized key 5: {d:#?}");
}

// ---------------------------------------------------------------------------
// Silence matrix (A7 read-context whitelist + §7 provability).
// ---------------------------------------------------------------------------

#[test]
fn silent_on_present_key() {
    assert!(missing("<?php\n$a = ['x' => 1];\n$b = $a['x'];\n").is_empty());
}

#[test]
fn silent_under_null_coalesce() {
    // `$a[0] ?? 'd'` is the isset-family silence leg — never a finding.
    assert!(offset_diags("<?php\n$a = [];\n$b = $a[0] ?? 'd';\n").is_empty());
}

#[test]
fn silent_in_isset() {
    assert!(offset_diags("<?php\n$a = [];\nif (isset($a[0])) { $x = 1; }\n").is_empty());
}

#[test]
fn silent_on_write_target() {
    // A write creates the key — the offset lvalue is never a read.
    assert!(offset_diags("<?php\n$a = [];\n$a[0] = 5;\n").is_empty());
}

#[test]
fn silent_as_call_argument() {
    // The call-argument read position is deferred (A7 by-ref / unresolved-callee
    // autovivification risk) — safe silence in v1.
    assert!(offset_diags("<?php\n$a = [];\nfoo($a[0]);\n").is_empty());
}

#[test]
fn silent_on_general_array_param() {
    // A declared `array` param is not a proven whole value — no `Singleton`, silent.
    assert!(offset_diags("<?php\nfunction f(array $v): int {\n    return $v[0];\n}\n").is_empty());
}

#[test]
fn silent_when_key_unproven() {
    // The container is proven but the key is an unproven value ⇒ cannot decide.
    assert!(offset_diags("<?php\nfunction f($k) {\n    $a = ['x' => 1];\n    return $a[$k];\n}\n").is_empty());
}

#[test]
fn silent_on_string_base() {
    // A string is offsetable — the in-range/uninitialized/TypeError split is deferred.
    assert!(offset_diags("<?php\n$a = 'hello';\n$b = $a[0];\n").is_empty());
}

#[test]
fn silent_on_oneof_with_a_member_that_has_the_key() {
    // The adversarial join case: one member carries key 0 ⇒ not a definite absence.
    let src = "<?php\nif (rand()) {\n    $a = [0 => 'here'];\n} else {\n    $a = ['y' => 2];\n}\n$b = $a[0];\n";
    assert!(missing(src).is_empty(), "a member with the key ⇒ silent");
}

#[test]
fn silent_when_a_conditional_write_may_add_the_key() {
    // `$a = []; if (cond) { $a[0] = 1; } $b = $a[0];` — the offset write invalidates
    // `$a`, so the read sees no proven container: silent (never a false positive).
    let src = "<?php\n$a = [];\nif (rand()) {\n    $a[0] = 1;\n}\n$b = $a[0];\n";
    assert!(missing(src).is_empty(), "a possible write ⇒ no proven absence ⇒ silent");
}

#[test]
fn silent_after_by_ref_argument_poisoning() {
    // Handing `$a` to a call may mutate it by reference (C3): the value is invalidated
    // after the call, so a later read proves nothing.
    let src = "<?php\n$a = ['x' => 1];\nmutate($a);\n$b = $a[0];\n";
    assert!(missing(src).is_empty(), "by-ref poisoning ⇒ silent");
}

// ---------------------------------------------------------------------------
// Stratum discipline (N2) — an Asserted narrowing never premises a proof.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_asserted_empty_singleton() {
    // `assert($v === [])` narrows to `Singleton([])` at the *Asserted* stratum by
    // default (`zend.assertions=-1`); proof-layer offset requires all-Verified.
    let src = "<?php\nfunction h(array $v): int {\n    assert($v === []);\n    return $v[0];\n}\n";
    assert!(offset_diags(src).is_empty(), "an Asserted Singleton must not fire");
}

#[test]
fn fires_on_asserted_empty_singleton_under_zend_assertions() {
    // Under `[runtime] zend-assertions = "enabled"` the same narrowing is Verified.
    let src = "<?php\nfunction h(array $v): int {\n    assert($v === []);\n    return $v[0];\n}\n";
    let tree = SourceTree::parse(src);
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Ready, true, true)
        .into_iter()
        .filter(|d| d.id == OFFSET_MISSING_ID)
        .collect();
    assert_eq!(d.len(), 1, "zend-assertions=enabled promotes the narrowing to Verified: {d:#?}");
}

// ---------------------------------------------------------------------------
// A9 availability + warning-handler posture (ADR-0049 §7).
// ---------------------------------------------------------------------------

#[test]
fn silent_without_a_sidecar() {
    let tree = SourceTree::parse("<?php\n$a = ['x' => 1];\n$b = $a[0];\n");
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Unavailable)
        .into_iter()
        .filter(|d| d.id == OFFSET_MISSING_ID || d.id == OFFSET_ON_UNSUPPORTED_ID)
        .collect();
    assert!(d.is_empty(), "the family is silent without a live boot surface (A9)");
}

#[test]
fn warning_handler_null_silences_warning_grade() {
    // Under `warning-handler = "null"` the application tolerates the warning: the
    // warning-grade finding leaves the proof surface.
    let tree = SourceTree::parse("<?php\n$a = ['x' => 1];\n$b = $a[0];\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Ready, false, false)
        .into_iter()
        .filter(|d| d.id == OFFSET_MISSING_ID)
        .collect();
    assert!(d.is_empty(), "\"null\" posture silences warning-grade offset findings");
}

#[test]
fn warning_handler_abort_emits() {
    let tree = SourceTree::parse("<?php\n$a = ['x' => 1];\n$b = $a[0];\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Ready, false, true)
        .into_iter()
        .filter(|d| d.id == OFFSET_MISSING_ID)
        .collect();
    assert_eq!(d.len(), 1, "the default \"abort\" posture emits: {d:#?}");
}
