//! Jumps credited by level, and the last case that may end (ADR-0103, issues #904
//! and #944).
//!
//! A `switch` arm used to be structured only when no `break`, `continue` or `goto`
//! sat in its body outside a nested loop or `switch`, and the scan did not look
//! inside those: `case 1: foreach (…) { break 2; } return;` read as an arm ending in
//! its `return`, every arm terminated, and the code after the switch was dead to the
//! walk although PHP runs it. A last case without `break` kept the whole switch
//! opaque, so nothing inside any of its arms was checked.
//!
//! Every fixture is a witness probed on PHP 8.5.11; the expected rows are what PHP
//! does. A fixture that expects silence has a twin that expects the finding.

use steins_infer::{Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// An engine that opens the absence family the `call.undefined-*` ids need and
/// says that no `nope*` function is resident.
struct Probe;

impl Folder for Probe {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        Some(!fqn.starts_with("nope"))
    }
}

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Probe)
}

/// The lines of the findings of `src` with id `id`, sorted.
fn lines_of(src: &str, id: &str) -> Vec<u32> {
    let mut out: Vec<u32> = run(src).into_iter().filter(|d| d.id == id).map(|d| d.line).collect();
    out.sort_unstable();
    out
}

/// Every `debug.type` answer of `src`, as `line: rendered`.
fn dumps(src: &str) -> Vec<String> {
    run(src)
        .into_iter()
        .filter(|d| d.id == "debug.type")
        .map(|d| {
            let answer = d.message.split_once("dumped type: ").map_or("?", |(_, a)| a);
            format!("{}: {answer}", d.line)
        })
        .collect()
}

// ---- #904: a jump out of a nested loop lands after the switch -------------------

#[test]
fn break_2_out_of_a_loop_in_a_case_reaches_the_successor() {
    // w904c (PHP: Error on null at line 8). The arm's own walk ends in `return`; the
    // `break 2` is what reaches line 7.
    let src = "<?php
$k = 1;
switch ($k) {
    case 1: foreach ([1] as $v) { break 2; } return;
    default: return;
}
$x = null;
$x->bar();
";
    assert_eq!(lines_of(src, "call.on-null"), vec![8]);
}

#[test]
fn continue_2_out_of_a_loop_in_a_case_reaches_the_successor() {
    // w904b `q`: a `continue 2` whose second level is the switch is its `break 2`
    // (PHP 8.5.11 warns at compile time and runs line 12, which errors).
    let src = "<?php
function q(int $k): void {
    $x = null;
    switch ($k) {
        case 1: foreach ([1] as $v) { continue 2; } return;
        default: return;
    }
    $x->bar();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![8]);
}

#[test]
fn break_3_out_of_a_switch_in_an_infinite_loop_is_the_loop_s() {
    // w904b `r`, the control: the jump leaves the `while`, whose own count owns it.
    let src = "<?php
function r(int $k): void {
    $x = null;
    while (true) {
        switch ($k) {
            case 1: foreach ([1] as $v) { break 3; } return;
            default: return;
        }
    }
    $x->bar();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![10]);
}

#[test]
fn break_2_out_of_a_switch_in_a_loop_ends_the_arm() {
    // `break 2` written in the case body leaves the `foreach`, not to the code after
    // the switch: with `default` returning, line 7 never runs (PHP 8.5.11: no
    // output). The switch used to be opaque here, which kept line 7 live.
    let src = "<?php
function f(int $k, array $xs): void {
    foreach ($xs as $v) {
        $x = null;
        switch ($k) { case 1: nope_two(); break 2; default: return; }
        $x->bar();
    }
}
";
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
    assert_eq!(lines_of(src, "call.undefined-function"), vec![5]);
}

#[test]
fn a_mid_arm_break_lands_on_the_successor() {
    // A guarded `break;` inside the case body (PHP: `$k = 1` runs line 6, Error).
    let src = "<?php
function g(int $k, bool $c): void {
    $x = null;
    switch ($k) { case 1: if ($c) { break; } return; default: return; }
    nope_mid();
    $x->bar();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![6]);
    assert_eq!(lines_of(src, "call.undefined-function"), vec![5]);
}

