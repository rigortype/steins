//! `type.return-missing` (ADR-0078, issue #199) and the reachability foundation
//! it traces: a function-like that declares a native non-void return type and
//! whose body **provably falls through** to the closing brace.
//!
//! PHP's consequence, `php -r`-witnessed on 8.5.9: a fatal `TypeError` when
//! control reaches the end, not at declaration time — e.g. for `f`:
//! `TypeError: f(): Return value must be of type int, none returned` (methods
//! and closures get the same sentence, named `A::m()` / `{closure:…}()`).
//!
//! # The definite/possibly split
//!
//! One judgment yields two ids, discriminated by whether the body exits the
//! function anywhere at all (`body_has_terminator`):
//!
//! * **`type.return-missing`** (proof / `Default`) — falls through and exits
//!   nowhere: every execution fatals.
//! * **`type.return-maybe-missing`** (proof / `Strict`) — falls through but
//!   also returns/throws/exits on some path; same fatal, reached only along
//!   the uncovered edge. Floored at `strict`: dominated by code that is
//!   correct by construction and unprovable by analysis — phpstan-src's own
//!   `src/` carries two such cases (verbatim below) and passes its own
//!   missing-return rule.
//!
//! Every firing fixture asserts which id it routes to; a finding on the wrong
//! id is a floor mistake, not a wording one.
//!
//! # The asymmetry these tests pin
//!
//! `BodyEnd::Unknown` — exit edges the judgment cannot bound — is
//! **terminating** for this consumer (silence), but a future dead-code
//! consumer must read the same `Unknown` the other way (not terminal). Every
//! silence leg below states which reason it is silent for: *proven to
//! terminate* or *undecided, and undecided means silence here* — a leg silent
//! for the wrong reason is a bug this suite exists to catch.
//!
//! Both premises are declaration-and-shape facts, so every fixture uses the
//! sound-subset [`NoFold`].

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    Diagnostic, Floor, Layer, NoFold, TYPE_RETURN_MAYBE_MISSING_ID, TYPE_RETURN_MISSING_ID,
    check_full, layer, surface_floor,
};
use steins_syntax::SourceTree;

/// Every finding of the return-missing PAIR — never one id alone, so a fixture
/// migrating from one id to the other fails a floor assertion, not "still fires".
fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == TYPE_RETURN_MISSING_ID || d.id == TYPE_RETURN_MAYBE_MISSING_ID)
        .collect()
}

/// Exactly one finding, on the **definite** id: body exits nowhere, every
/// execution fatals.
fn definite(src: &str) -> Diagnostic {
    let d = diags(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].id, TYPE_RETURN_MISSING_ID, "the unconditional class: {d:#?}");
    d.into_iter().next().expect("one finding")
}

/// Exactly one finding, on the **`maybe-` sibling**: the body returns somewhere,
/// just not on every path.
fn maybe(src: &str) -> Diagnostic {
    let d = diags(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].id, TYPE_RETURN_MAYBE_MISSING_ID, "the conditional class: {d:#?}");
    d.into_iter().next().expect("one finding")
}

fn assert_silent(src: &str, why: &str) {
    let d = diags(src);
    assert!(d.is_empty(), "expected silence ({why}), got {d:#?}");
}

// Registry wiring: layer shared (same fatal); floor differs (the corpus measurement).

#[test]
fn the_pair_shares_a_layer_and_splits_on_the_floor() {
    // One `TypeError` consequence, so the layer cannot differ by path-conditionality.
    assert_eq!(layer(TYPE_RETURN_MISSING_ID), Some(Layer::Proof));
    assert_eq!(layer(TYPE_RETURN_MAYBE_MISSING_ID), Some(Layer::Proof));
    // Floor is where the measurement lands: unconditional by default, strict-only if conditional.
    assert_eq!(surface_floor(TYPE_RETURN_MISSING_ID), Some(Floor::Default));
    assert_eq!(surface_floor(TYPE_RETURN_MAYBE_MISSING_ID), Some(Floor::Strict));
}

// Firing 1 — unconditional fall-through (exits nowhere): `type.return-missing`,
// `Default` floor.

#[test]
fn fires_on_plain_fall_through() {
    let d = definite(
        "<?php
function f(): int {
    $x = 1;
}
",
    );
    assert_eq!(d.line, 2, "reported at the declaration: {d:#?}");
    assert!(d.message.contains("function f"), "{}", d.message);
    assert!(
        d.message.contains("Return value must be of type int, none returned"),
        "the witnessed PHP sentence: {}",
        d.message
    );
}

