//! Value subtraction on the contract-arm lane (ADR-0052 §2).
//!
//! An arm dies iff the subtrahend covers it with `Yes`; `Maybe` keeps it. Thus
//! `false` deletes a `false` arm from a lane that also spells `true` as its own
//! arm. A general `bool` arm is the other shape entirely: `bool` is a two-point
//! domain with no interior point to protect (issue #443), so `false` NARROWS it
//! to `true` rather than surviving — the same partial-deletion move an
//! `int<lo, hi>` arm gets at its own endpoints, minus the interior-point
//! refusal an interval needs and `bool` does not. An `int<lo, hi>` arm minus an
//! endpoint shrinks by one, while an interior point keeps it whole because a
//! gap has no arm spelling (issue #90).
//!
//! Truthiness is the subtrahend next door (issue #557): `if ($x)` excludes `0`
//! and `''` alongside `false`, so it is not a value subtraction at all, and
//! `Subtrahend::Falsy` deletes every arm all of whose inhabitants are falsy. It
//! deletes whole arms and refines none: a surviving `int` arm still spells
//! `int` where the guard did exclude `0`.
//!
//! Two deliberate limits are pinned:
//! * the falsy branch of a truthiness test subtracts nothing — its complement
//!   has no arm spelling;
//! * the positive branch of `=== false` gains no keep-only arm narrowing because
//!   the value lane's `Refine::Exact` owns it.
//!
//! Narrowing emits no non-debug finding.

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// Every diagnostic a source produces, asserting on the way that narrowing emitted
/// nothing outside the `debug.*` surface.
fn silent_dumps(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let ds = check_with(&tree, &[], "t.php", folder);
    // `untyped.*` (ADR-0078, issue #200) is excluded alongside the dumps: these
    // fixtures declare bare `array` parameters on purpose, and a contract-layer id
    // observing the missing value type is not arm subtraction speaking.
    let other: Vec<&Diagnostic> = ds
        .iter()
        .filter(|d| !d.id.starts_with("debug.") && !d.id.starts_with("untyped."))
        .collect();
    assert!(other.is_empty(), "arm subtraction emitted a finding: {other:?}");
    ds
}

/// The single `debug.type` body a one-dump source produces.
fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    // Same `untyped.*` exclusion as `silent_dumps` above (ADR-0078, issue #200).
    let other: Vec<&Diagnostic> = ds
        .iter()
        .filter(|d| !d.id.starts_with("debug.") && !d.id.starts_with("untyped."))
        .collect();
    assert!(other.is_empty(), "arm subtraction emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// The single `debug.phpdoc-type` body — the declared-side view of the same lane.
fn one_phpdoc_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.phpdoc-type dump, got {ds:?}");
    ty[0].message.clone()
}

/// `@param <decl> $x`, dumped on the TRUE branch of `<cond>`.
fn then_branch(decl: &str, cond: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $x */\nfunction f($x): void {{ if ({cond}) {{ \\PHPStan\\dumpType($x); }} }}\n"
    ))
}

/// `@param <decl> $x`, dumped under `$x && $y` — the short-circuit spelling of
/// the same true branch (issue #557). `$y` is deliberately untyped; the second
/// operand's only job is to make the `&&` real.
fn and_branch(decl: &str) -> String {
    let src = format!(
        "<?php\n/** @param {decl} $x */\nfunction f($x, $y): void {{ if ($x && $y) {{ \\PHPStan\\dumpType($x); }} }}\n"
    );
    let tree = SourceTree::parse(&src);
    let ds = check(&tree, &[], "t.php");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// `@param <decl> $x`, dumped on the FALSE branch of `<cond>`.
fn else_branch(decl: &str, cond: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $x */\nfunction f($x): void {{ if ({cond}) {{ }} else {{ \\PHPStan\\dumpType($x); }} }}\n"
    ))
}

/// `@param <decl> $x`, dumped on the fall-through of `assert(<cond>)` — the
/// throw-guard twin of [`then_branch`] (ADR-0052's 2026-07-25 amendment).
fn after_assert(decl: &str, cond: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $x */\nfunction f($x): void {{ \\assert({cond}); \\PHPStan\\dumpType($x); }}\n"
    ))
}


