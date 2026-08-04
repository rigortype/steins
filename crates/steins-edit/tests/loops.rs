//! Integration tests for Transform #3 — loop→`array_map` (ADR-0076, issue #116).
//!
//! Two things are pinned here. First the **rewrite**: the flagship shape, and
//! that an applied file re-enumerates zero candidates. Second the **refusal
//! taxonomy**: every reason in ADR-0076 §4 is exercised at least once, because a
//! transform whose whole contract is "transformed or refused, by name" is only
//! as good as the names it can actually produce.
//!
//! Behaviour identity between the two spellings is measured, not argued, by the
//! differential fixture in `crates/steins-cli/tests/transform_loops.rs`, which
//! runs both under the real `php`.

use steins_edit::TransformReport;
use steins_edit::VouchSet;
use steins_edit::loops::{
    REASON_ACCUMULATOR_INIT_NOT_ADJACENT, REASON_ACCUMULATOR_NOT_EMPTY,
    REASON_ACCUMULATOR_READ_IN_BODY, REASON_BODY_CALL_UNRESOLVED, REASON_BODY_EFFECTS,
    REASON_BODY_NOT_SINGLE_APPEND, REASON_BODY_THROWS, REASON_EARLY_EXIT,
    REASON_ITERATION_VAR_LIVE_AFTER, REASON_KEY_BINDING, REASON_REFERENCE_BINDING,
    REASON_SUBJECT_NOT_PROVEN_ARRAY, REASON_SUBJECT_NOT_PROVEN_LIST, REASON_SUBJECT_NOT_VARIABLE,
    REASON_VALUE_BINDING_NOT_VARIABLE,
};
use steins_edit::plan_loop_to_array_map;