#[test]
fn fires_on_empty_body() {
    // The corpus's own dominant shape: a test double / stub `function (): bool {}`.
    definite("<?php\nfunction f(): int {\n}\n");
}

#[test]
fn fires_on_a_stub_closure() {
    let d = definite(
        "<?php
$f = function (): bool {
};
",
    );
    assert!(d.message.contains("closure"), "{}", d.message);
}

#[test]
fn fires_on_a_side_effect_only_body() {
    definite(
        "<?php
function f(): int {
    log_it('x');
    $this->counter++;
}
",
    );
}

#[test]
fn fires_on_loop_then_nothing() {
    // `foreach` always has an exit edge (iteration exhausts) and no `return` here,
    // so this is the unconditional class: every call runs the loop then off the end.
    definite(
        "<?php
function f(): int {
    foreach ($xs as $x) {
        echo $x;
    }
}
",
    );
}

#[test]
fn fires_on_conditional_while_then_nothing() {
    // `while ($c)` can exit on a false condition — an exit edge, so FallsThrough.
    definite(
        "<?php
function f(): int {
    while ($c) {
        $x = 1;
    }
}
",
    );
}

#[test]
fn fires_on_a_method() {
    let d = definite(
        "<?php
class A {
    public function m(): string {
        $x = 1;
    }
}
",
    );
    assert!(d.message.contains("A::m"), "{}", d.message);
    assert!(
        d.message.contains("Return value must be of type string, none returned"),
        "{}",
        d.message
    );
}

#[test]
fn fires_on_a_closure() {
    // Witnessed: a closure body falls off the same fatal, named `{closure:…}()`.
    let d = definite(
        "<?php
$f = function (): int {
    $x = 1;
};
",
    );
    assert!(d.message.contains("closure"), "{}", d.message);
}

#[test]
fn fires_on_a_nullable_return_type() {
    // `?int` is not optional: PHP demands an explicit `return null;`.
    let d = definite("<?php\nfunction f(): ?int {\n    $x = 1;\n}\n");
    assert!(
        d.message.contains("Return value must be of type ?int, none returned"),
        "{}",
        d.message
    );
}

#[test]
fn fires_on_a_union_return_type() {
    let d = definite("<?php\nfunction f(): int|string {\n    $x = 1;\n}\n");
    assert!(d.message.contains("int|string"), "{}", d.message);
}

#[test]
fn fires_on_types_that_lower_to_no_native_type() {
    // `: array` / `: mixed` lower to no `NativeType` at all, yet both fatal —
    // which is why the premise reads the RAW hint.
    for ty in ["array", "mixed"] {
        let src = format!("<?php\nfunction f(): {ty} {{\n    $x = 1;\n}}\n");
        let d = definite(&src);
        assert!(d.message.contains(ty), "{ty}: {}", d.message);
    }
}

#[test]
fn fires_on_a_match_statement_that_neither_returns_nor_covers() {
    // A `match` with a `default` whose arms are plain calls: falls through, and no
    // `return`/`throw`/`exit` anywhere — the unconditional class.
    definite(
        "<?php
function f(): int {
    match ($x) {
        1 => foo(),
        default => bar(),
    };
}
",
    );
}

// Firing 2 — conditional fall-through (returns/throws somewhere, not every
// path): `type.return-maybe-missing`, `Strict` floor.

#[test]
fn a_conditional_finding_is_absent_below_strict_and_present_at_strict() {
    // The floor's whole purpose, asserted through the surface rather than argued.
    let src = "<?php
function f(): int {
    if ($c) {
        return 1;
    }
}
";
    let tree = SourceTree::parse(src);
    let raw = check_full(&tree, "test.php", &mut NoFold, true);
    let finding = raw
        .iter()
        .find(|d| d.id == TYPE_RETURN_MAYBE_MISSING_ID)
        .expect("the conditional finding is emitted");
    for profile in ["default", "contracts", "throws-direct"] {
        let surface = ProfileConfigs::default().resolve(Some(profile)).expect("built-in");
        assert!(
            !surface.is_surfaced(finding),
            "`{profile}` must not show the conditional leg"
        );
    }
    let strict = ProfileConfigs::default().resolve(Some("strict")).expect("built-in");
    assert!(strict.is_surfaced(finding), "`strict` is where the conditional leg lives");
}

#[test]
fn fires_on_if_without_else() {
    // The implicit empty `else` IS a terminator-free path to the closing brace —
    // and the `return` in the taken arm is what makes it the conditional class.
    let d = maybe(
        "<?php
function f(): int {
    if ($c) {
        return 1;
    }
}
",
    );
    assert_eq!(d.line, 2, "{d:#?}");
    assert!(d.message.contains("returns on some paths"), "{}", d.message);
    assert!(
        d.message.contains("Return value must be of type int, none returned"),
        "the same witnessed sentence as the definite leg: {}",
        d.message
    );
}