#[test]
fn the_landing_forgets_what_the_case_body_writes() {
    // The jump may leave after the assignment: PHP 8.5.11 prints `1`, and no `null`
    // receiver may be claimed on line 6.
    let src = "<?php
function h(int $k, bool $c): void {
    $x = null;
    switch ($k) { case 1: $x = new ArrayObject(); foreach ([1] as $v) { break 2; } return; default: return; }
    \\PHPStan\\dumpType($x);
    echo $x->count();
}
";
    assert_eq!(dumps(src), vec!["5: unknown"]);
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
}

// ---- continue, goto and try/finally inside a case ---------------------------------

#[test]
fn a_continue_in_a_switch_is_its_break() {
    // PHP 8.5.11: `"continue" targeting switch is equivalent to "break"` (a compile
    // warning since 7.3), then line 5 runs. The arm used to keep the switch opaque.
    let src = "<?php
function c(int $k): void {
    $x = null;
    switch ($k) { case 1: nope_c(); continue; default: return; }
    $x->bar();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![5]);
    assert_eq!(lines_of(src, "call.undefined-function"), vec![4]);
}

#[test]
fn a_goto_in_a_case_keeps_the_successor_live_and_carries_nothing() {
    // `goto` leaves for its label, wherever that is: the successor stays reachable,
    // and nothing is known on that edge. PHP 8.5.11: `g(1)` skips line 6.
    let src = "<?php
function g(int $k): void {
    $x = null;
    switch ($k) { case 1: nope_g(); goto out; default: return; }
    nope_after();
    $x->bar();
    out: echo 1;
}
";
    assert_eq!(lines_of(src, "call.undefined-function"), vec![4, 5]);
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
}

#[test]
fn a_break_through_a_finally_lands_with_the_finally_s_writes_forgotten() {
    // PHP 8.5.11: the `finally` runs before the `break` lands, so `$x` is an
    // `ArrayObject` on line 9 and `count()` succeeds; the `return` on line 7 never
    // runs.
    let src = "<?php
function t(int $k): void {
    $x = null;
    switch ($k) {
        case 1:
            try { break; } finally { $x = new ArrayObject(); nope_fin(); }
            return;
        default: return;
    }
    echo $x->count();
}
";
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
    assert_eq!(lines_of(src, "call.undefined-function"), vec![6]);
}

#[test]
fn a_break_through_a_finally_still_reaches_the_successor() {
    // The twin: nothing rebinds `$x`, so line 10 is a proven Error (PHP 8.5.11).
    let src = "<?php
function t(int $k): void {
    $x = null;
    switch ($k) {
        case 1:
            try { break; } finally { echo 1; }
            return;
        default: return;
    }
    $x->bar();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![10]);
}

// ---- #944: the last case may end without `break` ----------------------------------

#[test]
fn a_last_case_without_break_is_structured() {
    // w944 `w` and its control `w2` (PHP: `nope_fn()` is an Error).
    let src = "<?php
function w(int $n): void {
    switch ($n) {
        case 1: nope_fn(); break;
        case 2: \\PHPStan\\dumpType($n);
    }
}
function w2(int $n): void {
    switch ($n) {
        case 1: nope_fn(); break;
        case 2: \\PHPStan\\dumpType($n); break;
    }
}
";
    assert_eq!(lines_of(src, "call.undefined-function"), vec![4, 10]);
    assert_eq!(dumps(src), vec!["5: 2", "11: 2"]);
}

#[test]
fn a_last_case_ending_in_a_call_falls_to_the_successor() {
    // The last case runs off its end and leaves the switch (PHP 8.5.11: `l(2)`
    // reaches line 5, Error).
    let src = "<?php
function l(int $n): void {
    $x = null;
    switch ($n) { case 1: return; default: echo 1; }
    $x->bar();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![5]);
}

#[test]
fn trailing_empty_labels_are_empty_arms() {
    // w944 `w3`: the last body falls into an empty `default:`; and trailing empty
    // `case`s (PHP 8.5.11: every path reaches the end).
    let src = "<?php
function w3(int $n): void {
    switch ($n) {
        case 1: nope_fn(); break;
        case 2: \\PHPStan\\dumpType($n);
        default:
    }
}
function w5(int $n): void {
    switch ($n) {
        case 1: nope_five(); break;
        case 2: case 3:
    }
}
";
    assert_eq!(lines_of(src, "call.undefined-function"), vec![4, 11]);
    assert_eq!(dumps(src), vec!["5: 2"]);
}

