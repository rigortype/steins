//! Regression tests for the `Opaque` **read-set** soundness fix.
//!
//! A control-flow construct that *reads* a variable may branch on it and
//! early-return, so the fall-through path can exclude the currently-known value.
//! Continuing with the binding intact asserts an unreachable path — a real
//! false-positive class observed in the field (a `?int` guard `if ($x == null)
//! { return; }` filters `null` out, yet the tail would still be analyzed with
//! `$x = null`). The fix invalidates a construct's read set as well as its write
//! set, from both the literal env and the exact-class env.

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
fn guard_reading_local_at_top_level_is_silent() {
    // A guard at the top level of a scope that reads the tracked local drops it:
    // the construct might branch on `$val` and skip the tail. Before the fix,
    // `$val = "abc"` survived the `if` and `width($val)` was flagged even though
    // the guard could have excluded "abc". Now the read of `$val` forgets it.
    let src = "<?php
declare(strict_types=1);
function width(int $w): int { return $w; }
$val = \"abc\";
if ($val !== \"\") { echo \"ok\"; }
width($val);
";
    assert_eq!(n(src), 0, "guard reading $val → forgotten → silent");
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
fn instanceof_guard_drops_exact_class_fact() {
    // An `instanceof` narrowing guard reads the object variable; the read must
    // drop its exact-class fact too, or method resolution on the fall-through
    // would assert an unreachable type. `if (!($x instanceof Foo)) { return; }`
    // reads `$x`, so `$x`'s exact-class fact is forgotten and `$x->m("abc")` no
    // longer resolves → silent (a missed finding, the FP-safe side).
    let src = "<?php
class Foo { public function m(int $w): void {} }
$x = new Foo();
if (!($x instanceof Foo)) { return; }
$x->m(\"abc\");
";
    assert_eq!(n(src), 0, "instanceof guard reads $x → class fact dropped → silent");
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
