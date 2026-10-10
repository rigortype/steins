//! `try` is a sub-trace (issues #943 and #905, the ADR-0027 `try` amendment).
//!
//! The construct used to lower to an `Opaque`: nothing inside a `try` body, a
//! `catch` or a `finally` was walked, and the walk always fell through it — so a
//! `try { return; } finally { … }` left the code after it live, and the default
//! profile reported a `null` receiver on a line PHP never runs (the t10 shape).
//!
//! Every fixture is a witness probed on PHP 8.5.11; the expected rows are what PHP
//! does. A fixture that expects silence has a twin that expects the finding, so
//! none of them passes because the finding was never reachable.

use steins_infer::{Diagnostic, Folder, check_with};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, SourceTree};

/// An engine that opens the absence family the `call.undefined-*` ids need, says
/// that no `nope*` function and no `NOPE*` constant is resident, and reflects
/// `strlen`'s one parameter.
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
    fn boot_surface_constant(&mut self, name: &str) -> Option<bool> {
        Some(!name.starts_with("NOPE"))
    }
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        name.eq_ignore_ascii_case("strlen").then(|| {
            vec![BuiltinParam {
                name: "string".to_owned(),
                ty: Some("string".to_owned()),
                by_ref: false,
                variadic: false,
                optional: false,
            }]
        })
    }
}

/// Every finding of `src` under [`Probe`], the dumps and the `untyped.*` surface
/// left out.
fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Probe)
        .into_iter()
        .filter(|d| d.id != "debug.type" && !d.id.starts_with("untyped."))
        .collect()
}

/// Every finding a source produces, as `line id`, sorted.
fn findings(src: &str) -> Vec<String> {
    let mut out: Vec<String> =
        run(src).into_iter().map(|d| format!("{} {}", d.line, d.id)).collect();
    out.sort();
    out
}

/// The findings of `src` with id `id`, as line numbers.
fn lines_of(src: &str, id: &str) -> Vec<u32> {
    let mut out: Vec<u32> = run(src).into_iter().filter(|d| d.id == id).map(|d| d.line).collect();
    out.sort_unstable();
    out
}

/// Every `debug.type` answer of `src`, as `line: rendered`.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Probe)
        .into_iter()
        .filter(|d| d.id == "debug.type")
        .map(|d| {
            let answer = d.message.split_once("dumped type: ").map_or("?", |(_, a)| a);
            format!("{}: {answer}", d.line)
        })
        .collect()
}

// ---- #943: the body, the catches and the finally are walked ------------------

#[test]
fn a_call_inside_a_try_body_is_checked_like_its_control() {
    // t2 and its control t1 (PHP: Error on both lines).
    let src = "<?php
final class K {}
function t1(): void { $x = null; $x->bar(); }
function t2(): void { try { $x = null; $x->bar(); } finally {} }
";
    assert_eq!(lines_of(src, "call.on-null"), vec![3, 4]);
}

#[test]
fn an_undefined_function_inside_a_try_body_is_reported() {
    // t3 (PHP: Error, `catch (Exception)` does not catch it).
    let src = "<?php
function t3(): void { try { nope_fn(); } catch (Exception $e) {} }
function control(): void { nope_fn(); }
";
    assert_eq!(lines_of(src, "call.undefined-function"), vec![2, 3]);
}

#[test]
fn a_dump_inside_a_try_body_answers() {
    // t4: the body is walked on the straight-line env.
    let src = "<?php
function t4(): void { try { $x = 1; \\PHPStan\\dumpType($x); } catch (Exception $e) {} }
";
    assert_eq!(dumps(src), vec!["2: 1"]);
}

#[test]
fn a_type_error_inside_a_try_body_is_reported() {
    // t5's shape (PHP: TypeError, not an `Exception`), through a user callee.
    let src = "<?php
declare(strict_types=1);
function t5(): void { try { takes_int('a'); } catch (Exception $e) {} }
function control(): void { takes_int('a'); }
function takes_int(int $i): void {}
";
    assert_eq!(lines_of(src, "type.argument-mismatch"), vec![3, 4]);
}

// ---- the successor: the join of the normal exits (S1b) ----------------------
//
// Witnessed on PHP 8.5.11 with `var_dump` in place of each dump, both with the
// block falling through and with `f()` throwing `E`.

