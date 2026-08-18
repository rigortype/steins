//! ADR-0088 §4 / issue #428: the **sentinel parameter**. A `@param never`
//! parameter is an author's explicit claim that no call reaches it — the
//! `default => assertNever($foo)` idiom an exhaustive `if`/`elseif` chain (or,
//! once value-position `match` is structured, a `match`) protects itself with.
//!
//! Two things had to change together:
//!
//! * `never` leaves `phpdoc.param-mismatch` entirely (not demoted, not
//!   reworded) — `never` is uninhabited, so the ordinary declared-contract
//!   acceptance path (`ContractTy::Never => No`, unconditionally) would convict
//!   every argument trivially, and the remedy it names ("fix this argument") is
//!   the wrong one for what is actually an incomplete case analysis upstream.
//! * the replacement id, `phpdoc.never-param-reachable`, asks its emptiness
//!   question at the **most-refined declared** grade — the `@param`-refined
//!   domain where a docblock narrows the argument's own native declaration, the
//!   native declaration alone otherwise — rather than the bare native
//!   (Verified) type the old path asked. That is the bug fixed here,
//!   reproducible on `master` with no `match` involved at all: an `elseif`
//!   chain narrows a phpdoc-refined `@param 1|2 $foo` over a native `int $foo`
//!   to nothing, but the OLD check asked the native `int` grade, which
//!   subtraction over two literal exclusions cannot empty.
//!
//! A **post-review amendment** (issue #428, audited) added a second gate on the
//! `$var` path: reporting requires not just a non-empty arm lane but a **proven
//! narrowing** — [`steins_infer`]'s `Store::contract_narrowed` bit, set only
//! where a subtraction demonstrably killed or shrank an arm. Without it, a guard
//! shape the arm lane cannot yet model (enum-case identity, boolean-literal
//! equality — issue #429's job) leaves the lane at its full seeded declaration,
//! and an exhaustive `if`/`elseif` over every enum case or both booleans read as
//! "still reaches" on a lane nothing ever touched — two false-positive classes
//! caught after the first version of this file landed. The trade: a completely
//! unguarded call (no chain above it at all) now declines too, since its lane is
//! equally untouched. See `an_unguarded_call_declines_an_untouched_lane`.
//!
//! Every fixture below calls a shared sentinel:
//! ```php
//! /** @param never $value */
//! function assertNever(mixed $value): never { throw new LogicException(); }
//! ```
//! and pins one cell of the taxonomy via an `if`/`elseif` chain — no `match`,
//! which is a separate, not-yet-landed slice (issue #427's blocking note).

use steins_infer::{NEVER_PARAM_REACHABLE_ID, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

const SENTINEL: &str = "/** @param never $value */\nfunction assertNever(mixed $value): never { throw new LogicException(); }\n";

fn run(src: &str) -> Vec<steins_infer::Diagnostic> {
    let full = format!("<?php\n{SENTINEL}{src}");
    let tree = SourceTree::parse(&full);
    check(&tree, &[], "t.php")
}

fn never_reachable(src: &str) -> Vec<steins_infer::Diagnostic> {
    run(src).into_iter().filter(|d| d.id == NEVER_PARAM_REACHABLE_ID).collect()
}

fn param_mismatch(src: &str) -> Vec<steins_infer::Diagnostic> {
    run(src).into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).collect()
}

// ---- The four taxonomy cells (ADR-0088's worked example, `if`/`elseif` form) ----

