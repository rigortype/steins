//! Acceptance tests for ADR-0027 Feature B: interprocedural argument binding.
//!
//! When a same-file, non-poisoned user function is called with literal-resolved
//! arguments, its body is re-analyzed with those parameters bound, and a proven
//! `type.argument-mismatch` inside is reported at the inner call site with a
//! provenance chain naming the outermost binding call site. These tests exercise
//! the zero-FP rules (entry coercion, by-ref skip, depth/recursion budget) and
//! the dedup/provenance behavior.

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

fn only(src: &str) -> Diagnostic {
    let f = findings(src);
    assert_eq!(f.len(), 1, "expected exactly one finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

const WIDTH: &str = "function width(int $w): int { return $w; }\n";

// ---- The headline case: bind a literal into a callee, flag the inner call ----

#[test]
fn binds_literal_and_flags_inner_call_with_chain_provenance() {
    // outer("abc") binds $s = "abc"; inside outer, width($s) is a proven
    // coercive TypeError. Reported at the inner width($s) site, provenance names
    // the outer binding call.
    let src = format!(
        "<?php\n{WIDTH}function outer(string $s): void {{ width($s); }}\nouter(\"abc\");\n"
    );
    let d = only(&src);
    assert_eq!(d.line, 3, "reported at the inner width($s) call (line 3)");
    assert_eq!(
        d.message,
        "argument \"abc\" (from $s, bound at outer(\"abc\") call on line 4) to width() cannot become int $w — proven TypeError (coercive mode)"
    );
}

// ---- Numeric-string coercion on the descend value ------------------------

#[test]
fn numeric_string_coerces_then_flows_silently() {
    // outer("5") binds $n = 5 (numeric string coerces to int at entry). Inside,
    // 5 flows to a float parameter — int→float widening, silent.
    let src = "<?php\nfunction need_float(float $f): void {}\nfunction outer(int $n): void { need_float($n); }\nouter(\"5\");\n";
    assert_eq!(n(src), 0, "numeric string → int 5 → float: no mismatch");
}

#[test]
fn coercion_produces_value_that_proves_later_mismatch() {
    // Strict file. outer(5) binds $f = 5.0 (int→float widening at entry). Inside,
    // need_int($f) passes a float to a strict int parameter — a proven TypeError.
    // The mismatch exists only because of the entry coercion (5 became 5.0).
    let src = "<?php\ndeclare(strict_types=1);\nfunction need_int(int $x): void {}\nfunction outer(float $f): void { need_int($f); }\nouter(5);\n";
    let d = only(src);
    assert_eq!(d.line, 4, "reported at the inner need_int($f) call");
    assert_eq!(
        d.message,
        "argument 5.0 (from $f, bound at outer(5) call on line 5) to need_int() cannot become int $x — proven TypeError (strict mode)"
    );
}

// ---- Entry check fires at the outer site instead of descending -----------

#[test]
fn strict_entry_check_fires_at_outer_site_not_inner() {
    // Strict file. outer("5") already violates outer's own int parameter, so the
    // real call fatals at entry. The existing direct check reports at the OUTER
    // site; we do not also descend (no double report, no inner finding).
    let src = format!(
        "<?php\ndeclare(strict_types=1);\n{WIDTH}function outer(int $n): void {{ width($n); }}\nouter(\"5\");\n"
    );
    let d = only(&src);
    assert_eq!(d.line, 5, "reported at the outer outer(\"5\") call, not width($n)");
    assert!(!d.message.contains("bound at"), "direct-site message, no binding chain: {}", d.message);
    assert!(d.message.contains("to outer()"), "{}", d.message);
}

// ---- By-ref callee parameter → skip the whole binding --------------------

#[test]
fn by_ref_bound_param_skips_binding() {
    // outer takes $s by reference; its in-callee value is not determined by the
    // caller's literal, so the whole binding is skipped — width($s) is never
    // descended and nothing is flagged.
    let src = format!(
        "<?php\n{WIDTH}function outer(string &$s): void {{ width($s); }}\n$x = \"abc\";\nouter($x);\n"
    );
    assert_eq!(n(&src), 0, "by-ref bound parameter → no descent, no finding");
}

// ---- Recursion and depth budget ------------------------------------------

#[test]
fn self_recursion_terminates_without_finding() {
    // A self-recursive function must not hang: the on-stack (function, binding)
    // set stops the re-descent. No mismatch here, so no finding.
    let src = "<?php\nfunction f(string $x): string { f($x); return $x; }\nf(\"abc\");\n";
    assert_eq!(n(src), 0, "self-recursion terminates, no finding");
}

#[test]
fn depth_chain_of_three_flags_with_first_site_named() {
    // a("abc") → b($x) → c($y) → width($z): the literal propagates three frames
    // deep to a proven mismatch, flagged with the FIRST (a) binding site named.
    let src = format!(
        "<?php\n{WIDTH}function c(string $z): void {{ width($z); }}\nfunction b(string $y): void {{ c($y); }}\nfunction a(string $x): void {{ b($x); }}\na(\"abc\");\n"
    );
    let d = only(&src);
    assert_eq!(d.line, 3, "reported at width($z) inside c()");
    assert!(d.message.contains("from $z"), "immediate var is the innermost param: {}", d.message);
    assert!(
        d.message.contains("bound at a(\"abc\") call on line 6"),
        "provenance names the FIRST binding site (a): {}",
        d.message
    );
}

// ---- Multiple call sites and dedup ---------------------------------------

#[test]
fn two_call_sites_with_different_values_give_two_findings() {
    let src = format!(
        "<?php\n{WIDTH}function outer(string $s): void {{ width($s); }}\nouter(\"abc\");\nouter(\"xyz\");\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 2, "two bad values → two findings: {f:#?}");
    assert!(f.iter().any(|d| d.message.contains("argument \"abc\"")));
    assert!(f.iter().any(|d| d.message.contains("argument \"xyz\"")));
}

#[test]
fn same_value_two_sites_gives_one_finding_per_binding_site() {
    // Identical bound value at two distinct call sites → one finding per site,
    // kept because the provenance (line number) differs.
    let src = format!(
        "<?php\n{WIDTH}function outer(string $s): void {{ width($s); }}\nouter(\"abc\");\nouter(\"abc\");\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 2, "one finding per binding site: {f:#?}");
    assert!(f.iter().any(|d| d.message.contains("call on line 4")));
    assert!(f.iter().any(|d| d.message.contains("call on line 5")));
}

#[test]
fn binding_independent_inner_finding_is_deduped() {
    // outer has a binding-independent local finding (width($bad)) and a
    // binding-dependent one (width($s)). The local finding is reachable from
    // both the empty-env walk and the descent, but is emitted once (dedup); the
    // binding-dependent one is distinct. Two findings total.
    let src = format!(
        "<?php\n{WIDTH}function outer(string $s): void {{ $bad = \"zzz\"; width($bad); width($s); }}\nouter(\"abc\");\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 2, "local finding deduped, binding finding kept: {f:#?}");
    assert!(
        f.iter().any(|d| d.message.contains("from $bad, assigned at line")),
        "local finding present once: {f:#?}"
    );
    assert!(
        f.iter().any(|d| d.message.contains("from $s, bound at outer(\"abc\")")),
        "binding finding present: {f:#?}"
    );
}

// ---- A clean same-file call does not fire --------------------------------

#[test]
fn well_typed_binding_is_silent() {
    // outer(5) binds $n = 5, width($n) is int→int — no mismatch anywhere.
    let src = format!(
        "<?php\n{WIDTH}function outer(int $n): void {{ width($n); }}\nouter(5);\n"
    );
    assert_eq!(n(&src), 0, "well-typed propagation is silent");
}

// ---- Calls in RETURN / assignment-RHS / echo positions -------------------

#[test]
fn return_position_call_flagged_via_binding() {
    // The coordinator's reproduction: `return width($s);` is invisible to the
    // old propagation pass. It must now flag with binding provenance.
    let src = "<?php\nfunction width(int $w): int { return $w * 2; }\nfunction outer(string $s): int {\n    return width($s);\n}\nouter(\"abc\");\n";
    let d = only(src);
    assert_eq!(d.line, 4, "reported at the `return width($s)` call (line 4)");
    assert_eq!(
        d.message,
        "argument \"abc\" (from $s, bound at outer(\"abc\") call on line 6) to width() cannot become int $w — proven TypeError (coercive mode)"
    );
}

#[test]
fn return_position_direct_literal_still_flagged() {
    // `return width("abc");` — the direct pass already covered literal args in
    // return position; confirm the IR change did not regress it.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction outer(): int { return width(\"abc\"); }\nouter();\n";
    let d = only(src);
    assert_eq!(d.line, 3, "flagged at the return call");
    assert!(!d.message.contains("bound at"), "direct-literal message: {}", d.message);
    assert!(d.message.contains("argument \"abc\""));
}

#[test]
fn assignment_rhs_call_flagged_via_binding() {
    // `$x = width($s);` inside outer must be checked and descended, not merely
    // resolved for `$x`'s value.
    let src = format!(
        "<?php\n{WIDTH}function outer(string $s): void {{ $x = width($s); }}\nouter(\"abc\");\n"
    );
    let d = only(&src);
    assert_eq!(d.line, 3, "reported at the `$x = width($s)` call");
    assert!(
        d.message.contains("from $s, bound at outer(\"abc\")"),
        "binding provenance: {}",
        d.message
    );
}

#[test]
fn echo_position_call_flagged_via_binding() {
    // `echo width($s);` in outer's body — a common template shape.
    let src = format!(
        "<?php\n{WIDTH}function outer(string $s): void {{ echo width($s); }}\nouter(\"abc\");\n"
    );
    let d = only(&src);
    assert_eq!(d.line, 3, "reported at the `echo width($s)` call");
    assert!(d.message.contains("from $s, bound at outer(\"abc\")"), "{}", d.message);
}

#[test]
fn const_fn_return_literal_still_qualifies() {
    // The `Return { value, .. }` IR reshape must not break constant-function
    // recognition: `price()` returning a bad literal into width() still flags,
    // with the const-fn provenance.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction price(): string { return \"abc\"; }\nwidth(price());\n";
    let d = only(src);
    assert!(d.message.contains("from price(), defined at line 3"), "{}", d.message);
    assert!(d.message.contains("argument \"abc\""));
}

#[test]
fn return_call_direct_var_flow_flagged() {
    // Not interprocedural: a local `$w` flowing into `return width($w)` at top
    // level is now caught by propagation (return position), where before it was
    // silent.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction top(): int { $w = \"abc\"; return width($w); }\ntop();\n";
    let d = only(src);
    assert_eq!(d.line, 3);
    assert!(d.message.contains("from $w, assigned at line 3"), "{}", d.message);
}
