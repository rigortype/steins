//! `type.return-missing` (ADR-0078, issue #199) and the reachability foundation
//! it traces: a function-like that declares a native non-void return type and
//! whose body **provably falls through** to the closing brace.
//!
//! PHP's own consequence, `php -r`-witnessed on 8.5.9 — a fatal `TypeError`
//! raised when control reaches the end, not at declaration time:
//!
//! ```text
//! TypeError: f(): Return value must be of type int, none returned
//! TypeError: A::m(): Return value must be of type int, none returned
//! TypeError: {closure:Command line code:1}(): Return value must be of type int, none returned
//! ```
//!
//! # The asymmetry these tests pin
//!
//! `BodyEnd::Unknown` — a body whose exit edges the judgment cannot bound — is
//! **terminating** for this consumer: silence. A future dead-code consumer must
//! read the very same `Unknown` the other way (not terminal, so never report a
//! statement dead). Every silence leg below says which of the two reasons it is
//! silent for: *proven to terminate* or *undecided, and undecided means silence
//! here*. That distinction is the point of the file — a leg that goes silent for
//! the wrong reason is a bug this suite is meant to catch.
//!
//! No sidecar, no env, no folder: both premises are declaration-and-shape facts,
//! so every fixture uses the sound-subset [`NoFold`].

use steins_infer::{Diagnostic, NoFold, TYPE_RETURN_MISSING_ID, check_full};
use steins_syntax::SourceTree;

fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == TYPE_RETURN_MISSING_ID)
        .collect()
}

fn assert_silent(src: &str, why: &str) {
    let d = diags(src);
    assert!(d.is_empty(), "expected silence ({why}), got {d:#?}");
}

// ---------------------------------------------------------------------------
// Firing: bodies whose fall-through the foundation proves.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_plain_fall_through() {
    let d = diags(
        "<?php
function f(): int {
    $x = 1;
}
",
    );
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 2, "reported at the declaration: {d:#?}");
    assert!(d[0].message.contains("function f"), "{}", d[0].message);
    assert!(
        d[0].message.contains("Return value must be of type int, none returned"),
        "the witnessed PHP sentence: {}",
        d[0].message
    );
}

#[test]
fn fires_on_empty_body() {
    let d = diags("<?php\nfunction f(): int {\n}\n");
    assert_eq!(d.len(), 1, "an empty statement list falls through: {d:#?}");
}

#[test]
fn fires_on_if_without_else() {
    // The implicit empty `else` IS a terminator-free path to the closing brace.
    let d = diags(
        "<?php
function f(): int {
    if ($c) {
        return 1;
    }
}
",
    );
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 2, "{d:#?}");
}

#[test]
fn fires_when_only_one_arm_returns() {
    let d = diags(
        "<?php
function f(): int {
    if ($c) {
        return 1;
    } else {
        $x = 2;
    }
}
",
    );
    assert_eq!(d.len(), 1, "the else arm falls through: {d:#?}");
}

#[test]
fn fires_when_an_elseif_chain_leaves_a_hole() {
    let d = diags(
        "<?php
function f(): int {
    if ($c) {
        return 1;
    } elseif ($d) {
        return 2;
    }
}
",
    );
    assert_eq!(d.len(), 1, "no else — the no-branch path reaches the end: {d:#?}");
}

