//! An `Asserted` fact every member of which is rejected reports the possibly grade
//! (issue #1099, ADR-0081's 2026-10-27 amendment): `phpdoc.maybe-argument-mismatch` at an
//! argument, `phpdoc.maybe-return-mismatch` at a return, naming every member.
//!
//! Three rows carry one rule: a finite `OneOf` rejected in every member, an abstract
//! `Every` whose stratum is `Asserted`, and a `Singleton` the proof rung declined. The
//! `Verified` abstract `Every` (`string` into `?int`) stays silent on issue #291's ruling,
//! and the `Partial` rows are untouched. Every PHP verdict quoted below was run on PHP.

use steins_infer::{
    Diagnostic, ID, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, PHPDOC_MAYBE_RETURN_MISMATCH_ID,
    TYPE_MAYBE_ARGUMENT_MISMATCH_ID, check,
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

/// Every possibly-grade finding, argument or return, whichever stratum routed it.
fn maybe_family(src: &str) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| {
            d.id == PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID
                || d.id == TYPE_MAYBE_ARGUMENT_MISMATCH_ID
                || d.id == PHPDOC_MAYBE_RETURN_MISMATCH_ID
                || d.id == "type.maybe-return-mismatch"
        })
        .collect()
}

const PRELUDE: &str = "function ti(int $i): void {}\nfunction tn(?int $i): void {}\n\
                       function ts(string $s): void {}\nfunction tf(float $f): void {}\n";

fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{PRELUDE}{body}")
}

fn coercive(body: &str) -> String {
    format!("<?php\n{PRELUDE}{body}")
}

// ---------------------------------------------------------------------------
// Row 1: a finite OneOf rejected in every member (the value lane)
// ---------------------------------------------------------------------------

/// PHP: `z7('a')` throws `TypeError` (`?int` given `'a'`), and so does `z7(null)` through
/// the `else` arm (`'c'`); `z7(7)` runs clean because the docblock lies.
#[test]
fn an_if_else_join_of_literals_reports_every_member() {
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction z7(?string $s): void {\n    \
         if ($s !== null) { $y = $s; } else { $y = 'c'; }\n    tn($y);\n}\n",
    );
    let d = with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert_eq!(
        d[0].message,
        "argument $y to tn() may not become ?int $i — $y is one of \"a\", \"b\", \"c\", \
         and its \"a\", \"b\" and \"c\" arms raise a TypeError (strict mode)"
    );
    assert!(with_id(&src, TYPE_MAYBE_ARGUMENT_MISMATCH_ID).is_empty());
    assert!(run(&src).iter().all(|d| d.id != ID), "no definite id: the premise is a claim");
}

#[test]
fn the_ternary_form_reports_the_same() {
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction z8(?string $s): void {\n    \
         $y = $s !== null ? $s : 'c';\n    tn($y);\n}\n",
    );
    let d = with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert!(d[0].message.contains("one of \"a\", \"b\", \"c\""), "{}", d[0].message);
}

#[test]
fn the_same_join_on_a_mixed_parameter_reports_too() {
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction z9(mixed $s): void {\n    \
         if ($s !== null) { $y = $s; } else { $y = 'c'; }\n    tn($y);\n}\n",
    );
    assert_eq!(with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID).len(), 1, "{:?}", run(&src));
}

/// PHP: `z5('a')` throws; `z5(7)` runs clean (the docblock is what is wrong then).
#[test]
fn a_declared_literal_list_reports_every_member() {
    let src = strict("/** @param 'a'|'b' $s */\nfunction z5(mixed $s): void { ti($s); }\n");
    let d = with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert_eq!(
        d[0].message,
        "argument $s to ti() may not become int $i — $s is one of \"a\", \"b\", \
         and its \"a\" and \"b\" arms raise a TypeError (strict mode)"
    );
}

#[test]
fn the_declared_list_through_a_copy_reports_too() {
    let src = strict("/** @param 'a'|'b' $s */\nfunction z6(mixed $s): void { $y = $s; ti($y); }\n");
    assert_eq!(with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID).len(), 1, "{:?}", run(&src));
}

