//! ADR-0057 amendment, slice T0 — return-FACT summaries.
//!
//! A callee's return proof crosses the boundary: during the binding descent the
//! checker already performs for a `$x = f(...)` positional call, the returned
//! expression's value-domain fact is joined over every returning exit and bound as
//! the call-result's VALUE fact (the value floor above the declared arms, A1). These
//! fixtures are A7's acceptance set.
//!
//! Zero-arg factories are out of T0 scope (deferred to T2's emission-suppressed
//! walk); the flagship's precision is arg-independent (the owner's `f(): int` takes
//! no parameters), so the shape here forces the SAME proof through a positional
//! descent — an unresolved second argument leaves that parameter on its native-int
//! seed, the assert narrows it, and the proof crosses.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, RETURN_ID, RETURN_MISMATCH_ID, check};
use steins_syntax::SourceTree;

/// All diagnostics for a source, resolving the file's own functions (so the descent
/// reaches project callees).
fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

/// The single `debug.type` message body a one-dump source produces.
fn one_type(src: &str) -> String {
    let ds: Vec<Diagnostic> =
        findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.clone()
}

fn count(src: &str, id: &str) -> usize {
    findings(src).iter().filter(|d| d.id == id).count()
}

// ==========================================================================
// (i) The flagship: the body's positive-int proof crosses, no stratum marker.
// ==========================================================================

#[test]
fn flagship_positive_int_crosses_verified() {
    // `$n` keeps its native int seed (the `rand()` argument does not resolve, so the
    // parameter is never bound to a concrete value); `assert($n > 0)` narrows it to
    // positive-int at the Verified stratum; the return crosses that fact.
    let src = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: positive-int");
}

// ==========================================================================
// (ii) Mixed strata: a Verified exit joined with an Asserted exit renders (asserted).
// ==========================================================================

#[test]
fn mixed_strata_join_renders_asserted() {
    // Exit 1 narrows a native-int param via `assert` → positive-int VERIFIED. Exit 2
    // narrows an untyped param via a `@phpstan-assert positive-int` helper → positive-int
    // ASSERTED (a docblock claim; it cannot overwrite a Verified fact, hence the untyped
    // param with no prior seed). The join is positive-int at the min stratum (Asserted).
    let src = "<?php\n\
        /** @phpstan-assert positive-int $v */\n\
        function assertPos($v): void {}\n\
        function f(int $trigger, int $n, $m, bool $b): int {\n\
            if ($b) {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
            assertPos($m);\n\
            return $m;\n\
        }\n\
        $x = f(1, rand(), rand(), (bool) rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: positive-int (asserted)");
}

// ==========================================================================
// (iii) A factless exit degrades the join to the arm floor — identical to
//       no-summary (a bare `int`, no stratum marker).
// ==========================================================================

#[test]
fn factless_exit_degrades_to_arm_floor() {
    // One exit narrows to positive-int; the other returns the opaque `rand()` (a
    // factless int exit → the declared floor `General{int}`). The join is `int`.
    let with_summary = "<?php\n\
        function f(int $trigger, int $n, bool $b): int {\n\
            if ($b) {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
            return rand();\n\
        }\n\
        $x = f(1, rand(), (bool) rand());\n\
        \\PHPStan\\dumpType($x);\n";
    // A no-summary control: a plain `: int` return the walk proves nothing about.
    let no_summary = "<?php\n\
        function g(int $trigger, int $n): int {\n\
            return rand();\n\
        }\n\
        $x = g(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(with_summary), "dumped type: int");
    assert_eq!(one_type(with_summary), one_type(no_summary), "degrade is observably no-summary");
}

// ==========================================================================
// (iv) A native return-mismatch exit is DROPPED; the callee's finding fires, the
//      caller sees the arm floor.
// ==========================================================================

#[test]
fn native_return_mismatch_drops_exit() {
    // The `return "oops"` violates the native `: int` — a proven boundary TypeError.
    // The callee's `type.return-mismatch` fires; the exit contributes nothing, so the
    // caller's summary joins only the conforming exit(s). Here the only other exit is
    // the opaque `rand()` (factless int) → arm floor `int`.
    let src = "<?php\n\
        function f(int $trigger, int $n, bool $b): int {\n\
            if ($b) {\n\
                return \"oops\";\n\
            }\n\
            return rand();\n\
        }\n\
        $x = f(1, rand(), (bool) rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(count(src, RETURN_ID), 1, "the callee's native return-mismatch fires");
    assert_eq!(one_type(src), "dumped type: int", "the caller sees the arm floor");
}

#[test]
fn native_return_mismatch_only_exit_no_summary() {
    // When the ONLY returning exit violates the native envelope, no exit remains — no
    // summary. The declared arm floor stands.
    let src = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            return \"oops\";\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(count(src, RETURN_ID), 1);
    assert_eq!(one_type(src), "dumped type: int");
}

// ==========================================================================
// (v) A phpdoc-only mismatch: the docblock is the lie, the walk truth crosses.
// ==========================================================================

#[test]
fn phpdoc_return_mismatch_crosses_walk_truth() {
    // Native `: int` (satisfied), `@return positive-int` (violated by the proven
    // `negative-int`). `phpdoc.return-mismatch` fires on the callee; the walk truth
    // (`negative-int`) crosses to the caller — claims do not edit proofs (A2).
    let src = "<?php\n\
        /** @return positive-int */\n\
        function f(int $trigger, int $n): int {\n\
            assert($n < 0);\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 1, "the phpdoc lie is reported on the callee");
    assert_eq!(one_type(src), "dumped type: negative-int", "the walk truth crosses");
}

