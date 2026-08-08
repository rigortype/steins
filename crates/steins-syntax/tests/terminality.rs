//! The reachability foundation itself (ADR-0078 §5, issue #199): `Stmt::end` and
//! the [`body_end`] fold, pinned as the **three-valued** judgment they are.
//!
//! `crates/steins-infer/tests/return_missing.rs` exercises the same foundation
//! through `type.return-missing`, but that consumer collapses `Terminates` and
//! `Unknown` into one observable outcome — silence. This file is where the two
//! are told apart, because the deferred dead-code consumer reads them the
//! opposite way round: it may report only on `Terminates`, so a construct that
//! answers `Terminates` when it should answer `Unknown` is a false "this code is
//! unreachable" waiting to happen, and no test on the tracer would ever see it.

use steins_syntax::{BodyEnd, ScopeOwner, SourceTree, body_end};

/// The [`body_end`] verdict for the body of `function f`, parsed from `body`.
fn end_of(body: &str) -> BodyEnd {
    let src = format!("<?php\nfunction f() {{\n{body}\n}}\n");
    let tree = SourceTree::parse(&src);
    let scope = tree
        .scopes()
        .iter()
        .find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f"))
        .expect("the function scope");
    body_end(&scope.stmts)
}

// ---------------------------------------------------------------------------
// Terminates — proven to have no edge to the successor.
// ---------------------------------------------------------------------------

#[test]
fn terminators_terminate() {
    assert_eq!(end_of("return 1;"), BodyEnd::Terminates);
    assert_eq!(end_of("throw new RuntimeException();"), BodyEnd::Terminates);
    assert_eq!(end_of("exit;"), BodyEnd::Terminates);
    assert_eq!(end_of("exit(1);"), BodyEnd::Terminates);
    assert_eq!(end_of("die('x');"), BodyEnd::Terminates);
}

#[test]
fn an_if_terminates_only_when_every_arm_does() {
    assert_eq!(end_of("if ($c) { return 1; } else { return 2; }"), BodyEnd::Terminates);
    assert_eq!(
        end_of("if ($c) { return 1; } elseif ($d) { return 2; } else { throw new E(); }"),
        BodyEnd::Terminates
    );
    // The implicit empty `else` is a terminator-free path.
    assert_eq!(end_of("if ($c) { return 1; }"), BodyEnd::FallsThrough);
    assert_eq!(end_of("if ($c) { return 1; } else { $x = 2; }"), BodyEnd::FallsThrough);
}

#[test]
fn a_literal_condition_is_not_a_branch() {
    // `if (true)` has no no-branch path to add, so the arm alone decides — this is
    // the one place a condition is read, and it exists to keep the tracer off a
    // function that demonstrably returns.
    assert_eq!(end_of("if (true) { return 1; }"), BodyEnd::Terminates);
    assert_eq!(end_of("if (1) { return 1; }"), BodyEnd::Terminates);
    // A literal-false arm contributes no path at all.
    assert_eq!(end_of("if (false) { $x = 1; } else { return 2; }"), BodyEnd::Terminates);
    assert_eq!(end_of("if (false) { return 1; }"), BodyEnd::FallsThrough);
    assert_eq!(end_of("if (false) { $x = 1; } elseif (true) { return 2; }"), BodyEnd::Terminates);
}

#[test]
fn an_unconditional_loop_with_no_break_terminates() {
    assert_eq!(end_of("while (true) { $x = 1; }"), BodyEnd::Terminates);
    assert_eq!(end_of("while (1) { $x = 1; }"), BodyEnd::Terminates);
    assert_eq!(end_of("for (;;) { $x = 1; }"), BodyEnd::Terminates);
    assert_eq!(end_of("do { $x = 1; } while (true);"), BodyEnd::Terminates);
}

#[test]
fn a_match_with_no_default_counts_its_unhandled_throw() {
    // PHP throws `\UnhandledMatchError` on no match, so the implicit no-match arm
    // is itself a terminator — every arm terminating proves the whole terminal.
    assert_eq!(
        end_of("match ($x) { 1 => throw new A(), 2 => throw new B() };"),
        BodyEnd::Terminates
    );
    // With a `default` the implicit arm is gone and the default decides.
    assert_eq!(end_of("match ($x) { 1 => throw new A(), default => f() };"), BodyEnd::FallsThrough);
}

#[test]
fn the_first_terminator_wins_over_a_later_tail() {
    // The fold is not "the last statement decides".
    assert_eq!(end_of("return 1;\n$x = 2;"), BodyEnd::Terminates);
    // …and an undecided statement does not stop a later proven terminator.
    assert_eq!(
        end_of("try { $x = g(); } catch (Throwable $e) { $x = 0; }\nreturn $x;"),
        BodyEnd::Terminates
    );
}

// ---------------------------------------------------------------------------
// FallsThrough — a terminator-free syntactic path to the end exists.
// ---------------------------------------------------------------------------

