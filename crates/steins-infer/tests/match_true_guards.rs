//! Issue #431 — `match (true) { is_string($foo) => …, is_int($foo) => … }` is an
//! `if`/`elseif` chain written in `match` syntax, and Steins reads it as one.
//!
//! Until this slice the construct was not structured at all: every arm condition
//! had to lower to a variable or a literal, so a *call* in condition position —
//! which is what a guard is — refused the whole `match` to `Opaque`. Every cell of
//! ADR-0088's worked example but the literal-arm one was invisible for that reason.
//!
//! The lowering now offers a refused `match` a second shape before giving up: with
//! a literal `true`/`false` subject, each arm condition lowers through `lower_cond`
//! — the very lowering the `if` path uses — and the construct becomes a
//! `StmtKind::If` whose links are the arms in source order and whose `else` is the
//! `default`. That is not a shortcut, it is the claim: first-match order *is*
//! `elseif` order, so the arm-wise subtraction of ADR-0052, the guard vocabulary,
//! the dead-branch marking and the join all arrive as the `if` path's rather than
//! as a second implementation that could disagree with it.
//!
//! What the fixtures pin, in order: the arms' own narrowing; the accumulated
//! subtraction later arms and the `default` inherit; `match (false)`; the three
//! refusals (non-boolean subject, truthy-valued arm condition, and — unchanged —
//! the class-constant arm); the inexpressible guard that subtracts nothing; and
//! ADR-0088 §8's cells staying silent.
//!
//! Two measured results are recorded here rather than smoothed over.
//!
//! * **Which lane prints the empty domain depends on the lane.** An exhausted
//!   *enum* subject dumps as `*NEVER*` (ADR-0052's 2026-08-18 note: an emptied
//!   all-`Verified` lane is kept, empty, and readable). An exhausted `string|int`
//!   dumps as `unknown` — the scalar value lane has no empty domain to print, so
//!   there a subtraction that removed everything reads the same as one that was
//!   never made. That is not this slice's doing: the `if` chain of each shape
//!   prints exactly the same thing, which is the property the slice actually owes
//!   (`the_if_chain_of_the_same_shape_answers_identically`). Where the value lane
//!   cannot tell the two apart, the contract lane the sentinel reads can, and
//!   `an_exhausted_chain_is_silent_and_a_partial_one_is_not` is the pair that
//!   proves the subtraction really landed.
//! * The no-match path of a `default`-less guard chain falls through to the
//!   statement after it, where a `default`-less by-value `match` terminates
//!   (`\UnhandledMatchError`). The `if` shape has no throw to model and inventing a
//!   `StmtKind::Throw` here would feed the throw accounting a contribution
//!   ADR-0088 §5 has not yet ruled on. Falling through only ever widens the join,
//!   so it costs precision and claims nothing.
//!
//! Issue #439 — the no-match subtraction for `match`/`switch` — sits directly
//! beneath this slice and does not touch it: it refines the no-match path inside
//! `walk_match`, and a desugared guard chain never reaches `walk_match`. Its
//! `default` is an `else`, and an `else` has carried the accumulated negation
//! since ADR-0031. Measured: every fixture in this file answers identically with
//! #439 beneath it and without it.
//!
//! One **false positive** is pinned here rather than hidden
//! (`a_chain_whose_later_arm_re_narrows_reports_where_php_reaches_nothing`): a
//! guard's positive refinement replaces the lane instead of intersecting it with
//! what the guards above left, so `$v === 1 => …, $v !== 1 => …` ends with the
//! `default` reading `1` on a path PHP never takes. The `if`/`elseif` spelling
//! reports it identically and did so before this slice existed — which is the
//! desugaring being faithful, not the desugaring being wrong — but `match (true)`
//! syntax newly reaches it, so it is pinned in both spellings, and the day the
//! positive side learns to intersect, both fixtures flip together.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, NEVER_PARAM_REACHABLE_ID, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

const SENTINEL: &str =
    "/** @param never $value */\nfunction assertNever(mixed $value): never { throw new LogicException(); }\n";

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

