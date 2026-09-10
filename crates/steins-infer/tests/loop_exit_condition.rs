//! What the statement after a loop knows (issue #651, completing ADR-0027's loop
//! amendments and ADR-0052's narrowing carriers).
//!
//! A loop is left in exactly two ways: its condition went false, or a jump left the
//! body. When the body holds no jump that can, the first is the only way out, so the
//! condition is false at the statement after the loop and its **else**-refinements
//! hold there — the mirror of the entry narrowing, through the same carrier.
//!
//! ```php
//! $x = $node->getAttribute('parent');
//! while ($x !== null) { $x = $x->getAttribute('parent'); }
//! // $x is null here
//! ```
//!
//! Two things make that sound, and both are pinned below. The negation is applied to
//! the **post-forget** env, so it never states iteration 1's value as the exit's: a
//! name the loop rewrote arrives with its lanes gone and can only be told what the
//! failing test proves about the value it holds NOW, and a name the loop did not
//! write arrives with its lanes intact and gives a subtractive negation its base.
//! And the gate is syntactic — read off the CST body, where a `break` of a nested
//! `switch` is the switch's and a `break 2` in the same place is this loop's.
//!
//! The same slice stops the fall-through forgetting a loop's `reads`: a name the
//! body only mentions is rebound by nothing in it, so it survives the loop exactly
//! as it survives the body's entry. That is what made a `foreach` subject unusable
//! after its own loop (issue #652's adversarial review).

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` dump a source produces, as `line: rendered fact`.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == DEBUG_TYPE_ID)
        .map(|d| format!("{}: {}", d.line, d.message))
        .collect()
}

/// The rendered dumps alone, line numbers dropped — the answer, not where it sits.
fn answers(src: &str) -> Vec<String> {
    dumps(src)
        .into_iter()
        .map(|d| d.split_once(": dumped type: ").expect("a dump line").1.to_owned())
        .collect()
}

/// A parent-pointer class world: `getAttribute()` is deliberately untyped, so the
/// only fact about what it returns is the one a guard establishes.
const NODES: &str = "\
class Node { public function getAttribute(string $key) { return null; } }
class ClassMethod extends Node {}
";

/// The motivating traversal with `body` as the loop body, dumping `$x` after it.
/// `$x` enters `unknown` and is rewritten by the body through the same untyped call,
/// so anything the dump answers came from the header's negation and from nowhere else.
fn traversal(body: &str) -> String {
    format!(
        "<?php
{NODES}
function f(Node $node): void {{
    $x = $node->getAttribute('parent');
    while ($x !== null) {{
{body}
        $x = $x->getAttribute('parent');
    }}
    \\PHPStan\\dumpType($x);
}}
"
    )
}

// ---- The negation itself ----------------------------------------------------

#[test]
fn a_break_free_loop_leaves_its_negated_header_on_the_fall_through() {
    // The issue's own shape. `$x` is `unknown` before the loop and rewritten inside
    // it, so the exit's `null` is not a value carried past the construct — it is
    // what the failing test proves about whatever the last iteration left behind.
    assert_eq!(answers(&traversal("")), vec!["null".to_owned()], "the negated header");
}

#[test]
fn the_same_negation_as_an_if_else_answers_the_same_thing() {
    // The `if` twin, hand-unrolled to one iteration. The loop must agree with it,
    // because it is the same carrier applied for the same reason — the condition
    // was evaluated false immediately before the code that reads it.
    let src = format!(
        "<?php
{NODES}
function f(Node $node): void {{
    $x = $node->getAttribute('parent');
    if ($x !== null) {{ $x = $x->getAttribute('parent'); }}
    else {{ \\PHPStan\\dumpType($x); }}
}}
"
    );
    assert_eq!(answers(&src), vec!["null".to_owned()], "the `if`/`else` twin of the shape above");
}

#[test]
fn a_name_the_loop_did_not_write_keeps_the_base_the_negation_subtracts_from() {
    // The other half of applying the negation to the post-forget env: `reads`
    // survives it, so a **subtractive** negation has a lane to subtract from. Both
    // shapes are minimal reproductions — a loop whose body cannot change what its
    // header tests is not code anyone writes — and both are the mechanism exactly.
    let comparison = "<?php
function f(int $i): void {
    while ($i < 10) { echo 'x'; }
    \\PHPStan\\dumpType($i);
}
";
    assert_eq!(
        answers(comparison),
        vec!["int<10, max>".to_owned()],
        "the negated comparison narrows the declared int it did not write"
    );
    let class = format!(
        "<?php
{NODES}
function f(?Node $x): void {{
    while ($x instanceof ClassMethod) {{ echo 'x'; }}
    \\PHPStan\\dumpType($x);
}}
"
    );
    assert_eq!(
        answers(&class),
        vec!["Node|null".to_owned()],
        "the negated `instanceof` subtracts the class from the arms that survived"
    );
}

#[test]
fn a_rewritten_subject_gets_only_what_mints_its_own_fact() {
    // The refusal the post-forget order buys, stated as an answer. A body that
    // rewrites the subject leaves it with no lane, so a subtractive negation has
    // nothing to subtract from and says nothing — where the `!== null` reading above
    // proves `null` outright. Silence, never iteration 1's type.
    let src = format!(
        "<?php
{NODES}
function f(Node $node): void {{
    $x = $node->getAttribute('parent');
    while ($x instanceof ClassMethod) {{ $x = $x->getAttribute('parent'); }}
    \\PHPStan\\dumpType($x);
}}
"
    );
    assert_eq!(answers(&src), vec!["unknown".to_owned()], "no base, so no subtraction");
}

// ---- The gate: what can leave a body ----------------------------------------

#[test]
fn a_break_disables_the_negation_at_any_depth() {
    // A `break` leaves the loop WITHOUT falsifying its header, so the condition
    // need not be false at the fall-through. Nesting it inside an `if` several
    // levels down changes nothing: the gate is a property of the body, not of the
    // path the walk took to it.
    for body in [
        "        break;",
        "        if ($x instanceof ClassMethod) { break; }",
        "        if ($x instanceof ClassMethod) { if ($x !== null) { break; } }",
    ] {
        assert_eq!(
            answers(&traversal(body)),
            vec!["unknown".to_owned()],
            "a `break` in `{body}` must leave the fall-through as it was"
        );
    }
}

#[test]
fn a_break_belonging_to_a_nested_construct_does_not_disqualify() {
    // `break;` inside a nested `switch` targets the SWITCH, and inside a nested loop
    // targets that loop. Neither can leave this one, so neither costs it its
    // negation — which is the whole reason the scan counts levels rather than
    // looking for the keyword.
    for body in [
        "        switch (1) { case 1: break; }",
        "        while (true) { break; }",
        "        foreach ([1] as $ignored) { break; }",
        "        do { break; } while (false);",
    ] {
        assert_eq!(
            answers(&traversal(body)),
            vec!["null".to_owned()],
            "`{body}` breaks its own construct, not this loop"
        );
    }
}

#[test]
fn a_header_that_can_never_fail_makes_the_successor_unreachable() {
    // Keeping `reads` at the fall-through (below) would otherwise carry `$x`'s
    // proven null into code no path reaches: the old `Opaque` washed this out by
    // forgetting `$x`, and `if (true) { return; }` already terminates. A header the
    // entry env decides `Yes` is decided `Yes` at every test, so with no jump to
    // leave by, nothing after the loop runs.
    let dead_after = |header: &str| {
        let src = format!(
            "<?php
final class Foo {{ public function bar(): void {{}} }}
function f(): void {{
    $x = null;
    {header} {{ echo $x; return; }}
    $x->bar();
}}
"
        );
        let tree = SourceTree::parse(&src);
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == "call.on-null").count()
    };
    assert_eq!(dead_after("while (true)"), 0, "`while (true)` with a returning body");
    assert_eq!(dead_after("for (;;)"), 0, "`for (;;)` with a returning body");
    assert_eq!(dead_after("while (1)"), 0, "a truthy literal header");
    assert_eq!(
        dead_after("while (rand() > 0)"),
        1,
        "an undecided header may fail: the successor is live and the finding is true"
    );
}

#[test]
fn a_break_makes_a_never_failing_header_fall_through_again() {
    // The pair is what proves unreachability; `break` alone puts the successor back.
    let src = "<?php
final class Foo { public function bar(): void {} }
function f(): void {
    $x = null;
    while (true) { if (rand() > 0) { break; } return; }
    $x->bar();
}
";
    let tree = SourceTree::parse(src);
    assert_eq!(
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == "call.on-null").count(),
        1,
        "the break leaves without failing the header, so the successor is reached"
    );
}

#[test]
fn a_for_negates_only_the_last_of_its_conditions_at_the_exit() {
    // PHP tests the last comma-separated condition and evaluates the others for
    // their effects, so only the last one's negation is a fact at the fall-through.
    let src = "<?php
function f(int $i, int $j): void {
    for (; $i < 10, $j < 5; ) {
        echo 1;
    }
    \\PHPStan\\dumpType($i);
    \\PHPStan\\dumpType($j);
}
";
    assert_eq!(
        answers(src),
        vec!["int".to_owned(), "int<5, max>".to_owned()],
        "`$i < 10` is evaluated, never tested; `$j < 5` is the header"
    );
}

#[test]
fn a_break_2_that_targets_an_enclosing_switch_does_not_disqualify() {
    // Level counting in the other direction: `break 2` inside a `while` that sits
    // inside a `switch` leaves the switch, which is still inside THIS loop.
    let body = "        switch (1) { case 1: while (true) { break 2; } }";
    assert_eq!(
        answers(&traversal(body)),
        vec!["null".to_owned()],
        "two levels reach the switch, not this loop"
    );
}

#[test]
fn a_break_2_reaches_out_of_a_nested_construct_and_does_disqualify() {
    // The same keyword one level further out. `break 2` inside a nested loop or
    // `switch` leaves THIS loop, so it is this loop's `break` and disables the
    // negation exactly as a bare one at the top level does.
    for body in [
        "        while (true) { break 2; }",
        "        switch (1) { case 1: break 2; }",
        "        foreach ([1] as $ignored) { if ($x !== null) { break 2; } }",
    ] {
        assert_eq!(
            answers(&traversal(body)),
            vec!["unknown".to_owned()],
            "`{body}` leaves this loop"
        );
    }
}

#[test]
fn continue_re_tests_the_header_and_only_an_outer_one_disqualifies() {
    // `continue` jumps to this loop's own condition, so the exit is still a failing
    // test of it and the negation stands. `continue 2` inside a nested loop targets
    // this one — also fine. `continue 2` at the body's top level targets a loop
    // OUTSIDE this one and leaves this body without evaluating this header at all.
    for body in ["        continue;", "        while (true) { continue 2; }"] {
        assert_eq!(
            answers(&traversal(body)),
            vec!["null".to_owned()],
            "`{body}` re-tests this loop's header"
        );
    }
    let outer = format!(
        "<?php
{NODES}
function f(Node $node): void {{
    foreach ([1, 2] as $ignored) {{
        $x = $node->getAttribute('parent');
        while ($x !== null) {{
            continue 2;
        }}
        \\PHPStan\\dumpType($x);
    }}
}}
"
    );
    assert_eq!(
        answers(&outer),
        vec!["unknown".to_owned()],
        "`continue 2` leaves the inner loop without testing its header"
    );
}

#[test]
fn a_goto_anywhere_in_the_body_disqualifies() {
    // A `goto`'s label is unbounded, so the scan cannot prove it stays inside; the
    // worst case is the answer.
    // The label sits BEFORE the loop on purpose: a label statement is a barrier, so
    // one between the loop and the dump would clear the env and hide what the gate
    // decided.
    let src = format!(
        "<?php
{NODES}
function f(Node $node): void {{
    top:
    $x = $node->getAttribute('parent');
    while ($x !== null) {{
        goto top;
    }}
    \\PHPStan\\dumpType($x);
}}
"
    );
    assert_eq!(answers(&src), vec!["unknown".to_owned()], "a `goto` may leave the loop");
}

#[test]
fn a_return_or_a_throw_disqualifies_nothing() {
    // Neither reaches the fall-through, so what holds there is not their business.
    for body in [
        "        if ($x instanceof ClassMethod) { return; }",
        "        if ($x instanceof ClassMethod) { throw new \\RuntimeException(); }",
    ] {
        assert_eq!(
            answers(&traversal(body)),
            vec!["null".to_owned()],
            "`{body}` never arrives at the statement after the loop"
        );
    }
}

// ---- The other loop forms ---------------------------------------------------

#[test]
fn a_do_while_negates_its_header_at_the_exit_and_never_at_the_entry() {
    // The condition IS evaluated immediately before the fall-through, so the exit
    // takes the `while` rule unchanged. The **entry** still refuses it — the first
    // iteration runs before the condition is ever evaluated — which is why the two
    // dumps below disagree about the same variable and the same test.
    let src = format!(
        "<?php
{NODES}
function f(Node $node): void {{
    $x = $node->getAttribute('parent');
    do {{
        \\PHPStan\\dumpType($x);
        $x = $x->getAttribute('parent');
    }} while ($x !== null);
    \\PHPStan\\dumpType($x);
}}
"
    );
    assert_eq!(
        dumps(&src),
        vec!["8: dumped type: unknown".to_owned(), "11: dumped type: null".to_owned()],
        "line 8 is the untested entry; line 11 is the failing test"
    );
}

#[test]
fn a_for_negates_the_condition_php_actually_tests() {
    // A `for`'s tested condition is the LAST one; a `for (;;)` has none at all and
    // carries the literal `true`, whose negation is nothing. The subject is a
    // parameter the loop never writes, so the negation has its base — an induction
    // variable the header owns is in `writes` and is gone. The `for (;;)` needs a
    // `break` to have a live successor at all (see the reachability fixtures).
    let tested = "<?php
function f(int $limit): void {
    for ($i = 0; $limit < 10; $i++) { echo $i; }
    \\PHPStan\\dumpType($limit);
    \\PHPStan\\dumpType($i);
}
";
    assert_eq!(
        answers(tested),
        vec!["int<10, max>".to_owned(), "unknown".to_owned()],
        "the tested condition narrows what the loop did not write, and nothing else"
    );
    let forever = "<?php
function f(int $limit): void {
    for (;;) { if (rand() > 0) { break; } echo $limit; }
    \\PHPStan\\dumpType($limit);
}
";
    assert_eq!(answers(forever), vec!["int".to_owned()], "an absent condition negates to nothing");
}

#[test]
fn a_foreach_has_no_condition_to_negate() {
    // A `foreach` exits on exhaustion, not on a lowered condition, so there is
    // nothing to carry out — with or without a `break`. What it does owe the
    // fall-through is the subject, which the next test pins.
    for body in ["", "break;"] {
        let src = format!(
            "<?php
/** @param list<int> $xs */
function f(array $xs): void {{
    foreach ($xs as $v) {{ {body} }}
    \\PHPStan\\dumpType($v);
}}
"
        );
        assert_eq!(
            answers(&src),
            vec!["unknown".to_owned()],
            "the bound value is a write and the loop states nothing about it afterwards"
        );
    }
}

// ---- The fall-through keeps what the loop cannot rebind ----------------------

#[test]
fn a_foreach_subject_survives_its_own_loop() {
    // Issue #652's adversarial review: the subject is in `reads`, the fall-through
    // used to forget both sets, and so a second `foreach` over the same declared
    // subject bound nothing. Nothing in a loop body can rebind a name in `reads`,
    // so it holds after the loop exactly what it held before — the argument the
    // body's entry has always been given.
    let src = "<?php
/** @param list<int> $xs */
function f(array $xs): void {
    foreach ($xs as $v) {}
    \\PHPStan\\dumpType($xs);
    foreach ($xs as $w) { \\PHPStan\\dumpType($w); }
}
";
    assert_eq!(
        answers(src),
        vec!["list<int> (asserted)".to_owned(), "int (asserted)".to_owned()],
        "the subject, and the second loop's binding that depends on it"
    );
}

#[test]
fn a_name_the_loop_may_write_is_still_forgotten_at_the_fall_through() {
    // The half that does NOT change. `writes` is the by-ref conservatism — every
    // name assigned in the body and every name handed to any call in it — and any
    // of them may hold something else once the loop is over.
    let src = "<?php
/** @param list<int> $xs */
function f(array $xs, string $s): void {
    foreach ($xs as $v) { $s = 1; }
    \\PHPStan\\dumpType($s);
}
";
    assert_eq!(answers(src), vec!["unknown".to_owned()], "a body write survives nothing");
}
