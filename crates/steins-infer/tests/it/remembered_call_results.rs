//! Remembered call results (ADR-0102, slice 1): the witness table, row by row.
//!
//! Every row is a PHP witness measured on 8.5.11 (`witnesses/remembered-results.php`
//! of the ADR-0102 design): "same" rows are where the first call's result may stand
//! for the second across the span, "DIFFERS" rows are the must-stay ones. Each is a
//! pair of fixtures, checked both ways: the remembered form where the witness says
//! "same", the call's own answer where it says "DIFFERS".
//!
//! A fixture is a guard that leaves the first call's result known (an early return,
//! so the fall-through is the surviving branch), a span, and then the second call
//! bound to `$b` and dumped with `dumpType($b)` — not `assertType($x, …)`, which
//! drops its subject to the writes set. The fixtures are function bodies: the
//! top-level script is the frame whose locals are the globals, and the effect lane
//! records no sites there, so nothing is remembered in it
//! ([`the_top_level_frame_remembers_nothing`]).
//!
//! The checks run with no PHP, so a call's own answer is the catalog's declared
//! floor, `(asserted)`; what a `=== literal` guard pins is `Verified`, which is the
//! only grade a decided branch reads.
//!
//! Only the builtins on the allowlist of `remembered.rs` are remembered: a setting
//! read (R1, R7), a `{}` row that reads unlabelled ambient state (R8, `strval`,
//! `preg_match`, `strtolower`) and every other builtin are not, whatever the catalog
//! row says. Those rows are pinned as "stays the call's own answer", and the review
//! witnesses of #1012 are in `remembered_witnesses`.
//!
//! A source without `declare(strict_types=1)` and without a `// coercive` line gets
//! the declaration: a builtin with `string` parameters is remembered only in a
//! strict file.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// The `dumpType` messages of a source, in source order, with the leading label cut.
fn dumps(src: &str) -> Vec<String> {
    let strict;
    let src = if src.contains("strict_types") || src.contains("// coercive") {
        src
    } else {
        strict = src.replacen("<?php\n", "<?php\ndeclare(strict_types=1);\n", 1);
        &strict
    };
    let tree = SourceTree::parse(src);
    let mut ds: Vec<Diagnostic> =
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    ds.sort_by_key(|d| (d.line, d.column));
    ds.into_iter()
        .map(|d| d.message.strip_prefix("dumped type: ").unwrap_or(&d.message).to_owned())
        .collect()
}

/// The one dump a one-dump source produces.
fn one(src: &str) -> String {
    let mut all = dumps(src);
    assert_eq!(all.len(), 1, "expected one dump in {src}, got {all:?}");
    all.remove(0)
}

/// A function `r` taking `params`, whose body is `guard`, `span` and then the second
/// call `second` bound to `$b` and dumped.
fn fixture(params: &str, guard: &str, span: &str, second: &str) -> String {
    format!(
        "<?php
declare(strict_types=1);
function byref(string &$x): void {{ $x .= '!'; }}
function opaque(callable $cb): void {{ $cb(); }}
function r({params}): void {{
    {guard}
    {span}
    $b = {second};
    \\PHPStan\\dumpType($b);
}}
"
    )
}

/// What `$b` is after `guard` held, `span` ran and `second` was called.
fn after(params: &str, guard: &str, span: &str, second: &str) -> String {
    one(&fixture(params, guard, span, second))
}

// R1: a setting-read row, no write between: not remembered in slice 1 (ADR-0102 §7)

#[test]
fn r1_a_locale_query_is_not_remembered_in_slice_1() {
    let guard = "if (setlocale(LC_ALL, '0') !== 'C') { return; }";
    let q = "setlocale(LC_ALL, '0')";
    // The call's own answer, the declared floor's `string|false`, guard or none: a setting
    // read changes when a site the effect lane does not record rewrites the setting.
    assert_eq!(after("", guard, "", q), "string|false (asserted)");
    assert_eq!(after("", "", "", q), "string|false (asserted)");
}

// R2: the same, across a write to the cell (must-stay)

#[test]
fn r2_an_intervening_setlocale_forgets_the_locale_query() {
    let guard = "if (setlocale(LC_ALL, '0') !== 'C') { return; }";
    let span = "setlocale(LC_NUMERIC, 'de_DE.UTF-8');";
    assert_eq!(after("", guard, span, "setlocale(LC_ALL, '0')"), "string|false (asserted)");
}

// R3: a {}-row callee on a place nothing writes