// The headline: the catalog floor's `T|false` row loses its `false` arm


/// ADR-0069/issue #79 puts `strpos`' functionMap return in the arm lane.
/// `positive-int|0|false` canonicalizes to an interval plus `false` (issue #90),
/// with no single value-domain fact.
const STRPOS_GUARDED: &str = "<?php
function f(string $h, string $n): void {
    $pos = strpos($h, $n);
    if ($pos !== false) { \\PHPStan\\dumpType($pos); }
}
";

const STRPOS_UNGUARDED: &str = "<?php
function f(string $h, string $n): void {
    $pos = strpos($h, $n);
    \\PHPStan\\dumpType($pos);
}
";

#[test]
fn the_strpos_floor_row_loses_its_false_arm_under_the_guard() {
    // The control: the whole mined row, at the `Asserted` grade the floor seeds.
    // Its `positive-int|0` reads as the one interval it denotes (issue #90).
    assert_eq!(one_type(STRPOS_UNGUARDED), "dumped type: int<0, max>|false (asserted)");
    // Under the guard, the `false` arm is gone; the int arm survives (disjoint
    // from `false`) — strictly more than PHPStan's plain `int`, so the row says so.
    assert_eq!(one_type(STRPOS_GUARDED), "dumped type: int<0, max> (asserted)");
}

#[test]
fn the_array_search_floor_row_loses_its_false_arm_too() {
    // `int|string|false`: the same mechanism over a two-survivor row, so the pin is
    // not an artifact of `strpos`' particular arm spelling.
    let src = "<?php
function f(string $needle, array $hay): void {
    $k = array_search($needle, $hay);
    if ($k !== false) { \\PHPStan\\dumpType($k); }
}
";
    assert_eq!(one_type(src), "dumped type: int|string (asserted)");
}


// The hand-written declaration: identical behavior, one rung up


#[test]
fn a_hand_written_string_false_narrows_to_string() {
    assert_eq!(then_branch("string|false", "$x !== false"), "dumped type: string (asserted)");
    assert_eq!(then_branch("int|false", "$x !== false"), "dumped type: int (asserted)");
}

#[test]
fn the_declared_side_surface_sees_the_narrowed_lane() {
    // `debug.phpdoc-type` reads the same carrier, so the narrowing is visible on the
    // declared surface as well as the value one.
    let guarded = "<?php
/** @param string|false $x */
function f($x): void {
    if ($x !== false) { \\PHPStan\\dumpPhpDocType($x); }
}
";
    let unguarded = "<?php
/** @param string|false $x */
function f($x): void {
    \\PHPStan\\dumpPhpDocType($x);
}
";
    assert_eq!(one_phpdoc_type(unguarded), "dumped phpdoc type: string|false (asserted)");
    assert_eq!(one_phpdoc_type(guarded), "dumped phpdoc type: string (asserted)");
}


// Polarity: which spellings produce the exclusion, and on which branch


#[test]
fn every_identity_spelling_of_the_guard_reaches_the_lane() {
    // `collect_refine` carries the branch polarity and `var_literal` normalizes
    // operand order, so all four spellings of "`$x` is not `false`" are one refinement.
    assert_eq!(then_branch("string|false", "$x !== false"), "dumped type: string (asserted)");
    assert_eq!(else_branch("string|false", "$x === false"), "dumped type: string (asserted)");
    assert_eq!(then_branch("string|false", "false !== $x"), "dumped type: string (asserted)");
    assert_eq!(else_branch("string|false", "false === $x"), "dumped type: string (asserted)");
    // …and negation flips polarity rather than dropping the refinement.
    assert_eq!(then_branch("string|false", "!($x === false)"), "dumped type: string (asserted)");
}

