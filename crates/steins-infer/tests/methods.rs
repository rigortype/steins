//! Acceptance tests for the class-world extension of `type.argument-mismatch`
//! and `effect.envelope-exceeded` (ADR-0001 sound dispatch).
//!
//! Method calls resolve to a same-file target ONLY under rules that respect
//! PHP's dynamic dispatch: exact-class receivers (`new Foo()`, `$x = new Foo()`)
//! resolve exactly; `$this->`/`self::` resolve only under a private/final/
//! final-class guard (a non-final public method may be overridden in another
//! file); `parent::`/`Foo::` are exact; `static::` and unknown receivers are
//! silent; a trait-using class or a chain that leaves the file gives up. Every
//! resolved call runs the full direct/propagation/binding machinery.

use steins_infer::{Diagnostic, EFFECT_ID, ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

fn only(src: &str) -> Diagnostic {
    let f = findings(src);
    assert_eq!(f.len(), 1, "expected exactly one finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

// ---- Constructor argument mismatch ---------------------------------------

#[test]
fn constructor_arg_mismatch_flagged() {
    // `new Foo("abc")` constructs exactly Foo; its __construct wants int.
    let src = "<?php\nclass Foo { public function __construct(int $w) {} }\nnew Foo(\"abc\");\n";
    let d = only(src);
    assert_eq!(d.id, ID);
    assert_eq!(d.line, 3);
    assert_eq!(
        d.message,
        "argument \"abc\" to Foo::__construct() cannot become int $w — proven TypeError (coercive mode)"
    );
}

#[test]
fn inherited_constructor_via_same_file_parent_flagged() {
    // Sub has no ctor; `new Sub("abc")` runs the inherited Base::__construct.
    let src = "<?php\nclass Base { public function __construct(int $w) {} }\nclass Sub extends Base {}\nnew Sub(\"abc\");\n";
    let d = only(src);
    assert_eq!(d.line, 4);
    assert!(d.message.contains("to Base::__construct()"), "{}", d.message);
}

#[test]
fn default_constructor_ignores_extra_args() {
    // No __construct in the complete in-file chain → PHP default ctor accepts
    // nothing, and `new Foo(1)` silently discards the extra arg (not an error).
    assert_eq!(n("<?php\nclass Foo {}\nnew Foo(1);\n"), 0);
}

#[test]
fn constructor_via_env_binding_descends() {
    // Constructor arg is a propagated variable, coerced then checked.
    let src = "<?php\nclass Foo { public function __construct(int $w) {} }\n$s = \"abc\";\nnew Foo($s);\n";
    let d = only(src);
    assert!(d.message.contains("from $s, assigned at line 3"), "{}", d.message);
    assert!(d.message.contains("to Foo::__construct()"), "{}", d.message);
}

// ---- Exact-class instance receivers --------------------------------------

#[test]
fn exact_receiver_direct_new_flagged() {
    let src = "<?php\nclass Foo { public function m(int $w): void {} }\n(new Foo())->m(\"abc\");\n";
    let d = only(src);
    assert_eq!(d.line, 3);
    assert!(d.message.contains("to Foo::m()"), "{}", d.message);
}

#[test]
fn exact_receiver_via_env_flagged() {
    let src = "<?php\nclass Foo { public function m(int $w): void {} }\n$x = new Foo();\n$x->m(\"abc\");\n";
    let d = only(src);
    assert_eq!(d.line, 4);
    assert!(d.message.contains("to Foo::m()"), "{}", d.message);
}

#[test]
fn nullsafe_exact_receiver_flagged() {
    // `$x?->m()` resolves like `$x->m()` for arg-checking purposes.
    let src = "<?php\nclass Foo { public function m(int $w): void {} }\n$x = new Foo();\n$x?->m(\"abc\");\n";
    assert_eq!(n(src), 1);
}

#[test]
fn unknown_receiver_variable_is_silent() {
    // `$x`'s class is unknown (parameter of unknown type) → dynamic dispatch.
    let src = "<?php\nclass Foo { public function m(int $w): void {} }\nfunction go($x): void { $x->m(\"abc\"); }\n";
    assert_eq!(n(src), 0, "unknown receiver class → silent");
}

// ---- `$this->` under the override guard ----------------------------------

#[test]
fn this_call_to_private_method_flagged() {
    let src = "<?php\nclass Foo { private function m(int $w): void {} public function go(): void { $this->m(\"abc\"); } }\n";
    assert_eq!(n(src), 1, "private $this->m is resolvable");
}

#[test]
fn this_call_to_final_method_flagged() {
    let src = "<?php\nclass Foo { final public function m(int $w): void {} public function go(): void { $this->m(\"abc\"); } }\n";
    assert_eq!(n(src), 1, "final $this->m is resolvable");
}

#[test]
fn this_call_in_final_class_flagged() {
    let src = "<?php\nfinal class Foo { public function m(int $w): void {} public function go(): void { $this->m(\"abc\"); } }\n";
    assert_eq!(n(src), 1, "$this->m in a final class is resolvable");
}

#[test]
fn this_call_to_nonfinal_public_is_silent() {
    // The FP guard: a subclass in another file could override m with a wider
    // signature, so a non-final public `$this->m()` is NOT resolved.
    let src = "<?php\nclass Foo { public function m(int $w): void {} public function go(): void { $this->m(\"abc\"); } }\n";
    assert_eq!(n(src), 0, "non-final public $this->m → silent (may be overridden)");
}

// ---- Static calls: self / parent / ClassName / static --------------------

#[test]
fn static_class_method_flagged() {
    let src = "<?php\nclass Foo { public static function m(int $w): void {} }\nFoo::m(\"abc\");\n";
    let d = only(src);
    assert!(d.message.contains("to Foo::m()"), "{}", d.message);
}

#[test]
fn self_call_to_final_method_flagged() {
    let src = "<?php\nclass Foo { final public function m(int $w): void {} public function go(): void { self::m(\"abc\"); } }\n";
    assert_eq!(n(src), 1);
}

#[test]
fn self_call_to_nonfinal_public_is_silent() {
    let src = "<?php\nclass Foo { public function m(int $w): void {} public function go(): void { self::m(\"abc\"); } }\n";
    assert_eq!(n(src), 0, "self:: to non-final public → silent (conservative guard)");
}

#[test]
fn parent_call_in_file_flagged() {
    // parent is fixed at compile time → exact, no final guard.
    let src = "<?php\nclass Base { public function m(int $w): void {} }\nclass Sub extends Base { public function go(): void { parent::m(\"abc\"); } }\n";
    let d = only(src);
    assert!(d.message.contains("to Base::m()"), "{}", d.message);
}

#[test]
fn static_call_lsb_is_silent() {
    // `static::m()` is late static binding → the running method is unknown.
    let src = "<?php\nclass Foo { public static function m(int $w): void {} public function go(): void { static::m(\"abc\"); } }\n";
    assert_eq!(n(src), 0, "static:: → unknown (LSB)");
}

// ---- Private visibility skip (call from outside the class) ----------------

#[test]
fn private_method_from_outside_is_skipped() {
    // Calling a private method from outside its class is a PHP fatal — a
    // different error we do not report. The arg check is skipped entirely.
    let src = "<?php\nclass Foo { private function m(int $w): void {} }\n(new Foo())->m(\"abc\");\n";
    assert_eq!(n(src), 0, "private method from outside → skip (not our error)");
}

// ---- Trait-using class / chain leaving the file → give up -----------------

#[test]
fn trait_using_class_is_silent() {
    // A `use`d trait merges methods from elsewhere → resolution gives up.
    let src = "<?php\ntrait T {}\nclass Foo { use T; public function __construct(int $w) {} }\nnew Foo(\"abc\");\n";
    assert_eq!(n(src), 0, "trait-using class → silent");
}

#[test]
fn chain_leaving_file_is_silent() {
    // `Foo` extends a class not defined here → the chain is incomplete → unknown.
    let src = "<?php\nclass Foo extends \\Vendor\\Base { public function go(): void { $this->m(\"abc\"); } }\n";
    assert_eq!(n(src), 0, "extends an out-of-file class → silent");
}

// ---- Binding descent into method bodies (this-context) --------------------

#[test]
fn binding_descent_two_hop_this_private() {
    // (new Foo())->go("abc") binds $s="abc" and descends into go(); inside, the
    // exact `$this` (Foo) makes `$this->inner($s)` resolve to the private inner,
    // where "abc" is a proven int mismatch. Provenance names the first site.
    let src = "<?php\nclass Foo { private function inner(int $w): void {} public function go(string $s): void { $this->inner($s); } }\n(new Foo())->go(\"abc\");\n";
    let d = only(src);
    assert!(d.message.contains("to Foo::inner()"), "{}", d.message);
    assert!(d.message.contains("from $s"), "immediate var named: {}", d.message);
    assert!(
        d.message.contains("bound at Foo::go(\"abc\") call on line 3"),
        "provenance names the first binding site: {}",
        d.message
    );
}

// ---- Exact-class fact semantics ------------------------------------------

#[test]
fn exact_class_fact_survives_method_call_while_literal_dies() {
    // An intervening *method call* on `$x` (`$x->other()`) cannot rebind the
    // caller's `$x`, so its exact-class fact survives and `$x->m("abc")` still
    // resolves. `$n` (a literal) IS invalidated by touch($n), so width($n) is
    // silent. Exactly one finding: the surviving `$x->m()`.
    let src = "<?php\nclass Foo { public function m(int $w): void {} public function other(): void {} }\nfunction touch($z): void {}\nfunction width(int $w): void {}\n$x = new Foo();\n$n = \"abc\";\n$x->other();\ntouch($n);\nwidth($n);\n$x->m(\"abc\");\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "class fact survives a method call, literal fact dies: {f:#?}");
    assert!(f[0].message.contains("to Foo::m()"), "{}", f[0].message);
    assert_eq!(f[0].line, 10);
}

#[test]
fn by_ref_rebinding_of_object_var_is_silent() {
    // ZERO-FP regression (ADR-0002). A by-ref parameter rebinds the *variable* to
    // a different object of a different class: at runtime `$x->m("abc")` calls
    // Bar::m(string), which is fine. An exact-class fact must therefore die when
    // `$x` is passed to any call — a call may take it by reference. Silent.
    let src = "<?php\nclass Foo { public function m(int $w): void {} }\nclass Bar { public function m(string $s): void {} }\nfunction swap(&$x): void { $x = new Bar(); }\n$x = new Foo();\nswap($x);\n$x->m(\"abc\");\n";
    assert_eq!(n(src), 0, "by-ref call may rebind $x's class → must be silent");
}

#[test]
fn by_value_pass_of_object_var_loses_class_fact_conservatively() {
    // The precision-loss twin: `log_it($x)` takes `$x` by value and cannot rebind
    // it, so `$x->m("abc")` would still be a real error — but the checker cannot
    // tell by-value from by-ref without callee-signature awareness, so it
    // conservatively drops the class fact here too. Silent (a missed finding, the
    // FP-safe side). Recovering it requires proving the resolved callee's
    // parameter at that position is not by-ref (deferred — not implemented now).
    let src = "<?php\nclass Foo { public function m(int $w): void {} }\nfunction log_it($o): void {}\n$x = new Foo();\nlog_it($x);\n$x->m(\"abc\");\n";
    assert_eq!(n(src), 0, "by-value pass conservatively drops the class fact → silent");
}

#[test]
fn reassigned_object_var_loses_its_class_fact() {
    // `$x` is reassigned to an unknown value before the method call, so its
    // exact-class fact is dropped and `$x->m()` no longer resolves.
    let src = "<?php\nclass Foo { public function m(int $w): void {} }\nfunction mk() { return 1; }\n$x = new Foo();\n$x = mk();\n$x->m(\"abc\");\n";
    assert_eq!(n(src), 0, "reassigned $x loses its class fact → silent");
}

// ---- Effect envelope on a method -----------------------------------------

#[test]
fn pure_method_flagged_via_method_edge_with_via_provenance() {
    // Pure f() calls $this->helper() (resolvable: helper is final+private), which
    // writes to the filesystem → a transitive envelope violation naming the
    // ultimate origin.
    let src = "<?php\nclass Foo { #[\\Steins\\Pure] final public function f(): void { $this->helper(); } final private function helper(): void { file_put_contents(\"/x\", \"y\"); } }\n";
    let f: Vec<_> = findings(src).into_iter().filter(|d| d.id == EFFECT_ID).collect();
    assert_eq!(f.len(), 1, "one effect finding, got: {f:#?}");
    assert_eq!(
        f[0].message,
        "Foo::helper() has effect io.fs.write (via file_put_contents at line 2), but Foo::f() is declared #[\\Steins\\Pure]"
    );
}

#[test]
fn pure_method_calling_rand_directly_flagged() {
    let src = "<?php\nfinal class Foo { #[\\Steins\\Pure] public function f(): int { return rand(); } }\n";
    let f: Vec<_> = findings(src).into_iter().filter(|d| d.id == EFFECT_ID).collect();
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("rand() has effect nondet.random"), "{}", f[0].message);
    assert!(f[0].message.contains("but Foo::f() is declared"), "{}", f[0].message);
}

#[test]
fn pure_method_calling_nonfinal_helper_is_silent() {
    // The helper method-edge does not resolve (non-final public `$this->`), so no
    // effect propagates — silent, not a false positive.
    let src = "<?php\nclass Foo { #[\\Steins\\Pure] public function f(): void { $this->helper(); } public function helper(): void { echo \"x\"; } }\n";
    let f: Vec<_> = findings(src).into_iter().filter(|d| d.id == EFFECT_ID).collect();
    assert_eq!(f.len(), 0, "unresolved method edge → silent");
}

// ---- Free functions and classes coexist unchanged -------------------------

#[test]
fn method_call_argument_that_is_a_function_call_still_direct_checks() {
    // A plain function call nested in a method body is still checked by the
    // function-world direct pass; the class extension does not disturb it.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nclass Foo { public function go(): void { width(\"abc\"); } }\nfunction run(): void { (new Foo())->go(); }\n";
    let d = only(src);
    assert!(d.message.contains("to width()"), "{}", d.message);
}