#[test]
fn fires_on_loop_then_nothing() {
    // A `foreach` always has an exit edge (the iteration exhausts), so the body
    // provably falls through even though the loop body itself is opaque.
    let d = diags(
        "<?php
function f(): int {
    foreach ($xs as $x) {
        echo $x;
    }
}
",
    );
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_conditional_while_then_nothing() {
    // `while ($c)` can exit on a false condition — an exit edge, so FallsThrough.
    let d = diags(
        "<?php
function f(): int {
    while ($c) {
        $x = 1;
    }
}
",
    );
    assert_eq!(d.len(), 1, "{d:#?}");
}

#[test]
fn fires_on_a_method() {
    let d = diags(
        "<?php
class A {
    public function m(): string {
        $x = 1;
    }
}
",
    );
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("A::m"), "{}", d[0].message);
    assert!(
        d[0].message.contains("Return value must be of type string, none returned"),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_a_closure() {
    // Witnessed: a closure body falls off the same fatal, named `{closure:…}()`.
    let d = diags(
        "<?php
$f = function (): int {
    $x = 1;
};
",
    );
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("closure"), "{}", d[0].message);
}

#[test]
fn fires_on_a_nullable_return_type() {
    // `?int` is not optional: PHP demands an explicit `return null;`.
    let d = diags("<?php\nfunction f(): ?int {\n    $x = 1;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains("Return value must be of type ?int, none returned"),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_a_union_return_type() {
    let d = diags("<?php\nfunction f(): int|string {\n    $x = 1;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("int|string"), "{}", d[0].message);
}

#[test]
fn fires_on_types_that_lower_to_no_native_type() {
    // `: array` / `: mixed` lower to no `NativeType` at all, yet both fatal
    // identically — which is why the premise reads the RAW hint.
    for ty in ["array", "mixed"] {
        let src = format!("<?php\nfunction f(): {ty} {{\n    $x = 1;\n}}\n");
        let d = diags(&src);
        assert_eq!(d.len(), 1, "{ty}: {d:#?}");
        assert!(d[0].message.contains(ty), "{ty}: {}", d[0].message);
    }
}

#[test]
fn fires_after_a_match_statement_that_is_not_exhaustively_terminal() {
    // A `match` with a `default` whose arms do not all terminate falls through.
    let d = diags(
        "<?php
function f(): int {
    match ($x) {
        1 => foo(),
        default => bar(),
    };
}
",
    );
    assert_eq!(d.len(), 1, "{d:#?}");
}

// ---------------------------------------------------------------------------
// Silence, leg 1: the body is PROVEN to terminate.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_a_trailing_return() {
    assert_silent("<?php\nfunction f(): int {\n    return 1;\n}\n", "proven: the body returns");
}

#[test]
fn silent_when_both_arms_return() {
    assert_silent(
        "<?php
function f(): int {
    if ($c) {
        return 1;
    } else {
        return 2;
    }
}
",
        "proven: every arm of the `if` terminates, so the join terminates",
    );
}

#[test]
fn silent_when_an_elseif_chain_is_closed_by_an_else() {
    assert_silent(
        "<?php
function f(): int {
    if ($c) {
        return 1;
    } elseif ($d) {
        return 2;
    } else {
        return 3;
    }
}
",
        "proven: every arm terminates and the `else` closes the join",
    );
}

#[test]
fn silent_on_a_trailing_throw() {
    assert_silent(
        "<?php\nfunction f(): int {\n    throw new RuntimeException('no');\n}\n",
        "proven: a `throw` has no edge to the successor",
    );
}

#[test]
fn silent_on_a_trailing_exit() {
    // `exit;` / `die;` surface as a real trace terminator (`StmtKind::Exit`), which
    // is what makes this a *proof* of termination rather than an undecided case.
    assert_silent(
        "<?php\nfunction f(): int {\n    exit;\n}\n",
        "proven: `exit` never returns to the caller",
    );
    assert_silent(
        "<?php\nfunction f(): int {\n    exit(1);\n}\n",
        "proven: `exit(1)` is the same terminator",
    );
    assert_silent("<?php\nfunction f(): int {\n    die('x');\n}\n", "proven: `die` likewise");
}

#[test]
fn silent_on_an_unconditional_infinite_loop() {
    // `while (true)` with no `break` has NO exit edge, so the body terminates —
    // this is a proof leg, not an undecided one. Witnessed: PHP accepts
    // `function f(): int { while (true) {} }` and never reaches the TypeError.
    assert_silent(
        "<?php
function f(): int {
    while (true) {
        $x = 1;
    }
}
",
        "proven: `while (true)` with no break has no exit edge",
    );
    assert_silent(
        "<?php
function f(): int {
    for (;;) {
        $x = 1;
    }
}
",
        "proven: `for (;;)` likewise",
    );
    assert_silent(
        "<?php
function f(): int {
    do {
        $x = 1;
    } while (true);
}
",
        "proven: `do … while (true)` likewise",
    );
}

#[test]
fn silent_on_a_match_statement_whose_every_arm_terminates() {
    // No `default`: PHP throws `\UnhandledMatchError` on no match, so the implicit
    // no-match arm is a terminator too. Every arm terminating therefore proves the
    // whole construct terminal.
    assert_silent(
        "<?php
function f(): int {
    match ($x) {
        1 => throw new LogicException(),
        2 => throw new RuntimeException(),
    };
}
",
        "proven: every match arm throws and the implicit no-match arm throws too",
    );
}

#[test]
fn silent_on_a_switch_whose_every_case_returns_under_a_default() {
    assert_silent(
        "<?php
function f(): int {
    switch ($x) {
        case 1:
            return 1;
        default:
            return 2;
    }
}
",
        "proven: every case terminates and the `default` closes the join",
    );
}

#[test]
fn silent_on_a_call_to_a_never_returning_callee() {
    // Witnessed: `function g(): never { exit(1); } function f(): int { g(); }` runs
    // clean — control never reaches `f`'s closing brace.
    assert_silent(
        "<?php
function g(): never {
    exit(1);
}
function f(): int {
    g();
}
",
        "proven-enough: the callee declares `: never`, so the call has no return edge",
    );
}

#[test]
fn silent_on_a_terminating_body_after_an_undecided_statement() {
    // The list fold is not "the last statement decides": the first proven
    // terminator wins outright, so a `try` earlier in the body does not infect it.
    assert_silent(
        "<?php
function f(): int {
    try {
        $x = g();
    } catch (Throwable $e) {
        $x = 0;
    }
    return $x;
}
",
        "proven: the trailing `return` terminates however the `try` resolves",
    );
}

// ---------------------------------------------------------------------------
// Silence, leg 2: the body is UNDECIDED — and undecided means silence *here*.
// A dead-code consumer must read every one of these the other way round.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_a_try_catch_tail() {
    // The recorded exclusion. `finally` OVERWRITES the exit point — witnessed on
    // 8.5.9, `try { return 1; } finally { return 2; }` evaluates to 2, and a
    // returning `finally` swallows an in-flight exception — so neither direction
    // can be read off the block ends. Undecided ⇒ silence for THIS id; a dead-code
    // consumer must not call the statement after it unreachable either.
    assert_silent(
        "<?php
function f(): int {
    try {
        return g();
    } catch (Throwable $e) {
        return 0;
    }
}
",
        "undecided: `try` is excluded whole, and undecided is silence for this id",
    );
}

#[test]
fn silent_on_a_try_finally_tail() {
    assert_silent(
        "<?php
function f(): int {
    try {
        $x = 1;
    } finally {
        $y = 2;
    }
}
",
        "undecided: the excluded-`finally` shape, pinned as silence with its reason",
    );
}

#[test]
fn silent_on_a_goto() {
    assert_silent(
        "<?php
function f(): int {
    goto done;
    done:
    $x = 1;
}
",
        "undecided: a `goto`/label pair is an unbounded jump, so silence",
    );
}

#[test]
fn silent_on_an_infinite_loop_containing_a_break() {
    // `while (true)` WITH a `break` somewhere inside: the break may belong to a
    // nested `switch` or loop, so whether this loop has an exit edge is undecided.
    assert_silent(
        "<?php
function f(): int {
    while (true) {
        if ($c) {
            break;
        }
    }
}
",
        "undecided: the break's target is not resolved by the judgment",
    );
}

#[test]
fn silent_on_a_switch_with_case_fall_through() {
    assert_silent(
        "<?php
function f(): int {
    switch ($x) {
        case 1:
            $y = 1;
        default:
            return 2;
    }
}
",
        "undecided: a case body running into the next case is not modelled",
    );
}

#[test]
fn silent_on_an_include() {
    // Included code can `exit` the whole script, so the fall-through path has an
    // exit this judgment cannot see.
    assert_silent(
        "<?php\nfunction f(): int {\n    include 'x.php';\n}\n",
        "undecided: `include` brings in code that can terminate the script",
    );
}

// ---------------------------------------------------------------------------
// Silence, leg 3: the DECLARATION premise is absent — nothing to demand.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_a_generator_body() {
    // A body with `yield` returns a `Generator` from the CALL; the declared type
    // describes that object, never a body exit (ADR-0057 §5).
    assert_silent(
        "<?php\nfunction f(): Generator {\n    yield 1;\n}\n",
        "no premise: a generator's declared type is not a body-exit obligation",
    );
    assert_silent(
        "<?php\nfunction f(): iterable {\n    yield from [1, 2];\n}\n",
        "no premise: `yield from` makes it a generator too",
    );
}

