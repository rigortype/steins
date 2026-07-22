//! Acceptance tests for `type.argument-mismatch`, exercising the truth table
//! through the pure `check` core and (for the milestone) the salsa pipeline.

use steins_infer::{Diagnostic, check};
use steins_syntax::SourceTree;

/// Parse + check inline PHP, returning the findings.
fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

/// Count findings.
fn n(src: &str) -> usize {
    findings(src).len()
}

const COERCIVE_INT: &str = "<?php function width(int $w): int { return $w; }\n";
const STRICT_INT: &str =
    "<?php\ndeclare(strict_types=1);\nfunction width(int $w): int { return $w; }\n";
const STRICT_FLOAT: &str =
    "<?php\ndeclare(strict_types=1);\nfunction area(float $a): float { return $a; }\n";
const STRICT_STRING: &str =
    "<?php\ndeclare(strict_types=1);\nfunction name(string $s): string { return $s; }\n";
const STRICT_BOOL: &str =
    "<?php\ndeclare(strict_types=1);\nfunction flag(bool $b): bool { return $b; }\n";

// ---- Acceptance 1: coercive file, int parameter --------------------------

#[test]
fn coercive_int_param() {
    assert_eq!(n(&format!("{COERCIVE_INT}width(\"5\");")), 0, "numeric string coerces");
    assert_eq!(n(&format!("{COERCIVE_INT}width(\"5.5\");")), 0, "float-string coerces");
    assert_eq!(n(&format!("{COERCIVE_INT}width(\"  10  \");")), 0, "whitespace numeric coerces");
    assert_eq!(n(&format!("{COERCIVE_INT}width(\"abc\");")), 1, "non-numeric string errors");
    assert_eq!(n(&format!("{COERCIVE_INT}width(null);")), 1, "null to non-nullable errors");
    assert_eq!(n(&format!("{COERCIVE_INT}width(5);")), 0, "int ok");
    assert_eq!(n(&format!("{COERCIVE_INT}width(5.5);")), 0, "float->int silent (deprecation)");
    assert_eq!(n(&format!("{COERCIVE_INT}width(true);")), 0, "bool->int coerces");
}

// ---- Acceptance 2: strict file -------------------------------------------

#[test]
fn strict_int_param() {
    assert_eq!(n(&format!("{STRICT_INT}width(\"5\");")), 1, "string to int flagged in strict");
    assert_eq!(n(&format!("{STRICT_INT}width(5);")), 0, "int ok");
    assert_eq!(n(&format!("{STRICT_INT}width(5.0);")), 1, "float to int flagged in strict");
    assert_eq!(n(&format!("{STRICT_INT}width(true);")), 1, "bool to int flagged in strict");
    assert_eq!(n(&format!("{STRICT_INT}width(null);")), 1, "null to non-nullable flagged");
}

#[test]
fn strict_float_param() {
    assert_eq!(n(&format!("{STRICT_FLOAT}area(5);")), 0, "int->float widening allowed in strict");
    assert_eq!(n(&format!("{STRICT_FLOAT}area(5.0);")), 0, "float ok");
    assert_eq!(n(&format!("{STRICT_FLOAT}area(\"5\");")), 1, "string to float flagged in strict");
    assert_eq!(n(&format!("{STRICT_FLOAT}area(true);")), 1, "bool to float flagged in strict");
}

#[test]
fn strict_string_and_bool_params() {
    assert_eq!(n(&format!("{STRICT_STRING}name(\"x\");")), 0);
    assert_eq!(n(&format!("{STRICT_STRING}name(5);")), 1, "int to string flagged in strict");
    assert_eq!(n(&format!("{STRICT_BOOL}flag(true);")), 0);
    assert_eq!(n(&format!("{STRICT_BOOL}flag(1);")), 1, "int to bool flagged in strict");
}

// ---- Acceptance 3: nullable accepts null in both modes -------------------

#[test]
fn nullable_accepts_null_both_modes() {
    let coercive = "<?php function maybe(?int $n): ?int { return $n; }\nmaybe(null);";
    let strict =
        "<?php\ndeclare(strict_types=1);\nfunction maybe(?int $n): ?int { return $n; }\nmaybe(null);";
    assert_eq!(n(coercive), 0);
    assert_eq!(n(strict), 0);
    // A non-null mismatch is still caught for a nullable param under strict.
    let strict_bad =
        "<?php\ndeclare(strict_types=1);\nfunction maybe(?int $n): ?int { return $n; }\nmaybe(\"x\");";
    assert_eq!(n(strict_bad), 1);
}

