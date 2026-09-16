//! A ternary as a **value at every seam** (issue #625, leg 1).
//!
//! `eval_ternary_fact` has existed since ADR-0031 and has always been right: it
//! evaluates the guard, marks the untaken arm proven-unevaluated (ADR-0052 §6),
//! threads each arm's refinement env, and joins. What it was not, was reachable.
//! It had exactly one caller — the assignment seam — so `$t = true ? 1 : 2;`
//! bound `1` while `dumpType(true ? 1 : 2)` on the very next line answered
//! `unknown`, and `return $c ? A : B;` crossed a `return` carrying nothing.
//!
//! That asymmetry is what these fixtures pin, and nothing else. No ternary
//! semantics are restated here; every verdict asserted below is one the
//! assignment seam already reached before this slice. What is new is that the
//! dump surface, the return-exit reader and the assignment ladder now answer the
//! same expression the same way — the invariant issue #260 and issue #579 both
//! state and both test.
//!
//! Leg 1 changes no IR: `ArgValue::Ternary` already lowered, so nothing here
//! moves `SCHEMA_VERSION`.

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

// (i) The witness: `binary.php:344`, an expression whose every part is a literal.

#[test]
fn a_decided_ternary_dumps_its_taken_arm() {
    assert_eq!(dumped("true ? 1 : 2"), "1");
    assert_eq!(dumped("false ? 1 : 2"), "2");
    assert_eq!(dumped("1 === 1 ? 'a' : 'b'"), "'a'");
    assert_eq!(dumped("1 === 2 ? 'a' : 'b'"), "'b'");
}

#[test]
fn an_undecided_ternary_dumps_the_join_of_both_arms() {
    let src = "<?php\n\
        function f(int $x): void {\n\
            \\PHPStan\\dumpType($x > 3 ? 1 : 2);\n\
        }\n";
    assert_eq!(types(src), vec!["1|2"]);
}

// (ii) The invariant: every seam agrees about the same expression.

#[test]
fn the_dump_seam_agrees_with_the_assignment_seam() {
    // The exact asymmetry that named this leg: before it, the first line
    // answered `unknown` and the second `1`.
    let src = "<?php\n\
        \\PHPStan\\dumpType(true ? 1 : 2);\n\
        $t = true ? 1 : 2;\n\
        \\PHPStan\\dumpType($t);\n";
    let ts = types(src);
    assert_eq!(ts, vec!["1", "1"]);
}

#[test]
fn the_return_seam_agrees_with_the_assignment_seam() {
    // A returned ternary crosses the `return` with the fact `$x = <rvalue>`
    // binds (issue #590's rule), so the caller's binding sees the taken arm.
    //
    // Two shapes of the fixture are load-bearing, both learned from a probe
    // rather than assumed. No declared return type: a declared `int` floors the
    // exit to its own arm and hides which reader produced the answer. And the
    // callee takes a parameter: a PARAMETERLESS project call resolves through
    // the fold path instead of the binding descent, so it never reaches
    // `return_value_fact` at all and would test nothing here.
    let src = "<?php\n\
        function g(int $z) { return $z === 5 ? 'a' : 'b'; }\n\
        function f(): void {\n\
            $v = g(5);\n\
            \\PHPStan\\dumpType($v);\n\
        }\n";
    assert_eq!(types(src), vec!["'a'"]);
}

// (iii) The arm envs thread, at the new seams too.

#[test]
fn each_arm_sees_the_guards_refinement_at_the_dump_seam() {
    // `$x === 5 ? $x : 0` — the then-arm reads `$x` under the guard's own
    // refinement, so it is `5`, and the join with `0` is finite.
    let src = "<?php\n\
        function f(int $x): void {\n\
            \\PHPStan\\dumpType($x === 5 ? $x : 0);\n\
        }\n";
    assert_eq!(types(src), vec!["0|5"]);
}

// (iv) The floor this leg does NOT claim.

#[test]
fn an_arm_with_no_fact_still_declines() {
    // A ternary has no total floor the way a comparison does — its type is its
    // arms' — so an arm the value domain cannot spell drops the whole answer.
    // That is the unchanged ADR-0031 behaviour, reached from a new seam.
    let src = "<?php\n\
        function f(object $o, bool $b): void {\n\
            \\PHPStan\\dumpType($b ? $o->a : $o->b);\n\
        }\n";
    assert_eq!(types(src), vec!["unknown"]);
}

#[test]
fn a_short_ternary_still_declines() {
    // `?:` does not lower (`lower_arg_value` requires `cond.then`), so it stays
    // `ArgValue::Other` and this rung never sees it. Leg 4's territory.
    let src = "<?php\n\
        function f(string $s): void {\n\
            \\PHPStan\\dumpType($s ?: 12);\n\
        }\n";
    assert_eq!(types(src), vec!["unknown"]);
}
