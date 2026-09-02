//! The logical family as a **value** (issue #625, legs 2 and 3): `&& || and or
//! xor !` and `<=>`.
//!
//! The comparison node (#260) and the value-position `isset` (#579) each brought
//! one operator to the value seam with a **total floor** — a type PHP guarantees
//! whatever the operands are, so an undecided one answers that type rather than
//! `unknown`. This file extends the same property to every remaining operator
//! that has one:
//!
//! 1. **The logical connectives are `bool` unconditionally.** PHP has no operator
//!    overloading for `&& || and or xor !` — no extension can make `$a && $b`
//!    return anything else — which is exactly why `bcmath-number.php` asserts
//!    `bool` for `Number || Number` while it asserts `BcMath\Number` for `Number
//!    + Number`.
//! 2. **`<=>` is `int<-1, 1>` unconditionally**, for every operand pairing PHP
//!    admits, arrays and objects included.
//!
//! Every cell asserted here was authored from `php -r` at `PINNED_PHP` 8.5.9
//! (ADR-0061 §4), not recalled. Two probes are load-bearing enough to name:
//!
//! - `(bool) new \BcMath\Number("0")` is **`false`**, and a childless
//!   `SimpleXMLElement` is falsy too. So "an object is truthy" is not a rule this
//!   engine may adopt, and the object rows answer the `bool` floor — which the
//!   corpus itself agrees with, asserting `bool` (not `true`) for `Number ||
//!   Number`.
//! - Every `<=>` spelling probed lands in `-1|0|1`: `[1,2] <=> [1,3]` is `-1`,
//!   `[1,2] <=> [1,2,3]` is `-1`, two fresh `stdClass` instances compare `0`.
//!
//! No falsiness table is written in the engine for any of this: `Fact::truthy`
//! answers each operand and `Certainty::and`/`or`/`not` fold the verdicts.

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

// (i) The measured truth table, decided over literals.

#[test]
fn literal_conjunctions_decide() {
    assert_eq!(dumped("true && true"), "true");
    assert_eq!(dumped("true && false"), "false");
    assert_eq!(dumped("false && true"), "false");
    assert_eq!(dumped("false && false"), "false");
}

#[test]
fn literal_disjunctions_decide() {
    assert_eq!(dumped("true || true"), "true");
    assert_eq!(dumped("true || false"), "true");
    assert_eq!(dumped("false || true"), "true");
    assert_eq!(dumped("false || false"), "false");
}

#[test]
fn the_low_precedence_spellings_are_the_same_operator() {
    // `and`/`or` differ from `&&`/`||` in precedence, not semantics, so they
    // share a `LogicalOp` and must answer identically.
    assert_eq!(dumped("true and false"), "false");
    assert_eq!(dumped("false or true"), "true");
    assert_eq!(dumped("true and true"), "true");
    assert_eq!(dumped("false or false"), "false");
}

#[test]
fn xor_decides_over_literals() {
    // No `Certainty::xor` exists; it is composed as `(a || b) && !(a && b)`,
    // which must reproduce PHP's table exactly.
    assert_eq!(dumped("true xor true"), "false");
    assert_eq!(dumped("true xor false"), "true");
    assert_eq!(dumped("false xor true"), "true");
    assert_eq!(dumped("false xor false"), "false");
}

#[test]
fn negation_decides_over_literals() {
    assert_eq!(dumped("!true"), "false");
    assert_eq!(dumped("!false"), "true");
}

#[test]
fn truthiness_is_phps_own_and_not_a_second_table() {
    // Every one of these is `Fact::truthy` reached from a new position — the
    // same rule `if ($x)` uses, and the same rule the `bool` cast uses.
    assert_eq!(dumped("!'0'"), "true");
    assert_eq!(dumped("!''"), "true");
    assert_eq!(dumped("!'a'"), "false");
    assert_eq!(dumped("!'0.0'"), "false");
    assert_eq!(dumped("!'00'"), "false");
    assert_eq!(dumped("!0"), "true");
    assert_eq!(dumped("!null"), "true");
    assert_eq!(dumped("!0.0"), "true");
    assert_eq!(dumped("![]"), "true");
    assert_eq!(dumped("![0]"), "false");
    assert_eq!(dumped("'0' || false"), "false");
    assert_eq!(dumped("'0.0' && true"), "true");
    assert_eq!(dumped("[0] && true"), "true");
    assert_eq!(dumped("[] || false"), "false");
}

