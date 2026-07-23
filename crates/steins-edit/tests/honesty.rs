//! Integration tests for Transform #2 — phpdoc-honesty repair (ADR-0037 /
//! ADR-0041). Each test builds a real multi-file salsa project and asserts on the
//! plan's edits AND the named refusal reasons, plus the applied rewrite where the
//! new tag text is what matters. Every rendered type is round-tripped through the
//! phpdoc parser as a self-check.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::TransformReport;
use steins_edit::plan_phpdoc_honesty;
use steins_edit::honesty::{
    REASON_AMBIGUOUS, REASON_ARGUMENT_NOT_PROVEN, REASON_DYNAMIC_CALL, REASON_NAMED_OR_SPREAD,
    REASON_NATIVE_CONTRADICTS, REASON_REFERENCED_AS_VALUE, REASON_RETURN_NOT_PROVEN,
    REASON_TYPE_NOT_RENDERABLE,
};

fn plan(files: &[(&str, &str)]) -> TransformReport {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs);
    plan_phpdoc_honesty(&db, project)
}

fn apply_first(files: &[(&str, &str)]) -> String {
    let report = plan(files);
    report.plan.apply_file(files[0].0, files[0].1)
}

fn assert_oracle_complete(report: &TransformReport) {
    assert!(report.oracle.is_complete(), "oracle incomplete: {:?}", report.oracle);
}

fn only_reason(report: &TransformReport) -> &str {
    assert_eq!(report.refusals.len(), 1, "expected one refusal, got: {:#?}", report.refusals);
    &report.refusals[0].reason
}

/// Every rewritten tag type in the applied output must parse as a phpdoc type
/// (the round-trip self-check). We scan the applied file's `@param`/`@return`
/// lines and re-parse their type prefix.
fn assert_docblock_types_parse(applied: &str) {
    for line in applied.lines() {
        let trimmed = line.trim_start_matches([' ', '\t', '*']);
        for tag in ["@param ", "@return ", "@phpstan-param ", "@psalm-param "] {
            if let Some(rest) = trimmed.strip_prefix(tag) {
                let parsed = steins_phpdoc::parse_type(rest)
                    .unwrap_or_else(|e| panic!("re-parse `{rest}`: {e}"));
                assert!(parsed.consumed > 0, "empty type in `{rest}`");
            }
        }
    }
}

// ---- 1. `@param` widening: the canonical ADR-0037 case ---------------------

#[test]
fn canonical_int_plus_numeric_string_widens_to_union() {
    // `@param int $id` but callers pass both an int and numeric strings (the PDO
    // illusion). The honest type is `int|numeric-string`.
    let lib = "<?php\n/** @param int $id */\nfunction f($id) { return $id; }\n";
    let main = "<?php\nf(1);\nf(\"12\");\nf(\"34\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@param int|numeric-string $id"), "got:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn no_int_caller_widens_to_proven_type_alone_not_unioned_with_declared() {
    // `@param int $x` but NO caller passes an int — only numeric strings. The
    // honest type is the proven type ALONE (`numeric-string`), never `int|…`
    // (proven beats declared; the declared type is not gratuitously unioned in).
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"12\");\nf(\"34\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@param numeric-string $x"), "got:\n{out}");
    assert!(!out.contains("int"), "declared `int` must not be unioned in:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn join_includes_every_observed_value_not_only_the_violating_ones() {
    // Callers pass an int (admitted by `int`) and a non-numeric string (violates).
    // The honest type must admit BOTH: `int|'nope'`.
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(\"nope\");\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    assert!(out.contains("@param int|'nope' $x"), "got:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn dedup_collapses_repeated_literals() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"a\");\nf(\"a\");\nf(\"a\");\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    // Three identical string literals collapse to one member.
    assert!(out.contains("@param 'a' $x"), "got:\n{out}");
}

// ---- 2. `@param` refusals --------------------------------------------------

#[test]
fn refuses_argument_not_proven_when_a_caller_is_non_literal() {
    // A literal violation makes the site a candidate; a non-literal sibling caller
    // then blocks the join.
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"nope\");\nfunction caller($y) { f($y); }\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_ARGUMENT_NOT_PROVEN);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_on_dynamic_call() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"nope\");\n$fn = 'g';\n$fn(1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_DYNAMIC_CALL);
}

#[test]
fn refuses_on_named_args() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"nope\");\nf(x: 1);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_NAMED_OR_SPREAD);
}