// ==========================================================================
// Recursion terminates and degrades to the arm floor (A5).
// ==========================================================================

#[test]
fn recursion_terminates_arm_floor() {
    // A recursive factory: the on-stack call yields no summary, so its exit degrades
    // to the floor; the whole summary degrades to `int`. Terminating and sound.
    let src = "<?php\n\
        function f(int $n, bool $b): int {\n\
            if ($b) {\n\
                return f($n, false);\n\
            }\n\
            return $n;\n\
        }\n\
        $x = f(3, true);\n\
        \\PHPStan\\dumpType($x);\n";
    // `int` (the floor) or the concrete singleton — either way it terminates and is
    // never wrong. We pin termination + a sound rendering.
    let ty = one_type(src);
    assert!(ty == "dumped type: int" || ty == "dumped type: 3", "sound + terminating: {ty}");
}

// ==========================================================================
// The literal-return degenerate case agrees with `resolve_const_fn`.
// ==========================================================================

#[test]
fn literal_return_agrees_with_resolve_const_fn() {
    // A one-arg function with a literal return: the T0 summary produces `Singleton`,
    // exactly what `resolve_const_fn` crosses for the zero-arg twin. Both render `42`.
    let via_summary = "<?php\n\
        function pick(int $x): int { return 42; }\n\
        $a = pick(1);\n\
        \\PHPStan\\dumpType($a);\n";
    let via_const_fn = "<?php\n\
        function answer(): int { return 42; }\n\
        $a = answer();\n\
        \\PHPStan\\dumpType($a);\n";
    assert_eq!(one_type(via_summary), "dumped type: 42");
    assert_eq!(one_type(via_summary), one_type(via_const_fn), "the two literal paths agree");
}

// ==========================================================================
// A Refined-string summary crosses (a guard-narrowed non-empty string).
// ==========================================================================

#[test]
fn refined_string_summary_crosses() {
    // `$s` keeps its native string seed; the guard proves it non-empty; the return
    // crosses a `non-empty-string` fact.
    let src = "<?php\n\
        function f(int $trigger, string $s): string {\n\
            if ($s === '') {\n\
                return 'x';\n\
            }\n\
            return $s;\n\
        }\n\
        $x = f(1, (string) rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: non-empty-string");
}

// ==========================================================================
// The summary composes through two descent levels (A1 replayable query answer).
// ==========================================================================

#[test]
fn summary_composes_through_two_levels() {
    // `g` proves positive-int and returns it; `f` returns `g(...)`'s result. The
    // inner summary is the outer exit's fact — positive-int crosses both boundaries.
    let src = "<?php\n\
        function g(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        function f(int $t): int {\n\
            return g(1, rand());\n\
        }\n\
        $x = f(9);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: positive-int");
}

// ==========================================================================
// A bare-parameter return degrades to `General{int}` → the arm floor stands.
// ==========================================================================

#[test]
fn bare_general_return_falls_to_arm_floor() {
    // The body returns the opaque native-seeded `$n` with no narrowing: `General{int}`,
    // which carries nothing beyond the declared arms — the arm floor stands (`int`).
    let src = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: int");
}

// ==========================================================================
// Memo replay is deterministic: two identical calls render identically (§3).
// ==========================================================================

#[test]
fn memo_replay_is_deterministic() {
    let src = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        $y = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n\
        \\PHPStan\\dumpType($y);\n";
    let ds: Vec<String> = findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(ds, vec!["dumped type: positive-int".to_owned(), "dumped type: positive-int".to_owned()]);
}

// ==========================================================================
// A nullable single-base return: the floor carries `|null` (a factless exit).
// ==========================================================================

#[test]
fn nullable_return_floor_carries_null() {
    // A factless exit under `?int` degrades to `int|null` — observably the arm floor.
    let src = "<?php\n\
        function f(int $trigger, ?int $n): ?int {\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: int|null");
}

// ==========================================================================
// A zero-arg factory does NOT descend in T0 — it keeps the declared arm floor
// (deferred to T2). A one-arg twin with the same body DOES cross.
// ==========================================================================

#[test]
fn zero_arg_factory_keeps_arm_floor() {
    // `make()` is zero-arg: no descent in T0, so `$x` takes the declared `int` arm
    // (its body's positive-int proof does not cross — deferred to T2).
    let src = "<?php\n\
        function opaque(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        function make(): int {\n\
            $v = 0;\n\
            $v++;\n\
            return $v;\n\
        }\n\
        $x = make();\n\
        \\PHPStan\\dumpType($x);\n";
    // `make()` has a literal-ish body but `$v++` blocks `resolve_const_fn`; being
    // zero-arg it does not descend — the declared `int` arm is the floor.
    assert_eq!(one_type(src), "dumped type: int");
}

// ==========================================================================
// The summary premises no false proof: a positive-int result flowing into an
// `int` sink is silent (soundness — the value fits its contract).
// ==========================================================================

#[test]
fn summary_value_premises_no_false_finding() {
    let src = "<?php\n\
        function takesInt(int $x): void {}\n\
        function f(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        takesInt($x);\n";
    // positive-int into int: no argument-mismatch, no return-mismatch, nothing.
    assert_eq!(findings(src).len(), 0, "a sound summary premises no finding: {:?}", findings(src));
}
