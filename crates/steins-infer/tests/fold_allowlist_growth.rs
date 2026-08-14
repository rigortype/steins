//! Issue #78 allowlist admissions through the real sidecar.
//!
//! Each name has an ADR-0008 purity argument, a 32/64-bit differential verdict,
//! and a `WIDTH_SAFE`/`WIDTH_REFUSED` row; fixtures assert the dump surface
//! renders the engine's answer. `replay_fold.rs` covers the 32-bit width gate;
//! probe evidence is in the ADR-0066 amendment. Requires `php` on `PATH` — each
//! test skips explicitly otherwise.

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

/// `version_compare` is `WIDTH_REFUSED`: folds on this 64-bit engine, declines
/// in the browser. Both arities are pinned — 2-arg is `-1|0|1`, 3-arg is bool —
/// since the refusal covers them together (operator form runs the same comparison).
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

/// String predicates fold to real booleans — a folded `false` is a value the
/// narrowing lane can act on, where declared `bool` is not.
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

/// `base64_decode`'s strict 2nd arg turns a malformed payload into `false`
/// rather than a lossy string — the folded value changes *type* with an argument.
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

/// `strtr` at both arities: the 2-arg form takes an array (fold wire since
/// issue #39), and PHP's longest-key-first rule (not ours) decides the result.
#[test]
fn strtr_folds_at_both_arities() {
    let Some(mut folder) = live("strtr_folds_at_both_arities") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(strtr(\"abc\", \"a\", \"x\"));\n\
         \\PHPStan\\dumpType(strtr(\"abc\", \"ab\", \"x\"));\n\
         \\PHPStan\\dumpType(strtr(\"hi all\", [\"hi\" => \"hello\", \"all\" => \"world\"]));\n\
         \\PHPStan\\dumpType(strtr(\"abc\", [\"a\" => \"1\", \"ab\" => \"2\"]));\n\
         \\PHPStan\\dumpType(strtr(\"abc\", [\"ab\" => \"2\", \"a\" => \"1\"]));\n";
    // Last two differ only in source order and agree: longest-key-first, engine-decided.
    assert_eq!(dumps(SRC, &mut folder), vec!["'xbc'", "'xbc'", "'hello world'", "'2c'", "'2c'"]);
}

/// Rest of the admitted surface — each name needed only a table row.
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

/// `substr_replace` folds its **scalar** subject; an array subject folds too,
/// since ADR-0028's 2026-08-14 amendment (issue #330) — previously widened on
/// the Rust side (the old #41/#42 boundary). Array and scalar sit side by side
/// so a regression disabling the array path can't hide behind the scalar one.
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

/// Wave-0 sibling: `str_replace` over an array subject. Both names were already
/// `WIDTH_SAFE`, held back only by the old result boundary — no new width verdict
/// needed. PHP preserves the subject's keys, so mixed string/int keys survive distinct.
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

/// §5 derivation: an all-literal argument list is `Verified`, so the fact
/// carries no `(asserted)` marker — the visible line between engine and declaration.
#[test]
fn a_folded_array_is_verified_not_asserted() {
    let Some(mut folder) = live("a_folded_array_is_verified_not_asserted") else { return };
    const SRC: &str = "<?php\n\
         $x = str_replace(\"o\", \"0\", [\"foo\"]);\n\
         \\PHPStan\\dumpType($x);\n";
    let d = dumps(SRC, &mut folder);
    assert_eq!(d, vec!["list{'f00'}"], "no `(asserted)` marker: the engine answered");
}

// wave 1: the width-UNVERIFIED names (ADR-0028 §4/§5, issue #330)

/// `explode` folds to the engine's own pieces on the all-literal path — the
/// Rust rung answers a *type* (`non-empty-list<string>`), the fold the *value*,
/// strictly stronger. `WIDTH_UNVERIFIED`: folds here on 64-bit, declines in the
/// browser with no probe on record; `replay_fold.rs` pins the decline.
#[test]
fn explode_folds_to_the_engines_own_pieces() {
    let Some(mut folder) = live("explode_folds_to_the_engines_own_pieces") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(explode(\",\", \"a,b,c\"));\n\
         \\PHPStan\\dumpType(explode(\",\", \"\"));\n\
         \\PHPStan\\dumpType(explode(\",\", \"a,b,c\", 2));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "list{'a', 'b', 'c'}",
            // `explode(',', '')` is `['']` not `[]` — the rung's non-emptiness witness.
            "list{''}",
            // RUNG declines the 3-arg form (negative `$limit` breaks non-emptiness);
            // the fold doesn't — the engine evaluated this one tuple.
            "list{'a', 'b,c'}",
        ]
    );
    assert_eq!(
        steins_catalog::width_class("explode"),
        Some(steins_catalog::WidthClass::Unverified),
        "folds here, declines in the browser, with no probe on record either way"
    );
}

/// `array_merge` folds to the engine's merged array, exercising the two rules
/// the catalog refuses to re-derive in Rust (ADR-0004): duplicate **string**
/// keys resolve last-wins, and **integer** keys renumber from zero in argument
/// order. `['k'=>'v',7=>'s']` merged with `['k'=>'w',3=>'t']` becomes
/// `['k'=>'w',0=>'s',1=>'t']` — the engine computed that, not Rust.
#[test]
fn array_merge_folds_with_the_engines_own_key_resolution() {
    let Some(mut folder) = live("array_merge_folds_with_the_engines_own_key_resolution") else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(array_merge([\"k\" => \"v\", 7 => \"s\"], [\"k\" => \"w\", 3 => \"t\"]));\n\
         \\PHPStan\\dumpType(array_merge([\"a\", \"b\"], [\"c\"]));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "array{k: 'w', 0: 's', 1: 't'}",
            // Unkeyed case alongside it, so keys-dropped-wholesale can't fake a pass.
            "list{'a', 'b', 'c'}",
        ]
    );
    assert_eq!(
        steins_catalog::width_class("array_merge"),
        Some(steins_catalog::WidthClass::Unverified)
    );
}

/// **The fold shadows a floor it never removes** — §5's "strictly stronger"
/// rule made observable, same source, one folder apart. Two rungs sit under
/// it: `arg_dispatch_return_fact` admits a rule only when the engine's own
/// reflection matches the rule (ADR-0061 cross-check), so with no engine it
/// declines too, falling to ADR-0069's declared floor, `list<string>
/// (asserted)`. The rung stands where reflection works but the fold doesn't —
/// the browser case; `replay_fold.rs` pins that on a 32-bit table.
#[test]
fn the_explode_fold_shadows_a_floor_it_never_removes() {
    const SRC: &str = "<?php\n\\PHPStan\\dumpType(explode(\",\", \"a,b,c\"));\n";
    // No engine: no fold, no reflection, no rung — the declared floor, still a type.
    assert_eq!(
        dumps(SRC, &mut SidecarFolder::new(true)),
        vec!["list<string> (asserted)"],
        "a folderless run keeps the declared floor"
    );
    // With an engine: the value — skip stays loud so a false pass can't hide.
    let Some(mut folder) = live("the_explode_fold_shadows_a_floor_it_never_removes") else {
        return;
    };
    assert_eq!(dumps(SRC, &mut folder), vec!["list{'a', 'b', 'c'}"]);
}

/// Over-budget case wave 0 couldn't hit: `str_replace`/`substr_replace`'s array
/// result is bounded by their array argument (already budget-admitted), but
/// `explode` grows — a 257-piece literal exceeds the 256-entry bound. Must
/// widen, not lose: the runner charges the budget before encoding, answers
/// `'array result over entry budget'` (pinned in `steins-sidecar/tests/protocol.rs`),
/// the fold declines, and the dump falls to the type floor. One entry under folds.
#[test]
fn an_over_budget_explode_widens_to_the_rung_rather_than_losing_the_answer() {
    let Some(mut folder) =
        live("an_over_budget_explode_widens_to_the_rung_rather_than_losing_the_answer")
    else {
        return;
    };
    /// `explode(',', 'x,x,…')` over `pieces` pieces, as source.
    fn src(pieces: usize) -> String {
        format!("<?php\n\\PHPStan\\dumpType(explode(\",\", \"{}\"));\n", vec!["x"; pieces].join(","))
    }
    assert_eq!(
        dumps(&src(257), &mut folder),
        vec!["non-empty-list<string>"],
        "an over-budget result widens to the rung — the answer is coarser, never absent"
    );
    // 256 is the last admissible width and folds; checked by shape since the dump is long.
    let d = dumps(&src(256), &mut folder);
    assert_eq!(d.len(), 1);
    assert!(d[0].starts_with("list{'x', 'x', "), "the boundary case folds, got: {}", &d[0][..40]);
    assert_eq!(d[0].matches("'x'").count(), 256, "every piece survived the seam");
}

/// `explode('', 'x')` is a `ValueError` at `PINNED_PHP` (PHP 8.0 replaced the
/// old `false` return with a throw) — `WIDTH_UNVERIFIED`. Engine reports the
/// throw, fold declines, dump falls to ADR-0069's declared floor: two
/// independent refusals (the rung also declines — no return value to describe
/// `non-empty` about). `steins-sidecar`'s protocol tests pin `FoldResult::Throw`.
#[test]
fn an_empty_explode_separator_throws_and_falls_to_the_floor() {
    let Some(mut folder) = live("an_empty_explode_separator_throws_and_falls_to_the_floor") else {
        return;
    };
    const SRC: &str = "<?php\n\\PHPStan\\dumpType(explode(\"\", \"x\"));\n";
    assert_eq!(dumps(SRC, &mut folder), vec!["list<string> (asserted)"]);
    // `non-empty` is what the throw costs — a non-empty separator folds fine.
    assert_eq!(
        dumps("<?php\n\\PHPStan\\dumpType(explode(\",\", \"x\"));\n", &mut folder),
        vec!["list{'x'}"]
    );
}
