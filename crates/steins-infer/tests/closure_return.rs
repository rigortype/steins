//! Issue #128 — closure return lane: native return checking + T0 summary rebind
//! for `$fn(...)` invocations.
//!
//! Closures already had capture snapshots and binding descent (ADR-0033). This
//! slice fills the return side: `ScopeOwner::Closure` answers `scope_return` from
//! `Scope::ret_ty`, and a proven-closure `$fn(args)` rebinds its summary like a
//! free function.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, RETURN_ID, check, check_with};
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

// ==========================================================================
// (a) Conformance — closure return sites against native `: R`
// ==========================================================================

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

// ==========================================================================
// (b) Value lane — `$fn(...)` summary rebinds at assignment
// ==========================================================================

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
