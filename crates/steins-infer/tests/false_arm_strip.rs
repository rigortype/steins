//! ADR-0052 §2 — **value subtraction on the contract arm lane**: the `!== v` guard
//! reaching the arm carrier.
//!
//! Until this slice the arm lane received guard subtraction for exactly two
//! subtrahends — `instanceof` (`Subtrahend::Class`) and `!== null`
//! (`Subtrahend::Null`) — so the refinement PHPStan is best known for,
//! `if (strpos($h, $n) !== false)`, left the declared `T|false` arm list wholly
//! untouched. steins-contract's side was already complete and already sound:
//! `normalize::subtrahend_covers(Subtrahend::Value(v), arm)` judges arm-wise and
//! kills exactly the arms the literal provably covers. What was missing was the
//! wire, and this file is its fixture-level evidence.
//!
//! The law it pins is one sentence of §2: **an arm dies iff the subtrahend covers
//! it with `Yes`; `Maybe` keeps it.** That single rule is simultaneously the reason
//! `false` deletes a `false` arm and the reason it does NOT delete a general `bool`
//! arm — `bool` has an interior point (`true`) the guard says nothing about. Both
//! directions are asserted below, because only the pair is the rule.
//!
//! Two deliberate non-changes are pinned here as well, so a later reader can tell a
//! refusal from an oversight:
//!
//! * `if ($pos)` over `int|false` (`Refine::Truthy`) strips nothing. Truthiness
//!   kills `0` as well as `false`, so it is not a value subtraction; PHPStan's
//!   classic `strpos` footgun needs its own designed subtrahend.
//! * The positive branch of `=== false` gains no *keep-only* arm narrowing. The
//!   value lane's `Refine::Exact` already owns it, and the arm lane is a
//!   subtraction carrier by construction.
//!
//! Zero non-debug emission is asserted on every fixture: narrowing is a *type*,
//! never a finding.

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// Every diagnostic a source produces, asserting on the way that narrowing emitted
/// nothing outside the `debug.*` surface.
fn silent_dumps(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let ds = check_with(&tree, &[], "t.php", folder);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "arm subtraction emitted a finding: {other:?}");
    ds
}

/// The single `debug.type` body a one-dump source produces.
fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
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

/// `@param <decl> $x`, dumped on the FALSE branch of `<cond>`.
fn else_branch(decl: &str, cond: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $x */\nfunction f($x): void {{ if ({cond}) {{ }} else {{ \\PHPStan\\dumpType($x); }} }}\n"
    ))
}

// ---------------------------------------------------------------------------
// The headline: the catalog floor's `T|false` row loses its `false` arm
// ---------------------------------------------------------------------------

/// The ADR-0069 / issue #79 declared-return floor puts `strpos`' functionMap row
/// into the arm lane — mined as `positive-int|0|false`, which the seeding
/// canonicalizes to two arms (issue #90: the literal and the interval it abuts are
/// one denotation) — no single value-domain fact, so before this slice the guard
/// had nothing to bite on.
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
    assert_eq!(one_type(STRPOS_UNGUARDED), "dumped type: non-negative-int|false (asserted)");
    // …and under the guard, the `false` arm is gone. The int arm survives (it is
    // disjoint from `false`), which is strictly more than PHPStan's `int` — the row
    // says so, so the lane says so.
    assert_eq!(one_type(STRPOS_GUARDED), "dumped type: non-negative-int (asserted)");
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

// ---------------------------------------------------------------------------
// The hand-written declaration: identical behavior, one rung up
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Polarity: which spellings produce the exclusion, and on which branch
// ---------------------------------------------------------------------------

#[test]
fn every_identity_spelling_of_the_guard_reaches_the_lane() {
    // `collect_refine` carries the branch polarity and `var_literal` normalizes the
    // operand order, so all four spellings of "on this branch, `$x` is not `false`"
    // are one refinement.
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

#[test]
fn a_loose_comparison_is_not_an_identity_and_subtracts_nothing() {
    // `$x != false` is true for `''` and `'0'` as well, so it establishes no
    // exclusion of the VALUE `false`; only `!==` does.
    assert_eq!(
        then_branch("string|false", "$x != false"),
        "dumped type: string|false (asserted)"
    );
}

// ---------------------------------------------------------------------------
// The FP-safety direction: `Maybe` keeps the arm
// ---------------------------------------------------------------------------

#[test]
fn a_general_bool_arm_survives_the_false_exclusion() {
    // THE soundness pin. `subtrahend_covers(Value(false), Base(Bool))` is `Maybe` —
    // a `bool` may still be `true` — so the arm must stay. Deleting it would be a
    // false narrowing of exactly the kind this lane exists to avoid.
    assert_eq!(then_branch("bool", "$x !== false"), "dumped type: bool (asserted)");
    assert_eq!(
        then_branch("bool|string", "$x !== false"),
        "dumped type: string|bool (asserted)"
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

// ---------------------------------------------------------------------------
// Generality: the subtrahend is a value, not the `false` special case
// ---------------------------------------------------------------------------

#[test]
fn an_int_literal_exclusion_deletes_its_arm() {
    assert_eq!(then_branch("1|2|3", "$x !== 2"), "dumped type: 1|3 (asserted)");
    assert_eq!(else_branch("1|2|3", "$x === 2"), "dumped type: 1|3 (asserted)");
    // A literal not in the lane deletes nothing.
    assert_eq!(then_branch("1|2|3", "$x !== 7"), "dumped type: 1|2|3 (asserted)");
}

#[test]
fn a_string_literal_exclusion_deletes_its_arm() {
    assert_eq!(
        then_branch("'GET'|'POST'|'PUT'", "$x !== 'GET'"),
        "dumped type: 'POST'|'PUT' (asserted)"
    );
}

// ---------------------------------------------------------------------------
// Refusal: truthiness is NOT a value subtraction
// ---------------------------------------------------------------------------

#[test]
fn a_truthiness_guard_strips_no_arm() {
    // `if ($pos)` is false for `0` as well as for `false`, so on the true branch the
    // `false` arm is indeed impossible — but so is `0`, and the `0`-vs-`false`
    // asymmetry is precisely PHPStan's `strpos` footgun. `Refine::Truthy` is
    // deliberately not wired to the lane; when it is, it gets its own subtrahend.
    assert_eq!(then_branch("int|false", "$x"), "dumped type: int|false (asserted)");
    assert_eq!(then_branch("string|false", "$x"), "dumped type: string|false (asserted)");
}

// ---------------------------------------------------------------------------
// Regression: the `null` path is unchanged
// ---------------------------------------------------------------------------

#[test]
fn the_null_subtrahend_still_works_exactly_as_before() {
    assert_eq!(then_branch("string|null", "$x !== null"), "dumped type: string (asserted)");
    assert_eq!(else_branch("string|null", "$x === null"), "dumped type: string (asserted)");
    assert_eq!(then_branch("string|null", "null !== $x"), "dumped type: string (asserted)");
}

// ---------------------------------------------------------------------------
// Consumption: the narrowed lane reaches the argument-dispatch read (issue #77)
// ---------------------------------------------------------------------------

/// A mock PHP answering the two reflection surfaces the string-predicate transfer
/// rung consults — the declaration (its admission gate) and the reflected envelope
/// it falls back to when a transfer declines, so a decline reads as `string` rather
/// than `unknown`.
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
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
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
