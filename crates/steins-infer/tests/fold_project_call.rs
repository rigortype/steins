//! Issue #127 — a foldable builtin's project-call argument resolves through the
//! T0 summary, so `strtoupper(g(1))` folds once `g` proves a string.
//!
//! Project shadows, ambiguous simple names, conditional polyfills, and
//! non-Singleton summaries still decline.

use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, Folder, ID as ARG_MISMATCH_ID, RETURN_ID, check, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_uppercase().into())),
            ("strtolower", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_lowercase().into())),
            ("str_repeat", [ArgValue::Str(s), ArgValue::Int(n)]) => {
                Some(ArgValue::Str(s.as_str()?.repeat(usize::try_from(*n).ok()?).into()))
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

fn one_folded(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src, Some(&mut Mock))
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

fn one_type(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src, None)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

fn count(src: &str, id: &str, folder: Option<&mut dyn Folder>) -> usize {
    findings(src, folder).iter().filter(|d| d.id == id).count()
}

// Flagship

#[test]
fn strtoupper_of_project_call_folds() {
    // The gap the value IR documented: `strtoupper(g(1))` widened because the
    // fold gate only saw direct literals. With #127, `g(1)`'s Singleton summary
    // is a concrete fold argument.
    let src = "<?php\n\
        function g(int $t): string { return \"hi\"; }\n\
        $x = strtoupper(g(1));\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'HI'");
}

#[test]
fn strtoupper_of_project_call_folds_in_argument_position() {
    let src = "<?php\n\
        function g(int $t): string { return \"hi\"; }\n\
        \\PHPStan\\dumpType(strtoupper(g(1)));\n";
    assert_eq!(one_folded(src), "'HI'");
}

#[test]
fn nested_fold_over_project_call_composes() {
    // Outer builtin, inner project, no assignment detour.
    let src = "<?php\n\
        function g(int $t): string { return \"ab\"; }\n\
        $x = str_repeat(strtoupper(g(1)), 2);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'ABAB'");
}

// Refusals — silence, never a partial fold

#[test]
fn project_function_shadowing_the_builtin_declines() {
    // A same-named project function shadows the fold allowlist entirely.
    let src = "<?php\n\
        function strtoupper(string $s): string { return \"shadow\"; }\n\
        function g(int $t): string { return \"hi\"; }\n\
        $x = strtoupper(g(1));\n\
        \\PHPStan\\dumpType($x);\n";
    // Shadowed name: no fold. Summary of the project strtoupper may still bind
    // via the method/function summary path if it descends — here the body returns
    // a literal, so the summary can pin 'shadow'. Either way it must not invent
    // 'HI' from the builtin.
    let ty = one_type(src);
    assert_ne!(ty, "'HI'", "must not fold through a project-shadowed name: {ty}");
}

#[test]
fn non_singleton_project_summary_declines_the_fold() {
    // `g` returns opaque `(string) rand()` → no Singleton → fold gate declines.
    // The assignment then takes the builtin declared-return floor (uppercase-string
    // asserted), never invents a concrete folded string from a non-Singleton arg.
    let src = "<?php\n\
        function g(int $t): string { return (string) rand(); }\n\
        $x = strtoupper(g(1));\n\
        \\PHPStan\\dumpType($x);\n";
    let ty = one_folded(src);
    // Floor or abstract refinement — never a concrete folded string like `'…'`.
    assert!(
        !ty.starts_with('\''),
        "must not invent a folded string literal from a non-Singleton arg: {ty}"
    );
}

#[test]
fn zero_arg_project_call_stays_on_const_fn_lane() {
    // Zero-arg project calls are `resolve_const_fn`, not the T0 summary (T0 needs
    // a bound arg). Flagship still works for the empty-arg constant form.
    let src = "<?php\n\
        function hi(): string { return \"hi\"; }\n\
        $x = strtoupper(hi());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'HI'");
}

// Recursion through a fold arg terminates

/// Shared helpers for the Asserted-fold laundering fixtures (issue #127 review).
/// `g` asserts its second arg is `'hi'` and returns it; `strtoupper(g(...))` folds
/// to `'HI'` at Asserted — every proof-layer consumer must stay silent.
const ASSERTED_FOLD_PRELUDE: &str = "\
        /** @phpstan-assert 'hi' $v */\n\
        function claimHi($v): void {}\n\
        function g(int $trigger, $x): string {\n\
            claimHi($x);\n\
            return $x;\n\
        }\n\
        function takesInt(int $n): void {}\n";