// (ii) The floor — the prize, and total.

#[test]
fn an_undecided_connective_is_bool_not_unknown() {
    let src = "<?php\n\
        function f(bool $b, string $s, int $i): void {\n\
            \\PHPStan\\dumpType($b && $s);\n\
            \\PHPStan\\dumpType($b || $s);\n\
            \\PHPStan\\dumpType($b xor $i);\n\
            \\PHPStan\\dumpType($b and $s);\n\
            \\PHPStan\\dumpType($b or $s);\n\
            \\PHPStan\\dumpType(!$b);\n\
        }\n";
    assert_eq!(types(src), vec!["bool", "bool", "bool", "bool", "bool", "bool"]);
}

#[test]
fn an_unrepresentable_operand_still_yields_bool() {
    // Neither operand is a value this crate can see; the operator's own
    // guarantee survives. This is the whole of the bcmath prize: nothing needs
    // to be known about either side.
    let src = "<?php\n\
        function f(object $o): void {\n\
            \\PHPStan\\dumpType($o->a && $o->b->c);\n\
            \\PHPStan\\dumpType($o->a xor $o->b->c);\n\
            \\PHPStan\\dumpType(!$o->b->c);\n\
        }\n";
    assert_eq!(types(src), vec!["bool", "bool", "bool"]);
}

#[test]
fn an_object_operand_answers_the_floor_and_never_true() {
    // The rung this deliberately does NOT have. `(bool) new \BcMath\Number("0")`
    // is `false` at PINNED_PHP 8.5.9 and a childless `SimpleXMLElement` is falsy
    // too, so "an object is truthy" would be unsound — in exactly the place it
    // was proposed for. `bool` is the honest answer, and the corpus agrees.
    let src = "<?php\n\
        function f(object $o, bool $b): void {\n\
            \\PHPStan\\dumpType($o || $b);\n\
            \\PHPStan\\dumpType($o && $b);\n\
            \\PHPStan\\dumpType(!$o);\n\
        }\n";
    assert_eq!(types(src), vec!["bool", "bool", "bool"]);
}

#[test]
fn one_decided_operand_can_decide_the_whole_connective() {
    // The other half of the same fixture shape, and the reason the bcmath
    // `bcVsNull` rows are winnable without any object knowledge at all: an
    // operand proven falsy decides a `&&` by itself, whatever the other side is.
    let src = "<?php\n\
        function f(object $o): void {\n\
            $n = null;\n\
            \\PHPStan\\dumpType($o && $n);\n\
            \\PHPStan\\dumpType($n && $o);\n\
            \\PHPStan\\dumpType($o || $n);\n\
        }\n";
    assert_eq!(types(src), vec!["false", "false", "bool"]);
}

// (iii) Compositionality — the family folds into itself.

#[test]
fn a_negated_isset_decides() {
    // The row no other slice could reach: issue #579 taught the value seam to
    // answer the inner `isset`, and the `!` around it still widened to `Other`.
    let src = "<?php\n\
        function f(): void {\n\
            $x = 1;\n\
            \\PHPStan\\dumpType(isset($x));\n\
            \\PHPStan\\dumpType(!isset($x));\n\
        }\n";
    assert_eq!(types(src), vec!["true", "false"]);
}

#[test]
fn nested_connectives_and_comparisons_fold() {
    assert_eq!(dumped("(1 === 1) && (2 > 1)"), "true");
    assert_eq!(dumped("(1 === 2) || (2 > 1)"), "true");
    assert_eq!(dumped("!(1 === 1)"), "false");
    assert_eq!(dumped("!!true"), "true");
    assert_eq!(dumped("true && (false || true)"), "true");
}