#[test]
fn silent_on_void_and_never() {
    assert_silent(
        "<?php\nfunction f(): void {\n    $x = 1;\n}\n",
        "no premise: `void` demands no value",
    );
    // `never` falling through IS a fatal, but a different one with a different
    // sentence (`never-returning function must not implicitly return`), and
    // ADR-0022 makes one id one consequence.
    assert_silent(
        "<?php\nfunction f(): never {\n    $x = 1;\n}\n",
        "no premise: `never`'s fall-through is a different id's consequence",
    );
}

#[test]
fn silent_on_an_untyped_function() {
    assert_silent(
        "<?php\nfunction f() {\n    $x = 1;\n}\n",
        "no premise: no written return type at all",
    );
}

#[test]
fn silent_on_an_abstract_method() {
    // Excluded by construction: the lowering builds a `Scope` only for a concrete
    // body, so a body-less declaration is never a candidate.
    assert_silent(
        "<?php
abstract class A {
    abstract public function m(): int;
}
",
        "no premise: an abstract method has no body to fall through",
    );
}

#[test]
fn silent_on_an_interface_method() {
    assert_silent(
        "<?php
interface I {
    public function m(): int;
}
",
        "no premise: an interface method has no body to fall through",
    );
}

#[test]
fn silent_on_a_constructor() {
    // Excluded by construction: PHP forbids a return type on `__construct`.
    assert_silent(
        "<?php
class A {
    public function __construct() {
        $x = 1;
    }
}
",
        "no premise: a constructor cannot declare a return type",
    );
}

