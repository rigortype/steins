//! A write through `$GLOBALS['lit']` is a write of `lit`, and a write through
//! `$GLOBALS[$k]` poisons the scope (issue #902, walker coverage run #1033 D5).
//!
//! At file scope `$GLOBALS['go']` IS `$go`. A loop whose body writes it that way used
//! to leave `$go = true` in the entry env, so the header read `Yes`, no test could fail,
//! and the statement after the loop was dead to the walker while PHP reaches it:
//!
//! ```php
//! $go = true;
//! while ($go) { $GLOBALS['go'] = false; }
//! $x = null;
//! $x->bar();   // PHP 8.5: Error "Call to a member function bar() on null"
//! ```
//!
//! Every form is witnessed on PHP 8.5.11 to write the global (the rule table is the
//! doc comment of `lower_globals.rs`). Each case below judges the header, not the
//! loop's own exit: the finding after the loop is the proof that the header was not
//! read as `Yes`.

use steins_infer::check;
use steins_syntax::SourceTree;

/// How many `call.on-null` findings follow `prelude` and a `while ($go) { body }`
/// loop, at file scope or in a function body.
fn on_null_after_loop(prelude: &str, body: &str, in_function: bool) -> usize {
    let (open, close) = if in_function { ("function f(): void {", "}\nf();") } else { ("", "") };
    let src = format!(
        "<?php
final class Foo {{ public function bar(): void {{}} }}
function drop(&$p): void {{ $p = false; }}
{open}
{prelude}
while ($go) {{ {body} }}
$x = null;
$x->bar();
{close}
"
    );
    let tree = SourceTree::parse(&src);
    check(&tree, &[], "t.php").into_iter().filter(|d| d.id == "call.on-null").count()
}

fn top(prelude: &str, body: &str) -> usize {
    on_null_after_loop(prelude, body, false)
}

#[test]
fn the_header_is_decided_yes_when_nothing_writes_the_name() {
    // The control every case below leans on: with a body that writes nothing, the
    // header is `Yes` on the entry env and the successor is dead.
    assert_eq!(top("$go = true;", "echo 1;"), 0);
}

#[test]
fn a_literal_key_write_forgets_the_name_in_every_write_form() {
    // Each of these writes the global `$go` on PHP 8.5.11.
    for (prelude, body) in [
        ("$go = true;", "$GLOBALS['go'] = false;"),
        ("$go = true;", "$GLOBALS['go'] &= 0;"),
        ("$go = true;", "$GLOBALS['go'] ??= false;"),
        ("$go = true;", "unset($GLOBALS['go']);"),
        ("$go = [1];", "$GLOBALS['go'][] = 2;"),
        ("$go = [1];", "$GLOBALS['go']['k'] = 2;"),
        ("$go = -1;", "++$GLOBALS['go'];"),
        ("$go = -1;", "$GLOBALS['go']++;"),
        ("$go = true;", "[$GLOBALS['go'], $y] = [false, 1];"),
        ("$go = true;", "['a' => $GLOBALS['go']] = ['a' => false];"),
        ("$go = true;", "foreach ([false] as $GLOBALS['go']) {}"),
        ("$go = true;", "foreach ([false] as $k => $GLOBALS['go']) {}"),
        ("$go = true;", "$GLOBALS['go'] = false; $GLOBALS['other'] = 1;"),
    ] {
        assert_eq!(top(prelude, body), 1, "`{body}` writes the global `$go`");
    }
}

#[test]
fn a_literal_key_write_of_another_name_leaves_the_header_decided() {
    // The twin: the same shapes aimed at a different name keep the entry env.
    for body in [
        "$GLOBALS['other'] = false;",
        "unset($GLOBALS['other']);",
        "$GLOBALS['other'][] = 1;",
        "echo $GLOBALS['go'];",
        "$y = $GLOBALS['go'];",
        "echo isset($GLOBALS['go']);",
    ] {
        assert_eq!(top("$go = true;", body), 0, "`{body}` does not write `$go`");
    }
}

#[test]
fn a_by_ref_argument_through_dollar_globals_hands_the_name_to_the_call() {
    // `settype(&$var, …)` writes through the reference: PHP 8.5.11 leaves `$go`
    // false. (A userland callee would forget the whole top-level frame on its own,
    // #762, so the discriminating call is a builtin.)
    assert_eq!(top("$go = true;", "settype($GLOBALS['go'], 'bool'); settype($GLOBALS['go'], 'int');"), 1);
    assert_eq!(top("$go = [1];", "sort($GLOBALS['go']['k']);"), 1);
    assert_eq!(top("$go = true;", "settype($GLOBALS['other'], 'bool');"), 0);
}

#[test]
fn a_reference_to_a_global_element_poisons_the_scope_as_it_always_did() {
    // No finding either way: `$r = &$GLOBALS['go']` is a reference assignment, on the
    // give-up list, so the scope holds no fact to convict with.
    assert_eq!(top("$go = true;", "$r = &$GLOBALS['go']; $r = false;"), 0);
}

