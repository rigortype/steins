//! Integration tests for Transform #1 — phpdoc→native parameter promotion
//! (ADR-0034 / ADR-0037). Each test builds a real multi-file salsa project and
//! asserts on the plan's edits AND the named refusal reasons, plus the applied
//! result where the rewrite is what matters.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::{TransformReport, VouchSet};
use steins_edit::plan_phpdoc_to_native;
use steins_edit::promote::{
    REASON_AMBIGUOUS, REASON_ARG_NOT_PROVEN, REASON_DEFAULT_INCOMPATIBLE, REASON_DYNAMIC_CALL,
    REASON_FINER_THAN_NATIVE, REASON_IMPLICIT_NULLABLE, REASON_NAMED_OR_SPREAD,
    REASON_NOT_REPRESENTABLE, REASON_NO_OBSERVED_CALLERS, REASON_REFERENCED_AS_VALUE,
};

/// Plan the transform over a `(path, source)` project.
fn plan(files: &[(&str, &str)]) -> TransformReport {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs);
    plan_phpdoc_to_native(&db, project, &VouchSet::empty(), None)
}

/// Plan, then apply the plan's edits to the first file, returning its rewrite.
fn apply_first(files: &[(&str, &str)]) -> String {
    let report = plan(files);
    report.plan.apply_file(files[0].0, files[0].1)
}

/// Every enumerated site must end transformed-or-refused (ADR-0034 point 3b).
fn assert_oracle_complete(report: &TransformReport) {
    assert!(
        report.oracle.is_complete(),
        "oracle incomplete: {:?}",
        report.oracle
    );
}

fn only_reason(report: &TransformReport) -> &str {
    assert_eq!(report.refusals.len(), 1, "expected one refusal, got: {:#?}", report.refusals);
    &report.refusals[0].reason
}

// ---- 1. Happy promotion --------------------------------------------------

#[test]
fn happy_promotion_all_callers_literal_int() {
    let lib = "<?php\n/**\n * @param int $x\n */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(2);\nf(3);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(report.oracle.transformed, 1);
    assert!(report.refusals.is_empty(), "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    // Native hint added, @param line removed, docblock still valid.
    assert!(out.contains("function f(int $x)"), "got:\n{out}");
    assert!(!out.contains("@param"), "tag should be gone:\n{out}");
    assert!(out.contains("/**"), "docblock delimiters preserved:\n{out}");
}

#[test]
fn zero_callers_refuses_no_observed_callers() {
    // ADR-0047 §4 / ADR-0041 §3 amendment: no call sites at all is zero evidence,
    // not vacuous proof — a candidate with an empty enumerated caller set must
    // refuse rather than promote (the framework-reflection hole this closes).
    let lib = "<?php\n/** @param string $s */\nfunction g($s) { return $s; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_NO_OBSERVED_CALLERS);
    assert!(report.plan.is_empty(), "must not promote on zero observed callers");
}

#[test]
fn promotes_nullable_union() {
    let lib = "<?php\n/** @param int|null $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(null);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    // A single scalar plus `null` renders in the canonical `?T` short form.
    assert!(out.contains("function f(?int $x)"), "got:\n{out}");
}

#[test]
fn promotes_multi_scalar_nullable_union_long_form() {
    let lib = "<?php\n/** @param int|string|null $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(\"a\");\nf(null);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    // A multi-member nullable union renders the explicit `|null` long form.
    assert!(out.contains("function f(int|string|null $x)"), "got:\n{out}");
}

// ---- 2. Refusals ---------------------------------------------------------

#[test]
fn refuses_unproven_variable_argument() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    // `$y` is not a proven literal at the call site.
    let main = "<?php\nfunction caller($y) { f($y); }\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_ARG_NOT_PROVEN);
    // Nothing was edited.
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_when_a_caller_passes_wrong_literal() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(\"nope\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_ARG_NOT_PROVEN);
}

#[test]
fn refuses_on_dynamic_call() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    // A dynamic call could target any free function.
    let main = "<?php\n$fn = 'f';\n$fn(1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_DYNAMIC_CALL);
}

#[test]
fn refuses_on_named_or_spread_args() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(x: 1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_NAMED_OR_SPREAD);
}

#[test]
fn refuses_when_function_referenced_as_string_value() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    // `'f'` flows as a string value — a call_user_func-style caller is invisible.
    let main = "<?php\narray_map('f', [1, 2, 3]);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_REFERENCED_AS_VALUE);
}

