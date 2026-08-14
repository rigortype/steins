//! Control-flow soundness around early-return guards.
//!
//! `Opaque` read-set invalidation drops a read variable because the construct may
//! branch and early-return, excluding the known value on fall-through (ADR-0027).
//! Structured `if`/`elseif`/`else` uses branch analysis instead (ADR-0031); the
//! read-set rule still applies to opaque constructs/conditions, e.g. a by-ref call in a guard.

use steins_infer::{Diagnostic, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    // `untyped.*` (ADR-0078, #200) reports on the fixtures' own deliberately-untyped
    // declarations, not the behaviour under test — dropped to keep counts stable.
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn n(src: &str) -> usize {
    findings(src).len()
}

// The field reproduction: guard inside a descended callee

#[test]
fn null_guard_in_descended_callee_is_silent() {
    // Field shape: `getRole(null)` binds `$user_id = null`; the `if ($user_id == null)
    // { return 'guest'; }` guard FILTERS null, so `check($user_id)` on fall-through
    // can't see it. Before the fix, the guard (write-only) let the binding survive and
    // flagged a provably-unreachable null (FP); the guard now READS `$user_id`, dropping it.
    let src = "<?php
declare(strict_types=1);
function check(int $user_id): bool { return $user_id > 0; }
function getRole(?int $user_id): string
{
    if ($user_id == null) { return 'guest'; }
    check($user_id);
    return 'user';
}
getRole(null);
";
    assert_eq!(n(src), 0, "null guard filters null before check() → silent (no FP)");
}

#[test]
fn false_guard_in_descended_callee_is_silent() {
    // Second observed shape: an `=== false` guard. `make(false)` binds `$token =
    // false`; `if ($token === false) { return; }` filters it, so `use_token` never sees false.
    let src = "<?php
declare(strict_types=1);
function use_token(string $token): void {}
function make(string|bool $token): void
{
    if ($token === false) { return; }
    use_token($token);
}
make(false);
";
    assert_eq!(n(src), 0, "false guard filters false before use_token() → silent (no FP)");
}

// The top-level guard shape

#[test]
fn guard_reading_local_survives_structured_if() {
    // EXPECTATION CHANGE (ADR-0031, was `..._is_silent` → 0): structured `if` no
    // longer blanket-invalidates a variable merely *read* by a branch — modeled
    // instead of forgotten. Here `$val = "abc"`, guard `$val !== ""` is provably
    // TRUE (then-branch is the only live path and falls through), and `echo $val`
    // only READS it, not filters — the fact survives and `width($val)` is FLAGGED.
    let src = "<?php
declare(strict_types=1);
function width(int $w): int { return $w; }
$val = \"abc\";
if ($cond) { echo $val; }
width($val);
";
    let f = findings(src);
    assert_eq!(f.len(), 1, "echo reads $val but does not filter it → still flagged: {f:#?}");
    assert!(f[0].message.contains("argument \"abc\""), "{}", f[0].message);
}

// Precision preserved: reads of OTHER variables keep the fact

#[test]
fn construct_reading_other_var_preserves_unrelated_fact() {
    // Read-set must not over-forget: a construct reading/writing only OTHER vars
    // leaves tracked `$w` known, so the TypeError still FIRES. Here `if` reads
    // `$cond` and calls `use_it($cond)`; neither `reads` nor `writes` mentions `$w`.
    let src = "<?php
function width(int $w): int { return $w; }
function use_it($c): void {}
$w = \"abc\";
if ($cond) { use_it($cond); }
width($w);
";
    let f = findings(src);
    assert_eq!(f.len(), 1, "unrelated construct preserves $w → still flagged: {f:#?}");
    assert!(f[0].message.contains("argument \"abc\""), "{}", f[0].message);
    assert!(f[0].message.contains("from $w"), "{}", f[0].message);
}

// instanceof guard filters exact-class facts the same way

#[test]
fn instanceof_guard_prunes_dead_return_path() {
    // EXPECTATION CHANGE (ADR-0031, was `..._drops_exact_class_fact` → 0): met now
    // by *branch pruning*, not forgetting. `$x = new Foo()` proves `$x instanceof
    // Foo` (Yes), so `!(...)` is No: the early-`return` then-branch is DEAD, the
    // fall-through keeps `$x`'s exact class, and `$x->m("abc")` resolves + FLAGS.
    let src = "<?php
class Foo { public function m(int $w): void {} }
$x = new Foo();
if (!($x instanceof Foo)) { return; }
$x->m(\"abc\");
";
    assert_eq!(n(src), 1, "instanceof-true → early-return path dead → $x survives → flagged");
}

#[test]
fn method_call_without_guard_still_resolves() {
    // Control: with no guard reading `$x`, the exact-class fact survives and fires —
    // proving the previous test's silence is the guard's read, not a broken fact.
    let src = "<?php
class Foo { public function m(int $w): void {} }
$x = new Foo();
$x->m(\"abc\");
";
    assert_eq!(n(src), 1, "no guard → class fact survives → flagged");
}
