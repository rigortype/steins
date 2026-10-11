//! Offset, property and class-constant reads judged at every position they are written
//! (ADR-0104, issues #936 and #949): one test per row of the carrier table in
//! `stmt_checks`'s module doc, the witness rows of both issues, and the soundness traps
//! a position-wide judgement opens.
//!
//! Every "warns" or "throws" below was run on PHP 8.5 with an error handler installed;
//! every "silent" row is silent there.

use steins_infer::{
    CLASS_CONST_INACCESSIBLE_ID, CLASS_CONST_UNDEFINED_ID, Diagnostic, Folder, OFFSET_MISSING_ID,
    OFFSET_ON_UNSUPPORTED_ID, PROPERTY_INACCESSIBLE_ID, PROPERTY_ON_NON_OBJECT_ID,
    PROPERTY_UNDEFINED_ID, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

/// A live, monkey-patch-free boot surface that knows no class: every absence family
/// may fire.
struct Boot;

impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        None
    }
}

const IDS: [&str; 7] = [
    OFFSET_MISSING_ID,
    OFFSET_ON_UNSUPPORTED_ID,
    PROPERTY_UNDEFINED_ID,
    PROPERTY_INACCESSIBLE_ID,
    PROPERTY_ON_NON_OBJECT_ID,
    CLASS_CONST_UNDEFINED_ID,
    CLASS_CONST_INACCESSIBLE_ID,
];

/// The declarations every fixture shares, then `body` as the body of `f()`. Line 2
/// holds the body, so a finding's column is its offset in `body` plus the prefix.
const PRELUDE: &str = "<?php
class A { const K = 1; private const P = 2; private $priv = 1; public $pub = 1; }
enum Suit { case Hearts; }
function byval($v) { return $v; }
function byref(&$v) {}
function two($x, $y) { return 1; }
";

const OPEN: &str = "function f(A $o, $u) { $a = ['k' => 1, 'n' => ['x' => 1]]; $c = new A(); $z = null; ";

fn findings(body: &str) -> Vec<Diagnostic> {
    let src = format!("{PRELUDE}{OPEN}{body} }}\n");
    let tree = SourceTree::parse(&src);
    check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| IDS.contains(&d.id))
        .collect()
}

fn ids(body: &str) -> Vec<&'static str> {
    findings(body).into_iter().map(|d| d.id).collect()
}

/// The column of `needle` in `body` as a finding reports it (1-based).
fn column_of(body: &str, needle: &str) -> u32 {
    let at = body.find(needle).expect("needle in body");
    u32::try_from(OPEN.len() + at + 1).expect("small")
}

#[track_caller]
fn fires(body: &str, id: &str) {
    assert_eq!(ids(body), [id], "`{body}`: {:#?}", findings(body));
}

#[track_caller]
fn silent(body: &str) {
    assert!(ids(body).is_empty(), "`{body}` must be silent: {:#?}", findings(body));
}

// The carrier table, one test per cell group.

#[test]
fn plain_judges_every_family() {
    fires("echo $a['nope'];", OFFSET_MISSING_ID);
    fires("echo $z['k'];", OFFSET_ON_UNSUPPORTED_ID);
    fires("echo $c->nope;", PROPERTY_UNDEFINED_ID);
    fires("echo $c->priv;", PROPERTY_INACCESSIBLE_ID);
    fires("echo $z->p;", PROPERTY_ON_NON_OBJECT_ID);
    fires("echo A::NOPE;", CLASS_CONST_UNDEFINED_ID);
    fires("echo A::P;", CLASS_CONST_INACCESSIBLE_ID);
}

#[test]
fn a_finding_sits_at_the_read_not_the_statement() {
    // D1: `echo 'x' . $a['nope'];` points at `$a['nope']`.
    for (body, needle) in [
        ("echo 'x' . $a['nope'];", "$a['nope']"),
        ("$x = 1 + $z->p;", "$z->p"),
        ("return [A::NOPE];", "A::NOPE"),
        ("$y = $a['nope'];", "$a['nope']"),
    ] {
        let d = findings(body);
        assert_eq!(d.len(), 1, "`{body}`: {d:#?}");
        assert_eq!(d[0].column, column_of(body, needle), "`{body}`: {d:#?}");
    }
}

#[test]
fn a_by_value_function_argument_judges_every_family() {
    fires("byval($a['nope']);", OFFSET_MISSING_ID);
    fires("strlen($a['nope']);", OFFSET_MISSING_ID);
    fires("byval($z->p);", PROPERTY_ON_NON_OBJECT_ID);
    fires("byval($c->priv);", PROPERTY_INACCESSIBLE_ID);
    fires("strlen(A::NOPE);", CLASS_CONST_UNDEFINED_ID);
}

