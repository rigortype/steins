//! Acceptance tests for native union/nullable parameter types and native
//! return-type checking (`type.return-mismatch`).
//!
//! The union-coercion cells exercised here were settled empirically against
//! PHP 8.5.8 (see the `is_type_error` rustdoc for the reproduction snippets):
//! e.g. `1.5` into `int|string` *coerces* (silent) in coercive mode but is a
//! `TypeError` in strict mode, `"abc"` into `int|float` fails in both modes, and
//! `false` into `string|false` is always fine.

use steins_infer::{Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// Parse + check inline PHP (coercive/strict decided by the file itself).
fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn n(src: &str) -> usize {
    findings(src).len()
}

// ==========================================================================
// 1. Native union / nullable parameter types.
// ==========================================================================

#[test]
fn union_param_coercive_cells() {
    // 1.5 -> int|string: the string sink accepts (becomes int 1) → silent.
    let f = "<?php function f(int|string $v): void {}\n";
    assert_eq!(n(&format!("{f}f(1.5);")), 0, "1.5 -> int|string coercive silent");
    // "abc" -> int|float: no string sink, non-numeric string → flagged.
    let g = "<?php function g(int|float $v): void {}\n";
    assert_eq!(n(&format!("{g}g(\"abc\");")), 1, "abc -> int|float coercive flagged");
    // "5" -> int|float: numeric string coerces → silent.
    assert_eq!(n(&format!("{g}g(\"5\");")), 0, "5 -> int|float coercive silent");
    // false -> string|false: matches the `false` literal member → silent.
    let h = "<?php function h(string|false $v): void {}\n";
    assert_eq!(n(&format!("{h}h(false);")), 0, "false -> string|false silent");
    // true -> string|false: coerces to '1' via the string member → silent.
    assert_eq!(n(&format!("{h}h(true);")), 0, "true -> string|false coercive silent");
    // null -> int|string (non-nullable) → flagged.
    assert_eq!(n(&format!("{f}f(null);")), 1, "null -> int|string flagged");
    // null -> int|null: nullable → silent.
    let k = "<?php function k(int|null $v): void {}\n";
    assert_eq!(n(&format!("{k}k(null);")), 0, "null -> int|null silent");
    // "abc" -> int|false: the `false` literal does not sink strings → flagged.
    let m = "<?php function m(int|false $v): void {}\n";
    assert_eq!(n(&format!("{m}m(\"abc\");")), 1, "abc -> int|false coercive flagged");
    // true -> int|false: bool coerces via the int member → silent.
    assert_eq!(n(&format!("{m}m(true);")), 0, "true -> int|false coercive silent");
}

#[test]
fn union_param_strict_cells() {
    // The conformance near-win: 1.5 (float) into int|string strict is a TypeError.
    let f = "<?php\ndeclare(strict_types=1);\nfunction f(int|string $v): void {}\n";
    assert_eq!(n(&format!("{f}f(1.5);")), 1, "1.5 -> int|string strict flagged");
    assert_eq!(n(&format!("{f}f(5);")), 0, "int matches member");
    assert_eq!(n(&format!("{f}f(\"x\");")), 0, "string matches member");
    // bool has no member (and no matching bool-literal) → flagged.
    assert_eq!(n(&format!("{f}f(true);")), 1, "true -> int|string strict flagged");

    // int|float strict: int OK, float OK, numeric string still flagged.
    let g = "<?php\ndeclare(strict_types=1);\nfunction g(int|float $v): void {}\n";
    assert_eq!(n(&format!("{g}g(5);")), 0, "int -> int|float strict OK");
    assert_eq!(n(&format!("{g}g(5.0);")), 0, "float -> int|float strict OK");
    assert_eq!(n(&format!("{g}g(\"5\");")), 1, "numeric string -> int|float strict flagged");

    // string|false strict: false matches literal; true/int do not.
    let h = "<?php\ndeclare(strict_types=1);\nfunction h(string|false $v): void {}\n";
    assert_eq!(n(&format!("{h}h(false);")), 0, "false matches literal member");
    assert_eq!(n(&format!("{h}h(true);")), 1, "true not a member strict");
    assert_eq!(n(&format!("{h}h(\"x\");")), 0, "string matches member");
    assert_eq!(n(&format!("{h}h(5);")), 1, "int no member strict");
}

#[test]
fn abc_into_int_float_flagged_both_modes() {
    let coercive = "<?php function g(int|float $v): void {}\n";
    let strict = "<?php\ndeclare(strict_types=1);\nfunction g(int|float $v): void {}\n";
    assert_eq!(n(&format!("{coercive}g(\"abc\");")), 1, "coercive");
    assert_eq!(n(&format!("{strict}g(\"abc\");")), 1, "strict");
}

#[test]
fn nullable_union_null_member() {
    // null OK only when a `null` member / `?` is present.
    let strict = "<?php\ndeclare(strict_types=1);\n";
    assert_eq!(
        n(&format!("{strict}function f(int|string $v): void {{}}\nf(null);")),
        1,
        "null -> int|string flagged"
    );
    assert_eq!(
        n(&format!("{strict}function f(int|string|null $v): void {{}}\nf(null);")),
        0,
        "null -> int|string|null silent"
    );
}

#[test]
fn union_message_renders_all_members() {
    let src = "<?php\ndeclare(strict_types=1);\nfunction f(int|string $v): void {}\nf(1.5);\n";
    let f = findings(src);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].id, "type.argument-mismatch");
    assert_eq!(
        f[0].message,
        "argument 1.5 to f() cannot become int|string $v — proven TypeError (strict mode)"
    );
}