/// The coercive table is the same exact judgment: `'a'` is not numeric, so `ti('a')`
/// throws in a coercive file too (checked with `php -r 'function ti(int $i){} ti("a");'`).
#[test]
fn a_non_numeric_literal_list_reports_in_a_coercive_file() {
    let src = coercive("/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { ti($s); }\n");
    let d = with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert!(d[0].message.ends_with("(coercive mode)"), "{}", d[0].message);
}

#[test]
fn a_method_argument_is_judged_the_same() {
    let src = strict(
        "class K { public function m(int $i): void {} }\n\
         /** @param 'a'|'b' $s */\nfunction z(mixed $s): void { $k = new K; $k->m($s); }\n",
    );
    assert_eq!(with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID).len(), 1, "{:?}", run(&src));
}

// ---------------------------------------------------------------------------
// Row 2: an abstract Every at the Asserted stratum
// ---------------------------------------------------------------------------

/// PHP: `ti('5')` throws under strict types, whatever string the docblock lets through.
#[test]
fn an_asserted_abstract_fact_rejected_in_every_arm_reports() {
    let src = strict("/** @param string $s */\nfunction z(mixed $s): void { ti($s); }\n");
    let d = with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert_eq!(
        d[0].message,
        "argument $s to ti() may not become int $i — $s is string, \
         and its every arm raises a TypeError (strict mode)"
    );
}

#[test]
fn an_asserted_union_rejected_in_every_arm_names_the_union() {
    let src = strict("/** @param int|float $s */\nfunction z(mixed $s): void { ts($s); }\n");
    let d = with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert!(d[0].message.contains("$s is int|float, and its every arm raises"), "{}", d[0].message);
}

// ---------------------------------------------------------------------------
// Row 3: an Asserted Singleton the proof rung declined
// ---------------------------------------------------------------------------

/// PHP: `ti('x')` throws in a coercive file too; the preg-seeded group is `'x'` by the
/// docblock-grade shape the engine mints for `preg_match`.
#[test]
fn a_preg_seeded_singleton_group_reports() {
    let src = "<?php\nfunction takes_int(int $i): int { return $i; }\n\
               function p3(string $s): int { if (preg_match('/(x)/', $s, $m) === 1) \
               { return takes_int($m[1]); } return 0; }\n";
    let d = with_id(src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(src));
    assert_eq!(
        d[0].message,
        "argument $m[1] to takes_int() may not become int $i — $m[1] is \"x\", \
         and its \"x\" arm raises a TypeError (coercive mode)"
    );
}

#[test]
fn a_declared_single_literal_reports() {
    let src = strict("/** @param 'a' $s */\nfunction z(mixed $s): void { ti($s); }\n");
    let d = with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert!(d[0].message.contains("$s is \"a\", and its \"a\" arm"), "{}", d[0].message);
}

// ---------------------------------------------------------------------------
// What stays as it was
// ---------------------------------------------------------------------------

/// Decision 1 of run #1114: a `Verified` abstract `Every` stays silent (#291).
#[test]
fn a_verified_abstract_every_stays_silent() {
    for body in [
        "function z1(string $s): void { tn($s); }\n",
        "function z2(string $s): void { ti($s); }\n",
        "function z3(string $s): void { $y = $s; ti($y); }\n",
        "function z7(float $f): void { ti($f); }\n",
    ] {
        let src = strict(body);
        assert!(maybe_family(&src).is_empty(), "{body}: {:?}", run(&src));
        assert!(run(&src).iter().all(|d| d.id != ID), "{body}: {:?}", run(&src));
    }
}

/// A `Verified` `OneOf` rejected in every member is the definite id (D6), unchanged.
#[test]
fn a_verified_oneof_stays_definite() {
    let src = strict("function z4(bool $c): void { $y = $c ? 'a' : 'b'; ti($y); }\n");
    assert_eq!(with_id(&src, ID).len(), 1, "{:?}", run(&src));
    assert!(maybe_family(&src).is_empty(), "{:?}", run(&src));
}

