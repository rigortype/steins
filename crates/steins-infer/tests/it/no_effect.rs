//! Acceptance tests for `statement.no-effect` (ADR-0096, issue #320): a
//! statement-position call whose result is unused and whose callee, at this
//! call site, is proven to do nothing observable.
//!
//! The conjuncts of ADR-0096 §3 each get their own refutation here, and each
//! refutation is the deletion test for its leg: remove the check and the named
//! case starts reporting. The second half of the file rebuilds the probes of
//! the 2026-09-12 adversarial review, every one of which reported before the
//! argument bar existed.

use std::collections::HashMap;

use steins_infer::{Diagnostic, Folder, ID, STATEMENT_NO_EFFECT_ID, check, check_with};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, SourceTree};

/// Parse + check inline PHP, returning only the discarded-call findings.
fn dead(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == STATEMENT_NO_EFFECT_ID)
        .collect()
}

fn one(src: &str) -> Diagnostic {
    let f = dead(src);
    assert_eq!(f.len(), 1, "expected exactly one discarded-call finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

/// `call;` as the one statement of a function body, with the declared
/// parameters the probe needs in scope.
fn body(params: &str, call: &str) -> String {
    format!("<?php\nfunction f({params}): void {{ {call}; }}\n")
}

fn reports(params: &str, call: &str, why: &str) {
    assert_eq!(dead(&body(params, call)).len(), 1, "`{call};` must report: {why}");
}

fn silent(params: &str, call: &str, why: &str) {
    let f = dead(&body(params, call));
    assert!(f.is_empty(), "`{call};` must be silent ({why}), got: {f:#?}");
}

// ---------------------------------------------------------------------------
// What reports, and why each is provable.
// ---------------------------------------------------------------------------

#[test]
fn a_pure_builtin_over_literal_arguments_is_a_dead_statement() {
    let d = one(&body("", "strlen('x')"));
    assert_eq!(d.id, STATEMENT_NO_EFFECT_ID);
    assert_eq!(
        d.message,
        "`strlen()` has no effect anything can observe and its result is unused — the statement does nothing"
    );
    assert_eq!(d.line, 2);
}

#[test]
fn the_fold_allowlist_calibration_holds_under_exactly_typed_literals() {
    // Each of these is catalogued pure BECAUSE it folds, and it folds because
    // it is pure given literal arguments — so the literal-argument call is the
    // one the calibration was made for. Every argument is exactly of its
    // parameter's declared type, so no coercion and no deprecation is left.
    for call in [
        "strlen('x')",
        "substr('abc', 1)",
        "strtolower('A')",
        "in_array(1, [1, 2])",
        "array_merge([1], [2])",
        "gettype(null)",
        "floor(1.5)",
        "abs(-1)",
    ] {
        reports("", call, "a pure builtin over literals it accepts");
    }
}

#[test]
fn an_impure_but_discardable_builtin_is_a_dead_statement() {
    // The case the curated `hasSideEffects` boolean structurally could not
    // reach: `rand` is impure and never foldable, and discarding its result is
    // still dead code. These carry an explicit colour of their own, so the bar
    // is met by the row and by the (empty or literal) argument list together.
    for call in ["time()", "rand()", "rand(1, 10)", "microtime(true)", "uniqid()", "getenv('HOME')"] {
        reports("", call, "reads ambient state and hands it back");
    }
}

#[test]
fn a_call_whose_result_is_used_is_not_a_discarded_call() {
    // The family is about the STATEMENT, so every non-statement position is
    // silent: the same call assigned, returned, or nested in an argument.
    for body in ["$n = strlen('x');", "echo strlen('x');", "return strlen('x');"] {
        let src = format!("<?php\nfunction f(): mixed {{ {body} }}\n");
        assert_eq!(dead(&src).len(), 0, "`{body}` uses the result");
    }
}

// ---------------------------------------------------------------------------
// ADR-0096 §3 leg by leg: the callee side.
// ---------------------------------------------------------------------------

#[test]
fn a_statement_position_method_call_is_out_of_scope() {
    // ADR-0096 §5: the slice judges named functions. `$repo->save($x);` is
    // silent whatever `save` does, and so is the pure-getter shape — the
    // receiver is a variable, which draws no edge in the effect graph at all.
    let src = "<?php\nclass Repo { public function save(int $x): int { return $x; } }\nfunction f(Repo $repo, int $x): void { $repo->save($x); }\n";
    assert_eq!(dead(src).len(), 0, "a method receiver is out of scope here");
}

#[test]
fn an_effect_outside_the_discardable_set_is_silence() {
    for (call, why) in [
        ("fgets($h)", "io.input: consuming a stream advances its position"),
        ("sort($rows)", "mutate.local: the caller-visible mutation is the point"),
        ("print_r([1])", "io.output.buffer: it writes"),
        ("file_put_contents('/tmp/x', 'y')", "io: a write"),
        ("header('X: 1')", "io.output.header"),
    ] {
        silent("$h, array $rows", call, why);
    }
}

#[test]
fn a_builtin_that_writes_through_an_argument_is_silence() {
    // `shuffle` is the name where the two catalog axes both fire: its colour
    // (`nondet.random`) is discardable and its by-ref out-parameter row is the
    // whole reason the call was written. The out-parameter row decides.
    silent("array $rows", "shuffle($rows)", "the call writes through its argument");
}

#[test]
fn an_uncatalogued_builtin_is_silence() {
    // No colour at all is the builtin world's `…?`: nothing is known about what
    // `ob_start` does to the output stack, so nothing is claimed.
    silent("", "ob_start()", "an uncatalogued name proves nothing");
}

#[test]
fn a_builtin_with_a_throw_row_is_a_validity_check_whatever_it_is_passed() {
    // The throw row is per NAME. `random_int` and `intdiv` carried theirs from
    // the start; the review's eight `ValueError` names carry one now — and so
    // `str_repeat('a', 3);` is silent although that particular call cannot
    // throw. The seam that could re-derive the row per call (the fold) cannot
    // tell a value from a value-with-a-deprecation, so the row is read as
    // written (ADR-0096 §3).
    for call in ["random_int(1, 10)", "intdiv(10, 2)", "str_repeat('a', 3)", "count([1])", "explode(',', 'a,b')", "mt_rand(1, 2)"] {
        silent("", call, "the name can throw, so the statement can be the check");
    }
}

#[test]
fn a_name_whose_literal_call_can_still_diagnose_is_refused() {
    // The refusal list: catalogued pure, argument bar met, and yet a literal
    // call can raise a warning or a deprecation, or write an engine slot
    // (`json_last_error`). The runner cannot see a diagnostic, so nothing here
    // can prove its absence.
    for call in ["json_decode('{}')", "json_encode('x')", "trim('a')", "bindec('1')", "preg_split('/,/', 'a,b')", "idate('Y')", "strtr('a', 'a', 'b')"] {
        silent("", call, "a literal call can diagnose or write engine state");
    }
}

// ---------------------------------------------------------------------------
// ADR-0096 §3: the argument bar.
// ---------------------------------------------------------------------------

#[test]
fn a_non_literal_argument_is_silence() {
    // The fold allowlist calibrates "pure given literal arguments"; a variable,
    // a call, a constant fetch or a concatenation is not one, whatever the
    // parameter type, because what the value can DO is not known (a
    // `__toString`, a `Countable`, an `ArrayAccess`).
    for call in ["strlen($s)", "strlen(g())", "strlen(PHP_EOL)", "strlen('a' . $s)", "getenv($s)", "date($s)"] {
        silent("string $s", call, "not a literal");
    }
}

#[test]
fn a_literal_of_the_wrong_type_is_silence_in_both_modes() {
    // Exact typing, strict-mode acceptance: an `int` into `string` coerces, a
    // fraction into `int` deprecates, `null` into a non-nullable parameter is a
    // `TypeError` under strict_types and a deprecation without — and each of
    // those is something the statement observably does.
    for call in ["strlen(1)", "strlen(null)", "str_replace('a', 'b', 1)", "rand(1.5, 2)", "array_merge([], 'string')"] {
        silent("", call, "a coercion or a TypeError, not a no-op");
        let strict = format!("<?php\ndeclare(strict_types=1);\nfunction f(): void {{ {call}; }}\n");
        assert!(dead(&strict).is_empty(), "`{call};` under strict_types must be silent");
    }
}

#[test]
fn a_null_literal_is_admitted_only_where_the_parameter_is_nullable() {
    reports("", "getenv(null)", "`?string $name` admits null exactly");
    silent("", "strlen(null)", "`string $string` does not");
}

#[test]
fn a_nested_array_literal_is_silence() {
    // `implode(',', [[1]])` renders an element to a string and warns; the bar
    // refuses a nested array for every name rather than know which render.
    silent("", "implode(',', [[1]])", "an element rendered to a string warns");
    silent("", "array_merge([[1]], [[2]])", "refused whole, the silence direction");
}

#[test]
fn an_arity_the_engine_refuses_is_silence() {
    // Too few or too many arguments is an `ArgumentCountError` at the boundary;
    // the engine's own parameter counts decide, and a variadic tail admits any
    // count from its position on.
    for call in ["strlen()", "strlen('a', 'b')", "time(1)"] {
        silent("", call, "ArgumentCountError");
    }
    reports("", "array_merge()", "a variadic-only list admits zero arguments");
    reports("", "array_merge([1], [2], [3])", "and any count past it");
}

#[test]
fn an_arity_inside_the_interval_but_outside_the_set_is_silence() {
    // `rand()` takes exactly zero or exactly two arguments, and reflection can
    // only spell that as "0 required of 2" — so `rand(1)` is inside the
    // engine's interval, an `ArgumentCountError` raised from inside the call,
    // and silent on the arity checker too. The explicit set in the rule is
    // what answers it (second review, 2026-09-12).
    silent("", "rand(1)", "ArgumentCountError from inside the call");
    reports("", "rand()", "zero is in the set");
    reports("", "rand(1, 10)", "and so is two");
}

#[test]
fn an_array_literal_is_admitted_only_at_an_array_typed_slot() {
    // `strval([1])` is an "Array to string conversion" warning: `mixed` is a
    // slot the engine does not check, so what the builtin does with an array
    // there is its own business. The bar admits an array literal only where
    // the declaration names `array` or `iterable`; the rule is uniform, so
    // the names that take an array without a word are silent with it.
    silent("", "strval([1])", "an array rendered to a string warns");
    for call in ["intval([1])", "gettype([1])", "boolval([1])"] {
        silent("", call, "an array at a mixed slot, the uniform rule");
    }
    reports("", "in_array(1, [1, 2])", "`array $haystack` names the type");
    reports("", "array_merge([1], [2])", "and so does a variadic `array`");
}

#[test]
fn a_named_spread_or_first_class_argument_is_silence() {
    // Each shape leaves a positional prefix the arity leg would ACCEPT on its
    // own (`rand` and `time` need nothing, `implode` needs one), so it is the
    // shape rule that answers, not the count.
    for call in ["rand(1, max: 10)", "implode(',', ...$a)", "time(...)"] {
        silent("array $a", call, "not a positional literal list");
    }
    // A spread of an array LITERAL is flattened into a proven positional list
    // at lowering (issue #616), so `strlen(...['x'])` is `strlen('x')` to every
    // reader of `args` — and the claim is true of it.
    reports("", "strlen(...['x'])", "a literal spread is a proven positional list");
}

#[test]
fn a_by_ref_position_declines_the_name_even_when_it_is_not_passed() {
    // `str_replace` folds and is pure over these literals, but its `&$count`
    // row declines the whole name: the out-parameter rows are read per name,
    // as the throw rows are, and this call not passing the position is a
    // refinement the slice does not make.
    silent("", "str_replace('a', 'b', 'abc')", "an out-parameter row on the name");
}

#[test]
fn a_callable_position_is_silence_even_under_a_literal() {
    // A string literal at a `callable` position NAMES user code, and the
    // catalog's pure colour on `array_filter` is for the callback-less call.
    // Both the declared-callable position and the exact typing refuse it.
    silent("", "array_filter([1], 'unlink')", "the callback runs user code");
    silent("", "array_filter([1], 'strlen')", "a builtin callback is still a callback");
}

#[test]
fn an_error_suppressed_call_is_not_a_statement_position_call() {
    // `@strlen('x');` lowers to no `StmtKind::Call` at all — the operator is a
    // request about diagnostics, which is exactly the thing this family cannot
    // see — so it is silence by the lowering, pinned here so it stays so.
    silent("", "@strlen('x')", "under `@`");
}

#[test]
fn a_proven_throw_on_the_same_call_silences_the_no_op_claim() {
    // The co-fire rule. The argument checker's builtin arm reads the sidecar's
    // reflected parameter list; here that list disagrees with the mined table
    // (`strlen(int)`), so the checker proves `strlen('x')` a `TypeError` while
    // the argument bar, reading the table, admits it. The proof wins: a
    // statement that provably throws is a validity check, never dead code too.
    struct Disagreeing;
    impl Folder for Disagreeing {
        fn fold(&mut self, _: &str, _: &[ArgValue], _: bool) -> Option<ArgValue> {
            None
        }
        fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
            (name == "strlen").then(|| {
                vec![BuiltinParam {
                    name: "string".to_owned(),
                    ty: Some("int".to_owned()),
                    by_ref: false,
                    variadic: false,
                    optional: false,
                }]
            })
        }
    }
    let src = "<?php\ndeclare(strict_types=1);\nfunction f(): void { strlen('x'); }\n";
    let tree = SourceTree::parse(src);
    let ids: HashMap<&str, usize> =
        check_with(&tree, &[], "t.php", &mut Disagreeing).iter().fold(HashMap::new(), |mut m, d| {
            *m.entry(d.id).or_default() += 1;
            m
        });
    assert_eq!(ids.get(ID), Some(&1), "the TypeError is proven: {ids:?}");
    assert_eq!(ids.get(STATEMENT_NO_EFFECT_ID), None, "and the no-op claim yields to it: {ids:?}");
}

// ---------------------------------------------------------------------------
// The review's probes (2026-09-12), each of which reported before the bar.
// ---------------------------------------------------------------------------

#[test]
fn review_probe_a_callback_argument_runs_user_code() {
    silent("array $paths", "array_filter($paths, 'unlink')", "the callback unlinks");
    silent("array $paths", "array_filter($paths, fn($p) => unlink($p))", "the arrow function unlinks");
}

#[test]
fn review_probe_json_encode_throws_under_a_flag_and_serializes_user_code() {
    silent("array $m", "json_encode($m, JSON_THROW_ON_ERROR)", "JsonException under the flag");
    silent("\\JsonSerializable $j", "json_encode($j)", "jsonSerialize() runs");
    silent("", "json_encode(\"\\xff\", 4194304)", "a literal flag bit throws");
}

#[test]
fn review_probe_the_value_error_names_with_non_literal_arguments() {
    for call in [
        "str_repeat($s, $n)",
        "array_fill(0, $n, 1)",
        "range(1, 10, $n)",
        "str_pad($s, 10, '')",
        "str_split($s, $n)",
        "explode('', $s)",
        "str_increment($s)",
        "round(1.5, 0, $n)",
    ] {
        silent("string $s, int $n", call, "a ValueError arm the catalog now carries");
    }
}

#[test]
fn review_probe_the_shape_dependent_type_errors_the_types_cannot_show() {
    // Second review (2026-09-12): the exact-typing bar admits each of these,
    // and each is a `TypeError` on PHP 8.5.10. `implode`'s legacy signature
    // (`array|string $separator, ?array $array`) couples the shapes it accepts
    // across the two positions, and `substr_replace`'s array `$offset` and
    // `$length` arms are for the array-subject shape. The refusal is per name,
    // so the well-formed `implode(',', [1, 2]);` is silent with them.
    for call in [
        "implode('a')",
        "implode('x', null)",
        "implode([1], [2])",
        "join('a')",
        "join('x', null)",
        "join([1], [2])",
        "implode(',', [1, 2])",
        "substr_replace('abc', 'x', [1])",
        "substr_replace('abc', 'x', 0, [1])",
    ] {
        silent("", call, "a TypeError the declared parameter types cannot show");
    }
}

#[test]
fn review_probe_an_object_argument_can_run_user_code() {
    let src = "<?php\nclass S { public function __toString(): string { return 'x'; } }\nclass C implements \\Countable { public function count(): int { return 1; } }\nfunction f(S $o, C $c, array $arr, $x): void { strlen($o); count($c); implode(',', $arr); in_array($x, $arr); strval($o); }\n";
    assert_eq!(dead(src).len(), 0, "`__toString`, `Countable::count` and an unknown `$x` all run or may run user code");
}

#[test]
fn review_probe_the_two_ledger_rows_are_silent_now() {
    // composer `ErrorHandlerTest.php:66`: a bare `array_merge([], 'string');`
    // under strict_types, written to provoke the TypeError it then asserts.
    let composer = "<?php\ndeclare(strict_types=1);\nfunction f(): void {\n    // @phpstan-ignore function.resultUnused, argument.type\n    array_merge([], 'string');\n}\n";
    assert_eq!(dead(composer).len(), 0, "a literal-args call the type table proves throws");
    // phpunit `DeprecatedPhpFeatureTest.php:19`: a bare `strlen(null);` written
    // to raise the deprecation the event recorder observes.
    let phpunit = "<?php\nfunction f(): void {\n    strlen(null);\n    @strlen(null);\n}\n";
    assert_eq!(dead(phpunit).len(), 0, "a literal-args call the engine deprecates");
}

// ---------------------------------------------------------------------------
// ADR-0096 §5: the project-callee half is deferred, so every shape below is
// silent. Pinned now so that the day it lands, the cases that must STAY silent
// are already written and only the first of these changes.
// ---------------------------------------------------------------------------

#[test]
fn a_project_function_is_out_of_scope_in_this_slice() {
    // The shape the family will answer first once the project half is priced
    // per file (ADR-0096 §5): a leaf helper that proves pure.
    let src = "<?php\nfunction pureHelper(string $s): int { return strlen($s); }\nfunction f(): void { pureHelper('x'); }\n";
    assert_eq!(dead(src).len(), 0, "the project half is deferred");
}

#[test]
fn an_unknown_callee_is_silence() {
    silent("string $s", "mystery('x')", "nothing is known about an unresolved name");
}

#[test]
fn a_throwing_project_callee_is_a_validity_check() {
    // The shape PHPStan exempts and names the reason for: the throw IS the
    // result, so the bare call is the whole point of writing it. Silent twice
    // over here — once as a project callee, once as a throwing one.
    let src = "<?php\nfunction validate(string $s): int { if ($s === '') { throw new \\InvalidArgumentException('empty'); } return strlen($s); }\nfunction f(): void { validate('x'); }\n";
    assert_eq!(dead(src).len(), 0, "a throwing call can be a validity check");
}

#[test]
fn a_match_arm_body_is_not_a_statement() {
    // The FP this family would otherwise make, and the reason `Stmt` carries a
    // `value_position` bit: an arm body lowers to a `StmtKind::Call` whose result
    // the construct hands to whoever consumes the `match`.
    let value = "<?php\nfunction f(?string $v): void {\n\techo match (true) {\n\t\t$v === null => strlen('x'),\n\t\tdefault => 0,\n\t};\n}\n";
    assert_eq!(dead(value).len(), 0, "the `echo` consumes the arm's value");
    let assigned = "<?php\nfunction f(?string $v): void {\n\t$r = match (true) {\n\t\t$v === null => strlen('x'),\n\t\tdefault => 0,\n\t};\n}\n";
    assert_eq!(dead(assigned).len(), 0, "the assignment consumes the arm's value");
}

#[test]
fn a_switch_body_is_structured_and_reports() {
    // `lower_switch` structures a `switch` body; the review confirmed the probe
    // reports there (a `try` body is opaque and stays silent).
    let src = "<?php\nfunction f(int $n): void {\n\tswitch ($n) {\n\t\tcase 1:\n\t\t\tstrlen('x');\n\t\t\tbreak;\n\t}\n}\n";
    assert_eq!(dead(src).len(), 1, "a switch arm is a statement list");
}

#[test]
fn a_switch_arm_the_lowering_cannot_structure_is_silence() {
    // `lower_switch` structures an arm only when it ends in a plain `break;`
    // or terminates; a trailing arm without one, a `default:` without one and
    // a braced body leave the whole `switch` opaque, so a discarded call
    // inside is silence — a missed true positive, not a wrong claim, and a
    // property of the lowering (ADR-0096 §5). Pinned so the next change to
    // the lowering moves this deliberately.
    for body in [
        "switch ($n) {\n\t\tcase 1:\n\t\t\tstrlen('x');\n\t}",
        "switch ($n) {\n\t\tdefault:\n\t\t\tstrlen('x');\n\t}",
        "switch ($n) {\n\t\tcase 1: {\n\t\t\tstrlen('x');\n\t\t\tbreak;\n\t\t}\n\t}",
    ] {
        let src = format!("<?php\nfunction f(int $n): void {{\n\t{body}\n}}\n");
        assert_eq!(dead(&src).len(), 0, "an unstructured switch arm is opaque: {body}");
    }
}
