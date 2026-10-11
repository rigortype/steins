//! Unresolved aliases of heap objects (issue #1048): an object that reaches a name
//! the store does not bind — a foreach value over the array it was stored in, an
//! element read back out, a callback parameter — is the same object, so a write
//! through that name must not leave the original binding's property fact standing.
//!
//! Every fixture is a witness probed on PHP 8.5.11 with `var_dump` in place of each
//! dump: in each reach row the write through the alias is visible through `$o`, and
//! in each control it is not. The rule is documented in `heap_alias.rs`.

use steins_infer::{Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// An engine that folds nothing and says every function exists.
struct Probe;

impl Folder for Probe {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_function(&mut self, _fqn: &str) -> Option<bool> {
        Some(true)
    }
}

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Probe)
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

/// The findings of `src` other than the dumps and the `untyped.*` surface, as
/// `line id`.
fn findings(src: &str) -> Vec<String> {
    run(src)
        .into_iter()
        .filter(|d| d.id != "debug.type" && !d.id.starts_with("untyped."))
        .map(|d| format!("{} {}", d.line, d.id))
        .collect()
}

const PRELUDE: &str = "<?php
class O { public $p = 0; public function set($v): void { $this->p = $v; } }
function byref(&$x): void { $x = 'changed'; }
function ident($x) { return $x; }
function gen($o) { yield $o; }
function mutate($x): void { $x->p = 'changed'; }
function spreadw(O ...$xs): void { foreach ($xs as $q) { $q->p = 'changed'; } }
";

/// `PRELUDE` plus `function {sig} { $o = new O(); $o->p = 1; {body} dump($o->p); }`,
/// the dump on line 8.
fn case(sig: &str, body: &str) -> String {
    let dump = "\\PHPStan\\dumpType($o->p);";
    format!("{PRELUDE}function {sig} {{ $o = new O(); $o->p = 1; {body} {dump} }}\n")
}

/// The reach rows of the rule table: how `$o` comes to `$q`. `WRITE` is replaced by
/// one write row. In every row PHP prints `'changed'` (or nothing, for `unset`).
const REACH: &[(&str, &str)] = &[
    ("foreach", "$arr = [$o]; foreach ($arr as $q) { WRITE }"),
    ("foreach by-ref", "$arr = [$o]; foreach ($arr as &$q) { WRITE } unset($q);"),
    ("foreach literal", "foreach ([$o] as $q) { WRITE }"),
    ("list", "$arr = [$o]; [$q] = $arr; WRITE"),
    ("array_map", "$arr = [$o]; array_map(function ($q) { WRITE return $q; }, $arr);"),
    ("array_walk", "$arr = [$o]; array_walk($arr, function ($q) { WRITE });"),
    ("array_map literal", "array_map(function ($q) { WRITE return $q; }, [$o]);"),
    ("current", "$arr = [$o]; $q = current($arr); WRITE"),
    ("reset", "$arr = [$o]; $q = reset($arr); WRITE"),
    ("end", "$arr = [$o]; $q = end($arr); WRITE"),
    ("array_pop", "$arr = [$o]; $q = array_pop($arr); WRITE"),
    ("array_shift", "$arr = [$o]; $q = array_shift($arr); WRITE"),
    ("dynamic key", "$arr = [$o]; $k = rand(0, 0); $q = $arr[$k]; WRITE"),
    ("constant key", "$arr = [$o]; $q = $arr[0]; WRITE"),
    (
        "iterator_to_array",
        "$arr = [$o]; foreach (iterator_to_array(new ArrayIterator($arr)) as $q) { WRITE }",
    ),
    ("generator", "foreach (gen($o) as $q) { WRITE }"),
    ("array_values", "$arr = [$o]; $q = array_values($arr)[0]; WRITE"),
    ("array_filter", "$arr = [$o]; foreach (array_filter($arr) as $q) { WRITE }"),
    ("identity", "$q = ident($o); WRITE"),
    ("ternary", "$q = rand(0, 1) ? $o : $o; WRITE"),
    ("coalesce", "$q = $o ?? null; WRITE"),
    ("join", "$o2 = new O(); if (rand(0, 1)) { $q = $o; } else { $q = $o2; } WRITE"),
    ("property", "$h = new O(); $h->p = $o; $q = $h->p; WRITE"),
    ("match", "$q = match (rand(0, 0)) { 0 => $o, default => $o }; WRITE"),
    ("object cast", "$w = (object) ['o' => $o]; $q = $w->o; WRITE"),
    ("array cast", "$a = (array) [$o]; foreach ($a as $q) { WRITE }"),
    ("loop-carried name", "$o2 = new O(); $q = $o; $i = 0; while ($i++ < 2) { WRITE $q = $o2; }"),
];

