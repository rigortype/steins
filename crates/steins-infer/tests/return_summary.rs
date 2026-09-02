//! ADR-0057 amendment T0 — return-FACT summaries.
//!
//! A callee's return proof crosses the boundary: during the binding descent the
//! checker already performs for a `$x = f(...)` positional call, the returned
//! expression's value-domain fact is joined over every returning exit and bound as
//! the call-result's VALUE fact (the value floor above the declared arms, A1).
//!
//! Zero-arg factories do not descend in T0, so most fixtures here add an unused
//! first `int $trigger` parameter — an unresolved argument leaves it on its
//! native-int seed, forcing the SAME proof through a positional descent.

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

// (i) The flagship: the body's positive-int proof crosses, no stratum marker.

#[test]
fn flagship_positive_int_crosses_verified() {
    // `$n` keeps its native int seed (`rand()` doesn't resolve); `assert($n > 0)`
    // narrows it to positive-int at Verified; the return crosses that fact.
    let src = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: int<1, max>");
}

// (ii) Mixed strata: a Verified exit joined with an Asserted exit renders (asserted).

#[test]
fn mixed_strata_join_renders_asserted() {
    // Exit 1 narrows a native-int param via `assert` → positive-int VERIFIED. Exit 2
    // narrows an untyped param via a docblock `@phpstan-assert` helper → positive-int
    // ASSERTED (untyped since a docblock claim can't overwrite a Verified fact). The
    // join is positive-int at the min stratum (Asserted).
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
    assert_eq!(one_type(src), "dumped type: int<1, max> (asserted)");
}

// (iii) A factless exit degrades the join to the arm floor — identical to
//       no-summary (a bare `int`, no stratum marker).

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

// (iv) A native return-mismatch exit is DROPPED; the callee's finding fires, the
//      caller sees the arm floor.

#[test]
fn native_return_mismatch_drops_exit() {
    // `return "oops"` violates native `: int` (a proven boundary TypeError): the
    // callee's `type.return-mismatch` fires and the exit contributes nothing, so the
    // summary joins only the other exit — opaque `rand()` (factless) → arm floor `int`.
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

// (v) A phpdoc-only mismatch: the docblock is the lie, the walk truth crosses.

#[test]
fn phpdoc_return_mismatch_crosses_walk_truth() {
    // Native `: int` (satisfied), `@return positive-int` (violated by the proven
    // `negative-int`): `phpdoc.return-mismatch` fires on the callee, but the walk
    // truth (`negative-int`) crosses to the caller — claims don't edit proofs (A2).
    let src = "<?php\n\
        /** @return positive-int */\n\
        function f(int $trigger, int $n): int {\n\
            assert($n < 0);\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 1, "the phpdoc lie is reported on the callee");
    assert_eq!(one_type(src), "dumped type: int<min, -1>", "the walk truth crosses");
}

// Recursion terminates and degrades to the arm floor (A5).

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

// The literal-return degenerate case agrees with `resolve_const_fn`.

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

// A Refined-string summary crosses (a guard-narrowed non-empty string).

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

// The summary composes through two descent levels (A1 replayable query answer).

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
    assert_eq!(one_type(src), "dumped type: int<1, max>");
}

// A bare-parameter return degrades to `General{int}` → the arm floor stands.

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

// Memo replay is deterministic: two identical calls render identically (§3).

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
    assert_eq!(ds, vec!["dumped type: int<1, max>".to_owned(), "dumped type: int<1, max>".to_owned()]);
}

// A nullable single-base return: the floor carries `|null` (a factless exit).

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

// A zero-arg factory does not descend in T0; a one-arg twin does.

#[test]
fn zero_arg_factory_keeps_arm_floor() {
    // With no descent, `make()` retains its declared `int` arm.
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
    // `$v++` blocks `resolve_const_fn`; being zero-arg it doesn't descend either.
    assert_eq!(one_type(src), "dumped type: int");
}

// The summary premises no false proof: a positive-int result flowing into an
// `int` sink is silent (soundness — the value fits its contract).

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

// Argument position (issue #60): a call's summary crosses WITHOUT the assignment
// detour. Every fixture here pins the argument form against the assignment form.

#[test]
fn flagship_crosses_in_argument_position() {
    // The flagship proof, dumped directly: `dumpType(f(1, rand()))` — no `$x`.
    let arg_form = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        \\PHPStan\\dumpType(f(1, rand()));\n";
    let assigned_form = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            assert($n > 0);\n\
            return $n;\n\
        }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(arg_form), "dumped type: int<1, max>");
    assert_eq!(one_type(arg_form), one_type(assigned_form), "the two forms are identical");
}

