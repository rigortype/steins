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
    assert_eq!(dumps(src), vec!["5: int", "11: int"]);
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
    assert_eq!(dumps(src), vec!["5: int"]);
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