/// Every `\PHPStan\dumpType()` message the source emitted, in order. An empty
/// vector is the "not structured at all" answer, which several fixtures below want.
fn dumps(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.clone())
        .collect()
}

/// One function over a `string|int $foo`, with the sentinel in scope.
fn over_union(body: &str) -> String {
    format!("<?php\n{SENTINEL}function f(string|int $foo): void {{\n{body}\n}}\n")
}

/// The `phpdoc.never-param-reachable` findings a source emits — the sentinel of
/// ADR-0088 §4, and the only surface on which "the subtraction emptied the lane"
/// is observable at all.
fn sentinel(src: &str) -> Vec<Diagnostic> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == NEVER_PARAM_REACHABLE_ID || d.id == PARAM_MISMATCH_ID)
        .collect()
}

// ---- The worked example, end to end ------------------------------------

#[test]
fn guard_arms_narrow_and_the_default_sees_the_accumulated_subtraction() {
    // ADR-0088's `f1`, the demo this slice exists for. Before it, all three dumps
    // were silent — the construct was one `Opaque` statement.
    assert_eq!(
        dumps(&over_union(
            "\techo match (true) {\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t\tis_int($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};"
        )),
        vec!["dumped type: string", "dumped type: int", "dumped type: unknown"],
        "each arm narrows to its own guard; the default's `unknown` is the value lane's \
         spelling of a domain the subtraction emptied"
    );
}

#[test]
fn the_if_chain_of_the_same_shape_answers_identically() {
    // The property the slice owes: `match (true)` is not a second, weaker reading
    // of a guard chain — it is the same reading. If this pair ever diverges, the
    // desugaring has grown a special case it should not have.
    let chain = "\tif (is_string($foo)) { \\PHPStan\\dumpType($foo); }\n\telseif (is_int($foo)) { \\PHPStan\\dumpType($foo); }\n\telse { \\PHPStan\\dumpType($foo); }";
    let arms = "\techo match (true) {\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t\tis_int($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};";
    assert_eq!(dumps(&over_union(chain)), dumps(&over_union(arms)));
}

#[test]
fn a_partial_chain_leaves_the_default_the_residue() {
    // One guard, so the `default` sees exactly what it did not cover — the
    // subtraction is visible in the value lane whenever it does not empty it.
    assert_eq!(
        dumps(&over_union(
            "\tmatch (true) {\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};"
        )),
        vec!["dumped type: string", "dumped type: int"],
        "statement position, one arm subtracted"
    );
}

#[test]
fn a_multi_condition_arm_subtracts_the_whole_disjunction() {
    // `a, b => …` takes the arm when either holds, so the arms below it inherit
    // `!a && !b`. The arm itself narrows nothing (a disjunction of two predicates
    // is not one fact), which is the `if (a || b)` answer, unchanged.
    assert_eq!(
        dumps(
            "<?php\nfunction f(string|int|float $foo): void {\n\techo match (true) {\n\t\tis_string($foo), is_int($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};\n}\n"
        ),
        vec!["dumped type: int|float|string", "dumped type: float"]
    );
}

#[test]
fn match_false_inverts_the_sense() {
    // `match (false)` takes the arm whose condition is FALSE, so the first arm is
    // the `!is_string` branch and the `default` is the one that saw a string.
    assert_eq!(
        dumps(&over_union(
            "\techo match (false) {\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};"
        )),
        vec!["dumped type: int", "dumped type: string"]
    );
}

#[test]
fn an_enum_identity_chain_narrows_and_subtracts_cases() {
    // The finite Verified domain of #429, reached through `match` syntax: the arm
    // holds one case and the `default` holds what is left of the case set.
    assert_eq!(
        dumps(
            "<?php\nenum Suit { case Hearts; case Spades; }\nfunction f(Suit $s): void {\n\techo match (true) {\n\t\t$s === Suit::Hearts => \\PHPStan\\dumpType($s),\n\t\tdefault => \\PHPStan\\dumpType($s),\n\t};\n}\n"
        ),
        vec!["dumped type: Suit::Hearts", "dumped type: Suit::Spades"]
    );
}

