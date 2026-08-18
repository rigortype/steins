//! Issue #439 — the **no-match path** of a `match`/`switch` subtracts the arms.
//!
//! `walk_match` refined the subject *inside* a conditional arm and refined nothing
//! on the path reached because every arm failed. A `default` therefore read the
//! subject exactly as it arrived, and the negated-guard reasoning an `elseif` chain
//! has always done stopped at the `match` keyword.
//!
//! What this file pins:
//!
//! * the `?string` reproducer is silent, in value position and statement position
//!   alike, and the `if ($s === null) { return; }` twin it should have matched all
//!   along stays silent too;
//! * `switch` subtracts the same set — the failing branch of a loose `==` carries
//!   the failure of the strict `===` (the issue #391 reading, one construct up);
//! * `switch`'s residue is nonetheless **not evidence**: the loose-equal set of a
//!   literal is infinite, so a non-empty residue may hold exactly the values the
//!   comparison already consumed, and nothing may read reachability off it;
//! * a subtraction that cannot be expressed leaves the lane wide, and a chain where
//!   only *some* conditions landed claims no exhaustion at all;
//! * an exhausted declared domain empties the lane, which is what makes
//!   `default => assertNever($foo)` silent for the right reason, and a domain
//!   missing one alternative leaves exactly that alternative and reports it.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, NEVER_PARAM_REACHABLE_ID, check};
use steins_syntax::SourceTree;

/// A `string`-taking sink: handing it a `string|null` is the strict-floor
/// possibly-grade finding the reproducer is measured by.
const HDR: &str = "<?php\nfunction name(string $s): string { return $s; }\n";

/// The sentinel parameter of ADR-0088 §4.
const SENTINEL: &str =
    "/** @param never $value */\nfunction assertNever(mixed $value): never { throw new LogicException(); }\n";

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

/// The possibly-grade argument findings a source emitted, in order.
fn maybe_mismatches(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id.contains("maybe-argument"))
        .map(|d| d.message.clone())
        .collect()
}

fn dumps(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.clone())
        .collect()
}

fn sentinels(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == NEVER_PARAM_REACHABLE_ID)
        .map(|d| d.message.clone())
        .collect()
}

// ---- The reproducer, in both syntactic positions ------------------------

#[test]
fn the_default_arm_reads_the_subject_the_arms_left_behind() {
    // Issue #439's own reproducer. The `default` is reached only because the `null`
    // arm consumed `null`; before this slice the lane still said `string|null` and
    // the strict floor said so out loud, in both positions.
    let src = format!(
        "{HDR}function stmt(?string $s): void {{\n\tmatch ($s) {{ null => 'none', default => name($s) }};\n}}\nfunction value(?string $s): string {{\n\treturn match ($s) {{ null => 'none', default => name($s) }};\n}}\n"
    );
    assert_eq!(
        maybe_mismatches(&src),
        Vec::<String>::new(),
        "the null arm consumed null; neither position may still see it"
    );
}

#[test]
fn the_default_arm_now_answers_what_the_negated_guard_always_answered() {
    // The `if` form was correct all along, and the point of the slice is that the
    // two forms stop disagreeing. Both dump the subtracted subject.
    let src = format!(
        "{HDR}function guarded(?string $s): void {{\n\tif ($s === null) {{ return; }}\n\t\\PHPStan\\dumpType($s);\n}}\nfunction matched(?string $s): void {{\n\tmatch ($s) {{ null => 1, default => \\PHPStan\\dumpType($s) }};\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: string", "dumped type: string"]);
}

#[test]
fn a_default_less_match_refines_nothing_because_it_throws() {
    // No `default` means the no-match path raises `\UnhandledMatchError` — a
    // terminator with no successor to refine. The tail is dead, so the call it
    // holds is silent for the reachability reason, not the subtraction one.
    let src = format!(
        "{HDR}function f(?string $s): void {{\n\tmatch ($s) {{ null => 1 }};\n\tname($s);\n}}\n"
    );
    assert_eq!(maybe_mismatches(&src), Vec::<String>::new());
}

// ---- `switch` subtracts the same set ------------------------------------

#[test]
fn a_switch_fall_through_subtracts_the_cases() {
    // A `default`-less `switch` falls through to after itself, and that path is the
    // no-match path: `$s != null` failed for every case, and the failing branch of a
    // loose comparison carries the strict one's failure.
    let src = format!(
        "{HDR}function f(?string $s): void {{\n\tswitch ($s) {{\n\t\tcase null:\n\t\t\treturn;\n\t}}\n\tname($s);\n}}\n"
    );
    assert_eq!(maybe_mismatches(&src), Vec::<String>::new());
}

#[test]
fn a_switch_default_body_subtracts_the_cases() {
    let src = format!(
        "{HDR}function f(?string $s): string {{\n\tswitch ($s) {{\n\t\tcase null:\n\t\t\treturn 'none';\n\t\tdefault:\n\t\t\treturn name($s);\n\t}}\n}}\n"
    );
    assert_eq!(maybe_mismatches(&src), Vec::<String>::new());
}