#[test]
fn a_dynamic_key_write_poisons_the_scope() {
    // Silence by the poisoned scope, in the same family as `extract` and `$$v`.
    let control = "while (rand() > 0) { echo 1; }\n$x = null;\n$x->bar();";
    let src = |extra: &str| {
        format!(
            "<?php
final class Foo {{ public function bar(): void {{}} }}
function drop(&$p): void {{ $p = false; }}
$k = 'go';
{extra}
{control}
"
        )
    };
    let count = |src: &str| {
        let tree = SourceTree::parse(src);
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == "call.on-null").count()
    };
    assert_eq!(count(&src("")), 1, "the control reports");
    for stmt in [
        "$GLOBALS[$k] = false;",
        "$GLOBALS[$k] .= 'x';",
        "$GLOBALS[$k][] = 1;",
        "$GLOBALS[$k]->p = 1;",
        "unset($GLOBALS[$k]);",
        "++$GLOBALS[$k];",
        "$GLOBALS[$k]--;",
        "[$GLOBALS[$k], $y] = [1, 2];",
        "foreach ([1] as $GLOBALS[$k]) {}",
        "drop($GLOBALS[$k]);",
        "drop($GLOBALS[$k]['j']);",
        "$f = 'drop'; $f($GLOBALS[$k]);",
        "$o = new ArrayObject(); $o->append($GLOBALS[$k]);",
        "$o = new ArrayObject($GLOBALS[$k]);",
        "Foo::make($GLOBALS[$k]);",
    ] {
        assert_eq!(count(&src(stmt)), 0, "`{stmt}` writes a global named at run time");
    }
}

#[test]
fn a_dynamic_key_read_does_not_poison() {
    let src = "<?php
final class Foo { public function bar(): void {} }
$k = 'go';
echo $GLOBALS[$k];
echo isset($GLOBALS[$k]) ? 1 : 2;
$v = $GLOBALS[$k] ?? null;
$GLOBALS['other'] = $GLOBALS[$k];
while (rand() > 0) { echo 1; }
$x = null;
$x->bar();
";
    let tree = SourceTree::parse(src);
    let n = check(&tree, &[], "t.php").into_iter().filter(|d| d.id == "call.on-null").count();
    assert_eq!(n, 1);
}

fn count_on_null(src: &str) -> usize {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php").into_iter().filter(|d| d.id == "call.on-null").count()
}

#[test]
fn a_literal_key_in_a_function_body_writes_no_local() {
    // `$GLOBALS['go']` there is the global, not the local: the local keeps `true`, the
    // loop never ends, and the successor is dead (PHP 8.5.11: `var_dump($go)` prints
    // `bool(true)`). Forgetting the local would turn the `Yes` header unknown and report.
    let body = "$go = true; $x = null; while ($go) { $GLOBALS['go'] = false; } $x->bar();";
    let src = |wrap: &str| {
        format!(
            "<?php
final class Foo {{ public function bar(): void {{}} }}
{}
",
            wrap.replace("BODY", body)
        )
    };
    assert_eq!(count_on_null(&src("function a(): void { BODY }")), 0);
    assert_eq!(count_on_null(&src("final class K { public function m(): void { BODY } }")), 0);
    assert_eq!(count_on_null(&src("$f = function (): void { BODY };")), 0);
    assert_eq!(count_on_null(&src("$f = static fn () => 1; function b(): void { BODY }")), 0);
    // The control: the same function with a header that can fail reports.
    let live = "$go = true; $x = null; while (rand() > 0) { $GLOBALS['go'] = false; } $x->bar();";
    assert_eq!(count_on_null(&src(&format!("function a(): void {{ {live} }}"))), 1);
}

#[test]
fn the_ways_a_local_is_bound_to_the_global_stay_silent() {
    // `global $go;` and a by-ref `use (&$go)` make the local the global (PHP reaches
    // `bar()` in both); each is a give-up construct, so the scope holds no `Yes`.
    let src = |wrap: &str| {
        format!(
            "<?php
final class Foo {{ public function bar(): void {{}} }}
{wrap}
"
        )
    };
    let global = "function a(): void { global $go; $x = null; while ($go) { $GLOBALS['go'] = false; } $x->bar(); }";
    let capture = "$go = true; $f = function () use (&$go): void { $x = null; while ($go) { $GLOBALS['go'] = false; } $x->bar(); };";
    assert_eq!(count_on_null(&src(global)), 0);
    assert_eq!(count_on_null(&src(capture)), 0);
}

#[test]
fn a_dynamic_key_handed_to_a_call_inside_a_function_does_not_poison() {
    // No callee can reach a local through `$GLOBALS[$k]`, so a by-value builtin (the
    // idiomatic `isset($GLOBALS[$k]) && is_array($GLOBALS[$k])`) keeps the findings.
    for call in ["is_array($GLOBALS[$k])", "strlen($GLOBALS[$k])", "count($GLOBALS[$k])", "sort($GLOBALS[$k])"] {
        let src = format!(
            "<?php
final class Foo {{ public function bar(): void {{}} }}
function f(string $k): void {{ $x = null; if ({call}) {{ echo 1; }} $x->bar(); }}
"
        );
        assert_eq!(count_on_null(&src), 1, "`{call}` inside a function");
    }
}

#[test]
fn a_dynamic_key_handed_to_a_call_at_file_scope_poisons() {
    // Conservative: the syntax layer cannot ask whether the callee's parameter is by-ref.
    let src = "<?php
final class Foo { public function bar(): void {} }
$k = 'x';
sort($GLOBALS[$k]);
$x = null;
$x->bar();
";
    assert_eq!(count_on_null(src), 0);
}