#[test]
fn mixed_strata_render_asserted_in_argument_position() {
    // The (ii) shape, argument form: the Asserted marker survives the position
    // change — a docblock claim must not launder by being dumped directly.
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
        \\PHPStan\\dumpType(f(1, rand(), rand(), (bool) rand()));\n";
    assert_eq!(one_type(src), "dumped type: int<1, max> (asserted)");
}

#[test]
fn factless_exit_degrades_to_declared_floor_in_argument_position() {
    // The (iii) degrade, argument form: the issue-#60 declared arm list, rendered
    // at the dump — observably the same `int` the assignment form reads back.
    let arg_form = "<?php\n\
        function f(int $trigger, int $n, bool $b): int {\n\
            if ($b) {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
            return rand();\n\
        }\n\
        \\PHPStan\\dumpType(f(1, rand(), (bool) rand()));\n";
    assert_eq!(one_type(arg_form), "dumped type: int");
}

#[test]
fn declared_floor_spells_nullable_in_argument_position() {
    // `?int` floor: the argument form spells `int|null` exactly as the assigned
    // form does (the nullable arm is part of the declared envelope, not a bonus).
    let src = "<?php\n\
        function f(int $trigger, ?int $n): ?int {\n\
            return $n;\n\
        }\n\
        \\PHPStan\\dumpType(f(1, rand()));\n";
    assert_eq!(one_type(src), "dumped type: int|null");
}

#[test]
fn no_declared_type_stays_unknown_in_argument_position() {
    // No summary and no declared return type: the floor has nothing to spell, so
    // the dump stays honestly unknown rather than inventing `mixed`.
    let src = "<?php\n\
        function f($t) { return rand(); }\n\
        \\PHPStan\\dumpType(f(1));\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn nested_call_boundary_finding_fires() {
    // Issue #60: `takesInt(g(1))` sees `g`'s proven return value and fires the
    // boundary TypeError — previously invisible without an intermediate variable.
    let src = "<?php\n\
        function g(int $t): string { return \"hi\"; }\n\
        function takesInt(int $n): int { return $n; }\n\
        takesInt(g(1));\n";
    let ds: Vec<Diagnostic> =
        findings(src).into_iter().filter(|d| d.id == "type.argument-mismatch").collect();
    assert_eq!(ds.len(), 1, "the nested call's value reaches the boundary check: {ds:?}");
    assert!(
        ds[0].message.contains("returned from g()"),
        "provenance names the nested call: {}",
        ds[0].message
    );
}

#[test]
fn nested_call_binds_one_level_deep() {
    // `$x = f(g(1))`: `g`'s Singleton summary binds `f`'s parameter, and `f`'s own
    // summary then crosses — one level of nesting, the issue-#60 acceptance bound
    // (the body concatenation is the #59 lane: proven operands, no folder needed).
    let src = "<?php\n\
        function g(int $t): string { return \"hi\"; }\n\
        function f(string $s): string { return $s . \"!\"; }\n\
        $x = f(g(1));\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: 'hi!'");
}

#[test]
fn recursion_in_argument_position_terminates_to_floor() {
    // Self- and mutual recursion through argument position: the on-stack guard
    // (threaded descents) and the plain-pass-only gate (fresh trees) keep both
    // bounded; each degrades to the declared arm floor.
    let self_rec = "<?php\n\
        function r(int $n): int { return r($n); }\n\
        \\PHPStan\\dumpType(r(1));\n";
    assert_eq!(one_type(self_rec), "dumped type: int");
    let mutual = "<?php\n\
        function m1(int $n): int { return m2($n); }\n\
        function m2(int $n): int { return m1($n); }\n\
        \\PHPStan\\dumpType(m1(1));\n";
    assert_eq!(one_type(mutual), "dumped type: int");
}