#[test]
fn native_union_exhausted_is_silent() {
    // Verified-only premise: no `@param`, the native `string|int` union alone.
    // `is_string`/`is_int` cover it exhaustively — the sentinel is unreachable.
    let d = never_reachable(
        "/** @param string|int $foo */\nfunction h(string|int $foo): void {\n\tif (is_string($foo)) { echo 1; }\n\telseif (is_int($foo)) { echo 2; }\n\telse { assertNever($foo); }\n}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn mixed_plus_phpdoc_union_exhausted_is_silent() {
    // Asserted-only premise: native `mixed` carries no shape of its own; the
    // `@param string|int` docblock is the only declared domain, and the
    // `is_string`/`is_int` pair exhausts it just as completely.
    let d = never_reachable(
        "/** @param string|int $foo */\nfunction h(mixed $foo): void {\n\tif (is_string($foo)) { echo 1; }\n\telseif (is_int($foo)) { echo 2; }\n\telse { assertNever($foo); }\n}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn int_plus_phpdoc_literal_union_exhausted_is_silent() {
    // Both premises: native `int` (Verified) refined by `@param 1|2` (Asserted).
    // This is the regression the fix repairs — on master this fires
    // `phpdoc.param-mismatch` because the old check asked the Verified `int`
    // grade (which two `!==` exclusions cannot empty), not the Asserted `1|2`
    // grade the `elseif` chain actually narrows to nothing.
    let src = "/** @param 1|2 $foo */\nfunction h(int $foo): void {\n\tif ($foo === 1) { echo 1; }\n\telseif ($foo === 2) { echo 2; }\n\telse { assertNever($foo); }\n}\n";
    assert!(never_reachable(src).is_empty(), "{:?}", never_reachable(src));
    assert!(param_mismatch(src).is_empty(), "{:?}", param_mismatch(src));
}

#[test]
fn a_genuinely_reachable_sentinel_reports_the_surviving_type() {
    // Only `is_string` is handled; `is_int` never runs, so `int` still reaches
    // the sentinel on the `else` branch — the subtraction that killed the
    // `string` arm on this path is a proven narrowing.
    let d = never_reachable(
        "/** @param string|int $foo */\nfunction h(string|int $foo): void {\n\tif (is_string($foo)) { echo 1; }\n\telse { assertNever($foo); }\n}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("int"), "{}", d[0].message);
    assert!(d[0].message.contains("assertNever"), "{}", d[0].message);
    assert!(
        d[0].message.contains("@param never $value"),
        "{}",
        d[0].message
    );
}

// ---- The proven-narrowing amendment: two false-positive classes, and their guard ----

#[test]
fn an_enum_exhausted_over_every_case_is_silent() {
    // The arm lane cannot yet subtract an enum-case identity guard (issue #429).
    // A lane nothing touched must NOT read as "still reaches" — the false
    // positive audit found reproducible on this exact shape.
    let d = never_reachable(
        "enum Suit { case Hearts; case Spades; case Clubs; }\nfunction e2(Suit $s): void {\n\tif ($s === Suit::Hearts) { echo 1; }\n\telseif ($s === Suit::Spades) { echo 2; }\n\telseif ($s === Suit::Clubs) { echo 3; }\n\telse { assertNever($s); }\n}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_bool_covered_by_both_literals_is_silent() {
    // Same root cause as the enum cell: the arm lane cannot yet subtract a
    // boolean-literal equality guard, so an exhaustive `true`/`false` pair must
    // not read as reachable either.
    let d = never_reachable(
        "function k(bool $b): void {\n\tif ($b === true) { echo 1; }\n\telseif ($b === false) { echo 2; }\n\telse { assertNever($b); }\n}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_native_union_missing_an_arm_still_reports() {
    // Regression guard for the proven-narrowing rule: it must not swing so far
    // that it silences the cells that matter. `is_string` narrows the union
    // (kills the `string` arm on this path) and leaves `int` un-excluded.
    let d = never_reachable(
        "function h(string|int $foo): void {\n\tif (is_string($foo)) { echo 1; }\n\telse { assertNever($foo); }\n}\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("int"), "{}", d[0].message);
}

#[test]
fn an_instanceof_chain_over_two_classes_exhausted_is_silent() {
    // `instanceof` narrows the arm lane correctly (already true before this
    // amendment) — pinned here so a later change to the proven-narrowing gate
    // cannot regress it.
    let d = never_reachable(
        "class Circ {}\nclass Sq {}\nfunction h(Circ|Sq $shape): void {\n\tif ($shape instanceof Circ) { echo 1; }\n\telseif ($shape instanceof Sq) { echo 2; }\n\telse { assertNever($shape); }\n}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn an_unguarded_call_declines_an_untouched_lane() {
    // No chain above it at all: the argument's arm lane is exactly its full
    // seeded declaration, indistinguishable from an exhausted-but-unmodelled
    // guard shape (the enum/bool cells above) without a narrowing proof. This
    // cell is a deliberate, accepted casualty of the proven-narrowing rule — an
    // unguarded call to `assertNever` is in fact always reachable, but the
    // check cannot tell that apart from "nothing has looked at this yet".
    let d = never_reachable("function h(int $foo): void {\n\tassertNever($foo);\n}\n");
    assert!(d.is_empty(), "{d:?}");
}

// ---- `never` never reaches `phpdoc.param-mismatch`, under any profile ----

#[test]
fn a_never_parameter_never_reaches_param_mismatch_exhausted_or_not() {
    // Exhausted case (would have fired the OLD check).
    let exhausted = "/** @param 1|2 $foo */\nfunction h(int $foo): void {\n\tif ($foo === 1) { echo 1; }\n\telseif ($foo === 2) { echo 2; }\n\telse { assertNever($foo); }\n}\n";
    assert!(param_mismatch(exhausted).is_empty(), "{:?}", param_mismatch(exhausted));

    // Reachable case — reports `phpdoc.never-param-reachable`, never
    // `phpdoc.param-mismatch` (one id, one remedy — ADR-0088 §4).
    let reachable = "/** @param string|int $foo */\nfunction h(string|int $foo): void {\n\tif (is_string($foo)) { echo 1; }\n\telse { assertNever($foo); }\n}\n";
    assert!(param_mismatch(reachable).is_empty(), "{:?}", param_mismatch(reachable));

    // An unguarded call — silent on both ids now.
    let unguarded = "function h(int $foo): void {\n\tassertNever($foo);\n}\n";
    assert!(param_mismatch(unguarded).is_empty(), "{:?}", param_mismatch(unguarded));

    // The enum cell — silent on both ids too (the false-positive class this
    // amendment fixes was specifically about the NEW id, but the old id must
    // stay excluded here regardless).
    let enum_case = "enum Suit { case Hearts; case Spades; case Clubs; }\nfunction e2(Suit $s): void {\n\tif ($s === Suit::Hearts) { echo 1; }\n\telseif ($s === Suit::Spades) { echo 2; }\n\telseif ($s === Suit::Clubs) { echo 3; }\n\telse { assertNever($s); }\n}\n";
    assert!(param_mismatch(enum_case).is_empty(), "{:?}", param_mismatch(enum_case));
}
