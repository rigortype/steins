//! ADR-0043's 2026-10-11 amendment on #950: `return f(args)` is judged as
//! `$v = f(args); return $v;` is (the rule table in `return_call`), and a finite fact
//! with several members (`OneOf`) is judged member by member (the rule table in
//! `finite_verdict`). One test per row of either table.
//!
//! Every verdict was run on PHP 8.5.11 in both modes; the witness scripts are quoted
//! in the comment of the test they pin.

use std::collections::HashMap;

use steins_infer::{Diagnostic, Folder, check_with};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, SourceTree};

/// A folder that knows `strtoupper` and one builtin parameter list.
struct Boot {
    params: HashMap<String, Vec<BuiltinParam>>,
}

impl Boot {
    fn new() -> Self {
        let mut params = HashMap::new();
        params.insert(
            "strlen".to_owned(),
            vec![BuiltinParam {
                name: "string".to_owned(),
                ty: Some("string".to_owned()),
                by_ref: false,
                variadic: false,
                optional: false,
            }],
        );
        Boot { params }
    }
}

impl Folder for Boot {
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => {
                Some(ArgValue::Str(steins_domain::PhpStr::from_vec(s.as_bytes().to_ascii_uppercase())))
            }
            _ => None,
        }
    }
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        self.params.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Boot::new())
        .into_iter()
        .filter(|d| d.id.starts_with("type.") || d.id.starts_with("phpdoc."))
        .collect()
}

fn ids(src: &str) -> Vec<String> {
    findings(src).into_iter().map(|d| d.id.to_owned()).collect()
}

fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{body}")
}

fn coercive(body: &str) -> String {
    format!("<?php\n{body}")
}

const RET: &str = "type.return-mismatch";
const ARG: &str = "type.argument-mismatch";
const MAYBE_RET: &str = "type.maybe-return-mismatch";
const MAYBE_ARG: &str = "type.maybe-argument-mismatch";
const PHPDOC_MAYBE_RET: &str = "phpdoc.maybe-return-mismatch";
const PHPDOC_MAYBE_ARG: &str = "phpdoc.maybe-argument-mismatch";

fn fires(src: &str, id: &str) {
    assert_eq!(ids(src), vec![id.to_owned()], "expected one {id} for:\n{src}");
}

fn silent(src: &str) {
    let got = ids(src);
    assert!(got.is_empty(), "expected silence, got {got:?} for:\n{src}");
}

/// The return spelling and the assignment spelling of one call report the same ids,
/// and the return spelling reports `expected` (or nothing for `None`).
fn parity(prelude: &str, ret_ty: &str, call: &str, expected: Option<&str>) {
    for mode in [strict, coercive] {
        let returned = mode(&format!("{prelude}\nfunction g(): {ret_ty} {{ return {call}; }}\n"));
        let assigned =
            mode(&format!("{prelude}\nfunction g(): {ret_ty} {{ $v = {call}; return $v; }}\n"));
        let (r, a) = (ids(&returned), ids(&assigned));
        assert_eq!(r, a, "the spellings disagree for {call}:\n{returned}");
        assert_eq!(r, expected.map(str::to_owned).into_iter().collect::<Vec<_>>(), "{returned}");
    }
}

// ---------------------------------------------------------------------------
// Table 1: `return f(args)` (R6)
// ---------------------------------------------------------------------------

#[test]
fn a_project_call_with_arguments_is_judged_by_its_summary() {
    // #950 g2. PHP 8.5.11: `s(1)` returns "abc"; `: int` refuses it in both modes.
    parity("function s(int $n): string { return \"abc\"; }", "int", "s(1)", Some(RET));
}

#[test]
fn a_nullary_project_call_still_reports() {
    // #950 g1, the kept shape.
    parity("function s0(): string { return \"abc\"; }", "int", "s0()", Some(RET));
}

#[test]
fn a_project_call_whose_summary_depends_on_its_arguments() {
    // The callee returns its argument, so the summary exists only for this call's
    // literal. PHP 8.5.11: `id("abc")` reaches `: int` and throws.
    parity("function id($v) { return $v; }", "int", "id(\"abc\")", Some(RET));
    parity("function id($v) { return $v; }", "int", "id(5)", None);
}