#[test]
fn ambiguous_simple_name_declines_in_argument_position() {
    // The value IR carries only the call's simple name, so value-position
    // resolution is unique-by-simple (the `resolve_const_fn` precedent): two
    // same-named functions decline — a documented ceiling, pinned so widening it
    // is a decision, not an accident. (The ASSIGNED form still resolves via the
    // statement's `NameRef` — the forms are NOT identical in this corner.)
    let src = "<?php\n\
        namespace A { function d(int $x): string { return \"a\"; } }\n\
        namespace B { function d(int $x): string { return \"b\"; } }\n\
        namespace C {\n\
            \\PHPStan\\dumpType(\\A\\d(1));\n\
        }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn phpdoc_type_dump_reaches_argument_position() {
    // `dumpPhpDocType(f(…))` spells the declared envelope — parity with the
    // assigned form's contract store, same speller.
    let src = "<?php\n\
        function f(int $trigger, int $n): int {\n\
            return rand();\n\
        }\n\
        \\PHPStan\\dumpPhpDocType(f(1, rand()));\n";
    let ds: Vec<Diagnostic> = findings(src)
        .into_iter()
        .filter(|d| d.id == "debug.phpdoc-type")
        .collect();
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].message, "dumped phpdoc type: int");
}

#[test]
fn conditional_polyfill_declines_in_value_position() {
    // A `function_exists`-guarded polyfill: which body binds is a load-order fact
    // (ADR-0049 A2i), so neither the summary nor the floor may speak for it.
    let src = "<?php\n\
        if (!function_exists('poly')) { function poly(int $x): string { return \"p\"; } }\n\
        \\PHPStan\\dumpType(poly(1));\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn namespaced_builtin_homonym_declines_in_value_position() {
    // A namespaced project function shadowing a builtin: the value IR can't see the
    // caller's qualification, and an unqualified call outside the namespace targets
    // the BUILTIN, so the value lane declines (static catalog, folderless). Even
    // this same-namespace call, which really does target the shadow, is declined —
    // conservative on purpose.
    let src = "<?php\n\
        namespace Util;\n\
        function strtoupper(int $x): string { return \"shadow\"; }\n\
        \\PHPStan\\dumpType(strtoupper(1));\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

// `: mixed` is the total envelope — no hint at all, for the summary (issue #364).

#[test]
fn mixed_hint_summarizes_like_no_hint_at_all() {
    // Three spellings of "this callee promises nothing": no hint, `: mixed`, and a
    // `@return mixed` docblock. The body proves `1` under each, and the call site
    // reads the same `1` — the native `: mixed` no longer kills the summary because
    // there is no value it could have refused at the boundary.
    let untyped = "<?php\n\
        function f(int $x) { return $x; }\n\
        \\PHPStan\\dumpType(f(1));\n";
    let native_mixed = "<?php\n\
        function f(int $x): mixed { return $x; }\n\
        \\PHPStan\\dumpType(f(1));\n";
    let phpdoc_mixed = "<?php\n\
        /** @return mixed */\n\
        function f(int $x) { return $x; }\n\
        \\PHPStan\\dumpType(f(1));\n";
    assert_eq!(one_type(native_mixed), "dumped type: 1");
    assert_eq!(one_type(native_mixed), one_type(untyped), "`: mixed` reads as no hint");
    assert_eq!(one_type(native_mixed), one_type(phpdoc_mixed), "native twins the docblock");
}

#[test]
fn mixed_hint_premises_the_boundary_typeerror() {
    // The acceptance case: the proven `1` crossing a `: mixed` boundary is the same
    // premise the hint-less callee supplies, so `takesString()` reports the same
    // strict-mode TypeError with the same provenance.
    let native_mixed = "<?php\n\
        declare(strict_types=1);\n\
        function f(int $x): mixed { return $x; }\n\
        function takesString(string $s): void {}\n\
        takesString(f(1));\n";
    let untyped = "<?php\n\
        declare(strict_types=1);\n\
        function f(int $x) { return $x; }\n\
        function takesString(string $s): void {}\n\
        takesString(f(1));\n";
    let mismatches = |src: &str| -> Vec<String> {
        findings(src)
            .into_iter()
            .filter(|d| d.id == "type.argument-mismatch")
            .map(|d| d.message)
            .collect()
    };
    let under_mixed = mismatches(native_mixed);
    assert_eq!(under_mixed.len(), 1, "the `: mixed` callee premises the boundary: {under_mixed:?}");
    assert!(under_mixed[0].contains("returned from f()"), "{}", under_mixed[0]);
    assert_eq!(under_mixed, mismatches(untyped), "identical to the hint-less spelling");
}

#[test]
fn phpdoc_refines_within_the_mixed_envelope() {
    // `/** @return int */` under `: mixed`: the docblock is a claim ABOUT the proof,
    // not a replacement for it. Before the exemption the refused summary left the
    // caller reading the claim alone (`int (asserted)`); now the proof crosses and
    // the claim covers it, exactly as it does with no native hint.
    let under_mixed = "<?php\n\
        /** @return int */\n\
        function f(int $x): mixed { return $x; }\n\
        \\PHPStan\\dumpType(f(1));\n";
    let untyped = "<?php\n\
        /** @return int */\n\
        function f(int $x) { return $x; }\n\
        \\PHPStan\\dumpType(f(1));\n";
    assert_eq!(one_type(under_mixed), "dumped type: 1");
    assert_eq!(one_type(under_mixed), one_type(untyped), "the claim refines, never replaces");
}

#[test]
fn other_unlowerable_hints_still_refuse_the_summary() {
    // The rest of the refusal list is untouched: `: array` and `: object` lower to no
    // `NativeType` and — unlike `mixed` — really can be violated, so the A2 oracle's
    // silence still means "refuse". The dump stays honestly unknown, and no finding
    // is manufactured out of an exit that would never reach the caller.
    for hint in ["array", "object"] {
        let src = format!(
            "<?php\n\
             declare(strict_types=1);\n\
             function f(int $x): {hint} {{ return $x; }}\n\
             function takesString(string $s): void {{}}\n\
             takesString(f(1));\n\
             \\PHPStan\\dumpType(f(1));\n"
        );
        assert_eq!(one_type(&src), "dumped type: unknown", ": {hint} refuses the summary");
        assert_eq!(count(&src, "type.argument-mismatch"), 0, ": {hint} premises nothing");
    }
}

#[test]
fn void_and_never_hints_still_refuse_the_summary() {
    // `: void` is the deliberate v1 refusal (ADR-0075 §2.4: PHP does hand the caller
    // `NULL`, and the summary still declines to say so); `: never` has no caller-side
    // value at all. Neither is a total envelope, so neither follows `mixed`.
    let void = "<?php\n\
        function f(int $x): void { return; }\n\
        \\PHPStan\\dumpType(f(1));\n";
    let never = "<?php\n\
        function f(int $x): never { throw new \\RuntimeException(); }\n\
        \\PHPStan\\dumpType(f(1));\n";
    assert_eq!(one_type(void), "dumped type: unknown");
    assert_eq!(one_type(never), "dumped type: unknown");
}

#[test]
fn mixed_hint_keeps_the_missing_return_fatal() {
    // The exemption is scoped to the summary: everywhere else `: mixed` is the written
    // hint it is. A body that falls off its end is a runtime `TypeError` under
    // `: mixed` exactly as under `: int`, and the return-missing pair still says so.
    let src = "<?php\n\
        function f(int $x): mixed { $y = $x; }\n";
    assert_eq!(count(src, "type.return-missing"), 1, "{:?}", findings(src));
}

#[test]
fn nested_descent_emits_callee_finding_exactly_once() {
    // A caller-bound proof INSIDE the nested callee: binding `$t = 1` into `g`
    // makes `needsString($t)` a proven strict-mode TypeError. The threaded descent
    // emits it exactly once — the value-lane scratch walks must never double it.
    let src = "<?php\n\
        declare(strict_types=1);\n\
        function needsString(string $s): void {}\n\
        function g(int $t): string { needsString($t); return \"x\"; }\n\
        function f(string $s): string { return $s; }\n\
        $x = f(g(1));\n\
        \\PHPStan\\dumpType($x);\n";
    let ds = findings(src);
    let mismatches: Vec<&Diagnostic> =
        ds.iter().filter(|d| d.id == "type.argument-mismatch").collect();
    assert_eq!(mismatches.len(), 1, "exactly once: {mismatches:?}");
    assert_eq!(one_type(src), "dumped type: 'x'");
}

// The return reader's parity rungs (issue #590): `return <rvalue>` crosses the
// same fact `$v = <rvalue>; return $v;` always did, because `return_value_fact`
// now calls the assignment ladder's own rung functions. Each fixture pins the
// return form against its assignment twin inside the same descent.

/// The two `debug.type` message bodies of a two-dump source, in source order.
fn two_types(src: &str) -> (String, String) {
    let ds: Vec<Diagnostic> =
        findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 2, "expected exactly two debug.type dumps, got {ds:?}");
    (ds[0].message.clone(), ds[1].message.clone())
}