#[test]
fn asserted_project_summary_fold_stays_asserted() {
    // Assignment path: fold result binds Asserted; env-read `takesInt($result)`
    // must not launder to Verified.
    let src = format!(
        "<?php\n{ASSERTED_FOLD_PRELUDE}\
        $result = strtoupper(g(1, (string) rand()));\n\
        \\PHPStan\\dumpType($result);\n\
        takesInt($result);\n"
    );
    assert_eq!(one_folded(&src), "'HI' (asserted)");
    assert_eq!(
        count(&src, ARG_MISMATCH_ID, Some(&mut Mock)),
        0,
        "Asserted fold result must not premise type.argument-mismatch"
    );
}

#[test]
fn asserted_fold_direct_free_function_argument_stays_silent() {
    // Direct argument position — no assignment detour. The argument checker must
    // use the fold's resolved stratum, not a syntactic re-read of the Call tree.
    let src = format!(
        "<?php\n{ASSERTED_FOLD_PRELUDE}\
        takesInt(strtoupper(g(1, (string) rand())));\n"
    );
    assert_eq!(
        count(&src, ARG_MISMATCH_ID, Some(&mut Mock)),
        0,
        "direct free-function arg: Asserted fold must not premise type.argument-mismatch"
    );
}

#[test]
fn asserted_fold_direct_method_argument_stays_silent() {
    // Method argument path — same stratum rule as free-function args.
    let src = format!(
        "<?php\n{ASSERTED_FOLD_PRELUDE}\
        class Sink {{\n\
            public function takesInt(int $n): void {{}}\n\
        }}\n\
        (new Sink)->takesInt(strtoupper(g(1, (string) rand())));\n"
    );
    assert_eq!(
        count(&src, ARG_MISMATCH_ID, Some(&mut Mock)),
        0,
        "direct method arg: Asserted fold must not premise type.argument-mismatch"
    );
}

#[test]
fn asserted_fold_return_position_stays_silent() {
    // Native return check must use the fold's resolved stratum.
    let src = format!(
        "<?php\n{ASSERTED_FOLD_PRELUDE}\
        function f(): int {{\n\
            return strtoupper(g(1, (string) rand()));\n\
        }}\n\
        f();\n"
    );
    assert_eq!(
        count(&src, RETURN_ID, Some(&mut Mock)),
        0,
        "return strtoupper(g(...)): Asserted fold must not premise type.return-mismatch"
    );
    assert_eq!(
        count(&src, ARG_MISMATCH_ID, Some(&mut Mock)),
        0,
        "no collateral argument mismatch either"
    );
}

#[test]
fn fold_arg_emits_binding_specific_finding() {
    // Issue #127 review (High): when `g(1)` is resolved only as a fold argument,
    // the nested descent must emit through the real findings sink — not a scratch
    // that discards binding-specific diagnostics plain walk cannot see.
    // Strict mode: int→string is a proven TypeError only under the binding `$x = 1`.
    let src = "<?php\n\
        declare(strict_types=1);\n\
        function takesString(string $s): void {}\n\
        function g(int $x): string {\n\
            takesString($x);\n\
            return 'hi';\n\
        }\n\
        $result = strtoupper(g(1));\n\
        \\PHPStan\\dumpType($result);\n";
    assert_eq!(one_folded(src), "'HI'");
    let ds = findings(src, Some(&mut Mock));
    let mismatches: Vec<&Diagnostic> =
        ds.iter().filter(|d| d.id == ARG_MISMATCH_ID).collect();
    assert_eq!(
        mismatches.len(),
        1,
        "binding-specific finding under fold arg must emit once: {mismatches:?}"
    );
    assert!(
        mismatches[0].message.contains("bound at"),
        "provenance should name the binding: {}",
        mismatches[0].message
    );
}

#[test]
fn mutual_recursion_through_fold_arg_terminates() {
    // a → strtoupper(b($n)) → b → strtoupper(a($n)): the on-stack guard (when
    // threaded) and depth bound keep this finite. Result is the arm floor or a
    // proven value — never a hang, never a wrong partial fold.
    let src = "<?php\n\
        function a(int $n): string {\n\
            if ($n <= 0) { return \"z\"; }\n\
            return strtoupper(b($n - 1));\n\
        }\n\
        function b(int $n): string {\n\
            if ($n <= 0) { return \"y\"; }\n\
            return strtolower(a($n - 1));\n\
        }\n\
        $x = a(1);\n\
        \\PHPStan\\dumpType($x);\n";
    let ty = one_folded(src);
    // Bounded: either a concrete folded string or the declared string floor.
    assert!(
        ty == "string" || ty.starts_with('\''),
        "sound + terminating: {ty}"
    );
}
