//! Issue #430 — a `match` in **value position** gets its arms walked.
//!
//! Until this slice only a statement-position `match` reached the structured
//! `StmtKind::Match`: `Expression::Match` was recognized by the expression-
//! statement lowering and nowhere else, so `$r = match (…)`, `return match (…)`,
//! `echo match (…)` and a `match` handed to a call collapsed whole. Their arm
//! bodies were never walked, no first-match certainty was computed over them, and
//! every diagnostic inside an arm was invisible — the form nearly all real code
//! uses was the form nothing was analyzed in.
//!
//! What this file pins is that the four value positions now answer exactly what
//! statement position already answered, and that **only** that changed:
//!
//! * an arm body reports, with the subject refined to the arm's literal;
//! * a decided-No arm and a decided-No `default` are dead, so what they contain
//!   is silent;
//! * the **value** the `match` expression produces is untouched — joining the arm
//!   values into the enclosing expression's result is a real win and a different
//!   slice, and `value_of_the_match_expression_is_unchanged` is the fixture that
//!   keeps this one from drifting into it;
//! * all-or-nothing structuring survives: an unlowerable subject or arm condition
//!   still buys nothing at all, because partial structuring is unsound for the
//!   first-match and no-`default`-throws rules;
//! * `default => assertNever($foo)` over an exhausted domain stays silent now
//!   that the arm is walked (ADR-0088 §4 / issue #428) — the sequencing reason
//!   that slice had to land first.
//!
//! Guard-shaped arm conditions (`match (true) { is_string($f) => … }`) are still
//! refused by `usable_operand` and still buy nothing; that is issue #431.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, NEVER_PARAM_REACHABLE_ID, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

/// Header: a `void` sink to hand a `match` to in argument position, and a
/// non-lowerable operand source for the all-or-nothing fixtures.
const HDR: &str =
    "<?php\nfunction sink(mixed $v): void {}\nfunction helper(): string { return 'a'; }\n";

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

/// Every `\PHPStan\dumpType()` message the source emitted, in order. An empty
/// vector is the "not walked at all" answer this file tests for as often as it
/// tests for a type.
fn dumps(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.clone())
        .collect()
}

/// One function body, with a `string $x` to match on.
fn body(stmts: &str) -> String {
    format!("{HDR}function f(string $x): void {{\n{stmts}\n}}\n")
}

// ---- The four value positions ------------------------------------------

#[test]
fn assignment_rhs_arms_are_walked_and_refined() {
    // The measured symptom of the bug: the dump one line above reported, the two
    // inside the arms reported nothing at all.
    assert_eq!(
        dumps(&body(
            "\\PHPStan\\dumpType($x);\n$r = match ($x) { 'a' => \\PHPStan\\dumpType($x), default => \\PHPStan\\dumpType($x) };"
        )),
        vec!["dumped type: string", "dumped type: 'a'", "dumped type: string"],
        "the arm refines the subject to its own literal; the default refines nothing"
    );
}

#[test]
fn return_position_arms_are_walked() {
    let src = format!(
        "{HDR}function f(string $x): int {{\n\treturn match ($x) {{ 'a' => \\PHPStan\\dumpType($x), default => 0 }};\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: 'a'"]);
}

#[test]
fn echo_position_arms_are_walked() {
    assert_eq!(
        dumps(&body("echo match ($x) { 'a' => \\PHPStan\\dumpType($x), default => '' };")),
        vec!["dumped type: 'a'"]
    );
}

#[test]
fn argument_position_arms_are_walked() {
    assert_eq!(
        dumps(&body("sink(match ($x) { 'a' => \\PHPStan\\dumpType($x), default => 0 });")),
        vec!["dumped type: 'a'"]
    );
}

#[test]
fn a_match_nested_in_an_arm_body_is_walked_too() {
    // The arm body is a statement position by any other name, so the hoist runs
    // inside it as well: the inner `match` binds `$y` where the outer bound `$x`.
    assert_eq!(
        dumps(&format!(
            "{HDR}function f(string $x, string $y): void {{\n\t$r = match ($x) {{ 'a' => $z = match ($y) {{ 'b' => \\PHPStan\\dumpType($y), default => 0 }}, default => 0 }};\n}}\n"
        )),
        vec!["dumped type: 'b'"]
    );
}

// ---- Deadness ----------------------------------------------------------

#[test]
fn a_decided_no_arm_is_dead_and_silent() {
    // `$x` is `'a'`, so the `'b'` arm provably does not match. Statement position
    // has always pruned it; value position now does too.
    assert_eq!(
        dumps(&body(
            "$x = 'a';\n$r = match ($x) { 'a' => \\PHPStan\\dumpType($x), 'b' => \\PHPStan\\dumpType($x) };"
        )),
        vec!["dumped type: 'a'"],
        "only the live arm reports"
    );
}

#[test]
fn a_dead_default_is_silent() {
    // A decided arm consumes the value, so the `default` is unreachable.
    assert_eq!(
        dumps(&body(
            "$x = 'a';\n$r = match ($x) { 'a' => 1, default => \\PHPStan\\dumpType($x) };"
        )),
        Vec::<String>::new(),
        "a default a decided arm shadows is dead → its dump never fires"
    );
}

// ---- Reachability, which arm walking now decides ------------------------

#[test]
fn a_match_whose_every_arm_throws_terminates_the_trace() {
    // `type.return-maybe-missing` fired here before the arms were walked: the
    // body's only statement is an assignment, which falls through as far as
    // `stmt_end` was concerned. It does not — every arm throws — and the hoisted
    // entry carries `match_end`'s answer, which says so.
    let src = format!(
        "{HDR}function f(string $x): int {{\n\t$r = match ($x) {{ 'a' => throw new LogicException(), default => throw new LogicException() }};\n}}\n"
    );
    let ds = findings(&src);
    assert!(ds.is_empty(), "a body that always throws misses no return, got: {ds:?}");
}