#[test]
fn coalesce_return_binds_like_its_assignment_twin() {
    // `$n ?? 3` under a bound `$n = 5`. The return form used to be a factless exit
    // with no floor (untyped callee) — honest unknown — while the twin crossed via
    // the bare-var rung.
    //
    // The answer is `5`, not the join `3|5`: `$n` is proven set and non-null, so
    // PHP never evaluates the right arm at all (issue #630's settled
    // short-circuit). At `PINNED_PHP` 8.5.9:
    //
    // ```
    // php -r '$n = 5; var_dump($n ?? 3);'
    // int(5)
    // ```
    let src = "<?php\n\
        function viaReturn(int $t, ?int $n) { return $n ?? 3; }\n\
        function viaAssign(int $t, ?int $n) { $v = $n ?? 3; return $v; }\n\
        $a = viaReturn(1, 5);\n\
        \\PHPStan\\dumpType($a);\n\
        $b = viaAssign(1, 5);\n\
        \\PHPStan\\dumpType($b);\n";
    let (ret, asg) = two_types(src);
    assert_eq!(ret, "dumped type: 5");
    assert_eq!(ret, asg, "the return form and its assignment twin agree");
}

#[test]
fn class_const_return_binds_the_fqn_literal() {
    // `Foo::class` is compiler-resolved (issue #236): the written form crosses the
    // FQN string literal, exactly as its assignment twin binds it.
    let src = "<?php\n\
        function viaReturn(int $t) { return \\DateTime::class; }\n\
        function viaAssign(int $t) { $v = \\DateTime::class; return $v; }\n\
        $a = viaReturn(1);\n\
        \\PHPStan\\dumpType($a);\n\
        $b = viaAssign(1);\n\
        \\PHPStan\\dumpType($b);\n";
    let (ret, asg) = two_types(src);
    assert_eq!(ret, "dumped type: 'DateTime'");
    assert_eq!(ret, asg, "the return form and its assignment twin agree");
}

