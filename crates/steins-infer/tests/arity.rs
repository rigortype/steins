//! ADR-0049 §6 / S5: the userland arity arms — `call.too-few-arguments` and
//! `call.unknown-named-argument`.
//!
//! The verified PHP 8.5 table is asymmetric (every row `php -r`-checked): too few
//! arguments to a userland target is always a fatal `ArgumentCountError`; too many
//! to a non-variadic runs clean and is NEVER a finding; an unknown named argument
//! to a non-variadic is a fatal `Error`, a variadic silently collects it; a named
//! argument overwriting a positional is a fatal (DEFERRED) `Error`, and the runtime
//! precedence overwrite ≻ unknown-named ≻ too-few is honored here.
//!
//! Like S2, the arity family is silent under the pure `NoFold` subset (no sidecar
//! to answer the A2ii homonym leg), so these tests drive a [`Boot`] mock that
//! stands in for the runtime boot surface. Every ladder/leg ships with a **silence
//! fixture** proving the id stays quiet when a precondition fails (the §10 silence
//! matrix), alongside the firing fixtures.

use steins_infer::{
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID, Diagnostic, Folder, check_with,
};
use steins_syntax::SourceTree;

/// A boot-surface mock. `available` is the A9 family gate; `fn_homonyms` /
/// `class_homonyms` are the lowercased names the boot surface reports as resident
/// functions / class-likes (the A2ii homonyms); `reflect_fails` simulates a mid-run
/// sidecar failure (every existence query returns Unknown).
struct Boot {
    available: bool,
    fn_homonyms: Vec<String>,
    class_homonyms: Vec<String>,
    reflect_fails: bool,
}

impl Boot {
    /// The common case: family available, empty boot surface (project symbols are
    /// never homonyms), reflect answers cleanly.
    fn ready() -> Self {
        Boot { available: true, fn_homonyms: Vec::new(), class_homonyms: Vec::new(), reflect_fails: false }
    }
}

impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[steins_syntax::ArgValue]) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        if self.reflect_fails {
            return None;
        }
        Some(self.class_homonyms.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        if self.reflect_fails {
            return None;
        }
        Some(self.fn_homonyms.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
}

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| d.id == CALL_TOO_FEW_ARGUMENTS_ID || d.id == CALL_UNKNOWN_NAMED_ARGUMENT_ID)
        .collect()
}

/// Findings with a ready boot surface.
fn fires(src: &str) -> Vec<Diagnostic> {
    run(src, &mut Boot::ready())
}

fn too_few(d: &Diagnostic) -> bool {
    d.id == CALL_TOO_FEW_ARGUMENTS_ID
}
fn unknown_named(d: &Diagnostic) -> bool {
    d.id == CALL_UNKNOWN_NAMED_ARGUMENT_ID
}

// ---------------------------------------------------------------------------
// Firing fixtures: every leg holds.
// ---------------------------------------------------------------------------