#[test]
fn a_switch_does_not_subtract_what_only_loose_equality_consumed() {
    // `'' == null` is true, so `case ''` consumes `null` as well and the truth on
    // the fall-through is a non-empty string. Steins models only the strict reading
    // (`$s !== ''`) and keeps the `null` arm — weaker than the truth, which is the
    // direction a subtraction may err in. The `match` twin gets the same answer,
    // where it is not weaker but exact.
    let src = format!(
        "{HDR}function sw(?string $s): void {{\n\tswitch ($s) {{\n\t\tcase '':\n\t\t\treturn;\n\t}}\n\t\\PHPStan\\dumpType($s);\n}}\nfunction m(?string $s): void {{\n\tmatch ($s) {{ '' => 1, default => \\PHPStan\\dumpType($s) }};\n}}\n"
    );
    assert_eq!(
        dumps(&src),
        vec!["dumped type: non-empty-string|null", "dumped type: non-empty-string|null"],
        "the loose-equal set of a literal has no finite subtrahend spelling"
    );
}

// ---- What may be read off a residue -------------------------------------

#[test]
fn a_match_that_misses_one_alternative_leaves_exactly_that_alternative() {
    // The finding the whole area exists to produce: the declared domain is `'a'|'b'`,
    // the arms cover `'a'`, and what reaches the sentinel is `'b'`.
    let src = format!(
        "<?php\n{SENTINEL}/** @param 'a'|'b' $foo */\nfunction h(string $foo): int {{\n\treturn match ($foo) {{\n\t\t'a' => 1,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    let ds = sentinels(&src);
    assert_eq!(ds.len(), 1, "the missing alternative reports: {ds:?}");
    assert!(ds[0].starts_with("'b' can still reach"), "{ds:?}");
}

#[test]
fn a_switch_that_misses_one_alternative_reports_nothing() {
    // The same shape under a `switch`. Its residue is an over-approximation — a
    // loose `case 'a'` may have consumed more than `'a'` — so a non-empty residue
    // is not reachability and nothing may be claimed from it.
    let src = format!(
        "<?php\n{SENTINEL}/** @param 'a'|'b' $foo */\nfunction h(string $foo): int {{\n\tswitch ($foo) {{\n\t\tcase 'a':\n\t\t\treturn 1;\n\t\tdefault:\n\t\t\treturn assertNever($foo);\n\t}}\n}}\n"
    );
    assert_eq!(sentinels(&src), Vec::<String>::new());
}

#[test]
fn a_chain_where_only_some_conditions_landed_claims_nothing() {
    // `?bool`: the `null` arm dies, but neither bool literal covers the general
    // `bool` arm (ADR-0052 §2's interior-point rule), so the residue reads `bool`
    // on a chain that is in fact exhaustive. One condition landing is not the chain
    // landing, and the mark ADR-0088 §4 reads must not be set by a partial one.
    let src = format!(
        "<?php\n{SENTINEL}function h(?bool $b): void {{\n\tmatch ($b) {{\n\t\tnull => 1,\n\t\ttrue => 2,\n\t\tfalse => 3,\n\t\tdefault => assertNever($b),\n\t}};\n}}\n"
    );
    assert_eq!(sentinels(&src), Vec::<String>::new());
}

#[test]
fn an_unrepresentable_arm_condition_subtracts_nothing_and_claims_nothing() {
    // The arm's condition is a variable, not a literal — nothing to subtract. The
    // lane stays exactly as wide as it arrived and the possibly-grade finding
    // survives, because silencing it would be an exhaustion nobody proved.
    let src = format!(
        "{HDR}function f(?string $s, ?string $k): void {{\n\tmatch ($s) {{ $k => 1, default => name($s) }};\n}}\n"
    );
    assert_eq!(
        maybe_mismatches(&src).len(),
        1,
        "an inexpressible condition leaves the lane wide"
    );
}

#[test]
fn a_literal_that_covers_no_arm_voids_the_chains_claim() {
    // `'z'` is outside the declared domain, so its subtraction lands on nothing.
    // The `'a'` arm did land and the residue is `'b'` — but a chain holding a
    // condition the lane could not model is ignorance about that condition, so no
    // reachability is claimed.
    let src = format!(
        "<?php\n{SENTINEL}/** @param 'a'|'b' $foo */\nfunction h(string $foo): int {{\n\treturn match ($foo) {{\n\t\t'a' => 1,\n\t\t'z' => 2,\n\t\tdefault => assertNever($foo),\n\t}};\n}}\n"
    );
    assert_eq!(sentinels(&src), Vec::<String>::new());
}

// ---- Hygiene ------------------------------------------------------------

#[test]
fn an_arms_own_rebinding_does_not_reach_the_no_match_path() {
    // Each arm walks a cloned env; the subtraction is applied to a clone of the
    // *entry* env, so an assignment inside an arm is invisible on the no-match path.
    let src = format!(
        "{HDR}function f(?string $s): void {{\n\tmatch ($s) {{ null => $s = null, default => \\PHPStan\\dumpType($s) }};\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: string"]);
}

#[test]
fn a_non_variable_subject_subtracts_nothing() {
    // Only a bare variable has a lane to subtract from; a constant-key projection
    // subject keeps the tag-guard treatment it already had and nothing more.
    let src = format!(
        "{HDR}function f(array $a): void {{\n\tmatch ($a['k']) {{ null => 1, default => 2 }};\n}}\n"
    );
    assert!(findings(&src).iter().all(|d| d.id != DEBUG_TYPE_ID));
}