#[test]
fn refuses_when_call_user_func_target_is_a_bare_variable() {
    // The generic-invoker gap flagged alongside issue #6 (same family: a
    // `call_user_func`/`call_user_func_array` callable argument that is not a
    // name-shaped literal carries no value the reference scan can see, so it
    // must taint broadly — mirroring a direct dynamic `$fn()` call — rather than
    // silently letting the candidate promote.
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nfunction caller($name) { call_user_func($name, 1); }\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_DYNAMIC_CALL);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_when_call_user_func_array_target_is_a_bare_variable() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nfunction caller($name) { call_user_func_array($name, [1]); }\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_DYNAMIC_CALL);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_first_class_callable_reference() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\n$g = f(...);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_REFERENCED_AS_VALUE);
}

#[test]
fn refuses_refined_scalar_as_finer_than_native() {
    // `positive-int` is a refinement of `int` — finer than its native rendering.
    let lib = "<?php\n/** @param positive-int $x */\nfunction f($x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_FINER_THAN_NATIVE);
}

#[test]
fn refuses_bounded_int_as_finer_than_native() {
    let lib = "<?php\n/** @param int<0, max> $x */\nfunction f($x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_FINER_THAN_NATIVE);
}

#[test]
fn refuses_non_empty_string_as_finer_than_native() {
    let lib = "<?php\n/** @param non-empty-string $x */\nfunction f($x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_FINER_THAN_NATIVE);
}

#[test]
fn refuses_array_type_as_non_representable() {
    // A genuinely non-scalar type — not a refinement, so the other reason.
    let lib = "<?php\n/** @param int[] $x */\nfunction f($x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_NOT_REPRESENTABLE);
}

#[test]
fn refuses_class_type_as_non_representable() {
    let lib = "<?php\n/** @param \\App\\User $x */\nfunction f($x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_NOT_REPRESENTABLE);
}