#[test]
fn a_by_reference_argument_declines_the_value_families() {
    // The key and the property are created (silent in PHP), and so are the base levels.
    silent("byref($a['nope']);");
    silent("byref($a['nope']['deeper']);");
    silent("sort($a['nope']);");
    silent("byref($z->p);");
    // A private property fetched for reference still throws, through `__get` like a read.
    fires("byref($c->priv);", PROPERTY_INACCESSIBLE_ID);
}

#[test]
fn an_argument_whose_parameter_is_unknowable_declines_the_value_families() {
    silent("unknown_fn($a['nope']);");
    silent("$o->m($a['nope']);");
    silent("new A($a['nope']);");
    silent("byval(v: $a['nope']);");
    silent("byval(...[1], $a['nope']);");
    // An env-independent family judges whatever the parameter.
    fires("$o->m(A::NOPE);", CLASS_CONST_UNDEFINED_ID);
}

#[test]
fn a_conditional_operand_declines_value_families_and_judges_the_others() {
    silent("echo $u ? $a['nope'] : 1;");
    silent("echo $u && $a['nope'];");
    silent("echo $u ?: $a['nope'];");
    silent("echo $u ? $z->p : 1;");
    silent("echo $u ? $c->nope : 1;");
    fires("echo $u ? $c->priv : 1;", PROPERTY_INACCESSIBLE_ID);
    fires("echo $u ? A::NOPE : 1;", CLASS_CONST_UNDEFINED_ID);
    fires("echo $u?->m(A::NOPE);", CLASS_CONST_UNDEFINED_ID);
}

#[test]
fn an_isset_like_operand_is_silent_except_for_a_class_constant() {
    silent("echo isset($a['nope']) ? 1 : 0;");
    silent("echo $a['nope'] ?? 1;");
    silent("echo empty($c->nope) ? 1 : 0;");
    silent("echo $c->priv ?? 1;");
    silent("$a['nope'] ??= 1;");
    fires("echo A::NOPE ?? 1;", CLASS_CONST_UNDEFINED_ID);
    fires("echo A::P ?? 1;", CLASS_CONST_INACCESSIBLE_ID);
}

#[test]
fn a_suppressed_operand_declines_warnings_and_judges_errors() {
    // D2: `@` silences a warning at the default floor; an Error is not suppressible.
    silent("echo @$a['nope'];");
    silent("echo @$c->nope;");
    silent("echo @$z->p;");
    fires("echo @$c->priv;", PROPERTY_INACCESSIBLE_ID);
    fires("echo @A::NOPE;", CLASS_CONST_UNDEFINED_ID);
}

#[test]
fn a_compound_read_judges_the_missing_key_only() {
    fires("$a['nope']++;", OFFSET_MISSING_ID);
    fires("$a['nope'] .= 'x';", OFFSET_MISSING_ID);
    fires("$a['nope'] += 1;", OFFSET_MISSING_ID);
    // A null base is created by the write (`Undefined array key`, not the on-null read).
    silent("$z['k'] .= 'x';");
    // On null the property is an Error of its own, and a private one takes `__set`.
    silent("$z->p .= 'x';");
    silent("$c->priv++;");
}

#[test]
fn a_dead_extent_is_not_judged() {
    silent("echo defined('A::NOPE') ? A::NOPE : 1;");
    silent("echo defined('A::NOPE') && A::NOPE;");
    silent("echo !defined('A::NOPE') || A::NOPE;");
    // Undecided (no boot surface answers `function_exists` here): the arm is live.
    fires("echo function_exists('nope_fn') ? A::NOPE : 1;", CLASS_CONST_UNDEFINED_ID);
}

#[test]
fn a_read_whose_base_the_statement_may_rebind_declines() {
    // `f2($a)` by reference may create the key before the read; the read before it
    // (`$a['nope'] . byref($a)`) warns, and is a recorded false negative.
    silent("echo byref($a) . $a['nope'];");
    silent("echo $a['nope'] . byref($a);");
    silent("echo ($a['nope'] = 1) + $a['nope'];");
    silent("$r = [&$a['nope'], $a['nope']];");
    silent("echo $o->m($a) . $a['nope'];");
    silent("echo byref($z) . $z->p;");
    // A by-value call keeps the binding.
    fires("echo byval($a) . $a['nope'];", OFFSET_MISSING_ID);
}

#[test]
fn a_top_level_userland_call_may_rebind_every_global() {
    // The top-level frame's locals are the globals: `g()` runs before the read and
    // creates the key (silent in PHP).
    let src = "<?php
function g() { global $a; $a['nope'] = 1; return ''; }
$a = ['k' => 1];
echo strlen('x') . $a['nope'];
echo g() . $a['nope'];
";
    let tree = SourceTree::parse(src);
    let lines: Vec<u32> = check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| d.id == OFFSET_MISSING_ID)
        .map(|d| d.line)
        .collect();
    assert_eq!(lines, [4], "only the engine builtin keeps the binding");
}

