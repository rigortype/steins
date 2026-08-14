//! Issue #78 allowlist admissions through the real sidecar.
//!
//! Each name has an ADR-0008 purity argument, a 32/64-bit differential verdict,
//! and a `WIDTH_SAFE`/`WIDTH_REFUSED` row. These fixtures assert that the dump
//! surface renders the engine's answer. `replay_fold.rs` covers the 32-bit width
//! gate; probe evidence is in the ADR-0066 amendment.
//!
//! Requires `php` on `PATH`; otherwise each test skips explicitly.

use steins_infer::{DEBUG_TYPE_ID, Folder, SidecarFolder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The `dumpType` bodies for `src`, in source order.
fn dumps(src: &str, folder: &mut dyn Folder) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", folder)
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// A live sidecar folder, or `None` when `php` cannot be reached — in which case
/// the caller skips loudly rather than asserting something vacuous.
fn live(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())]).is_none() {
        eprintln!("SKIP {test}: no folding engine — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

/// `version_compare` is on the allowlist as a **width-refused** row: it folds here,
/// on a 64-bit engine, and declines in the browser. Both arities are pinned — the
/// two-argument form is the `-1|0|1` int, the three-argument form the bool — because
/// the refusal covers them together (the operator form runs the same comparison).
#[test]
fn version_compare_folds_both_arities_on_a_64_bit_engine() {
    let Some(mut folder) = live("version_compare_folds_both_arities_on_a_64_bit_engine") else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(version_compare(\"1.0\", \"1.1\"));\n\
         \\PHPStan\\dumpType(version_compare(\"1.1\", \"1.0\"));\n\
         \\PHPStan\\dumpType(version_compare(\"1.0\", \"1.0\"));\n\
         \\PHPStan\\dumpType(version_compare(\"1.0\", \"1.1\", \"<\"));\n\
         \\PHPStan\\dumpType(version_compare(\"1.0\", \"1.1\", \"ge\"));\n";
    assert_eq!(dumps(SRC, &mut folder), vec!["-1", "1", "0", "true", "false"]);
    assert!(steins_catalog::foldable("version_compare"));
    assert!(!steins_catalog::width_safe("version_compare"), "refused on a 32-bit engine");
}

/// The string predicates fold to real booleans, which is what makes them worth
/// admitting: a folded `false` is a value the narrowing lane can act on, where the
/// declared `bool` envelope is not.
#[test]
fn the_string_predicates_fold_to_booleans() {
    let Some(mut folder) = live("the_string_predicates_fold_to_booleans") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(str_contains(\"Hello\", \"ell\"));\n\
         \\PHPStan\\dumpType(str_contains(\"Hello\", \"z\"));\n\
         \\PHPStan\\dumpType(str_starts_with(\"Hello\", \"He\"));\n\
         \\PHPStan\\dumpType(str_ends_with(\"Hello\", \"lo\"));\n\
         \\PHPStan\\dumpType(str_ends_with(\"Hello\", \"He\"));\n";
    assert_eq!(dumps(SRC, &mut folder), vec!["true", "false", "true", "true", "false"]);
}

/// `base64_decode`'s **strict** second argument is the interesting one: it turns a
/// malformed payload into `false` rather than a lossy string, so the folded value
/// changes *type* with an argument. The engine answers; nothing here models it.
#[test]
fn base64_decode_folds_its_strict_false() {
    let Some(mut folder) = live("base64_decode_folds_its_strict_false") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(base64_decode(\"YWJj\"));\n\
         \\PHPStan\\dumpType(base64_decode(\"!!!\", true));\n\
         \\PHPStan\\dumpType(base64_decode(\"!!!\"));\n\
         \\PHPStan\\dumpType(base64_encode(\"abc\"));\n";
    assert_eq!(dumps(SRC, &mut folder), vec!["'abc'", "false", "''", "'YWJj'"]);
}

/// `strtr` at **both** arities. The two-argument form takes an array, which the
/// fold wire has carried since issue #39 — so admitting the name lit the array form
/// up with it, and PHP's own longest-key-first rule (not ours) decides the result.
#[test]
fn strtr_folds_at_both_arities() {
    let Some(mut folder) = live("strtr_folds_at_both_arities") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(strtr(\"abc\", \"a\", \"x\"));\n\
         \\PHPStan\\dumpType(strtr(\"abc\", \"ab\", \"x\"));\n\
         \\PHPStan\\dumpType(strtr(\"hi all\", [\"hi\" => \"hello\", \"all\" => \"world\"]));\n\
         \\PHPStan\\dumpType(strtr(\"abc\", [\"a\" => \"1\", \"ab\" => \"2\"]));\n\
         \\PHPStan\\dumpType(strtr(\"abc\", [\"ab\" => \"2\", \"a\" => \"1\"]));\n";
    // The last two differ only in source order and agree: `strtr`'s array form is
    // longest-key-first, not first-listed-first, and the engine is the one saying so.
    assert_eq!(dumps(SRC, &mut folder), vec!["'xbc'", "'xbc'", "'hello world'", "'2c'", "'2c'"]);
}

/// The rest of the admitted surface, one line each — the point being that none of
/// them needed anything but a table row.
#[test]
fn the_admitted_surface_folds() {
    let Some(mut folder) = live("the_admitted_surface_folds") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(str_pad(\"abc\", 6, \"-\"));\n\
         \\PHPStan\\dumpType(substr_replace(\"Hello\", \"X\", 1, 2));\n\
         \\PHPStan\\dumpType(ucwords(\"hello world\"));\n\
         \\PHPStan\\dumpType(str_increment(\"Az\"));\n\
         \\PHPStan\\dumpType(str_decrement(\"Ba\"));\n\
         \\PHPStan\\dumpType(gettype(1));\n\
         \\PHPStan\\dumpType(addslashes(\"a'b\"));\n\
         \\PHPStan\\dumpType(preg_quote(\"a.b\"));\n\
         \\PHPStan\\dumpType(rawurlencode(\"a b\"));\n\
         \\PHPStan\\dumpType(urldecode(\"a+b\"));\n\
         \\PHPStan\\dumpType(dechex(255));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "'abc---'",
            "'HXlo'",
            "'Hello World'",
            "'Ba'",
            "'Az'",
            "'integer'",
            "'a\\\\\\'b'",
            "'a\\\\.b'",
            "'a%20b'",
            "'a b'",
            "'ff'",
        ]
    );
}

/// `substr_replace` is classified for its **scalar** subject; handed an array
/// subject the engine answers with an array. That answer used to widen on the Rust
/// side — the old #41/#42 boundary — and folds since ADR-0028's 2026-08-14
/// amendment (issue #330). The fixture pins the array value the engine actually
/// produced, and the scalar sibling on the very next line, so a regression that
/// disabled the folder outright cannot be mistaken for the array path working.
#[test]
fn an_array_returning_fold_carries_the_engines_array_back() {
    let Some(mut folder) = live("an_array_returning_fold_carries_the_engines_array_back") else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(substr_replace([\"aa\", \"bb\"], \"X\", 0));\n\
         \\PHPStan\\dumpType(substr_replace(\"Hello\", \"X\", 1));\n";
    let d = dumps(SRC, &mut folder);
    assert_eq!(d[0], "list{'X', 'X'}", "the array result comes back as the engine's value");
    assert_eq!(d[1], "'HX'", "and the scalar sibling is unaffected");
}

/// The wave-0 sibling: `str_replace` over an array subject. Both names were already
/// `WIDTH_SAFE` and held back only by the old result boundary, which is why they are
/// the amendment's first wave — no width verdict was needed for either.
///
/// The keyed subject is the load-bearing half. PHP preserves the subject's keys
/// through `str_replace`, so `['a' => …, 7 => …]` comes back keyed, and the dump
/// shows both key kinds surviving the round trip as *distinct* keys rather than
/// collapsing into a list.
#[test]
fn str_replace_folds_an_array_subject_keys_and_all() {
    let Some(mut folder) = live("str_replace_folds_an_array_subject_keys_and_all") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(str_replace(\"o\", \"0\", [\"foo\", \"boo\"]));\n\
         \\PHPStan\\dumpType(str_replace(\"o\", \"0\", [\"a\" => \"foo\", 7 => \"boo\"]));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec!["list{'f00', 'b00'}", "array{a: 'f00', 7: 'b00'}"]
    );
}

/// The stratum of a folded array is the §5 derivation clause unchanged: an
/// all-literal argument list is `Verified`, so the fact carries no `(asserted)`
/// marker. The marker is the whole visible difference between a fact the engine
/// produced and one a declaration claimed, and an array answer must not weaken it.
#[test]
fn a_folded_array_is_verified_not_asserted() {
    let Some(mut folder) = live("a_folded_array_is_verified_not_asserted") else { return };
    const SRC: &str = "<?php\n\
         $x = str_replace(\"o\", \"0\", [\"foo\"]);\n\
         \\PHPStan\\dumpType($x);\n";
    let d = dumps(SRC, &mut folder);
    assert_eq!(d, vec!["list{'f00'}"], "no `(asserted)` marker: the engine answered");
}
