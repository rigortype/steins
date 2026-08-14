//! A comparison as a **value** (issue #260, the operator-value node).
//!
//! Before this, `ArgValue` carried exactly one binary operator (`Concat`), so
//! `1 === 1` in value position lowered to `ArgValue::Other` and rendered
//! `unknown` while `'a' . 'b'` rendered `'ab'`. The gap was never in the
//! semantics: `eval_cmp` has decided `=== !== == != < <= > >=` over candidate
//! value sets since ADR-0031 — it was only ever reachable from *condition*
//! position.
//!
//! So these fixtures pin two things, and deliberately nothing else:
//!
//! 1. **The same decision procedure answers in both positions.** No
//!    comparison semantics are restated here; a verdict this file asserts is
//!    one `if (…)` would already have reached.
//! 2. **The `bool` floor is total.** A PHP comparison evaluates to `bool`
//!    whatever its operands are, so an *undecided* comparison is `bool` — an
//!    honest fact about the operator, not a guess about the operands. This is
//!    why an unknown operand renders `bool` rather than `unknown`.
//! 3. **The stratum split** (owner ruling 2026-08-09, ADR-0052 note): the
//!    three verdicts don't carry the same trust. `Maybe → bool` is the
//!    *operator's* guarantee, consuming no operand refinement, so it is
//!    **Verified always**; `Yes → true` / `No → false` say **which** bool,
//!    resting on the operands, so they keep the operands' `min` stratum. Both
//!    halves are pinned so a later refactor cannot collapse them together.
//!
//! Arithmetic, bitwise and logical operators still widen to `ArgValue::Other`
//! (`unimplemented_operators_still_decline`).

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` body in `src`, in source order, on the pure `check` path
/// (no folder — a comparison is decided without one).
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

// (i) The witness from the #257 probe: what used to be `unknown`.

#[test]
fn literal_comparisons_decide() {
    assert_eq!(dumped("1 == 1"), "true");
    assert_eq!(dumped("1 === 1"), "true");
    assert_eq!(dumped("1 === 2"), "false");
    assert_eq!(dumped("1 !== 2"), "true");
    assert_eq!(dumped("1 != 1"), "false");
    assert_eq!(dumped("1 < 2"), "true");
    assert_eq!(dumped("2 <= 2"), "true");
    assert_eq!(dumped("1 > 2"), "false");
    assert_eq!(dumped("1 >= 2"), "false");
}

#[test]
fn loose_equality_keeps_its_php_8_table() {
    // Not a new table — `php_loose_eq`'s, reached from a new position.
    assert_eq!(dumped("0 == 'php'"), "false");
    assert_eq!(dumped("'1' == 1"), "true");
    assert_eq!(dumped("'abc' == 0"), "false");
    assert_eq!(dumped("null == false"), "true");
    assert_eq!(dumped("null === false"), "false");
    assert_eq!(dumped("true == 'php'"), "true");
    assert_eq!(dumped("[] == false"), "true");
}

// (ii) The operand lanes.

#[test]
fn an_env_bound_operand_decides() {
    // Lowers structurally rather than folding at lowering time: `$n` is an
    // env fact, known only during the walk.
    let src = "<?php\n$n = 5;\n\\PHPStan\\dumpType($n > 3);\n\\PHPStan\\dumpType($n === 6);\n";
    assert_eq!(types(src), vec!["true", "false"]);
}

#[test]
fn a_declared_literal_operand_decides_and_stays_asserted() {
    // Where the corpus's finite operands live: `@param 1 $one` carries no
    // fact, only a declared arm — reading it inherits the declaration's trust
    // rather than laundering it (ADR-0052 §5), which the marker records.
    let src = "<?php\n\
        /**\n\
         * @param 1 $one\n\
         * @param 0 $zero\n\
         */\n\
        function f($one, $zero): void {\n\
            \\PHPStan\\dumpType($one == $zero);\n\
            \\PHPStan\\dumpType($one > $zero);\n\
        }\n";
    assert_eq!(types(src), vec!["false (asserted)", "true (asserted)"]);
}

#[test]
fn a_union_operand_decides_only_when_every_pair_agrees() {
    // ADR-0031 OneOf rule, unchanged: all member pairs agree → that verdict.
    // Stratum split (see module doc): the decided verdict rests on the
    // declared arms (`asserted`); the undecided one is the operator's own
    // guarantee (`Verified`), even though both operands are declared.
    let src = "<?php\n\
        /** @param 1|2 $i */\n\
        function f($i): void {\n\
            \\PHPStan\\dumpType($i === 3);\n\
            \\PHPStan\\dumpType($i === 1);\n\
        }\n";
    assert_eq!(types(src), vec!["false (asserted)", "bool"]);
}

#[test]
fn the_undecided_bool_is_verified_even_from_declared_operands() {
    // The split pinned from the other side: same declared operands, but the
    // decided comparison keeps the declaration's trust and the undecided one does not.
    let src = "<?php\n\
        /**\n\
         * @param 1 $one\n\
         * @param 0|1 $bit\n\
         */\n\
        function f($one, $bit, string $s): void {\n\
            \\PHPStan\\dumpType($one === 1);\n\
            \\PHPStan\\dumpType($bit === 1);\n\
            \\PHPStan\\dumpType($one === $s);\n\
        }\n";
    assert_eq!(types(src), vec!["true (asserted)", "bool", "bool"]);
}

/// Every `type.argument-mismatch` message in `src`, in source order.
fn mismatches(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == "type.argument-mismatch")
        .map(|d| d.message)
        .collect()
}

#[test]
fn a_decided_comparison_over_declared_operands_stays_out_of_the_proof_lane() {
    // The half of the ruling that protects the zero-FP bar: `$b` is decided
    // but from a declared arm, so it's Asserted, and the all-Verified premise
    // rule (ADR-0052 §5) keeps a lying `@param 1 $one` from premising a finding.
    let asserted = "<?php\n\
        function f(\\DateTime $p): void {}\n\
        /** @param 1 $one */\n\
        function g($one): void {\n\
            $b = ($one === 1);\n\
            f($b);\n\
        }\n";
    assert_eq!(mismatches(asserted), Vec::<String>::new());

    // Same shape with a *proven* operand: Verified, and the finding fires. The
    // contrast is the point — the stratum, not the verdict, is what gates it.
    let verified = "<?php\n\
        function f(\\DateTime $p): void {}\n\
        function g(): void {\n\
            $one = 1;\n\
            $b = ($one === 1);\n\
            f($b);\n\
        }\n";
    assert_eq!(mismatches(verified).len(), 1, "{:?}", mismatches(verified));
}

#[test]
fn a_comparison_assigns_its_fact() {
    let src = "<?php\n$b = 1 === 1;\n\\PHPStan\\dumpType($b);\n";
    assert_eq!(types(src), vec!["true"]);
}

#[test]
fn an_assigned_undecided_comparison_binds_bool() {
    let src = "<?php\n\
        function f(int $x): void {\n\
            $b = $x > 3;\n\
            \\PHPStan\\dumpType($b);\n\
        }\n";
    assert_eq!(types(src), vec!["bool"]);
}

// (iii) The floor and the refusals.

#[test]
fn an_undecided_comparison_is_bool_not_unknown() {
    // The one new claim: a comparison's *type* is known even when its value is not.
    let src = "<?php\n\
        function f(int $x, string $s): void {\n\
            \\PHPStan\\dumpType($x > 3);\n\
            \\PHPStan\\dumpType($s == 1);\n\
            \\PHPStan\\dumpType($x === $s);\n\
        }\n";
    assert_eq!(types(src), vec!["bool", "bool", "bool"]);
}

#[test]
fn an_unrepresentable_operand_still_yields_bool() {
    // Neither operand is a value this crate can see; the operator's own guarantee survives.
    let src = "<?php\n\
        function f(object $o): void {\n\
            \\PHPStan\\dumpType($o->a === $o->b->c);\n\
        }\n";
    assert_eq!(types(src), vec!["bool"]);
}

#[test]
fn unimplemented_operators_still_decline() {
    // Certainty discipline: an operator with no value-position evaluation is
    // NOT carried, so it declines rather than claiming an uncomputed type.
    assert_eq!(dumped("1 + 1"), "unknown");
    assert_eq!(dumped("5 & 3"), "unknown");
    assert_eq!(dumped("true && false"), "unknown");
}

#[test]
fn ordering_over_non_numeric_operands_stays_undecided() {
    // `php_num_order` decides only for concrete numeric operands; every other
    // pairing is `Maybe` — which here means `bool`, never a guessed pole.
    assert_eq!(dumped("'a' < 'b'"), "bool");
    assert_eq!(dumped("null < 1"), "bool");
}