#[test]
fn a_userland_call_may_rebind_a_by_reference_parameter() {
    // `$fn` captured the caller's `$arr` by reference: calling it writes through
    // `&$a` before the read runs (silent in PHP).
    let src = "<?php
function g(array &$a, callable $fn, array $b) {
    $a = [];
    $b = [];
    echo strlen('x') . $a['nope'];
    echo $fn() . $a['nope'];
    echo $fn() . $b['nope'];
}
";
    let tree = SourceTree::parse(src);
    let lines: Vec<u32> = check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| d.id == OFFSET_MISSING_ID)
        .map(|d| d.line)
        .collect();
    assert_eq!(lines, [5, 7], "a by-value local keeps its binding across the call");
}

#[test]
fn a_key_variable_resolves_through_the_env() {
    fires("$k = 'nope'; echo $a[$k];", OFFSET_MISSING_ID);
    silent("$k = 'k'; echo $a[$k];");
    silent("echo $a[$u];");
}

// #949: offset reads.

#[test]
fn issue_949_plain_positions_warn() {
    for body in [
        "echo $a['nope'];",
        "echo strlen((string) $a['nope']);",
        "echo 1 + $a['nope'];",
        "echo 'x' . $a['nope'];",
        "echo \"v {$a['nope']}\";",
        "echo (int) $a['nope'];",
        "echo $a['nope'] ? 1 : 2;",
        "$x = [$a['nope']];",
        "echo -$a['nope'];",
        "echo !$a['nope'];",
        "echo $a['nope'] === 1;",
        "return $a['nope'] + 1;",
        "echo $a['nope'] ?: 1;",
    ] {
        fires(body, OFFSET_MISSING_ID);
    }
}

#[test]
fn issue_949_every_level_of_a_chain_is_its_own_read() {
    // r4: the base `$a['n']` resolves through the same fact.
    fires("echo $a['n']['zz'];", OFFSET_MISSING_ID);
    silent("echo $a['n']['x'];");
    // r33: the outer level of a missing inner one is the inner read's cascade.
    fires("echo $a['nope']['deeper'];", OFFSET_MISSING_ID);
}

#[test]
fn issue_949_silent_rows_stay_silent() {
    for body in [
        "$r = array_key_exists('nope', $a);",
        "unset($a['nope']);",
        "$r = &$a['nope'];",
        "echo isset($a['k']) && $a['nope'];",
        "echo count($a) > 0 && $a['nope'];",
        "echo $a['k'] ?? $a['nope'];",
    ] {
        silent(body);
    }
}

// #936: properties and class constants.

#[test]
fn issue_936_property_plain_positions_warn() {
    for body in [
        "echo $c->nope;",
        "echo \"v {$c->nope}\";",
        "echo $c->nope + 1;",
        "echo (string) $c->nope;",
        "$x = [$c->nope];",
        "echo $c->nope === null;",
    ] {
        fires(body, PROPERTY_UNDEFINED_ID);
    }
    fires("echo $c->priv + 1;", PROPERTY_INACCESSIBLE_ID);
}

#[test]
fn issue_936_property_silent_rows_stay_silent() {
    for body in [
        "echo isset($c->nope) ? 1 : 0;",
        "echo $c->nope ?? 1;",
        "echo isset($c->priv) ? 1 : 0;",
        "echo property_exists($c, 'nope') ? $c->nope : 1;",
        "echo isset($c->nope) && $c->nope;",
    ] {
        silent(body);
    }
}

#[test]
fn issue_936_class_constant_positions_throw() {
    for body in [
        "echo A::NOPE;",
        "echo 1 === A::NOPE;",
        "echo A::NOPE + 1;",
        "$x = [A::NOPE];",
        "echo A::NOPE ?? 1;",
        "echo A::NOPE === null;",
        "echo strlen(A::NOPE);",
        "echo 'x' . A::NOPE;",
        "echo A::NOPE ? 1 : 2;",
        "echo (string) A::NOPE;",
        "$x = [A::NOPE => 1];",
        "echo !A::NOPE;",
    ] {
        fires(body, CLASS_CONST_UNDEFINED_ID);
    }
    for body in ["echo A::P;", "echo A::P + 1;", "echo A::P === 2;"] {
        fires(body, CLASS_CONST_INACCESSIBLE_ID);
    }
    fires("echo $u === Suit::Clubs;", CLASS_CONST_UNDEFINED_ID);
    silent("echo A::class;");
    silent("echo A::K;");
}

#[test]
fn a_read_is_reported_once() {
    // The whole value of `$x =` and of `return` was the only judged position before;
    // it is one position among the others now, never two.
    assert_eq!(ids("$x = $a['nope'];").len(), 1);
    assert_eq!(ids("return A::NOPE;").len(), 1);
    assert_eq!(ids("$x = $c->priv;").len(), 1);
}