#[test]
fn the_successor_joins_the_block_and_the_catch() {
    // t6 (PHP: 1 when nothing throws, 2 when the catch runs).
    let src = "<?php
function t6(): void { try { $x = 1; } catch (Exception $e) { $x = 2; } \\PHPStan\\dumpType($x); }
";
    assert_eq!(dumps(src), vec!["2: 1|2"]);
}

#[test]
fn a_catch_that_returns_leaves_the_block_alone_at_the_successor() {
    // PHP: 1, and nothing on the throwing path.
    let src = "<?php
final class E extends Exception {}
function r(): void { try { $x = 1; f(); } catch (E $e) { return; } \\PHPStan\\dumpType($x); }
function f(): void {}
";
    assert_eq!(dumps(src), vec!["3: 1"]);
}

#[test]
fn a_block_that_always_throws_leaves_the_catches_alone_at_the_successor() {
    // PHP: 2. The catch enters with the block's write forgotten, so its own write
    // is all the successor knows; the entry's `0` and the block's `1` are gone.
    let src = "<?php
final class E extends Exception {}
function r(): void { $x = 0; try { $x = 1; throw new E('x'); } catch (E $e) { $x = 2; } \\PHPStan\\dumpType($x); }
";
    assert_eq!(dumps(src), vec!["3: 2"]);
}

#[test]
fn a_name_the_catch_path_never_assigns_is_not_known_after_the_try() {
    // PHP: 1 when nothing throws; when `f()` throws, "Warning: Undefined variable
    // $x" and NULL. The walk knows nothing of `$x` after the construct, and the
    // presence pass reports the read on the strict floor; the twin whose catch
    // assigns is silent and knows `1|2`. The presence half reads without a dump,
    // whose unresolved callee may take `$x` by reference.
    let src = "<?php
final class E extends Exception {}
function r(): void { try { f(); $x = 1; } catch (E $e) { } \\PHPStan\\dumpType($x); }
function twin(): void { try { f(); $x = 1; } catch (E $e) { $x = 2; } \\PHPStan\\dumpType($x); }
function f(): void {}
";
    assert_eq!(dumps(src), vec!["3: unknown", "4: 1|2"]);
    let src = "<?php
final class E extends Exception {}
function r(): void { try { f(); $x = 1; } catch (E $e) { } echo $x; }
function twin(): void { try { f(); $x = 1; } catch (E $e) { $x = 2; } echo $x; }
function f(): void {}
";
    let tree = SourceTree::parse(src);
    let maybe: Vec<u32> = steins_infer::check_full(&tree, "t.php", &mut Probe, true)
        .into_iter()
        .filter(|d| d.id == "variable.maybe-undefined")
        .map(|d| d.line)
        .collect();
    assert_eq!(maybe, vec![3]);
}

#[test]
fn a_finally_runs_on_the_join_and_its_writes_reach_the_successor() {
    // PHP: `a` prints 1|2 then 3; `b` prints 5 on both paths; `c` prints 1.
    let src = "<?php
final class E extends Exception {}
function a(): void {
    try { $x = 1; f(); } catch (E $e) { $x = 2; } finally { $y = 3; }
    \\PHPStan\\dumpType($x);
    \\PHPStan\\dumpType($y);
}
function b(): void {
    try { $x = 1; f(); } catch (E $e) { } finally { $x = 5; }
    \\PHPStan\\dumpType($x);
}
function c(): void {
    try { $x = 1; } finally { $y = $x; }
    \\PHPStan\\dumpType($y);
}
function f(): void {}
";
    assert_eq!(dumps(src), vec!["5: 1|2", "6: 3", "10: 5", "14: 1"]);
}

#[test]
fn a_finally_is_judged_on_every_path_into_it_and_reported_once() {
    // PHP: 2 when nothing throws, 3 when `f()` does, both in the finally and
    // after it. The finally's own findings come from its walk over every entry,
    // where an exception may leave the block before either write; the walk from
    // the normal exits that builds the successor reports nothing, so the dump in
    // the finally answers once.
    let src = "<?php
final class E extends Exception {}
function r(): void {
    $x = 0;
    try { $x = 1; f(); $x = 2; } catch (E $e) { $x = 3; } finally { \\PHPStan\\dumpType($x); }
    \\PHPStan\\dumpType($x);
}
function f(): void {}
";
    assert_eq!(dumps(src), vec!["5: unknown", "6: 2|3"]);
}