#[test]
fn a_method_call_is_judged_as_its_assignment() {
    let prelude = "class K { function m(int $n): string { return \"abc\"; } }";
    parity(&format!("{prelude}\n$k = new K();"), "int", "(new K())->m(1)", Some(RET));
}

#[test]
fn a_method_call_on_this_is_judged_as_its_assignment() {
    for mode in [strict, coercive] {
        let returned = mode(
            "final class K {\n function m(int $n): string { return \"abc\"; }\n function g(): int { return $this->m(1); }\n}\n",
        );
        let assigned = mode(
            "final class K {\n function m(int $n): string { return \"abc\"; }\n function g(): int { $v = $this->m(1); return $v; }\n}\n",
        );
        assert_eq!(ids(&returned), vec![RET.to_owned()], "{returned}");
        assert_eq!(ids(&returned), ids(&assigned));
    }
}

#[test]
fn a_static_call_is_judged_as_its_assignment() {
    let prelude = "class K { static function m(int $n): string { return \"abc\"; } }";
    parity(prelude, "int", "K::m(1)", Some(RET));
}

#[test]
fn a_builtin_call_is_judged_as_its_assignment() {
    // The fold answers `"ABC"`; PHP 8.5.11: `: int` refuses it in both modes.
    parity("", "int", "strtoupper(\"abc\")", Some(RET));
    parity("", "string", "strtoupper(\"abc\")", None);
}

#[test]
fn a_builtin_asked_to_return_a_string_is_not_convicted_of_its_other_arms() {
    // PHP 8.5.11, strict: `var_export([1], true)` and `print_r([1], true)` return a
    // string, `var_export([1])` returns null. The declared floor is per name
    // (`string|null`, `string|true`), so the literal `true` has to narrow it.
    for mode in [strict, coercive] {
        for call in ["var_export($v, true)", "print_r($v, true)"] {
            let src = mode(&format!("function g($v): string {{ return {call}; }}\n"));
            silent(&src);
            let src = mode(&format!("function g($v): string {{ $s = {call}; return $s; }}\n"));
            silent(&src);
        }
    }
    let src = strict("function g($v): string { return var_export($v); }\n");
    fires(&src, PHPDOC_MAYBE_RET);
    let src = strict("function g($v): string { return var_export($v, false); }\n");
    fires(&src, PHPDOC_MAYBE_RET);
}

#[test]
fn a_call_that_proves_nothing_stays_silent_in_both_spellings() {
    parity("function s(int $n): string { return (string) rand(); }", "int", "s(1)", None);
    parity("function s(int $n): int { return $n; }", "int", "s(1)", None);
}

#[test]
fn a_call_whose_declared_arms_are_partly_rejected_reports_the_possibly_grade() {
    // The summary degrades to the declared floor `int|string`; the `string` arm is
    // refused by `: int` in strict mode.
    let prelude = "function s(int $n): int|string { return rand() ? 1 : \"a\"; }";
    let returned = strict(&format!("{prelude}\nfunction g(): int {{ return s(1); }}\n"));
    let assigned = strict(&format!("{prelude}\nfunction g(): int {{ $v = s(1); return $v; }}\n"));
    assert_eq!(ids(&returned), ids(&assigned), "{returned}");
}

#[test]
fn a_returned_call_under_a_docblock_contract_matches_its_assignment() {
    let prelude = "function s(int $n) { return \"abc\"; }";
    for mode in [strict, coercive] {
        let returned = mode(&format!(
            "{prelude}\n/** @return int */\nfunction g() {{ return s(1); }}\n"
        ));
        let assigned = mode(&format!(
            "{prelude}\n/** @return int */\nfunction g() {{ $v = s(1); return $v; }}\n"
        ));
        let all = |src: &str| {
            let tree = SourceTree::parse(src);
            check_with(&tree, &[], "t.php", &mut Boot::new())
                .into_iter()
                .map(|d| d.id.to_owned())
                .filter(|i| i == "return.mismatch" || i.starts_with("phpdoc."))
                .collect::<Vec<_>>()
        };
        assert_eq!(all(&returned), all(&assigned), "{returned}");
    }
}

#[test]
fn a_returned_variable_and_a_returned_literal_are_untouched() {
    fires(&strict("function g(): int { $v = \"abc\"; return $v; }"), RET);
    fires(&strict("function g(): int { return \"abc\"; }"), RET);
}