#[test]
fn straight_line_code_falls_through() {
    assert_eq!(end_of(""), BodyEnd::FallsThrough);
    assert_eq!(end_of("$x = 1;"), BodyEnd::FallsThrough);
    assert_eq!(end_of("g();"), BodyEnd::FallsThrough);
    assert_eq!(end_of("echo $x;"), BodyEnd::FallsThrough);
    assert_eq!(end_of("global $x;"), BodyEnd::FallsThrough);
    assert_eq!(end_of("static $x = 1;"), BodyEnd::FallsThrough);
    assert_eq!(end_of("unset($x);"), BodyEnd::FallsThrough);
}

#[test]
fn a_bounded_loop_falls_through_whatever_its_body_does() {
    // The loop may run zero times, so its exit edge exists regardless.
    assert_eq!(end_of("foreach ($xs as $x) { return $x; }"), BodyEnd::FallsThrough);
    assert_eq!(end_of("while ($c) { return 1; }"), BodyEnd::FallsThrough);
    assert_eq!(end_of("for ($i = 0; $i < 3; $i++) { return $i; }"), BodyEnd::FallsThrough);
}

#[test]
fn a_switch_with_no_default_falls_through() {
    assert_eq!(
        end_of("switch ($x) { case 1: return 1; case 2: return 2; }"),
        BodyEnd::FallsThrough,
        "no default — the no-match path reaches the successor"
    );
}

// ---------------------------------------------------------------------------
// Unknown — the exit edges are not bounded. THIS is the class the tracer cannot
// tell apart from `Terminates`, and the one a dead-code consumer must not
// mistake for it.
// ---------------------------------------------------------------------------

#[test]
fn a_try_is_undecided_whole() {
    // `finally` overwrites the exit point (`try { return 1; } finally { return 2; }`
    // is 2 on 8.5.9), so neither direction is readable off the block ends.
    assert_eq!(end_of("try { return 1; } catch (Throwable $e) { return 0; }"), BodyEnd::Unknown);
    assert_eq!(end_of("try { $x = 1; } finally { $y = 2; }"), BodyEnd::Unknown);
    assert_eq!(end_of("try { return 1; } finally { return 2; }"), BodyEnd::Unknown);
}

#[test]
fn a_goto_or_label_is_undecided() {
    assert_eq!(end_of("goto done;\ndone:\n$x = 1;"), BodyEnd::Unknown);
}

#[test]
fn an_unconditional_loop_containing_a_break_is_undecided() {
    // The break may belong to a nested `switch` or loop, so whether THIS loop has
    // an exit edge is not decided here.
    assert_eq!(end_of("while (true) { if ($c) { break; } }"), BodyEnd::Unknown);
    assert_eq!(end_of("for (;;) { break; }"), BodyEnd::Unknown);
}

#[test]
fn a_switch_containing_a_break_is_undecided_not_terminal() {
    // The regression this test exists for: every case ends in `break`, and a
    // `break` in isolation terminates the list it sits in — so a naive join would
    // call the whole switch terminal and a dead-code consumer would report
    // everything after it unreachable. It is FallsThrough in truth and `Unknown`
    // here, which is the safe answer for both consumers.
    assert_eq!(
        end_of("switch ($x) { case 1: $y = 1; break; default: $y = 2; break; }"),
        BodyEnd::Unknown
    );
    assert_eq!(
        end_of("switch ($x) { case 1: if ($c) { break; } return 1; default: return 2; }"),
        BodyEnd::Unknown
    );
}

#[test]
fn a_switch_with_case_to_case_fall_through_is_undecided() {
    assert_eq!(end_of("switch ($x) { case 1: $y = 1; default: return 2; }"), BodyEnd::Unknown);
}

#[test]
fn an_undecided_statement_with_no_later_terminator_leaves_the_list_undecided() {
    assert_eq!(end_of("try { $x = 1; } finally { $y = 2; }\n$z = 3;"), BodyEnd::Unknown);
}

// ---------------------------------------------------------------------------
// The two predicates, and why both exist.
// ---------------------------------------------------------------------------

#[test]
fn the_two_predicates_are_not_negations_of_each_other() {
    for end in [BodyEnd::Terminates, BodyEnd::FallsThrough, BodyEnd::Unknown] {
        // `Unknown` answers `false` to BOTH — which is the whole point: each
        // consumer's accusation needs a positive proof, and neither may reach for
        // the other's negation.
        assert!(!(end.provably_terminates() && end.provably_falls_through()));
    }
    assert!(!BodyEnd::Unknown.provably_terminates());
    assert!(!BodyEnd::Unknown.provably_falls_through());
    assert!(BodyEnd::Terminates.provably_terminates());
    assert!(BodyEnd::FallsThrough.provably_falls_through());
}

#[test]
fn the_arm_join_is_the_documented_lattice() {
    use BodyEnd::{FallsThrough, Terminates, Unknown};
    assert_eq!(BodyEnd::join_arms([]), Terminates, "the identity: no arms, no path");
    assert_eq!(BodyEnd::join_arms([Terminates, Terminates]), Terminates);
    assert_eq!(BodyEnd::join_arms([Terminates, FallsThrough]), FallsThrough);
    assert_eq!(BodyEnd::join_arms([Terminates, Unknown]), Unknown);
    // A provably terminator-free arm decides the construct whatever the rest do.
    assert_eq!(BodyEnd::join_arms([Unknown, FallsThrough]), FallsThrough);
}