fn plan(files: &[(&str, &str)]) -> TransformReport {
    let db = steins_db::SteinsDatabase::default();
    let inputs: Vec<steins_db::SourceFile> = files
        .iter()
        .map(|(p, t)| steins_db::SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = steins_db::Project::new(
        &db,
        inputs,
        steins_db::ProjectLayout::fallback(),
        steins_db::PluginFacts::none(),
    );
    plan_loop_to_array_map(&db, project, &VouchSet::empty(), None)
}

fn assert_oracle_complete(report: &TransformReport) {
    assert!(report.oracle.is_complete(), "oracle incomplete: {:?}", report.oracle);
}

/// Wrap a loop body in a function whose subject is a proven list literal, so the
/// parity gates pass and the test is about whatever `body` does.
fn with_proven_subject(body: &str) -> String {
    format!("<?php\nfunction run(): array {{\n    $xs = [1, 2, 3];\n    $out = [];\n{body}\n    return $out;\n}}\n")
}

/// The single refusal reason a one-loop project produced.
fn only_reason(report: &TransformReport) -> &str {
    assert_eq!(report.refusals.len(), 1, "expected one refusal, got: {:#?}", report.refusals);
    &report.refusals[0].reason
}

/// Plan a one-file project whose only loop should refuse, and name the reason.
fn refusal_for(source: &str) -> String {
    let report = plan(&[("lib.php", source)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 0, "expected a refusal, got a rewrite:\n{source}");
    only_reason(&report).to_owned()
}

// ---- 1. The rewrite --------------------------------------------------------

const FLAGSHIP: &str = "\
<?php

function double(int $n): int
{
    return $n * 2;
}

function run(): array
{
    $xs = [1, 2, 3];
    $out = [];
    foreach ($xs as $x) {
        $out[] = double($x);
    }

    return $out;
}
";

#[test]
fn flagship_shape_is_rewritten_consuming_both_statements() {
    let report = plan(&[("lib.php", FLAGSHIP)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", FLAGSHIP);
    assert!(
        out.contains("    $out = array_map(fn ($x) => double($x), $xs);\n"),
        "rewrite missing:\n{out}"
    );
    // Both statements are consumed — no stranded initializer, no leftover loop.
    assert!(!out.contains("foreach"), "loop survived:\n{out}");
    assert!(!out.contains("$out = [];"), "initializer stranded:\n{out}");
    // Everything outside the two statements is byte-identical (ADR-0003).
    assert!(out.contains("function double(int $n): int\n{\n    return $n * 2;\n}"), "got:\n{out}");
    assert!(out.contains("\n\n    return $out;\n}\n"), "tail perturbed:\n{out}");
}

#[test]
fn an_applied_file_enumerates_no_candidate() {
    // Idempotence: the rewritten statement is no longer a `foreach`, so it is no
    // longer even a candidate — the oracle counts zero, not "one, refused".
    let applied = plan(&[("lib.php", FLAGSHIP)]).plan.apply_file("lib.php", FLAGSHIP);
    let again = plan(&[("lib.php", &applied)]);
    assert_oracle_complete(&again);
    assert_eq!(again.oracle.enumerated, 0, "{:#?}", again.refusals);
    assert!(again.plan.is_empty());
}

#[test]
fn a_pure_builtin_body_is_rewritten() {
    let src = with_proven_subject(
        "    foreach ($xs as $x) {\n        $out[] = strlen((string) $x);\n    }",
    );
    let report = plan(&[("lib.php", &src)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("lib.php", &src);
    assert!(out.contains("array_map(fn ($x) => strlen((string) $x), $xs);"), "got:\n{out}");
}

#[test]
fn every_foreach_is_enumerated_even_the_ineligible_ones() {
    // The candidate domain is the whole construct family (ADR-0076 §4): the
    // narrowness is counted, not hidden.
    let src = "<?php\nfunction f(array $a, array $b): void {\n    foreach ($a as $x) { echo $x; }\n    foreach ($b as $k => $v) { echo $k; }\n}\n";
    let report = plan(&[("lib.php", src)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 2);
    assert_eq!(report.oracle.refused, 2);
}

// ---- 2. The purity bar (ADR-0076 §2) ---------------------------------------

#[test]
fn a_proven_effect_label_refuses_and_is_named() {
    let src = with_proven_subject(
        "    foreach ($xs as $x) {\n        $out[] = shout($x);\n    }",
    ) + "function shout(int $n): int { echo $n; return $n; }\n";
    let report = plan(&[("lib.php", &src)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_BODY_EFFECTS);
    // The detail names the proven label, so an agent can act on it.
    assert!(report.refusals[0].detail.contains("output"), "detail: {}", report.refusals[0].detail);
}

#[test]
fn a_proven_throw_refuses_even_though_pure_admits_throw() {
    // ADR-0006 `Pure` admits `throw`; this transform must not (ADR-0076 §2.3).
    let src = with_proven_subject("    foreach ($xs as $x) {\n        $out[] = boom($x);\n    }")
        + "function boom(int $n): int { throw new RuntimeException('no'); }\n";
    let report = plan(&[("lib.php", &src)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_BODY_THROWS);
    assert!(
        report.refusals[0].detail.contains("RuntimeException"),
        "detail: {}",
        report.refusals[0].detail
    );
}

#[test]
fn a_loop_inside_a_try_refuses_before_the_throw_gate_is_reached() {
    // The guards an enclosing `catch` contributes are deliberately stripped by
    // the purity probe (see `steins-infer/tests/region_purity.rs`, which pins
    // that asymmetry directly). End to end it cannot change a verdict today: a
    // `try` is an opaque construct for the trace, so the walk never reaches the
    // loop head and the subject is unproven — the earlier gate answers first.
    // Recorded as a test rather than left implicit, so the day `try` becomes
    // structured the change of reason is visible.
    let src = "<?php\nfunction boom(int $n): int { throw new RuntimeException('no'); }\nfunction run(): array {\n    $out = [];\n    try {\n        $xs = [1, 2];\n        $out = [];\n        foreach ($xs as $x) {\n            $out[] = boom($x);\n        }\n    } catch (RuntimeException $e) {\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_SUBJECT_NOT_PROVEN_ARRAY);
}

#[test]
fn an_unresolvable_callee_refuses_rather_than_being_assumed_harmless() {
    let src = "<?php\nfunction run(callable $f): array {\n    $xs = [1, 2, 3];\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $f($x);\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_BODY_CALL_UNRESOLVED);
}

#[test]
fn an_unmodelled_construct_refuses_as_an_unresolved_call() {
    // `new` is not on the effect fixpoint (constructor effects and throws are not
    // tracked), so it is read as an unanalyzable target rather than as pure.
    let src = with_proven_subject(
        "    foreach ($xs as $x) {\n        $out[] = new RuntimeException((string) $x);\n    }",
    );
    assert_eq!(refusal_for(&src), REASON_BODY_CALL_UNRESOLVED);
}

#[test]
fn a_declared_only_call_refuses_because_a_cap_is_not_a_proof() {
    // ADR-0067's lane wall at its first transform consumer. The interface
    // envelope caps what `load()` can do; it does not witness that the call does
    // nothing, and a cap cannot witness the equivalence of two evaluation orders.
    let src = "<?php\ninterface Repo {\n    #[\\Steins\\Effect('io')]\n    public function load(int $id): int;\n}\nfunction run(Repo $r): array {\n    $xs = [1, 2, 3];\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $r->load($x);\n    }\n    return $out;\n}\n";
    let report = plan(&[("lib.php", src)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_BODY_CALL_UNRESOLVED);
    assert!(
        report.refusals[0].detail.contains("io"),
        "the detail names the bound: {}",
        report.refusals[0].detail
    );
}

#[test]
fn a_frame_sensitive_builtin_refuses() {
    // `func_get_args()` answers about the *frame* it is written in; moving it
    // into an arrow function changes what it means.
    let src = with_proven_subject(
        "    foreach ($xs as $x) {\n        $out[] = count(func_get_args());\n    }",
    );
    assert_eq!(refusal_for(&src), REASON_BODY_CALL_UNRESOLVED);
}

// ---- 3. Semantic parity gates (ADR-0076 §3) --------------------------------

#[test]
fn a_non_list_subject_refuses() {
    let src = "<?php\nfunction run(): array {\n    $xs = ['a' => 1, 'b' => 2];\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_SUBJECT_NOT_PROVEN_LIST);
}

#[test]
fn a_traversable_subject_refuses() {
    let src = "<?php\nfunction run(iterable $xs): array {\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_SUBJECT_NOT_PROVEN_ARRAY);
}

#[test]
fn a_docblock_asserted_list_is_a_claim_not_a_proof() {
    // ADR-0037's iron rule at the transform's own boundary: an `@param list<int>`
    // is `Asserted`, and an asserted fact never premises a rewrite.
    let src = "<?php\n/** @param list<int> $xs */\nfunction run(array $xs): array {\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_SUBJECT_NOT_PROVEN_ARRAY);
}

#[test]
fn an_iteration_variable_used_after_the_loop_refuses() {
    // `foreach` leaves `$x` bound to the last element; the arrow function's
    // parameter does not escape.
    let src = "<?php\nfunction run(): array {\n    $xs = [1, 2, 3];\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return [$out, $x];\n}\n";
    assert_eq!(refusal_for(src), REASON_ITERATION_VAR_LIVE_AFTER);
}

#[test]
fn a_similarly_named_variable_after_the_loop_does_not_refuse() {
    // The liveness scan matches whole names: `$xs` is not an occurrence of `$x`.
    let src = "<?php\nfunction run(): array {\n    $xs = [1, 2, 3];\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return [$out, $xs];\n}\n";
    let report = plan(&[("lib.php", src)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
}

#[test]
fn the_accumulator_read_in_the_body_refuses() {
    let src = with_proven_subject(
        "    foreach ($xs as $x) {\n        $out[] = count($out) + $x;\n    }",
    );
    assert_eq!(refusal_for(&src), REASON_ACCUMULATOR_READ_IN_BODY);
}

#[test]
fn the_accumulator_bound_as_the_iteration_variable_refuses() {
    // `$out = []; foreach ($xs as $out) { $out[] = 1; }` would make every element
    // clobber the accumulator — the loop observes its own state through the
    // binding, which is still "occurs other than as the append target".
    let src = "<?php\nfunction run(): array {\n    $xs = [1, 2];\n    $out = [];\n    foreach ($xs as $out) {\n        $out[] = 1;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_ACCUMULATOR_READ_IN_BODY);
}

// ---- 4. Shape gates (ADR-0076 §1) ------------------------------------------

#[test]
fn a_key_binding_refuses() {
    let src = with_proven_subject("    foreach ($xs as $k => $x) {\n        $out[] = $x;\n    }");
    assert_eq!(refusal_for(&src), REASON_KEY_BINDING);
}

#[test]
fn a_reference_binding_refuses() {
    let src = with_proven_subject("    foreach ($xs as &$x) {\n        $out[] = $x;\n    }");
    assert_eq!(refusal_for(&src), REASON_REFERENCE_BINDING);
}

#[test]
fn a_destructuring_binding_refuses_with_its_own_reason() {
    // The ADR-0076 §4 taxonomy addition: arrow functions cannot spell a
    // destructuring parameter, and no listed reason describes this shape.
    let src = "<?php\nfunction run(): array {\n    $xs = [[1, 2]];\n    $out = [];\n    foreach ($xs as [$a, $b]) {\n        $out[] = $a;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_VALUE_BINDING_NOT_VARIABLE);
}

#[test]
fn a_non_variable_subject_refuses() {
    let src = "<?php\nfunction run(): array {\n    $out = [];\n    foreach ([1, 2, 3] as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_SUBJECT_NOT_VARIABLE);
}

#[test]
fn a_multi_statement_body_refuses() {
    let src = with_proven_subject(
        "    foreach ($xs as $x) {\n        $y = $x;\n        $out[] = $y;\n    }",
    );
    assert_eq!(refusal_for(&src), REASON_BODY_NOT_SINGLE_APPEND);
}

#[test]
fn an_append_expression_that_writes_a_variable_refuses() {
    // `fn` captures by value, so `$i++` inside the expression would not carry.
    let src = "<?php\nfunction run(): array {\n    $xs = [1, 2];\n    $i = 0;\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $i++ + $x;\n    }\n    return $out;\n}\n";
    let reason = refusal_for(src);
    assert_eq!(reason, REASON_BODY_NOT_SINGLE_APPEND);
}

#[test]
fn an_early_exit_refuses() {
    let src = with_proven_subject(
        "    foreach ($xs as $x) {\n        if ($x > 1) {\n            break;\n        }\n    }",
    );
    assert_eq!(refusal_for(&src), REASON_EARLY_EXIT);
}

// ---- 5. The accumulator's initializer (ADR-0076 §3) ------------------------

#[test]
fn a_non_adjacent_initializer_refuses() {
    let src = "<?php\nfunction run(): array {\n    $out = [];\n    $xs = [1, 2, 3];\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_ACCUMULATOR_INIT_NOT_ADJACENT);
}

#[test]
fn a_comment_in_the_gap_refuses_rather_than_being_eaten() {
    // The rewrite consumes both spans, so anything between them would vanish.
    let src = "<?php\nfunction run(): array {\n    $xs = [1, 2, 3];\n    $out = [];\n    // keep me\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_ACCUMULATOR_INIT_NOT_ADJACENT);
}

#[test]
fn a_non_empty_accumulator_refuses() {
    let src = "<?php\nfunction run(): array {\n    $xs = [1, 2, 3];\n    $out = [0];\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_ACCUMULATOR_NOT_EMPTY);
}

#[test]
fn a_loop_that_opens_its_block_refuses_for_want_of_an_initializer() {
    let src = "<?php\nfunction run(array $xs): array {\n    foreach ($xs as $x) {\n        $out[] = $x;\n    }\n    return $out;\n}\n";
    assert_eq!(refusal_for(src), REASON_ACCUMULATOR_INIT_NOT_ADJACENT);
}