#[test]
fn an_exhausted_enum_domain_dumps_as_never() {
    // The one lane that can print an emptied domain, so the one place the
    // accumulated subtraction is directly legible: cover every case and the
    // `default` holds nothing at all. The `if` chain of the same shape says the
    // same word, which is the point.
    let arms = "<?php\nenum Suit { case Hearts; case Spades; }\nfunction f(Suit $s): void {\n\techo match (true) {\n\t\t$s === Suit::Hearts => 1,\n\t\t$s === Suit::Spades => 2,\n\t\tdefault => \\PHPStan\\dumpType($s),\n\t};\n}\n";
    let chain = "<?php\nenum Suit { case Hearts; case Spades; }\nfunction f(Suit $s): void {\n\tif ($s === Suit::Hearts) { echo 1; }\n\telseif ($s === Suit::Spades) { echo 2; }\n\telse { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(arms), vec!["dumped type: *NEVER*"]);
    assert_eq!(dumps(chain), dumps(arms));
}

#[test]
fn a_conjunctive_guard_narrows_as_it_does_in_an_if() {
    // `&&` and `||` yield `bool` whatever their operands are, so a composed guard
    // is accepted unconditionally and refines through the same walk.
    assert_eq!(
        dumps(
            "<?php\nfunction f(?string $a): void {\n\techo match (true) {\n\t\t$a !== null && $a !== '' => \\PHPStan\\dumpType($a),\n\t\tdefault => \\PHPStan\\dumpType($a),\n\t};\n}\n"
        ),
        vec!["dumped type: non-empty-string", "dumped type: string|null"]
    );
}

// ---- The three refusals, each all-or-nothing ---------------------------

