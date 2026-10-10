//! Which parameter defaults make a parameter implicitly nullable (issue #1023).
//!
//! PHP makes a parameter implicitly nullable when its default evaluates to `null` at compile
//! time, whatever the spelling. Measured on PHP 8.5.9 in a namespace: `\null`, `\NULL`,
//! `null ?? null`, `true ? null : 0`, `false ?: null` and `[null][0]` all let the argument be
//! omitted and print `NULL`; `= N` with `const N = null`, `0 ?? null` and
//! `PHP_INT_SIZE ? null : 0` do not (the first and last are a `TypeError`).

use steins_syntax::SourceTree;

fn has_null_default(default: &str) -> bool {
    let src = format!("<?php\nnamespace A;\nconst N = null;\nfunction f(int $x = {default}) {{}}\n");
    let tree = SourceTree::parse(&src);
    tree.functions().iter().find(|f| f.name == "f").expect("f").params[0].has_null_default
}

#[test]
fn every_spelling_that_folds_to_null_is_a_null_default() {
    for default in [
        "null",
        "NULL",
        "NuLl",
        "(null)",
        "\\null",
        "\\NULL",
        "(\\null)",
        "null ?? null",
        "true ? null : 0",
        "false ? 0 : null",
        "false ?: null",
        "1 ? null : 1",
        "[null][0]",
        "[1, null][1]",
        "\\true ? null : 0",
    ] {
        assert!(has_null_default(default), "{default}");
    }
}

#[test]
fn a_default_that_does_not_fold_to_null_is_not() {
    for default in [
        "0",
        "N",
        "\\N",
        "0 ?? null",
        "true ? 1 : null",
        "PHP_INT_SIZE ? null : 0",
        "[null][1]",
        "[null]",
        "[1, null][0]",
        "N ?? null",
        "N ? null : 0",
        "\\strlen('')",
    ] {
        assert!(!has_null_default(default), "{default}");
    }
}