#[test]
fn an_operator_node_as_a_comparison_operand_resolves_only_where_the_literal_seam_knows_it() {
    // Recorded rather than fixed. `cmp_operand_candidates` resolves an operand
    // through the literal/candidate lane, which knows no operator node at all —
    // so a comparison whose OPERAND is itself an operator expression is
    // undecided, and has been since issue #260 shipped the first one. It is not
    // specific to the carriers this slice adds: the same is true of a nested
    // comparison, which predates them.
    //
    // The floor holds in every case, which is why this is a precision gap and
    // not a soundness one. Closing it means teaching the candidate lane the
    // total-floor evaluators, and two of those (`&&`, `!`) need the walk context
    // the candidate lane does not carry — a seam change, not an operator one.
    // A nested COMPARISON does fold: `resolve_literal_under` has known the
    // comparison node since issue #260, so it resolves as an operand.
    assert_eq!(dumped("(1 === 1) === true"), "true");
    // So does a nested `<=>`, which this slice taught the same seam, sharing one
    // `spaceship_pole` with the fact seam so the two cannot disagree.
    assert_eq!(dumped("(1 <=> 2) === -1"), "true");
    // The connectives do NOT, and follow `isset`'s precedent exactly: their
    // evaluators need the walk context the literal seam does not carry, so they
    // decline there and answer at the fact seam one level up. The floor holds in
    // every case, which is why this is a precision gap and not a soundness one.
    let src = "<?php\n\
        function f(): void {\n\
            $x = 1;\n\
            \\PHPStan\\dumpType(isset($x) === true);\n\
            \\PHPStan\\dumpType((true && true) === true);\n\
            \\PHPStan\\dumpType((!false) === true);\n\
        }\n";
    assert_eq!(types(src), vec!["bool", "bool", "bool"]);
}

// (iv) Every value seam agrees about the same expression.

#[test]
fn the_assignment_seam_agrees_with_the_dump_seam() {
    let src = "<?php\n\
        function f(bool $b): void {\n\
            \\PHPStan\\dumpType($b && false);\n\
            $y = $b && false;\n\
            \\PHPStan\\dumpType($y);\n\
            $z = !$b;\n\
            \\PHPStan\\dumpType($z);\n\
            $w = $b <=> true;\n\
            \\PHPStan\\dumpType($w);\n\
        }\n";
    assert_eq!(types(src), vec!["false", "false", "bool", "int<-1, 1>"]);
}

#[test]
fn the_return_seam_agrees_with_the_assignment_seam() {
    // The callee takes a parameter deliberately: a parameterless project call
    // resolves through the fold path and never reaches the return-exit reader.
    let src = "<?php\n\
        function g(int $z) { return $z === 5 && true; }\n\
        function h(int $z) { return $z <=> 5; }\n\
        function f(): void {\n\
            $v = g(5);\n\
            \\PHPStan\\dumpType($v);\n\
            $w = h(1);\n\
            \\PHPStan\\dumpType($w);\n\
        }\n";
    assert_eq!(types(src), vec!["true", "-1"]);
}

// (v) The spaceship.

#[test]
fn a_decided_spaceship_answers_its_pole() {
    assert_eq!(dumped("5 <=> 3"), "1");
    assert_eq!(dumped("3 <=> 5"), "-1");
    assert_eq!(dumped("1 <=> 1"), "0");
    assert_eq!(dumped("1.5 <=> 1.5"), "0");
    assert_eq!(dumped("2 <=> 1.5"), "1");
}

#[test]
fn an_undecided_spaceship_is_the_three_point_range() {
    // The floor, and the exact rendering the corpus asks for. Note this is a
    // refined `int<-1, 1>`, not a `OneOf` — which would render `-1|0|1` and claim
    // a finite-member layer the operator does not justify.
    let src = "<?php\n\
        function f(int $i, string $s, object $o): void {\n\
            \\PHPStan\\dumpType($i <=> 3);\n\
            \\PHPStan\\dumpType($s <=> $i);\n\
            \\PHPStan\\dumpType($o <=> $o);\n\
        }\n";
    assert_eq!(types(src), vec!["int<-1, 1>", "int<-1, 1>", "int<-1, 1>"]);
}

#[test]
fn string_ordering_stays_at_the_spaceship_floor() {
    // The decided arm is `eval_cmp` asked twice, so it inherits that procedure's
    // declines as well as its rules: `php_num_order` decides only for concrete
    // numeric operands. Widening string ordering is a comparison-family change,
    // not a spaceship one — recorded here so a later slice knows this row is
    // waiting rather than wrong.
    assert_eq!(dumped("'foo' <=> 'bar'"), "int<-1, 1>");
    assert_eq!(dumped("'1' <=> 'a'"), "int<-1, 1>");
}

#[test]
fn the_spaceship_is_never_decided_by_subtraction() {
    // ADR-0028 §3's engine-int-width trap, pinned from the outside: operands
    // whose difference overflows a 64-bit int must still answer, and must answer
    // the pole rather than a wrapped number.
    assert_eq!(dumped("PHP_INT_MIN <=> PHP_INT_MAX"), "int<-1, 1>");
    assert_eq!(dumped("-9223372036854775807 <=> 9223372036854775807"), "-1");
}
