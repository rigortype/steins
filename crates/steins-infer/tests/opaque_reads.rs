//! Regression tests for control-flow soundness around early-return guards.
//!
//! The original mechanism (ADR-0027) was the `Opaque` **read-set** invalidation: a
//! construct that *reads* a variable dropped it, because the construct might branch
//! on it and early-return, so the fall-through could exclude the known value.
//!
//! Under ADR-0031, `if`/`elseif`/`else` is **structured** — its control flow is
//! modeled, not erased — so the guard cases here are now handled by real branch
//! analysis (dead-path pruning, fall-through joins) rather than by blanket
//! read-invalidation. The read-set rule still governs the constructs that remain
//! `Opaque` (loops, `switch`, `try`), and it still governs *opaque conditions*
//! (a by-ref call in a guard). Two tests below record the resulting precision
//! gains (see their EXPECTATION CHANGE notes).

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

// ---- The field reproduction: guard inside a descended callee --------------

#[test]
fn null_guard_in_descended_callee_is_silent() {
    // The exact anonymized field shape. `getRole(null)` descends into getRole
    // binding `$user_id = null`; the `if ($user_id == null) { return 'guest'; }`
    // guard FILTERS null out, so `check($user_id)` on the fall-through can never
    // see null. Before the fix, the binding survived the guard (it only writes,
    // and the guard writes nothing) and `check(int $user_id)` was flagged for a
    // null that is provably unreachable — a false positive. The guard now READS
    // `$user_id`, dropping the binding → silent.
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
    // The second observed shape: an `=== false` early-return guard. `make(false)`
    // descends binding `$token = false`; `if ($token === false) { return; }`
    // filters it out, so `use_token(string $token)` never sees the bool false.
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

// ---- The top-level guard shape --------------------------------------------

#[test]
fn guard_reading_local_survives_structured_if() {
    // EXPECTATION CHANGE (ADR-0031, was `..._is_silent` → 0): the structured `if`
    // no longer blanket-invalidates a variable merely *read* by a branch. This is
    // the precision payoff. Original intent — "a guard that could exclude a value
    // must not keep it on an unreachable path" — is now enforced by *modeling* the
    // control flow instead of by forgetting: here `$val = "abc"`, the guard
    // `$val !== ""` is provably TRUE (so the then-branch is the only live path and
    // it falls through), and `echo $val` merely READS `$val` without filtering it.
    // The fact survives, and the proven TypeError at `width($val)` is now FLAGGED
    // (the ADR-0031 read-of-$w-no-longer-kills-the-fact case).
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

// ---- Precision preserved: reads of OTHER variables keep the fact ----------

#[test]
fn construct_reading_other_var_preserves_unrelated_fact() {
    // The read-set must not over-forget: a construct that reads/writes only OTHER
    // variables leaves the tracked `$w` known, so the proven TypeError still
    // FIRES. Here the `if` reads `$cond` and calls `use_it($cond)`; neither `reads`
    // nor `writes` mentions `$w`.
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

// ---- instanceof guard filters exact-class facts the same way --------------

#[test]
fn instanceof_guard_prunes_dead_return_path() {
    // EXPECTATION CHANGE (ADR-0031, was `..._drops_exact_class_fact` → 0): the
    // original intent — "an `instanceof` guard must not let a fall-through assert an
    // unreachable type" — is now met by *branch pruning* rather than by forgetting
    // the fact. `$x = new Foo()` proves `$x instanceof Foo` (verdict Yes), so
    // `!(...)` is No: the early-`return` then-branch is DEAD, the fall-through keeps
    // `$x`'s exact class, and `$x->m("abc")` resolves + is FLAGGED. The bad path the
    // old read-invalidation feared is proven dead, not merely forgotten.
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
    // Control: with no intervening guard reading `$x`, the exact-class fact
    // survives and `$x->m("abc")` is flagged — proving the previous test's
    // silence is caused by the guard's read, not by a broken class fact.
    let src = "<?php
class Foo { public function m(int $w): void {} }
$x = new Foo();
$x->m(\"abc\");
";
    assert_eq!(n(src), 1, "no guard → class fact survives → flagged");
}
