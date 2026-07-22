//! Folding integration at the inference layer, driven by a deterministic mock
//! [`Folder`] (no PHP needed). Real end-to-end folding through a live sidecar is
//! covered by the CLI tests; here we prove the engine's gate and provenance.

use steins_infer::{Diagnostic, Folder, NoFold, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A canned folder mimicking the allowlisted builtins the tests use.
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        match (name, args) {
            ("strtolower", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.to_lowercase())),
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.to_uppercase())),
            ("strval", [ArgValue::Int(i)]) => Some(ArgValue::Str(i.to_string())),
            _ => None,
        }
    }
}

fn find(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", folder)
}

const COERCIVE_INT: &str = "<?php function width(int $w): int { return $w; }\n";
const STRICT_INT: &str =
    "<?php\ndeclare(strict_types=1);\nfunction width(int $w): int { return $w; }\n";

#[test]
fn folds_builtin_in_argument_position() {
    // width(strtolower("ABC")) → "abc", a non-numeric string into int (coercive
    // TypeError).
    let f = find(&format!("{COERCIVE_INT}width(strtolower(\"ABC\"));"), &mut Mock);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].message,
        "argument \"abc\" (folded from strtolower(\"ABC\")) to width() cannot become int $w — proven TypeError (coercive mode)"
    );
}

#[test]
fn folds_builtin_on_assignment_rhs() {
    // $w = strtoupper("xy"); width($w);  → "XY" into int.
    let f = find(&format!("{COERCIVE_INT}$w = strtoupper(\"xy\");\nwidth($w);"), &mut Mock);
    assert_eq!(f.len(), 1, "got {f:#?}");
    // Chained through a variable → provenance is the immediate $w hop.
    assert!(f[0].message.contains("from $w, assigned at line"), "{}", f[0].message);
    assert!(f[0].message.contains("argument \"XY\""));
}

#[test]
fn non_literal_inner_arg_is_silent() {
    // width(strtolower($x)) — inner arg is a variable, not a literal, so the gate
    // never asks the folder.
    let src = format!("{COERCIVE_INT}$x = $_GET['x'];\nwidth(strtolower($x));");
    assert_eq!(find(&src, &mut Mock).len(), 0);
}

#[test]
fn strval_folds_strict_flagged_coercive_silent() {
    // strval(5) → "5". In strict, string→int is a TypeError; in coercive, "5" is
    // numeric and coerces silently.
    let strict = find(&format!("{STRICT_INT}width(strval(5));"), &mut Mock);
    assert_eq!(strict.len(), 1, "strict flags string→int: {strict:#?}");
    assert!(strict[0].message.contains("(folded from strval(5))"));
    assert!(strict[0].message.contains("(strict mode)"));

    let coercive = find(&format!("{COERCIVE_INT}width(strval(5));"), &mut Mock);
    assert_eq!(coercive.len(), 0, "coercive coerces numeric string: {coercive:#?}");
}

#[test]
fn nofold_is_silent_for_folded_findings() {
    // The sound subset never executes the fold, so a folded-only finding vanishes.
    let src = format!("{COERCIVE_INT}width(strtolower(\"ABC\"));");
    assert_eq!(find(&src, &mut NoFold).len(), 0, "NoFold widens the fold");
    // But `check` (== NoFold) still reports direct literals.
    let direct = "<?php function width(int $w): int { return $w; }\nwidth(\"abc\");";
    let tree = SourceTree::parse(direct);
    let funcs = tree.functions().to_vec();
    assert_eq!(check(&tree, &funcs, "d.php").len(), 1);
}

#[test]
fn user_function_named_like_builtin_is_not_folded() {
    // A same-file user function shadowing an allowlisted name must not be sent to
    // the sidecar. Here `strtolower` is user-defined and non-constant, so silent.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction strtolower(string $s): string { $x = 1; return $s; }\nwidth(strtolower(\"ABC\"));";
    // Mock would fold it to "abc" if asked — assert the gate prevents that.
    assert_eq!(find(src, &mut Mock).len(), 0, "user fn is not folded");
}
