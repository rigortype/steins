//! Integration tests for Transform #1 — phpdoc→native parameter promotion
//! (ADR-0034 / ADR-0037). Each test builds a real multi-file salsa project and
//! asserts on the plan's edits AND the named refusal reasons, plus the applied
//! result where the rewrite is what matters.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::TransformReport;
use steins_edit::plan_phpdoc_to_native;
use steins_edit::promote::{
    REASON_AMBIGUOUS, REASON_ARG_NOT_PROVEN, REASON_DEFAULT_INCOMPATIBLE, REASON_DYNAMIC_CALL,
    REASON_FINER_THAN_NATIVE, REASON_IMPLICIT_NULLABLE, REASON_NAMED_OR_SPREAD,
    REASON_NOT_REPRESENTABLE, REASON_REFERENCED_AS_VALUE,
};

/// Plan the transform over a `(path, source)` project.
fn plan(files: &[(&str, &str)]) -> TransformReport {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs);
    plan_phpdoc_to_native(&db, project)
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
fn zero_callers_promotes_vacuously() {
    // No call sites at all — vacuously all-callers-proven (external callers are
    // outside the analysis boundary).
    let lib = "<?php\n/** @param string $s */\nfunction g($s) { return $s; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("function g(string $s)"), "got:\n{out}");
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
    // `int $x = 0` is a compatible, provable default — promotion is safe.
    let lib = "<?php\n/** @param int $x */\nfunction f($x = 0) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("function f(int $x = 0)"), "got:\n{out}");
}

#[test]
fn promotes_nullable_with_null_default() {
    // `?int $x = null` is valid — the nullable native type admits the null default.
    let lib = "<?php\n/** @param int|null $x */\nfunction f($x = null) { return $x; }\n";
    let report = plan(&[("lib.php", lib)]);
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
