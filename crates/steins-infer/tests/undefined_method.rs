//! ADR-0049 §4 / S2: `call.undefined-method`, the finding-breadth flagship.
//!
//! The absence-proof ladder fires only under complete closure (ADR-0013 zero-FP).
//! Under the pure `NoFold` subset the id is silent by design (no sidecar to answer
//! the A2ii homonym question), so these tests drive a [`Boot`] mock folder that
//! stands in for the runtime boot surface: `available` is A9's global gate, and
//! `builtins` is the set of names the boot surface knows as class-likes. Every
//! ladder leg ships with a **silence fixture** proving the id stays quiet when that
//! leg fails (the ADR-0049 §10 silence-matrix discipline), alongside the firing
//! fixtures that prove it speaks when every leg holds.

use steins_infer::{CALL_UNDEFINED_METHOD_ID, Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A boot-surface mock: `available` is the A9 family-availability gate; `builtins`
/// are the lowercased names the boot surface reports as resident class-likes (the
/// A2ii homonyms); `reflect_fails` simulates a mid-run sidecar failure (every
/// existence query returns Unknown).
struct Boot {
    available: bool,
    builtins: Vec<String>,
    reflect_fails: bool,
}

impl Boot {
    /// The common case: family available, empty boot surface (project classes are
    /// never homonyms), reflect answers cleanly.
    fn ready() -> Self {
        Boot { available: true, builtins: Vec::new(), reflect_fails: false }
    }
    fn with_builtins(names: &[&str]) -> Self {
        Boot {
            available: true,
            builtins: names.iter().map(|n| n.to_ascii_lowercase()).collect(),
            reflect_fails: false,
        }
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
        Some(self.builtins.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
}

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| d.id == CALL_UNDEFINED_METHOD_ID)
        .collect()
}

/// Fires with a ready boot surface.
fn fires(src: &str) -> Vec<Diagnostic> {
    run(src, &mut Boot::ready())
}

// ---------------------------------------------------------------------------
// Firing fixtures: every leg holds.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_exact_new_receiver() {
    let d = fires("<?php\nclass Order {}\n(new Order())->tyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined method Order::tyop()"), "{}", d[0].message);
    assert!(d[0].message.contains("no __call"), "{}", d[0].message);
}