/// The `Partial` row is untouched: some arm accepted, one rejected. (In a coercive file
/// the `string` arm of `int|string` is not wholly rejected, since a numeric string
/// coerces, so that source is silent on base and on head alike.)
#[test]
fn the_partial_row_is_unchanged() {
    let body = "/** @param int|string $v */\nfunction z(mixed $v): void { ti($v); }\n";
    let d = with_id(&strict(body), PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&strict(body)));
    assert!(d[0].message.contains("its string arm raises"), "{}", d[0].message);
    assert!(maybe_family(&coercive(body)).is_empty(), "{:?}", run(&coercive(body)));
}

/// A declared literal list with only some members rejected stays with its lane's old
/// silence: the some-rejected rows are for proven values (the arm lane has its own table).
#[test]
fn a_declared_list_rejected_in_some_members_only_stays_silent() {
    let src = strict("/** @param 1|'b' $s */\nfunction z(mixed $s): void { ti($s); }\n");
    assert!(maybe_family(&src).is_empty(), "{:?}", run(&src));
}

/// A caller passing a literal still gets the proven id through the call-site descent.
#[test]
fn the_call_site_descent_still_reports_the_proven_id() {
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction z7(?string $s): void {\n    \
         if ($s !== null) { $y = $s; } else { $y = 'c'; }\n    tn($y);\n}\n\
         z7('a');\n",
    );
    assert!(!with_id(&src, ID).is_empty(), "{:?}", run(&src));
}

// ---------------------------------------------------------------------------
// False-positive probes: PHP accepts at least one member, or the position is not judged
// ---------------------------------------------------------------------------

#[test]
fn coercive_numeric_literals_are_accepted() {
    for body in [
        "/** @param '1'|'2' $s */\nfunction z(mixed $s): void { ti($s); }\n",
        "/** @param '1'|'2' $s */\nfunction z(mixed $s): void { $y = $s; ti($y); }\n",
        "/** @param numeric-string $s */\nfunction z(mixed $s): void { ti($s); }\n",
        "/** @param 1|2 $s */\nfunction z(mixed $s): void { tf($s); }\n",
        "/** @param 1|2 $s */\nfunction z(mixed $s): void { ts($s); }\n",
        "/** @param 1.0|2.0 $s */\nfunction z(mixed $s): void { ti($s); }\n",
        "/** @param true|false $s */\nfunction z(mixed $s): void { ti($s); }\n",
        "/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { tstr($s); }\n\
         function tstr(string|Stringable $s): void {}\n",
        "/** @param '1' $s */\nfunction z(mixed $s): void { ti_def($s); }\n\
         function ti_def(int $i = 0): void {}\n",
        "/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { tc($s); }\n\
         function tc(callable $c): void {}\n",
    ] {
        let src = coercive(body);
        assert!(maybe_family(&src).is_empty(), "{body}: {:?}", run(&src));
    }
}

#[test]
fn strict_accepted_members_are_accepted() {
    for body in [
        "/** @param 1|2 $s */\nfunction z(mixed $s): void { tf($s); }\n",
        "/** @param 1|2 $s */\nfunction z(mixed $s): void { ti($s); }\n",
        "/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { ts($s); }\n",
        "/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { tm($s); }\n\
         function tm(mixed $m): void {}\n",
        "/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { tu($s); }\n\
         function tu(int|float|string $m): void {}\n",
        "/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { tstr($s); }\n\
         function tstr(string|Stringable $s): void {}\n",
        "/** @param 'a'|null $s */\nfunction z(mixed $s): void { ts($s ?? 'z'); }\n",
        "/** @param 'a'|null $s */\nfunction z(mixed $s): void { tn(null); }\n",
        "/** @param 'a' $s */\nfunction z(mixed $s): void { tv($s); }\n\
         function tv(string ...$xs): void {}\n",
        "enum Suit: int { case A = 1; case B = 2; }\n\
         /** @param Suit::A|Suit::B $s */\nfunction z(mixed $s): void { ti($s->value); }\n",
        "/** @param 'a'|'b' $s */\nfunction z(mixed $s): void { if (is_string($s)) { ts($s); } }\n",
    ] {
        let src = strict(body);
        assert!(maybe_family(&src).is_empty(), "{body}: {:?}", run(&src));
    }
}

/// `null` is not rejected where the parameter's implicit-nullable default admits it.
#[test]
fn an_implicitly_nullable_parameter_accepts_a_null_member() {
    let src = strict(
        "function g(int $i = null): void {}\n\
         /** @param null $s */\nfunction z(mixed $s): void { g($s); }\n",
    );
    assert!(maybe_family(&src).is_empty(), "{:?}", run(&src));
}

