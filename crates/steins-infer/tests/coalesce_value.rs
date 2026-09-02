//! The `??` chain's **settled short-circuit** (issue #630, commit 1).
//!
//! `eval_coalesce_fact` has ended the spine at a settled arm since ADR-0062 S5,
//! but only ever asked the question of a *projection* arm: the other branch
//! hardcoded `settled = false`. So an arm that is a literal, or a variable proven
//! set and non-null, never settled, and the join went on to add an arm PHP's own
//! evaluation order proves is never evaluated —
//!
//! ```php
//! \PHPStan\dumpType('foo' ?? null);        // 'foo'|null, asserted 'foo'
//! $scalar = 3; \PHPStan\dumpType($scalar ?? 4);  // 3|4, asserted 3
//! ```
//!
//! The predicate that branch wanted already existed and already stood next door:
//! `coalesce_lhs_proven_present` decided exactly this question in the assignment
//! seam, for `mark_dead_span` alone. This slice hands it to the evaluator, which
//! now owns both answers — the value and the deadness — so no seam can name a
//! `??` fact while disagreeing about which arms PHP ran.
//!
//! Nothing here changes the IR: `ArgValue::Coalesce` already lowered, so
//! `SCHEMA_VERSION` does not move.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` body in `src`, in source order, on the pure `check` path.
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check(&tree, &functions, "test.php");
    ds.into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// `dumpType(<expr>);` at file scope — the value position itself.
fn dumped(expr: &str) -> String {
    let src = format!("<?php\n\\PHPStan\\dumpType({expr});\n");
    let ts = types(&src);
    assert_eq!(ts.len(), 1, "expected one dump for `{expr}`, got {ts:?}");
    ts.into_iter().next().expect("one dump")
}

// (i) The witnesses. `binary.php:350` and `isset-coalesce-empty-type.php:433`.

#[test]
fn a_non_null_literal_left_arm_settles_the_chain() {
    assert_eq!(dumped("'foo' ?? null"), "'foo'");
    assert_eq!(dumped("'foo' ?? 'bar'"), "'foo'");
    assert_eq!(dumped("1 ?? 2"), "1");
    assert_eq!(dumped("false ?? 1"), "false", "`??` tests null-ness, not truthiness");
    assert_eq!(dumped("0 ?? 1"), "0", "and `0` is not null either");
}

#[test]
fn a_proven_non_null_variable_settles_the_chain() {
    let src = "<?php\n\
        function f(): void {\n\
            $scalar = 3;\n\
            \\PHPStan\\dumpType($scalar ?? 4);\n\
        }\n";
    assert_eq!(types(src), vec!["3"]);
}

#[test]
fn the_settled_arm_ends_the_whole_spine_not_just_the_next_one() {
    // Right-associative, so this is ONE chain of three arms. Settling at the head
    // must drop both of the others, including an arm the domain cannot spell.
    let src = "<?php\n\
        function f(): void {\n\
            $a = 'x';\n\
            \\PHPStan\\dumpType($a ?? 'y' ?? 'z');\n\
        }\n";
    assert_eq!(types(src), vec!["'x'"]);
}

#[test]
fn an_unspellable_arm_right_of_a_settled_one_no_longer_silences_the_expression() {
    // The partiality this slice reaches through the short-circuit: `parts.push(part?)`
    // still yields `None` for an arm the domain cannot spell, but a settled arm
    // means the loop never asks about the arms behind it.
    let src = "<?php\n\
        function g() {}\n\
        function f(): void {\n\
            $a = 'x';\n\
            \\PHPStan\\dumpType($a ?? g());\n\
        }\n";
    assert_eq!(types(src), vec!["'x'"], "a call arm past a settled one is never consulted");
}

// (ii) The controls. Settling is a proof, and where the proof is absent the join
// stands exactly as it did.

#[test]
fn a_null_left_arm_does_not_settle() {
    assert_eq!(dumped("null ?? 'foo'"), "'foo'");
}

#[test]
fn an_undecided_left_arm_does_not_settle() {
    let src = "<?php\n\
        function f(?string $s): void {\n\
            \\PHPStan\\dumpType($s ?? 'foo');\n\
        }\n";
    assert_eq!(types(src), vec!["string"], "a `?string` is not proven present — the join stands");
}

#[test]
fn an_abstract_but_non_null_left_arm_does_not_settle_either() {
    // `string` is proven non-null, but `coalesce_lhs_proven_present` answers only
    // for an operand that resolves to a CONCRETE value. The join is right anyway
    // here (`clear_null(string) join 'foo'` is `string`), and the refusal is what
    // keeps the predicate one predicate.
    let src = "<?php\n\
        function f(string $s): void {\n\
            \\PHPStan\\dumpType($s ?? 'foo');\n\
        }\n";
    assert_eq!(types(src), vec!["string"]);
}

// (iii) The four seams answer the same `??` the same way (the invariant issues
// #260, #579 and #625 each state and each test).

#[test]
fn the_assignment_seam_and_the_dump_seam_agree() {
    let src = "<?php\n\
        function f(): void {\n\
            $t = 'foo' ?? null;\n\
            \\PHPStan\\dumpType($t);\n\
            \\PHPStan\\dumpType('foo' ?? null);\n\
        }\n";
    let ts = types(src);
    assert_eq!(ts, vec!["'foo'", "'foo'"], "binding and dumping one `??` cannot disagree");
}

#[test]
fn the_dump_seam_settles_a_chain_the_assignment_seam_never_sees() {
    // The dump surface reaches `eval_coalesce_fact` directly (no binding in
    // sight), so this is the settled rule proven on its own seam rather than
    // inherited from `$t = …`.
    let src = "<?php\n\
        function g() {}\n\
        function f(): void {\n\
            \\PHPStan\\dumpType('foo' ?? g());\n\
        }\n";
    assert_eq!(types(src), vec!["'foo'"]);
}