#[test]
fn r3_strlen_of_an_untouched_place_is_remembered() {
    let eq = "if (strlen($s) !== 5) { return; }";
    assert_eq!(after("string $s", eq, "", "strlen($s)"), "5");
    let gt = "if (strlen($s) <= 0) { return; }";
    assert_eq!(after("string $s", gt, "", "strlen($s)"), "int<1, max> (asserted)");
    // Control: unguarded.
    assert_eq!(after("string $s", "", "", "strlen($s)"), "int<0, max> (asserted)");
}

// R4: the argument place is assigned between (must-stay)

#[test]
fn r4_an_assignment_to_the_argument_place_forgets_the_key() {
    let guard = "if (strlen($s) !== 5) { return; }";
    let append = "$s .= '!';";
    assert_eq!(after("string $s", guard, append, "strlen($s)"), "int<0, max> (asserted)");
    // The place now holds another string; the old key says nothing of it.
    assert_ne!(after("string $s", guard, "$s = 'abcdef';", "strlen($s)"), "5");
    // An offset write to the place is a write to it.
    let offset = "$s[0] = 'x';";
    assert_eq!(after("string $s", guard, offset, "strlen($s)"), "int<0, max> (asserted)");
    // And so is `unset`.
    assert_eq!(after("string $s", guard, "unset($s);", "strlen($s)"), "int<0, max> (asserted)");
}

// R5: a by-reference write to the argument place (must-stay)

#[test]
fn r5_a_by_reference_write_to_the_argument_place_forgets_the_key() {
    let guard = "if (strlen($s) !== 5) { return; }";
    assert_eq!(
        after("string $s", guard, "byref($s);", "strlen($s)"),
        "int<0, max> (asserted)"
    );
    // A by-value pass keeps it: the place cannot have changed.
    assert_eq!(after("string $s", guard, "$n = strtoupper($s);", "strlen($s)"), "5");
}

// R6: an intervening user call with an unknown effect (must-stay for a setting read)

#[test]
fn r6_a_user_call_forgets_a_setting_read_and_keeps_a_pure_key() {
    let guard = "if (sprintf('%.2f', $x) !== '2.50') { return; }";
    let span = "opaque(fn() => setlocale(LC_NUMERIC, 'de_DE.UTF-8'));";
    assert_eq!(after("float $x", guard, span, "sprintf('%.2f', $x)"), "string (asserted)");
    // The `{}` row's result is a function of its places, which an unseen call cannot
    // write: it survives the same call.
    let pure = "if (strlen($s) !== 5) { return; }";
    assert_eq!(after("string $s", pure, span, "strlen($s)"), "5");
}

// R7: a locale-row printf, with and without a locale write (must-stay)

#[test]
fn r7_a_locale_printf_is_never_remembered() {
    let guard = "if (sprintf('%.2f', $x) !== '2.50') { return; }";
    let q = "sprintf('%.2f', $x)";
    assert_eq!(after("float $x", guard, "", q), "string (asserted)");
    let write = "setlocale(LC_NUMERIC, 'de_DE.UTF-8');";
    assert_eq!(after("float $x", guard, write, q), "string (asserted)");
}

// R8: a printf whose literal format drops every read is a `{}` row, and `{}` is not the
// allowlist: it is not remembered either (it reads `precision` for a `%s` of a float
// in the same family, and the catalog row cannot tell the two apart for a variable).

#[test]
fn r8_a_printf_with_no_setting_read_is_not_on_the_allowlist() {
    let guard = "if (sprintf('%05d', $n) !== '00042') { return; }";
    let write = "setlocale(LC_NUMERIC, 'de_DE.UTF-8');";
    assert_eq!(after("int $n", guard, write, "sprintf('%05d', $n)"), "string (asserted)");
}

// R9: the guard-then-use idiom

#[test]
fn r9_the_guard_then_use_idiom_narrows_the_second_call() {
    let guard = "if (strpos($h, '=') === false) { return; }";
    let q = "strpos($h, '=')";
    assert_eq!(after("string $h", guard, "", q), "int<0, max> (asserted)");
    // Control: unguarded.
    assert_eq!(after("string $h", "", "", q), "int<0, max>|false (asserted)");
    // Inside the branch, as the idiom is written.
    let inside = "<?php
function r(string $h): void {
    if (strpos($h, '=') !== false) {
        $p = strpos($h, '=');
        \\PHPStan\\dumpType($p);
        \\PHPStan\\dumpType(strpos($h, '='));
    }
}
";
    assert_eq!(dumps(inside), ["int<0, max> (asserted)", "int<0, max> (asserted)"]);
}