#[test]
fn a_fall_through_into_a_non_empty_case_stays_opaque() {
    // w944 `w4`: the edge from case 1 into case 2 is not modelled.
    let src = "<?php
function w4(int $n): void {
    switch ($n) {
        case 1: nope_fn();
        case 2: \\PHPStan\\dumpType($n); break;
    }
}
";
    assert_eq!(lines_of(src, "call.undefined-function"), Vec::<u32>::new());
    assert_eq!(dumps(src), Vec::<String>::new());
}

// ---- round 1 review: goto edges, terminating finally ------------------------------

#[test]
fn a_goto_landing_does_not_erase_what_the_other_paths_know() {
    // The reviewer's `g3`/`g4`: `g3(2)` and `g4(2)` reach line 4 and 7 with `$o`
    // null (PHP 8.5.11: "Call to a member function m() on null"). Every path from the
    // `goto` to the call passes the label, so the `goto` adds nothing to the join.
    let src = "<?php
function g3(int $k): void {
    $o = null; switch ($k) { case 1: goto out; }
    $o->m();
    out: echo 1;
}
function g4(int $k): void {
    $o = null; $p = 1; switch ($k) { case 1: $p = 2; goto out; }
    $o->m();
    out: echo 1;
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![4, 9]);
}

#[test]
fn a_goto_alone_keeps_the_successor_reachable_with_nothing_known() {
    // Every other arm returns: only the `goto` reaches past the switch, through the
    // label's barrier, so line 4 is reachable and nothing is known there.
    let src = "<?php
function g(int $k): void {
    $o = null; switch ($k) { case 1: goto out; default: return; }
    \\PHPStan\\dumpType($o);
    out: echo 1;
}
";
    assert_eq!(dumps(src), vec!["4: unknown"]);
}

#[test]
fn a_jump_replaced_by_a_returning_finally_does_not_land() {
    // The reviewer's `r4` (PHP 8.5.11: `r4(1)` returns from the `finally`, and line 6
    // never runs). Base and the first S2 head both reported it.
    let src = "<?php
function r4(int $k): void {
    switch ($k) {
        case 1: try { break; } finally { return; }
        default: return;
    }
    $n = null; $n->foo();
}
";
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
    // Control: a `finally` that falls through lets the `break` land.
    let control = src.replace("finally { return; }", "finally { echo 1; } return;");
    assert_eq!(lines_of(&control, "call.on-null"), vec![7]);
}

#[test]
fn a_switch_whose_trailing_label_falls_through_misses_a_return() {
    // The reviewer's `s1`, `s3`, `s6` (PHP 8.5.11: TypeError "none returned").
    let src = "<?php
function s1(int $k): int { switch ($k) { case 1: return 1; default: } }
function s3(int $k, bool $c): int { do { switch ($k) { case 1: return 1; default: } } while ($c); }
function s6(int $k): int { switch ($k) { case 1: return 1; default: return 2; case 3: } }
function ok(int $k): int { switch ($k) { case 1: return 1; default: return 2; } }
";
    let mut lines = lines_of(src, "type.return-missing");
    lines.extend(lines_of(src, "type.return-maybe-missing"));
    lines.sort_unstable();
    assert_eq!(lines, vec![2, 3, 4]);
}

// ---- round 2 review: an unconditional goto ends its list ------------------------

#[test]
fn an_unconditional_goto_ends_its_list() {
    // The reviewer's `h1` and its `if` twin (PHP 8.5.11: neither `$o = null;` runs,
    // so `$o->m()` succeeds or is skipped). A `goto` used to read as a barrier that
    // falls through, which carried the dead `$o = null` to the read.
    let src = "<?php
final class X { public function m(): int { return 1; } }
function h1(int $k): void {
    $o = new X(); switch ($k) { case 1: goto out; $o = null; break; default: return; }
    $o->m(); out: echo 1;
}
function i1(bool $c): void {
    $o = new X(); if ($c) { goto out; $o = null; }
    $o->m(); out: echo 1;
}
function e1(bool $c): void {
    if ($c) { goto out; } else { goto out; }
    nope_dead();
    out: nope_live();
}
";
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
    assert_eq!(lines_of(src, "call.undefined-function"), vec![14]);
}