/// A by-reference parameter is not an argument position this judgment reads.
#[test]
fn a_by_reference_argument_is_not_judged_here() {
    let src = strict(
        "function r(int &$r): void {}\n\
         /** @param 'a'|'b' $s */\nfunction z(mixed $s): void { $q = $s; r($q); }\n",
    );
    assert!(with_id(&src, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID).is_empty(), "{:?}", run(&src));
}

// ---------------------------------------------------------------------------
// The return seam's twins
// ---------------------------------------------------------------------------

#[test]
fn a_returned_join_of_literals_reports_every_member() {
    let src = strict(
        "/** @param 'a'|'b'|null $s */\nfunction r7(?string $s): ?int {\n    \
         if ($s !== null) { $y = $s; } else { $y = 'c'; }\n    return $y;\n}\n",
    );
    let d = with_id(&src, PHPDOC_MAYBE_RETURN_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert_eq!(
        d[0].message,
        "return value $y may not become ?int (return type of r7()) — $y is one of \"a\", \"b\", \"c\", \
         and its \"a\", \"b\" and \"c\" arms raise a TypeError (strict mode)"
    );
}

#[test]
fn a_returned_declared_literal_list_reports_every_member() {
    let src = strict("/** @param 'a'|'b' $s */\nfunction r5(mixed $s): int { return $s; }\n");
    let d = with_id(&src, PHPDOC_MAYBE_RETURN_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&src));
    assert!(
        d[0].message.contains("$s is 'a'|'b', and its \"a\" and \"b\" arms raise"),
        "{}",
        d[0].message
    );
}

#[test]
fn a_returned_asserted_abstract_every_and_singleton_report() {
    let abstract_every =
        strict("/** @param string $s */\nfunction r(mixed $s): int { return $s; }\n");
    let d = with_id(&abstract_every, PHPDOC_MAYBE_RETURN_MISMATCH_ID);
    assert_eq!(d.len(), 1, "{:?}", run(&abstract_every));
    assert!(d[0].message.contains("its every arm raises a TypeError"), "{}", d[0].message);

    let single = strict("/** @param 'a' $s */\nfunction r(mixed $s): int { return $s; }\n");
    assert_eq!(with_id(&single, PHPDOC_MAYBE_RETURN_MISMATCH_ID).len(), 1, "{:?}", run(&single));
}

#[test]
fn a_returned_verified_every_stays_silent() {
    let src = strict("function r(string $s): int { return $s; }\n");
    assert!(maybe_family(&src).is_empty(), "{:?}", run(&src));
}

#[test]
fn a_returned_coercible_literal_list_stays_silent() {
    let src = coercive("/** @param '1'|'2' $s */\nfunction r(mixed $s): int { return $s; }\n");
    assert!(maybe_family(&src).is_empty(), "{:?}", run(&src));
}

// ---------------------------------------------------------------------------
// A guard that already removed every allowed value (review round 1)
// ---------------------------------------------------------------------------