// ---------------------------------------------------------------------------
// Table 2: a finite fact with several members (R5)
// ---------------------------------------------------------------------------

#[test]
fn a_singleton_is_the_exact_rungs_and_stays_definite() {
    // Singleton, 1 of 1 rejected.
    fires(&strict("function ti(int $i): void {}\nfunction h() { $b = null; ti($b); }"), ARG);
    fires(&strict("function h(): int { $b = null; return $b; }"), RET);
}

#[test]
fn a_oneof_with_some_members_rejected_is_the_possibly_grade_at_the_argument() {
    // #950 h2. PHP 8.5.11: `ti(null)` throws on the null path, `ti(1)` runs.
    let body = "function ti(int $i): void {}\nfunction h(bool $r) { $b = $r ? null : 1; ti($b); }";
    for src in [strict(body), coercive(body)] {
        let d = findings(&src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].id, MAYBE_ARG);
        assert!(d[0].message.contains("one of null, 1"), "{}", d[0].message);
        assert!(d[0].message.contains("its null arm raises a TypeError"), "{}", d[0].message);
    }
}

#[test]
fn a_oneof_with_some_members_rejected_is_the_possibly_grade_at_the_return() {
    // #950 h3.
    let body = "function h(bool $r): int { $b = $r ? null : 1; return $b; }";
    for src in [strict(body), coercive(body)] {
        fires(&src, MAYBE_RET);
    }
}

#[test]
fn a_oneof_with_every_member_rejected_is_definite_at_the_argument() {
    // Decision D6: every path throws. PHP 8.5.11: both `ti(null)` and `ti([])` throw.
    let body = "function ti(int $i): void {}\nfunction h(bool $r) { $b = $r ? null : []; ti($b); }";
    for src in [strict(body), coercive(body)] {
        let d = findings(&src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].id, ARG);
        assert!(d[0].message.contains("proven TypeError"), "{}", d[0].message);
    }
}

#[test]
fn a_oneof_with_every_member_rejected_is_definite_at_the_return() {
    let body = "function h(bool $r): int { $b = $r ? null : \"abc\"; return $b; }";
    for src in [strict(body), coercive(body)] {
        let d = findings(&src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].id, RET);
        assert!(d[0].message.contains("every path"), "{}", d[0].message);
    }
}

#[test]
fn a_oneof_with_no_member_rejected_is_silent() {
    for body in [
        "function ti(int $i): void {}\nfunction h(bool $r) { $b = $r ? 1 : 2; ti($b); }",
        "function h(bool $r): int { $b = $r ? 1 : 2; return $b; }",
        "function ti(?int $i): void {}\nfunction h(bool $r) { $b = $r ? null : 1; ti($b); }",
        "function h(bool $r): ?int { $b = $r ? null : 1; return $b; }",
    ] {
        silent(&strict(body));
        silent(&coercive(body));
    }
}

#[test]
fn a_member_is_judged_in_the_files_own_mode() {
    // PHP 8.5.11, `ti("5")`: strict throws, coercive converts. `ti("abc")` throws in
    // both. So `"5"|"abc"` is every-member in strict mode and some-member in coercive.
    let arg = "function ti(int $i): void {}\nfunction h(bool $r) { $b = $r ? \"5\" : \"abc\"; ti($b); }";
    fires(&strict(arg), ARG);
    fires(&coercive(arg), MAYBE_ARG);
    let ret = "function h(bool $r): int { $b = $r ? \"5\" : \"abc\"; return $b; }";
    fires(&strict(ret), RET);
    fires(&coercive(ret), MAYBE_RET);
}

#[test]
fn an_int_member_widens_to_float_but_a_string_member_does_not_in_strict_mode() {
    // PHP 8.5.11, `tf(1)` runs in both modes (int widens); `tf("1")` throws strictly.
    let body = "function tf(float $f): void {}\nfunction h(bool $r) { $b = $r ? 1 : \"1\"; tf($b); }";
    fires(&strict(body), MAYBE_ARG);
    silent(&coercive(body));
}

