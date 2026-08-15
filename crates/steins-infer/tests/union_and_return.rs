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
    // Drop `untyped.*` (ADR-0078, #200): it flags the fixtures' own deliberately
    // untyped signatures, not the behavior under test.
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn n(src: &str) -> usize {
    findings(src).len()
}

// 1. Native union / nullable parameter types.

#[test]
fn union_param_coercive_cells() {
    // Empirical PHP 8.5.8 coercion table (module doc); each comment gives the
    // mechanism the assert message's verdict doesn't spell out.
    let f = "<?php function f(int|string $v): void {}\n";
    // string sink: 1.5 becomes int 1
    assert_eq!(n(&format!("{f}f(1.5);")), 0, "1.5 -> int|string coercive silent");
    let g = "<?php function g(int|float $v): void {}\n";
    // no string sink for a non-numeric string
    assert_eq!(n(&format!("{g}g(\"abc\");")), 1, "abc -> int|float coercive flagged");
    // numeric string coerces
    assert_eq!(n(&format!("{g}g(\"5\");")), 0, "5 -> int|float coercive silent");
    let h = "<?php function h(string|false $v): void {}\n";
    // matches the `false` literal member
    assert_eq!(n(&format!("{h}h(false);")), 0, "false -> string|false silent");
    // coerces to '1' via the string member
    assert_eq!(n(&format!("{h}h(true);")), 0, "true -> string|false coercive silent");
    assert_eq!(n(&format!("{f}f(null);")), 1, "null -> int|string flagged");
    let k = "<?php function k(int|null $v): void {}\n";
    assert_eq!(n(&format!("{k}k(null);")), 0, "null -> int|null silent");
    let m = "<?php function m(int|false $v): void {}\n";
    // the `false` literal member does not sink strings
    assert_eq!(n(&format!("{m}m(\"abc\");")), 1, "abc -> int|false coercive flagged");
    // bool coerces via the int member
    assert_eq!(n(&format!("{m}m(true);")), 0, "true -> int|false coercive silent");
}

#[test]
fn union_param_strict_cells() {
    // Conformance near-win: 1.5 (float) into int|string strict is a TypeError.
    // bool has no member and no matching bool-literal → also flagged.
    let f = "<?php\ndeclare(strict_types=1);\nfunction f(int|string $v): void {}\n";
    assert_eq!(n(&format!("{f}f(1.5);")), 1, "1.5 -> int|string strict flagged");
    assert_eq!(n(&format!("{f}f(5);")), 0, "int matches member");
    assert_eq!(n(&format!("{f}f(\"x\");")), 0, "string matches member");
    assert_eq!(n(&format!("{f}f(true);")), 1, "true -> int|string strict flagged");

    let g = "<?php\ndeclare(strict_types=1);\nfunction g(int|float $v): void {}\n";
    assert_eq!(n(&format!("{g}g(5);")), 0, "int -> int|float strict OK");
    assert_eq!(n(&format!("{g}g(5.0);")), 0, "float -> int|float strict OK");
    assert_eq!(n(&format!("{g}g(\"5\");")), 1, "numeric string -> int|float strict flagged");

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
    // A member that doesn't lower to a `TypeMember` (`array`/`mixed`/`callable`/
    // `iterable`) lowers the WHOLE type to `None` → silence (zero-FP), even for an
    // obvious mismatch.
    for ty in ["int|array", "int|mixed", "int|callable", "iterable"] {
        let src = format!("<?php\ndeclare(strict_types=1);\nfunction f({ty} $v): void {{}}\nf(1.5);\n");
        assert_eq!(n(&src), 0, "type `{ty}` must lower to silence");
    }
    // Same for a DNF hint whose intersection conjunct is a non-class type (`int&B`
    // is not a valid object intersection): the conjunct guard silences it too.
    let src = "<?php\ndeclare(strict_types=1);\nfunction f(int|(iterable&Countable) $v): void {}\nf(1.5);\n";
    assert_eq!(n(src), 0, "non-class conjunct silences the whole type");
}

#[test]
fn intersection_union_member_is_now_modeled_adr0043() {
    // ADR-0043: `A&B` is now a modeled conjunctive member, no longer silenced. `1.5`
    // matches neither `int` nor the intersection (a scalar is never an object) — a
    // proven TypeError, verified at PHP 8.5.8.
    let src = "<?php\ndeclare(strict_types=1);\ninterface A {}\ninterface B {}\nfunction f(int|(A&B) $v): void {}\nf(1.5);\n";
    let d = findings(src);
    assert_eq!(d.len(), 1, "1.5 vs int|(A&B) strict is a proven TypeError");
    assert_eq!(d[0].id, "type.argument-mismatch");
}

#[test]
fn object_union_member_is_now_modeled_adr0043_stage3() {
    // ADR-0043 stage 3: `int|\Foo` is no longer silenced. `1.5` matches neither
    // `int` nor `\Foo` (a scalar is never an object) — a proven TypeError,
    // verified at PHP 8.5.8.
    let src = "<?php\ndeclare(strict_types=1);\nfinal class Foo {}\nfunction f(int|\\Foo $v): void {}\nf(1.5);\n";
    let d = findings(src);
    assert_eq!(d.len(), 1, "1.5 vs int|Foo strict is a proven TypeError");
    assert_eq!(d[0].id, "type.argument-mismatch");
}

// 2. Native return-type checking (`type.return-mismatch`).

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
    assert_eq!(n("<?php function f(): int { return \"5\"; }\n"), 0, "coercive 5 silent");
    assert_eq!(n("<?php function f(): int { return \"abc\"; }\n"), 1, "coercive abc flagged");
}

#[test]
fn return_strict_numeric_string_flagged() {
    // A numeric string is still not an `int` in strict mode (module doc's table).
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
        fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
            match (name, args) {
                ("strtolower", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_lowercase().into())),
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
    // ADR-0031: an `if` is a structured trace, so a Maybe-guarded branch return is
    // walked and proof-checked like a top-level return — lifting the former
    // "only top-of-trace returns" limit. Loops/switch/try stay `Opaque`, still unseen.
    let src =
        "<?php\ndeclare(strict_types=1);\nfunction f($c): int { if ($c) { return \"abc\"; } return 1; }\n";
    let d = findings(src);
    assert_eq!(d.len(), 1, "return inside structured if is now checked: {d:#?}");
    assert_eq!(d[0].id, "type.return-mismatch");
}

#[test]
fn return_into_union_type() {
    // No string sink in int|float, so "abc" flags; int|string's string sink accepts it.
    assert_eq!(n("<?php function f(): int|float { return \"abc\"; }\n"), 1, "abc -> int|float");
    assert_eq!(n("<?php function f(): int|string { return \"abc\"; }\n"), 0, "abc -> int|string");
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
    // void is out of scope even for a value return; untyped has no type to check
    // against; never/array are not a scalar or union — all four skip the check.
    assert_eq!(n("<?php function f(): void { return; }\n"), 0, "void skipped");
    assert_eq!(n("<?php function f() { return \"abc\"; }\n"), 0, "untyped skipped");
    assert_eq!(n("<?php function f(): never { throw new \\Exception(); }\n"), 0, "never skipped");
    assert_eq!(n("<?php function f(): array { return \"abc\"; }\n"), 0, "array skipped");
}

#[test]
fn return_without_value_is_silent() {
    // A bare `return;` in a typed function proves nothing about the value
    // (missing-return-path analysis is out of scope).
    let src = "<?php\ndeclare(strict_types=1);\nfunction f($c): int { if ($c) { return; } return 1; }\n";
    assert_eq!(n(src), 0, "bare return; is not a value proof");
}