#[test]
fn silent_on_an_arrow_function() {
    // Excluded by construction a third way: an arrow body lowers to a `return`.
    assert_silent(
        "<?php\n$f = fn (): int => 1;\n",
        "no premise: an arrow body IS a return, so the trace always terminates",
    );
}

#[test]
fn silent_on_a_closure_that_returns() {
    assert_silent(
        "<?php
$f = function (): int {
    return 1;
};
",
        "proven: the closure counterpart of the returning-function leg",
    );
}

// ---------------------------------------------------------------------------
// Registry wiring.
// ---------------------------------------------------------------------------

#[test]
fn the_id_is_suppressible_by_name() {
    use steins_infer::apply_inline_ignores;
    let src = "<?php
// @steins-ignore type.return-missing
function f(): int {
    $x = 1;
}
";
    let tree = SourceTree::parse(src);
    let raw = check_full(&tree, "test.php", &mut NoFold, true);
    assert_eq!(raw.iter().filter(|d| d.id == TYPE_RETURN_MISSING_ID).count(), 1);
    let outcome = apply_inline_ignores(raw, &[("test.php".to_owned(), &tree)]);
    assert_eq!(
        outcome.kept.iter().filter(|d| d.id == TYPE_RETURN_MISSING_ID).count(),
        0,
        "the registry-governed inline ignore channel reaches this id"
    );
    assert_eq!(outcome.suppressed, 1);
}