#[test]
fn the_guard_threads_through_and_and_or_by_de_morgan() {
    assert_eq!(
        then_branch("string|false", "$x !== false && $x !== ''"),
        "dumped type: string (asserted)",
        "`&&` distributes on the true-path"
    );
    assert_eq!(
        else_branch("string|false", "$x === false || $x === ''"),
        "dumped type: string (asserted)",
        "`||` distributes on the false-path"
    );
}

#[test]
fn the_opposite_branch_is_the_value_lanes_and_leaves_the_arms_alone() {
    // On the branch where `$x` IS `false`, the value lane's `Refine::Exact` answers
    // and the arm lane is not intersected down to `{false}` — the refusal in
    // `apply_class_narrowing`'s doc comment, observed.
    assert_eq!(then_branch("string|false", "$x === false"), "dumped type: false");
    assert_eq!(else_branch("string|false", "$x !== false"), "dumped type: false");
}

// `assert($expr)` reaches the same lane as `if ($expr)` (issue #391)


#[test]
fn an_assert_subtracts_the_false_arm_exactly_as_its_if_twin_does() {
    // The corpus shape this repair is named after: `realpath()`-grade `T|false`
    // guarded by an assert chain. Before the fix the assert arm of `walk_trace`
    // called only the VALUE-lane refinements, and a `T|false` binding has no
    // value-lane carrier at all — so the lane came out untouched.
    assert_eq!(after_assert("string|false", "$x !== false"), "dumped type: string (asserted)");
    assert_eq!(
        after_assert("string|false", "$x !== false && $x !== ''"),
        "dumped type: string (asserted)",
        "the `&&` chain's non-final conjunct subtracts too"
    );
    assert_eq!(
        after_assert("non-empty-string|false", "$x !== false && $x !== ''"),
        "dumped type: non-empty-string (asserted)"
    );
    // The `if` twin, re-pinned beside it: one behavior, two spellings.
    assert_eq!(then_branch("non-empty-string|false", "$x !== false && $x !== ''"), "dumped type: non-empty-string (asserted)");
}

#[test]
fn an_assert_reaches_the_null_subtrahend_too() {
    // The repair is the missing `apply_class_narrowing` call, so every subtrahend it
    // carries arrives at once — pinned so a later reader does not read the fix as
    // `false`-specific. (Its `Class` subtrahend's observable is the declared-receiver
    // lane, pinned in `phpdoc_undefined_method.rs`.)
    assert_eq!(after_assert("string|null", "$x !== null"), "dumped type: string (asserted)");
}

#[test]
fn an_assert_narrows_the_general_bool_arm_like_every_other_guard() {
    // The wiring direction is the guard's, not the statement form's: `assert()`
    // reaches the same `Base::Bool` endpoint clip the `if` twin does (issue #443).
    assert_eq!(
        after_assert("string|bool", "$x !== false"),
        "dumped type: string|true (asserted)"
    );
}

#[test]
fn a_loose_comparison_is_not_an_identity_and_subtracts_nothing() {
    // `$x != false` is true for `''` and `'0'` as well, so it establishes no
    // exclusion of the VALUE `false`; only `!==` does.
    assert_eq!(
        then_branch("string|false", "$x != false"),
        "dumped type: string|false (asserted)"
    );
}


// The FP-safety direction: `Maybe` keeps the arm


#[test]
fn a_general_bool_arm_narrows_to_the_surviving_literal() {
    // THE re-pinned soundness pin (issue #443). This used to read
    // `a_general_bool_arm_survives_the_false_exclusion` and assert survival on the
    // theory that `subtrahend_covers(Value(false), Base(Bool))` answering `Maybe`
    // meant nothing more could be said — `bool` "may still be `true`", so deleting
    // the arm looked like a false narrowing.
    //
    // That theory conflated `Maybe` (the whole-arm death question `subtrahend_covers`
    // answers) with "nothing more can be said": `bool` is a two-point domain, not an
    // interval with an unreachable interior, and its one other point is exactly
    // `true` — the same shape `int<lo, hi>` already narrows at an endpoint
    // (`subtract_arm`'s `ArmFate::Narrows`). Surviving whole was therefore an
    // under-approximation, not a soundness floor, and it is what let an exhaustive
    // `if ($b === true) … elseif ($b === false) … else { assertNever($b); }` report
    // a residue PHP can never reach. `Maybe` still governs whether the arm DIES;
    // it was never the answer to whether it narrows.
    assert_eq!(then_branch("bool", "$x !== false"), "dumped type: true (asserted)");
    assert_eq!(
        then_branch("bool|string", "$x !== false"),
        "dumped type: string|true (asserted)"
    );
}

