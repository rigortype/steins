//! The review witnesses of #1012: each is a PHP program on which a remembered result that outlived
//! a change to its result made the analyzer drop a branch PHP takes (measured on PHP 8.5.11, the
//! second guard's `else` is reached in every one of them).
//!
//! The property is the same for all of them: every `dumpType` of the fixture reports, so no branch
//! is marked dead on a remembered fact. The files are the reviewer's, verbatim, named by the
//! reviewer's own labels (`w01` to `w74`, and `s01` to `s32` of the sweep). `w35` is left out because
//! PHP itself takes its `then`, and `w57` because a `try` is an opaque construct whose body is
//! never walked, on any build.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// The `dumpType` messages of a source.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> =
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    ds.into_iter().map(|d| d.message).collect()
}

const WITNESSES: &[(&str, &str)] = &[
    ("w01_preg_locale", r####"<?php
declare(strict_types=1);
function w01(string $s): void {
    setlocale(LC_CTYPE, 'de_DE.ISO8859-1');
    if (preg_match('/^\w+$/', $s) !== 1) { echo "  early\n"; return; }
    setlocale(LC_CTYPE, 'C');
    if (preg_match('/^\w+$/', $s) === 1) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w02_ini_callee", r####"<?php
declare(strict_types=1);
function setp(): void { ini_set('precision', '3'); }
function w02(float $x): void {
    if (strval($x) !== '0.33333333333333') { echo "  early\n"; return; }
    setp();
    if (strval($x) === '0.33333333333333') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w03_ini_dyn", r####"<?php
declare(strict_types=1);
function w03(float $x, string $fn): void {
    if (strval($x) !== '0.33333333333333') { echo "  early\n"; return; }
    $fn('precision', '3');
    if (strval($x) === '0.33333333333333') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w04_func_get_arg", r####"<?php
declare(strict_types=1);
function w04(int $a): void {
    if (func_get_arg(0) !== 1) { echo "  early\n"; return; }
    $a = 2;
    if (func_get_arg(0) === 1) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w05_error_get_last", r####"<?php
declare(strict_types=1);
function w05(string $h): void {
    error_clear_last();
    if (error_get_last() !== null) { echo "  early\n"; return; }
    $r = @hex2bin($h);
    if (error_get_last() === null) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w06_json_last_error", r####"<?php
declare(strict_types=1);
function w06(string $j): void {
    if (json_last_error() !== 0) { echo "  early\n"; return; }
    $r = json_decode($j);
    if (json_last_error() === 0) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w07_defined", r####"<?php
declare(strict_types=1);
function w07(string $n): void {
    if (defined($n)) { echo "  early\n"; return; }
    define($n, 1);
    if (defined($n) === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w08_function_exists", r####"<?php
declare(strict_types=1);
function w08(string $n): void {
    if (function_exists($n)) { echo "  early\n"; return; }
    eval('function ' . $n . '() {}');
    if (function_exists($n) === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w09_ob_level", r####"<?php
declare(strict_types=1);
function w09(): void {
    if (ob_get_level() !== 0) { echo "  early\n"; return; }
    ob_start();
    $l = ob_get_level();
    ob_end_flush();
    if ($l === 0) {}
    ob_start();
    if (ob_get_level() === 0) { ob_end_flush(); \PHPStan\dumpType('then'); } else { ob_end_flush(); \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w10_include", r####"<?php
declare(strict_types=1);
function w10(string $s, string $f): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    include $f;
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w11_ref_alias", r####"<?php
declare(strict_types=1);
function w11(string $s): void {
    $r = &$s;
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    $r .= 'x';
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w12_ref_alias_after", r####"<?php
declare(strict_types=1);
function w12(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    $r = &$s;
    $r .= 'x';
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w13_closure_byref", r####"<?php
declare(strict_types=1);
function w13(string $s): void {
    $f = function () use (&$s): void { $s .= 'x'; };
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    $f();
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w14_closure_byref_opaque", r####"<?php
declare(strict_types=1);
function w14(string $s): void {
    $f = function () use (&$s): void { $s .= 'x'; };
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    array_map($f, [1]);
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w15_static_recursion", r####"<?php
declare(strict_types=1);
function w15(bool $inner): void {
    static $s = 'abcde';
    if ($inner) { $s .= 'x'; return; }
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    w15(true);
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w16_global", r####"<?php
declare(strict_types=1);
function w16set(): void { $GLOBALS['gs'] .= 'x'; }
function w16(): void {
    global $gs;
    $s = 'q';
    if (strlen($gs) !== 5) { echo "  early\n"; return; }
    w16set();
    if (strlen($gs) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w17_foreach_ref", r####"<?php
declare(strict_types=1);
/** @param list<string> $a */
function w17(array $a): void {
    foreach ($a as &$v) { }
    if (implode(',', $a) !== 'a,b') { echo "  early\n"; return; }
    $v = 'z';
    if (implode(',', $a) === 'a,b') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w18_elem_ref", r####"<?php
declare(strict_types=1);
function w18(string $x): void {
    $a = ['p', 'q'];
    $a[0] = &$x;
    if (implode(',', $a) !== 'a,q') { echo "  early\n"; return; }
    $x = 'z';
    if (implode(',', $a) === 'a,q') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w19_elem_ref_list", r####"<?php
declare(strict_types=1);
function w19(string $x): void {
    $a = [&$x, 'q'];
    if (implode(',', $a) !== 'a,q') { echo "  early\n"; return; }
    $x = 'z';
    if (implode(',', $a) === 'a,q') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w20_list", r####"<?php
declare(strict_types=1);
function w20(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    [$s] = ['abcdefg'];
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w21_varvar", r####"<?php
declare(strict_types=1);
function w21(string $s, string $n): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    $$n = 'abcdefg';
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w22_settype", r####"<?php
declare(strict_types=1);
function w22(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    settype($s, 'array');
    $s = 'abcdefgh';
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w23_sort", r####"<?php
declare(strict_types=1);
/** @param list<int> $a */
function w23(array $a): void {
    if (implode(',', $a) !== '2,1') { echo "  early\n"; return; }
    sort($a);
    if (implode(',', $a) === '2,1') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w24_preg_m", r####"<?php
declare(strict_types=1);
function w24(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    preg_match('/.+/', 'abcdefgh', $s);
    $s = $s[0];
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w25_extract_after", r####"<?php
declare(strict_types=1);
/** @param array<string, string> $m */
function w25(string $s, array $m): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    extract($m);
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w26_inner_func_closure_static", r####"<?php
declare(strict_types=1);
function w26(string $s): void {
    $g = static function () use (&$s) { $s = 'abcdefghij'; };
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    $h = $g;
    $h();
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w30_is_resource_fclose", r####"<?php
declare(strict_types=1);
function w30($fh): void {
    if (is_resource($fh) !== true) { echo "  early\n"; return; }
    fclose($fh);
    if (is_resource($fh) === true) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w31_gettype_fclose", r####"<?php
declare(strict_types=1);
function w31($fh): void {
    if (gettype($fh) !== 'resource') { echo "  early\n"; return; }
    fclose($fh);
    if (gettype($fh) === 'resource') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w32_is_resource_local", r####"<?php
declare(strict_types=1);
function w32(string $p): void {
    $fh = fopen($p, 'r');
    if (is_resource($fh) !== true) { echo "  early\n"; return; }
    fclose($fh);
    if (is_resource($fh) === true) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w33_json_obj", r####"<?php
declare(strict_types=1);
function w33(): void {
    $o = new \stdClass();
    $a = [$o];
    if (json_encode($a) !== '[{}]') { echo "  early\n"; return; }
    $o->x = 1;
    if (json_encode($a) === '[{}]') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w34_in_array_obj", r####"<?php
declare(strict_types=1);
/** @param list<array<int, object>> $hay @param list<object> $needle */
function w34(array $hay, array $needle, object $o): void {
    if (in_array($needle, $hay) !== false) { echo "  early\n"; return; }
    $o->x = 1;
    if (in_array($needle, $hay) === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w36_errhandler", r####"<?php
declare(strict_types=1);
function w36(float $x, string $h): void {
    if (sprintf('%.2f', $x) !== '2.50') { echo "  early\n"; return; }
    $r = hex2bin($h);
    if (sprintf('%.2f', $x) === '2.50') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w37_obcb", r####"<?php
declare(strict_types=1);
function w37(float $x): void {
    if (sprintf('%.2f', $x) !== '2.50') { echo "  early\n"; return; }
    echo "zz";
    if (sprintf('%.2f', $x) === '2.50') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w38_dtor", r####"<?php
declare(strict_types=1);
final class W38D { public function __destruct() { setlocale(LC_NUMERIC, 'de_DE.UTF-8'); } }
function w38(float $x): void {
    $d = new W38D();
    if (sprintf('%.2f', $x) !== '2.50') { echo "  early\n"; return; }
    $d = null;
    if (sprintf('%.2f', $x) === '2.50') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w39_dtor_unset", r####"<?php
declare(strict_types=1);
final class W39D { public function __destruct() { setlocale(LC_NUMERIC, 'de_DE.UTF-8'); } }
function w39(float $x): void {
    $d = new W39D();
    if (sprintf('%.2f', $x) !== '2.50') { echo "  early\n"; return; }
    unset($d);
    if (sprintf('%.2f', $x) === '2.50') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w40_ticks", r####"<?php
declare(ticks=1);
function w40(float $x): void {
    if (sprintf('%.2f', $x) !== '2.50') { echo "  early\n"; return; }
    $k = 1;
    if (sprintf('%.2f', $x) === '2.50') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w41_errhandler_pure_preg", r####"<?php
declare(strict_types=1);
function w41(string $s, string $h): void {
    if (preg_match('/^\w+$/', $s) !== 1) { echo "  early\n"; return; }
    $r = hex2bin($h);
    if (preg_match('/^\w+$/', $s) === 1) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w54_strtolower81", r####"<?php
declare(strict_types=1);
function w54(string $s): void {
    setlocale(LC_CTYPE, 'de_DE.ISO8859-1');
    if (strtolower($s) !== "\xe4") { echo "  early\n"; return; }
    setlocale(LC_CTYPE, 'C');
    if (strtolower($s) === "\xe4") { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w55_fiber", r####"<?php
declare(strict_types=1);
function w55(float $x): void {
    if (sprintf('%.2f', $x) !== '2.50') { echo "  early\n"; return; }
    \Fiber::suspend(1);
    if (sprintf('%.2f', $x) === '2.50') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w56_loop_backedge", r####"<?php
declare(strict_types=1);
function w56(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    for ($i = 0; $i < 2; $i++) {
        if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
        $s .= 'x';
    }
}
"####),
    ("w58_or_assign", r####"<?php
declare(strict_types=1);
function w58(string $s, string $t): void {
    if (strlen($s) !== 5 || ($s = $t) === '') { echo "  early\n"; return; }
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w59_inline_assign_cond", r####"<?php
declare(strict_types=1);
function w59(string $s, string $t): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    if (($s = $t) !== '' && strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w63_assert_assign", r####"<?php
declare(strict_types=1);
function w63(string $s, string $t): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    $s = $t;
    assert(strlen($s) > 0);
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w64_compound_in_args", r####"<?php
declare(strict_types=1);
function w64(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    echo $s .= 'xy';
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w65_while_cond_assign", r####"<?php
declare(strict_types=1);
function w65(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    $k = 0;
    while (($s .= 'x') !== '' && $k++ < 1) { }
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w66_ns_const", r####"<?php
declare(strict_types=1);
namespace W66;
const MARK = 'unused';
function w66(): void {
    if (strlen(FOO) !== 3) { echo "  early\n"; return; }
    \define('W66\FOO', 'abcdefg');
    if (strlen(FOO) === 3) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w67_line", r####"<?php
declare(strict_types=1);
function w67(): void {
    if (intval(__LINE__) !== 4) { echo "  early\n"; return; }
    $k = 1;
    if (intval(__LINE__) === 4) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w68_catch_var", r####"<?php
declare(strict_types=1);
function w68thrower(): void { throw new \RuntimeException('x'); }
function w68(string $s): void {
    if (!is_string($s)) { echo "  early\n"; return; }
    try { w68thrower(); } catch (\RuntimeException $s) { }
    if (is_string($s)) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w69_eval", r####"<?php
declare(strict_types=1);
function w69(string $s, string $code): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    eval($code);
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w70_json_obj_identity", r####"<?php
declare(strict_types=1);
function w70(string $j): void {
    if (json_decode($j) === null) { echo "  early\n"; return; }
    $a = json_decode($j);
    $b = json_decode($j);
    \PHPStan\dumpType($a === $b);
    if ($a === $b) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w71_is_string_param_reassign_via_func_args", r####"<?php
declare(strict_types=1);
function w71(string $s): void {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    foreach (['abcdefg'] as $s) { }
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w72_byref_gen", r####"<?php
declare(strict_types=1);
function &w72(string $s): \Generator {
    if (strlen($s) !== 5) { echo "  early\n"; return; }
    yield $s;
    if (strlen($s) === 5) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("w73_gen_preg", r####"<?php
declare(strict_types=1);
function w73(string $s): \Generator {
    if (preg_match('/^\w+$/', $s) !== 1) { echo "  early\n"; return; }
    yield 1;
    if (preg_match('/^\w+$/', $s) === 1) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s01", r####"<?php
declare(strict_types=1);
function s01(): void {
    if (date_default_timezone_get() !== 'UTC') { echo "  early\n"; return; }
    date_default_timezone_set('Asia/Tokyo');
    if (date_default_timezone_get() === 'UTC') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s02", r####"<?php
declare(strict_types=1);
function s02(): void {
    if (mb_internal_encoding() !== 'UTF-8') { echo "  early\n"; return; }
    mb_internal_encoding('ISO-8859-1');
    if (mb_internal_encoding() === 'UTF-8') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s03", r####"<?php
declare(strict_types=1);
function s03(): void {
    if (bcscale() !== 0) { echo "  early\n"; return; }
    bcscale(5);
    if (bcscale() === 0) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s04", r####"<?php
declare(strict_types=1);
function s04(): void {
    if (error_reporting() !== E_ALL) { echo "  early\n"; return; }
    error_reporting(0);
    if (error_reporting() === E_ALL) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s05", r####"<?php
declare(strict_types=1);
function s05(): void {
    if (ini_get('precision') !== '14') { echo "  early\n"; return; }
    call_user_func('ini_set', 'precision', '3');
    if (ini_get('precision') === '14') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s06", r####"<?php
declare(strict_types=1);
function s06(): void {
    if (get_include_path() !== '.') { echo "  early\n"; return; }
    set_include_path('/tmp');
    if (get_include_path() === '.') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s07", r####"<?php
declare(strict_types=1);
function s07(): void {
    if (getcwd() !== '/') { echo "  early\n"; return; }
    chdir('/tmp');
    if (getcwd() === '/') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s08", r####"<?php
declare(strict_types=1);
function s08(): void {
    if (umask() !== 18) { echo "  early\n"; return; }
    umask(0);
    if (umask() === 18) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s09", r####"<?php
declare(strict_types=1);
function s09(): void {
    if (gc_enabled() !== true) { echo "  early\n"; return; }
    gc_disable();
    if (gc_enabled() === true) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s10", r####"<?php
declare(strict_types=1);
function s10(): void {
    if (setlocale(LC_ALL, '0') !== 'C') { echo "  early\n"; return; }
    call_user_func('setlocale', LC_ALL, 'de_DE.UTF-8');
    if (setlocale(LC_ALL, '0') === 'C') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s11", r####"<?php
declare(strict_types=1);
function s11(): void {
    if (localeconv()['decimal_point'] !== '.') { echo "  early\n"; return; }
    setlocale(LC_ALL, 'de_DE.UTF-8');
    if (localeconv()['decimal_point'] === '.') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s12", r####"<?php
declare(strict_types=1);
function s12(): void {
    if (sprintf('%.1f', 1.5) !== '1.5') { echo "  early\n"; return; }
    call_user_func('setlocale', LC_ALL, 'de_DE.UTF-8');
    if (sprintf('%.1f', 1.5) === '1.5') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s13", r####"<?php
declare(strict_types=1);
function s13(): void {
    if (ob_get_level() !== 0) { echo "  early\n"; return; }
    ob_start();
    if (ob_get_level() === 0) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s14", r####"<?php
declare(strict_types=1);
function s14(): void {
    if (error_get_last() !== null) { echo "  early\n"; return; }
    @hex2bin('a');
    if (error_get_last() === null) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s15", r####"<?php
declare(strict_types=1);
function s15(): void {
    if (libxml_use_internal_errors() !== false) { echo "  early\n"; return; }
    libxml_use_internal_errors(true);
    if (libxml_use_internal_errors() === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s16", r####"<?php
declare(strict_types=1);
function s16(): void {
    if (mt_getrandmax() !== 2147483647) { echo "  early\n"; return; }
    mt_srand(1);
    if (mt_getrandmax() === 2147483647) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s17", r####"<?php
declare(strict_types=1);
function s17(): void {
    if (json_encode(0.1 + 0.2) !== '0.30000000000000004') { echo "  early\n"; return; }
    call_user_func('ini_set', 'serialize_precision', '5');
    if (json_encode(0.1 + 0.2) === '0.30000000000000004') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s18", r####"<?php
declare(strict_types=1);
function s18(): void {
    if (headers_sent() !== false) { echo "  early\n"; return; }
    print('x');
    if (headers_sent() === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s19", r####"<?php
declare(strict_types=1);
function s19(): void {
    if (ignore_user_abort() !== 0) { echo "  early\n"; return; }
    ignore_user_abort(true);
    if (ignore_user_abort() === 0) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s20", r####"<?php
declare(strict_types=1);
function s20(): void {
    if (set_time_limit(0) !== true) { echo "  early\n"; return; }
    set_time_limit(5);
    if (set_time_limit(0) === true) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s21", r####"<?php
declare(strict_types=1);
function s21(): void {
    if (array_sum([0.1, 0.2]) !== 0.30000000000000004) { echo "  early\n"; return; }
    ini_set('precision', '3');
    if (array_sum([0.1, 0.2]) === 0.30000000000000004) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s22", r####"<?php
declare(strict_types=1);
function s22(): void {
    if (php_ini_loaded_file() !== false) { echo "  early\n"; return; }
    ini_set('x','y');
    if (php_ini_loaded_file() === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s23", r####"<?php
declare(strict_types=1);
function s23(): void {
    if (spl_autoload_functions() !== []) { echo "  early\n"; return; }
    spl_autoload_register(fn() => null);
    if (spl_autoload_functions() === []) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s24", r####"<?php
declare(strict_types=1);
function s24(): void {
    if (get_declared_classes() === [] !== true) { echo "  early\n"; return; }
    eval('class S24 {}');
    if (get_declared_classes() === [] === true) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s25", r####"<?php
declare(strict_types=1);
function s25(): void {
    if (class_exists('S25', false) !== false) { echo "  early\n"; return; }
    eval('class S25 {}');
    if (class_exists('S25', false) === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s26", r####"<?php
declare(strict_types=1);
function s26(): void {
    if (function_exists('s26f') !== false) { echo "  early\n"; return; }
    eval('function s26f() {}');
    if (function_exists('s26f') === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s27", r####"<?php
declare(strict_types=1);
function s27(): void {
    if (defined('S27') !== false) { echo "  early\n"; return; }
    define('S27', 1);
    if (defined('S27') === false) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s28", r####"<?php
declare(strict_types=1);
function s28(): void {
    if (memory_get_usage() !== 0) { echo "  early\n"; return; }
    $x = str_repeat('a', 100000);
    if (memory_get_usage() === 0) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s29", r####"<?php
declare(strict_types=1);
function s29(): void {
    if (assert_options(ASSERT_ACTIVE) !== 1) { echo "  early\n"; return; }
    assert_options(ASSERT_ACTIVE, 0);
    if (assert_options(ASSERT_ACTIVE) === 1) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s30", r####"<?php
declare(strict_types=1);
function s30(): void {
    if (preg_match('/^\\w$/', "\xe4") !== 0) { echo "  early\n"; return; }
    setlocale(LC_CTYPE, 'de_DE.ISO8859-1');
    if (preg_match('/^\\w$/', "\xe4") === 0) { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s31", r####"<?php
declare(strict_types=1);
function s31(): void {
    if (strval(1/3) !== '0.33333333333333') { echo "  early\n"; return; }
    call_user_func('ini_set', 'precision', '3');
    if (strval(1/3) === '0.33333333333333') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
    ("s32", r####"<?php
declare(strict_types=1);
function s32(): void {
    if (ini_get('precision') !== '14') { echo "  early\n"; return; }
    $f = 'ini_set'; $f('precision', '3');
    if (ini_get('precision') === '14') { \PHPStan\dumpType('then'); } else { \PHPStan\dumpType('else-reached'); }
}
"####),
];

#[test]
fn no_witness_has_a_branch_dropped_on_a_remembered_fact() {
    let mut failures = Vec::new();
    for (name, src) in WITNESSES {
        let calls = src.matches("dumpType(").count();
        let got = dumps(src);
        if got.len() != calls {
            failures.push(format!("{name}: {calls} dumps in the source, {got:?} reported"));
        }
    }
    assert!(failures.is_empty(), "a branch PHP takes was dropped:\n{}", failures.join("\n"));
}