#[test]
fn unmodeled_union_member_silences_whole_type() {
    // A union containing an `array` / `mixed` / `callable` / `iterable` member (none
    // of which lower to a `TypeMember`) still lowers the WHOLE type to `None` →
    // silence (zero-FP): no finding even for an obvious mismatch.
    for ty in ["int|array", "int|mixed", "int|callable", "iterable"] {
        let src = format!("<?php\ndeclare(strict_types=1);\nfunction f({ty} $v): void {{}}\nf(1.5);\n");
        assert_eq!(n(&src), 0, "type `{ty}` must lower to silence");
    }
    // An intersection anywhere collapses the type too (intersections are unlowered).
    let src = "<?php\ndeclare(strict_types=1);\nfunction f(int|(A&B) $v): void {}\nf(1.5);\n";
    assert_eq!(n(src), 0, "intersection member silences the type");
}

#[test]
fn object_union_member_is_now_modeled_adr0043_stage3() {
    // ADR-0043 stage 3: an `int|\Foo` union lowers to `[Int, Instance(foo)]` — no
    // longer silenced. A `1.5` (float) matches neither `int` (no float→int in
    // strict) nor `\Foo` (a scalar is never an object) → a proven TypeError.
    // Verified against php 8.5.8: `f(int|Foo $v); f(1.5)` strict → TypeError.
    let src = "<?php\ndeclare(strict_types=1);\nfinal class Foo {}\nfunction f(int|\\Foo $v): void {}\nf(1.5);\n";
    let d = findings(src);
    assert_eq!(d.len(), 1, "1.5 vs int|Foo strict is a proven TypeError");
    assert_eq!(d[0].id, "type.argument-mismatch");
}

// ==========================================================================
// 2. Native return-type checking (`type.return-mismatch`).
// ==========================================================================

#[test]
fn return_strict_abc_into_int_flagged() {
    let src = "<?php\ndeclare(strict_types=1);\nfunction f(): int { return \"abc\"; }\n";
    let f = findings(src);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].id, "type.return-mismatch");
    assert_eq!(
        f[0].message,
        "return \"abc\" cannot become int (return type of f()) — proven TypeError (strict mode)"
    );
}

#[test]
fn return_coercive_numeric_vs_nonnumeric() {
    // "5" -> int coercive: coerces → silent.
    assert_eq!(n("<?php function f(): int { return \"5\"; }\n"), 0, "coercive 5 silent");
    // "abc" -> int coercive: non-numeric → flagged.
    assert_eq!(n("<?php function f(): int { return \"abc\"; }\n"), 1, "coercive abc flagged");
}

#[test]
fn return_strict_numeric_string_flagged() {
    // strict "5" -> int: the string is not an int → flagged (per empirical table).
    assert_eq!(
        n("<?php\ndeclare(strict_types=1);\nfunction f(): int { return \"5\"; }\n"),
        1,
        "strict numeric string return flagged"
    );
}

