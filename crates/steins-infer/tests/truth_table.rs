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
    assert_eq!(n(&format!("{STRICT_INT}$x = \"abc\";\nwidth($x);")), 0, "non-literal silent");
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
