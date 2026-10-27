//! An `Asserted` fact every member of which is rejected reports the possibly id
//! (issue #1099): `phpdoc.maybe-argument-mismatch` / `phpdoc.maybe-return-mismatch`,
//! naming every member.
//!
//! Before it, such a fact fell between the rungs: the proof layer needs a `Verified`
//! premise (ADR-0052 §5) and the possibly grade emitted only a partial rejection
//! (issue #291's ruling), so `@param 'a'|'b'|null $s` joined with `'c'` and passed to
//! `?int` was silent while the same fact with one accepted member reported. The
//! `Verified` all-arms leg (`string $s` into `?int`) stays silent on #291's ruling,
//! and is pinned below from the other side.

use steins_infer::{
    Diagnostic, ID, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, PHPDOC_MAYBE_RETURN_MISMATCH_ID,
    TYPE_MAYBE_ARGUMENT_MISMATCH_ID, TYPE_MAYBE_RETURN_MISMATCH_ID, check,
};
use steins_syntax::SourceTree;

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php")
}

fn with_id(src: &str, id: &str) -> Vec<Diagnostic> {
    run(src).into_iter().filter(|d| d.id == id).collect()
}

fn maybe_arg(src: &str) -> Vec<Diagnostic> {
    with_id(src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID)
}

fn maybe_ret(src: &str) -> Vec<Diagnostic> {
    with_id(src, PHPDOC_MAYBE_RETURN_MISMATCH_ID)
}

/// Every possibly-grade finding of either side and either stratum.
fn any_maybe(src: &str) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| {
            [
                PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID,
                TYPE_MAYBE_ARGUMENT_MISMATCH_ID,
                PHPDOC_MAYBE_RETURN_MISMATCH_ID,
                TYPE_MAYBE_RETURN_MISMATCH_ID,
            ]
            .contains(&d.id)
        })
        .collect()
}

const PRELUDE: &str = "function takes_nint(?int $i): void {}\nfunction takes_int(int $i): void {}\n";

fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{PRELUDE}{body}")
}

fn coercive(body: &str) -> String {
    format!("<?php\n{PRELUDE}{body}")
}

// The headline: an Asserted fact whose every member fails the parameter

#[test]
fn a_join_of_asserted_literals_and_a_literal_reports() {
    // v1099 z7: the `'c'` of the else branch is as wrong as `'a'|'b'`.
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction z7(?string $s): void {\n    \
         if ($s !== null) { $y = $s; } else { $y = 'c'; }\n    takes_nint($y);\n}\n",
    );
    let d = maybe_arg(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "argument $y to takes_nint() may not become ?int $i — $y is one of \"a\", \"b\", \"c\", \
         and every one of its arms (\"a\", \"b\", \"c\") raises a TypeError (strict mode)"
    );
}

#[test]
fn the_ternary_form_reports() {
    // v1099 z8.
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction z8(?string $s): void {\n    \
         $y = $s !== null ? $s : 'c';\n    takes_nint($y);\n}\n",
    );
    assert_eq!(maybe_arg(&src).len(), 1, "{:?}", run(&src));
}

#[test]
fn a_declared_literal_union_on_a_mixed_parameter_reports() {
    // v1099b z5 / v1099 z10: the declared-arm lane's finite lowering.
    let src = strict("/** @param 'a'|'b' $s */\nfunction z5(mixed $s): void { takes_int($s); }\n");
    let d = maybe_arg(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("every one of its arms (\"a\", \"b\")"), "{}", d[0].message);
}

#[test]
fn a_copy_of_a_declared_literal_union_reports() {
    // v1099b z6 / v1099 z12.
    let src = strict(
        "/** @param 'a'|'b' $s */\nfunction z6(mixed $s): void { $y = $s; takes_int($y); }\n",
    );
    assert_eq!(maybe_arg(&src).len(), 1, "{:?}", run(&src));
}

#[test]
fn an_asserted_singleton_reports() {
    let src = strict("/** @param 'a' $s */\nfunction f(mixed $s): void { takes_int($s); }\n");
    let d = maybe_arg(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its only value \"a\" raises a TypeError"), "{}", d[0].message);
}

#[test]
fn the_preg_capture_seed_reports() {
    // w1096d p3: the seeded `'x'` is an Asserted singleton on the shape lane.
    let src = coercive(
        "function p3(string $s): int {\n    \
         if (preg_match('/(x)/', $s, $m) === 1) { return takes_int2($m[1]); }\n    return 0;\n}\n\
         function takes_int2(int $i): int { return $i; }\n",
    );
    let d = maybe_arg(&src);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert!(d[0].message.contains("$m[1]"), "{}", d[0].message);
}

// What stays silent or unchanged