#[test]
fn relative_class_const_crosses_the_method_summary() {
    // `static::class` in a method scope is the `class-string` refinement (the
    // relative form refuses the literal — casing may differ). It crosses through
    // the method summary the same way (issue #386's seam).
    let src = "<?php\n\
        class Rel {\n\
            public function name(int $t) { return static::class; }\n\
        }\n\
        $r = new Rel();\n\
        $x = $r->name(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: class-string");
}

#[test]
fn shape_read_return_crosses_the_declared_slot() {
    // The base is an in-body partial literal — the `rand()` slot keeps `$a` on the
    // abstract stratum — so `return $a['k']` is a constant-key shape read whose
    // slot carries the assert-narrowed positive-int.
    let src = "<?php\n\
        function viaReturn(int $t, int $n) {\n\
            assert($n > 0);\n\
            $a = ['k' => $n, 'r' => rand()];\n\
            return $a['k'];\n\
        }\n\
        function viaAssign(int $t, int $n) {\n\
            assert($n > 0);\n\
            $a = ['k' => $n, 'r' => rand()];\n\
            $v = $a['k'];\n\
            return $v;\n\
        }\n\
        $x = viaReturn(1, rand());\n\
        \\PHPStan\\dumpType($x);\n\
        $y = viaAssign(1, rand());\n\
        \\PHPStan\\dumpType($y);\n";
    let (ret, asg) = two_types(src);
    assert_eq!(ret, "dumped type: int<1, max>");
    assert_eq!(ret, asg, "the return form and its assignment twin agree");
}

#[test]
fn full_literal_array_return_binds_the_list() {
    // The issue's witness in its descendable form (zero-arg factories do not
    // descend in T0, and `: array` refuses the summary — both pinned above): a
    // fully-proven literal array crosses as a `Singleton` and binds whole.
    let src = "<?php\n\
        function retArray(int $t) { return [1, 2]; }\n\
        $x = retArray(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: list{1, 2}");
}

#[test]
fn partial_array_return_binds_the_shape_at_the_binding() {
    // The witness #591 pinned as "the one to flip" (issue #596). A partly-proven
    // literal crosses the exit as a `Fact::Shape`, and the A1 binding vocabulary
    // now admits that layer, so the shape survives the boundary instead of
    // degrading to nothing — for the return form and its assignment twin ALIKE.
    // The untyped callee has no arms to fall back to, which is exactly why the
    // old refusal read as `unknown` rather than as a coarser array.
    let src = "<?php\n\
        function viaReturn(int $t, int $n) { return [1, $n]; }\n\
        function viaAssign(int $t, int $n) { $v = [1, $n]; return $v; }\n\
        $a = viaReturn(1, rand());\n\
        \\PHPStan\\dumpType($a);\n\
        $b = viaAssign(1, rand());\n\
        \\PHPStan\\dumpType($b);\n";
    let (ret, asg) = two_types(src);
    assert_eq!(ret, "dumped type: list{1, int}");
    assert_eq!(ret, asg, "the crossing is the same on both forms");
}

