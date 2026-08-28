//! Issue #158 — a write inside a **comparison operand** is still a write.
//!
//! The lowering represents a condition's shape (`Cmp`, `Instanceof`, `Truthy`,
//! `Call`, `Opaque`); before this slice an unmodelled operand became a bare
//! `CondOperand::Other` that forgot everything — so a guard call in *guard*
//! position invalidated its by-ref arguments, but the same call one character
//! away, inside a comparison, invalidated nothing.
//!
//! Field shape: phpstan-src's `nsrt/preg_match_shapes.php::bug11622` reports
//! `list{}` (an EMPTY array) on the one branch where PHP has provably written
//! into it — a false fact on a reachable path, the zero-FP bar's foundational
//! rule. It needed a prior `$matches = []` binding to show at all, so every
//! test below **binds first**, which is what makes the assertion able to fail.
//!
//! Three things are pinned here:
//!
//! 1. every operand shape that can write forgets what it may have written;
//! 2. an operand that cannot write forgets **nothing** — reads/arithmetic in a
//!    comparison must keep facts they never touched;
//! 3. what ADR-0070's by-value gate proves unreachable survives the
//!    comparison, exactly as it survives a statement-position call.

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
/// written down**: `list{}` became `array{}` in #159 while this file was in
/// flight, and a literal `assert_ne!(…, "list{}")` would have kept passing with
/// the bug fully intact.
fn empty_array() -> String {
    let rendered = one_dump("<?php\nfunction e(): void { $m = []; \\PHPStan\\dumpType($m); }\n");
    // A degenerating reference fixture takes every dependent test down with it,
    // silently; pin that it did not.
    assert_ne!(rendered, "unknown", "the empty-array reference stopped binding");
    rendered
}

/// What the bare truthy guard (ADR-0077's spelling) answers for the same call.
/// Seed tests compare against THIS — a written-down rendering would state a
/// weaker claim and need re-chasing every time the shape is re-spelled.
fn truthy_guard_shape() -> String {
    let rendered = one_dump(
        "<?php\nfunction t(string $s): void { \
         if (preg_match('/^abc(def|$)/', $s, $m)) { \\PHPStan\\dumpType($m); } }\n",
    );
    assert_ne!(rendered, "unknown", "the reference guard stopped seeding");
    rendered
}

/// The same body with the guard removed — what the binding reads as when nothing
/// tests it. A "this guard changed nothing" claim should compare against *that*,
/// measured: the spelling moved twice (#159, #163) while this branch was open.
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
    // bug11622 itself: the seed (ADR-0077, extended below) answers the success
    // shape; what must never come back is the pre-guard empty array.
    let got = bound("if (preg_match('/^abc(def|$)/', $s, $m) === 1) { \\PHPStan\\dumpType($m); }");
    assert_ne!(got, empty_array(), "the pre-guard `$m = []` survived the write");
    // And the branch is as sharp as the spelling ADR-0077 already supported.
    assert_eq!(got, truthy_guard_shape());
}

#[test]
fn every_equality_operator_carries_the_write() {
    // The four equality operators reach `CondExpr::Cmp` (orderings lower to
    // `Opaque`, which always collected reads); none may keep the binding.
    // Whether the comparison also proves the write is answered by the seed tests below.
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
    // `<`/`<=`/`>`/`>=` lower to `Opaque` (unrepresentable operand), always sound
    // via its whole read set — pinned so a future lift of that fallback (routing
    // through `Cmp`, letting `> 0` reach the seed) can't quietly lose the soundness.
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
    // The fix is not "an unmodelled operand is dangerous": a property fetch,
    // offset read, or arithmetic read forgetting there is precision loss, not soundness.
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
    // `count($a)` hands `$a` to a by-value parameter, so the callee can't reach
    // the caller's binding (same reason `count($a);` alone leaves it). Without
    // this, the soundness fix above would cost 191 true nsrt observations. Claim
    // is "the guard changed nothing": guarded reading must equal unguarded.
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
    // `$m` by reference — keep the first, forget the second (`$s` is the witness).
    let dumped = dumps(
        "<?php\nfunction f(): void { $s = 'abc'; $m = [];\n\
         if (preg_match('/x/', $s, $m) === 0) { \\PHPStan\\dumpType($s); \\PHPStan\\dumpType($m); } }\n",
    );
    assert_eq!(dumped, vec!["'abc'".to_owned(), "unknown".to_owned()]);
}

#[test]
fn a_by_value_call_beside_a_non_call_writer_keeps_the_blanket_drop() {
    // By-value evidence describes what a *callee* does to its arguments, not an
    // assignment beside it — an operand carrying both refuses the exemption.
    let got = one_dump(
        "<?php\nfunction f(): void { $t = 'abc'; if (strlen($t) + ($t = 'z') === 4) { \\PHPStan\\dumpType($t); } }\n",
    );
    assert_ne!(got, "'abc'", "an assignment beside a by-value call was excused");
}

// 4. The seed's witness (ADR-0077 §3.2) extended to the compared call