#[test]
fn a_branch_the_normal_paths_skip_in_a_finally_stays_live() {
    // PHP: when `f()` throws `E`, the finally runs with `$x` still 0 and the
    // `else` reads the missing constant (Error). The walk from the normal exits
    // decides the `if`, and the region it would skip must not be marked dead: the
    // file-wide constant pass skips a reference there.
    let src = "<?php
final class E extends Exception {}
function r(): void {
    $x = 0;
    try { f(); $x = 1; } catch (E $e) { return; } finally { if ($x === 1) { echo 1; } else { echo NOPE_CONST; } }
}
function f(): void {}
";
    assert_eq!(lines_of(src, "constant.undefined"), vec![5]);
}

#[test]
fn a_finally_that_returns_on_the_normal_path_ends_the_construct() {
    // PHP: `r(true)` returns 7 from the finally; the line after never runs. The
    // finally's walk over every entry cannot decide `$x`, the walk from the
    // normal exits can.
    let src = "<?php
final class E extends Exception {}
function r(): int {
    $o = null;
    try { $x = 1; f(); } catch (E $e) { $x = 2; } finally { if ($x > 0) { return 7; } }
    $o->bar();
    return 0;
}
function f(): void {}
";
    assert_eq!(lines_of(src, "call.on-null"), Vec::<u32>::new());
}

#[test]
fn a_nested_try_joins_inside_out() {
    // PHP: 1, 2 and 3 for the three paths.
    let src = "<?php
final class E extends Exception {}
function r(): void {
    try {
        try { $x = 1; f(); } catch (E $e) { $x = 2; }
        f();
    } catch (E $e) { $x = 3; }
    \\PHPStan\\dumpType($x);
}
function f(): void {}
";
    assert_eq!(dumps(src), vec!["8: 1|2|3"]);
}

#[test]
fn an_object_mutated_in_the_catch_joins_its_property() {
    // PHP: 1 when nothing throws, 2 when the catch runs.
    let src = "<?php
final class E extends Exception {}
final class O { public int $p = 0; }
function r(): void {
    $o = new O();
    $o->p = 1;
    try { f(); } catch (E $e) { $o->p = 2; }
    \\PHPStan\\dumpType($o->p);
}
function f(): void {}
";
    assert_eq!(dumps(src), vec!["8: 1|2"]);
}

#[test]
fn at_file_scope_a_userland_call_in_the_block_still_forgets_the_globals() {
    // Issue #762's rule: `u()` may rebind any global, so the block's end knows
    // nothing of `$y`; without the call the join is precise.
    let src = "<?php
final class E extends Exception {}
function u(): void {}
try { $y = 2; u(); } catch (E $e) { $y = 3; }
\\PHPStan\\dumpType($y);
try { $z = 2; } catch (E $e) { $z = 3; }
\\PHPStan\\dumpType($z);
";
    assert_eq!(dumps(src), vec!["5: unknown", "7: 2|3"]);
}

#[test]
fn a_try_holding_a_label_keeps_the_conservative_successor() {
    // ADR-0103: control may reach the label from a `goto` with anything bound.
    let src = "<?php
final class E extends Exception {}
function r(): void { try { $x = 1; l: f(); } catch (E $e) { $x = 2; } \\PHPStan\\dumpType($x); }
function f(): void {}
";
    assert_eq!(dumps(src), vec!["3: unknown"]);
}

#[test]
fn calls_in_a_catch_and_a_finally_are_reported() {
    // t7 (PHP: the finally always runs; the catch runs when `f()` throws).
    let src = "<?php
function t7(): void {
    try { $x = f(); }
    catch (Exception $e) { nope_in_catch(); }
    finally { nope_in_finally(); }
}
function f(): int { return 1; }
";
    assert_eq!(lines_of(src, "call.undefined-function"), vec![4, 5]);
}

