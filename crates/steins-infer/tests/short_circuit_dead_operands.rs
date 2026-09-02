//! Short-circuit refinement, the reachability half (ADR-0052 §6 / issue #266
//! slice 1): the spans PHP's own evaluation order proves are **never evaluated**.
//!
//! §6 promised the direct env-free pass would stand down on spans `mark_dead`
//! already models, but the threading slice (N3) delivered only the verdict half.
//! The gap was a live false-positive class: `$x === 2 && f("bad")` reported
//! inside a short-circuited operand, `$c ? f("bad") : 0` inside an arm a decided
//! guard never takes, `$x ?? f("bad")` inside a right operand a proven-present
//! left never reaches.
//!
//! Every test here is a PAIR: the decided form must be silent, and its undecided
//! twin must still fire (the suppression is the decision, not a blanket). Every
//! movement this file pins is **finding-removing** — no test here gains a finding.

use steins_infer::{Diagnostic, ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    // Drop `untyped.*`: it flags the fixtures' own deliberately bare declarations.
    check(&tree, &functions, "demo.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn mismatches(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == ID).count()
}

const HDR: &str = "<?php\ndeclare(strict_types=1);\nfunction takesInt(int $n): bool { return true; }\n";

// `&&` — the right operand under a decided-false left.

#[test]
fn and_right_operand_is_not_evaluated_when_left_is_false() {
    let src = format!(
        "{HDR}function f(): void {{ $x = 1; if ($x === 2 && takesInt(\"abc\")) {{ echo 1; }} }}"
    );
    assert_eq!(
        mismatches(&src),
        0,
        "`$x === 2` is proven false, so PHP never evaluates the right operand — \
         a finding there is an FP"
    );
}

#[test]
fn and_right_operand_control_fires_when_left_is_undecided() {
    // The control: an undecided left operand leaves the right one live, and the
    // literal-argument break is reported exactly as it always was.
    let src =
        format!("{HDR}function f(bool $b): void {{ if ($b && takesInt(\"abc\")) {{ echo 1; }} }}");
    assert_eq!(mismatches(&src), 1, "an undecided left operand keeps the right one live");
}

// `||` — the right operand under a decided-true left (De Morgan mirror).

#[test]
fn or_right_operand_is_not_evaluated_when_left_is_true() {
    let src = format!(
        "{HDR}function f(): void {{ $x = 1; if ($x === 1 || takesInt(\"abc\")) {{ echo 1; }} }}"
    );
    assert_eq!(mismatches(&src), 0, "`$x === 1` is proven true — the `||` right operand never runs");
}

#[test]
fn or_right_operand_control_fires_when_left_is_undecided() {
    let src =
        format!("{HDR}function f(bool $b): void {{ if ($b || takesInt(\"abc\")) {{ echo 1; }} }}");
    assert_eq!(mismatches(&src), 1, "an undecided left operand keeps the `||` right operand live");
}

// Ternary arms — exactly one of the two runs.

#[test]
fn ternary_then_arm_is_not_evaluated_under_a_false_guard() {
    let src =
        format!("{HDR}function f(): void {{ $x = 1; $y = $x === 2 ? takesInt(\"abc\") : false; }}");
    assert_eq!(mismatches(&src), 0, "a proven-false guard never evaluates the then-arm");
}

#[test]
fn ternary_else_arm_is_not_evaluated_under_a_true_guard() {
    let src =
        format!("{HDR}function f(): void {{ $x = 1; $y = $x === 1 ? false : takesInt(\"abc\"); }}");
    assert_eq!(mismatches(&src), 0, "a proven-true guard never evaluates the else-arm");
}

#[test]
fn ternary_arms_control_fires_under_an_undecided_guard() {
    // Both arms live: the undecided guard suppresses neither, and each bad call
    // is reported once.
    let src = format!(
        "{HDR}function f(bool $b): void {{ $y = $b ? takesInt(\"abc\") : takesInt(\"def\"); }}"
    );
    assert_eq!(mismatches(&src), 2, "an undecided guard leaves both arms live");
}

// `??` — the right operand under a proven set-and-non-null left.

#[test]
fn coalesce_right_operand_is_not_evaluated_when_left_is_proven_present() {
    let src = format!("{HDR}function f(): void {{ $x = 1; $y = $x ?? takesInt(\"abc\"); }}");
    assert_eq!(mismatches(&src), 0, "a proven non-null left operand never reaches the `??` right");
}

#[test]
fn coalesce_right_operand_control_fires_when_left_is_proven_null() {
    // The mirror control: a left operand proven NULL means the right one is the
    // value — unambiguously live, and the finding stands.
    let src = format!("{HDR}function f(): void {{ $x = null; $y = $x ?? takesInt(\"abc\"); }}");
    assert_eq!(mismatches(&src), 1, "a proven-null left operand makes the `??` right operand live");
}

#[test]
fn coalesce_right_operand_control_fires_when_left_is_unknown() {
    let src = format!("{HDR}function f(?int $a): void {{ $y = $a ?? takesInt(\"abc\"); }}");
    assert_eq!(mismatches(&src), 1, "an undecided left operand keeps the `??` right operand live");
}

// The stratum pin: reachability stays proof-only.

#[test]
fn an_asserted_left_operand_does_not_silence_the_coalesce_right() {
    // `@phpstan-assert int $x` is a CLAIM (ADR-0052 §5), and reachability is
    // proof-only — an Asserted "present" must NOT stand the direct pass down, or a
    // lying tag could buy silence on a live path, the one thing this rule prevents.
    let src = format!(
        "{HDR}/** @phpstan-assert int $v */
function assertInt(mixed $v): void {{}}
function f(mixed $x): void {{ assertInt($x); $y = $x ?? takesInt(\"abc\"); }}"
    );
    assert_eq!(
        mismatches(&src),
        1,
        "an Asserted presence must not mark the `??` right operand dead — silence on \
         a live path is exactly what a lying tag must not buy"
    );
}

// The nested / repeated shapes.

#[test]
fn nested_and_chain_suppresses_only_past_the_decided_point() {
    // `$x === 1 && $x === 2 && takesInt("abc")`: the SECOND operand is live (the
    // first is true), decides No, and the third is then unevaluated.
    let src = format!(
        "{HDR}function f(): void {{ $x = 1; if ($x === 1 && $x === 2 && takesInt(\"abc\")) {{ echo 1; }} }}"
    );
    assert_eq!(mismatches(&src), 0, "the chain short-circuits before the third operand");
}

#[test]
fn a_dead_operand_does_not_silence_the_same_call_elsewhere() {
    // Span-keyed, not call-keyed: an identical call on a live path keeps firing.
    let src = format!(
        "{HDR}function f(): void {{ $x = 1; if ($x === 2 && takesInt(\"abc\")) {{ echo 1; }} takesInt(\"abc\"); }}"
    );
    assert_eq!(mismatches(&src), 1, "suppression is per-span; the live call still fires");
}

// The ternary at the DUMP seam (issue #625 leg 1). `\PHPStan\dumpType($c ? A :
// B)` reads the same evaluator the assignment above does, and marks the untaken
// arm the same way — the deadness is a fact about PHP's evaluation order, not
// about which seam noticed it, so declining to mark here would make the same
// expression report differently for being written inside a dump.

#[test]
fn a_dumped_ternarys_else_arm_is_not_evaluated_under_a_true_guard() {
    let src = format!(
        "{HDR}function f(): void {{ $x = 1; \\PHPStan\\dumpType($x === 1 ? false : takesInt(\"abc\")); }}"
    );
    assert_eq!(mismatches(&src), 0, "a dumped ternary's untaken arm never runs either");
}

#[test]
fn a_dumped_ternarys_then_arm_is_not_evaluated_under_a_false_guard() {
    let src = format!(
        "{HDR}function f(): void {{ $x = 1; \\PHPStan\\dumpType($x === 2 ? takesInt(\"abc\") : false); }}"
    );
    assert_eq!(mismatches(&src), 0, "a dumped ternary's untaken arm never runs either");
}

#[test]
fn a_dumped_ternarys_arms_control_fires_under_an_undecided_guard() {
    // The pair's other half: the dump seam suppresses the decision, not the arms.
    let src = format!(
        "{HDR}function f(bool $b): void {{ \\PHPStan\\dumpType($b ? takesInt(\"abc\") : takesInt(\"def\")); }}"
    );
    assert_eq!(mismatches(&src), 2, "an undecided guard leaves both dumped arms live");
}
