//! Issue #158 — a write inside a **comparison operand** is still a write.
//!
//! The lowering represents a condition's shape (`Cmp`, `Instanceof`, `Truthy`,
//! `Call`, `Opaque`) and, until this slice, an operand it could not model became
//! a bare `CondOperand::Other` that mentioned nothing: not the call it was, not
//! the variables that call may have written by reference. So a guard call in
//! *guard* position invalidated its by-ref arguments and the same call one
//! character away — inside a comparison — invalidated nothing.
//!
//! The field shape is phpstan-src's `nsrt/preg_match_shapes.php::bug11622`:
//!
//! ```text
//! $matches = [];
//! if (preg_match('/^abc(def|$)/', $expression, $matches) === 1) {
//!     // reported `list{}` — an EMPTY array, on the one branch where PHP has
//!     // provably written ['abc…', …] into it
//! }
//! ```
//!
//! `list{}` there is a false fact on a reachable path, which is the zero-FP
//! bar's foundational rule, and it needed a prior binding to show: with no
//! `$matches = []` ahead of the guard nothing was bound, so nothing wrong
//! survived and the same bug answered a harmless `unknown`. Every test below
//! therefore **binds first** — that is what makes the assertion able to fail.
//!
//! Three things are pinned here:
//!
//! 1. every operand shape that can write forgets what it may have written;
//! 2. an operand that cannot write forgets **nothing** — the fix is not "forget
//!    more", and a property read or arithmetic in a comparison must keep the
//!    facts it never touched;
//! 3. what ADR-0070's by-value gate proves the callee could not reach survives
//!    the comparison, exactly as it survives a statement-position call.

use steins_infer::{DEBUG_TYPE_ID, check};
use steins_syntax::SourceTree;

/// Every `debug.type` message body a source produces, in source order.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The single dump a one-dump source produces.
fn one_dump(src: &str) -> String {
    let d = dumps(src);
    assert_eq!(d.len(), 1, "expected exactly one dump, got {d:?}");
    d[0].clone()
}

/// A function body dumping once, with `$m` pre-bound to the empty array — the
/// binding whose survival is the bug.
fn bound(body: &str) -> String {
    one_dump(&format!(
        "<?php\nfunction f(string $s, bool $b, object $o): void {{ $m = []; {body} }}\n"
    ))
}

/// However the engine currently spells the empty-array shape — **measured, not
/// written down**. The bug is "the pre-guard binding survived", and a literal
/// expectation makes that assertion silently vacuous the day the rendering
/// changes: `list{}` became `array{}` in #159 while this file was in flight, and
/// every `assert_ne!(…, "list{}")` in it would have gone on passing with the bug
/// fully intact.
fn empty_array() -> String {
    let rendered = one_dump("<?php\nfunction e(): void { $m = []; \\PHPStan\\dumpType($m); }\n");
    // A reference fixture that degenerates takes every test resting on it down
    // with it, silently and in the passing direction. Pin that it did not.
    assert_ne!(rendered, "unknown", "the empty-array reference stopped binding");
    rendered
}

/// What the bare truthy guard — the spelling ADR-0077 already supported —
/// answers for the same call. The seed tests below compare against THIS, because
/// that IS their claim: "the compared spelling witnesses exactly what the tested
/// spelling does". A written-down rendering would state something weaker and
/// would have to be chased every time the shape is re-spelled.
fn truthy_guard_shape() -> String {
    let rendered = one_dump(
        "<?php\nfunction t(string $s): void { \
         if (preg_match('/^abc(def|$)/', $s, $m)) { \\PHPStan\\dumpType($m); } }\n",
    );
    assert_ne!(rendered, "unknown", "the reference guard stopped seeding");
    rendered
}

/// The same body with the guard removed — what the binding reads as when nothing
/// tests it. A test whose claim is "this guard changed nothing" should compare
/// against *that*, measured, rather than against a written-down rendering: the
/// sealed-shape spelling moved twice (#159, #163) while this branch was open,
/// and each time a literal expectation would have had to be chased.
fn with_and_without_guard(decl: &str, guard: &str, dumped: &str) -> (String, String) {
    let guarded = one_dump(&format!(
        "<?php\nfunction f(): void {{ {decl} if ({guard}) {{ \\PHPStan\\dumpType({dumped}); }} }}\n"
    ));
    let plain = one_dump(&format!(
        "<?php\nfunction f(): void {{ {decl} \\PHPStan\\dumpType({dumped}); }}\n"
    ));
    (guarded, plain)
}