#[test]
fn fires_when_only_one_arm_returns() {
    maybe(
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
}

#[test]
fn fires_when_an_elseif_chain_leaves_a_hole() {
    maybe(
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
}

#[test]
fn fires_when_a_loop_returns_on_a_match_but_the_collection_may_be_empty() {
    // Escape edge "no element matched" is real (may never occur); `return` sits in
    // an `Opaque` loop body invisible to the trace IR, so the discriminator uses the CST.
    maybe(
        "<?php
function f(): int {
    foreach ($xs as $x) {
        return $x;
    }
}
",
    );
}

#[test]
fn fires_on_a_match_statement_with_a_throwing_arm_and_a_plain_default() {
    maybe(
        "<?php
function f(): int {
    match ($x) {
        1 => throw new LogicException(),
        default => bar(),
    };
}
",
    );
}

#[test]
fn fires_on_the_phpstan_src_no_default_switch_shape() {
    // `TypeNodeResolver.php:697` / `ClassNameUsageLocation.php:128`, reduced: a
    // `switch` with no `default`, every case returns; the no-match edge exists in
    // the CFG but not in the program's data — phpstan-src passes its own `MissingReturnRule`.
    maybe(
        "<?php
function resolve(string $name): string {
    switch ($name) {
        case 'int':
            return 'integer';
        case 'bool':
            return 'boolean';
        case 'float':
            return 'double';
    }
}
",
    );
}

#[test]
fn fires_on_the_phpstan_src_shape_inside_a_closure() {
    // `TypeNodeResolver.php:697`'s switch, but this time inside a closure.
    let d = maybe(
        "<?php
$f = function (string $name): string {
    switch ($name) {
        case 'int':
            return 'integer';
        case 'bool':
            return 'boolean';
    }
};
",
    );
    assert!(d.message.contains("closure"), "{}", d.message);
}

#[test]
fn fires_on_a_method_that_returns_in_a_guard_only() {
    let d = maybe(
        "<?php
class A {
    public function m(): string {
        if ($this->ok) {
            return $this->value;
        }
    }
}
",
    );
    assert!(d.message.contains("A::m"), "{}", d.message);
}

#[test]
fn a_throw_counts_as_an_exit_sighting_but_a_break_does_not() {
    // A `throw` in one arm is a function exit — conditional class.
    maybe(
        "<?php
function f(): int {
    if ($c) {
        throw new LogicException();
    }
}
",
    );
    // A `break` leaves a construct, never the function — still the unconditional class.
    definite(
        "<?php
function f(): int {
    while ($c) {
        if ($d) {
            break;
        }
    }
}
",
    );
}

// Silence 1: the body is PROVEN to terminate.

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
    // `exit`/`die` surface as a real terminator (`StmtKind::Exit`) — proof, not undecided.
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
    // `while (true)` with no `break` has no exit edge (proof, not undecided).
    // Witnessed: PHP accepts `while (true) {}` and never reaches the TypeError.
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
    // no-match arm is a terminator too — every arm terminating proves the whole construct.
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
    // Witnessed: with a `: never` callee that exits, control never reaches `f`'s
    // closing brace — runs clean.
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

// Silence 2: UNDECIDED body — silence *here*; a dead-code consumer reads it the other way.

#[test]
fn silent_on_a_try_catch_tail() {
    // `finally` overwrites the exit point — witnessed on 8.5.9: `try { return 1; }
    // finally { return 2; }` evaluates to 2 and swallows an in-flight exception, so
    // neither direction is readable off the block ends. Undecided ⇒ silence here;
    // a dead-code consumer must not call the next statement unreachable either.
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
    // `while (true)` WITH a `break` inside: the break may belong to a nested
    // `switch`/loop, so whether this loop has an exit edge is undecided.
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
    // Included code can `exit` the whole script — an exit this judgment can't see.
    assert_silent(
        "<?php\nfunction f(): int {\n    include 'x.php';\n}\n",
        "undecided: `include` brings in code that can terminate the script",
    );
}

// Silence 3: the DECLARATION premise is absent — nothing to demand.

#[test]
fn silent_on_a_generator_body() {
    // A `yield` body returns a `Generator` from the CALL; the declared type
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
    // `never` falling through IS a fatal, but a different one — different sentence
    // (`never-returning function must not implicitly return`); ADR-0022: one id, one consequence.
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

// Registry wiring.

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
