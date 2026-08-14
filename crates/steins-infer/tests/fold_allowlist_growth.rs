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

// ---- wave 1: the width-UNVERIFIED names (ADR-0028 §4/§5, issue #330) -------

/// `explode` folds to the engine's own pieces on the all-literal path. It is the
/// amendment's §5 case in its purest form: the Rust rung answers a *type*
/// (`non-empty-list<string>`), the fold answers the *value*, so the fold is
/// strictly stronger and the rung stays underneath it.
///
/// It is on the allowlist as a `WIDTH_UNVERIFIED` row — it folds here, on a
/// 64-bit engine, and declines in the browser, with no probe on record either
/// way. `replay_fold.rs` pins that decline; this pins what a 64-bit engine gets.
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
            // `explode(',', '')` is `['']`, not `[]` — the witness the rung's
            // non-emptiness rests on, now readable as a value.
            "list{''}",
            // The three-argument form the RUNG declines (a negative `$limit`
            // breaks non-emptiness outright) folds without trouble: a fold is a
            // claim about one argument tuple, and the engine evaluated this one.
            "list{'a', 'b,c'}",
        ]
    );
    assert_eq!(
        steins_catalog::width_class("explode"),
        Some(steins_catalog::WidthClass::Unverified),
        "folds here, declines in the browser, with no probe on record either way"
    );
}

/// `array_merge` folds to the engine's merged array — and the fixture is chosen to
/// exercise exactly the two rules the catalog refuses to re-derive in Rust
/// (ADR-0004): a duplicate **string** key resolves last-wins, and every **integer**
/// key is renumbered from zero in argument order regardless of what it was.
///
/// `['k' => 'v', 7 => 's']` merged with `['k' => 'w', 3 => 't']` is
/// `['k' => 'w', 0 => 's', 1 => 't']`: `'v'` lost to `'w'`, and `7`/`3` became
/// `0`/`1`. Nothing in Rust computed that; the engine did, which is the whole
/// argument for the name being on the list rather than getting a rung.
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
            // The unkeyed case beside it, so the fixture above cannot be passing
            // because keys are being dropped wholesale.
            "list{'a', 'b', 'c'}",
        ]
    );
    assert_eq!(
        steins_catalog::width_class("array_merge"),
        Some(steins_catalog::WidthClass::Unverified)
    );
}

/// **The fold shadows a floor it never removes** — §5's "strictly stronger" rule
/// made observable: the same source, one folder apart.
///
/// The floor is two rungs, not one, and which one answers depends on the engine —
/// a detail worth stating because it is easy to assume the `explode` rung is
/// engine-free. It is not: `arg_dispatch_return_fact` admits a rule only when the
/// engine's own reflected declaration matches the one the rule was written against
/// (ADR-0061's independent-implementation cross-check), so with NO engine the rung
/// declines with everything else and the answer is ADR-0069's declared-return
/// floor, `list<string> (asserted)` — a type, still, and marked as a declaration's
/// claim rather than an engine's. The rung proper stands where reflection works but
/// the fold does not, which is exactly the browser; `replay_fold.rs` pins that on a
/// 32-bit table, since it is the case no local sidecar can produce.
#[test]
fn the_explode_fold_shadows_a_floor_it_never_removes() {
    const SRC: &str = "<?php\n\\PHPStan\\dumpType(explode(\",\", \"a,b,c\"));\n";
    // No engine at all: no fold, no reflection, and therefore no rung either —
    // the declared-return floor, which is still a type and still not lost.
    assert_eq!(
        dumps(SRC, &mut SidecarFolder::new(true)),
        vec!["list<string> (asserted)"],
        "a folderless run keeps the declared floor"
    );
    // With an engine: the value. Skipping here would leave the assertion above
    // passing for the wrong reason, so the skip stays as loud as everywhere else.
    let Some(mut folder) = live("the_explode_fold_shadows_a_floor_it_never_removes") else {
        return;
    };
    assert_eq!(dumps(SRC, &mut folder), vec!["list{'a', 'b', 'c'}"]);
}

/// The over-budget case wave 0 could not construct. `str_replace` and
/// `substr_replace` return an array bounded by their array *argument*, which the
/// argument budget has already admitted — so no all-literal call of theirs can
/// exceed the result budget. `explode` grows: a 257-piece subject is an ordinary
/// literal and its result is one past the 256-entry bound.
///
/// What must happen is a *widen*, not a loss. The runner charges the budget before
/// encoding and answers `'array result over entry budget'` (pinned as a protocol
/// fact in `steins-sidecar/tests/protocol.rs`), the fold declines, and the dump
/// falls back to the type-level floor. One entry below the bound the same call
/// folds, so this is a boundary and not a blanket refusal of long results.
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
    // 256 pieces is the last admissible width, and it folds. The dump is long, so
    // it is checked by shape: a folded value, with every piece in it.
    let d = dumps(&src(256), &mut folder);
    assert_eq!(d.len(), 1);
    assert!(d[0].starts_with("list{'x', 'x', "), "the boundary case folds, got: {}", &d[0][..40]);
    assert_eq!(d[0].matches("'x'").count(), 256, "every piece survived the seam");
}

/// `explode('', 'x')` is a `ValueError` at `PINNED_PHP` — PHP 8.0 replaced the old
/// `false` return with a throw — and that edge is precisely on the list of
/// semantics the catalog declines to re-derive in Rust (`WIDTH_UNVERIFIED`).
///
/// The engine reports the throw, the fold declines, and the dump falls through.
/// The rung declines here too, on its own separate grounds (an empty separator has
/// no return value to describe, so there is no `non-empty` to promise), which is
/// why the answer is ADR-0069's declared floor rather than the rung: two
/// independent refusals agreeing, which is the shape a soundness floor is supposed
/// to have. `steins-sidecar`'s protocol tests pin the `FoldResult::Throw` itself.
#[test]
fn an_empty_explode_separator_throws_and_falls_to_the_floor() {
    let Some(mut folder) = live("an_empty_explode_separator_throws_and_falls_to_the_floor") else {
        return;
    };
    const SRC: &str = "<?php\n\\PHPStan\\dumpType(explode(\"\", \"x\"));\n";
    assert_eq!(dumps(SRC, &mut folder), vec!["list<string> (asserted)"]);
    // The `non-empty` is what the throw costs, and the contrast is one line away:
    // a non-empty separator on the same folder folds to the value itself.
    assert_eq!(
        dumps("<?php\n\\PHPStan\\dumpType(explode(\",\", \"x\"));\n", &mut folder),
        vec!["list{'x'}"]
    );
}