/// The write rows: how the write reaches the object through `$q`.
const WRITES: &[(&str, &str)] = &[
    ("property write", "$q->p = 'changed';"),
    ("method call", "$q->set('changed');"),
    ("unset", "unset($q->p);"),
    ("by-ref pass", "byref($q->p);"),
    ("pass to a mutating function", "mutate($q);"),
];

#[test]
fn no_reach_and_write_pair_leaves_the_original_fact_stale() {
    let mut stale = Vec::new();
    for (reach, body) in REACH {
        for (write, w) in WRITES {
            let got = dumps(&case("t(): void", &body.replace("WRITE", w)));
            if got.len() != 1 || got[0].ends_with(": 1") {
                stale.push(format!("{reach} x {write}: {got:?}"));
            }
        }
    }
    assert!(stale.is_empty(), "stale facts:\n{}", stale.join("\n"));
}

#[test]
fn a_spread_into_a_resolved_callee_reaches_the_element() {
    let src = case("t(): void", "$arr = [$o]; spreadw(...$arr);");
    assert_eq!(dumps(&src), vec!["8: unknown"]);
}

#[test]
fn a_by_ref_pass_of_a_bound_property_forgets_it() {
    // No alias at all: the callee writes `$o->p` in place.
    let src = case("t(): void", "byref($o->p);");
    assert_eq!(dumps(&src), vec!["8: unknown"]);
}

#[test]
fn a_yield_suspends_the_generator_and_its_consumer_may_write_the_handle() {
    let src = case("t(): \\Generator", "$r = yield $o;");
    assert_eq!(dumps(&src), vec!["8: unknown"]);
}

#[test]
fn an_unknown_call_in_a_loop_body_reaches_the_fall_through() {
    // `keep` stores the object where `mut` reaches it; the loop body's unknown call
    // used to sweep only the body's own env.
    let src = "<?php
class O { public $p = 0; }
class Reg { public static $o; }
function keep(O $o): void { Reg::$o = $o; }
function t(): void {
    $o = new O(); keep($o); $o->p = 1;
    foreach ([1] as $i) { unknownfn(); }
    \\PHPStan\\dumpType($o->p);
}
";
    assert_eq!(dumps(src), vec!["8: unknown"]);
}

// ---- the reviewer's probes (#1047): t7 b6-b9 and t6 b3-b5 ---------------------

const T_PRELUDE: &str = "<?php
class E extends Exception {}
class O { public $p = 0; }
function g(): void { if (rand(0, 1)) throw new E(); }
function f(): void { throw new E(); }
";

#[test]
fn the_t7_probes_no_longer_answer_the_stale_value() {
    let src = format!(
        "{T_PRELUDE}function b6(): void {{
    $o = new O(); $o->p = 1; $arr = [$o];
    foreach ($arr as $q) {{ $q->p = 'changed'; }}
    \\PHPStan\\dumpType($o->p);
}}
function b7(): void {{
    $o = new O(); $o->p = 1; $arr = [$o];
    try {{ foreach ($arr as $q) {{ $q->p = 'changed'; }} g(); }} catch (E $e) {{ }}
    \\PHPStan\\dumpType($o->p);
}}
function b8(): void {{
    $o = new O(); $o->p = 1; $arr = [$o];
    try {{ foreach ($arr as $q) {{ $q->p = 'changed'; }} }} finally {{ }}
    \\PHPStan\\dumpType($o->p);
}}
function b9(): void {{
    $o = new O(); $o->p = 1; $arr = [$o];
    try {{ $q = $arr[0]; $q->p = 'changed'; g(); }} catch (E $e) {{ }}
    \\PHPStan\\dumpType($o->p);
}}
"
    );
    assert_eq!(dumps(&src), vec!["9: unknown", "14: unknown", "19: unknown", "24: unknown"]);
}