#[test]
fn a_disjoint_arm_survives_and_a_covered_literal_arm_dies() {
    // `int` is disjoint from `false` and survives untouched…
    assert_eq!(then_branch("int", "$x !== false"), "dumped type: int (asserted)");
    // …while a lane spelling both bool literals loses exactly the covered one.
    assert_eq!(
        then_branch("true|false|string", "$x !== false"),
        "dumped type: string|true (asserted)"
    );
}

#[test]
fn an_emptied_lane_drops_to_no_fact_never_a_death_signal() {
    // `@param false $x` + `!== false` subtracts the only arm. ADR-0052 §2: the lane
    // goes to no-fact and the walk continues; unreachability is not this carrier's
    // claim to make.
    assert_eq!(then_branch("false", "$x !== false"), "dumped type: unknown");
}

#[test]
fn excluding_both_bool_literals_in_sequence_empties_the_general_arm() {
    // The chained shape issue #443 exists for: `bool` narrows to `true` under the
    // first exclusion, then that literal arm dies under the second — an exhausted
    // `bool`, exactly as an `int<lo, hi>` walked down by repeated endpoint clips
    // (`repeated_endpoint_clips_walk_the_interval_down_to_a_literal`) collapses to
    // nothing rather than surviving whole.
    assert_eq!(
        then_branch("bool", "$x !== false && $x !== true"),
        "dumped type: unknown"
    );
}


// Generality: the subtrahend is a value, not the `false` special case


#[test]
fn an_int_literal_exclusion_deletes_its_arm() {
    assert_eq!(then_branch("1|2|3", "$x !== 2"), "dumped type: 1|3 (asserted)");
    assert_eq!(else_branch("1|2|3", "$x === 2"), "dumped type: 1|3 (asserted)");
    // A literal not in the lane deletes nothing.
    assert_eq!(then_branch("1|2|3", "$x !== 7"), "dumped type: 1|2|3 (asserted)");
    // A run of literals is NOT absorbed into an interval (issue #90 merges a
    // literal only into an interval it abuts, never literal-to-literal) — that
    // refusal keeps this family narrowable: collapsing `1|2|3` would cost the
    // discrimination `int<1, 3>` (no arm for `!== 2`) couldn't carry.
}

#[test]
fn subtracting_an_endpoint_of_an_interval_arm_clips_it() {
    // An interval arm can be partly deleted at an endpoint. `strpos` denotes
    // `int<0, max>|false`; `!== 0` clips it to `int<1, max>|false`, matching
    // PHPStan. The `false` arm is disjoint and survives.
    let src = "<?php
function f(string $h, string $n): void {
    $pos = strpos($h, $n);
    if ($pos !== 0) { \\PHPStan\\dumpType($pos); }
}
";
    assert_eq!(one_type(src), "dumped type: int<1, max>|false (asserted)");
}

#[test]
fn subtracting_an_interior_point_of_an_interval_arm_keeps_it_whole() {
    // An interior point would split the interval into two arms — a gap the arm
    // vocabulary cannot spell — so the arm is left unchanged (ADR-0052 §2's
    // interior-point discipline).
    let src = "<?php
function f(string $h, string $n): void {
    $pos = strpos($h, $n);
    if ($pos !== 5) { \\PHPStan\\dumpType($pos); }
}
";
    assert_eq!(one_type(src), "dumped type: int<0, max>|false (asserted)");
}