#[test]
fn a_label_resumes_the_walk_with_nothing_known() {
    // A terminator ends a list only up to the next label: `f(1)` reaches line 4 and
    // fails on null (PHP 8.5.11). The walk used to call it dead after the `return`.
    let src = "<?php
function f(int $k): void {
    if ($k) { goto out; } return;
    out: $n = null; $n->foo();
}
function later(): void {
    $o = new ArrayObject(); goto out; $o = null;
    out: echo $o->count();
}
function nested(bool $c): void {
    $o = new ArrayObject(); goto in; $o = null;
    if ($c) { in: echo $o->count(); $n = null; $n->foo(); }
}
function backward(): void {
    $o = null; top: if ($o) { echo $o->count(); return; } $o = new ArrayObject(); goto top;
}
function into_case(int $k): void {
    $o = new ArrayObject();
    switch ($k) { case 1: goto two; case 2: two: $o = null; break; default: return; }
    $o->count();
}
";
    // Line 4 is the resumed list; 20 the reviewer's case-to-case `goto`. Line 12,
    // the twin inside the `if` a `goto` enters (PHP lets it, even when `$c` is
    // false), is a known false negative: an `if` holding a label is dead up to the
    // label, and the walk does not enter it there (ADR-0103 §2.2b).
    assert_eq!(lines_of(src, "call.on-null"), vec![4, 20]);
}

#[test]
fn a_terminator_then_a_label_does_not_end_a_case() {
    // The architect's `p1` (round 3). Case 1 runs on past `out:` into case 2, so every
    // PHP path through `p1` returns and line 10 is dead; `p1b` reaches line 18 with an
    // `X`. Read as terminating, case 1 was structured and its resumed run fell onto
    // the successor with `$o` null.
    let src = "<?php
final class X { public function m(): int { return 1; } }
function p1(int $k, bool $c): void {
    $o = new X();
    switch ($k) {
        case 1: if ($c) { goto out; } return; out: $o = null;
        case 2: $o = new X(); return;
        default: return;
    }
    $o->m();
}
function p1b(int $k, bool $c): void {
    $o = new X();
    switch ($k) {
        case 1: if ($c) { goto out; } return; out: $o = null;
        case 2: $o = new X(); break;
    }
    $o->m();
}
";
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
}

#[test]
fn an_if_holding_a_label_is_dead_before_the_label() {
    // The reviewer's `a3` (PHP 8.5.11 prints `a3`; `$o->m()` never runs): after the
    // `return`, a `goto` enters the `if` at `l:`, so what precedes the label in it is
    // dead. The walk resumes after the `if` with nothing known; `b3` is the twin that
    // shows the code after it is still walked.
    let src = "<?php
final class X { public function m(): int { return 1; } }
function a3(bool $c): void {
    if ($c) goto l;
    return;
    if ($c) { $o = null; $o->m(); l: echo 1; }
}
function b3(bool $c): void {
    if ($c) goto l;
    return;
    if ($c) { l: echo 1; }
    $n = null; $n->m();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![12]);
}

// ---- #1041: a loose switch refines an int or bool subject -----------------------
//
// Under `==` an int subject agrees with `===` against int labels, so the case binds
// the label set. A nullable int meets `null == 0`, and a `?bool` meets `null == false`,
// so those labels keep `null`. Everything else (strings, floats, mixed kinds) declines.
// Every fixture is a witness probed on PHP 8.5.

const LOOSE_HDR: &str =
    "<?php\ndeclare(strict_types=1);\nclass X { function m(): string { return 'm'; } }\n";

#[test]
fn loose_int_case_that_leaves_a_local_alone_is_silent() {
    // h3 / h3r / h3s: inside `case 1:` the `if` over `$k` is decided true, so the
    // `$o = null` below it never runs and `$o->m()` is not on null.
    for guard in ["if ($k) { return 'r'; }", "if ($k === 1) { return 'r'; }"] {
        let src = format!(
            "{LOOSE_HDR}function f(int $k): string {{ $o = new X(); switch ($k) {{ case 1: {guard} $o = null; break; default: return 'd'; }} return $o->m(); }}\n"
        );
        assert!(lines_of(&src, "call.on-null").is_empty(), "silent for `{guard}`");
    }
}