#[test]
fn identity_against_one_witnesses_the_write() {
    // PHPStan types all of these; `=== 1` satisfies the witness just as bare
    // truthiness does — every value satisfying the comparison is truthy.
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
    // The refusals are the load-bearing half: `preg_match` returns `false` on an
    // uncompilable pattern and writes **nothing**, so a guard admitting `false`
    // proves no write; one proving `0` proves the failure branch, not success.
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
    // The seed's witness is truthiness; `@phpstan-assert-if-true` is about the
    // callee returning `true`, and `f() === 1` doesn't witness that (`1` is
    // truthy, not `true`) — the two collectors stay separate.
    let src = "<?php
/** @phpstan-assert-if-true non-empty-string $v */
function isFilled(?string $v): bool { return $v !== null && $v !== ''; }
function f(?string $v): void { %s \\PHPStan\\dumpType($v); }
";
    // `$v` reads exactly as with no guard: not narrowed by the envelope, not
    // forgotten either — `?string $v` is a by-value parameter (the gate above).
    let compared = one_dump(&src.replace("%s", "if (isFilled($v) === 1)"));
    let plain = one_dump(&src.replace("%s", ""));
    assert_eq!(compared, plain);
    assert_ne!(compared, "non-empty-string", "the envelope was applied to a comparison");
}


// The class-reflection family answers about a value and writes nothing (#569)


/// Issue #569. `get_class`, `is_a`, `is_subclass_of` and kin declare every
/// parameter by value in PHP's own signature, so a guard built from one cannot
/// have rebound its subject. Before this they were unrecognized, and the operand
/// rule charged the whole read set — so `get_class($x) === A::class` answered
/// `unknown` about `$x` on BOTH branches, where the `$x::class` spelling of the
/// same test left the declared arms standing.
///
/// What these guards PROVE is not claimed here and stays #538's: the arms are
/// unnarrowed, they are merely still there.
#[test]
fn a_class_reflection_guard_keeps_its_subjects_facts() {
    for guard in [
        "\\get_class($x) === A::class",
        "\\get_class($x) !== A::class",
        "A::class === \\get_class($x)",
        "\\is_a($x, A::class)",
        "\\is_subclass_of($x, A::class)",
        "\\get_parent_class($x) === A::class",
        "\\get_debug_type($x) === 'A'",
        "\\spl_object_id($x) === 1",
    ] {
        let src = format!(
            "<?php\nfinal class A {{}}\nfinal class B {{}}\n\
             /** @param A|B $x */\nfunction f($x): void {{ if ({guard}) {{ \\PHPStan\\dumpType($x); }} }}\n"
        );
        assert_eq!(dumps(&src), ["A|B (asserted)"], "{guard}");
    }
}

/// The option flags are the half that could have been got wrong: they change
/// what the call ANSWERS, never whether it writes, so every arity is exempt.
#[test]
fn the_option_flags_do_not_change_what_the_guard_forgets() {
    for guard in [
        "\\is_a($x, A::class, true)",
        "\\is_a($x, A::class, false)",
        "\\is_subclass_of($x, A::class, true)",
        "\\is_subclass_of($x, A::class, false)",
    ] {
        let src = format!(
            "<?php\nfinal class A {{}}\nfinal class B {{}}\n\
             /** @param A|B $x */\nfunction f($x): void {{ if ({guard}) {{ \\PHPStan\\dumpType($x); }} }}\n"
        );
        assert_eq!(dumps(&src), ["A|B (asserted)"], "{guard}");
    }
}

/// The control: a builtin that really does write an argument by reference still
/// forgets it, in the identical operand position.
#[test]
fn a_by_reference_builtin_in_the_same_position_still_forgets() {
    let src = "<?php\n\
        /** @param A|B $x */\nfunction f($x, string $s): void {\n\
        \\preg_match('/x/', $s, $x);\n  \\PHPStan\\dumpType($x);\n}\n";
    assert_ne!(dumps(src), ["A|B (asserted)"]);
}


// `instanceof` with a dynamic class writes neither side either (#571)


/// Issue #571, the last spelling of the same defect. `$v instanceof $class`
/// fitted no case in the lowering — only a written `Identifier` right-hand side
/// builds `CondExpr::Instanceof` — so the whole condition became `Opaque` and the
/// subject was charged the by-reference conservatism an unmodellable condition
/// owes. `instanceof` is an operator: it writes neither side.
#[test]
fn a_dynamic_class_instanceof_keeps_its_subjects_facts() {
    for guard in ["$v instanceof $class", "!($v instanceof $class)"] {
        let src = format!(
            "<?php\nfinal class A {{}}\nfinal class B {{}}\n\
             /** @param A|B $v @param class-string<A> $class */\n\
             function f($v, string $class): void {{ if ({guard}) {{ \\PHPStan\\dumpType($v); }} }}\n"
        );
        assert_eq!(dumps(&src), ["A|B (asserted)"], "{guard}");
    }
}

/// The class operand is carried rather than dropped, so the value that decides
/// the guard survives to be read later (#573). Nothing reads it yet, which is
/// what this pins: the subject is unnarrowed, merely intact.
#[test]
fn the_dynamic_form_narrows_nothing_yet() {
    let src = "<?php\nfinal class A {}\nfinal class B {}\n\
        /** @param A|B $v @param class-string<A> $class */\n\
        function f($v, string $class): void { if ($v instanceof $class) { \\PHPStan\\dumpType($v); } else { \\PHPStan\\dumpType($v); } }\n";
    assert_eq!(dumps(src), ["A|B (asserted)", "A|B (asserted)"]);
}

/// The written form is untouched — it narrows as it always did, which is the
/// regression this variant could most easily have caused.
#[test]
fn the_written_form_still_narrows() {
    let src = "<?php\nfinal class A {}\nfinal class B {}\n\
        /** @param A|B $v */\n\
        function f($v): void { if ($v instanceof A) { \\PHPStan\\dumpType($v); } else { \\PHPStan\\dumpType($v); } }\n";
    assert_eq!(dumps(src), ["A", "B (asserted)"]);
}