// 1. The reported shape, and every operand position that reaches it

#[test]
fn a_by_ref_write_in_a_comparison_operand_is_not_forgotten_as_empty() {
    // bug11622 itself. The seed (ADR-0077, extended below) answers the success
    // shape; what must never come back is the pre-guard empty array.
    let got = bound("if (preg_match('/^abc(def|$)/', $s, $m) === 1) { \\PHPStan\\dumpType($m); }");
    assert_ne!(got, empty_array(), "the pre-guard `$m = []` survived the write");
    // And the branch is not merely honest, it is as sharp as the spelling
    // ADR-0077 already supported.
    assert_eq!(got, truthy_guard_shape());
}

#[test]
fn every_equality_operator_carries_the_write() {
    // The four equality operators reach `CondExpr::Cmp` (the orderings lower to
    // `Opaque`, which always collected its reads). None may keep the binding —
    // whether or not the comparison also proves the write happened, which is a
    // separate question answered by the seed tests below.
    for guard in [
        "preg_match('/a(b)/', $s, $m) === 0",
        "preg_match('/a(b)/', $s, $m) !== 0",
        "preg_match('/a(b)/', $s, $m) == 0",
        "preg_match('/a(b)/', $s, $m) != 0",
        "0 === preg_match('/a(b)/', $s, $m)",
    ] {
        let got = bound(&format!("if ({guard}) {{ \\PHPStan\\dumpType($m); }}"));
        assert_ne!(got, empty_array(), "`{guard}` kept the pre-guard binding");
    }
}

#[test]
fn an_ordering_comparison_carries_the_write() {
    // `<`/`<=`/`>`/`>=` with an unrepresentable operand lower to `Opaque`, which
    // has always collected its whole read set — so these were sound before this
    // slice and are pinned here so a future lift of that fallback (which would
    // route them through `Cmp` like the rest, and let `> 0` reach the seed)
    // cannot quietly take the soundness with it.
    for guard in [
        "preg_match('/a(b)/', $s, $m) > 0",
        "preg_match('/a(b)/', $s, $m) >= 1",
        "0 < preg_match('/a(b)/', $s, $m)",
    ] {
        let got = bound(&format!("if ({guard}) {{ \\PHPStan\\dumpType($m); }}"));
        assert_ne!(got, empty_array(), "`{guard}` kept the pre-guard binding");
    }
}

#[test]
fn a_negated_comparison_carries_the_write() {
    // `!` recurses into the comparison — the polarity flips, the write does not.
    let got = bound("if (!(preg_match('/a(b)/', $s, $m) === 0)) { \\PHPStan\\dumpType($m); }");
    assert_eq!(got, "unknown");
}

#[test]
fn a_comparison_inside_a_conjunction_carries_the_write() {
    // The `&&`/`||` walk already recursed; what it recursed *into* was blind.
    let got = bound("if ($b && preg_match('/a(b)/', $s, $m) === 0) { \\PHPStan\\dumpType($m); }");
    assert_eq!(got, "unknown");
    let got = bound("if ($b || preg_match('/a(b)/', $s, $m) === 0) { } else { \\PHPStan\\dumpType($m); }");
    assert_eq!(got, "unknown");
}

#[test]
fn a_call_in_an_instanceof_operand_carries_the_write() {
    // The other operand-carrying variant, reached only when the right-hand side
    // is a plain class name (anything else already lowered to `Opaque`).
    let got = bound("if (mk($s, $m) instanceof \\DateTime) { \\PHPStan\\dumpType($m); }");
    assert_eq!(got, "unknown");
}

#[test]
fn a_ternary_condition_carries_the_write() {
    // A ternary guard is the same `CondExpr` under a different construct, and
    // its arms are evaluated under the guard's threaded env.
    let got = bound("$r = preg_match('/a(b)/', $s, $m) === 1 ? 1 : 2; \\PHPStan\\dumpType($m);");
    assert_eq!(got, "unknown");
}

