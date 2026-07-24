//! Value-side `instanceof` verdicts (field survey FP class 14): a value that is
//! not an object is never an instance of anything.
//!
//! When a binding descent imports a call-site `null` argument as a parameter's
//! fact (`Singleton(null)`), an `instanceof` guard on that parameter must REFUTE
//! it: `null instanceof T` is `false` for every `T` in PHP, so the then-branch is
//! dead on that path. Before the value-side pre-check, `eval_instanceof` answered
//! `Maybe` for a non-object-valued operand (no heap object → `member_instanceof`
//! → `Maybe`), the then-branch walked LIVE with the null fact intact, and a
//! `call.on-null` "proven null on this path" fired INSIDE the guard that proves
//! the opposite — 3 live proof-layer false positives in the field (kimai ×2,
//! firefly-iii ×1).
//!
//! The rule is a **value-side** verdict that precedes all class reasoning: if the
//! operand's value-domain fact proves a non-object value (`null`/int/float/string/
//! bool/array), answer `No`. Sound unconditionally — it needs no class resolution
//! and no exactness (the G1 exactness discipline is scoped to object-class `No`
//! verdicts on the heap path and is untouched).

use steins_infer::{Diagnostic, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

fn ids(src: &str) -> Vec<String> {
    findings(src).into_iter().map(|d| d.id.to_owned()).collect()
}

// ==========================================================================
// 1. The isolated repro cases (case4 / case5) — FP before, SILENT after.
//    Descent imports the caller's `null` as the param fact; the `instanceof`
//    guard refutes it → then-branch dead → no `call.on-null`. Class resolution
//    is NOT required (case4 uses an unresolvable class, case5 a defined one).
// ==========================================================================

#[test]
fn case4_descent_null_unresolvable_class_is_silent() {
    // `instanceof UndefCarbon4` against an UNRESOLVABLE class; caller passes null.
    let src = "<?php
class Enrich4 {
    public function setDate(?UndefCarbon4 $date): void
    {
        if ($date instanceof UndefCarbon4) {
            $date->endOfDay();
        }
    }
}
function caller4(): void {
    $e = new Enrich4();
    $e->setDate(null);
}
";
    assert_eq!(n(src), 0, "null instanceof (unresolvable) → No → then-branch dead → no call.on-null");
}

#[test]
fn case5_descent_null_defined_class_is_silent() {
    // `instanceof DefCarbon5` against a DEFINED class; caller passes null.
    let src = "<?php
class DefCarbon5 { public function endOfDay(): void {} }
class Enrich5 {
    public function setDate(?DefCarbon5 $date): void
    {
        if ($date instanceof DefCarbon5) {
            $date->endOfDay();
        }
    }
}
function caller5(): void {
    $e = new Enrich5();
    $e->setDate(null);
}
";
    assert_eq!(n(src), 0, "null instanceof (defined) → No → then-branch dead → no call.on-null");
}

// ==========================================================================
// 2. The kimai field shape: `mixed $value`, `instanceof \DateTimeInterface`
//    (a BUILTIN interface), a separate caller passing null. Plus the
//    non-monotonicity pin: the same class WITHOUT the null-passing caller must
//    produce identical (zero) findings — adding the caller must not flip a
//    verdict on the guarded line.
// ==========================================================================

#[test]
fn kimai_shape_mixed_builtin_interface_null_caller_is_silent() {
    let src = "<?php
class DateStringFormatter {
    public function formatValue(mixed $value): string
    {
        if ($value instanceof \\DateTimeInterface) {
            return $value->format('Y-m-d');
        }
        return (string) $value;
    }
}
function kimai_caller(): void {
    $f = new DateStringFormatter();
    $f->formatValue(null);
}
";
    assert_eq!(n(src), 0, "null instanceof \\DateTimeInterface (builtin) → No → guarded call silent");
}

#[test]
fn kimai_shape_without_null_caller_is_identical_zero() {
    // Non-monotonicity pin: the class alone, no caller feeding null. The guarded
    // line must be equally silent — adding the null-passing caller (test above)
    // must not FLIP any verdict on the guarded line.
    let src = "<?php
class DateStringFormatter {
    public function formatValue(mixed $value): string
    {
        if ($value instanceof \\DateTimeInterface) {
            return $value->format('Y-m-d');
        }
        return (string) $value;
    }
}
";
    assert_eq!(n(src), 0, "class alone → zero; identical to the with-null-caller shape");
}

// ==========================================================================
// 3. The firefly-iii field shape: `?Carbon`-style nullable class param.
//    (a) caller passes null → guarded call silent.
//    (b) caller passes a REAL instance → the guarded call still resolves/checks
//        (no lost precision — the heap-object path is untouched).
// ==========================================================================

#[test]
fn firefly_shape_nullable_class_null_caller_is_silent() {
    let src = "<?php
class Carbon3 { public function endOfDay(): void {} }
class AccountEnrichment {
    public function enrich(?Carbon3 $date): void
    {
        if ($date instanceof Carbon3) {
            $date->endOfDay();
        }
    }
}
function firefly_caller(): void {
    $e = new AccountEnrichment();
    $e->enrich(null);
}
";
    assert_eq!(n(src), 0, "?Carbon null caller → instanceof No → guarded call silent");
}

#[test]
fn firefly_shape_real_instance_caller_keeps_branch_live() {
    // A REAL instance is NOT a proven non-object value — the value-side rule never
    // fires; the then-branch stays LIVE (no lost precision). A wrong-typed free
    // call placed inside the guard is therefore reached and flagged, proving the
    // branch was not silenced. (Contrast the null caller, where the same branch is
    // dead and this call would be pruned.)
    let src = "<?php declare(strict_types=1);
function width3(int $w): int { return $w; }
class Carbon3b { public function endOfDay(): void {} }
class AccountEnrichment2 {
    public function enrich(?Carbon3b $date): void
    {
        if ($date instanceof Carbon3b) {
            width3(\"nope\");
        }
    }
}
function firefly_caller2(): void {
    $e = new AccountEnrichment2();
    $e->enrich(new Carbon3b());
}
";
    assert_eq!(
        ids(src),
        vec!["type.argument-mismatch"],
        "real instance → not proven non-object → branch live → bad call inside guard fires"
    );
}

// ==========================================================================
// 4. Boundary pins from the survey.
// ==========================================================================

#[test]
fn boundary_eq_null_first_guard_stays_clean() {
    // The `=== null`-first lane already refuted the premise (early return) — the
    // instanceof site is never reached with null. Must stay clean.
    let src = "<?php
class Carbon4 { public function endOfDay(): void {} }
class Enrich6 {
    public function setDate(?Carbon4 $date): void
    {
        if ($date === null) { return; }
        if ($date instanceof Carbon4) {
            $date->endOfDay();
        }
    }
}
function caller6(): void {
    $e = new Enrich6();
    $e->setDate(null);
}
";
    assert_eq!(n(src), 0, "=== null early-return → instanceof site unreached with null → clean");
}

#[test]
fn boundary_no_null_check_sibling_stays_clean() {
    // `instanceof` with NO `=== null` sibling at all — still clean (the value-side
    // No does not depend on the presence of any sibling null check).
    let src = "<?php
class Carbon5 { public function endOfDay(): void {} }
class Enrich7 {
    public function setDate(?Carbon5 $date): void
    {
        if ($date instanceof Carbon5) {
            $date->endOfDay();
        }
    }
}
function caller7(): void {
    $e = new Enrich7();
    $e->setDate(null);
}
";
    assert_eq!(n(src), 0, "instanceof, no null-check sibling → value-side No → clean");
}

// ==========================================================================
// 5. Value-side matrix: every proven non-object operand → instanceof No (dead
//    then-branch, no finding, ELSE branch stays live); a genuine could-be-object
//    operand → Maybe (both branches live). `width(int)` fires on a bad arg.
// ==========================================================================

const HDR: &str = "<?php declare(strict_types=1);
function width(int $w): int { return $w; }
class Thing { public function m(): void {} }
";

#[test]
fn matrix_singleton_int_operand_is_no_else_lives() {
    // $x is int 5 → instanceof No → then dead; the else runs and a bad arg fires.
    let src = format!(
        "{HDR}$x = 5;\nif ($x instanceof Thing) {{ $x->m(); }} else {{ width(\"bad\"); }}"
    );
    assert_eq!(ids(&src), vec!["type.argument-mismatch"], "int instanceof → No → else live → fires");
}

#[test]
fn matrix_singleton_string_operand_is_no() {
    let src = format!("{HDR}$x = \"s\";\nif ($x instanceof Thing) {{ $x->m(); }}");
    assert_eq!(n(&src), 0, "string instanceof → No → then dead → silent");
}

#[test]
fn matrix_singleton_bool_operand_is_no() {
    let src = format!("{HDR}$x = true;\nif ($x instanceof Thing) {{ $x->m(); }}");
    assert_eq!(n(&src), 0, "bool instanceof → No → then dead → silent");
}

#[test]
fn matrix_singleton_array_operand_is_no() {
    let src = format!("{HDR}$x = [1, 2];\nif ($x instanceof Thing) {{ $x->m(); }}");
    assert_eq!(n(&src), 0, "array instanceof → No → then dead → silent");
}

#[test]
fn matrix_oneof_all_non_object_operand_is_no() {
    // An undecided OneOf of two non-object values (int | string) → every member
    // non-object → No.
    let src = format!("{HDR}$x = $c ? 1 : \"s\";\nif ($x instanceof Thing) {{ $x->m(); }}");
    assert_eq!(n(&src), 0, "OneOf(int,string) → all non-object → No → then dead → silent");
}

#[test]
fn matrix_could_be_object_or_null_is_maybe_both_live() {
    // A `?Thing` param with NO descent binding it — the analyzer holds no value
    // fact (an object never lives in the value domain; the un-narrowed param could
    // be a Thing or null). `instanceof` stays Maybe: both branches live, and no
    // proof finding is manufactured on either.
    let src = "<?php declare(strict_types=1);
class Thing2 { public function m(): void {} }
function handle(?Thing2 $x): void {
    if ($x instanceof Thing2) { $x->m(); } else { $x; }
}
";
    assert_eq!(n(src), 0, "un-narrowed ?Thing → Maybe → both branches live → no proof finding");
}

// ==========================================================================
// 6. TRUE-positive retention: the value-side rule silences ONLY the genuinely
//    dead branch — a real null dereference must still fire.
// ==========================================================================

#[test]
fn retention_unguarded_null_deref_still_fires() {
    let src = "<?php
class U10 { public function m(): void {} }
function f10(): void {
    $x = null;
    $x->m();
}
";
    assert_eq!(ids(src), vec!["call.on-null"], "unguarded null deref → still fires");
}

#[test]
fn retention_negated_instanceof_reaches_null_deref_fires() {
    // `!($x instanceof T)` with $x null from the caller: `!(null instanceof T)` =
    // `!(false)` = true → the guarded call IS reached WITH null → a TRUE
    // `call.on-null` must fire on that live path. (eval_instanceof No → Not → Yes
    // → then-branch live, null fact intact.)
    let src = "<?php
class Carbon11 { public function endOfDay(): void {} }
class Enrich11 {
    public function setDate(?Carbon11 $date): void
    {
        if (!($date instanceof Carbon11)) {
            $date->endOfDay();
        }
    }
}
function caller11(): void {
    $e = new Enrich11();
    $e->setDate(null);
}
";
    assert_eq!(
        ids(src),
        vec!["call.on-null"],
        "!(null instanceof T) = true → guarded call reached with null → TRUE finding fires"
    );
}