#[test]
fn a_non_boolean_subject_stays_unstructured() {
    // `match ($k) { is_string($foo) => … }` compares `$k` against the *result* of
    // `is_string($foo)`. That is a comparison, not a guard chain, and reading it as
    // one would narrow `$foo` on a branch PHP selected by `$k`.
    assert_eq!(
        dumps(
            "<?php\nfunction f(string|int $foo, int $k): void {\n\techo match ($k) {\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};\n}\n"
        ),
        Vec::<String>::new(),
        "an integer subject buys nothing, arms included"
    );
    // A literal non-boolean subject is the same refusal, not a special case.
    assert_eq!(
        dumps(
            "<?php\nfunction f(string|int $foo): void {\n\techo match (1) {\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};\n}\n"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_truthy_valued_arm_condition_refuses_the_whole_construct() {
    // `match (true)` compares with `===`, so an arm whose condition is not
    // boolean-valued is not the arm's truth: `$n = 5` matches nothing here, and
    // reading the residue as "`$n` is falsy" would be a narrowing PHP never proved.
    // One such arm opaques the construct — including the guards beside it.
    assert_eq!(
        dumps(
            "<?php\nfunction f(string|int $foo, int $n): void {\n\techo match (true) {\n\t\t$n => \\PHPStan\\dumpType($foo),\n\t\tis_int($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => 0,\n\t};\n}\n"
        ),
        Vec::<String>::new(),
        "all-or-nothing: a bare-variable arm takes the guards down with it"
    );
}

#[test]
fn a_class_constant_arm_is_refused_exactly_as_before() {
    // `usable_operand`'s explicit `ClassConst` refusal is untouched by this slice,
    // and the guard chain does not smuggle it back in: `Suit::Hearts` lowers to a
    // truthiness test over a class constant, which the shape above refuses.
    assert_eq!(
        dumps(
            "<?php\nenum Suit { case Hearts; case Spades; }\nfunction f(Suit $s): void {\n\techo match (true) {\n\t\tSuit::Hearts => \\PHPStan\\dumpType($s),\n\t\tdefault => \\PHPStan\\dumpType($s),\n\t};\n}\n"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_by_value_match_on_a_boolean_subject_is_unchanged() {
    // The by-value shape is tried first, so `match (true) { true => …, false => … }`
    // is still the boolean-subject `match` it always was — the guard chain is only
    // ever reached where the answer used to be `Opaque`.
    assert_eq!(
        dumps(
            "<?php\nfunction f(bool $b): void {\n\techo match ($b) {\n\t\ttrue => \\PHPStan\\dumpType($b),\n\t\tfalse => \\PHPStan\\dumpType($b),\n\t};\n}\n"
        ),
        vec!["dumped type: true", "dumped type: false"]
    );
}

// ---- The guard whose narrowing is inexpressible ------------------------

#[test]
fn an_inexpressible_guard_subtracts_nothing_and_the_arm_is_still_walked() {
    // `cool($foo)` narrows nothing in either direction. The arm is walked — the
    // dump inside it fires — and the `default` is handed no subtraction at all.
    // (`unknown` on both sides is the guard call's own by-ref invalidation, the
    // answer `if (cool($foo))` gives; what matters here is that neither side
    // claims a narrowing.)
    assert_eq!(
        dumps(
            "<?php\nfunction cool(mixed $v): bool { return true; }\nfunction f(string|int $foo): void {\n\techo match (true) {\n\t\tcool($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t};\n}\n"
        ),
        vec!["dumped type: unknown", "dumped type: unknown"]
    );
}

#[test]
fn an_inexpressible_guard_does_not_let_the_sentinel_claim_an_exhaustion() {
    // The hazard this run has hit three times: a lane nobody narrowed read as
    // evidence. Here the chain *looks* like a case analysis and proves nothing, so
    // the sentinel must stay silent rather than report a coverage it did not prove.
    let src = format!(
        "<?php\n{SENTINEL}function cool(mixed $v): bool {{ return true; }}\nfunction f(string|int $foo): void {{\n\techo match (true) {{\n\t\tcool($foo) => 1,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert!(sentinel(&src).is_empty(), "an untouched lane is ignorance, not evidence");
}

#[test]
fn a_chain_mixing_modellable_and_unmodellable_guards_claims_nothing() {
    // `is_string` subtracts, `cool` does not — and the second one also forgets the
    // lane the first narrowed, so the `default` arrives with no proof at all.
    let src = format!(
        "<?php\n{SENTINEL}function cool(mixed $v): bool {{ return true; }}\nfunction f(string|int $foo): void {{\n\techo match (true) {{\n\t\tis_string($foo) => 1,\n\t\tcool($foo) => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert!(sentinel(&src).is_empty(), "a partly-modelled chain proves no exhaustion");
}

// ---- The subtraction really landed: the pair that proves it ------------

#[test]
fn an_exhausted_chain_is_silent_and_a_partial_one_is_not() {
    // Read together, these two are the evidence that `guard_arms_…` above measured
    // a subtraction and not a coincidence. The sentinel reports only where the
    // residue is non-empty AND strictly smaller than the declaration seeded
    // (ADR-0088 §4's proven-narrowing gate), so:
    //   * two guards covering `string|int` → empty residue → silent;
    //   * one guard → residue `int`, demonstrably narrowed → reported.
    let exhaustive = format!(
        "<?php\n{SENTINEL}function f(string|int $foo): void {{\n\techo match (true) {{\n\t\tis_string($foo) => 1,\n\t\tis_int($foo) => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert!(sentinel(&exhaustive).is_empty(), "an exhaustive chain is silent");

    let partial = format!(
        "<?php\n{SENTINEL}function f(string|int $foo): void {{\n\techo match (true) {{\n\t\tis_string($foo) => 1,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    let d = sentinel(&partial);
    assert_eq!(d.len(), 1, "the uncovered `int` refutes the claim: {d:?}");
    assert_eq!(d[0].id, NEVER_PARAM_REACHABLE_ID);
    assert!(d[0].message.contains("int"), "{}", d[0].message);
}

#[test]
fn an_enum_chain_that_forgot_a_case_says_so() {
    // ADR-0088 §8's last row — "someone adds a case to an enum, and every `match`
    // that forgot it says so" — reached through `match (true)` syntax for the first
    // time. The covering chain beside it must stay silent.
    let covered = format!(
        "<?php\n{SENTINEL}enum Suit {{ case Hearts; case Spades; }}\nfunction f(Suit $s): void {{\n\techo match (true) {{\n\t\t$s === Suit::Hearts => 1,\n\t\t$s === Suit::Spades => 2,\n\t\tdefault => assertNever($s),\n\t}};\n}}\n"
    );
    assert!(sentinel(&covered).is_empty(), "every case covered → silent");

    let missed = format!(
        "<?php\n{SENTINEL}enum Suit {{ case Hearts; case Spades; case Clubs; }}\nfunction f(Suit $s): void {{\n\techo match (true) {{\n\t\t$s === Suit::Hearts => 1,\n\t\t$s === Suit::Spades => 2,\n\t\tdefault => assertNever($s),\n\t}};\n}}\n"
    );
    let d = sentinel(&missed);
    assert_eq!(d.len(), 1, "the missed case reports: {d:?}");
    assert!(d[0].message.contains("Suit::Clubs"), "{}", d[0].message);
}

// ---- ADR-0088 §8's cells, which must all stay silent -------------------

/// The worked example's rows, each written as a `match (true)` chain: the native
/// `string|int` row (`f`) and the `mixed` + `@param string|int` row (`g`), crossed
/// with no `default` (`0`) and `default => assertNever` (`2`). Every one of the
/// four is silent on every built-in surface — `f0` and `g0` because nothing this
/// slice added reports on a `default`-less chain, `f2` and `g2` because the
/// exhaustion is proven and the sentinel is answered.
#[test]
fn the_worked_examples_f0_f2_g0_g2_cells_are_silent() {
    let cells = [
        ("f0", "function f0(string|int $foo): void {\n\techo match (true) {\n\t\tis_string($foo) => 1,\n\t\tis_int($foo) => 2,\n\t};\n}\n"),
        ("f2", "function f2(string|int $foo): void {\n\techo match (true) {\n\t\tis_string($foo) => 1,\n\t\tis_int($foo) => 2,\n\t\tdefault => assertNever($foo),\n\t};\n}\n"),
        ("g0", "/** @param string|int $foo */\nfunction g0(mixed $foo): void {\n\techo match (true) {\n\t\tis_string($foo) => 1,\n\t\tis_int($foo) => 2,\n\t};\n}\n"),
        ("g2", "/** @param string|int $foo */\nfunction g2(mixed $foo): void {\n\techo match (true) {\n\t\tis_string($foo) => 1,\n\t\tis_int($foo) => 2,\n\t\tdefault => assertNever($foo),\n\t};\n}\n"),
    ];
    for (name, body) in cells {
        let ds = findings(&format!("<?php\n{SENTINEL}{body}"));
        assert!(ds.is_empty(), "{name} must be silent, got: {ds:?}");
    }
}

#[test]
fn the_literal_refined_row_and_the_instanceof_chain_are_silent_too() {
    // `int` + `@param 1|2` is the row where the two premise grades disagree
    // (ADR-0088 §2), and the `instanceof` pair is the class-lane spelling of the
    // same exhaustion. Neither may report.
    let refined = format!(
        "<?php\n{SENTINEL}/** @param 1|2 $foo */\nfunction h(int $foo): void {{\n\techo match (true) {{\n\t\t$foo === 1 => 1,\n\t\t$foo === 2 => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert!(findings(&refined).is_empty(), "the 1|2 row is silent: {:?}", findings(&refined));

    let shapes = format!(
        "<?php\n{SENTINEL}class Circ {{}}\nclass Sq {{}}\nfunction h(Circ|Sq $x): void {{\n\techo match (true) {{\n\t\t$x instanceof Circ => 1,\n\t\t$x instanceof Sq => 2,\n\t\tdefault => assertNever($x),\n\t}};\n}}\n"
    );
    assert!(findings(&shapes).is_empty(), "the instanceof chain is silent: {:?}", findings(&shapes));
}

#[test]
fn a_match_false_chain_over_negated_guards_is_silent_too() {
    // The inverted spelling of the exhaustive chain: `match (false)` with negated
    // guards covers `string|int` exactly as `match (true)` with plain ones.
    let src = format!(
        "<?php\n{SENTINEL}function f(string|int $foo): void {{\n\techo match (false) {{\n\t\t!is_string($foo) => 1,\n\t\t!is_int($foo) => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert!(findings(&src).is_empty(), "got: {:?}", findings(&src));
}

// ---- Positions and shapes that ride along ------------------------------

#[test]
fn statement_and_value_positions_answer_the_same() {
    // The value-position hoist of #430 carries the guard chain too — it takes
    // whatever the lowering structures, and does not care which shape it got.
    let arms = "match (true) {\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t}";
    let stmt = over_union(&format!("\t{arms};"));
    let value = over_union(&format!("\t$r = {arms};"));
    assert_eq!(dumps(&stmt), vec!["dumped type: string", "dumped type: int"]);
    assert_eq!(dumps(&value), dumps(&stmt));
}

#[test]
fn a_guard_chain_nested_in_an_arm_body_is_walked() {
    // An arm body is a statement position by any other name, so the chain inside
    // one gets the same treatment — including the inner subtraction.
    assert_eq!(
        dumps(
            "<?php\nfunction f(string|int $foo, ?int $n): void {\n\techo match (true) {\n\t\tis_string($foo) => match (true) { $n === null => \\PHPStan\\dumpType($n), default => \\PHPStan\\dumpType($n) },\n\t\tdefault => 0,\n\t};\n}\n"
        ),
        vec!["dumped type: null", "dumped type: int"]
    );
}

#[test]
fn a_default_written_first_is_still_the_no_match_arm() {
    // PHP consults `default` only when nothing else matched, wherever it is
    // written, so it becomes the `else` and not the first link.
    assert_eq!(
        dumps(&over_union(
            "\techo match (true) {\n\t\tdefault => \\PHPStan\\dumpType($foo),\n\t\tis_string($foo) => \\PHPStan\\dumpType($foo),\n\t};"
        )),
        vec!["dumped type: string", "dumped type: int"],
        "the guard link is walked first and the default sees the residue"
    );
}

#[test]
fn a_guard_chain_whose_every_arm_throws_terminates_the_trace() {
    // Reachability rides on the same `if` machinery: no branch falls through, so
    // the function does not read as falling off its end.
    let src =
        "<?php\nfunction f(string|int $foo): int {\n\t$r = match (true) {\n\t\tis_string($foo) => throw new LogicException(),\n\t\tdefault => throw new LogicException(),\n\t};\n}\n";
    let ds = findings(src);
    assert!(ds.is_empty(), "a body that always throws misses no return, got: {ds:?}");
}

#[test]
fn the_no_match_path_of_a_default_less_chain_falls_through() {
    // Recorded, not celebrated: a `default`-less by-value `match` terminates on the
    // no-match path (PHP raises `\UnhandledMatchError`), and the desugared guard
    // chain falls through to the statement after it instead. Falling through only
    // widens the join — `$foo` below is the entry union joined with both arms, so
    // nothing downstream claims a narrowing — and teaching the chain to terminate
    // means giving the throw accounting a contribution ADR-0088 §5 has not ruled
    // on. The day that changes, this fixture is the one to edit on purpose.
    assert_eq!(
        dumps(&over_union(
            "\techo match (true) {\n\t\tis_string($foo) => 1,\n\t\tis_int($foo) => 2,\n\t};\n\t\\PHPStan\\dumpType($foo);"
        )),
        vec!["dumped type: unknown"],
        "the tail is reachable and its lane claims nothing"
    );
}

// ---- The inherited gap, pinned in both spellings -----------------------

#[test]
fn a_chain_whose_later_arm_re_narrows_reports_where_php_reaches_nothing() {
    // A FALSE POSITIVE, pinned so it cannot be forgotten. `$v === 1` leaves the
    // residue `2`; `$v !== 1` should intersect that with `{1}` and empty it, and
    // instead its positive refinement *replaces* the lane, so the `default` reads
    // `1` on a path PHP never takes and the sentinel reports.
    //
    // The point of the pair is where the fault lives: the `if`/`elseif` spelling
    // reports the identical finding, and reported it before this slice existed.
    // The desugaring inherits the gap exactly — that is
    // `the_if_chain_of_the_same_shape_answers_identically` doing its job on a
    // shape where the shared answer happens to be wrong. It closes when a guard's
    // positive side intersects the lane it finds instead of seeding it, and both
    // halves of this fixture flip on the same day.
    let arms = format!(
        "<?php\n{SENTINEL}/** @param 1|2 $foo */\nfunction f(int $foo): int {{\n\treturn match (true) {{\n\t\t$foo === 1 => 1,\n\t\t$foo !== 1 => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    let chain = format!(
        "<?php\n{SENTINEL}/** @param 1|2 $foo */\nfunction f(int $foo): int {{\n\tif ($foo === 1) {{ return 1; }}\n\telseif ($foo !== 1) {{ return 2; }}\n\telse {{ assertNever($foo); }}\n}}\n"
    );
    let a = sentinel(&arms);
    assert_eq!(a.len(), 1, "the re-narrowed lane reports: {a:?}");
    assert_eq!(a[0].id, NEVER_PARAM_REACHABLE_ID);
    assert_eq!(
        a.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
        sentinel(&chain).iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
        "both spellings are wrong in exactly the same words"
    );
}

#[test]
fn the_re_narrowing_is_the_guard_vocabulary_and_needs_no_match_at_all() {
    // The same gap with no `match` in sight, so the next reader does not go
    // looking for it in the lowering. Two sequential guards: the first leaves the
    // residue, the second replaces it.
    assert_eq!(
        dumps(
            "<?php\n/** @param 1|2 $v */\nfunction f(int $v): void {\n\tif ($v === 1) { return; }\n\t\\PHPStan\\dumpType($v);\n\tif ($v !== 1) { return; }\n\t\\PHPStan\\dumpType($v);\n}\n"
        ),
        vec!["dumped type: int", "dumped type: 1"],
        "the second dump is on a path PHP cannot reach, and reads the later guard"
    );
}

// ---- Two shapes the mixing rule must NOT over-silence ------------------

#[test]
fn an_unmodellable_guard_on_another_variable_leaves_the_subject_proven() {
    // `cool($s)` models nothing and touches nothing the subject's lane holds, so
    // the `is_string` above it still stands: an `int` genuinely reaches the
    // `default` when `cool()` answers false, and saying so is a TRUE positive.
    // The mixing refusal is about the *subject's* lane, not about the presence of
    // any unmodellable condition anywhere in the chain.
    let src = format!(
        "<?php\n{SENTINEL}function cool(mixed $v): bool {{ return true; }}\nfunction f(string|int $foo, string $s): int {{\n\treturn match (true) {{\n\t\tis_string($foo) => 1,\n\t\tcool($s) => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    let d = sentinel(&src);
    assert_eq!(d.len(), 1, "the uncovered `int` still refutes the claim: {d:?}");
    assert!(d[0].message.contains("int"), "{}", d[0].message);
}

#[test]
fn a_guard_on_another_variable_does_not_break_an_exhaustive_chain() {
    // The mirror: a foreign guard sitting between two guards that do exhaust the
    // subject must not cost the exhaustion. Nothing about `$flag` says anything
    // about `$foo`, in either direction.
    let src = format!(
        "<?php\n{SENTINEL}function f(string|int $foo, bool $flag): int {{\n\treturn match (true) {{\n\t\tis_string($foo) => 1,\n\t\t$flag === true => 2,\n\t\tis_int($foo) => 3,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert!(sentinel(&src).is_empty(), "the chain still exhausts `$foo`: {:?}", sentinel(&src));
}

#[test]
fn a_non_bool_callee_in_arm_position_is_accepted_and_claims_nothing() {
    // The one judgment ADR-0052's note records rather than proves: a call is taken
    // for a predicate. `nonbool()` returns `int`, so in PHP its arm matches
    // nothing at all and the arms below it all run — and Steins reads it as an
    // ordinary unmodellable guard, which subtracts nothing and lets the chain
    // below it stand. The `is_int` arm still exhausts what is left, so the
    // sentinel is answered. If `arm_cond_is_bool_valued` ever gains a return-type
    // gate, this is the fixture that changes.
    let src = format!(
        "<?php\n{SENTINEL}function nonbool(mixed $v): int {{ return 1; }}\nfunction f(string|int $foo): int {{\n\treturn match (true) {{\n\t\tis_string($foo) => 1,\n\t\tnonbool($foo) => 2,\n\t\tis_int($foo) => 3,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert!(sentinel(&src).is_empty(), "got: {:?}", sentinel(&src));
}

#[test]
fn a_default_less_chain_whose_arms_all_throw_is_not_a_missing_return() {
    // The fall-through recorded above must not manufacture a finding out of the
    // path it invents. PHP raises `\UnhandledMatchError` here and never reaches
    // the end of the body; Steins falls through to it and still says nothing,
    // because the widened path carries no claim. The `if`/`elseif` spelling of the
    // same code DOES report — and correctly, since an `else`-less chain really
    // does fall through in PHP — so this is the one place the two spellings are
    // allowed to differ, and they differ in PHP too.
    let arms = "<?php\nfunction f(string|int $foo): int {\n\tmatch (true) {\n\t\tis_string($foo) => throw new LogicException(),\n\t\tis_int($foo) => throw new LogicException(),\n\t};\n}\n";
    assert!(findings(arms).is_empty(), "got: {:?}", findings(arms));

    let chain = "<?php\nfunction f(string|int $foo): int {\n\tif (is_string($foo)) { throw new LogicException(); }\n\telseif (is_int($foo)) { throw new LogicException(); }\n}\n";
    let d = findings(chain);
    assert_eq!(d.len(), 1, "the else-less chain really does fall through: {d:?}");
    assert!(d[0].id.contains("return"), "{}", d[0].id);
}

// ---- Whole-file adversarial probes -------------------------------------

#[test]
fn a_file_of_exhaustive_guard_chains_reports_nothing_at_all() {
    // Every idiom the slice newly structures, in one file, with no finding of any
    // id permitted: type predicates, negated predicates, `instanceof`, enum
    // identity, literal identity, a disjunctive arm, and `match (false)`.
    let src = format!(
        "<?php\n{SENTINEL}\
         enum Suit {{ case Hearts; case Spades; }}\n\
         class Circ {{}}\nclass Sq {{}}\n\
         function a(string|int $v): int {{ return match (true) {{ is_string($v) => 1, is_int($v) => 2, default => assertNever($v) }}; }}\n\
         function b(string|int $v): int {{ return match (true) {{ !is_string($v) => 1, is_string($v) => 2, default => assertNever($v) }}; }}\n\
         function c(Circ|Sq $v): int {{ return match (true) {{ $v instanceof Circ => 1, $v instanceof Sq => 2, default => assertNever($v) }}; }}\n\
         function d(Suit $v): int {{ return match (true) {{ $v === Suit::Hearts => 1, $v === Suit::Spades => 2, default => assertNever($v) }}; }}\n\
         function e(?string $v): int {{ return match (true) {{ $v === null => 1, is_string($v) => 2, default => assertNever($v) }}; }}\n\
         function g(string|int|float $v): int {{ return match (true) {{ is_string($v), is_int($v) => 1, is_float($v) => 2, default => assertNever($v) }}; }}\n\
         function h(string|int $v): int {{ return match (false) {{ !is_string($v) => 1, !is_int($v) => 2, default => assertNever($v) }}; }}\n"
    );
    let ds = findings(&src);
    assert!(ds.is_empty(), "an exhaustive guard chain is silent, got: {ds:?}");
}

#[test]
fn a_guard_chain_over_a_bad_argument_still_reports_inside_the_live_arm() {
    // The other direction: structuring must not swallow what an arm body says. The
    // arm proves `$v` is `null`, and `null` into an `int` parameter is the proven
    // `TypeError` it always was.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction f(?string $v): void {\n\techo match (true) {\n\t\t$v === null => width($v),\n\t\tdefault => 0,\n\t};\n}\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let ds = check(&tree, &functions, "t.php");
    assert_eq!(ds.len(), 1, "the arm body is judged with the guard's narrowing: {ds:?}");
    assert!(ds[0].id.contains("argument-mismatch"), "{}", ds[0].id);
}