#[test]
fn the_t6_probes_keep_their_answers() {
    let src = format!(
        "{T_PRELUDE}function b3(): void {{
    $o = new O(); $o->p = 1;
    $c = function () use ($o) {{ $o->p = 'changed'; f(); }};
    try {{ $c(); }} catch (E $e) {{ }}
    \\PHPStan\\dumpType($o->p);
}}
function b4(): void {{
    $o = new O(); $o->p = 1; $h = new O(); $h->p = $o;
    try {{ $h->p->p = 'changed'; f(); }} catch (E $e) {{ }}
    \\PHPStan\\dumpType($o->p);
}}
function b5(): void {{
    $o = new O(); $o->p = 1; $arr = [$o];
    try {{ $arr[0]->p = 'changed'; f(); }} catch (E $e) {{ }}
    \\PHPStan\\dumpType($o->p);
}}
"
    );
    assert_eq!(dumps(&src), vec!["10: unknown", "15: unknown", "20: unknown"]);
}

// ---- controls: no alias, so the fact survives ----------------------------------

#[test]
fn a_write_through_an_alias_of_another_object_keeps_the_fact() {
    // PHP: 1 — the array holds a different object.
    let src = case("t(): void", "$arr = [new O()]; foreach ($arr as $q) { $q->p = 'changed'; }");
    assert_eq!(dumps(&src), vec!["8: 1"]);
}

#[test]
fn a_write_through_a_parameter_keeps_a_local_objects_fact() {
    // PHP: 1 — `$x` cannot be `$o`, which no route handed out.
    let src = case("t(O $x): void", "$x->p = 'changed';");
    assert_eq!(dumps(&src), vec!["8: 1"]);
}

#[test]
fn storing_an_object_without_a_later_write_keeps_the_fact() {
    // Escaping is not forgetting: no write and no call follows.
    let src = case("t(): void", "$arr = [$o]; foreach ($arr as $q) { }");
    assert_eq!(dumps(&src), vec!["8: 1"]);
}

// ---- a finding that rested on the stale fact -----------------------------------

const SINK: &str = "<?php
function needInt(int $x): int { return $x; }
class O { public $p = 0; }
";

#[test]
fn a_mismatch_resting_on_the_stale_fact_is_not_reported() {
    // PHP: no TypeError — the foreach wrote `5` through the alias before the call.
    let src = format!(
        "{SINK}function t(): void {{
    $o = new O(); $o->p = 'abc';
    $arr = [$o];
    foreach ($arr as $q) {{ $q->p = 5; }}
    needInt($o->p);
}}
"
    );
    assert_eq!(findings(&src), Vec::<String>::new());
}

#[test]
fn the_same_mismatch_without_the_alias_is_reported() {
    // The twin: the array holds another object, and PHP throws the TypeError.
    let src = format!(
        "{SINK}function t(): void {{
    $o = new O(); $o->p = 'abc';
    $arr = [new O()];
    foreach ($arr as $q) {{ $q->p = 5; }}
    needInt($o->p);
}}
"
    );
    assert_eq!(findings(&src), vec!["8 type.argument-mismatch"]);
}

// ---- review round 1 (PR #1056): the reviewer's witnesses, PHP 8.5.11 ----------

const F_PRELUDE: &str = "<?php
final class F { public $p = 0; public $r = 0; }
";

/// `F_PRELUDE` plus one function body; the dump is on line 3.
fn f_case(sig: &str, body: &str) -> String {
    format!("{F_PRELUDE}function {sig} {{ {body} }}\n")
}