#[test]
fn a_returning_body_under_a_finally_or_a_dead_catch_needs_no_tail() {
    // t8 and t9 return 1 on PHP; the `: int` functions need no trailing `return`.
    let src = "<?php
function t8(): int { try { return 1; } finally { echo 1; } }
function t9(): int { try { return 1; } catch (Exception $e) { echo 1; } }
";
    assert_eq!(findings(src), Vec::<String>::new());
}

#[test]
fn the_statement_after_a_terminating_try_is_dead() {
    // t10: `return` then `echo 1`, and the `$x->bar()` line never runs. Its twin
    // without the `return` reaches the line (PHP: Error).
    let src = "<?php
function t10(): void { $x = null; try { return; } finally { echo 1; } $x->bar(); }
function twin(): void { $x = null; try { echo 2; } finally { echo 1; } $x = null; $x->bar(); }
";
    assert_eq!(lines_of(src, "call.on-null"), vec![3]);
}

// ---- #905: terminality inside a do-while -------------------------------------

#[test]
fn a_do_while_around_a_terminating_try_returns() {
    // w905: f, h, i and j return 1 on PHP; g has no return at all and fatals.
    let src = "<?php
function f(int $c): int {
    do { try { return 1; } finally { echo 1; } } while ($c);
}
function g(int $c): int {
    do { switch ($c) { case 1: echo 1; break; } } while ($c);
}
function h(int $c): int {
    do { try { return 1; } catch (Exception $e) { echo 1; } } while ($c);
}
function i(int $c): int {
    try { return 1; } finally { echo 1; }
}
function j(int $c): int {
    try { return 1; } catch (Exception $e) { echo 1; }
}
";
    assert_eq!(findings(src), vec!["5 type.return-missing"]);
}

#[test]
fn an_assignment_in_the_block_keeps_the_catches_live() {
    // Witnessed on 8.5.11 (#943's review): a plain assignment throws from inside
    // the block in two ways, so a block of one plus `return 1;` does not make the
    // catch dead. Overwriting an object whose `__destruct` throws, and writing a
    // string through a reference to an `int` property: each prints "caught", then
    // reaches the null receiver.
    let src = "<?php
class Conn { public function __destruct() { throw new RuntimeException('d'); } }
function a(): int {
    $conn = new Conn();
    try { $conn = null; return 1; } catch (RuntimeException $e) { echo 'caught'; }
    $x = null;
    return $x->count();
}
function b(&$ref): int {
    $x = null;
    try { $ref = 'many'; return 1; } catch (TypeError $e) { echo 'caught'; }
    return $x->count();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![7, 12]);
}

// ---- finally ----------------------------------------------------------------

#[test]
fn a_returning_finally_overrides_a_throwing_body() {
    // PHP: `c()` returns 9 although the body throws; the line after never runs.
    let src = "<?php
function c(): int { try { throw new LogicException('x'); } finally { return 9; } }
function d(): void { $x = null; try { throw new LogicException('x'); } finally { return; } $x->bar(); }
";
    assert_eq!(findings(src), Vec::<String>::new());
}

#[test]
fn a_falling_finally_does_not_rescue_a_returning_body_and_a_falling_try_is_a_missing_return() {
    // PHP: `d()` returns 1 (the finally runs, the body's return proceeds); `m()`
    // runs off its end and fatals with "none returned".
    let src = "<?php
function d(): int { $r = 0; try { return 1; } finally { $r = 2; } }
function m(): int { try { $x = 1; } finally { $y = 2; } }
";
    assert_eq!(findings(src), vec!["3 type.return-missing"]);
}

#[test]
fn a_catch_that_falls_through_keeps_the_successor_live() {
    // PHP: "reached", then the Error on the null receiver.
    let src = "<?php
function g(): void {
    try { throw new LogicException('x'); } catch (LogicException $e) { }
    $x = null;
    $x->bar();
}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![5]);
}

// ---- nesting and loops ------------------------------------------------------

#[test]
fn a_nested_try_is_walked_and_terminates_through_both_finallys() {
    let src = "<?php
function n(): void {
    $x = null;
    try {
        try { $y = 'a'; \\PHPStan\\dumpType($y); return; } finally { echo 1; }
    } finally { echo 2; }
    $x->bar();
}
function twin(): void {
    $x = null;
    try {
        try { $y = 'a'; } catch (LogicException $e) { return; }
    } finally { echo 2; }
    $x->bar();
}
";
    assert_eq!(dumps(src), vec!["5: 'a'"]);
    assert_eq!(lines_of(src, "call.on-null"), vec![14]);
}

