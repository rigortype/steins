//! A pure expression statement, or a literal `match` arm, is a no-op for the walk, not a
//! barrier (issue #946, walker coverage run #1033 S4).
//!
//! The lowering turned anything it did not recognise into a `Barrier`, and a barrier
//! forgets every local. `1;`, `$i;` and the literal arms of `$v = match ($i) { 1 => 10,
//! default => 20 }` reached it, so a string bound before them dumped as `'a'` and then
//! as `unknown`, and `return $s;` in an `: int` function (a `TypeError` on PHP 8.5.11)
//! went unreported. The predicate that decides which expressions are no-ops is a table
//! in `lower_pure.rs`; each excluded row is witnessed there to run user code, write or
//! throw, and each has a control below that must stay a barrier.

use steins_infer::{DEBUG_TYPE_ID, check};
use steins_syntax::SourceTree;

/// The `debug.type` body of a dump of `$s` that follows `stmt`, inside a function whose
/// parameters are an int, a `Foo`, a string and an array, with `$s = "a"` bound first.
fn dump_after(stmt: &str) -> String {
    let src = format!(
        "<?php
declare(strict_types=1);
final class Foo {{ public const BAR = 1; public int $p = 1; }}
function f(int $i, Foo $o, string $c, array $a): void {{
    $s = \"a\";
    {stmt}
    \\PHPStan\\dumpType($s);
}}
"
    );
    let tree = SourceTree::parse(&src);
    let dumps: Vec<String> = check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(dumps.len(), 1, "`{stmt}` leaves one dump, got {dumps:?}");
    dumps.into_iter().next().unwrap()
}

fn mismatches(src: &str) -> usize {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php").into_iter().filter(|d| d.id == "type.return-mismatch").count()
}

#[test]
fn a_pure_statement_keeps_the_env() {
    // Each is a no-op on PHP 8.5.11: no user code, no write, no output.
    for stmt in [
        "1;",
        "'text';",
        "null;",
        "$i;",
        "!1;",
        "1 === 2;",
        "'a' !== 1;",
        "['k' => 2] === [1];",
        "isset($i);",
        "isset($i, $c);",
        "empty($c);",
        "[1, 2.5, 'x', null, true];",
        "['a' => 1, 'b' => [2, 3]];",
        "[0 => 'a', 1 => 'b'];",
        "__LINE__;",
        "(1);",
    ] {
        assert_eq!(dump_after(stmt), "dumped type: 'a'", "`{stmt}` is a no-op");
    }
}

#[test]
fn a_literal_match_arm_keeps_the_env() {
    for stmt in [
        "$v = match ($i) { 1 => 10, default => 20 };",
        "$v = match ($i) { 1 => 'x', 2 => [1, 2], default => null };",
        "match ($i) { 1 => 10, default => 20 };",
        "echo match ($i) { 1 => 10, default => 20 };",
    ] {
        assert_eq!(dump_after(stmt), "dumped type: 'a'", "`{stmt}` keeps $s");
    }
}

#[test]
fn the_witness_functions_report_what_php_throws() {
    // `a1`..`a3` of the issue: each throws `TypeError` (string returned from `: int`).
    let src = "<?php
declare(strict_types=1);
function a1(int $i): int { $s = \"a\"; 1; return $s; }
function a2(int $i): int { $s = \"a\"; $i; return $s; }
function a3(int $i): int { $s = \"a\"; $v = match ($i) { 1 => 10, default => 20 }; return $s; }
function a4(int $i): int { $s = \"a\"; $v = match ($i) { 1 => f(), default => f() }; return $s; }
function f(): int { return 1; }
";
    assert_eq!(mismatches(src), 4);
}

#[test]
fn a_statement_that_runs_code_or_writes_stays_a_barrier() {
    // Each row is why the predicate refuses it; the dump after is the barrier's `unknown`.
    for stmt in [
        // `__get`, `offsetGet` and autoload (witnessed on PHP 8.5.11).
        "$o->p;",
        "$a[0];",
        "$a['k'] ?? 1;",
        "Foo::BAR;",
        "Nope::C;",
        "Foo::$q;",
        // `__toString` of an operand the lowering cannot type.
        "$c . 'a';",
        "$c == 'a';",
        "$c < 'b';",
        "$c <=> 'b';",
        "(string) $o;",
        "(array) $o;",
        // A constant may be undefined, which throws.
        "NOPE;",
        "PHP_INT_MAX;",
        // A write, an output, an error-level change, a code run.
        "$i++;",
        "--$i;",
        "@$i;",
        "print 1;",
        "clone $o;",
        "include 'x.php';",
        "eval('1;');",
        "`ls`;",
        "$o instanceof $c;",
        "[...$a];",
        "[&$i];",
        "$this;",
        "fn() => 1;",
        "function () use (&$s) {};",
        // A diagnostic reaches `set_error_handler` (user code), so each of these is out,
        // witnessed on PHP 8.5.11 with a handler that rebinds a local.
        "~1.5;",
        "1.5 | 1;",
        "(int) 1e30;",
        "0 ** -1;",
        "'1a' + 1;",
        "['a' => 1]['b'];",
        "'ab'[5];",
        "'abc'[1.5];",
        "[1, 2][1.5];",
        "[1][null];",
        "[1.5 => 1];",
        "[null => 1];",
        "[true => 1];",
        "['a' => 1, 'a' => 2];",
        "[1, 0 => 2];",
        "[0 => 1, '0' => 2];",
        // An undefined variable read inside a larger expression warns.
        "$zz ? 1 : 2;",
        "$zz && 1;",
        "(int) $zz;",
        "-$zz;",
        "$zz instanceof Foo;",
        "$i === 1;",
        "!$i;",
        "[$i];",
        // These throw, so the code after them is not reached with these values.
        "1 / 0;",
        "1 % 0;",
        "-[1];",
        "1 << -1;",
        "$i + 1;",
        "1 . 'a';",
        "1 == 2;",
        "(string) 1;",
    ] {
        assert_eq!(dump_after(stmt), "dumped type: unknown", "`{stmt}` stays a barrier");
    }
}

#[test]
fn a_handler_that_rebinds_a_local_is_not_outrun_by_a_diagnostic_statement() {
    // `~1.5;` raises a deprecation, the handler writes `$x`, and PHP prints `ok`. The
    // argument check must not run on the stale `'a'`.
    for stmt in ["~1.5;", "['a' => 1]['b'];", "$zz ? 1 : 2;", "-$zz;"] {
        let src = format!(
            "<?php
declare(strict_types=1);
function takes_int(int $n): void {{ echo \"ok\"; }}
set_error_handler(function () {{ $GLOBALS['x'] = 5; return true; }});
$x = 'a';
{stmt}
takes_int($x);
"
        );
        let tree = SourceTree::parse(&src);
        let found: Vec<_> = check(&tree, &[], "t.php")
            .into_iter()
            .filter(|d| d.id == "type.argument-mismatch")
            .collect();
        assert!(found.is_empty(), "`{stmt}` may rebind `$x` through the handler: {found:?}");
    }
}

#[test]
fn a_statement_that_throws_does_not_vouch_for_the_return_after_it() {
    // `1/0;` throws, so the `return` never runs and there is no mismatch to report.
    let src = "<?php
declare(strict_types=1);
function e(): int { $s = \"a\"; 1 / 0; return $s; }
";
    assert_eq!(mismatches(src), 0);
}