// R10: a nondet row is never remembered (must-stay)

#[test]
fn r10_a_nondet_row_is_never_remembered() {
    let seeded = "if (mt_rand() !== 5) { return; }";
    assert_eq!(after("", seeded, "", "mt_rand()"), "int (asserted)");
    assert_eq!(after("", "if (rand() !== 5) { return; }", "", "rand()"), "int (asserted)");
    let unguarded = after("", "", "", "time()");
    assert_eq!(after("", "if (time() !== 5) { return; }", "", "time()"), unguarded);
}

// R11: a method call has no key in slice 1 (must-stay)

#[test]
fn r11_a_method_call_is_never_remembered() {
    let guard = "if ($o->count() !== 2) { return; }";
    assert_ne!(after("ArrayObject $o", guard, "", "$o->count()"), "2");
}

// R12: a read of the environment is not a setting read (S6 owns the env cell)

#[test]
fn r12_getenv_is_not_remembered() {
    let guard = "if (getenv('X') !== '1') { return; }";
    assert_ne!(after("", guard, "", "getenv('X')"), "'1'");
}

// R13: the stat family is never remembered (ADR-0102 D1)

#[test]
fn r13_the_stat_family_is_never_remembered() {
    let missing = "if (is_dir($d) !== false) { return; }";
    assert_ne!(after("string $d", missing, "", "is_dir($d)"), "false");
    let present = "if (is_dir($d) !== true) { return; }";
    assert_ne!(after("string $d", present, "", "is_dir($d)"), "true");
    let exists = "if (file_exists($d) !== true) { return; }";
    assert_ne!(after("string $d", exists, "", "file_exists($d)"), "true");
}

// Neighbouring rows, each a PHP witness of its own

/// A guard on one key and a decided second guard on the same key: the first call's
/// result is the second's, so the second test is decided and its branch is dead.
#[test]
fn a_second_guard_on_the_same_call_is_decided_by_the_first() {
    let decided = "<?php
function r(string $s): void {
    if (strlen($s) === 5) {
        if (strlen($s) === 6) { \\PHPStan\\dumpType(1); }
    }
}
";
    assert_eq!(dumps(decided), Vec::<String>::new(), "`strlen($s) === 6` after `=== 5` is dead");
    // Control: a write between makes it live again.
    let live = "<?php
function r(string $s): void {
    if (strlen($s) === 5) {
        $s .= 'x';
        if (strlen($s) === 6) { \\PHPStan\\dumpType(1); }
    }
}
";
    assert_eq!(dumps(live), ["1"]);
}

/// The right operand of `&&` reads the left operand's call as narrowed.
#[test]
fn the_right_operand_of_a_conjunction_reads_the_left_calls_narrowing() {
    let src = "<?php
function r(string $s, string $t): void {
    if (strlen($s) === 5 && strlen($s) === 6) { \\PHPStan\\dumpType(1); }
    if (strlen($t) === 5 && strlen($s) === 6) { \\PHPStan\\dumpType(2); }
}
";
    assert_eq!(dumps(src), ["2"], "a second test of one call, against another value, is dead");
}

/// A key produced in one arm of a branch is gone at the join, unless both arms agree.
#[test]
fn a_key_survives_a_join_only_where_both_arms_hold_it() {
    let one_arm = "<?php
function r(string $s, bool $c): void {
    if ($c) { if (strlen($s) !== 5) { return; } }
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(one_arm), "int<0, max> (asserted)");
    let both = "<?php
function r(string $s, bool $c): void {
    if ($c) { if (strlen($s) !== 5) { return; } } else { if (strlen($s) !== 5) { return; } }
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(both), "5");
    let disagree = "<?php
function r(string $s, bool $c): void {
    if ($c) { if (strlen($s) !== 5) { return; } } else { if (strlen($s) !== 6) { return; } }
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(disagree), "5|6");
}

/// A loop forgets what its body writes: the next iteration enters with the place
/// rewritten.
#[test]
fn a_loop_forgets_what_its_body_writes() {
    let guard = "if (strlen($s) !== 5) { return; }";
    let written = "while ($c) { $s .= 'x'; }";
    assert_eq!(
        after("string $s, bool $c", guard, written, "strlen($s)"),
        "int<0, max> (asserted)"
    );
    let untouched = "while ($c) { $k = 1; }";
    assert_eq!(after("string $s, bool $c", guard, untouched, "strlen($s)"), "5");
}