// ---- Acceptance 4: non-literal args and unknown callees are silent -------

#[test]
fn non_literal_and_unknown_are_silent() {
    // A value from an unknown source (a function not defined in this file) is
    // not provable, so it stays silent even under strict mode.
    assert_eq!(n(&format!("{STRICT_INT}$x = getInput();\nwidth($x);")), 0, "unknown source silent");
    assert_eq!(n(&format!("{STRICT_INT}unknownFunc(\"abc\");")), 0, "unknown fn silent");
    assert_eq!(n("<?php strlen(\"abc\");"), 0, "builtin not in file silent");
    // spread / named args are skipped (positional mapping unreliable).
    assert_eq!(n(&format!("{STRICT_INT}$a=[1];\nwidth(...$a);")), 0, "spread silent");
}

// ---- Acceptance 5: parse error does not panic, no false diagnostic -------

#[test]
fn parse_error_is_safe() {
    let broken = "<?php\nfunction width(int $w): int { return $w;\nfunction broken( int $x {\nwidth(123);";
    let f = findings(broken);
    assert!(f.iter().all(|d| d.id == "type.argument-mismatch"));
    // width(123) is a valid int->int call, so there must be no finding at all.
    assert_eq!(f.len(), 0, "no false positive from a broken file");
}

// ---- Message shape (matches the ADR-0022 spirit) -------------------------

#[test]
fn message_is_value_precise() {
    let src = "<?php\n\nfunction width(int $w): int {\n    return $w;\n}\n\nwidth(\"abc\");\n";
    let f = findings(src);
    assert_eq!(f.len(), 1);
    let d = &f[0];
    assert_eq!(d.id, "type.argument-mismatch");
    assert_eq!((d.line, d.column), (7, 7), "points at the argument literal");
    assert_eq!(
        d.message,
        "argument \"abc\" to width() cannot become int $w — proven TypeError (coercive mode)"
    );
}

// ==========================================================================
// ADR-0001 value propagation: local-variable flow.
// ==========================================================================