#[test]
fn the_implicit_null_default_admits_the_null_member() {
    // `function ti(int $i = null)` is `?int` to PHP.
    let body = "function ti(int $i = null): void {}\nfunction h(bool $r) { $b = $r ? null : 1; ti($b); }";
    silent(&strict(body));
    silent(&coercive(body));
}

#[test]
fn the_internal_null_carve_out_admits_the_null_member_only_in_coercive_mode() {
    // PHP 8.5.11: `strlen(null)` is a deprecation coercively and a TypeError strictly.
    let body = "function h(bool $r) { $b = $r ? null : \"a\"; strlen($b); }";
    fires(&strict(body), MAYBE_ARG);
    silent(&coercive(body));
}

#[test]
fn a_oneof_from_a_calls_summary_is_judged_like_a_variable_holding_it() {
    // `r()`'s summary is `null|1`; `ti(r())` and `$v = r(); ti($v)` agree.
    let prelude = "function r(bool $r) { return $r ? null : 1; }\nfunction ti(int $i): void {}";
    for mode in [strict, coercive] {
        let direct = mode(&format!("{prelude}\nfunction h() {{ ti(r(true)); }}"));
        let via = mode(&format!("{prelude}\nfunction h() {{ $v = r(true); ti($v); }}"));
        assert_eq!(ids(&direct), ids(&via), "{direct}");
    }
}

#[test]
fn a_returned_call_with_a_oneof_summary_is_judged_like_its_assignment() {
    let prelude = "function r(bool $r) { return $r ? null : 1; }";
    for mode in [strict, coercive] {
        let returned = mode(&format!("{prelude}\nfunction g(bool $x): int {{ return r($x); }}"));
        let assigned =
            mode(&format!("{prelude}\nfunction g(bool $x): int {{ $v = r($x); return $v; }}"));
        assert_eq!(ids(&returned), ids(&assigned), "{returned}");
    }
}

#[test]
fn a_declared_arm_list_keeps_its_own_table() {
    // A `@param 1|null` seed lives in the declared-arm lane, which this judgment does
    // not read on the argument side and which the return seam has always judged: some
    // rejected rides the contract id, every rejected stays silent. (No source seeds an
    // `Asserted` `OneOf` on the value lane today, so that row of the table is held by
    // the code, not by a test.)
    let some = "/** @param 1|null $x */\nfunction f($x): int { return $x; }";
    fires(&strict(some), PHPDOC_MAYBE_RET);
    let every = "/** @param null|'a' $x */\nfunction f($x): int { return $x; }";
    silent(&strict(every));
    let arg = "function ti(int $i): void {}\n/** @param 1|null $x */\nfunction f($x) { ti($x); }";
    silent(&strict(arg));
    let _ = PHPDOC_MAYBE_ARG;
}

#[test]
fn union_refined_and_general_facts_are_unchanged() {
    // Union, some rejected: the possibly grade (unchanged).
    let some = "function h(int|string $x): int { return $x; }";
    fires(&strict(some), MAYBE_RET);
    // General, all rejected: silent by #291's ruling (unchanged).
    silent(&strict("function h(string $x): int { return $x; }"));
}

// ---------------------------------------------------------------------------
// Table 3: a guard over a OneOf is decided member by member (the #1071 review).
// The table is on `oneof_guard`. PHP 8.5.11 raises no TypeError on any path of the
// "silent" shapes below: every member passes the guard, so the `return $x` is dead.
// ---------------------------------------------------------------------------

const BODY: &str = "$x = $c ? 'a' : 'b';";

fn guarded(guard: &str, rest: &str) -> String {
    strict(&format!("function r(bool $c): int {{ {BODY} if ({guard}) {{ return 1; }} {rest} }}\n"))
}

#[test]
fn a_guard_no_member_decides_forgets_the_set_on_both_sides() {
    // Undecidable for every member, every polarity, every shape the review swept.
    for guard in [
        "ctype_alpha($x)",
        "!ctype_alpha($x)",
        "strlen($x) === 1",
        "!(strlen($x) === 1)",
        "gettype($x) === 'string'",
        "(int) $x === 0",
        "intval($x) === 0",
        "trim($x) === $x",
        "!ctype_digit($x)",
        "strtoupper($x) !== $x",
        "in_array($x, ['a', 'b'], true)",
        "preg_match('/^[ab]$/', $x)",
        "str_contains('ab', $x)",
        "is_string($x)",
    ] {
        silent(&guarded(guard, "return $x;"));
    }
}