#[test]
fn a_verified_all_arms_rejected_fact_stays_silent() {
    // Decision 1 of the run: issue #291's ruling stands at `Verified`.
    for body in [
        "function z1(string $s): void { takes_nint($s); }\n",
        "function z2(string $s): void { takes_int($s); }\n",
        "function z3(string $s): void { $y = $s; takes_int($y); }\n",
        "function z7(float $f): void { takes_int($f); }\n",
    ] {
        let src = strict(body);
        assert!(any_maybe(&src).is_empty(), "{body}: {:?}", any_maybe(&src));
        assert!(run(&src).iter().all(|d| d.id != ID), "{body}: and no definite id either");
    }
}

#[test]
fn a_verified_finite_fact_keeps_its_definite_id() {
    // v1099b z4: every member of a proven `OneOf` is refused, so every path throws.
    let src = strict("function z4(bool $c): void { $y = $c ? 'a' : 'b'; takes_int($y); }\n");
    assert_eq!(with_id(&src, ID).len(), 1, "{:?}", run(&src));
    assert!(any_maybe(&src).is_empty(), "{:?}", any_maybe(&src));
}

#[test]
fn a_partial_rejection_keeps_the_possibly_grade_in_strict() {
    let src = strict("/** @param int|string $v */\nfunction f(mixed $v): void { takes_int($v); }\n");
    let d = maybe_arg(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its string arm raises"), "{}", d[0].message);
}

#[test]
fn an_accepting_parameter_is_silent() {
    let src = strict(
        "function takes_str(string $s): void {}\n/** @param 'a'|'b' $s */\n\
         function f(mixed $s): void { takes_str($s); }\n",
    );
    assert!(any_maybe(&src).is_empty(), "{:?}", any_maybe(&src));
}

#[test]
fn a_literal_caller_still_reports_the_proven_finding() {
    // w1099: the call-site descent proves the `'a'` path; the possibly id rides beside it.
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction z7(?string $s): void {\n    \
         if ($s !== null) { $y = $s; } else { $y = 'c'; }\n    takes_nint($y);\n}\nz7('a');\n",
    );
    assert!(!with_id(&src, ID).is_empty(), "{:?}", run(&src));
}

// The coercive table: non-numeric strings fail in PHP too, numeric ones convert

#[test]
fn coercive_non_numeric_literals_report_and_numeric_ones_stay_silent() {
    let bad = coercive("/** @param 'a'|'b' $s */\nfunction f(mixed $s): void { takes_int($s); }\n");
    assert_eq!(maybe_arg(&bad).len(), 1, "{:?}", run(&bad));
    let good = coercive("/** @param '1'|'2' $s */\nfunction f(mixed $s): void { takes_int($s); }\n");
    assert!(any_maybe(&good).is_empty(), "{:?}", any_maybe(&good));
}

#[test]
fn coercive_mixed_literals_keep_the_partial_row() {
    let src = coercive("/** @param 'a'|'1' $s */\nfunction f(mixed $s): void { takes_int($s); }\n");
    let d = maybe_arg(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its \"a\" arm raises"), "{}", d[0].message);
}

// The return seam

#[test]
fn a_returned_declared_literal_union_reports() {
    let src = strict("/** @param 'a'|'b' $s */\nfunction r1(mixed $s): int { return $s; }\n");
    let d = maybe_ret(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("every one of its arms (\"a\", \"b\")"), "{}", d[0].message);
}

#[test]
fn a_returned_copy_reports() {
    let src = strict(
        "/** @param 'a'|'b' $s */\nfunction r2(mixed $s): int { $y = $s; return $y; }\n",
    );
    assert_eq!(maybe_ret(&src).len(), 1, "{:?}", run(&src));
}

#[test]
fn a_returned_numeric_literal_union_is_a_strict_error_and_a_coercive_pass() {
    let strict_src = strict("/** @param '1'|'2' $s */\nfunction r3(mixed $s): int { return $s; }\n");
    assert_eq!(maybe_ret(&strict_src).len(), 1, "{:?}", run(&strict_src));
    let coercive_src = coercive("/** @param '1'|'2' $s */\nfunction c4(mixed $s): int { return $s; }\n");
    assert!(any_maybe(&coercive_src).is_empty(), "{:?}", any_maybe(&coercive_src));
}

#[test]
fn a_coercive_non_numeric_return_reports() {
    let src = coercive("/** @param 'a'|'b' $s */\nfunction c3(mixed $s): int { return $s; }\n");
    assert_eq!(maybe_ret(&src).len(), 1, "{:?}", run(&src));
}

#[test]
fn a_returned_native_string_stays_silent() {
    let src = strict("function r4(string $s): int { return $s; }\n");
    assert!(any_maybe(&src).is_empty(), "{:?}", any_maybe(&src));
}

#[test]
fn a_returned_mixed_union_keeps_the_partial_row() {
    let src = strict("/** @param 'a'|'b'|int $s */\nfunction r5(mixed $s): int { return $s; }\n");
    let d = maybe_ret(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its 'a' and 'b' arms raise"), "{}", d[0].message);
}
