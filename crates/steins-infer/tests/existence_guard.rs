//! ADR-0049 §4 / N3: foldable existence-guard verdicts + the conservative
//! guard-respect leg.
//!
//! Two legs, both exercised here:
//!   1. A `method_exists`/`function_exists`/`class_exists` … call in guard position
//!      folds to a real Yes/No/Maybe verdict against the closed world, so the
//!      ADR-0031 dead-region discipline prunes the branch the runtime provably never
//!      takes. The catastrophic FP class 15 (`if (!method_exists(C, 'm')) { return; }
//!      C::m();`) dies because the fall-through is proven dead.
//!   2. An absence-family id at a site DOMINATED by a positive same-symbol existence
//!      guard stays silent even on a `Maybe` verdict — the guard is programmer-supplied
//!      evidence (the instance-receiver idiom `if (method_exists($o,'m')) { $o->m(); }`).
//!
//! The verdict rests on the same runtime boot surface the absence family asks, so
//! these tests drive the [`Boot`] mock folder (mirroring `undefined_method.rs`).

use steins_infer::{CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_METHOD_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A boot-surface mock: `available` is A9's family-availability gate; `class_builtins`
/// / `fn_builtins` are the lowercased names the boot surface reports as resident
/// (the A2ii homonyms); `reflect_fails` simulates a mid-run sidecar failure.
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

// ---------------------------------------------------------------------------
// Leg 1: the verdict prunes the dead branch.
// ---------------------------------------------------------------------------

/// The reproduced FP class 15 (phpstan-src `nsrt/static-has-method.php`): the negated
/// guard returns, so the fall-through is proven dead — the `No` verdict makes
/// `!method_exists(...)` true, the `return` branch live, the call unreachable. Silence
/// is the only correct verdict. This is the shape that would be catastrophic in the
/// field (`if (method_exists($x,'m')) { $x->m(); }` is pervasive).
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

/// The same shape without the guard MUST still fire — pins that the silence comes
/// from the verdict, not from blanket-silencing every `rex_var::varsIterator()`.
#[test]
fn unguarded_absent_static_call_still_fires() {
    let src = "<?php
class rex_var {}
rex_var::varsIterator();
";
    assert_eq!(undef(src).len(), 1, "{:?}", undef(src));
}

/// Positive-guard TRUE retention (the polarity crux): the call sits on the guard-FALSE
/// fall-through. `method_exists(C::class,'m')` folds to `No` (m provably absent, C
/// closed), so the `return` then-branch is DEAD and control falls through to the call,
/// which S2 independently proves absent — it MUST STILL FIRE. The guard-respect vouch
/// is bound only on the (dead) true branch, so it never reaches this false-path site.
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

/// Verdict `No` on a `class_exists` guard prunes its then-branch: an undefined-method
/// call inside a provably-absent-class guard is dead.
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

// ---------------------------------------------------------------------------
// Verdict `Yes`: present symbol → true-branch live, call resolves, checks run.
// ---------------------------------------------------------------------------

/// A present method → `Yes` verdict → the guard true-branch stays live: the guarded
/// call resolves (no undefined-method finding), a sibling absent call inside the same
/// branch DOES fire (the branch is walked), proving the branch was not pruned.
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

/// `Yes`-branch downstream checks still run: the guarded present method called with
/// too few arguments raises the arity finding (the branch is live and fully checked).
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

// ---------------------------------------------------------------------------
// Leg 2: the conservative guard-respect leg (Maybe verdict → vouch silence).
// ---------------------------------------------------------------------------

/// The instance-receiver idiom: `method_exists($o,'m')` cannot fold ($o is not a
/// literal class), so the verdict is `Maybe` and both branches walk live. S2 would
/// prove `$o->m()` absent (C closed, m missing) — but the positive guard vouched
/// `C::m`, so the absence id stays silent. Firing here would call the programmer a liar.
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

/// The vouch is scoped to the guarded symbol only: a sibling absent call inside the
/// same branch (different method) still fires — the guard-respect leg is exact-textual.
#[test]
fn vouch_is_scoped_to_the_guarded_symbol() {
    // `$o->m()` is vouched (C::m); a sibling absent call on a FRESH exact receiver of
    // the same class but a DIFFERENT method (C::other) is un-vouched and fires.
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

/// The vouch does NOT leak past the `if`: an empty guarded branch that falls through
/// must not silence the tail. `(new C)->m()` after the `if` is un-guarded and fires
/// (the join intersects the vouch away — the sibling fall-through never carried it).
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

/// A trait-bearing chain taints closure → `method_exists` folds to `Maybe`; both
/// branches walk live and the positive guard's vouch silences the absence id inside.
#[test]
fn trait_bearing_chain_maybe_verdict_vouched_silent() {
    let src = "<?php
trait T {}
class C { use T; }
if (method_exists(C::class, 'm')) {
    (new C)->m();
}
";
    // C `uses_traits` → the verdict is Maybe (a trait could carry `m`); S2 is itself
    // silent on a trait-using class, and the vouch keeps it silent regardless.
    assert_eq!(undef(src).len(), 0, "{:?}", undef(src));
}

// ---------------------------------------------------------------------------
// Sidecar-availability gate (A9 / A2ii): no boot surface ⇒ no folding.
// ---------------------------------------------------------------------------

/// Without a live boot surface the verdict is `Maybe` (the sound subset): the negated
/// guard is NOT proven, both paths walk, and the vouch on the (true) return branch
/// does not reach the false-path call — which S2 also cannot fire (no sidecar). So the
/// whole file is silent, but for the honest reason (nothing is decidable), not a prune.
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

/// A homonym on the guard's class (`Some(true)`) taints the verdict to `Maybe`: the
/// textual class may be shadowed by a resident builtin, so absence is undecidable. The
/// negated-guard fall-through then stays live, but S2 is itself silenced by the same
/// homonym — no finding, again for the honest reason.
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

// ---------------------------------------------------------------------------
// function_exists / class_exists positive verdicts + the polyfill non-regression.
// ---------------------------------------------------------------------------

/// A catalog builtin function → `function_exists` folds to `Yes`; the true-branch is
/// live and a sibling absent method call inside it fires.
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

/// The `function_exists`-guarded polyfill (`if (!function_exists('f')) { function f(){} }
/// f();`): `f` is declared conditionally, so `function_exists('f')` folds to `Maybe`
/// (the dam stands) — NEITHER branch is pruned. Verified by a probe: an absent method
/// call placed in the (non-negated) shadow branch stays live and fires, proving the
/// verdict never collapsed to a spurious `Yes`/`No`.
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
    // The `else` (the `function_exists` TRUE path) must stay live under `Maybe`.
    assert_eq!(undef(src).len(), 1, "polyfill else-branch must not be pruned: {:?}", undef(src));
}