#[test]
fn a_hand_written_interval_clips_at_both_endpoints_and_only_there() {
    // The same rule over a bounded, hand-declared `int<lo, hi>`: each endpoint
    // clips by one, the interior refuses.
    assert_eq!(then_branch("int<0, 10>", "$x !== 0"), "dumped type: int<1, 10> (asserted)");
    assert_eq!(then_branch("int<0, 10>", "$x !== 10"), "dumped type: int<0, 9> (asserted)");
    assert_eq!(then_branch("int<0, 10>", "$x !== 5"), "dumped type: int<0, 10> (asserted)");
    // A point outside the interval deletes nothing and clips nothing.
    assert_eq!(then_branch("int<0, 10>", "$x !== 42"), "dumped type: int<0, 10> (asserted)");
}

#[test]
fn repeated_endpoint_clips_walk_the_interval_down_to_a_literal() {
    // Two clips compose: `int<0, 2>` less `0` is `int<1, 2>`, and less `1` is
    // the point — which collapses to the literal `2`, the canonical arm the #90
    // absorption vocabulary would rebuild an interval from.
    assert_eq!(
        then_branch("int<0, 2>", "$x !== 0 && $x !== 1"),
        "dumped type: 2 (asserted)"
    );
}

#[test]
fn a_string_literal_exclusion_deletes_its_arm() {
    assert_eq!(
        then_branch("'GET'|'POST'|'PUT'", "$x !== 'GET'"),
        "dumped type: 'POST'|'PUT' (asserted)"
    );
}


// Truthiness: its own subtrahend, deleting whole arms only (issue #557)


#[test]
fn a_truthiness_guard_deletes_the_arms_it_proves_out() {
    // `if ($pos)` is false for `0` as well as for `false`, so this was never a
    // value subtraction — the `0`-vs-`false` asymmetry is PHPStan's classic
    // `strpos` footgun. `Subtrahend::Falsy` is the subtrahend that owns it: the
    // `false` arm is all-falsy and dies, and the `int` arm survives because it
    // admits truthy values.
    assert_eq!(then_branch("int|false", "$x"), "dumped type: int (asserted)");
    assert_eq!(then_branch("string|false", "$x"), "dumped type: string (asserted)");
    // The `string|false` builtin-return idiom is the shape the FP was measured
    // on (issue #557, three sites in the public corpus), in each of the three
    // spellings that reach the true branch.
    assert_eq!(and_branch("string|false"), "dumped type: string (asserted)");
    assert_eq!(else_branch("string|false", "!$x"), "dumped type: string (asserted)");
    assert_eq!(after_assert("string|false", "$x"), "dumped type: string (asserted)");
}

#[test]
fn every_all_falsy_arm_dies_and_no_other_does() {
    // Arm by arm, against a `mixed`-free union that keeps one truthy anchor so an
    // emptied lane never hides the answer.
    for falsy in ["false", "null", "0", "''", "'0'", "0.0"] {
        assert_eq!(
            then_branch(&format!("\\DateTime|{falsy}"), "$x"),
            "dumped type: DateTime (asserted)",
            "the all-falsy arm `{falsy}` must die"
        );
    }
    // A truthy literal arm is not touched.
    assert_eq!(then_branch("string|true", "$x"), "dumped type: string|true (asserted)");
    assert_eq!(then_branch("int|true", "$x"), "dumped type: int|true (asserted)");
    assert_eq!(then_branch("int|1", "$x"), "dumped type: int|1 (asserted)");
}

#[test]
fn a_surviving_arm_is_never_refined_within() {
    // The deliberate bound (issue #557): whole-arm deletion, nothing finer. Each
    // of these admits both falsy and truthy values, so the guard does exclude
    // *some* of what the arm spells — and the arm still spells it. Widening the
    // truth is sound; the finer readings are neighbouring work.
    assert_eq!(then_branch("int", "$x"), "dumped type: int (asserted)");
    assert_eq!(then_branch("bool", "$x"), "dumped type: bool (asserted)");
    assert_eq!(then_branch("string", "$x"), "dumped type: string (asserted)");
    // `array` is the one shape where the dump moves, and it is not this lane
    // speaking: the value lane's own `truthy_narrow` has always added
    // non-emptiness to an array fact under a truthiness guard. The ARM is
    // untouched — `array` admits `[]` and survives whole.
    assert_eq!(then_branch("array", "$x"), "dumped type: non-empty-array (asserted)");
    // An interval that straddles zero keeps its interior point, exactly as a
    // value subtrahend's interior point leaves an interval whole.
    assert_eq!(then_branch("int<-1, 1>", "$x"), "dumped type: int<-1, 1> (asserted)");
    // The point interval at zero IS `0` under another spelling, so it dies.
    assert_eq!(then_branch("\\DateTime|int<0, 0>", "$x"), "dumped type: DateTime (asserted)");
}