#[test]
fn an_assignment_or_increment_in_a_comparison_operand_carries_its_write() {
    // Not every writer is a call. `$i++` rebinds `$i` before the branch sees it,
    // so the tested value is exactly the one value the branch may NOT assume.
    let got = one_dump(
        "<?php\nfunction f(): void { $i = 5; if ($i++ === 5) { \\PHPStan\\dumpType($i); } }\n",
    );
    assert_ne!(got, "5", "the branch saw the pre-increment value");
    let got = one_dump(
        "<?php\nfunction f(): void { $x = 1; if (($x = 2) === 2) { \\PHPStan\\dumpType($x); } }\n",
    );
    assert_ne!(got, "1", "the branch saw the pre-assignment value");
}

// 2. The other half: an operand that cannot write forgets nothing

#[test]
fn an_operand_that_cannot_write_keeps_every_fact() {
    // The fix is not "an unmodelled operand is dangerous". A property fetch, an
    // offset read and arithmetic read and return; forgetting there would be a
    // precision loss with no soundness content.
    for guard in ["$o->p === $t", "$o->p[0] === $t", "$o->p . 'x' === $t", "-$n === 3"] {
        let got = one_dump(&format!(
            "<?php\nfunction f(object $o, int $n): void {{ $t = 'abc'; if ({guard}) {{ \\PHPStan\\dumpType($t); }} }}\n"
        ));
        assert_eq!(got, "'abc'", "`{guard}` forgot a variable it cannot write");
    }
}

#[test]
fn a_method_receiver_survives_its_own_call_in_a_comparison() {
    // ADR-0052 §6 payoff (i), which the operand path inherits: `$d->m()` does not
    // rebind `$d`, so a proven class survives the comparison it sits in.
    let got = one_dump(
        "<?php\nfunction f(): void { $d = new \\DateTime(); if ($d->format('Y') === '2026') { \\PHPStan\\dumpType($d); } }\n",
    );
    assert_eq!(got, "DateTime");
}

// 3. ADR-0070's by-value gate, applied in operand position

#[test]
fn a_by_value_argument_survives_the_comparison_it_is_compared_in() {
    // `count($a)` hands `$a` to a by-value parameter, so the callee cannot reach
    // the caller's binding — the same reason `count($a);` as a statement leaves
    // it alone. Without this the soundness fix above would have cost 191 nsrt
    // observations that are true. The claim is "the guard changed nothing", so
    // it is asked as exactly that: the guarded reading must equal the unguarded
    // one, whatever either currently renders as.
    let (guarded, plain) = with_and_without_guard("$a = [1, 2];", "count($a) === 2", "$a");
    assert_eq!(guarded, plain, "the comparison forgot a by-value argument");
    assert_ne!(guarded, "unknown", "the fixture proves nothing if nothing was bound");

    let (guarded, plain) = with_and_without_guard("$t = 'abc';", "strlen($t) === 3", "$t");
    assert_eq!(guarded, plain, "the comparison forgot a by-value argument");
    assert_eq!(guarded, "'abc'");
}

#[test]
fn the_by_ref_position_of_a_partly_by_value_call_still_condemns_its_argument() {
    // The gate is per name, not per call: `preg_match` takes `$s` by value and
    // `$m` by reference, and the comparison must keep the first while forgetting
    // the second. (`$s` here is the seed's own witness that the split is real.)
    let dumped = dumps(
        "<?php\nfunction f(): void { $s = 'abc'; $m = [];\n\
         if (preg_match('/x/', $s, $m) === 0) { \\PHPStan\\dumpType($s); \\PHPStan\\dumpType($m); } }\n",
    );
    assert_eq!(dumped, vec!["'abc'".to_owned(), "unknown".to_owned()]);
}

