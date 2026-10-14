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

// The headline case: bind a literal into a callee, flag the inner call

#[test]
fn binds_literal_and_flags_inner_call_with_chain_provenance() {
    // outer("abc") binds $s = "abc"; inside outer, width($s) is a proven coercive TypeError.
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

// Numeric-string coercion on the descend value

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

// Entry check fires at the outer site instead of descending

#[test]
fn strict_entry_check_fires_at_outer_site_not_inner() {
    // Strict file. outer("5") already violates outer's own int parameter, so the
    // direct check reports at the OUTER site; we do not also descend.
    let src = format!(
        "<?php\ndeclare(strict_types=1);\n{WIDTH}function outer(int $n): void {{ width($n); }}\nouter(\"5\");\n"
    );
    let d = only(&src);
    assert_eq!(d.line, 5, "reported at the outer outer(\"5\") call, not width($n)");
    assert!(!d.message.contains("bound at"), "direct-site message, no binding chain: {}", d.message);
    assert!(d.message.contains("to outer()"), "{}", d.message);
}

// By-ref callee parameter → skip the whole binding

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

// Recursion and depth budget

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

// Multiple call sites and dedup

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
    // Identical bound value, two sites → kept separately since provenance (line) differs.
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
    // width($bad) is reachable from both the empty-env walk and the descent, but
    // emitted once (dedup); width($s) is binding-dependent and distinct.
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

// A clean same-file call does not fire

#[test]
fn well_typed_binding_is_silent() {
    let src = format!(
        "<?php\n{WIDTH}function outer(int $n): void {{ width($n); }}\nouter(5);\n"
    );
    assert_eq!(n(&src), 0, "well-typed propagation is silent");
}

// Calls in RETURN / assignment-RHS / echo positions

#[test]
fn return_position_call_flagged_via_binding() {
    // `return width($s);` must flag with binding provenance: a call in return
    // position is reachable to the propagation pass, not only assignment RHS.
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
    // `return width("abc");` — the direct pass covers literal args in return
    // position; regression guard that the IR reshape kept this working.
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
    let src = format!(
        "<?php\n{WIDTH}function outer(string $s): void {{ echo width($s); }}\nouter(\"abc\");\n"
    );
    let d = only(&src);
    assert_eq!(d.line, 3, "reported at the `echo width($s)` call");
    assert!(d.message.contains("from $s, bound at outer(\"abc\")"), "{}", d.message);
}

#[test]
fn const_fn_return_literal_still_qualifies() {
    // Regression guard: the `Return { value, .. }` IR reshape must not break
    // constant-function recognition.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction price(): string { return \"abc\"; }\nwidth(price());\n";
    let d = only(src);
    assert!(d.message.contains("from price(), defined at line 3"), "{}", d.message);
    assert!(d.message.contains("argument \"abc\""));
}

#[test]
fn return_call_direct_var_flow_flagged() {
    // Local propagation also checks return position: `$w` flowing into
    // `return width($w)` is caught without interprocedural descent.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction top(): int { $w = \"abc\"; return width($w); }\ntop();\n";
    let d = only(src);
    assert_eq!(d.line, 3);
    assert!(d.message.contains("from $w, assigned at line 3"), "{}", d.message);
}

/// A stand-in for the sidecar's `preg_match` fold: a `/x/` pattern over a literal
/// subject answers whether the subject contains `x`, as PHP does.
struct PregFold;

impl steins_infer::Folder for PregFold {
    fn fold(
        &mut self,
        name: &str,
        args: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        use steins_syntax::ArgValue;
        let [ArgValue::Str(p), ArgValue::Str(s)] = args else { return None };
        if !name.eq_ignore_ascii_case("preg_match") {
            return None;
        }
        let text = |v: &steins_domain::PhpStr| String::from_utf8_lossy(v.as_bytes()).into_owned();
        let needle = text(p).trim_matches('/').to_owned();
        Some(ArgValue::Int(i64::from(text(s).contains(&needle))))
    }
}

fn bound_findings(src: &str) -> usize {
    let tree = SourceTree::parse(src);
    steins_infer::check_with(&tree, &[], "test.php", &mut PregFold)
        .iter()
        .filter(|d| d.message.contains("bound at"))
        .count()
}

/// A call condition the fold decides is decided, in a binding descent as anywhere
/// (ADR-0105 §2.6). Each row is PHP 8.5.11's verdict on `outer("abc", "abc")`:
///
/// ```text
/// row  condition                                  sides with width($s)   PHP        here
/// b01  preg_match('/x/', $f)  (false)             then, else             TypeError  1 (else)
/// b02  preg_match('/a/', $f)  (true)              then                   TypeError  1
/// b03  preg_match('/q/', $f) || rand() >= 0       then                   TypeError  1 (undecided)
/// b06  preg_match('/a/', $f), then rand() >= 0    inner then             TypeError  1
/// b07  $g = $f . ""; preg_match('/q/', $g)        then                   clean      0
/// b09  preg_match('/q/', $f, $m)  (false)         then                   clean      0
/// ```
#[test]
fn a_call_condition_the_fold_decides_is_decided_in_a_descent() {
    let row = |body: &str| {
        format!(
            "<?php\n{WIDTH}function outer(string $f, string $s): void {{ {body} }}\nouter(\"abc\", \"abc\");\n"
        )
    };
    let cases = [
        ("if (preg_match('/x/', $f)) { width($s); } else { width($s); }", 1),
        ("if (preg_match('/a/', $f)) { width($s); }", 1),
        ("if (preg_match('/q/', $f) || rand(0, 1) >= 0) { width($s); }", 1),
        ("if (preg_match('/a/', $f)) { if (rand(0, 1) >= 0) { width($s); } }", 1),
        ("$g = $f . \"\"; if (preg_match('/q/', $g)) { width($s); }", 0),
        ("if (preg_match('/q/', $f, $m)) { width($s); }", 0),
    ];
    for (body, n) in cases {
        assert_eq!(bound_findings(&row(body)), n, "{body}");
    }
}

/// A call whose `@phpstan-assert` a bound value contradicts ends what a binding descent
/// proves on that path (ADR-0105 §2.6): `null` into an asserted `string` throws in the
/// helper (PHP 8.5.11: `InvalidArgumentException`), or the docblock lies; either way the
/// `V::mk(null)` after it is not a proven TypeError. Reached by a plain call, a static
/// call, a method call on a resolved receiver, and an assert nested in an argument; an
/// assert the value satisfies (`string|null`) withholds nothing.
#[test]
fn a_contradicted_assert_withholds_the_rest_of_the_descent_path() {
    let prelude = "<?php\n\
        final class A { /** @phpstan-assert string $s */ public static function str($s) { if (!is_string($s)) { throw new InvalidArgumentException('x'); } return $s; }\n\
          /** @phpstan-assert string $s */ public function inst($s) { if (!is_string($s)) { throw new InvalidArgumentException('x'); } return $s; }\n\
          /** @phpstan-assert string|null $s */ public static function maybe($s) { return $s; } }\n\
        /** @phpstan-assert string $s */ function str_of($s) { if (!is_string($s)) { throw new InvalidArgumentException('x'); } return $s; }\n\
        final class V { public static function mk(string $f): void {} }\n";
    let row = |body: &str| format!("{prelude}final class D {{ public static function c($x): void {{ {body} V::mk($x); }} }}\nD::c(null);\n");
    for body in ["A::str($x);", "str_of($x);", "$a = new A(); $a->inst($x);", "strlen(A::str($x));"] {
        let src = row(body);
        assert_eq!(bound_findings(&src), 0, "{body}");
    }
    assert_eq!(bound_findings(&row("A::maybe($x);")), 1, "a satisfied assert withholds nothing");
}