#[test]
fn the_falsy_branch_of_a_truthiness_test_subtracts_nothing() {
    // `collect_refine` only yields `Refine::Truthy` where the test HOLDS: the
    // falsy branch is the complement of a set the arm vocabulary cannot spell
    // (`""`, `"0"`, `0`, `null`, `[]`), and inventing an arm for it is not this
    // issue's business.
    assert_eq!(else_branch("string|false", "$x"), "dumped type: string|false (asserted)");
    assert_eq!(then_branch("string|false", "!$x"), "dumped type: string|false (asserted)");
}


// Regression: the `null` path is unchanged


#[test]
fn the_null_subtrahend_still_works_exactly_as_before() {
    assert_eq!(then_branch("string|null", "$x !== null"), "dumped type: string (asserted)");
    assert_eq!(else_branch("string|null", "$x === null"), "dumped type: string (asserted)");
    assert_eq!(then_branch("string|null", "null !== $x"), "dumped type: string (asserted)");
}


// Consumption: the narrowed lane reaches the argument-dispatch read (issue #77)


/// A mock PHP answering the two reflection surfaces the string-predicate transfer
/// rung consults — the declaration (its admission gate) and the reflected envelope
/// it falls back to when a transfer declines, so a decline reads `string`, not `unknown`.
struct Mock {
    types: HashMap<String, String>,
    facts: HashMap<String, Fact>,
}

impl Mock {
    fn new() -> Mock {
        let mut types = HashMap::new();
        let mut facts = HashMap::new();
        for n in ["trim", "strrev"] {
            types.insert(n.to_owned(), "string".to_owned());
            facts.insert(n.to_owned(), Fact::General { base: Base::String, nullable: false });
        }
        Mock { types, facts }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
}

/// `@param <decl> $v`, one dump inside `<body>`, against the reflecting mock.
fn transfer_dump(decl: &str, body: &str) -> String {
    let src =
        format!("<?php\n/** @param {decl} $v */\nfunction f($v): void {{ {body} }}\n");
    let ds = silent_dumps(&src, &mut Mock::new());
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

#[test]
fn the_narrowed_lane_becomes_a_transfer_subject() {
    // `transfer_arg_known` (issue #77) reads the arm lane when the env fact is only
    // an envelope, and `declared_arm_known` needs the arms to join to ONE fact. A
    // `lowercase-string|false` lane joins to nothing, so the casing transfer through
    // `trim` declines and the reflected `string` envelope stands…
    assert_eq!(
        transfer_dump("lowercase-string|false", "\\PHPStan\\dumpType(trim($v));"),
        "dumped type: string"
    );
    // …and once the guard removes the `false` arm, the single surviving arm lowers,
    // the transfer fires, and the casing predicate crosses the call — carrying the
    // `Asserted` grade of the declaration it came from.
    assert_eq!(
        transfer_dump(
            "lowercase-string|false",
            "if ($v !== false) { \\PHPStan\\dumpType(trim($v)); }"
        ),
        "dumped type: lowercase-string (asserted)"
    );
    // The same at the length axis, through a different rule.
    assert_eq!(
        transfer_dump("non-empty-string|false", "\\PHPStan\\dumpType(strrev($v));"),
        "dumped type: string"
    );
    assert_eq!(
        transfer_dump(
            "non-empty-string|false",
            "if ($v !== false) { \\PHPStan\\dumpType(strrev($v)); }"
        ),
        "dumped type: non-empty-string (asserted)"
    );
}
