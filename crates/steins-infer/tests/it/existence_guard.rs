//! ADR-0049 §4 / N3: foldable existence-guard verdicts + the conservative
//! guard-respect leg. Two legs, both exercised here:
//!   1. `method_exists`/`function_exists`/`class_exists` in guard position folds to a
//!      Yes/No/Maybe verdict; ADR-0031 dead-region discipline then prunes a
//!      provably-dead branch (FP class 15: `if (!method_exists(C,'m')) return; C::m();`).
//!   2. An absence-family id DOMINATED by a positive same-symbol guard stays silent
//!      even on `Maybe` — the guard is programmer-supplied evidence.
//!
//! Drives the [`Boot`] mock folder (mirrors `undefined_method.rs`).

use steins_infer::{
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, Folder, check_with,
};
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
    fn fold(&mut self, _: &str, _: &[ArgValue], _strict: bool) -> Option<ArgValue> {
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

// `defined('C::K')` over class constants (read-position run S0c, ADR-0049 A25).

fn class_const_findings(src: &str) -> Vec<Diagnostic> {
    diags_with(src, &mut Boot::ready())
        .into_iter()
        .filter(|d| d.id == "class-const.undefined" || d.id == "class-const.inaccessible")
        .collect()
}

const DEFINED_CLASSES: &str = "class A { private const P = 1; protected const Q = 2; const OK = 3; }
class B extends A { const BK = 4; }
";

fn defined_case(body: &str) -> usize {
    class_const_findings(&format!("<?php\n{DEFINED_CLASSES}function f() {{\n{body}\n}}\n")).len()
}

#[test]
fn defined_private_class_const_guard_discharges_inaccessible() {
    assert_eq!(defined_case("if (defined('A::P')) { return A::P; } return 0;"), 0);
    assert_eq!(defined_case("if (defined('\\\\A::P')) { return A::P; } return 0;"), 0);
    assert_eq!(defined_case("if (defined('a::P')) { return A::P; } return 0;"), 0);
}

#[test]
fn defined_undefined_class_const_guard_discharges_undefined() {
    assert_eq!(defined_case("if (defined('A::NOPE')) { $x = A::NOPE; } return 0;"), 0);
    assert_eq!(defined_case("if (!defined('A::NOPE')) { return 0; } $x = A::NOPE; return $x;"), 0);
    assert_eq!(defined_case("if (defined('B::NOPE')) { $x = B::NOPE; } return 0;"), 0);
}

#[test]
fn defined_protected_class_const_from_global_scope_discharges() {
    assert_eq!(defined_case("if (defined('A::Q')) { return A::Q; } return 0;"), 0);
}

#[test]
fn defined_class_const_controls_still_report() {
    // Unguarded, a guard on another constant, a case-mismatched constant name.
    assert_eq!(defined_case("return A::NOPE;"), 1);
    assert_eq!(defined_case("if (defined('A::OTHER')) { return 0; } return A::NOPE;"), 1);
    // The constant name is case-sensitive: `A::nope` is not defined, so the branch is dead.
    assert_eq!(defined_case("if (defined('A::nope')) { return A::NOPE; } return 0;"), 0);
    assert_eq!(defined_case("if (defined('A::P')) { return 1; } return A::P;"), 1);
    // A defined, public constant makes the guard true: the code after the return is dead.
    assert_eq!(defined_case("if (defined('A::OK')) { return A::OK; } return A::NOPE;"), 0);
    assert_eq!(defined_case("if (defined('A::OK')) { echo 1; } return A::NOPE;"), 1);
}

#[test]
fn defined_guard_with_exit_discharges_the_rest() {
    assert_eq!(defined_case("if (!defined('A::NOPE')) { exit; } return A::NOPE;"), 0);
    assert_eq!(defined_case("if (!defined('A::OTHER')) { return 0; } return A::NOPE;"), 0);
}

/// `defined(...) || die; A::NOPE` is not modelled: the connective folds its right operand
/// only, and the path ending inside it is not a branch, so the fold changes nothing after
/// the statement (a recorded gap, S4).
#[test]
fn defined_or_die_statement_is_not_a_guard_region() {
    assert_eq!(defined_case("defined('A::NOPE') || die; return A::NOPE;"), 1);
}

#[test]
fn defined_private_const_is_visible_inside_its_class() {
    let src = "<?php
class A { private const P = 1; function m() { if (defined('A::P')) { return A::P; } return A::NOPE; } function n() { return A::NOPE; } }
";
    // Visible here, so the guard is true and its else leg is dead; only `n` reports.
    assert_eq!(class_const_findings(src).len(), 1, "{:?}", class_const_findings(src));
}

/// A closure, bound closure or arrow fn can run in any class's scope, so a non-public
/// constant is `Maybe` there: both branches stay live (PHP answers true in these).
#[test]
fn defined_non_public_const_in_a_rebindable_scope_is_undecided() {
    let in_closure = "<?php
class SC { private const SP = 1; protected const PP = 2; function m() { return function() {
    if (defined('SC::SP')) { return SC::NOPE; } return 0; }; } }
";
    assert_eq!(class_const_findings(in_closure).len(), 1, "{:?}", class_const_findings(in_closure));
    let plain = "<?php
class SC { private const SP = 1; }
$f = function() { if (defined('SC::SP')) { return SC::NOPE; } return 0; };
";
    assert_eq!(class_const_findings(plain).len(), 1, "{:?}", class_const_findings(plain));
    let bound = "<?php
class SC { private const SP = 1; }
$g = Closure::bind(function() { if (defined('SC::SP')) { return SC::NOPE; } return 0; }, null, SC::class);
";
    assert_eq!(class_const_findings(bound).len(), 1, "{:?}", class_const_findings(bound));
    // An arrow fn: a `Maybe` ternary guard leaves both arms live, and an env-independent
    // family judges a conditional arm (ADR-0104, D3), while a wrong `No` would make the
    // then-arm dead and silent.
    let arrow = "<?php
class SC { private const SP = 1; function a() { return fn() => defined('SC::SP') ? SC::NOPE : 0; } }
";
    assert_eq!(class_const_findings(arrow).len(), 1, "{:?}", class_const_findings(arrow));
}

/// Issue #1103: `method_exists` on a literal class counts a method an abstract class or
/// an interface declares only through an interface it implements or extends, as PHP
/// does, so the body it guards stays live. PHP 8.5: both calls are true and each body
/// raises a `TypeError` on `strlen(null)`.
#[test]
fn method_exists_on_a_literal_climbs_the_interfaces_of_an_abstract_class_or_interface() {
    let fixture = "<?php
interface HasM { public function m(): int; }
abstract class Abs implements HasM {}
abstract class AbsKid extends Abs {}
interface J1 {} interface J2 { public function m2(): int; }
interface IJ extends J1, J2 {}
interface Deep extends IJ {}
";
    // Whether the body `method_exists(C::class, 'm')` guards is live (its dump emitted).
    let live = |class: &str, method: &str| {
        let src = format!(
            "{fixture}function f(?string $n): int {{ if (method_exists({class}::class, '{method}')) \
             {{ \\PHPStan\\dumpType($n); return strlen($n); }} return 0; }}\n"
        );
        diags_with(&src, &mut Boot::ready()).iter().filter(|d| d.id == DEBUG_TYPE_ID).count()
    };
    // The interface method of an abstract class's interface, and the second parent of
    // an interface (the issue's witness).
    assert_eq!(live("Abs", "m"), 1, "e1");
    assert_eq!(live("IJ", "m2"), 1, "e2");
    // Through an abstract parent, and through an interface's interface.
    assert_eq!(live("AbsKid", "m"), 1, "e3");
    assert_eq!(live("Deep", "m2"), 1, "e4");
    // Kept: a method no interface in reach declares still folds `No`, the body dead.
    assert_eq!(live("Abs", "nope"), 0, "k1");
    assert_eq!(live("IJ", "nope"), 0, "k2");
}