#[test]
fn a_guard_no_member_decides_forgets_the_set_before_an_argument_too() {
    let src = strict(
        "function ti(int $i): int { return $i; }\nfunction a(bool $c): int { $x = $c ? 'a' : 'b'; if (!ctype_alpha($x)) { return ti($x); } return 1; }\n",
    );
    silent(&src);
    let src = strict(
        "function ti(int $i): int { return $i; }\nfunction a(bool $c): int { $x = $c ? 'a' : 5; if (intval($x) !== 0) { return ti($x); } return 1; }\n",
    );
    silent(&src);
}

#[test]
fn an_undecidable_guard_with_a_rejected_member_stays_silent() {
    // `'a'|5` into `: int`: the `'a'` arm is rejected, but whether it reaches the
    // fall-through is the guard's to say, and it cannot be decided.
    let src = strict("function r(bool $c): int { $x = $c ? 'a' : 5; if (ctype_alpha($x)) { return 1; } return $x; }\n");
    silent(&src);
    let src = strict("function r(bool $c): int { $x = $c ? 'a' : 5; if (gettype($x) === 'string') { return 1; } return $x; }\n");
    silent(&src);
}

#[test]
fn an_undecidable_guard_that_every_side_survives_restores_the_set() {
    // Both sides fall through, so the code after the join runs whichever side PHP took
    // and every member reaches it: the definite verdict is valid there.
    let src = strict("function r(bool $c): int { $x = $c ? 'a' : 'b'; if (ctype_alpha($x)) { echo 1; } return $x; }\n");
    fires(&src, RET);
}

#[test]
fn a_guard_every_member_passes_prunes_the_side_no_member_reaches() {
    // `'a'` and `'b'` are both non-empty: the else side is dead.
    silent(&guarded("$x !== ''", "return $x;"));
    silent(&guarded("$x === 'a' || $x === 'b'", "return $x;"));
    // Every member fails: the then side is dead, and the code after reads the set.
    let src = strict("function r(bool $c): int { $x = $c ? 'a' : 'b'; if ($x === '') { return $x; } return 1; }\n");
    silent(&src);
}

#[test]
fn a_guard_that_splits_the_members_narrows_each_side_to_its_own() {
    // `$x === 'a'`: the then side holds `'a'`, the else side `'b'`; each is a
    // `Singleton` and reports as one.
    let then = strict("function r(bool $c): int { $x = $c ? 'a' : 'b'; if ($x === 'a') { return $x; } return 1; }\n");
    let d = findings(&then);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].id, RET);
    assert!(d[0].message.contains("\"a\""), "{}", d[0].message);
    let other = strict("function r(bool $c): int { $x = $c ? 'a' : 'b'; if ($x === 'a') { return 1; } return $x; }\n");
    let d = findings(&other);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("\"b\""), "{}", d[0].message);
}

#[test]
fn a_guard_reading_two_oneof_variables_forgets_both() {
    let src = strict(
        "function r(bool $c): int { $x = $c ? 'a' : 'b'; $y = $c ? 'a' : 'b'; if ($x === $y) { return 1; } return $x; }\n",
    );
    silent(&src);
}

#[test]
fn a_loop_header_over_a_oneof_forgets_it_in_the_body() {
    let src = strict(
        "function r(bool $c): int { $x = $c ? 'a' : 'b'; while (ctype_alpha($x)) { return $x; } return 1; }\n",
    );
    silent(&src);
}

// ---------------------------------------------------------------------------
// The `$return` flag narrows a builtin's floor only by what PHP never answers.
// ---------------------------------------------------------------------------

#[test]
fn highlight_file_keeps_its_false_arm_when_asked_for_a_string() {
    // PHP 8.5.11: `highlight_file('/nonexistent', true)` is `false`.
    let src = strict("function f(string $p): string { return highlight_file($p, true); }\n");
    fires(&src, PHPDOC_MAYBE_RET);
    let src = strict("function f(string $p): string { $x = highlight_file($p, true); return $x; }\n");
    fires(&src, PHPDOC_MAYBE_RET);
    // `highlight_string` answers a string even for garbage input.
    let src = strict("function f(string $p): string { return highlight_string($p, true); }\n");
    silent(&src);
}