#[test]
fn a_property_written_inside_an_expression_reaches_the_original() {
    // S1: `eb4`, `exprw`, `exprw2`, `exprw4`, `forw`. PHP: 'changed' in each.
    let shapes = [
        "foreach ($arr as $q) { $y = $q->p = 'changed'; }",
        "$q = $arr[0]; echo ($q->p = 'changed');",
        "$q = $arr[0]; if ($q->p = 'changed') {}",
        "$q = $arr[0]; $y = $q->p = 'changed';",
        "$q = $arr[0]; foreach (['changed'] as $q->p) {}",
    ];
    for shape in shapes {
        let dump = "\\PHPStan\\dumpType($o->p);";
        let body = format!("$o = new F(); $o->p = 1; $arr = [$o]; {shape} {dump}");
        assert_eq!(dumps(&f_case("t(): void", &body)), vec!["3: unknown"], "{shape}");
    }
}

#[test]
fn a_yield_anywhere_in_a_statement_suspends_the_generator() {
    // S2: `yw1` (PHP: 'changed'), and a condition and a return operand.
    for shape in ["echo (yield $o);", "if (yield $o) {}", "$x = [yield $o];"] {
        let body = format!("$o = new F(); $o->p = 1; {shape} \\PHPStan\\dumpType($o->p);");
        assert_eq!(dumps(&f_case("g(): \\Generator", &body)), vec!["3: unknown"], "{shape}");
    }
}

#[test]
fn a_by_ref_generator_forgets_its_locals_at_a_yield() {
    // S6, `refgen` and `refgen2`: the consumer writes `$x` through the reference.
    // PHP: int(2).
    for shape in ["$r = yield $x;", "echo (yield $x);"] {
        let body = format!("$x = 1; {shape} \\PHPStan\\dumpType($x);");
        assert_eq!(dumps(&f_case("&g()", &body)), vec!["3: unknown"], "{shape}");
    }
}

#[test]
fn a_by_value_generator_keeps_its_locals_at_a_yield() {
    // S6's precision half (the reviewer's `y2.php`): only objects can be reached.
    // (A bare `yield $x;` statement is a barrier, as on the base.)
    for shape in ["$r = yield;", "$r = yield $x;", "$r = yield from inner();"] {
        let body = format!("$x = 1; {shape} \\PHPStan\\dumpType($x);");
        let src = format!("{}function inner() {{ yield 1; }}\n", f_case("g()", &body));
        assert_eq!(dumps(&src), vec!["3: 1"], "{shape}");
    }
}

#[test]
fn a_name_forgotten_by_an_embedded_write_escapes_its_object() {
    // S3: `ub1`, `ub10`, `ub16`, `ub17`, `ub18`. PHP: 'changed' in each.
    let shapes = [
        "$y = $o->p = 'changed';",
        "echo $o->r = 2; $o->p = 'changed';",
        "if ($o->r = 2) {} $o->p = 'changed';",
        "if (($o->r = 2) && true) { $o->p = 'changed'; }",
        "assert(($o->r = 2) > 0); $o->p = 'changed';",
    ];
    for shape in shapes {
        let body = format!("$o = new F(); $o->p = 1; $b = $o; {shape} \\PHPStan\\dumpType($b->p);");
        assert_eq!(dumps(&f_case("t(): void", &body)), vec!["3: unknown"], "{shape}");
    }
}

#[test]
fn an_escape_inside_a_loop_body_reaches_the_fall_through() {
    // S4: `lb20` and `sl6d`. PHP: 'changed' in both.
    let shapes = [
        "foreach ($items as $i) { $f = fn() => $o; } $q = $f(); $q->p = 'changed';",
        "foreach ([1] as $_) { $f = fn() => $o; $q = $f(); $q->p = 'changed'; }",
    ];
    for shape in shapes {
        let body = format!("$o = new F(); $o->p = 1; {shape} \\PHPStan\\dumpType($o->p);");
        assert_eq!(dumps(&f_case("t(array $items): void", &body)), vec!["3: unknown"], "{shape}");
    }
}

#[test]
fn a_reassigned_alias_does_not_escape_the_object() {
    // `release`, not `unbind`: `$q` stops holding `$o` and no name can write it.
    let body = "$o = new F(); $o->p = 1; $q = $o; $q = 5; \\PHPStan\\dumpType($o->p);";
    assert_eq!(dumps(&f_case("t(): void", body)), vec!["3: 1"]);
}