/// A condition the walk cannot decide (`in_array`, `strlen`, a loose `==`, `ctype_*`) may
/// already have removed every value the docblock allows, so the Asserted all-rejected rows
/// decline a variable a branch condition read. PHP never reaches the call with an allowed
/// value in any of these.
#[test]
fn an_undecided_guard_on_the_variable_silences_the_asserted_rows() {
    for body in [
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (in_array($x, ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (!in_array($x, ['a', 'b'], true)) { ti($x); } }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (in_array($x, ['a', 'b'])) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if ($x == 'a' || $x == 'b') { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (strlen($x) === 1) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (preg_match('/^[ab]$/', $x)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (ctype_alpha($x)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if ($x) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (array_search($x, ['a', 'b'], true) !== false) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (in_array($x, ['a', 'b'], true)) { return; } $y = $x; ti($y); }",
        "/** @param 'a'|'b' $s */ function f(mixed $s): void { $y = $s; if ($y) { return; } ti($y); }",
        "/** @param 'a'|'b' $s */ function f(mixed $s): void { $y = $s; if ($y == 'a' || $y == 'b') { return; } ti($y); }",
        "/** @param 'a' $s */ function f(mixed $s): void { if ($s == 'a') { return; } ti($s); }",
        "/** @param 'a' $s */ function f(mixed $s): void { if (is_string($s) && $s !== '') { return; } ti($s); }",
        "/** @param string $s */ function f(mixed $s): void { if (gettype($s) === 'string') { return; } ti($s); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): int { if (in_array($x, ['a', 'b'], true)) { return 0; } return $x; }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $ok = in_array($x, ['a', 'b'], true); if ($ok) { return; } ti($x); }",
        "function chk(mixed $v, ?bool &$ok): void { $ok = in_array($v, ['a', 'b'], true); }\n\
         /** @param 'a'|'b' $x */ function f(mixed $x): void { chk($x, $ok); if ($ok) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $y = $x; if (in_array($x, ['a', 'b'], true)) { return; } ti($y); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { switch ($x) { case 'a': case 'b': return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): int { return in_array($x, ['a', 'b'], true) ? 0 : ti2($x); } function ti2(int $i): int { return $i; }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { in_array($x, ['a', 'b'], true) || ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $g = static function () use ($x) { if (in_array($x, ['a', 'b'], true)) { return; } ti($x); }; $g(); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { echo \"$x\"; ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if ($x === 'c') { ti($x); } }",
    ] {
        let src = strict(body);
        assert!(maybe_family(&src).is_empty(), "{body}: {:?}", run(&src));
    }
}

/// Reads the scan must not mistake for a copy or a null test, and constructs that read a
/// variable by name: each runs in PHP and the base is silent, so the rows decline.
#[test]
fn reads_by_name_and_non_null_tests_silence_the_asserted_rows() {
    for body in [
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $n = 'x'; if (in_array($$n, ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (in_array(${'x'}, ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $c = compact('x'); if (in_array($c['x'], ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (in_array(get_defined_vars()['x'], ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (in_array(func_get_args()[0], ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (in_array(func_get_arg(0), ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $z = $x ?? 'q'; if (in_array($z, ['a', 'b'], true)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (isset($x)) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $anull = 'a'; if ($anull === $x) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $y = $x ?? 'b'; if ($y === 'a' || $y === 'b') { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (($x ?? 1) !== 1) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { foreach (func_get_args() as $v) { if ($v === 'a') { return; } } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $GLOBALS['g'] = &$x; if (strlen($GLOBALS['g']) === 1) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): int { if (strlen(func_get_arg(0)) === 1) { return 0; } return $x; }",
    ] {
        let src = strict(body);
        assert!(maybe_family(&src).is_empty(), "{body}: {:?}", run(&src));
    }
}

/// A multibyte character anywhere in the body must not panic the scan, and a multibyte
/// name is an ordinary variable.
#[test]
fn multibyte_bodies_do_not_panic_the_scan() {
    for body in [
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $café = $x; ti($café); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $t = ('éé' === $x); ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $é = 'ü'; if ('éé' !== $x) { return; } $日本 = $x; ti($日本); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { echo 'é'; ti($x); /* éé null === $x */ }",
    ] {
        let src = strict(body);
        let _ = run(&src);
    }
    let plain = strict(
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { echo 'é'; ti($x); }",
    );
    assert_eq!(maybe_family(&plain).len(), 1, "{:?}", run(&plain));
}

/// `null` compared by `===` / `!==` as a standalone keyword is the one test a copy group
/// admits: it removes nothing the docblock allows.
#[test]
fn a_standalone_null_comparison_does_not_silence() {
    for body in [
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if ($x !== null) { ti($x); } }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (null === $x) { return; } ti($x); }",
    ] {
        let src = strict(body);
        assert_eq!(maybe_family(&src).len(), 1, "{body}: {:?}", run(&src));
    }
}

/// A null test narrows only the reported variable (or a never-copied-into ancestor tested
/// before it is copied); nested in a larger comparison, or applied to a copy, it removes
/// nothing the docblock allows, and a ternary joins both of its arms to its target.
#[test]
fn a_null_test_that_is_not_the_whole_story_silences_the_asserted_rows() {
    for body in [
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (($x === null) === false) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $x2 = $x; if ($x2 !== null) { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $x2 = $x; if ($x !== null) { return; } ti($x2); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { $y = $x === null ? 'q' : $x; if ($y === 'a' || $y === 'b') { return; } ti($x); }",
        "/** @param 'a'|'b' $x */ function f(mixed $x): void { if (strlen(json_encode(1)) === 1 && strlen(debug_backtrace()[0]['args'][0]) === 1) { return; } ti($x); }",
    ] {
        let src = strict(body);
        assert!(maybe_family(&src).is_empty(), "{body}: {:?}", run(&src));
    }
}

/// A guard that reads another variable does not silence the rows.
#[test]
fn a_guard_that_reads_another_variable_does_not_silence() {
    for body in [
        "/** @param 'a'|'b' $x */ function f(mixed $x, bool $c): void { if ($c) { return; } ti($x); }",
        "/** @param 'a'|'b' $s */ function f(mixed $s, int $n): void { if ($n > 1) { return; } $y = $s; ti($y); }",
    ] {
        let src = strict(body);
        assert_eq!(maybe_family(&src).len(), 1, "{body}: {:?}", run(&src));
    }
}

// ---------------------------------------------------------------------------
// A `return` a `finally` replaces
// ---------------------------------------------------------------------------

/// PHP: every one of these returns the `finally`'s value, whatever the `try` returned. The
/// new Asserted all-rejected return row declines them.
#[test]
fn a_return_inside_a_try_whose_finally_exits_is_not_judged() {
    for body in [
        "/** @param 'a' $s */ function f(mixed $s): ?int { try { return $s; } finally { return 1; } }",
        "/** @param 'a'|'b' $s */ function f(mixed $s): int { try { return $s; } finally { return 1; } }",
        "/** @param string $s */ function f(mixed $s): int { try { return $s; } finally { return 2; } }",
        "/** @param 'a' $s */ function f(mixed $s): int { $y = $s; try { return $y; } finally { return 5; } }",
        "/** @param 'a' $s */ function f(mixed $s): int { try { return $s; } finally { if (true) { return 6; } } }",
        "/** @param 'a' $s */ function f(mixed $s): int { try { throw new \\Exception(); } catch (\\Exception $e) { return $s; } finally { return 7; } }",
    ] {
        let src = strict(body);
        assert!(run(&src).is_empty(), "{body}: {:?}", run(&src));
    }
}

/// Master's own reports in the same shape are not this slice's to change: the Partial row
/// (f4) and the definite `type.return-mismatch` of a Verified value (f5, k1, k7, k12, whose
/// `finally` terminator never runs) stay exactly as they were.
#[test]
fn the_existing_return_rows_are_untouched_by_a_finally() {
    let f4 = strict(
        "/** @param 'a'|1 $s */ function f(mixed $s): int { try { return $s; } finally { return 3; } }",
    );
    assert_eq!(with_id(&f4, PHPDOC_MAYBE_RETURN_MISMATCH_ID).len(), 1, "{:?}", run(&f4));
    for finally in [
        "return 4;",
        "if (!$c && $c) { return 1; }",
        "foreach ([] as $_) { return 1; }",
        "while (false) { return 1; }",
    ] {
        let src = strict(&format!(
            "function f(bool $c): int {{ $y = $c ? 'a' : 'b'; try {{ return $y; }} finally {{ {finally} }} }}"
        ));
        assert_eq!(with_id(&src, "type.return-mismatch").len(), 1, "{finally}: {:?}", run(&src));
    }
}

/// A `finally` that cannot exit leaves the return judged; code after a `try` is judged.
#[test]
fn a_finally_that_cannot_exit_leaves_the_return_judged() {
    let plain = strict(
        "/** @param 'a' $s */ function f(mixed $s): int { try { return $s; } finally { echo 1; } }",
    );
    assert_eq!(with_id(&plain, PHPDOC_MAYBE_RETURN_MISMATCH_ID).len(), 1, "{:?}", run(&plain));
    let after = strict(
        "/** @param 'a' $s */ function f(mixed $s): int { try { echo 1; } finally { echo 2; } return $s; }",
    );
    assert_eq!(with_id(&after, PHPDOC_MAYBE_RETURN_MISMATCH_ID).len(), 1, "{:?}", run(&after));
}
