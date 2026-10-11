//! A call is judged at every position it is written (ADR-0104, issue #935): the rule
//! table on `stmt_calls.rs`, one case per row.
//!
//! Each fixture is a function body over a small prelude; a finding is named by the
//! id and the source text it points at, so the cases also pin the diagnostic column
//! (D1: the call's own span, never the statement's). Every firing row is a fatal on
//! PHP 8.5.11 (`Error` or `ArgumentCountError`), witnessed with a handler installed.

use steins_infer::{Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A boot surface that has only the guard builtins: every other function and class is
/// absent, so an undefined name is a proof.
struct Boot;

impl Folder for Boot {
    fn fold(
        &mut self,
        _n: &str,
        _a: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_function(&mut self, name: &str) -> Option<bool> {
        Some(
            [
                "defined",
                "function_exists",
                "method_exists",
                "is_callable",
                "property_exists",
                "sprintf",
                "preg_match",
            ]
            .contains(&name),
        )
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

const PRELUDE: &str = "<?php
class E extends Exception { public function __construct(string $a, int $b) {} }
class C {
    public function ok(): int { return 1; }
    private function hidden(): int { return 1; }
    public static function make(int $a, int $b): int { return $a; }
}
function id(mixed $x): mixed { return $x; }
function two(int $a, int $b): int { return $a + $b; }
";

/// Every finding of `body` as `"<id> @ <source text at its position>"`.
fn found(body: &str) -> Vec<String> {
    let src = format!("{PRELUDE}function t(bool $a, ?int $n) {{ $c = new C(); {body} }}\n");
    let tree = SourceTree::parse(&src);
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot);
    out.retain(|d| d.id.starts_with("call."));
    out.sort_by_key(|d| (d.line, d.column));
    out.iter()
        .map(|d| {
            let line = lines[d.line as usize - 1];
            let text: String = line.chars().skip(d.column as usize - 1).take(14).collect();
            format!("{} @ {}", d.id, text)
        })
        .collect()
}

macro_rules! row {
    ($name:ident, $body:expr, [$($expect:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            let expect: Vec<&str> = vec![$($expect),*];
            let got = found($body);
            assert_eq!(got.len(), expect.len(), "{got:#?}");
            for (g, e) in got.iter().zip(&expect) {
                assert!(g.starts_with(e), "got {g:?}, wanted a prefix of {e:?}\n{got:#?}");
            }
        }
    };
}

const UNDEF_FN: &str = "call.undefined-function @ nope_fn()";
const UNDEF_REF: &str = "call.undefined-function @ nope_fn(...)";
const UNDEF_M: &str = "call.undefined-method @ $c->gone()";
const FEW: &str = "call.too-few-arguments @ two(1)";

// ---- Plain: the statement's own call is judged once (dedupe by span) ----

row!(own_call_of_an_echo_reports_once, "echo nope_fn();", [UNDEF_FN]);
row!(own_call_of_a_statement_reports_once, "nope_fn();", [UNDEF_FN]);
row!(own_call_of_a_return_reports_once, "return nope_fn();", [UNDEF_FN]);
row!(own_call_of_an_assignment_reports_once, "$x = nope_fn();", [UNDEF_FN]);

// ---- Plain: every operand position that is not the statement's own call ----

row!(operand_of_arithmetic, "return nope_fn() + 1;", [UNDEF_FN]);
row!(operand_of_concatenation, "return 'a' . nope_fn();", [UNDEF_FN]);
row!(operand_of_a_cast, "return (string) nope_fn();", [UNDEF_FN]);
row!(array_element, "return [nope_fn()];", [UNDEF_FN]);
row!(array_key, "return [nope_fn() => 1];", [UNDEF_FN]);
row!(condition_of_a_ternary, "return nope_fn() ? 1 : 2;", [UNDEF_FN]);
row!(left_of_coalesce, "return nope_fn() ?? 1;", [UNDEF_FN]);
row!(left_of_instanceof, "return nope_fn() instanceof C;", [UNDEF_FN]);
row!(operand_of_exit, "exit(nope_fn());", [UNDEF_FN]);
row!(operand_of_throw, "throw nope_fn();", [UNDEF_FN]);
row!(operand_of_print, "print(nope_fn());", [UNDEF_FN]);
row!(operand_of_print_without_parentheses, "print nope_fn();", [UNDEF_FN]);
row!(second_operand_of_echo, "echo 1, nope_fn() . 'x';", [UNDEF_FN]);
row!(interpolation, "echo \"x {$c->gone()}\";", [UNDEF_M]);
row!(static_initializer, "static $s = nope_fn();", [UNDEF_FN]);
row!(
    only_the_nested_call_of_a_known_function_reports,
    "echo id(nope_fn());",
    [UNDEF_FN]
);

// ---- Argument positions: positional, named, spread, constructor ----

row!(positional_argument_of_a_project_function, "id(nope_fn());", [UNDEF_FN]);
row!(positional_argument_of_a_builtin, "echo strlen(nope_fn());", [UNDEF_FN]);
row!(named_argument, "echo two(b: nope_fn(), a: 1);", [UNDEF_FN]);
row!(element_of_a_literal_spread, "echo strlen(...[nope_fn()]);", [UNDEF_FN]);
row!(argument_of_a_new, "$o = new E(nope_fn(), 2);", [UNDEF_FN]);
row!(argument_of_a_new_in_a_throw, "throw new E(nope_fn(), 2);", [UNDEF_FN]);
row!(argument_of_a_static_call, "echo C::make(nope_fn(), 2);", [UNDEF_FN]);
row!(argument_of_a_method_call, "echo $c->ok(nope_fn());", [UNDEF_FN]);

// ---- Arity (E) at a nested position ----

row!(nested_arity_in_an_argument, "echo id(two(1));", [FEW]);
row!(nested_arity_in_an_operand, "return two(1) + 1;", [FEW]);
row!(nested_arity_of_a_constructor_in_a_throw, "throw new E('a');", ["call.too-few-arguments @ new E('a')"]);
row!(nested_arity_of_a_static_call, "echo id(C::make(1));", ["call.too-few-arguments @ C::make(1)"]);

// ---- Methods (E) at a nested position ----

row!(undefined_method_in_an_argument, "echo strlen($c->gone());", [UNDEF_M]);
row!(undefined_method_in_an_array, "return [$c->gone()];", [UNDEF_M]);
row!(undefined_method_on_a_new_receiver, "return (new C)->gone() + 1;", ["call.undefined-method @ (new C)->gone"]);
row!(undefined_static_method_in_an_argument, "echo id(C::gone());", ["call.undefined-method @ C::gone()"]);

// ---- Conditional: E judges, V declines ----

row!(right_of_and_is_judged, "return $a && nope_fn();", [UNDEF_FN]);
row!(right_of_or_is_judged, "return $a || nope_fn();", [UNDEF_FN]);
row!(arm_of_a_ternary_is_judged, "return $a ? nope_fn() : 1;", [UNDEF_FN]);
row!(else_arm_of_a_short_ternary_is_judged, "return $a ?: nope_fn();", [UNDEF_FN]);
row!(right_of_coalesce_is_judged, "return $n ?? nope_fn();", [UNDEF_FN]);
row!(arity_at_a_conditional_position, "return $a && two(1);", [FEW]);
row!(method_existence_at_a_conditional_position, "return $a && $c->gone();", [UNDEF_M]);
row!(
    value_family_declines_at_a_conditional_position,
    "$k = null; return $a && $k->m();",
    []
);
row!(
    value_family_fires_at_a_plain_position,
    "$k = null; return 1 + $k->m();",
    ["call.on-null @ $k->m()"]
);
row!(
    value_family_fires_at_an_argument_position,
    "$k = null; echo strlen($k->m());",
    ["call.on-null @ $k->m()"]
);
row!(
    inaccessible_method_declines_at_a_conditional_position,
    "return $a && $c->hidden();",
    []
);
row!(
    inaccessible_method_fires_at_a_plain_position,
    "return 1 + $c->hidden();",
    ["call.inaccessible-method @ $c->hidden"]
);

// ---- Guards: a decided guard makes the arm dead; an undecided one is not a vouch ----

row!(
    function_exists_makes_the_right_operand_dead,
    "return function_exists('nope_fn') && nope_fn();",
    []
);
row!(
    function_exists_makes_the_arm_dead,
    "return function_exists('nope_fn') ? nope_fn() : 0;",
    []
);
row!(
    negated_function_exists_keeps_the_other_arm_live,
    "return function_exists('nope_fn') ? 0 : nope_fn();",
    [UNDEF_FN]
);
row!(
    method_exists_in_the_same_expression_declines_undefined_method,
    "return method_exists($c, 'gone') && $c->gone();",
    []
);
row!(
    method_exists_in_the_same_expression_still_lets_a_function_report,
    "return method_exists($c, 'gone') && nope_fn();",
    [UNDEF_FN]
);

// ---- Suppressed: silence at the default floor (D2) ----

row!(error_control_operand_is_silent, "echo @nope_fn();", []);
row!(error_control_statement_is_silent, "@nope_fn();", []);
row!(error_control_over_an_argument_is_silent, "echo @strlen(nope_fn());", []);

// ---- Stale entry facts: a name the statement may rebind is not read ----

row!(
    receiver_rebound_in_the_same_statement_is_not_judged,
    "$k = null; echo id($k = new C()) . $k->m();",
    []
);
row!(
    receiver_untouched_by_the_statement_is_judged,
    "$k = null; echo id(1) . $k->m();",
    ["call.on-null @ $k->m()"]
);

// ---- First-class callables: existence only ----

row!(function_callable_is_an_existence_site, "$f = nope_fn(...);", [UNDEF_REF]);
row!(method_callable_is_an_existence_site, "$f = $c->gone(...);", ["call.undefined-method @ $c->gone(...)"]);
row!(static_callable_is_an_existence_site, "$f = C::gone(...);", ["call.undefined-method @ C::gone(...)"]);
row!(callable_in_an_argument_is_judged, "echo id(nope_fn(...));", [UNDEF_REF]);
row!(callable_of_an_existing_function_is_silent, "$f = two(...);", []);
row!(callable_of_an_existing_method_is_silent, "$f = $c->ok(...);", []);
row!(
    callable_of_a_vouched_function_is_silent,
    "if (function_exists('nope_fn')) { $f = nope_fn(...); }",
    []
);
row!(callable_under_error_control_is_silent, "$f = @nope_fn(...);", []);
row!(
    callable_at_a_conditional_position_is_judged,
    "return $a && nope_fn(...);",
    [UNDEF_REF]
);
row!(
    callable_declines_method_existence_beside_a_member_test,
    "return method_exists($c, 'gone') && $c->gone(...);",
    []
);

// ---- A first-class callable is not a call: no consumer but existence sees it ----

row!(callable_has_no_arity, "$f = two(...);", []);
row!(callable_of_a_static_method_has_no_arity, "$f = C::make(...);", []);
row!(
    callable_is_not_a_dump,
    "$f = \\PHPStan\\dumpType(...);",
    []
);
row!(
    callable_statement_is_not_a_discarded_call,
    "$c->ok(...);",
    []
);

// ---- Boundaries: own scopes and constructs the scan does not enter ----

row!(closure_body_is_its_own_scope_not_a_position, "$f = function () { return 1; }; echo $f();", []);
row!(
    arrow_function_body_is_judged_by_its_own_trace,
    "$f = fn () => nope_fn();",
    [UNDEF_FN]
);
row!(
    call_in_a_hoisted_match_arm_is_judged_by_the_arm,
    "return match ($n) { 1 => nope_fn(), default => 0 };",
    [UNDEF_FN]
);

// ---- A guard assigned before it is used as a connective's operand (review of #1066) ----

row!(assigned_guard_on_the_left_of_and, "$k = function_exists('nope_fn') and nope_fn();", []);
row!(assigned_negated_guard_on_the_left_of_or, "$k = !function_exists('nope_fn') or nope_fn();", []);
row!(assigned_guard_into_an_append, "$r = []; $r[] = function_exists('nope_fn') and nope_fn();", []);
row!(
    assigned_guard_as_a_ternary_condition,
    "return ($k = function_exists('nope_fn')) ? nope_fn() : 0;",
    []
);
row!(
    assigned_guard_as_a_conjunct_still_judges_the_other_arm,
    "return ($k = function_exists('nope_fn')) ? 0 : nope_fn();",
    [UNDEF_FN]
);
row!(assignment_of_a_non_guard_declines_nothing, "$k = $a and nope_fn();", [UNDEF_FN]);

// ---- A guard stored in a variable and tested through it ----

row!(
    stored_guard_in_a_ternary,
    "$has = function_exists('nope_fn'); return $has ? strlen(nope_fn()) : 0;",
    []
);
row!(
    stored_guard_in_an_if,
    "$has = function_exists('nope_fn'); if ($has) { echo strlen(nope_fn()); }",
    []
);
row!(
    stored_guard_in_an_early_return,
    "$has = function_exists('nope_fn'); if (!$has) { return 0; } return strlen(nope_fn());",
    []
);
row!(
    stored_guard_in_a_conjunction,
    "$has = function_exists('nope_fn'); return $has && nope_fn();",
    []
);
row!(
    stored_guard_does_not_cover_a_call_outside_a_test,
    "$has = function_exists('nope_fn'); return nope_fn();",
    [UNDEF_FN]
);
row!(
    stored_guard_covers_only_its_own_name,
    "$has = function_exists('sprintf'); return $has ? nope_fn() : 0;",
    [UNDEF_FN]
);
row!(
    stored_guard_dies_on_reassignment,
    "$has = function_exists('nope_fn'); $has = $a; return $has ? nope_fn() : 0;",
    [UNDEF_FN]
);
row!(
    stored_guard_dies_on_a_rebind_in_the_same_statement,
    "$has = function_exists('nope_fn'); return ($has = $a) ? nope_fn() : 0;",
    [UNDEF_FN]
);
row!(
    stored_method_guard_dies_when_its_receiver_is_rebound,
    "$has = method_exists($c, 'gone'); $c = new C(); return $has ? $c->gone() : 0;",
    [UNDEF_M]
);

// ---- A literal that is not null decides `??` ----

row!(non_null_literal_on_the_left_of_coalesce_kills_the_right, "return 1 ?? nope_fn();", []);
row!(string_literal_on_the_left_of_coalesce_kills_the_right, "return 'x' ?? nope_fn();", []);
row!(false_on_the_left_of_coalesce_kills_the_right, "return false ?? nope_fn();", []);
row!(null_on_the_left_of_coalesce_keeps_the_right, "return null ?? nope_fn();", [UNDEF_FN]);
