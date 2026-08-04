//! Issue #128 — closure return lane: native return checking + T0 summary rebind
//! for `$fn(...)` invocations.
//!
//! `ScopeOwner::Closure` answers `scope_return` from `Scope::ret_ty`, and a
//! proven-closure `$fn(args)` rebinds its summary like a free function. Capture
//! snapshots and binding descent follow ADR-0033.

use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, Folder, ID as ARG_MISMATCH_ID, RETURN_ID, check, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.to_uppercase())),
            ("str_repeat", [ArgValue::Str(s), ArgValue::Int(n)]) => {
                Some(ArgValue::Str(s.repeat(usize::try_from(*n).ok()?)))
            }
            _ => None,
        }
    }
}

fn findings(src: &str, folder: Option<&mut dyn Folder>) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    match folder {
        Some(f) => check_with(&tree, &functions, "test.php", f),
        None => check(&tree, &functions, "test.php"),
    }
}

fn one_type(src: &str) -> String {
    let ds: Vec<_> = findings(src, None)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one debug.type, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

fn one_folded(src: &str) -> String {
    let ds: Vec<_> = findings(src, Some(&mut Mock))
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one debug.type, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

fn count(src: &str, id: &str) -> usize {
    findings(src, None).iter().filter(|d| d.id == id).count()
}

// (a) Conformance — closure return sites against native `: R`

#[test]
fn closure_native_return_mismatch_fires() {
    // Arrow body is a `return` of the expression; `: int` vs `"hi"` is a proven
    // TypeError at the closure definition scope.
    let src = "<?php\n\
        $f = fn(): int => \"hi\";\n";
    assert_eq!(count(src, RETURN_ID), 1, "native return mismatch on the arrow body");
}

#[test]
fn closure_native_return_good_is_silent() {
    let src = "<?php\n\
        $f = fn(): int => 42;\n";
    assert_eq!(count(src, RETURN_ID), 0);
}

#[test]
fn block_closure_return_mismatch_fires() {
    let src = "<?php\n\
        $f = function (): int {\n\
            return \"hi\";\n\
        };\n";
    assert_eq!(count(src, RETURN_ID), 1);
}

#[test]
fn generator_closure_return_is_not_checked_against_generator() {
    // `: Generator` names the *call* result. In-body `return 7` is getReturn()'s
    // value and must not fire type.return-mismatch (issue #128 review).
    let src = "<?php\n\
        $f = function (): Generator {\n\
            yield 1;\n\
            return 7;\n\
        };\n";
    assert_eq!(count(src, RETURN_ID), 0, "generator body return is not a Generator value");
}

#[test]
fn generator_function_return_is_not_checked_against_generator() {
    let src = "<?php\n\
        function gen(): Generator {\n\
            yield 1;\n\
            return 7;\n\
        }\n";
    assert_eq!(count(src, RETURN_ID), 0);
}

// (b) Value lane — `$fn(...)` summary rebinds at assignment

#[test]
fn closure_call_summary_rebinds_literal() {
    let src = "<?php\n\
        $f = fn(int $x): int => $x;\n\
        $y = $f(42);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "42");
}

#[test]
fn flagship_closure_greet_inlines_to_its_value() {
    // Method/function twin of the greet flagship, as a locally bound closure.
    let src = "<?php\n\
        $greet = fn(int $times, string $name): string =>\n\
            str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
        $x = $greet(2, \"World\");\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}

#[test]
fn closure_call_factless_falls_to_declared_floor() {
    let src = "<?php\n\
        $f = fn(int $x): int => rand();\n\
        $y = $f(1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

#[test]
fn named_arg_closure_call_keeps_declared_floor() {
    // Named args refuse binding descent (positional map), but the declared
    // return arms must still seed the floor — same rung as free functions.
    let src = "<?php\n\
        $f = fn(int $x): int => rand();\n\
        $y = $f(x: 1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

#[test]
fn first_class_callable_summary_rebinds() {
    // `$fn = pick(...); $fn(1)` resolves as a named free function through the
    // proven ClosureVal::Named path.
    let src = "<?php\n\
        function pick(int $x): int { return 42; }\n\
        $fn = pick(...);\n\
        $y = $fn(1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "42");
}

#[test]
fn string_callable_summary_rebinds() {
    let src = "<?php\n\
        function pick(int $x): int { return 7; }\n\
        $fn = \"pick\";\n\
        $y = $fn(1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "7");
}

#[test]
fn named_arg_string_callable_keeps_declared_floor() {
    // A string-callable with named/spread args keeps the declared return floor,
    // like local closures and first-class callables — not falling to `unknown`.
    let src = "<?php\n\
        function pick(int $x): int { return rand(); }\n\
        $fn = 'pick';\n\
        $y = $fn(x: 1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

#[test]
fn named_arg_first_class_callable_keeps_declared_floor() {
    let src = "<?php\n\
        function pick(int $x): int { return rand(); }\n\
        $fn = pick(...);\n\
        $y = $fn(x: 1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

// Capture stratum — Asserted must not launder through the summary (issue #128)

#[test]
fn asserted_capture_summary_stays_asserted() {
    // Regression: a capture snapshot must preserve stratum. If it re-seeds as
    // Verified, `$result = $f()` launders an Asserted 'hi' into a proof premise.
    let src = "<?php\n\
        /** @phpstan-assert 'hi' $v */\n\
        function claimHi($v): void {}\n\
        function takesInt(int $n): void {}\n\
        $x = (string) rand();\n\
        claimHi($x);\n\
        $f = function () use ($x): string {\n\
            return $x;\n\
        };\n\
        $result = $f();\n\
        \\PHPStan\\dumpType($result);\n\
        takesInt($result);\n";
    assert_eq!(one_type(src), "'hi' (asserted)");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "Asserted capture summary must not premise type.argument-mismatch"
    );
}

#[test]
fn memo_does_not_launder_asserted_after_verified_same_value() {
    // Issue #128 review: BindingKey without stratum collides Verified `$f('hi')`
    // with Asserted `$f($u)` (same Singleton value) inside one outer descent —
    // memo replay would hand a Verified summary to the Asserted call and premise
    // a proof finding. Stratum is part of the key so the second call re-walks.
    let src = "<?php\n\
        /** @phpstan-assert 'hi' $v */\n\
        function claimHi($v): void {}\n\
        function takesInt(int $n): void {}\n\
        function outer(int $trigger, $u): string {\n\
            $f = fn(string $x): string => $x;\n\
            $a = $f('hi');\n\
            claimHi($u);\n\
            $b = $f($u);\n\
            return $b;\n\
        }\n\
        $result = outer(1, (string) rand());\n\
        \\PHPStan\\dumpType($result);\n\
        takesInt($result);\n";
    assert_eq!(one_type(src), "'hi' (asserted)");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "Verified memo must not launder Asserted same-value call into a proof premise"
    );
}

#[test]
fn memo_does_not_launder_asserted_capture_after_verified_same_value() {
    // The parameter-stratum fixture above does not by itself prove that
    // `use:{name}` entries carry stratum too. Two walks of `wrap` share one memo
    // and instantiate the same closure definition with equal capture values but
    // different trust; omitting capture stratum would replay the first summary.
    let src = "<?php\n\
        /** @phpstan-assert 'hi' $v */\n\
        function claimHi($v): void {}\n\
        function takesInt(int $n): void {}\n\
        function wrap(string $x): string {\n\
            $f = function () use ($x): string { return $x; };\n\
            return $f();\n\
        }\n\
        function outer(int $trigger, $u): string {\n\
            $a = wrap('hi');\n\
            claimHi($u);\n\
            $b = wrap($u);\n\
            return $b;\n\
        }\n\
        $result = outer(1, (string) rand());\n\
        \\PHPStan\\dumpType($result);\n\
        takesInt($result);\n";
    assert_eq!(one_type(src), "'hi' (asserted)");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "Verified capture memo must not launder an Asserted same-value capture"
    );
}