#[test]
fn shape_summary_keeps_the_stratum_it_was_joined_at() {
    // A4 across the array stratum: the binding copies the summary's stratum, it
    // does not mint one. The element is narrowed by a `@phpstan-assert` helper, so
    // the literal's own `min` (ADR-0061 §3) is `Asserted` — and the caller renders
    // `(asserted)`, both for the whole shape and for the field projected out of it.
    // That marker is what keeps ADR-0062 A-G9's corollary true across a boundary:
    // an `Asserted` shape premises `phpdoc.maybe-*` and never the `type.*` sibling.
    let src = "<?php\n\
        /** @phpstan-assert positive-int $v */\n\
        function assertPos($v): void {}\n\
        function f(int $t, $n) { assertPos($n); return ['k' => $n]; }\n\
        $a = f(1, rand());\n\
        \\PHPStan\\dumpType($a);\n\
        \\PHPStan\\dumpType($a['k']);\n";
    let (whole, field) = two_types(src);
    assert_eq!(whole, "dumped type: array{k: int<1, max>} (asserted)");
    assert_eq!(field, "dumped type: int<1, max> (asserted)");
}

#[test]
fn bound_shape_reaches_the_caller_s_shape_consumers() {
    // The point of the crossing: what binds is the value lane every shape consumer
    // reads (ADR-0062 §4), so the consumers answer at the call site exactly what
    // they answer one statement later on the local twin. PARITY is the assertion,
    // not any particular answer — a shape that crossed but read differently would
    // be a second array vocabulary, which is what ADR-0071 forbids. The read is
    // pinned outright because it is the one this slice exists for; `count` rides
    // along to prove the parity is not read-shaped, and whatever the shape-builtin
    // rung answers there it answers identically on both sides.
    let src = "<?php\n\
        function rows(int $t, int $n) { return ['a' => $n, 'b' => 2]; }\n\
        function viaCall(int $n) {\n\
            $r = rows(1, $n);\n\
            \\PHPStan\\dumpType($r['b']);\n\
            \\PHPStan\\dumpType(count($r));\n\
        }\n\
        function viaLocal(int $n) {\n\
            $r = ['a' => $n, 'b' => 2];\n\
            \\PHPStan\\dumpType($r['b']);\n\
            \\PHPStan\\dumpType(count($r));\n\
        }\n";
    let ds: Vec<String> = findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(ds.len(), 4, "expected four debug.type dumps, got {ds:?}");
    assert_eq!(ds[0], "dumped type: 2", "the constant-key read projects the crossed slot");
    assert_eq!(ds[..2], ds[2..], "the call site reads what the local twin reads");
}

#[test]
fn general_summary_still_defers_to_the_arm_lane() {
    // The `General` half of issue #596, decided by measurement and pinned so the
    // measurement cannot rot. Admitting `General` into `summary_binds` is SOUND —
    // the body really did prove `string` here — but it binds into a lane that
    // EVICTS the arm lane, and for this layer the arms are the richer carrier: the
    // `@return class-string` a body-proved `General{string}` would replace is
    // strictly sharper than the fact replacing it. So `General` stays filtered and
    // the docblock arm answers, exactly as before the shape layer joined — and it
    // answers `(asserted)`, which is the whole trade in one rendering: a Verified
    // `string` is not worth an Asserted `class-string`.
    let src = "<?php\n\
        class Holder {\n\
            /** @return class-string */\n\
            public function name(int $t, string $s): string { return $s; }\n\
        }\n\
        $h = new Holder();\n\
        $x = $h->name(1, (string) rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: class-string (asserted)");
}

#[test]
fn undecided_comparison_return_degrades_like_the_floor() {
    // The comparison rung is total: an undecided `$n > 3` crosses the Verified
    // `bool` floor, which carries nothing beyond the arms — `: bool` renders the
    // floor, an untyped callee stays honestly unknown. A decided comparison
    // crosses its verdict.
    let untyped = "<?php\n\
        function f(int $t, int $n) { return $n > 3; }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    let hinted = "<?php\n\
        function f(int $t, int $n): bool { return $n > 3; }\n\
        $x = f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    let decided = "<?php\n\
        function f(int $t, ?int $n) { return $n === 5; }\n\
        $x = f(1, 5);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(untyped), "dumped type: unknown");
    assert_eq!(one_type(hinted), "dumped type: bool");
    assert_eq!(one_type(decided), "dumped type: true");
}
