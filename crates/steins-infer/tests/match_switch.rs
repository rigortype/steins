//! Acceptance tests for match/switch structuring (ADR-0031 Part B): the deferred
//! sibling of the structured-`if` slice. A statement-position `match` and a
//! non-fall-through `switch` are lowered to the same `StmtKind::Match` trace node
//! and walked with first-match `taken` certainty, subject refinement, arm joins,
//! and the no-`default` `\UnhandledMatchError` terminator.
//!
//! Two-pass reminder (see `branch_analysis.rs`): the env-free **direct** pass
//! flags every literal call argument regardless of reachability, so a decided
//! arm's deadness is observed by a **literal-bad** call going silent inside it;
//! the reachability-sensitive **propagation** pass is driven through a *variable*
//! (`width($v)`) or through subject **refinement**.

use steins_infer::{Diagnostic, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "demo.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

/// Header: `width(int)` (a bad non-numeric string into it is a proven TypeError in
/// the coercive mode of this un-`declare`d file) plus a bad string local.
const HDR: &str = "<?php\nfunction width(int $w): int { return $w; }\nfunction helper(): int { return 1; }\n$bad = \"abc\";\n";

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

// ---- match: decided first arm prunes later arms dead -----------------------

#[test]
fn match_decided_first_arm_prunes_later_arms() {
    // `$x = 5`; arm `5` is a decided match (first), so arm `6` is unreachable —
    // its literal-bad `width("abc")` must be pruned (direct pass silent).
    let src = format!(
        "{HDR}$x = 5;\nmatch ($x) {{ 5 => helper(), 6 => width(\"abc\") }};\n"
    );
    assert_eq!(n(&src), 0, "later arm after a decided-first match is dead → silent");
    // Control: make arm `6` the matching one → it is live → flagged.
    let live = format!(
        "{HDR}$x = 6;\nmatch ($x) {{ 5 => helper(), 6 => width(\"abc\") }};\n"
    );
    assert_eq!(n(&live), 1, "the matching arm is live → its bad literal is flagged");
}

// ---- match: a decided LATER arm requires all earlier arms No ---------------

#[test]
fn match_decided_later_arm_needs_earlier_all_no() {
    // `$x = 6`; arm `5` is a decided No → dead → its bad literal pruned; arm `6`
    // is the decided match (all earlier No).
    let src = format!(
        "{HDR}$x = 6;\nmatch ($x) {{ 5 => width(\"abc\"), 6 => helper() }};\n"
    );
    assert_eq!(n(&src), 0, "earlier decided-No arm is dead → its bad literal pruned");
    // Control: `$x = 5` makes the earlier arm live → flagged.
    let live = format!(
        "{HDR}$x = 5;\nmatch ($x) {{ 5 => width(\"abc\"), 6 => helper() }};\n"
    );
    assert_eq!(n(&live), 1);
}

// ---- match: Maybe walks all arms; fall-through joins -----------------------

#[test]
fn match_maybe_walks_all_and_join_agree_keeps_singleton() {
    // `$x` unknown → both arms Maybe → both walked. Both assign the SAME bad value
    // → the join keeps a `Singleton` → the following `width($w)` is flagged.
    let src = format!(
        "{HDR}match ($x) {{ 1 => $w = \"abc\", 2 => $w = \"abc\", default => $w = \"abc\" }};\nwidth($w);\n"
    );
    assert_eq!(n(&src), 1, "agreeing arm joins → Singleton survives → flagged");
}

#[test]
fn match_join_disagree_widens_to_oneof_silent() {
    // Arms assign DIFFERENT bad values → the join is a `OneOf`, which is not a
    // single proven value, so `width($w)` stays silent (the safe side).
    let src = format!(
        "{HDR}match ($x) {{ 1 => $w = \"abc\", 2 => $w = \"xyz\", default => $w = \"pqr\" }};\nwidth($w);\n"
    );
    assert_eq!(n(&src), 0, "disagreeing arms → OneOf → not a proven value → silent");
}

// ---- match: arm refinement binds the subject var ---------------------------

#[test]
fn match_arm_refinement_binds_singleton() {
    // Subject `$s` unknown; inside the `"abc"` arm it is refined to Singleton("abc")
    // — a proven bad string flowing into `width()`.
    let src = "<?php
function width(int $w): int { return $w; }
match ($s) { \"abc\" => width($s), default => 0 };
";
    assert_eq!(n(src), 1, "single-literal arm refines the subject to a proven Singleton");
}

#[test]
fn match_arm_refinement_multi_literal_is_oneof_silent() {
    // Two literal conditions → the subject is refined to a OneOf, not a Singleton,
    // so it is not a single proven value → `width($s)` stays silent.
    let src = "<?php
function width(int $w): int { return $w; }
match ($s) { \"abc\", \"xyz\" => width($s), default => 0 };
";
    assert_eq!(n(src), 0, "multi-literal arm → OneOf → no proven single value → silent");
}

// ---- match without default: no-match throws → tail dead --------------------

#[test]
fn match_no_default_all_no_terminates_tail_dead() {
    // `$x = 9`; both arms decided No, no default → the match provably throws
    // `\UnhandledMatchError`, so the tail is unreachable and its bad literal is
    // pruned.
    let src = "<?php
function width(int $w): int { return $w; }
function f(): void {
    $x = 9;
    match ($x) { 1 => 10, 2 => 20 };
    width(\"abc\");
}
";
    assert_eq!(n(src), 0, "all-No no-default match terminates → tail unreachable → silent");
    // Control: adding a default makes the match fall through → tail reachable.
    let live = "<?php
function width(int $w): int { return $w; }
function f(): void {
    $x = 9;
    match ($x) { 1 => 10, 2 => 20, default => 30 };
    width(\"abc\");
}
";
    assert_eq!(n(live), 1, "a default arm makes the match fall through → tail reachable → flagged");
}

// ---- match without default: UnhandledMatchError surfaces in annotate throws -

#[test]
fn match_no_default_surfaces_unhandled_match_error_throw() {
    let src = "<?php\nfunction f(int $x): void { match ($x) { 1 => 10, 2 => 20 }; }\n";
    let s = summary(src, "f");
    assert!(
        s.throws.iter().any(|t| t == "UnhandledMatchError"),
        "no-default match surfaces \\UnhandledMatchError, got: {:?}",
        s.throws
    );
}

#[test]
fn match_with_default_has_no_unhandled_match_error_throw() {
    let src =
        "<?php\nfunction f(int $x): void { match ($x) { 1 => 10, default => 20 }; }\n";
    let s = summary(src, "f");
    assert!(
        !s.throws.iter().any(|t| t == "UnhandledMatchError"),
        "a default arm cannot raise \\UnhandledMatchError, got: {:?}",
        s.throws
    );
}

// ---- throw origins inside match arms (CST scan is trace-independent) -------

#[test]
fn throw_inside_match_arm_surfaces_and_is_dammed_by_enclosing_try() {
    // The structural throw scan walks the CST, not the trace, so a `throw` inside
    // a match arm is collected with its enclosing try/catch guards intact.
    let raises = "<?php
function f(int $x): void { match ($x) { 1 => throw new \\RuntimeException(), default => 0 }; }
";
    let s = summary(raises, "f");
    assert!(
        s.throws.iter().any(|t| t == "RuntimeException"),
        "throw in a match arm surfaces, got: {:?}",
        s.throws
    );

    // Wrapping the match in a matching try/catch dams the arm's throw.
    let dammed = "<?php
function g(int $x): void {
    try { match ($x) { 1 => throw new \\RuntimeException(), default => 0 }; }
    catch (\\RuntimeException $e) {}
}
";
    let s = summary(dammed, "g");
    assert!(
        !s.throws.iter().any(|t| t == "RuntimeException"),
        "an enclosing catch dams the arm's throw, got: {:?}",
        s.throws
    );
}

// ---- switch: loose `==` evaluation via the measured table ------------------

#[test]
fn switch_loose_case_numeric_string_matches_int() {
    // `switch (5)` with `case "5"`: loose `5 == "5"` is TRUE → the case is live →
    // its bad literal is flagged.
    let src = format!(
        "{HDR}$x = 5;\nswitch ($x) {{ case \"5\": width(\"abc\"); break; }}\n"
    );
    assert_eq!(n(&src), 1, "loose 5 == \"5\" → case live → flagged");
    // The strict `match` on the same shape does NOT match (`5 === \"5\"` is false).
    let strict = format!(
        "{HDR}$x = 5;\nmatch ($x) {{ \"5\" => width(\"abc\") }};\n"
    );
    assert_eq!(n(&strict), 0, "strict 5 === \"5\" → arm dead → silent");
}

#[test]
fn switch_loose_trap_zero_vs_non_numeric_string_php8() {
    // PHP 8: `0 == "abc"` is FALSE (string→number comparison rule changed). The
    // case is decided No → dead → its bad literal is pruned.
    let src = format!(
        "{HDR}$x = 0;\nswitch ($x) {{ case \"abc\": width(\"bad\"); break; }}\n"
    );
    assert_eq!(n(&src), 0, "PHP 8: 0 == \"abc\" is false → case dead → silent");
    // Control: `0 == \"0\"` is TRUE → the case is live → flagged.
    let live = format!(
        "{HDR}$x = 0;\nswitch ($x) {{ case \"0\": width(\"bad\"); break; }}\n"
    );
    assert_eq!(n(&live), 1, "0 == \"0\" → case live → flagged");
}

// ---- switch binds nothing (loose truth sets are multi-valued) --------------

#[test]
fn switch_binds_nothing_in_arm() {
    // Under loose `==`, a `switch` arm does NOT refine the subject: `$s` stays
    // unknown inside `case "abc"`, so `width($s)` is silent...
    let src = "<?php
function width(int $w): int { return $w; }
switch ($s) { case \"abc\": width($s); break; }
";
    assert_eq!(n(src), 0, "switch binds nothing → subject stays unknown → silent");
    // ...whereas the strict `match` of the same shape DOES bind (Singleton) → flagged.
    let matched = "<?php
function width(int $w): int { return $w; }
match ($s) { \"abc\" => width($s), default => 0 };
";
    assert_eq!(n(matched), 1, "match binds the subject Singleton → flagged (contrast)");
}

// ---- adversarial: a fall-through switch stays Opaque -----------------------

#[test]
fn fallthrough_switch_stays_opaque_no_misbinding() {
    // `case 1` has no `break` and falls through into `case 2`, so `width("abc")`
    // in `case 2` DOES run when `$x == 1`. Structuring would (wrongly) prune it as
    // a decided-No case; staying Opaque keeps it reachable → the direct pass
    // flags it. This is the load-bearing adversarial: a fall-through that would
    // misbind if structured.
    let fallthrough = format!(
        "{HDR}$x = 1;\nswitch ($x) {{ case 1: helper(); case 2: width(\"abc\"); break; }}\n"
    );
    assert_eq!(n(&fallthrough), 1, "fall-through case reaches the bad literal → Opaque → flagged");
    // With a proper `break`, the same shape IS structured: `$x = 1` decides case 1,
    // so case 2 is dead → its bad literal is pruned.
    let structured = format!(
        "{HDR}$x = 1;\nswitch ($x) {{ case 1: helper(); break; case 2: width(\"abc\"); break; }}\n"
    );
    assert_eq!(n(&structured), 0, "no fall-through → structured → decided-No case pruned");
}

// ---- non-lowerable arm makes the WHOLE construct Opaque --------------------

#[test]
fn non_lowerable_arm_condition_forces_whole_opaque() {
    // One arm's condition is a call (`helper()`), which does not lower to a
    // variable/literal → the WHOLE match stays Opaque, so the decided `5` arm does
    // NOT prune the sibling arm's bad literal.
    let src = format!(
        "{HDR}$x = 5;\nmatch ($x) {{ helper() => width(\"abc\"), 5 => 0 }};\n"
    );
    assert_eq!(n(&src), 1, "a non-lowerable arm condition keeps the whole match Opaque → no pruning");
    // Control: a fully-lowerable version IS structured → the `helper()`-replaced
    // arm becomes a decided-No literal `4` and is pruned.
    let structured = format!(
        "{HDR}$x = 5;\nmatch ($x) {{ 4 => width(\"abc\"), 5 => 0 }};\n"
    );
    assert_eq!(n(&structured), 0, "fully-lowerable → structured → decided-No arm pruned");
}

// ---- assignment-RHS match is unchanged (not structured) --------------------

#[test]
fn assignment_rhs_match_is_not_structured() {
    // `$w = match (...) { ... }` keeps today's behavior: the RHS is not flow-
    // structured, so a decided arm does NOT prune a sibling arm's bad literal —
    // the direct pass still flags it.
    let src = format!(
        "{HDR}$x = 5;\n$w = match ($x) {{ 5 => 1, 6 => width(\"abc\") }};\n"
    );
    assert_eq!(n(&src), 1, "assignment-RHS match is unstructured → sibling bad literal still flagged");
    // Contrast: the SAME shape in statement position IS structured → pruned.
    let stmt = format!(
        "{HDR}$x = 5;\nmatch ($x) {{ 5 => 1, 6 => width(\"abc\") }};\n"
    );
    assert_eq!(n(&stmt), 0, "statement-position match is structured → sibling arm pruned");
}

// ---- adversarial: first-match ordering with an overlapping truthy subject --

#[test]
fn ordering_rule_prevents_later_arm_sole_live_when_earlier_maybe() {
    // `match (true)` with variable conditions. `$a` is unknown → arm 1 is Maybe;
    // `$b = true` → arm 2 WOULD be a decided match in isolation, but because an
    // earlier arm is Maybe (not provably No), arm 2 is NOT decided-Yes, so the
    // `default` is NOT pruned and its bad literal is flagged.
    let src = "<?php
function width(int $w): int { return $w; }
$b = true;
match (true) { $a => 1, $b => 2, default => width(\"abc\") };
";
    assert_eq!(n(src), 1, "earlier-Maybe arm blocks a later sole-live decision → default stays live");
    // Control: make arm 1 a provable No (`$a = false`) → arm 2 becomes decided-Yes
    // → the default is pruned.
    let control = "<?php
function width(int $w): int { return $w; }
$a = false;
$b = true;
match (true) { $a => 1, $b => 2, default => width(\"abc\") };
";
    assert_eq!(n(control), 0, "earlier provable-No → arm 2 decided-Yes → default pruned");
}
