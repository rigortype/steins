//! ADR-0075 — a method/static call's return summary rebinds on the T0 rungs.
//!
//! The function leg of call-site value reflection is covered by `return_summary.rs`
//! and the `concat.rs` flagship. This file pins the method twin: the walk already
//! descends into a resolved method body, and the summary that descent produces is
//! now consumed at `apply_assign` (and return composition) exactly as a function's
//! is. Value/argument-position method calls and constructors stay out of scope.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// Canned folder for the two allowlisted builtins the greeter flagship needs.
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

fn types(src: &str) -> Vec<String> {
    findings(src, None)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

fn one_type(src: &str) -> String {
    let ts = types(src);
    assert_eq!(ts.len(), 1, "expected exactly one debug.type dump, got {ts:?}");
    ts.into_iter().next().expect("one dump")
}

fn one_folded(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src, Some(&mut Mock))
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

// ==========================================================================
// Flagship — the function twin across the receiver seam.
// ==========================================================================

#[test]
fn flagship_method_greet_inlines_to_its_value() {
    // `$g = new Greeter(); $x = $g->greet(2, "World")` — the method body walks,
    // proves the string, and the summary rebinds at the assignment (ADR-0075).
    let src = "<?php\n\
        final class Greeter {\n\
            public function greet(int $times, string $name): string {\n\
                return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
            }\n\
        }\n\
        $g = new Greeter();\n\
        $x = $g->greet(2, \"World\");\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}

#[test]
fn flagship_static_greet_inlines_to_its_value() {
    let src = "<?php\n\
        final class Greeter {\n\
            public static function greet(int $times, string $name): string {\n\
                return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
            }\n\
        }\n\
        $x = Greeter::greet(2, \"World\");\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}

#[test]
fn method_literal_return_agrees_with_function_twin() {
    // A one-arg method with a literal return: the T0 summary produces `Singleton`,
    // matching the free-function path in `return_summary.rs`.
    let via_method = "<?php\n\
        final class C {\n\
            public function pick(int $x): int { return 42; }\n\
        }\n\
        $a = (new C())->pick(1);\n\
        \\PHPStan\\dumpType($a);\n";
    let via_function = "<?php\n\
        function pick(int $x): int { return 42; }\n\
        $a = pick(1);\n\
        \\PHPStan\\dumpType($a);\n";
    assert_eq!(one_type(via_method), "42");
    assert_eq!(one_type(via_method), one_type(via_function), "method and function paths agree");
}

// ==========================================================================
// Positive-int proof crosses the method boundary (return_summary flagship twin).
// ==========================================================================

#[test]
fn method_positive_int_crosses_verified() {
    let src = "<?php\n\
        final class C {\n\
            public function f(int $trigger, int $n): int {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
        }\n\
        $x = (new C())->f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "int<1, max>");
}

// ==========================================================================
// Inheritance: the `this:` key component keeps Sub1/Sub2 summaries distinct.
// ==========================================================================

#[test]
fn inherited_body_does_not_replay_across_receivers() {
    // `Base::m` is the declaring key for both exact receivers. Inside the body,
    // `$this->tag($x)` dispatches under `this_exact`. Without a `this:` component on
    // the binding key, the first call's summary would replay for the second.
    // (Zero-arg callees do not descend in T0, so `tag` takes a positional arg.)
    let src = "<?php\n\
        class Base {\n\
            public function m(int $x): string {\n\
                return $this->tag($x);\n\
            }\n\
            public function tag(int $x): string { return \"?\"; }\n\
        }\n\
        final class Sub1 extends Base {\n\
            public function tag(int $x): string { return \"A\"; }\n\
        }\n\
        final class Sub2 extends Base {\n\
            public function tag(int $x): string { return \"B\"; }\n\
        }\n\
        $a = (new Sub1())->m(1);\n\
        $b = (new Sub2())->m(1);\n\
        \\PHPStan\\dumpType($a);\n\
        \\PHPStan\\dumpType($b);\n";
    assert_eq!(types(src), vec!["'A'".to_owned(), "'B'".to_owned()]);
}

// ==========================================================================
// Silences: overridable / unknown receivers stay on the arm floor.
// ==========================================================================

#[test]
fn exact_receiver_dispatches_inherited_override() {
    // `(new Sub())->call(1)` resolves `Base::call` with `this_exact = Sub`, so the
    // inner `$this->m` hits `Sub::m` and rebinds 99 — the `this:` key component
    // keeps this distinct from a bare `Base` walk.
    let src = "<?php\n\
        class Base {\n\
            public function m(int $x): int { return $x; }\n\
            public function call(int $x): int {\n\
                return $this->m($x);\n\
            }\n\
        }\n\
        final class Sub extends Base {\n\
            public function m(int $x): int { return 99; }\n\
        }\n\
        $x = (new Sub())->call(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "99");
}

#[test]
fn unknown_receiver_assignment_is_unknown() {
    // Parameter receiver → no exact class → resolve_call_target declines → no summary.
    let src = "<?php\n\
        final class C {\n\
            public function m(int $x): int { return $x; }\n\
        }\n\
        function go($c): void {\n\
            $x = $c->m(1);\n\
            \\PHPStan\\dumpType($x);\n\
        }\n";
    assert_eq!(one_type(src), "unknown");
}

#[test]
fn constructor_assignment_stays_on_exactness_lane() {
    // `$x = new C(1)` is the ADR-0036 object lane, not a value summary of
    // `__construct`. The dump reports the class, never a constructor return pin.
    let src = "<?php\n\
        final class C {\n\
            public function __construct(int $n) {}\n\
        }\n\
        $x = new C(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "C");
}

// ==========================================================================
// Return composition: `return $o->m(...)` crosses into an outer summary.
// ==========================================================================

#[test]
fn method_summary_composes_through_function_return() {
    let src = "<?php\n\
        final class C {\n\
            public function g(int $trigger, int $n): int {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
        }\n\
        function f(int $t): int {\n\
            return (new C())->g(1, rand());\n\
        }\n\
        $x = f(9);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "int<1, max>");
}
