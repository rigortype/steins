//! ADR-0049 §4 / N3: foldable existence-guard verdicts + the conservative
//! guard-respect leg. Two legs, both exercised here:
//!   1. `method_exists`/`function_exists`/`class_exists` in guard position folds to a
//!      Yes/No/Maybe verdict; ADR-0031 dead-region discipline then prunes a
//!      provably-dead branch (FP class 15: `if (!method_exists(C,'m')) return; C::m();`).
//!   2. An absence-family id DOMINATED by a positive same-symbol guard stays silent
//!      even on `Maybe` — the guard is programmer-supplied evidence.
//!
//! Drives the [`Boot`] mock folder (mirrors `undefined_method.rs`).

use steins_infer::{CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_METHOD_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A boot-surface mock: `available` is A9's family-availability gate; `class_builtins`/
/// `fn_builtins` are resident names (A2ii homonyms); `reflect_fails` sims a sidecar failure.
struct Boot {
    available: bool,
    class_builtins: Vec<String>,
    fn_builtins: Vec<String>,
    reflect_fails: bool,
}

impl Boot {
    /// Family available, empty boot surface (project symbols are never homonyms).
    fn ready() -> Self {
        Boot { available: true, class_builtins: Vec::new(), fn_builtins: Vec::new(), reflect_fails: false }
    }
}

impl Folder for Boot {
    fn fold(&mut self, _: &str, _: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        if self.reflect_fails {
            return None;
        }
        Some(self.class_builtins.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        if self.reflect_fails {
            return None;
        }
        Some(self.fn_builtins.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
}

fn diags_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
}

fn undef(src: &str) -> Vec<Diagnostic> {
    diags_with(src, &mut Boot::ready())
        .into_iter()
        .filter(|d| d.id == CALL_UNDEFINED_METHOD_ID)
        .collect()
}

// Leg 1: the verdict prunes the dead branch.

/// FP class 15 (phpstan-src `nsrt/static-has-method.php`): the `No` verdict proves the
/// `return` branch live and the fall-through call dead — catastrophic if it fired.
#[test]
fn nsrt_negated_guard_return_kills_the_fallthrough() {
    let src = "<?php
class rex_var {}
class HelloWorld {
    public function sayHello(): void {
        if (!method_exists(rex_var::class, 'varsIterator')) { return; }
        $it = rex_var::varsIterator();
    }
}
";
    assert_eq!(undef(src).len(), 0, "the call sits on a proven-dead path: {:?}", undef(src));
}

/// The same shape unguarded MUST still fire — the silence comes from the verdict,
/// not blanket-silencing of `rex_var::varsIterator()`.
#[test]
fn unguarded_absent_static_call_still_fires() {
    let src = "<?php
class rex_var {}
rex_var::varsIterator();
";
    assert_eq!(undef(src).len(), 1, "{:?}", undef(src));
}

/// The polarity crux: `No` makes the `return` then-branch DEAD, so control falls to
/// the call — the vouch binds only the dead true branch, so it MUST STILL FIRE here.
#[test]
fn positive_guard_true_branch_returns_fallthrough_call_fires() {
    let src = "<?php
class C {}
if (method_exists(C::class, 'm')) { return; }
(new C)->m();
";
    let d = undef(src);
    assert_eq!(d.len(), 1, "the fall-through is the guard-FALSE path — m is absent there: {d:?}");
}

/// Verdict `No` on `class_exists` prunes its then-branch: the call inside is dead.
#[test]
fn class_exists_absent_prunes_then_branch() {
    let src = "<?php
class C {}
if (class_exists('NopeAbsentClass')) {
    (new C)->undef();
}
";
    assert_eq!(undef(src).len(), 0, "then-branch is dead (class provably absent): {:?}", undef(src));
}

/// Verdict `No` on a `function_exists` guard prunes its then-branch likewise.
#[test]
fn function_exists_absent_prunes_then_branch() {
    let src = "<?php
class C {}
if (function_exists('nope_absent_function')) {
    (new C)->undef();
}
";
    assert_eq!(undef(src).len(), 0, "then-branch is dead (function provably absent): {:?}", undef(src));
}

// Verdict `Yes`: present symbol → true-branch live, call resolves, checks run.

/// A present method → `Yes` → true-branch live: a sibling absent call still fires.
#[test]
fn method_exists_present_keeps_true_branch_live() {
    let src = "<?php
class C { public function m(): void {} }
if (method_exists(C::class, 'm')) {
    (new C)->m();
    (new C)->absent();
}
";
    let d = undef(src);
    assert_eq!(d.len(), 1, "only the absent sibling call fires: {d:?}");
    assert!(d[0].message.contains("absent()"), "{}", d[0].message);
}

/// `Yes`-branch downstream checks still run: a too-few-arguments call raises arity.
#[test]
fn method_exists_present_branch_runs_downstream_arg_checks() {
    let src = "<?php
class C { public function m(int $a): void {} }
if (method_exists(C::class, 'm')) {
    (new C)->m();
}
";
    let arity: Vec<_> = diags_with(src, &mut Boot::ready())
        .into_iter()
        .filter(|d| d.id == CALL_TOO_FEW_ARGUMENTS_ID)
        .collect();
    assert_eq!(arity.len(), 1, "too-few-arguments fires inside the live true-branch: {arity:?}");
}

// Leg 2: the conservative guard-respect leg (Maybe verdict → vouch silence).

/// `method_exists($o,'m')` can't fold ($o isn't a literal class) → `Maybe`. S2 would
/// prove `$o->m()` absent, but the positive guard vouched `C::m`.
#[test]
fn instance_receiver_maybe_verdict_is_vouched_silent() {
    let src = "<?php
class C {}
$o = new C();
if (method_exists($o, 'm')) {
    $o->m();
}
";
    assert_eq!(undef(src).len(), 0, "the positive guard vouches C::m: {:?}", undef(src));
}

/// The vouch is scoped to the guarded symbol only: a different-method sibling call
/// still fires — the guard-respect leg is exact-textual.
#[test]
fn vouch_is_scoped_to_the_guarded_symbol() {
    let src = "<?php
class C {}
$o = new C();
if (method_exists($o, 'm')) {
    $o->m();
    (new C)->other();
}
";
    let d = undef(src);
    assert_eq!(d.len(), 1, "only the un-vouched sibling fires: {d:?}");
    assert!(d[0].message.contains("other()"), "{}", d[0].message);
}

/// The vouch does NOT leak past the `if`: `(new C)->m()` after an empty guarded branch
/// is un-guarded and fires (the join intersects the vouch away).
#[test]
fn vouch_does_not_leak_to_the_fallthrough_tail() {
    let src = "<?php
class C {}
$o = new C();
if (method_exists($o, 'm')) {}
(new C)->m();
";
    let d = undef(src);
    assert_eq!(d.len(), 1, "the tail call is outside the guard — must fire: {d:?}");
}

/// A trait-bearing (`uses_traits`) class taints closure → `Maybe`; the positive
/// guard's vouch silences the absence id (S2 is itself silent on trait-using classes).
#[test]
fn trait_bearing_chain_maybe_verdict_vouched_silent() {
    let src = "<?php
trait T {}
class C { use T; }
if (method_exists(C::class, 'm')) {
    (new C)->m();
}
";
    assert_eq!(undef(src).len(), 0, "{:?}", undef(src));
}

// Sidecar-availability gate (A9 / A2ii): no boot surface ⇒ no folding.

/// No boot surface ⇒ `Maybe` (sound subset): both paths walk, and S2 also can't fire —
/// silent for the honest reason (undecidable), not a prune.
#[test]
fn no_sidecar_no_folding() {
    let src = "<?php
class rex_var {}
if (!method_exists(rex_var::class, 'varsIterator')) { return; }
rex_var::varsIterator();
";
    let mut boot = Boot { available: false, ..Boot::ready() };
    let d: Vec<_> = diags_with(src, &mut boot)
        .into_iter()
        .filter(|x| x.id == CALL_UNDEFINED_METHOD_ID)
        .collect();
    assert_eq!(d.len(), 0, "no absence claim without a sidecar: {d:?}");
}

/// A homonym on the guard's class taints the verdict to `Maybe` (may be shadowed by a
/// resident builtin): fall-through stays live, but S2 is silenced by the same homonym.
#[test]
fn boot_surface_homonym_taints_the_verdict() {
    let src = "<?php
class rex_var {}
if (!method_exists(rex_var::class, 'varsIterator')) { return; }
rex_var::varsIterator();
";
    let mut boot = Boot { class_builtins: vec!["rex_var".into()], ..Boot::ready() };
    let d: Vec<_> = diags_with(src, &mut boot)
        .into_iter()
        .filter(|x| x.id == CALL_UNDEFINED_METHOD_ID)
        .collect();
    assert_eq!(d.len(), 0, "a boot-surface homonym is silence: {d:?}");
}

// function_exists / class_exists positive verdicts + the polyfill non-regression.

/// A catalog builtin → `function_exists` folds to `Yes`; a sibling absent call still fires.
#[test]
fn function_exists_builtin_yes_keeps_branch_live() {
    let src = "<?php
class C {}
if (function_exists('strlen')) {
    (new C)->undef();
}
";
    assert_eq!(undef(src).len(), 1, "strlen is a resident builtin — branch is live: {:?}", undef(src));
}

/// A present project class → `class_exists` folds to `Yes`; the branch stays live.
#[test]
fn class_exists_present_yes_keeps_branch_live() {
    let src = "<?php
class Widget {}
class C {}
if (class_exists('Widget')) {
    (new C)->undef();
}
";
    assert_eq!(undef(src).len(), 1, "Widget exists — branch is live: {:?}", undef(src));
}

/// The `function_exists`-guarded polyfill folds to `Maybe` — NEITHER branch is pruned,
/// probed by an absent-method call in the shadow branch that must stay live.
#[test]
fn function_exists_conditional_polyfill_is_maybe_no_prune() {
    let src = "<?php
class C {}
if (!function_exists('poly_f')) {
    function poly_f() {}
} else {
    (new C)->undef();
}
";
    assert_eq!(undef(src).len(), 1, "polyfill else-branch must not be pruned: {:?}", undef(src));
}