#[test]
fn fires_on_exact_var_receiver() {
    let d = fires("<?php\nclass Order {}\n$o = new Order();\n$o->tyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("Order::tyop()"), "{}", d[0].message);
}

#[test]
fn fires_across_a_fully_enumerated_chain() {
    let d = fires(
        "<?php\nclass AbstractOrder { public function pay() {} }\nclass Order extends AbstractOrder {}\n$o = new Order();\n$o->tyop();\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(
        d[0].message.contains("Order → AbstractOrder"),
        "chain render missing: {}",
        d[0].message
    );
}

#[test]
fn fires_on_static_named_call_with_callstatic_phrasing() {
    let d = fires("<?php\nclass Registry {}\nRegistry::tyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("Registry::tyop()"), "{}", d[0].message);
    assert!(d[0].message.contains("no __callStatic"), "{}", d[0].message);
}

#[test]
fn fires_with_a_positional_argument_present() {
    // Method existence is argument-shape-independent; a call with at least one
    // positional argument to an undefined method still fatals at runtime.
    let d = fires("<?php\nclass Order {}\n$o = new Order();\n$o->tyop(1);\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_with_mixed_positional_and_named_arguments() {
    // A positional arg makes `args` non-empty, so the call is distinguishable from
    // a first-class callable and stays eligible.
    let d = fires("<?php\nclass Order {}\n$o = new Order();\n$o->tyop(1, x: 2);\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn silent_on_named_only_arguments_conflated_with_first_class_callable() {
    // Lowering represents BOTH the first-class callable `m(...)` and a named-only
    // call `m(x: 1)` as `args: [], positional_only: false` — they are
    // indistinguishable in the current CallExpr shape. Since the first-class form
    // MUST stay silent (leg l — it builds a Closure, it does not invoke) and the two
    // cannot be told apart, S2 conservatively silences both. A completeness loss on
    // named-only calls, never an FP (the zero-FP identity governs the tie-break).
    let d = fires("<?php\nclass Order {}\n$o = new Order();\n$o->tyop(x: 1);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn fires_case_insensitively_on_absence() {
    // `TYOP` vs a chain that defines only `pay` — still undefined (folding-insensitive
    // absence). A defined `Pay`/`pay` would silence; `tyop` is genuinely absent.
    let d = fires("<?php\nclass Order { public function pay() {} }\n$o = new Order();\n$o->TYOP();\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_under_conditional_decl_when_dam_is_clear() {
    // A2i: a conditional declaration re-dams — but with the whole-universe dam clear
    // (no eval/include/alias mint), the claim still stands.
    let d = fires("<?php\nif (true) {\n  class Order {}\n}\n(new Order())->tyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

// ---------------------------------------------------------------------------
// Silence matrix — one fixture per ladder leg (ADR-0049 §10).
// ---------------------------------------------------------------------------

#[test]
fn silent_when_family_unavailable() {
    // A9 / no-sidecar: the whole family is silent.
    let mut boot = Boot { available: false, builtins: Vec::new(), reflect_fails: false };
    let d = run("<?php\nclass Order {}\n(new Order())->tyop();\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_reflect_unanswerable() {
    // A mid-run sidecar failure ⇒ Unknown per chain FQN ⇒ silence.
    let mut boot = Boot { available: true, builtins: Vec::new(), reflect_fails: true };
    let d = run("<?php\nclass Order {}\n(new Order())->tyop();\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_a_on_this_receiver() {
    // A1: `$this` is membership, not exactness — S6's lane, not ours.
    let d = fires("<?php\nclass Handler { public int $n = 0; public function run() { $this->tyop(); } }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_a_on_laundered_this_alias() {
    // The audit's 13th FP class (template method): `$u = $this; $u->handle()` where a
    // subclass defines `handle`. The alias is a lower bound → inexact → silent.
    let d = fires(
        "<?php\nclass Handler { public int $n = 0; public function run() { $u = $this; $u->handle(); } }\nclass MailHandler extends Handler { public function handle() {} }\n(new MailHandler())->run();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_a_on_clone_of_this() {
    // `clone $this` on a non-final class is a lower bound too.
    let d = fires(
        "<?php\nclass Handler { public int $n = 0; public function run() { $u = clone $this; $u->tyop(); } }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_b_incomplete_chain() {
    // A parent unresolvable in the project taints closure.
    let d = fires("<?php\nclass Order extends Missing {}\n(new Order())->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_c_method_present_on_class() {
    let d = fires("<?php\nclass Order { public function tyop() {} }\n(new Order())->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_c_method_present_on_parent() {
    let d = fires(
        "<?php\nclass Base { public function tyop() {} }\nclass Order extends Base {}\n(new Order())->tyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_d_magic_call() {
    let d = fires("<?php\nclass Order { public function __call($n, $a) {} }\n(new Order())->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_d_magic_call_in_grandparent() {
    let d = fires(
        "<?php\nclass Root { public function __call($n, $a) {} }\nclass Mid extends Root {}\nclass Order extends Mid {}\n(new Order())->tyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_d_callstatic_for_static_call() {
    let d = fires("<?php\nclass Registry { public static function __callStatic($n, $a) {} }\nRegistry::tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_e_trait_use() {
    let d = fires("<?php\ntrait T {}\nclass Order { use T; }\n(new Order())->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_e_trait_use_in_ancestor() {
    let d = fires(
        "<?php\ntrait T {}\nclass Base { use T; }\nclass Order extends Base {}\n(new Order())->tyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_f_builtin_ancestor() {
    // A chain reaching a builtin (`Exception`) waits for the reflect method surface.
    let d = fires("<?php\nclass AppError extends Exception {}\n(new AppError())->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_h_boot_surface_homonym() {
    // A2ii: the project declares a root-namespace twin of a loaded extension class.
    // The boot surface knows `DateTime`, so the textual twin may be dead code.
    let mut boot = Boot::with_builtins(&["DateTime"]);
    let d = run("<?php\nclass DateTime {}\n(new DateTime())->tyop();\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_h_homonym_on_an_ancestor() {
    // The homonym leg must taint on ANY chain node, not just the receiver.
    let mut boot = Boot::with_builtins(&["ArrayObject"]);
    let d = run(
        "<?php\nclass ArrayObject {}\nclass Order extends ArrayObject {}\n(new Order())->tyop();\n",
        &mut boot,
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_i_ambiguous_duplicate_decl() {
    // Two declarations of one FQN ⇒ Ambiguous ⇒ closure tainted.
    let d = fires("<?php\nclass Order {}\nclass Order {}\n(new Order())->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_j_enum_static_call() {
    // A3: enum methods (and the engine-provided statics) are not lowered.
    let d = fires("<?php\nenum Suit { case Hearts; }\nSuit::tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_g_conditional_decl_under_dam() {
    // A2i: a conditional declaration + a standing dam site (eval) ⇒ re-dammed ⇒ silent.
    let d = fires("<?php\neval('$x = 1;');\nif (true) {\n  class Order {}\n}\n(new Order())->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_l_nullsafe() {
    let d = fires("<?php\nclass Order {}\n$o = new Order();\n$o?->tyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_l_first_class_callable() {
    // `$o->tyop(...)` builds a Closure — it does not invoke.
    let d = fires("<?php\nclass Order {}\n$o = new Order();\n$f = $o->tyop(...);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_leg_l_dynamic_method_name() {
    let d = fires("<?php\nclass Order {}\n$o = new Order();\n$m = 'tyop';\n$o->$m();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_self_static_parent_calls() {
    // `self`/`static`/`parent` stay unlowered (ADR-0043 §1).
    let d = fires(
        "<?php\nclass Base {}\nclass Order extends Base { public function go() { self::tyop(); static::tyop(); parent::tyop(); } }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_inexact_non_final_this_seed() {
    // A non-final class's `$this` is a lower bound even without laundering; a direct
    // `$this->tyop()` in an overridable class must not fire (also leg-a coverage).
    let d = fires(
        "<?php\nclass Shape { public int $sides = 0; public function draw() { $this->tyop(); } }\nclass Square extends Shape { public function tyop() {} }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

// ---------------------------------------------------------------------------
// Adversarial: constructed counterexamples that must be silenced by a real test.
// ---------------------------------------------------------------------------

#[test]
fn adversarial_final_this_still_routes_through_receiver_legs() {
    // A `final` class's `$this` IS exact — but a direct `$this->m()` receiver is
    // still `Receiver::This`, which S2 excludes (A1). This must stay silent even
    // though exactness is provable, because S2 v1 never rests on a `$this` receiver.
    let d = fires(
        "<?php\nfinal class Leaf { public int $n = 0; public function run() { $this->tyop(); } }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn adversarial_new_of_final_homonym_still_checks_boot_surface() {
    // Even a `final` project class must clear the homonym leg.
    let mut boot = Boot::with_builtins(&["SplStack"]);
    let d = run("<?php\nfinal class SplStack {}\n(new SplStack())->tyop();\n", &mut boot);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn adversarial_defined_method_deep_in_chain_silences() {
    // The method exists three levels up — closure must find it and stay silent.
    let d = fires(
        "<?php\nclass A { public function tyop() {} }\nclass B extends A {}\nclass C extends B {}\n(new C())->tyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn adversarial_abstract_method_declaration_counts_as_present() {
    // An abstract declaration is still a declaration — the name is defined.
    let d = fires(
        "<?php\nabstract class Base { abstract public function tyop(); }\nclass Order extends Base { public function tyop() {} }\n(new Order())->tyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn adversarial_declared_type_parameter_receiver_is_silent() {
    // THE soundness boundary: a `Shape $s` parameter is a *declared* receiver, not an
    // exact one — a subclass passed at runtime may define the method (and `eval` can
    // mint such a subclass). This is S6's descendant-closure lane, never S2's. The
    // heap seeds no exact object for a declared param, so `class_exact` is false and
    // the receiver leg (A1) silences it.
    let d = fires(
        "<?php\nclass Shape { public function area() {} }\nfunction f(Shape $s): void { $s->tyop(); }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn adversarial_object_returned_from_call_receiver_is_silent() {
    // `make()->tyop()` — the receiver is a call result, never an exact heap object in
    // v1 (leg l: method-on-call-result is out of scope). Silent.
    let d = fires(
        "<?php\nclass Shape {}\nfunction make(): Shape { return new Shape(); }\nmake()->tyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn adversarial_reassigned_receiver_loses_exactness() {
    // `$o = new Order(); $o = maybe(); $o->tyop();` — the rebind to an unknown call
    // result drops the exact-object binding, so the stale `new Order()` fact cannot
    // launder the receiver back to exact.
    let d = fires(
        "<?php\nclass Order {}\nfunction maybe() { return null; }\n$o = new Order();\n$o = maybe();\n$o->tyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

