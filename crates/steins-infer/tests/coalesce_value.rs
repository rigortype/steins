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

// (iv) A projection arm over an order-witnessed array VALUE (issue #630, commit
// 2). A fully literal array binds `Fact::Singleton(Val::Array)`, never
// `Fact::Shape`, so the projection arm used to decline on the base test alone —
// while `isset()` on the very same binding answered from the literal directly.

#[test]
fn a_literal_array_base_answers_a_projection_arm() {
    // `isset-coalesce-empty-type.php:437`. Measured at `PINNED_PHP` 8.5.9:
    //
    // ```
    // php -r '$a=[1,2,3]; var_dump($a["string"] ?? 0);'
    // int(0)
    // ```
    let src = "<?php\n\
        function f(): void {\n\
            $array = [1, 2, 3];\n\
            \\PHPStan\\dumpType($array['string'] ?? 0);\n\
            \\PHPStan\\dumpType($array[0] ?? 9);\n\
        }\n";
    assert_eq!(types(src), vec!["0", "1"]);
}

#[test]
fn a_present_literal_key_settles_the_chain() {
    // The lifted base makes the same settled test available: `$a[0]` is present and
    // non-null, so nothing behind it is evaluated — including an arm the domain
    // cannot spell.
    let src = "<?php\n\
        function g() {}\n\
        function f(): void {\n\
            $array = ['k' => 'v'];\n\
            \\PHPStan\\dumpType($array['k'] ?? g());\n\
        }\n";
    assert_eq!(types(src), vec!["'v'"]);
}

#[test]
fn a_proven_absent_arm_falls_through_instead_of_silencing_the_chain() {
    // The partiality fix proper. `DeclaredAbsent` is a proof, not an absence of
    // knowledge: PHP skips the arm and evaluates the next, so the arm contributes no
    // fact and the chain goes on. Before, `taken_fact()`'s `None` for a proven
    // absence was read as "the domain cannot spell this" and killed the expression.
    let src = "<?php\n\
        function f(): void {\n\
            $empty = [];\n\
            \\PHPStan\\dumpType($empty['nope'] ?? 'fallback');\n\
        }\n";
    assert_eq!(types(src), vec!["'fallback'"]);
}

#[test]
fn a_proven_absent_arm_still_lets_the_arms_behind_it_join() {
    let src = "<?php\n\
        function f(bool $b): void {\n\
            $a = ['k' => 1];\n\
            $t = $b ? 'x' : 'y';\n\
            \\PHPStan\\dumpType($a['nope'] ?? $t);\n\
        }\n";
    assert_eq!(types(src), vec!["'x'|'y'"], "the absent arm adds nothing and hides nothing");
}

#[test]
fn a_nested_literal_array_slot_is_read_as_a_value_not_a_shape() {
    // `isset-coalesce-empty-type.php:441`: the base is a list of lists, and the
    // lift makes each entry a `Singleton` slot — strictly more precise than a
    // nested shape, which is why the miss on `'string'` still reads as absent.
    let src = "<?php\n\
        function f(): void {\n\
            $multiDimArray = [[1], [2], [3]];\n\
            \\PHPStan\\dumpType($multiDimArray['string'] ?? 0);\n\
        }\n";
    assert_eq!(types(src), vec!["0"]);
}

#[test]
fn an_unsealed_base_still_declines() {
    // The control the lift must not swallow: a base whose keys were NOT observed
    // proves nothing about a missing one, so the arm cannot fall through and the
    // expression stays silent rather than answering the right arm.
    let src = "<?php\n\
        function f(array $a): void {\n\
            \\PHPStan\\dumpType($a['nope'] ?? 'fallback');\n\
        }\n";
    assert_eq!(types(src), vec!["unknown"], "an unobserved base proves no absence");
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