/// Return the single finding, asserting there is exactly one.
fn only(src: &str) -> Diagnostic {
    let f = findings(src);
    assert_eq!(f.len(), 1, "expected exactly one finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

#[test]
fn var_flow_flagged_coercive_and_strict() {
    // Coercive: non-numeric string into int is a proven TypeError.
    assert_eq!(n(&format!("{COERCIVE_INT}$w = \"abc\";\nwidth($w);")), 1, "coercive abc via $w");
    // Strict: even a numeric string is rejected.
    assert_eq!(n(&format!("{STRICT_INT}$w = \"5\";\nwidth($w);")), 1, "strict 5 via $w");
    // Coercive numeric string still coerces silently through a variable.
    assert_eq!(n(&format!("{COERCIVE_INT}$w = \"5\";\nwidth($w);")), 0, "coercive 5 via $w silent");
}

#[test]
fn var_flow_message_shows_value_and_provenance() {
    let src = "<?php\n\nfunction width(int $w): int {\n    return $w;\n}\n\n$w = \"abc\";\nwidth($w);\n";
    let d = only(src);
    // `$w = "abc"` is on line 7; `width($w)` is on line 8, column 7.
    assert_eq!((d.line, d.column), (8, 7), "points at the argument $w");
    assert_eq!(
        d.message,
        "argument \"abc\" (from $w, assigned at line 7) to width() cannot become int $w — proven TypeError (coercive mode)"
    );
}

#[test]
fn reassignment_uses_last_literal() {
    // A later literal assignment replaces the value; the last one wins.
    assert_eq!(
        n(&format!("{COERCIVE_INT}$w = 5;\n$w = \"abc\";\nwidth($w);")),
        1,
        "last literal (bad) is used"
    );
    assert_eq!(
        n(&format!("{COERCIVE_INT}$w = \"abc\";\n$w = 5;\nwidth($w);")),
        0,
        "last literal (good) is used"
    );
}

// ==========================================================================
// ADR-0027 Feature A: write-set `Opaque` refinement of control-flow barriers.
//
// A control-flow construct no longer erases the *whole* env — it forgets only
// the variables it might write. So a value survives an intervening construct
// that does not touch it, and is forgotten by one that does.
// ==========================================================================

#[test]
fn construct_writing_var_forgets_it() {
    // (Refinement of the former "intervening if → silent" test.) An `if` that
    // *writes* `$w` makes it unknown at the later use → silent. This preserves
    // the original intent: a construct that could have changed `$w` is not
    // second-guessed.
    let src = format!("{COERCIVE_INT}$w = \"abc\";\nif ($cond) {{ $w = 5; }}\nwidth($w);");
    assert_eq!(n(&src), 0, "if writes $w → forgotten → silent");
}

#[test]
fn irrelevant_construct_preserves_var() {
    // The surviving case: an `if` that does not write `$w` (only calls a
    // side-effecting helper and writes an unrelated `$y`) leaves `$w` known, so
    // the proven TypeError at `width($w)` is now FLAGGED — where the old blanket
    // Barrier would have gone silent.
    let src = format!(
        "{COERCIVE_INT}function log_it(): void {{}}\n$w = \"abc\";\nif ($cond) {{ log_it(); $y = 1; }}\nwidth($w);"
    );
    let d = only(&src);
    assert!(d.message.contains("argument \"abc\""), "{}", d.message);
    assert!(d.message.contains("from $w"), "{}", d.message);
}

#[test]
fn loop_not_writing_var_survives() {
    // A loop whose body does not write `$w` leaves it known → flagged.
    let src = format!("{COERCIVE_INT}$w = \"abc\";\nwhile ($cond) {{ $y = 1; }}\nwidth($w);");
    assert_eq!(n(&src), 1, "loop not writing $w → survives → flagged");
    // foreach binding an unrelated value likewise preserves `$w`.
    let each = format!("{COERCIVE_INT}$w = \"abc\";\nforeach ($items as $it) {{ echo $it; }}\nwidth($w);");
    assert_eq!(n(&each), 1, "foreach not writing $w → survives");
}

#[test]
fn loop_writing_var_becomes_unknown() {
    // A `for` loop that assigns `$w` in its body forgets it → silent.
    let src = format!("{COERCIVE_INT}$w = \"abc\";\nfor ($i = 0; $i < 3; $i++) {{ $w = $i; }}\nwidth($w);");
    assert_eq!(n(&src), 0, "loop writing $w → unknown → silent");
    // A `foreach` binding *into* `$w` (as the value target) also forgets it.
    let each = format!("{COERCIVE_INT}$w = \"abc\";\nforeach ($items as $w) {{ echo $w; }}\nwidth($w);");
    assert_eq!(n(&each), 0, "foreach binding $w → unknown");
}

#[test]
fn try_catch_forgets_only_catch_param() {
    // A `try`/`catch` whose body touches neither `$w` nor the catch var leaves
    // `$w` known → flagged; the catch parameter `$e` is in the write set but is
    // irrelevant to `$w`.
    let src = format!(
        "{COERCIVE_INT}$w = \"abc\";\ntry {{ echo 1; }} catch (\\Throwable $e) {{ echo 2; }}\nwidth($w);"
    );
    assert_eq!(n(&src), 1, "try/catch not writing $w → $w survives");
    // But if the catch parameter *is* `$w`, the construct forgets it → silent.
    let clobber = format!(
        "{COERCIVE_INT}$w = \"abc\";\ntry {{ risky(); }} catch (\\Throwable $w) {{ echo 2; }}\nwidth($w);"
    );
    assert_eq!(n(&clobber), 0, "catch (... $w) → $w forgotten");
}

#[test]
fn variable_written_via_call_in_construct_becomes_unknown() {
    // `$w` handed to a call *inside* the construct is by-ref-conservatively part
    // of the write set → forgotten (a callee could mutate it by reference).
    let src = format!(
        "{COERCIVE_INT}function sink($x): void {{}}\n$w = \"abc\";\nif ($cond) {{ sink($w); }}\nwidth($w);"
    );
    assert_eq!(n(&src), 0, "$w passed to a call inside the if → forgotten");
}

#[test]
fn poison_inside_construct_still_poisons() {
    // A poison marker anywhere in a construct's subtree poisons the whole scope,
    // exactly as before — the write-set refinement never weakens poisoning.
    let global = format!("{COERCIVE_INT}$w = \"abc\";\nif ($cond) {{ global $g; }}\nwidth($w);");
    assert_eq!(n(&global), 0, "global inside if → scope poisoned → silent");
    let byref = format!(
        "{COERCIVE_INT}$w = \"abc\";\nif ($cond) {{ $f = function () use (&$w) {{}}; }}\nwidth($w);"
    );
    assert_eq!(n(&byref), 0, "by-ref use inside if → scope poisoned → silent");
    let extract = format!("{COERCIVE_INT}$w = \"abc\";\nwhile ($cond) {{ extract($d); }}\nwidth($w);");
    assert_eq!(n(&extract), 0, "extract inside loop → scope poisoned → silent");
}

#[test]
fn variable_passed_to_another_call_becomes_unknown() {
    // `$w` handed to `sink()` might be mutated by-ref, so its value is no longer
    // trusted at the later `width($w)`.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction sink($x) { return $x; }\n$w = \"abc\";\nsink($w);\nwidth($w);";
    assert_eq!(n(src), 0, "$w passed to a call → unknown afterwards");
}

#[test]
fn reference_poisoned_scope_is_silent() {
    // A reference assignment anywhere in the scope poisons all local values.
    let src = format!("{COERCIVE_INT}$w = \"abc\";\n$r = &$w;\nwidth($w);");
    assert_eq!(n(&src), 0, "reference assignment poisons the scope");
}

#[test]
fn extract_poisoned_scope_is_silent() {
    let src = format!("{COERCIVE_INT}$w = \"abc\";\nextract($data);\nwidth($w);");
    assert_eq!(n(&src), 0, "extract() poisons the scope");
}

// ==========================================================================
// ADR-0001 value propagation: constant-function return flow.
// ==========================================================================

const CONST_PRICE: &str =
    "<?php\nfunction width(int $w): int { return $w; }\nfunction price(): string { return \"abc\"; }\n";

#[test]
fn constant_function_flow_flagged() {
    // `width(price())` where price() is a constant function returning a bad
    // literal is a proven TypeError.
    let d = only(&format!("{CONST_PRICE}width(price());"));
    assert!(
        d.message.contains("from price(), defined at line 3"),
        "provenance names the const function: {}",
        d.message
    );
    assert!(d.message.contains("argument \"abc\""));
}

#[test]
fn non_constant_functions_are_silent() {
    // Two statements in the body → not constant.
    let two = "<?php\nfunction width(int $w): int { return $w; }\nfunction price(): string { $x = 1; return \"abc\"; }\nwidth(price());";
    assert_eq!(n(two), 0, "two-statement body is not constant");
    // Has a parameter → only zero-arg calls qualify, and it isn't constant.
    let params = "<?php\nfunction width(int $w): int { return $w; }\nfunction price(string $s): string { return \"abc\"; }\nwidth(price(\"x\"));";
    assert_eq!(n(params), 0, "parametrized function is not a constant function");
    // Has a branch → body is a Barrier, not `[Return(literal)]`.
    let branch = "<?php\nfunction width(int $w): int { return $w; }\nfunction price(): string { if (true) { return \"a\"; } return \"abc\"; }\nwidth(price());";
    assert_eq!(n(branch), 0, "branching function is not constant");
}

#[test]
fn nested_and_chained_constant_function_flow() {
    // Nested: `width(price())`.
    assert_eq!(n(&format!("{CONST_PRICE}width(price());")), 1, "nested width(price())");
    // Chained through a variable: `$w = price(); width($w);`.
    let chain = format!("{CONST_PRICE}$w = price();\nwidth($w);");
    let d = only(&chain);
    assert!(
        d.message.contains("from $w, assigned at line 4"),
        "chain reports the immediate $w hop: {}",
        d.message
    );
}

#[test]
fn constant_function_composes_in_strict_mode() {
    // Strict mode: a constant function returning a numeric string still fails.
    let src = "<?php\ndeclare(strict_types=1);\nfunction width(int $w): int { return $w; }\nfunction price(): string { return \"5\"; }\nwidth(price());";
    assert_eq!(n(src), 1, "strict: numeric-string const return into int flagged");
}

// ---- Salsa pipeline routing ----------------------------------------------

#[test]
fn routes_through_salsa_and_memoizes() {
    use steins_db::{SourceFile, SteinsDatabase};
    let db = SteinsDatabase::default();
    let src = "<?php function width(int $w): int { return $w; }\nwidth(\"abc\");".to_owned();
    let file = SourceFile::new(&db, "mem.php".to_owned(), src);
    let first = steins_infer::diagnostics(&db, file).clone();
    let second = steins_infer::diagnostics(&db, file).clone();
    assert_eq!(first.len(), 1);
    assert_eq!(first, second, "query is stable/memoized");
    assert_eq!(first[0].path, "mem.php");
}
