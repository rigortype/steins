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
