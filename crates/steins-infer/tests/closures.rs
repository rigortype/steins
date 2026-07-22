//! Stage A-runtime acceptance tests (ADR-0033): closures as env values with
//! by-value capture snapshots, and `$fn(...)` variable-call binding descent.
//!
//! The two adversarial duties:
//!   1. capture-then-mutate — the closure sees the SNAPSHOT taken at creation, not
//!      the variable's later value (naive late-lookup would misbind);
//!   2. reassign-between-create-and-call — a closure reassigned before the call
//!      uses the NEW closure's behavior (stale-fact FP risk).

use steins_infer::{check, Diagnostic};
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

const WIDTH: &str = "function width(int $w): int { return $w; }\n";

// ---- Adversarial #1: capture-then-mutate uses the snapshot -----------------

#[test]
fn capture_snapshot_good_value_stays_silent() {
    // $x = 1 at creation → closure captures 1. Mutating $x to "abc" afterward does
    // NOT change what the closure sees: it descends with $x = 1 → width(1) OK.
    let src = format!(
        "<?php\n{WIDTH}$x = 1;\n$f = fn() => width($x);\n$x = \"abc\";\n$f();\n"
    );
    assert_eq!(n(&src), 0, "closure sees the snapshot value 1 — silent");
}

#[test]
fn capture_snapshot_bad_value_fires_with_closure_provenance() {
    // The inverse: $x = "abc" at creation → the SNAPSHOT is the bad value. A later
    // $x = 1 must not rescue it: the closure descends with $x = "abc" → width("abc")
    // is a proven coercive TypeError, reported with the closure call in provenance.
    let src = format!(
        "<?php\n{WIDTH}$x = \"abc\";\n$f = fn() => width($x);\n$x = 1;\n$f();\n"
    );
    let d = only(&src);
    assert!(
        d.message.contains("cannot become int $w") && d.message.contains("closure"),
        "fires with closure provenance: {}",
        d.message
    );
}

// ---- Descent type check: an arrow param mismatch at the $fn() site ----------

#[test]
fn closure_arg_mismatch_fires_at_call_site() {
    // $f = fn(int $w) => width($w); $f("abc") — the literal "abc" against the arrow
    // param int $w is a proven coercive TypeError at the call site.
    let src = format!("<?php\n{WIDTH}$f = fn(int $w) => width($w);\n$f(\"abc\");\n");
    let d = only(&src);
    assert!(d.message.contains("cannot become int $w"), "{}", d.message);
    assert_eq!(d.line, 4, "reported at the $f(\"abc\") call site");
}

#[test]
fn closure_arg_good_value_is_silent() {
    let src = format!("<?php\n{WIDTH}$f = fn(int $w) => width($w);\n$f(5);\n");
    assert_eq!(n(&src), 0, "5 into int $w is fine");
}

// ---- Adversarial #2: reassignment governs ----------------------------------

#[test]
fn reassigned_closure_uses_new_behavior() {
    // $f is first a param-less closure, then reassigned to fn(int $w) => …. The
    // call $f("abc") must use the SECOND closure (int $w) → fires. A stale fact
    // from the first closure would wrongly stay silent.
    let src = format!(
        "<?php\n{WIDTH}$f = fn() => 1;\n$f = fn(int $w) => width($w);\n$f(\"abc\");\n"
    );
    let d = only(&src);
    assert!(d.message.contains("cannot become int $w"), "{}", d.message);
}

#[test]
fn reassignment_to_safe_closure_silences() {
    // The reverse: a bad-arg closure reassigned to a param-less one → the call is
    // silent (the new closure ignores the arg).
    let src = format!(
        "<?php\n{WIDTH}$f = fn(int $w) => width($w);\n$f = fn() => 1;\n$f(\"abc\");\n"
    );
    assert_eq!(n(&src), 0, "the live closure takes no int param — silent");
}

// ---- By-ref use poison preserved -------------------------------------------

#[test]
fn by_ref_use_poison_suppresses_descent() {
    // A by-ref `use (&$x)` poisons the enclosing scope, so no closure value is
    // tracked and the $f() call resolves nothing — silent (honest give-up).
    let src = format!(
        "<?php\n{WIDTH}$w = 5;\n$f = function () use (&$w) {{ return width($w); }};\n$f();\n"
    );
    assert_eq!(n(&src), 0, "by-ref use poisons — no false positive");
}

// ---- String-callable dispatch ----------------------------------------------

#[test]
fn string_callable_resolves_as_function_name() {
    // $fn = 'width'; $fn("abc") — the proven string resolves as the function name,
    // so the "abc" argument is checked against width(int $w) → fires.
    let src = format!("<?php\n{WIDTH}$fn = \"width\";\n$fn(\"abc\");\n");
    let d = only(&src);
    assert!(d.message.contains("cannot become int"), "{}", d.message);
}

// ---- Unresolved variable call stays silent ---------------------------------

#[test]
fn unresolved_var_call_is_silent() {
    // $f is unknown (a parameter with no proven value) → $f("abc") resolves no
    // target → silent (opaque; no false positive).
    let src = "<?php\nfunction run($f) { $f(\"abc\"); }\n";
    assert_eq!(n(src), 0, "unresolved $f() is silent");
}
