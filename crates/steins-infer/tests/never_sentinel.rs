//! ADR-0088 §3 / issue #428: the **sentinel parameter**. A `@param never`
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
    // the sentinel on the `else` branch — the case analysis is NOT exhaustive.
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

// ---- `never` never reaches `phpdoc.param-mismatch`, under any profile ----

#[test]
fn a_never_parameter_never_reaches_param_mismatch_exhausted_or_not() {
    // Exhausted case (would have fired the OLD check).
    let exhausted = "/** @param 1|2 $foo */\nfunction h(int $foo): void {\n\tif ($foo === 1) { echo 1; }\n\telseif ($foo === 2) { echo 2; }\n\telse { assertNever($foo); }\n}\n";
    assert!(param_mismatch(exhausted).is_empty(), "{:?}", param_mismatch(exhausted));

    // Reachable case — reports `phpdoc.never-param-reachable`, never
    // `phpdoc.param-mismatch` (one id, one remedy — ADR-0088 design ruling 1).
    let reachable = "/** @param string|int $foo */\nfunction h(string|int $foo): void {\n\tif (is_string($foo)) { echo 1; }\n\telse { assertNever($foo); }\n}\n";
    assert!(param_mismatch(reachable).is_empty(), "{:?}", param_mismatch(reachable));

    // An unguarded, always-reachable call — the simplest possible case.
    let unguarded = "function h(int $foo): void {\n\tassertNever($foo);\n}\n";
    assert!(param_mismatch(unguarded).is_empty(), "{:?}", param_mismatch(unguarded));
}

#[test]
fn an_unguarded_call_reports_reachable_with_the_native_type() {
    // No docblock, no narrowing at all — the native declaration is the only
    // declared domain, and it plainly still reaches the sentinel.
    let d = never_reachable("function h(int $foo): void {\n\tassertNever($foo);\n}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("int"), "{}", d[0].message);
}