#[test]
fn refuses_ambiguous_duplicate_fqn() {
    // `f` defined twice — resolution is ambiguous.
    let a = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let b = "<?php\nfunction f($x) { return $x; }\n";
    let report = plan(&[("a.php", a), ("b.php", b)]);
    // The documented decl is the only enumerated site; it must refuse ambiguous.
    assert!(report.refusals.iter().any(|r| r.reason == REASON_AMBIGUOUS), "{:#?}", report.refusals);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_implicit_nullable_default() {
    // `= null` default makes the param implicitly nullable; native `int` is not.
    let lib = "<?php\n/** @param int $x */\nfunction f($x = null) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_IMPLICIT_NULLABLE);
}

// ---- 3. Out-of-domain (not enumerated) -----------------------------------

#[test]
fn param_with_native_hint_is_not_enumerated() {
    let lib = "<?php\n/** @param int $x */\nfunction f(int $x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(report.oracle.enumerated, 0);
    assert!(report.refusals.is_empty());
}

#[test]
fn param_with_complex_hint_is_not_enumerated() {
    // A complex hint lowers `param.ty` to None, but the source shows a hint.
    let lib = "<?php\n/** @param int $x */\nfunction f(\\Foo|\\Bar $x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(report.oracle.enumerated, 0, "{:#?}", report);
}

#[test]
fn undocumented_param_is_not_enumerated() {
    let lib = "<?php\nfunction f($x) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(report.oracle.enumerated, 0);
}

// ---- 4. Edit mechanics ---------------------------------------------------

#[test]
fn multibyte_source_is_promoted_correctly() {
    // A multibyte string literal in the body — spans are byte offsets, the splice
    // must stay on char boundaries.
    let lib =
        "<?php\n/** @param string $s */\nfunction greet($s) { return \"caf\u{e9} \" . $s; }\n";
    let main = "<?php\ngreet(\"x\");\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    assert!(out.contains("function greet(string $s)"), "got:\n{out}");
    assert!(out.contains("caf\u{e9}"), "multibyte body preserved:\n{out}");
}

#[test]
fn tag_deletion_leaves_valid_multiline_docblock() {
    let lib = "<?php\n/**\n * @param int $x\n * @return int\n */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    assert!(out.contains("function f(int $x)"), "got:\n{out}");
    // The @return line survives; the @param line is gone; delimiters intact.
    assert!(out.contains("@return int"), "sibling tag kept:\n{out}");
    assert!(!out.contains("@param"), "promoted tag removed:\n{out}");
    assert!(out.contains("/**") && out.contains("*/"), "docblock valid:\n{out}");
}

#[test]
fn single_line_docblock_is_removed_whole() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    assert!(out.contains("function f(int $x)"), "got:\n{out}");
    assert!(!out.contains("@param"), "tag gone:\n{out}");
    // No dangling `/**` from a half-deleted single-line docblock.
    assert!(!out.contains("/**"), "single-tag docblock removed whole:\n{out}");
}

#[test]
fn two_params_promoted_in_one_signature() {
    let lib = "<?php\n/**\n * @param int $x\n * @param string $y\n */\nfunction f($x, $y) { return $x; }\n";
    let main = "<?php\nf(1, \"a\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.enumerated, 2);
    assert_eq!(report.oracle.transformed, 2, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("function f(int $x, string $y)"), "got:\n{out}");
    assert!(!out.contains("@param"), "both tags gone:\n{out}");
}

// ---- 5. Incompatible parameter defaults (would emit a compile-time fatal) ----

#[test]
fn refuses_string_default_for_int_param() {
    // `int $x = 'str'` is a compile-time fatal ("Cannot use string as default
    // value for parameter of type int"). Must refuse, never promote.
    let lib = "<?php\n/** @param int $x */\nfunction f($x = 'str') { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_DEFAULT_INCOMPATIBLE);
    assert!(report.plan.is_empty(), "must not emit a fatal-producing edit");
}

#[test]
fn refuses_float_default_for_int_param() {
    // `int $x = 3.0` is a compile-time fatal.
    let lib = "<?php\n/** @param int $x */\nfunction f($x = 3.0) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_DEFAULT_INCOMPATIBLE);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_array_default_for_int_param() {
    // `int $x = []` is a compile-time fatal.
    let lib = "<?php\n/** @param int $x */\nfunction f($x = []) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_DEFAULT_INCOMPATIBLE);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_unprovable_constant_default_conservatively() {
    // `int $x = PHP_INT_MAX` is legal PHP, but v1 cannot prove the constant's
    // type — refuse rather than risk an unprovable promotion (sound, conservative).
    let lib = "<?php\n/** @param int $x */\nfunction f($x = PHP_INT_MAX) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(only_reason(&report), REASON_DEFAULT_INCOMPATIBLE);
}

#[test]
fn promotes_compatible_int_default() {
    // `int $x = 0` is a compatible, provable default — promotion is safe. A
    // literal-proven caller keeps this test independent of the zero-caller gate.
    let lib = "<?php\n/** @param int $x */\nfunction f($x = 0) { return $x; }\n";
    let main = "<?php\nf();\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("function f(int $x = 0)"), "got:\n{out}");
}

#[test]
fn promotes_nullable_with_null_default() {
    // `?int $x = null` is valid — the nullable native type admits the null
    // default. A literal-proven caller keeps this test independent of the
    // zero-caller gate.
    let lib = "<?php\n/** @param int|null $x */\nfunction f($x = null) { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("function f(?int $x = null)"), "got:\n{out}");
}

// ---- 6. Variadic parameters collect every trailing argument ----------------

#[test]
fn refuses_variadic_when_a_trailing_arg_is_bad() {
    // `@param int $xs` on `...$xs`: the call `f(1, 'str')` flows `'str'` into the
    // variadic at arg position 1. Promoting to `int ...$xs` would make it a
    // runtime TypeError — every trailing arg must be proven, not just index 0.
    let lib = "<?php\n/** @param int $xs */\nfunction f(...$xs) { return $xs; }\n";
    let main = "<?php\nf(1, 'str');\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_ARG_NOT_PROVEN);
    assert!(report.plan.is_empty());
}

#[test]
fn promotes_variadic_when_all_trailing_args_are_proven() {
    let lib = "<?php\n/** @param int $xs */\nfunction f(...$xs) { return $xs; }\n";
    let main = "<?php\nf(1, 2, 3);\nf();\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("function f(int ...$xs)"), "got:\n{out}");
}

#[test]
fn by_ref_param_without_literal_callers_is_refused_not_broken() {
    // A by-ref param can only receive lvalues, never proven literals → refusal.
    let lib = "<?php\n/** @param int $x */\nfunction f(&$x) { $x = 1; }\n";
    let main = "<?php\nfunction caller() { $v = 0; f($v); }\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_ARG_NOT_PROVEN);
}

// ---- ADR-0046 §2: dynamic-code obstacles ----------------------------------

use steins_edit::promote::{REASON_DYNAMIC_INCLUDE, REASON_EVAL_PRESENT};

/// Plan with a vouch set (ADR-0046 §2 vouching valve).
fn plan_vouched(files: &[(&str, &str)], vouches: &VouchSet) -> TransformReport {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs);
    plan_phpdoc_to_native(&db, project, vouches, None)
}

/// An unproven `eval` anywhere in the project makes "all callers proven" false:
/// every candidate refuses `eval-present`, and the obstacle is recorded once.
#[test]
fn eval_in_project_refuses_all_candidates() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let evil = "<?php\neval('do_something();');\n";
    let report = plan(&[("lib.php", lib), ("main.php", main), ("evil.php", evil)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 0, "eval must block promotion: {:#?}", report.plan);
    assert_eq!(only_reason(&report), REASON_EVAL_PRESENT);
    assert_eq!(report.obstacles.len(), 1);
    assert_eq!(report.obstacles[0].reason, REASON_EVAL_PRESENT);
    assert_eq!(report.obstacles[0].sites.len(), 1);
    assert!(report.obstacles[0].sites[0].path.ends_with("evil.php"));
}