#[test]
fn return_env_var_value_checked() {
    // `$x = "abc"; return $x;` — the env-known value flows into the return check.
    let src = "<?php function f(): int { $x = \"abc\"; return $x; }\n";
    let d = findings(src);
    assert_eq!(d.len(), 1, "got: {d:#?}");
    assert_eq!(d[0].id, "type.return-mismatch");
}

#[test]
fn return_const_fn_value_checked() {
    // f(): int returns bad(), a const-fn returning "abc" → resolved and checked.
    let src = "<?php\nfunction bad(): string { return \"abc\"; }\nfunction f(): int { return bad(); }\n";
    let d = findings(src);
    assert_eq!(d.len(), 1, "only f()'s return is bad; got: {d:#?}");
    assert_eq!(d[0].id, "type.return-mismatch");
    assert!(d[0].message.contains("return type of f()"), "got: {}", d[0].message);
    assert!(d[0].message.contains("return \"abc\""), "resolved value shown: {}", d[0].message);
}

#[test]
fn return_folded_builtin_value_checked() {
    struct Mock;
    impl Folder for Mock {
        fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
            match (name, args) {
                ("strtolower", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.to_lowercase())),
                _ => None,
            }
        }
    }
    // f(): int { return strtolower("ABC"); } → folds to "abc" (non-numeric) → int.
    let src = "<?php function f(): int { return strtolower(\"ABC\"); }\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let d = check_with(&tree, &functions, "test.php", &mut Mock);
    assert_eq!(d.len(), 1, "got: {d:#?}");
    assert_eq!(d[0].id, "type.return-mismatch");
}

#[test]
fn return_inside_structured_if_is_now_checked() {
    // EXPECTATION CHANGE (ADR-0031, was `..._is_silent` → 0): an `if` is now a
    // structured trace, so a `return "abc";` inside a branch is walked and
    // proof-checked like a top-level return. With `$c` unknown the guard is Maybe,
    // the then-branch is walked, and `return "abc"` into `int` (strict) is FLAGGED.
    // (The former "only top-of-trace returns are checked" limitation is lifted for
    // `if`; loops/switch/try returns remain inside `Opaque` and are still unseen.)
    let src =
        "<?php\ndeclare(strict_types=1);\nfunction f(): int { if ($c) { return \"abc\"; } return 1; }\n";
    let d = findings(src);
    assert_eq!(d.len(), 1, "return inside structured if is now checked: {d:#?}");
    assert_eq!(d[0].id, "type.return-mismatch");
}

#[test]
fn return_into_union_type() {
    // "abc" -> int|float return, coercive → flagged (no string sink).
    assert_eq!(n("<?php function f(): int|float { return \"abc\"; }\n"), 1, "abc -> int|float");
    // "abc" -> int|string return → silent (string sink accepts).
    assert_eq!(n("<?php function f(): int|string { return \"abc\"; }\n"), 0, "abc -> int|string");
    // strict 1.5 -> int|string return → flagged.
    assert_eq!(
        n("<?php\ndeclare(strict_types=1);\nfunction f(): int|string { return 1.5; }\n"),
        1,
        "strict 1.5 -> int|string return flagged"
    );
}

#[test]
fn method_return_checked() {
    let src = "<?php\ndeclare(strict_types=1);\nclass C { function m(): int { return \"abc\"; } }\n";
    let d = findings(src);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].id, "type.return-mismatch");
    assert!(d[0].message.contains("return type of C::m()"), "got: {}", d[0].message);
}

#[test]
fn void_never_untyped_and_nonscalar_returns_skipped() {
    // void: `return;` and even a value return are out of scope.
    assert_eq!(n("<?php function f(): void { return; }\n"), 0, "void skipped");
    // untyped: no return type to check against.
    assert_eq!(n("<?php function f() { return \"abc\"; }\n"), 0, "untyped skipped");
    // never: not a scalar/union → skipped.
    assert_eq!(n("<?php function f(): never { throw new \\Exception(); }\n"), 0, "never skipped");
    // non-scalar (array): skipped.
    assert_eq!(n("<?php function f(): array { return \"abc\"; }\n"), 0, "array skipped");
}

#[test]
fn return_without_value_is_silent() {
    // A bare `return;` in a typed function proves nothing about the value
    // (missing-return-path analysis is out of scope).
    let src = "<?php\ndeclare(strict_types=1);\nfunction f(): int { if ($c) { return; } return 1; }\n";
    assert_eq!(n(src), 0, "bare return; is not a value proof");
}