#[test]
fn refuses_when_function_referenced_as_value() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"nope\");\narray_map('f', [1, 2, 3]);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_REFERENCED_AS_VALUE);
}

#[test]
fn refuses_ambiguous_when_simple_name_also_calls_unresolved() {
    // `A\f` is called (and its lie observed) inside namespace A; a bare `f(1)` in
    // the global namespace does not resolve, so the simple name `f` is ambiguous —
    // its callers can't all be proven (PHP's global-fallback could target A\f).
    let a = "<?php\nnamespace A;\n/** @param int $x */\nfunction f($x) { return $x; }\nf(\"nope\");\n";
    let b = "<?php\nf(1);\n";
    let report = plan(&[("a.php", a), ("b.php", b)]);
    assert_oracle_complete(&report);
    assert!(report.refusals.iter().any(|r| r.reason == REASON_AMBIGUOUS), "{:#?}", report.refusals);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_type_not_renderable_for_array_argument() {
    // An array argument violates `@param int` and is "proven", but a value set with
    // an array has no faithful scalar phpdoc spelling.
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf([1, 2]);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_TYPE_NOT_RENDERABLE);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_native_contradicts_proven() {
    // Native `int $x` but `@param string $x`; a caller passes an int (violates the
    // phpdoc `string` → lie) and another passes a string the native `int` rejects.
    // The join isn't admitted by the native hint — a different disease.
    let lib = "<?php\n/** @param string $x */\nfunction f(int $x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(\"y\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_NATIVE_CONTRADICTS);
    assert!(report.plan.is_empty());
}

#[test]
fn rewrites_param_when_native_hint_admits_the_join() {
    // Native `int $x`, lying `@param string $x`, all callers pass ints. The proven
    // join `int` IS admitted by the native hint → rewrite the phpdoc to match.
    let lib = "<?php\n/** @param string $x */\nfunction f(int $x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(2);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@param int $x"), "got:\n{out}");
}

// ---- 3. Out-of-domain (honest tags are not enumerated) ---------------------

#[test]
fn honest_param_is_not_enumerated() {
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(1);\nf(2);\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.enumerated, 0);
    assert!(report.plan.is_empty());
}

#[test]
fn assertion_helper_param_is_exempt() {
    // A `@phpstan-assert` makes `@param` a post-condition — no mismatch fires.
    let lib = "<?php\n/**\n * @param int $x\n * @phpstan-assert int $x\n */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"nope\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.enumerated, 0, "{:#?}", report);
}

// ---- 4. Prefixed / plain tag precedence ------------------------------------