/// The canonical gap regression: `eval('foo(42)')` calls `foo` with no CST call
/// site. The string-value reference scan (exact-name match) cannot see it, so
/// without the eval obstacle the promotion would be unsound. It must refuse.
#[test]
fn canonical_eval_foo_42_regression() {
    let lib = "<?php\n/** @param int $x */\nfunction foo($x) { return $x; }\n";
    // No visible call site — only an eval string mentioning foo(42).
    let main = "<?php\neval('foo(42)');\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 0, "eval('foo(42)') must block promotion");
    assert_eq!(only_reason(&report), REASON_EVAL_PRESENT);
}

/// A dynamic `require $x` (unproven path) is a `dynamic-include-present` obstacle.
#[test]
fn dynamic_include_refuses_all_candidates() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\n$page = $_GET['p'];\nrequire $page;\nf(1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 0);
    assert_eq!(only_reason(&report), REASON_DYNAMIC_INCLUDE);
    assert_eq!(report.obstacles[0].reason, REASON_DYNAMIC_INCLUDE);
}

/// A proven literal include that resolves INSIDE the analyzed universe is
/// enumeration-benign (its file's calls are already counted): no obstacle, and
/// the promotion proceeds.
#[test]
fn literal_in_universe_include_is_benign() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    // `require __DIR__ . '/lib.php'` resolves to the in-universe lib.php.
    let main = "<?php\nrequire __DIR__ . '/lib.php';\nf(1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert!(report.obstacles.is_empty(), "in-universe include is benign: {:#?}", report.obstacles);
    assert_eq!(report.oracle.transformed, 1, "promotion must proceed: {:#?}", report.refusals);
}

/// A proven literal include that resolves OUTSIDE the universe (a compiled-
/// template cache) is a `dynamic-include-present` obstacle.
#[test]
fn out_of_universe_literal_include_is_obstacle() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nrequire 'cache/compiled_tpl_9f3.php';\nf(1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 0);
    assert_eq!(only_reason(&report), REASON_DYNAMIC_INCLUDE);
}

/// Vendor presumption (ADR-0046): eval inside a `vendor/` path is composer
/// autoload plumbing — it does NOT raise an obstacle, and promotion proceeds.
#[test]
fn vendor_eval_is_not_an_obstacle() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let vendor = "<?php\neval('class_loader();');\n";
    let report = plan(&[
        ("lib.php", lib),
        ("main.php", main),
        ("vendor/composer/loader.php", vendor),
    ]);
    assert_oracle_complete(&report);
    assert!(report.obstacles.is_empty(), "vendor eval must not obstruct: {:#?}", report.obstacles);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
}

/// The vouching valve: a vouched eval site does not raise its obstacle, so the
/// promotion proceeds — but the run carries the completeness-claim downgrade
/// (`vouched_exemptions`), it does not silently pass (ADR-0046 §2 / ADR-0037).
#[test]
fn vouched_eval_site_proceeds_with_downgrade() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let evil = "<?php\neval('legacy_bootstrap();');\n"; // eval on line 2
    let vouches = VouchSet::from_entries([("evil.php".to_owned(), 2)]);
    let report = plan_vouched(&[("lib.php", lib), ("main.php", main), ("evil.php", evil)], &vouches);
    assert_oracle_complete(&report);
    assert!(report.obstacles.is_empty(), "vouched eval must not obstruct: {:#?}", report.obstacles);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    assert_eq!(report.vouched_exemptions.len(), 1, "downgrade note must carry the vouched site");
    assert!(report.vouched_exemptions[0].path.ends_with("evil.php"));
}

/// An unvouched eval site alongside a vouched one still raises the obstacle.
#[test]
fn partially_vouched_eval_still_obstructs() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\n";
    let a = "<?php\neval('a();');\n";
    let b = "<?php\neval('b();');\n";
    let vouches = VouchSet::from_entries([("a.php".to_owned(), 2)]);
    let report = plan_vouched(
        &[("lib.php", lib), ("main.php", main), ("a.php", a), ("b.php", b)],
        &vouches,
    );
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 0, "the unvouched eval still blocks");
    assert_eq!(only_reason(&report), REASON_EVAL_PRESENT);
    assert_eq!(report.obstacles[0].sites.len(), 1, "only the unvouched site is listed");
    assert!(report.obstacles[0].sites[0].path.ends_with("b.php"));
    assert_eq!(report.vouched_exemptions.len(), 1);
}