#[test]
fn loose_int_case_that_does_null_the_local_still_reports() {
    // h3m: the control. `case 1:` sets `$o = null` outright; PHP throws an Error.
    let src = format!(
        "{LOOSE_HDR}function f(int $k): string {{ $o = new X(); switch ($k) {{ case 1: $o = null; break; default: return 'd'; }} return $o->m(); }}\n"
    );
    assert_eq!(lines_of(&src, "call.on-null"), vec![4]);
}

#[test]
fn loose_nullable_int_case_zero_keeps_null_and_keeps_the_true_positive() {
    // trap_nullable: `null == 0`, so `case 0:` is taken for null and `$k === 0` is
    // false there; `$o = null` runs and PHP throws. The finding must stay.
    let src = format!(
        "{LOOSE_HDR}function f(?int $k): string {{ $o = new X(); switch ($k) {{ case 0: if ($k === 0) {{ return 'zero'; }} $o = null; break; default: return 'd'; }} return $o->m(); }}\n"
    );
    assert_eq!(lines_of(&src, "call.on-null"), vec![4]);
}

#[test]
fn loose_switch_dumps_the_label_sets() {
    // h3d: the arm binds its labels; `default` binds nothing the no-match path has not.
    let src = format!(
        "{LOOSE_HDR}function f(int $k): void {{\nswitch ($k) {{\ncase 1: \\PHPStan\\dumpType($k); break;\ncase 2: case 3: \\PHPStan\\dumpType($k); break;\ndefault: \\PHPStan\\dumpType($k);\n}}\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["6: 1", "7: 2|3", "8: int"]);
}

#[test]
fn loose_bool_switch_binds_the_bool_label() {
    let src = format!(
        "{LOOSE_HDR}function f(bool $b): void {{\nswitch ($b) {{\ncase true: \\PHPStan\\dumpType($b); break;\ncase false: \\PHPStan\\dumpType($b); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["6: true", "7: false"]);
}

#[test]
fn loose_nullable_bool_false_label_keeps_null() {
    // `null == false`: `case false:` is taken for null; `case true:` is not.
    let src = format!(
        "{LOOSE_HDR}function f(?bool $b): void {{\nswitch ($b) {{\ncase false: \\PHPStan\\dumpType($b); break;\ncase true: \\PHPStan\\dumpType($b); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["6: false|null", "7: true"]);
}

#[test]
fn loose_nullable_int_labels_dump_with_and_without_null() {
    // trap dumps: only the `0` label keeps null.
    let src = format!(
        "{LOOSE_HDR}function f(?int $k): void {{\nswitch ($k) {{\ncase 0: \\PHPStan\\dumpType($k); break;\ncase 1: \\PHPStan\\dumpType($k); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["6: 0|null", "7: 1"]);
}

#[test]
fn loose_switch_declines_a_string_subject() {
    // trap_string: `'01'` and `'1.0'` take `case '1'`, so the case is not `'1'`.
    let src = format!(
        "{LOOSE_HDR}function f(string $s): void {{\nswitch ($s) {{\ncase '1': \\PHPStan\\dumpType($s); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["6: string"]);
}

#[test]
fn loose_switch_declines_a_float_subject() {
    // trap_float: `1.0 == 1`, so the case is not an int.
    let src = format!(
        "{LOOSE_HDR}function f(float $f): void {{\nswitch ($f) {{\ncase 1: \\PHPStan\\dumpType($f); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["6: float"]);
}

#[test]
fn loose_switch_declines_mixed_and_string_labels_on_an_int() {
    // trap_mixed_labels / trap_int_string_label.
    let mixed = format!(
        "{LOOSE_HDR}function f(int $k): void {{\nswitch ($k) {{\ncase 1: case 'a': \\PHPStan\\dumpType($k); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&mixed), vec!["6: int"]);
    let string_label = format!(
        "{LOOSE_HDR}function f(int $k): void {{\nswitch ($k) {{\ncase '1': \\PHPStan\\dumpType($k); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&string_label), vec!["6: int"]);
}

#[test]
fn loose_switch_declines_a_bool_label_on_an_int_subject() {
    // `true == 1` and `true == 2`: a bool label is a truthiness test on an int.
    let src = format!(
        "{LOOSE_HDR}function f(int $k): void {{\nswitch ($k) {{\ncase true: \\PHPStan\\dumpType($k); break;\n}}\n}}\n"
    );
    assert_eq!(dumps(&src), vec!["6: int"]);
}

#[test]
fn loose_arm_keeps_the_subjects_thinned_flag() {
    // #1071 path rule: `$k` is a OneOf walked past an undecided exit, so "every
    // member is rejected" is no proof inside the case. PHP never reaches `takes`
    // (`$y > 0` holds for both members), so the finding stays maybe-grade.
    let src = "<?php
declare(strict_types=1);
function takes(string $s): void {}
function sw(bool $c): void { $k = $c ? 1 : 2; $y = $k; if ($y > 0 && time() > 0) return; switch ($k) { case 1: case 2: takes($k); } }
";
    assert!(lines_of(src, "type.argument-mismatch").is_empty(), "no proven mismatch");
}

#[test]
fn loose_int_case_reports_a_proven_argument_mismatch() {
    // The true positive the narrowing gains: `$k` is 1 inside the case.
    let src = "<?php
declare(strict_types=1);
function takes(string $s): void {}
function f(int $k): void { switch ($k) { case 1: takes($k); } }
";
    assert_eq!(lines_of(src, "type.argument-mismatch"), vec![4]);
}

#[test]
fn strict_arm_keeps_the_subjects_thinned_flag() {
    // #1090 (the `match` twin of the pin above): the strict arm used to be rebuilt as a
    // fresh `Verified` value, so the thinned `$k` came back un-thinned and the guard's
    // exit was forgotten. PHP never reaches `takes` (it prints `ok`): no proven
    // mismatch, and the possibly grade stays.
    let src = "<?php
declare(strict_types=1);
function takes(string $s): void {}
function mt(bool $c): void { $k = $c ? 1 : 2; $y = $k; if ($y > 0 && time() > 0) return; match ($k) { 1, 2 => takes($k), }; }
";
    assert!(lines_of(src, "type.argument-mismatch").is_empty(), "no proven mismatch");
    assert_eq!(lines_of(src, "type.maybe-argument-mismatch"), vec![4]);
}

#[test]
fn strict_arm_does_not_inherit_a_closure_subject() {
    // A closure is never `===` a literal: PHP prints `ok` for both, so the arm's
    // value is the label and `$f(...)` must not resolve to the subject's closure.
    let call = "<?php
declare(strict_types=1);
function mt(): void {
    $f = fn(int $x): int => $x;
    $r = match ($f) { 1 => $f(\"a\"), default => 0 };
    echo $r, \"\\n\";
}
";
    assert!(lines_of(call, "type.argument-mismatch").is_empty(), "call arg");
    let body = "<?php
declare(strict_types=1);
function takes(string $s): void {}
function mt(): void {
    $f = function (int $x): void { takes($x); };
    match ($f) { 1 => $f(5), default => null };
}
";
    assert!(lines_of(body, "type.argument-mismatch").is_empty(), "closure body");
}

#[test]
fn strict_arm_without_an_exit_keeps_the_proven_mismatch() {
    // No exit between the binding and the `match`: both members reach `takes`, which
    // raises a TypeError on either, so the proof stays.
    let src = "<?php
declare(strict_types=1);
function takes(string $s): void {}
function mt(bool $c): void { $k = $c ? 1 : 2; match ($k) { 1, 2 => takes($k), }; }
";
    assert_eq!(lines_of(src, "type.argument-mismatch"), vec![4]);
}

#[test]
fn strict_arm_over_an_asserted_subject_keeps_the_proven_mismatch() {
    // The strict arm's fact is exactly the label, established by `===`, so it is
    // `Verified` whatever the subject's stratum (decision 7 of #1114): the `@param`
    // subject's TypeError on 1 is still proven.
    let src = "<?php
declare(strict_types=1);
function takes_string(string $s): void {}
/** @param 1|2 $k */
function f(int $k): void { match ($k) { 1 => takes_string($k) }; }
";
    assert_eq!(lines_of(src, "type.argument-mismatch"), vec![5]);
}
