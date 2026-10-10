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

#[test]
fn a_literal_key_in_a_function_forgets_the_same_named_local_harmlessly() {
    // In a function `$GLOBALS['go']` is the global, not the local: the local keeps
    // `true` and the loop never ends. The walk forgets it anyway (D5's
    // over-approximation) and reports the successor, which is the price of one rule
    // for every scope.
    assert_eq!(on_null_after_loop("$go = true;", "$GLOBALS['go'] = false;", true), 1);
    assert_eq!(on_null_after_loop("$go = true;", "echo 1;", true), 0);
}