/// A by-reference parameter is another frame's variable, and a superglobal any
/// code's: neither stands as a place in a key.
#[test]
fn a_by_reference_parameter_and_a_superglobal_are_never_places() {
    let guard = "if (strlen($s) !== 5) { return; }";
    let by_ref = "<?php
function r(string &$s): void {
    if (strlen($s) !== 5) { return; }
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(by_ref), "int<0, max> (asserted)");
    let _ = guard;
    let global = "<?php
function r(): void {
    if (count($_GET) !== 5) { return; }
    $b = count($_GET);
    \\PHPStan\\dumpType($b);
}
";
    assert_ne!(one(global), "5");
}

/// An object, a callable or an untyped value reaches user code through the call, so
/// the site is not exhaustive and nothing is remembered.
#[test]
fn a_call_that_may_reach_user_code_is_never_remembered() {
    // `count` of a value that may be a `Countable` object runs its `count()`.
    let untyped = "<?php
function r($s): void {
    if (count($s) !== 5) { return; }
    $b = count($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_ne!(one(untyped), "5");
    // The same call over an array is remembered: the control.
    let array = untyped.replace("function r($s)", "function r(array $s)");
    assert_eq!(one(&array), "5");
}

/// A project function shadowing the builtin's simple name is a different call.
#[test]
fn a_project_function_of_the_same_simple_name_is_not_the_builtin() {
    let src = "<?php
namespace A;
function strlen(string $s): int { return 99; }
function r(string $s): void {
    if (\\strlen($s) !== 5) { return; }
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_ne!(one(src), "5");
}

/// The top-level script is the frame whose locals are the globals; no site is
/// recorded there, so no call has an answer from the effect lane to be remembered by.
#[test]
fn the_top_level_frame_remembers_nothing() {
    let src = "<?php
$s = $_GET['s'];
if (strlen($s) !== 5) { return; }
$b = strlen($s);
\\PHPStan\\dumpType($b);
";
    assert_ne!(one(src), "5");
}

/// A poisoned scope binds no fact, a key included.
#[test]
fn a_poisoned_scope_remembers_nothing() {
    let src = "<?php
function r(string $s, array $m): void {
    extract($m);
    if (strlen($s) !== 5) { return; }
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_ne!(one(src), "5");
}

/// A condition that rebinds the place after the call has made the refinement stale.
#[test]
fn a_conjunct_that_rebinds_the_place_makes_the_key_stale() {
    let src = "<?php
function r(string $s): void {
    if (strlen($s) === 5 && ($s = strrev($s)) !== '') {
        $b = strlen($s);
        \\PHPStan\\dumpType($b);
    }
}
";
    assert_ne!(one(src), "5");
}

/// `assert()` is a guard that holds for the rest of the body.
#[test]
fn an_assertion_is_a_guard_that_remembers() {
    let src = "<?php
function r(string $s): void {
    assert(strlen($s) === 5);
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(src), "5");
}

/// The truthiness of a call: the falsy arms of its row are gone.
#[test]
fn a_truthiness_guard_on_a_call_remembers_the_non_falsy_arms() {
    let src = "<?php
function r(string $h): void {
    if (!strpos($h, '=')) { return; }
    $b = strpos($h, '=');
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(src), "int<0, max> (asserted)");
}

/// The value operand and the dump argument read the same ladder the binding does.
#[test]
fn every_seam_of_the_ladder_reads_the_remembered_call() {
    let src = "<?php
function r(string $s): void {
    if (strlen($s) !== 5) { return; }
    \\PHPStan\\dumpType(strlen($s));
    \\PHPStan\\dumpType((string) strlen($s));
}
";
    assert_eq!(dumps(src), ["5", "'5'"]);
}

/// A `{}` row that reads unlabelled ambient state is not on the allowlist, so a guard on it
/// remembers nothing and no write of that state can make a remembered result stale: `strval`
/// renders a float through `precision`, `preg_match` consults the locale, `strtolower`
/// did before 8.2, `json_encode` reads `serialize_precision` (witnesses N4, N5).
#[test]
fn a_row_that_reads_unlabelled_state_is_never_remembered() {
    for (params, guard, second) in [
        ("float $f", "if (strval($f) !== '0.3') { return; }", "strval($f)"),
        ("string $s", "if (preg_match('/^\\w+$/', $s) !== 1) { return; }", "preg_match('/^\\w+$/', $s)"),
        ("string $s", "if (strtolower($s) !== 'a') { return; }", "strtolower($s)"),
        ("float $f", "if (json_encode($f) !== '0.3') { return; }", "json_encode($f)"),
        ("string $s", "if (trim($s) !== 'a') { return; }", "trim($s)"),
    ] {
        let got = after(params, guard, "", second);
        assert!(got != "'0.3'" && got != "'a'" && got != "1", "{second} was remembered: {got}");
    }
}

/// An ini write changes none of an allowed builtin's results (the allowlist's whole
/// premise), so a key survives it: a control for the row above.
#[test]
fn an_ini_write_leaves_an_allowed_key_alone() {
    let guard = "if (strlen($s) !== 5) { return; }";
    assert_eq!(after("string $s", guard, "ini_set('precision', '3');", "strlen($s)"), "5");
    assert_eq!(after("string $s", guard, "setlocale(LC_ALL, 'C');", "strlen($s)"), "5");
    assert_eq!(after("string $s", guard, "set_error_handler(fn() => true);", "strlen($s)"), "5");
}

/// A builtin with `string` parameters converts a float through `precision` in a coercive
/// file, so it is remembered only under `strict_types`; `count` has no such parameter.
#[test]
fn a_string_builtin_is_remembered_only_in_a_strict_file() {
    let coercive = "<?php // coercive
function r(string $s): void {
    if (strlen($s) !== 5) { return; }
    $b = strlen($s);
    \\PHPStan\\dumpType($b);
}
";
    assert_ne!(one(coercive), "5");
    let count = "<?php // coercive
function r(array $a): void {
    if (count($a) !== 5) { return; }
    $b = count($a);
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(count), "5");
}

/// The allowance covers the argument counts it was verified for: `count($a,
/// COUNT_RECURSIVE)` walks nested arrays whose reference slots another alias can rewrite.
#[test]
fn a_call_outside_the_verified_arity_is_never_remembered() {
    let src = "<?php
function r(array $a): void {
    if (count($a, 1) !== 5) { return; }
    $b = count($a, 1);
    \\PHPStan\\dumpType($b);
}
";
    assert_ne!(one(src), "5");
}

/// A constant's spelling is not its meaning inside a namespace (`FOO` is `ns\FOO` once that
/// is defined), so a call naming one has no key (witness w66).
#[test]
fn a_call_naming_a_constant_has_no_key() {
    let src = "<?php
namespace W66;
function r(string $s): void {
    if (strpos($s, \\PHP_EOL) === false) { return; }
    $b = strpos($s, \\PHP_EOL);
    \\PHPStan\\dumpType($b);
}
";
    assert_eq!(one(src), "int<0, max>|false (asserted)");
}

/// The proven findings of the reviewer's witnesses (w50 to w53): a key that outlived an `ini_set`
/// or a `setlocale` made the second guard decided, so a true finding was lost (w50) or a false
/// `type.invalid-operand` was reported (w52, w53). None of the three rows is on the allowlist.
fn ids(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php").into_iter().map(|d| d.id.to_string()).collect()
}

#[test]
fn the_review_findings_are_neither_lost_nor_invented() {
    let w52 = "<?php
declare(strict_types=1);
function setp52(): void { ini_set('precision', '3'); }
function w52a(float $x): int {
    if (strval($x) !== '0.33333333333333') { return 0; }
    setp52();
    $n = 'abc';
    if (strval($x) !== '0.33333333333333') { $n = 5; }
    return $n * 2;
}
";
    assert!(!ids(w52).iter().any(|i| i == "type.invalid-operand"), "{:?}", ids(w52));
    let w53 = "<?php
declare(strict_types=1);
function w53(string $s): int {
    setlocale(LC_CTYPE, 'de_DE.ISO8859-1');
    if (preg_match('/^\\w+$/', $s) !== 1) { return 0; }
    setlocale(LC_CTYPE, 'C');
    $n = 'abc';
    if (preg_match('/^\\w+$/', $s) !== 1) { $n = 5; }
    return $n * 2;
}
";
    assert!(!ids(w53).iter().any(|i| i == "type.invalid-operand"), "{:?}", ids(w53));
    let w50 = "<?php
declare(strict_types=1);
function setp50(): void { ini_set('precision', '3'); }
function w50b(float $x): int {
    if (strval($x) !== '0.33333333333333') { return 0; }
    setp50();
    $a = [];
    if (strval($x) !== '0.33333333333333') { $a['k'] = 1; }
    return $a['k'];
}
";
    assert!(ids(w50).iter().any(|i| i == "offset.maybe-missing"), "{:?}", ids(w50));
}