#[test]
fn function_too_few_positional() {
    let d = fires("<?php\nfunction format($a, $b) {}\nformat(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
    assert_eq!(
        d[0].message,
        "too few arguments to format(): 1 passed, 2 required — provable ArgumentCountError"
    );
}

#[test]
fn function_zero_args_too_few() {
    let d = fires("<?php\nfunction format($a, $b) {}\nformat();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("0 passed, 2 required"), "{}", d[0].message);
}

#[test]
fn function_unknown_named() {
    let d = fires("<?php\nfunction f($a, $b) {}\nf(a: 1, z: 2);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(unknown_named(&d[0]));
    assert_eq!(d[0].message, "unknown named argument $z to f() — no parameter $z, provable Error");
}

#[test]
fn function_too_few_via_named_gap() {
    // `f(b: 2)` leaves required $a unbound — ArgumentCountError at runtime.
    let d = fires("<?php\nfunction f($a, $b) {}\nf(b: 2);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
    assert!(d[0].message.contains("1 passed, 2 required"), "{}", d[0].message);
}

#[test]
fn constructor_too_few_promoted() {
    let d = fires("<?php\nclass C { public function __construct(public int $x) {} }\nnew C();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
    assert!(d[0].message.contains("C::__construct(): 0 passed, 1 required"), "{}", d[0].message);
}

#[test]
fn method_too_few_exact_new_receiver() {
    let d = fires("<?php\nclass C { public function m($a, $b) {} }\n(new C())->m(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
    assert!(d[0].message.contains("C::m(): 1 passed, 2 required"), "{}", d[0].message);
}

#[test]
fn method_too_few_exact_var_receiver() {
    let d = fires("<?php\nclass C { public function m($a, $b) {} }\n$o = new C();\n$o->m(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
}

#[test]
fn method_too_few_across_inherited_chain() {
    let d = fires(
        "<?php\nclass Base { public function m($a, $b) {} }\nclass C extends Base {}\n(new C())->m(1);\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
    // The declaring class names the method (PHP: `Base::m`).
    assert!(d[0].message.contains("Base::m(): 1 passed, 2 required"), "{}", d[0].message);
}

#[test]
fn static_too_few() {
    let d = fires("<?php\nclass C { public static function m($a, $b) {} }\nC::m(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
}

#[test]
fn method_unknown_named() {
    let d = fires("<?php\nclass C { public function m($a) {} }\n(new C())->m(z: 1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(unknown_named(&d[0]));
    assert!(d[0].message.contains("C::m()"), "{}", d[0].message);
}

#[test]
fn by_ref_param_is_required() {
    // `function f(&$x)` — by-ref params are required exactly like any other.
    let d = fires("<?php\nfunction f(&$x) {}\nf();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
    assert!(d[0].message.contains("0 passed, 1 required"), "{}", d[0].message);
}

#[test]
fn optional_before_required_is_implicitly_required() {
    // `f($a = 1, $b)` — PHP treats $a as implicitly required (required count 2).
    let d = fires("<?php\nfunction f($a = 1, $b) {}\nf(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
    assert!(d[0].message.contains("1 passed, 2 required"), "{}", d[0].message);
}

// ---------------------------------------------------------------------------
// Silence matrix — one fixture per leg (ADR-0049 §10: the fixture is written
// before the id fires on that shape).
// ---------------------------------------------------------------------------

#[test]
fn silent_no_sidecar() {
    // A9 / A2ii honest consequence: no live sidecar ⇒ the whole family is silent.
    let mut boot = Boot { available: false, ..Boot::ready() };
    let d = run("<?php\nfunction f($a, $b) {}\nf(1);\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_reflect_fails() {
    // A mid-run sidecar failure (homonym query Unknown) ⇒ silence.
    let mut boot = Boot { reflect_fails: true, ..Boot::ready() };
    let d = run("<?php\nfunction f($a, $b) {}\nf(1);\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_function_homonym() {
    // A2ii: the resolved FQN is a resident boot-surface function (a polyfill twin
    // shadowed by a loaded extension may be the real binding) ⇒ silence.
    let mut boot = Boot { fn_homonyms: vec!["f".into()], ..Boot::ready() };
    let d = run("<?php\nfunction f($a, $b) {}\nf(1);\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_class_homonym_method() {
    // A2ii (class-like): a traversed chain class is a boot-surface homonym ⇒ silence.
    let mut boot = Boot { class_homonyms: vec!["c".into()], ..Boot::ready() };
    let d = run("<?php\nclass C { public function m($a, $b) {} }\n(new C())->m(1);\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_ambiguous_function() {
    // Two same-name declarations ⇒ Ambiguous ⇒ Unknown ⇒ silence (which arity wins
    // is unknowable).
    let d = fires("<?php\nfunction f($a, $b) {}\nfunction f($a) {}\nf(1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn fires_top_level_function_despite_dam() {
    // A2i scoping: a top-level (unconditional) function is immune to the dam — an
    // extension cannot silently redefine it (a redeclare would fatal at load), so
    // its indexed signature is authoritative even with a dam site present.
    let d = fires("<?php\nfunction f($a, $b) {}\neval('');\nf(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
}

#[test]
fn silent_conditional_function_dam_standing() {
    // A2i: a conditionally-declared function re-dams the claim; a dam site (`eval`)
    // in the universe ⇒ silence (the polyfill-swap shape).
    let d = fires("<?php\nif (true) { function f($a, $b) {} }\neval('');\nf(1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn fires_conditional_function_dam_clear() {
    // The same conditional function, but no dam site ⇒ the dam is clear ⇒ fires.
    let d = fires("<?php\nif (true) { function f($a, $b) {} }\nf(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
}

#[test]
fn silent_unpacking() {
    // Argument unpacking (`...$a`) ⇒ the count is unproven ⇒ silence.
    let d = fires("<?php\nfunction f($a, $b) {}\n$xs = [1];\nf(...$xs);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_unpacking_mixed() {
    // A positional arg plus unpacking — still unproven.
    let d = fires("<?php\nfunction f($a, $b, $c) {}\n$xs = [1];\nf(1, ...$xs);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_trait_bearing_chain() {
    // Leg (e): a `use`-trait class may gain the method from the trait with a
    // different signature ⇒ Unknown ⇒ silence.
    let d = fires(
        "<?php\ntrait T { public function m($a) {} }\nclass C { use T; public function m2($a, $b) {} }\n(new C())->m2(1);\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_non_exact_receiver_refused_lane() {
    // The REFUSED declared-receiver lane: `$c` is a param (a lower bound, not
    // `class_exact`). Firing here would be a false positive — an override of `m`
    // may ADD optional parameters (`class D extends C { function m($a = 0) {} }`),
    // so `$c->m()` on a `D` runs. Arity refuses it outright (never deferred).
    let d = fires(
        "<?php\nclass C { public function m($a, $b) {} }\nfunction h(C $c) { $c->m(1); }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_this_receiver() {
    // A1: `$this` is a membership fact, never exactness ⇒ silence.
    let d = fires("<?php\nclass C { public function m($a, $b) {} public function go() { $this->m(1); } }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_self_static_parent() {
    let d = fires(
        "<?php\nclass C { public function m($a, $b) {} public function go() { self::m(1); } }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_variadic_too_few() {
    // A variadic-only target has 0 required parameters — no too-few possible.
    let d = fires("<?php\nfunction f(...$r) {}\nf();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_variadic_collects_unknown_name() {
    // A variadic silently collects an otherwise-unknown named argument
    // (`fv(x: 1)` → `{"x":1}`) ⇒ no unknown-named finding.
    let d = fires("<?php\nfunction f(...$r) {}\nf(x: 1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_variadic_after_params_collects_unknown() {
    // A declared variadic after explicit params still collects unknown names.
    let d = fires("<?php\nfunction f($a, ...$r) {}\nf(a: 1, zzz: 2);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_too_many_userland() {
    // Too many positional args to a userland non-variadic runs clean — NEVER a
    // finding (the ADR-0002 consequence pattern).
    let d = fires("<?php\nfunction f($a, $b) {}\nf(1, 2, 3);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_named_fills_positional_gap() {
    // `f(1, c: 3)` on `f($a, $b = 2, $c = 3)` is legal (required $a covered by the
    // positional; $b takes its default) ⇒ silence.
    let d = fires("<?php\nfunction f($a, $b = 2, $c = 3) {}\nf(1, c: 3);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_named_overwrites_positional_deferred() {
    // `f(1, a: 5)` raises the DEFERRED overwrite `Error` at runtime, which fires
    // BEFORE too-few/unknown — so both of our ids stay silent (no misclaim).
    let d = fires("<?php\nfunction f($a, $b) {}\nf(1, a: 5);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_first_class_callable() {
    // `format(...)` is a first-class-callable closure creation, not a call ⇒ never
    // an arity site (it lowers away from a `StmtKind::Call`).
    let d = fires("<?php\nfunction format($a, $b) {}\n$g = format(...);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_func_get_args_no_declared_variadic() {
    // A `func_get_args`-based function declares 0 required parameters — a bare
    // `f()` is sound (no missing required), and too-few cannot fire.
    let d = fires("<?php\nfunction f() { $x = func_get_args(); }\nf();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_builtin_function() {
    // A catalogued builtin (`strlen`) is the INTERNAL slice (reflect, M2), never the
    // userland arm — even with too few args.
    let d = fires("<?php\nstrlen();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_enum_method() {
    // A3: enum method bodies are not lowered, so an enum method call cannot resolve
    // to a signature ⇒ silence.
    let d = fires(
        "<?php\nenum E { case A; public function m($a, $b) {} }\n$x = E::A;\n$x->m(1);\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_closure_var_call() {
    // A `$fn(...)` variable call is the value lane's target, not arity's.
    let d = fires("<?php\nfunction f($a, $b) {}\n$fn = 'f';\n$fn(1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_nullsafe_method() {
    // `?->` is excluded in v1 (a null receiver short-circuits).
    let d = fires("<?php\nclass C { public function m($a, $b) {} }\n$o = new C();\n$o?->m(1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_protected_method_exact_receiver() {
    // A protected method may raise a visibility `Error` (or route to `__call`) from
    // an external call site — a distinct consequence, not an ArgumentCountError.
    let d = fires("<?php\nclass C { protected function m($a, $b) {} }\n(new C())->m(1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_new_abstract_class() {
    // `new AbstractClass()` raises "Cannot instantiate abstract class" before any
    // ArgumentCountError — silence (would misname).
    let d = fires(
        "<?php\nabstract class A { public function __construct($x) {} }\nnew A();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_static_call_to_instance_method() {
    // `C::m(1)` on a non-static `m` raises `Error: Non-static method … cannot be
    // called statically` BEFORE any ArgumentCountError — silence (would misname).
    let d = fires("<?php\nclass C { public function m($a, $b) {} }\nC::m(1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn fires_instance_syntax_call_to_static_method() {
    // The reverse is legal: a static method invoked via `->` still arity-checks
    // (verified `ArgumentCountError`).
    let d = fires("<?php\nclass C { public static function m($a, $b) {} }\n$o = new C();\n$o->m(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(too_few(&d[0]));
}

#[test]
fn silent_magic_call_absent_method() {
    // A method absent from the chain routes to `__call` (any arity) — S2's job if
    // undefined, never arity's; and here `m` is absent so no signature exists.
    let d = fires(
        "<?php\nclass C { public function __call($n, $a) {} }\n(new C())->m(1);\n",
    );
    assert!(d.is_empty(), "{d:?}");
}
