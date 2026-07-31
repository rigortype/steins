//! The issue-#78 allowlist admissions, end to end through the **real** sidecar.
//!
//! Every name here reached the catalog the same way: an ADR-0008 purity argument,
//! a 32/64-bit differential probe verdict, and a `WIDTH_SAFE`/`WIDTH_REFUSED` row.
//! No mechanism moved with them — the fold lane, the width gate and the replay loop
//! picked them up because they are on the allowlist, which is exactly what these
//! fixtures pin: the dump surface renders the engine's own answer, at the value
//! rung, for names nobody wrote a line of code for.
//!
//! The 32-bit half of each verdict is not testable from here (this machine's `php`
//! is 64-bit); `replay_fold.rs` covers the width gate against a synthetic 32-bit
//! `env`, and the probe evidence itself lives in the ADR-0066 amendment.
//!
//! Requires `php` on `PATH`; without it each test skips with an explicit marker.

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
    if folder.fold("strtoupper", &[ArgValue::Str("probe".to_owned())]).is_none() {
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

/// `substr_replace` is admitted for its **scalar** subject. Handed an array subject
/// the engine answers with an array, and an array *result* widens on the Rust side —
/// the documented #41/#42 boundary, unchanged by this slice. The fixture pins that
/// the scalar sibling on the very next line still folds, so the widen is the result
/// shape and not a disabled folder.
#[test]
fn an_array_returning_fold_widens_and_costs_nothing_after_it() {
    let Some(mut folder) = live("an_array_returning_fold_widens_and_costs_nothing_after_it") else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(substr_replace([\"aa\", \"bb\"], \"X\", 0));\n\
         \\PHPStan\\dumpType(substr_replace(\"Hello\", \"X\", 1));\n";
    let d = dumps(SRC, &mut folder);
    assert_ne!(d[0], "array{'X', 'X'}", "an array result does not come back as a value");
    assert_eq!(d[1], "'HX'", "and the next scalar fold is unaffected");
}