#[test]
fn a_try_in_a_loop_with_jumps_in_the_body_and_the_catch() {
    // PHP (`h()` in the probe): a `continue` in the catch skips the tail for that
    // iteration only, so the tail stays reachable through the body; a `break` in
    // the body under a falling finally ends that path.
    let src = "<?php
function loop_a(array $xs): void {
    foreach ($xs as $v) {
        $y = null;
        try { f($v); } catch (LogicException $e) { continue; } finally { echo 1; }
        $y->bar();
    }
}
function loop_b(array $xs): void {
    foreach ($xs as $v) {
        $y = null;
        try { break; } finally { echo 1; }
        $y->bar();
    }
}
function loop_c(array $xs): void {
    foreach ($xs as $v) {
        $y = null;
        try { f($v); continue; } catch (LogicException $e) { break; } finally { foreach ($xs as $w) { if ($w) { break; } } }
        $y->bar();
    }
}
function f(mixed $v): void {}
";
    assert_eq!(lines_of(src, "call.on-null"), vec![6]);
}

// ---- the caught variable ----------------------------------------------------

#[test]
fn the_caught_variable_is_a_receiver_of_the_caught_class() {
    // The method's declared return reaches the dump only through a receiver whose
    // class is known; the parameter of the same type is the control.
    let src = "<?php
final class MyEx extends Exception { public function code(): int { return 1; } }
function a(): void { try { f(); } catch (MyEx $e) { \\PHPStan\\dumpType($e->code()); } }
function control(MyEx $e): void { \\PHPStan\\dumpType($e->code()); }
function f(): void {}
";
    assert_eq!(dumps(src), vec!["3: int", "4: int"]);
}

#[test]
fn a_multi_catch_seeds_every_caught_class() {
    let src = "<?php
final class E1 extends Exception { public function code(): int { return 1; } }
final class E2 extends Exception { public function code(): string { return 'a'; } }
function a(): void { try { f(); } catch (E1 | E2 $e) { \\PHPStan\\dumpType($e); } }
function control(E1|E2 $e): void { \\PHPStan\\dumpType($e); }
function f(): void {}
";
    let got = dumps(src);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].split_once(": ").unwrap().1, got[1].split_once(": ").unwrap().1);
    assert_ne!(got[0].split_once(": ").unwrap().1, "unknown");
}

#[test]
fn a_catch_of_an_interface_binds_only_a_throwable_one() {
    // PHP binds the thrown object, which is a `Throwable` whatever the caught
    // interface says. An interface that extends `Throwable` names the receiver; one
    // that does not (`Marker`) names only part of it, and seeds nothing — a lane of
    // `Marker` alone would call `$e->getMessage()` missing.
    let src = "<?php
interface Coded extends Throwable { public function code(): int; }
interface Marker { public function code(): int; }
final class E1 extends Exception implements Coded, Marker { public function code(): int { return 1; } }
function a(): void { try { f(); } catch (Coded $e) { \\PHPStan\\dumpType($e->code()); } }
function b(): void { try { f(); } catch (Marker $e) { \\PHPStan\\dumpType($e->code()); } }
function f(): void { throw new E1('x'); }
";
    assert_eq!(dumps(src), vec!["5: int", "6: unknown"]);
}

#[test]
fn a_catch_without_a_variable_binds_nothing_and_the_old_binding_is_forgotten() {
    // `catch (E1)` binds no variable (PHP 8.0+); a `$e` from before the `try`
    // keeps its value, and a `$e` the body rebinds is forgotten.
    let src = "<?php
function a(): void {
    $e = 1;
    try { f(); } catch (LogicException) { \\PHPStan\\dumpType($e); }
    try { $e = 2; f(); } catch (LogicException $e) { }
}
function b(): void {
    $k = 1;
    try { $k = 'a'; f(); } catch (LogicException) { \\PHPStan\\dumpType($k); }
}
function f(): void {}
";
    assert_eq!(dumps(src), vec!["4: 1", "9: unknown"]);
}