#[test]
fn a_no_default_match_that_misses_every_arm_kills_the_tail() {
    // PHP raises `\UnhandledMatchError`, so the statement after is unreachable and
    // the proven-bad call it holds is silent — the answer statement position has
    // always given.
    let src = format!(
        "{HDR}function f(): void {{\n\t$x = 'z';\n\t$r = match ($x) {{ 'a' => 1, 'b' => 2 }};\n\tsink(\\PHPStan\\dumpType($x));\n}}\n"
    );
    assert_eq!(dumps(&src), Vec::<String>::new(), "the tail after an unhandled match is dead");
}

// ---- The value question, deliberately out of scope ----------------------

#[test]
fn value_of_the_match_expression_is_unchanged() {
    // Every arm of this `match` yields an `int` literal, and the assigned `$r` is
    // STILL unknown: this slice walks arms, it does not join their values. The
    // day that changes, this fixture is the one that has to be edited on purpose.
    assert_eq!(
        dumps(&body("$r = match ($x) { 'a' => 1, default => 2 };\n\\PHPStan\\dumpType($r);")),
        vec!["dumped type: unknown"],
        "the enclosing expression's own value lane is untouched by arm walking"
    );
}

// ---- All-or-nothing structuring ----------------------------------------

#[test]
fn an_unlowerable_subject_buys_nothing() {
    assert_eq!(
        dumps(&body("$r = match (helper()) { 'a' => \\PHPStan\\dumpType($x), default => 0 };")),
        Vec::<String>::new(),
        "a call subject does not lower → the whole construct stays Opaque"
    );
}

#[test]
fn an_unlowerable_arm_condition_buys_nothing() {
    assert_eq!(
        dumps(&body("$r = match ($x) { helper() => \\PHPStan\\dumpType($x), default => 0 };")),
        Vec::<String>::new(),
        "one unrepresentable arm opaques the whole construct, arms included"
    );
}

#[test]
fn a_guard_shaped_arm_condition_still_buys_nothing() {
    // `match (true) { is_string($f) => … }` — the arm condition is a call, so the
    // construct is unstructured exactly as before. Issue #431's subject.
    assert_eq!(
        dumps(&body(
            "$r = match (true) { is_string($x) => \\PHPStan\\dumpType($x), default => 0 };"
        )),
        Vec::<String>::new(),
        "a guard arm is not a literal — refused here, and still refused"
    );
}

// ---- The sentinel idiom the arm walk newly exposes (ADR-0088 §4) --------

const SENTINEL: &str =
    "/** @param never $value */\nfunction assertNever(mixed $value): never { throw new LogicException(); }\n";

#[test]
fn a_default_assert_never_over_an_exhausted_domain_stays_silent() {
    // The idiom ADR-0088 exists to protect, in the form this slice makes visible
    // to the argument checkers for the first time. Nothing may fire on it.
    let src = format!(
        "<?php\n{SENTINEL}/** @param 'a'|'b' $foo */\nfunction h(string $foo): void {{\n\t$r = match ($foo) {{\n\t\t'a' => 1,\n\t\t'b' => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    let noisy: Vec<Diagnostic> = findings(&src)
        .into_iter()
        .filter(|d| d.id == NEVER_PARAM_REACHABLE_ID || d.id == PARAM_MISMATCH_ID)
        .collect();
    assert!(noisy.is_empty(), "the sentinel arm must stay silent, got: {noisy:?}");
}

// ---- The residue this slice inherits, recorded rather than papered over --

#[test]
fn the_default_arm_reads_an_unsubtracted_subject_exactly_as_statement_position_does() {
    // `walk_match` refines the subject inside a conditional arm and refines
    // NOTHING on the default path: a `default` reached only because `null` was
    // consumed above it still sees `string|null`, and the strict-floor possibly
    // leg says so. That is wrong, it is wrong identically in statement position
    // (where it has always been wrong), and it is not this slice's to fix —
    // subtracting the arms' literals from the subject on the no-match path is one
    // change that has to land for both positions at once.
    //
    // What this fixture guards is the ONLY property value position owes here: it
    // introduces no asymmetry. Whatever the statement form answers, the value
    // form answers too. Zero occurrences of the shape across the 6670-file
    // fp-gate corpus, measured on this branch.
    let src = format!(
        "{HDR}function name(string $s): string {{ return $s; }}\nfunction stmt(?string $s): void {{\n\tmatch ($s) {{ null => 'none', default => name($s) }};\n}}\nfunction value(?string $s): string {{\n\treturn match ($s) {{ null => 'none', default => name($s) }};\n}}\n"
    );
    let maybe: Vec<Diagnostic> =
        findings(&src).into_iter().filter(|d| d.id.contains("maybe-argument")).collect();
    assert_eq!(
        maybe.len(),
        2,
        "one per position — the value form inherits the statement form's residue, no more: {maybe:?}"
    );
}

#[test]
fn an_exhaustive_match_over_a_known_subject_reports_nothing_at_all() {
    // A whole-file adversarial probe: every arm live-or-dead by the analyzer's own
    // verdict, a sentinel default, and no finding of any id.
    let src = format!(
        "<?php\n{SENTINEL}/** @param 'a'|'b'|'c' $foo */\nfunction h(string $foo): string {{\n\treturn match ($foo) {{\n\t\t'a' => 'A',\n\t\t'b' => 'B',\n\t\t'c' => 'C',\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    let ds = findings(&src);
    assert!(ds.is_empty(), "an exhaustive value-position match is silent, got: {ds:?}");
}