#[test]
fn a_by_value_call_beside_a_non_call_writer_keeps_the_blanket_drop() {
    // The by-value evidence describes what a *callee* does to its arguments and
    // says nothing about an assignment sitting beside it, so an operand carrying
    // both refuses the exemption outright.
    let got = one_dump(
        "<?php\nfunction f(): void { $t = 'abc'; if (strlen($t) + ($t = 'z') === 4) { \\PHPStan\\dumpType($t); } }\n",
    );
    assert_ne!(got, "'abc'", "an assignment beside a by-value call was excused");
}

// 4. The seed's witness (ADR-0077 §3.2) extended to the compared call

#[test]
fn identity_against_one_witnesses_the_write() {
    // PHPStan types all of these, and the witness is satisfied by `=== 1` just as
    // it is by bare truthiness: every value satisfying the comparison is truthy,
    // so the branch proves the callee performed its by-ref write.
    let expected = truthy_guard_shape();
    for guard in [
        "if (preg_match('/^abc(def|$)/', $s, $m) === 1) { \\PHPStan\\dumpType($m); }",
        "if (1 === preg_match('/^abc(def|$)/', $s, $m)) { \\PHPStan\\dumpType($m); }",
        "if (preg_match('/^abc(def|$)/', $s, $m) == 1) { \\PHPStan\\dumpType($m); }",
        "if (preg_match('/^abc(def|$)/', $s, $m) !== 1) { return; } \\PHPStan\\dumpType($m);",
        "if (preg_match('/^abc(def|$)/', $s, $m) != 1) { return; } \\PHPStan\\dumpType($m);",
        "if ($b && preg_match('/^abc(def|$)/', $s, $m) === 1) { \\PHPStan\\dumpType($m); }",
    ] {
        let got = one_dump(&format!(
            "<?php\nfunction f(string $s, bool $b): void {{ {guard} }}\n"
        ));
        assert_eq!(got, expected, "`{guard}` did not witness the write");
    }
}

#[test]
fn a_comparison_that_admits_a_falsy_result_witnesses_nothing() {
    // The refusals are the load-bearing half. `preg_match` returns `false` on a
    // pattern PCRE will not compile and writes **nothing at all**, so a guard
    // that admits `false` proves no write — and one that proves the result is
    // `0` proves the failure branch, not the success shape.
    for guard in [
        // `0` is falsy: no write is witnessed.
        "if (preg_match('/^abc(def|$)/', $s, $m) === 0) { \\PHPStan\\dumpType($m); }",
        // `!== false` still admits `0`.
        "if (preg_match('/^abc(def|$)/', $s, $m) !== false) { \\PHPStan\\dumpType($m); }",
        // The else branch of `=== 1` is where `0` and `false` live.
        "if (preg_match('/^abc(def|$)/', $s, $m) === 1) { return; } \\PHPStan\\dumpType($m);",
        // A comparison against a non-literal decides nothing.
        "if (preg_match('/^abc(def|$)/', $s, $m) === $n) { \\PHPStan\\dumpType($m); }",
    ] {
        let got =
            one_dump(&format!("<?php\nfunction f(string $s, int $n): void {{ {guard} }}\n"));
        assert_eq!(got, "unknown", "`{guard}` seeded a fact it had not proven");
    }
}

#[test]
fn a_compared_call_does_not_widen_the_assert_if_true_envelope() {
    // The seed's witness is truthiness; `@phpstan-assert-if-true` is stated about
    // the callee returning `true`, and `f() === 1` does not witness that — `1` is
    // truthy but is not `true`. The two collectors stay separate so a comparison
    // cannot silently widen every envelope in the project.
    let src = "<?php
/** @phpstan-assert-if-true non-empty-string $v */
function isFilled(?string $v): bool { return $v !== null && $v !== ''; }
function f(?string $v): void { %s \\PHPStan\\dumpType($v); }
";
    // `$v` reads exactly as it does with no guard at all: not narrowed by the
    // envelope, and not merely forgotten either — it keeps its declared arm,
    // because `?string $v` is a by-value parameter (the gate above).
    let compared = one_dump(&src.replace("%s", "if (isFilled($v) === 1)"));
    let plain = one_dump(&src.replace("%s", ""));
    assert_eq!(compared, plain);
    assert_ne!(compared, "non-empty-string", "the envelope was applied to a comparison");
}