// ---------------------------------------------------------------------------
// Table 4: the path rule (the #1071 re-review). A guard on a copy, a derived boolean or
// an array holding the set cannot be decided per member, so "every member refused" is
// not a proof after the branch with an exit: the set is thinned and the verdict drops to
// the possibly grade. PHP 8.5.11 raises no TypeError on any path of these shapes.
// ---------------------------------------------------------------------------

fn demoted(src: &str, maybe: &str) {
    let got = ids(src);
    assert_eq!(got, vec![maybe.to_owned()], "expected only {maybe} for:\n{src}");
}

fn one_fn(body: &str) -> String {
    strict(&format!(
        "function ti(int $i): int {{ return $i; }}\nfunction r(bool $c): int {{ $x = $c ? 'a' : 'b'; {body} }}\n"
    ))
}

#[test]
fn a_guard_on_a_copy_of_the_set_demotes_the_definite_verdict() {
    // d1, d2, d7: `$y = $x` binds nothing the walk can decide a guard on.
    demoted(&one_fn("$y = $x; if (ctype_alpha($y)) { return 1; } return $x;"), MAYBE_RET);
    demoted(&one_fn("$y = $x; if (strlen($y) === 1) { return 1; } return $x;"), MAYBE_RET);
    demoted(&one_fn("$y = $x; if ($y === 'a' || $y === 'b') { return 1; } return $x;"), MAYBE_RET);
}

#[test]
fn a_guard_on_a_derived_value_demotes_the_definite_verdict() {
    // d3, d9: a boolean computed from the set; d8: an array holding it.
    demoted(&one_fn("$ok = ctype_alpha($x); if ($ok) { return 1; } return $x;"), MAYBE_RET);
    demoted(&one_fn("$ok = $x === 'a' || $x === 'b'; if ($ok) { return 1; } return $x;"), MAYBE_RET);
    demoted(&one_fn("$arr = [$x]; if (ctype_alpha($arr[0])) { return 1; } return $x;"), MAYBE_RET);
}

#[test]
fn the_argument_side_demotes_the_same_way() {
    // d6.
    demoted(&one_fn("$y = $x; if (ctype_alpha($y)) { return 1; } return ti($x);"), MAYBE_ARG);
}

#[test]
fn an_unrelated_branch_with_an_exit_thins_the_set_too() {
    // The walk cannot say the exit is uncorrelated with the set.
    demoted(&one_fn("if (rand() === 1) { return 2; } return $x;"), MAYBE_RET);
    demoted(&one_fn("foreach ([1] as $_) { if (rand() === 1) { break; } } return $x;"), MAYBE_RET);
    demoted(&one_fn("try { echo 1; } catch (Exception $e) { return 2; } return $x;"), MAYBE_RET);
}

#[test]
fn a_branch_that_exits_through_a_decided_guard_keeps_the_definite_verdict() {
    // Three members, `$x === 'a'` decided per member: the fall-through holds `'b'|'c'`,
    // exactly, and both are refused.
    let src = strict(
        "function r(bool $c, bool $d): int { if ($c) { $x = 'a'; } elseif ($d) { $x = 'b'; } else { $x = 'c'; } if ($x === 'a') { return 1; } return $x; }\n",
    );
    fires(&src, RET);
    // A branch with no exit does not thin.
    fires(&one_fn("if (rand() === 1) { echo 2; } return $x;"), RET);
    // A rebinding after the exit starts a new epoch.
    fires(&one_fn("if (rand() === 1) { return 2; } $x = $c ? 'c' : 'd'; return $x;"), RET);
}

#[test]
fn a_ternary_whose_arm_is_a_set_its_condition_reads_is_unknown() {
    // og01 t4: `$r` would hold `1|'a'|'b'`, with `'a'` and `'b'` unreachable.
    silent(&one_fn("$r = ctype_alpha($x) ? 1 : $x; return $r;"));
    silent(&one_fn("$r = !ctype_alpha($x) ? $x : 1; return $r;"));
    // A ternary that does not read the set in its condition keeps the arm's members.
    demoted(
        &one_fn("$r = rand() === 1 ? 1 : $x; return $r;"),
        MAYBE_RET,
    );
}