#[test]
fn both_prefixed_and_plain_rewritten_when_plain_also_lies() {
    // Governing `@phpstan-param int` and a plain `@param int`; callers pass numeric
    // strings. The prefixed governs and is rewritten; the plain `int` also fails to
    // admit the join, so it is rewritten too (two edits, one site).
    let lib = "<?php\n/**\n * @param int $x\n * @phpstan-param int $x\n */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"12\");\nf(\"34\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    assert_eq!(report.plan.edits.len(), 2, "both tags rewritten: {:#?}", report.plan.edits);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@param numeric-string $x"), "plain rewritten:\n{out}");
    assert!(out.contains("@phpstan-param numeric-string $x"), "prefixed rewritten:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn plain_left_untouched_when_it_still_admits_the_join() {
    // Governing `@phpstan-param int` lies; plain `@param string` still admits the
    // numeric-string join, so only the prefixed governing tag is rewritten.
    let lib = "<?php\n/**\n * @param string $x\n * @phpstan-param int $x\n */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"12\");\nf(\"34\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    assert_eq!(report.plan.edits.len(), 1, "only the governing tag: {:#?}", report.plan.edits);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@param string $x"), "plain kept:\n{out}");
    assert!(out.contains("@phpstan-param numeric-string $x"), "prefixed rewritten:\n{out}");
}

// ---- 5. `@return` widening -------------------------------------------------

#[test]
fn return_widens_to_proven_union() {
    // Every return is a numeric string, but `@return int` is declared.
    let lib = "<?php\n/** @return int */\nfunction f($c) { if ($c) { return \"1\"; } return \"2\"; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@return numeric-string"), "got:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn return_edit_preserves_the_description_tail() {
    let lib = "<?php\n/** @return int the identifier */\nfunction f() { return \"1\"; }\n";
    let out = apply_first(&[("lib.php", lib)]);
    // Only the type prefix is replaced; the description survives.
    assert!(out.contains("@return '1' the identifier"), "got:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn refuses_return_not_proven_for_non_literal_return() {
    let lib = "<?php\n/** @return int */\nfunction f($c) { if ($c) { return \"x\"; } return $c; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_RETURN_NOT_PROVEN);
    assert!(report.plan.is_empty());
}

#[test]
fn refuses_return_not_proven_on_possible_fallthrough_null() {
    // A single `if` with no `else` may fall through and implicitly return `null`;
    // widening from the one explicit return would omit it.
    let lib = "<?php\n/** @return int */\nfunction f($c) { if ($c) { return \"x\"; } }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_RETURN_NOT_PROVEN);
}

#[test]
fn refuses_return_not_proven_when_a_return_hides_in_a_loop() {
    let lib = "<?php\n/** @return int */\nfunction f($xs) { foreach ($xs as $x) { return \"x\"; } return \"y\"; }\n";
    let report = plan(&[("lib.php", lib)]);
    // The foreach is Opaque — a hidden return path v1 cannot enumerate.
    assert_eq!(only_reason(&report), REASON_RETURN_NOT_PROVEN);
}

#[test]
fn honest_return_is_not_enumerated() {
    let lib = "<?php\n/** @return int */\nfunction f() { return 1; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_eq!(report.oracle.enumerated, 0);
}

// ---- 5b. Docblock-unsafe string literals must not corrupt the docblock -----

#[test]
fn star_slash_string_widens_to_keyword_not_a_broken_literal() {
    // A caller passes a string containing the block-comment terminator `*/`.
    // Rendering it as a literal (`'a*/b'`) would close the enclosing `/** … */`
    // early — a hard PHP parse error. The honest, valid repair widens to a keyword.
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"a*/b\");\n";
    let report = plan(&[("lib.php", lib), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@param non-falsy-string $x"), "got:\n{out}");
    // The applied docblock must contain exactly one `*/` — its own terminator.
    assert_eq!(out.matches("*/").count(), 1, "docblock terminator corrupted:\n{out}");
    assert!(!out.contains("'a*/b'"), "wrote a corrupting literal:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn star_slash_in_a_literal_union_widens_to_keyword() {
    // The literal-union path (multiple distinct values ≤ CAP) is equally unsafe:
    // one `*/`-bearing member forces the whole group to a keyword.
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf(\"ok\");\nf(\"a*/b\");\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    assert!(out.contains("@param non-falsy-string $x"), "got:\n{out}");
    assert_eq!(out.matches("*/").count(), 1, "docblock terminator corrupted:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn newline_bearing_string_widens_to_keyword_not_a_split_literal() {
    // A single-quoted PHP literal carrying a raw newline is a valid argument, but a
    // phpdoc quoted literal cannot hold a newline — a literal render would split the
    // tag across physical lines. It must widen to a keyword instead.
    let lib = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    let main = "<?php\nf('line1\nline2');\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    assert!(out.contains("@param non-falsy-string $x"), "got:\n{out}");
    // The docblock stays one physical line: `/** @param … $x */`.
    let doc_line = out.lines().find(|l| l.contains("@param")).unwrap();
    assert!(doc_line.contains("*/"), "docblock split across lines:\n{out}");
    assert_docblock_types_parse(&out);
}

// ---- 6. Edit mechanics -----------------------------------------------------

#[test]
fn multibyte_docblock_and_body_are_edited_on_byte_offsets() {
    // A multibyte description in the docblock plus a multibyte body: the type span
    // is a byte offset, so the splice must land on char boundaries.
    let lib = "<?php\n/** @param int $s café */\nfunction greet($s) { return \"caf\u{e9}\"; }\n";
    let main = "<?php\ngreet(\"x\");\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    assert!(out.contains("@param 'x' $s café"), "got:\n{out}");
    assert!(out.contains("caf\u{e9}"), "multibyte body preserved:\n{out}");
    assert_docblock_types_parse(&out);
}

#[test]
fn multiline_docblock_only_type_span_replaced() {
    let lib = "<?php\n/**\n * @param int $x the value\n * @return int\n */\nfunction f($x) { return \"z\"; }\n";
    let main = "<?php\nf(\"a\");\n";
    let out = apply_first(&[("lib.php", lib), ("main.php", main)]);
    // @param type widened, its description kept; @return also widened.
    assert!(out.contains("@param 'a' $x the value"), "got:\n{out}");
    assert!(out.contains("@return 'z'"), "got:\n{out}");
    assert!(out.contains("/**") && out.contains("*/"), "docblock intact:\n{out}");
    assert_docblock_types_parse(&out);
}
