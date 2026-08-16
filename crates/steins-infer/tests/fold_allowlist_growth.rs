//! Issue #78 allowlist admissions through the real sidecar.
//!
//! Each name has an ADR-0008 purity argument, a 32/64-bit differential verdict,
//! and a `PORTABLE`/`REFUSED` row; fixtures assert the dump surface
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
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())], true).is_none() {
        eprintln!("SKIP {test}: no folding engine — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

/// `version_compare` is `REFUSED`: folds on this 64-bit engine, declines
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
    assert!(!steins_catalog::portable("version_compare"), "refused on a 32-bit engine");
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
/// `PORTABLE`, held back only by the old result boundary — no new width verdict
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
/// strictly stronger. `UNVERIFIED`: folds here on 64-bit, declines in the
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
        steins_catalog::portability_class("explode"),
        Some(steins_catalog::PortabilityClass::Portable),
        "measured at last (issue #382): 25 generated tuples, both conventions, and the only \
         non-agreements are the narrow engine's TypeError on an oversized $limit"
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
        steins_catalog::portability_class("array_merge"),
        Some(steins_catalog::PortabilityClass::Portable),
        "measured at last (issue #382): 13 generated tuples across one, two and three arrays, \
         both conventions — neither the renumbering nor the last-wins rule consults the \
         machine word"
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
/// old `false` return with a throw) — `UNVERIFIED`. Engine reports the
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

// issue #354: the five names ADR-0028's wave 1 deferred, each landed in the
// class its differential probes chose. Evidence is in the ADR-0066 amendment.

/// The three that probed clean. `array_fill` and `str_split` take an `int`
/// parameter without ever coercing a *value* by it, and `array_unique` compares
/// string casts without retyping what it keeps — so all three are `PORTABLE`,
/// and `replay_fold.rs` pins that they fold on a 32-bit table too. Each also
/// exercises a rule Rust declines to re-derive (ADR-0004): `array_fill`'s
/// negative `$start_index` key sequence (PHP 8.3 changed it), `str_split`'s
/// empty-string return (8.2 changed it), `array_unique`'s `SORT_STRING`
/// comparison of unlike scalars.
#[test]
fn the_probed_clean_names_fold_to_the_engines_own_arrays() {
    let Some(mut folder) = live("the_probed_clean_names_fold_to_the_engines_own_arrays") else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(array_fill(0, 3, \"x\"));\n\
         \\PHPStan\\dumpType(array_fill(-5, 3, \"x\"));\n\
         \\PHPStan\\dumpType(str_split(\"abcdef\", 2));\n\
         \\PHPStan\\dumpType(str_split(\"\"));\n\
         \\PHPStan\\dumpType(array_unique([1, \"1\", 2]));\n\
         \\PHPStan\\dumpType(array_unique([\"a\" => 1, \"b\" => 1, \"c\" => 2]));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "list{'x', 'x', 'x'}",
            // 8.3+ counts up from the negative start; before it the third key was 1.
            "array{-5: 'x', -4: 'x', -3: 'x'}",
            "list{'ab', 'cd', 'ef'}",
            // 8.2+ returns the empty array; before it, `['']`.
            "array{}",
            // SORT_STRING: `1` and `"1"` cast alike, so the string goes.
            "array{0: 1, 2: 2}",
            "array{a: 1, c: 2}",
        ]
    );
    for name in ["array_fill", "str_split", "array_unique"] {
        assert_eq!(
            steins_catalog::portability_class(name),
            Some(steins_catalog::PortabilityClass::Portable),
            "{name} probed clean, so it folds in the browser too"
        );
    }
}

/// `array_fill(0, 1000000, 'x')` is the legitimate call with an illegitimate
/// reply: nothing about it is wrong except the size of the answer. The runner
/// charges the 256-entry budget before encoding, so the call is *made* and the
/// result declined — the dump falls to a type, never to a truncated array.
/// This is the `explode` sibling one step further along: `explode` needs a
/// 257-piece literal to get there, `array_fill` needs one integer.
#[test]
fn an_over_budget_array_fill_widens_rather_than_truncating() {
    let Some(mut folder) = live("an_over_budget_array_fill_widens_rather_than_truncating") else {
        return;
    };
    let over = dumps("<?php\n\\PHPStan\\dumpType(array_fill(0, 257, \"x\"));\n", &mut folder);
    assert_eq!(over.len(), 1);
    assert!(
        !over[0].starts_with("list{") && !over[0].starts_with("array{"),
        "an over-budget result must not come back as a value, got: {}",
        over[0]
    );
    // The size the runner declines is the reply's, not the argument's: one entry
    // under the same bound folds whole.
    let under = dumps("<?php\n\\PHPStan\\dumpType(array_fill(0, 256, \"x\"));\n", &mut folder);
    assert_eq!(under.len(), 1);
    assert!(under[0].starts_with("list{'x', 'x', "), "the boundary case folds: {}", &under[0][..40]);
    assert_eq!(under[0].matches("'x'").count(), 256, "every entry survived the seam");
}

/// `range` folds here and is refused in the browser, and the fold below is the
/// refusal's own witness: `range("3000000000", "3000000000")` is a
/// `list{3000000000}` of **int** on this engine and of **float** on a 32-bit
/// one. `range`'s bounds are declared `string|int|float`, so the engine's own
/// width types the numeric string — the same route that refused `bindec` and
/// `hexdec`, reached through an argument no range guard can reject, since the
/// guard bounds integers and this argument is a string.
#[test]
fn range_folds_on_a_64_bit_engine_including_its_own_refusal_witness() {
    let Some(mut folder) = live("range_folds_on_a_64_bit_engine_including_its_own_refusal_witness")
    else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(range(1, 5));\n\
         \\PHPStan\\dumpType(range(\"a\", \"e\", 2));\n\
         \\PHPStan\\dumpType(range(0, 1, 0.5));\n\
         \\PHPStan\\dumpType(range(\"3000000000\", \"3000000000\"));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "list{1, 2, 3, 4, 5}",
            "list{'a', 'c', 'e'}",
            "list{0.0, 0.5, 1.0}",
            "list{3000000000}",
        ]
    );
    assert_eq!(
        steins_catalog::portability_class("range"),
        Some(steins_catalog::PortabilityClass::Refused),
        "the last dump carries a float, not an int, on a 32-bit engine"
    );
}

/// `preg_split` is the one refused row whose divergence is not the integer
/// width: the two engines run different PCRE builds, and PCRE2's JIT does not
/// honour the inline `(*LIMIT_MATCH=…)` verbs its interpreter does. It still
/// folds here, on the project's own PCRE, which is the only engine whose answer
/// is the right one for the project's own runtime. ADR-0078's pattern lane
/// (`preg_refusal_memo`) is not consulted and does not need to be: an
/// uncompilable pattern makes `preg_split` return `false`, which is this
/// engine's own answer to the value question, while the lane answers the
/// separate question of whether the pattern is broken at all.
#[test]
fn preg_split_folds_on_the_projects_own_pcre() {
    let Some(mut folder) = live("preg_split_folds_on_the_projects_own_pcre") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(preg_split(\"/,/\", \"a,b,c\"));\n\
         \\PHPStan\\dumpType(preg_split(\"/,/\", \"a,b,c\", 2));\n\
         \\PHPStan\\dumpType(preg_split(\"/[/\", \"abc\"));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "list{'a', 'b', 'c'}",
            "list{'a', 'b,c'}",
            // The uncompilable pattern: the engine's own `false`, not the lane's verdict.
            "false",
        ]
    );
    assert_eq!(
        steins_catalog::portability_class("preg_split"),
        Some(steins_catalog::PortabilityClass::Refused)
    );
}

/// The alias rows: PHP's own second spellings of four names already here. The
/// pairs fold identically because they *are* the same function — one C handler
/// reached by two names — and the test asserts that by folding both spellings
/// of the same call and comparing, rather than by pinning four literals that
/// would pass just as well if the aliases had drifted apart.
#[test]
fn the_alias_rows_fold_exactly_as_the_names_they_alias() {
    let Some(mut folder) = live("the_alias_rows_fold_exactly_as_the_names_they_alias") else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(implode(\",\", [\"a\", \"b\"]));\n\
         \\PHPStan\\dumpType(join(\",\", [\"a\", \"b\"]));\n\
         \\PHPStan\\dumpType(rtrim(\"ab   \"));\n\
         \\PHPStan\\dumpType(chop(\"ab   \"));\n\
         \\PHPStan\\dumpType(count([\"a\", \"b\", \"c\"]));\n\
         \\PHPStan\\dumpType(sizeof([\"a\", \"b\", \"c\"]));\n\
         \\PHPStan\\dumpType(floatval(\"1.5\"));\n\
         \\PHPStan\\dumpType(doubleval(\"1.5\"));\n";
    let d = dumps(SRC, &mut folder);
    assert_eq!(d.len(), 8);
    for pair in d.chunks(2) {
        assert_eq!(pair[0], pair[1], "an alias must fold to what its target folds to");
    }
    // …and they are values, not the floor that would also compare equal.
    assert_eq!(d[0], "'a,b'");
    assert_eq!(d[2], "'ab'");
    assert_eq!(d[4], "3");
    assert_eq!(d[6], "1.5");
    for name in ["join", "chop", "sizeof", "doubleval"] {
        assert_eq!(
            steins_catalog::portability_class(name),
            Some(steins_catalog::PortabilityClass::Portable),
            "{name} folds in the browser too, on its target's probe family"
        );
    }
}

/// The call site's calling convention crosses the seam (#383).
///
/// `declare(strict_types=1)` binds to the file a call is *written* in, and the
/// fold seam spans two files: the user's and the runner's. Before this, every
/// fold ran in the runner's mode — so a strict file folded
/// `substr("abcdef", "1")` to `'bcdef'` where its own program throws, and the
/// value was `Verified`, the strongest thing the analysis has.
///
/// Both directions are asserted, because each has its own failure:
///
/// * a strict file must **decline** — folding there is unsound;
/// * a weak file must still **fold** — PHP really does coerce, and answering
///   `123` for `abs('123')` is the whole point of asking the engine.
///
/// The two runs share ONE folder, which is the third property: the memo is
/// keyed by the convention, so the file analyzed first cannot answer for the
/// other. With a `(name, args)` key this test passes or fails depending on the
/// order the two sources are checked in.
#[test]
fn the_call_sites_calling_convention_reaches_the_engine() {
    let Some(mut folder) = live("the_call_sites_calling_convention_reaches_the_engine") else {
        return;
    };
    const BODY: &str = "function f(): void {\n\
         \\PHPStan\\dumpType(abs(\"123\"));\n\
         \\PHPStan\\dumpType(substr(\"abcdef\", \"1\"));\n\
         \\PHPStan\\dumpType(substr(\"abcdef\", 1));\n\
         }\n";
    let strict = format!("<?php\ndeclare(strict_types=1);\n{BODY}");
    let weak = format!("<?php\ndeclare(strict_types=0);\n{BODY}");

    // Weak first, so a convention-blind memo would answer the strict file with
    // these — the failure this ordering is chosen to catch.
    let w = dumps(&weak, &mut folder);
    assert_eq!(w[0], "123", "a weak file folds what PHP's coercion really returns");
    assert_eq!(w[1], "'bcdef'", "…and the same for an oversized-looking offset");
    assert_eq!(w[2], "'bcdef'", "a well-typed call is unaffected by any of this");

    let s = dumps(&strict, &mut folder);
    assert_ne!(s[0], "123", "a strict file must not be handed the weak answer");
    assert!(
        !s[1].starts_with('\''),
        "a strict `substr(\"abcdef\", \"1\")` throws, so it widens rather than folding: {}",
        s[1]
    );
    assert_eq!(s[2], "'bcdef'", "a well-typed call folds in either convention");
}

// wave 2: six names chosen to be mechanical, so the probe set doubles as the
// specification a signature-driven generator would encode. Evidence is in the
// ADR-0066 amendment; the shapes are an `int` parameter and a float argument.

/// The offset family. Each takes an `int $offset` and returns a position
/// bounded by the subject, so the width has no path to the value: an oversized
/// offset is a `TypeError` on the narrow engine, which is a decline. `stripos`
/// carries the same ASCII-only caveat `ucwords` does — PHP 8.2 made its case
/// comparison locale-independent, and before that the project's own locale
/// decided, which is exactly the answer folding on the project's own PHP gives.
#[test]
fn the_offset_family_folds_to_positions() {
    let Some(mut folder) = live("the_offset_family_folds_to_positions") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(strpos(\"abcabc\", \"b\"));\n\
         \\PHPStan\\dumpType(strpos(\"abcabc\", \"b\", 2));\n\
         \\PHPStan\\dumpType(strpos(\"abc\", \"z\"));\n\
         \\PHPStan\\dumpType(stripos(\"ABC\", \"b\"));\n\
         \\PHPStan\\dumpType(strrpos(\"abcabc\", \"b\"));\n\
         \\PHPStan\\dumpType(strpos(\"ábc\", \"b\"));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "1",
            "4",
            // The `false` arm is a VALUE here, which the narrowing lane can act
            // on where a declared `int|false` cannot.
            "false",
            "1",
            "4",
            // Byte offsets, not character ones: `á` is two bytes.
            "2",
        ]
    );
}

/// The roundings. Floats are 64-bit doubles on both machines, so the only
/// integer in sight is `round`'s `$precision`, and an oversized one declines.
/// `round`'s edge cases are the ADR-0004 argument in miniature: PHP 8.4's
/// rounding RFC changed which way some of them go, and the engine that answers
/// is the one the project runs.
#[test]
fn the_roundings_fold_to_floats() {
    let Some(mut folder) = live("the_roundings_fold_to_floats") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(round(1.2345, 2));\n\
         \\PHPStan\\dumpType(round(0.285, 2));\n\
         \\PHPStan\\dumpType(floor(1.5));\n\
         \\PHPStan\\dumpType(ceil(1.5));\n\
         \\PHPStan\\dumpType(floor(-1.5));\n";
    assert_eq!(dumps(SRC, &mut folder), vec!["1.23", "0.29", "1.0", "2.0", "-2.0"]);
}

/// An overflowed float literal is **not** a fold argument, in either convention.
///
/// `1e309` has no finite `double`, so PHP's own lexer makes it `INF` — a value
/// the source spells as a literal and the IR carries as `ArgValue::Float`. JSON
/// has no spelling for it: `Number::from_f64(INF)` fails, and a wire encoder
/// that substituted anything at all would ask the engine about a *different*
/// argument. It substituted `null`, and `floor(null)` is `0.0`, so
/// `floor(1e309)` came back `Verified 0.0` where the program's own answer is
/// `INF` — a fabricated value, which is the one failure this seam may never
/// have.
///
/// Both conventions are asserted because they are different requests (#383) and
/// neither may fold: the argument is unrepresentable on the wire, which is a
/// property of the transport rather than of the coercion rules. The runner has
/// refused non-finite *results* since the fold lane opened; this is the same
/// refusal on the way in.
#[test]
fn a_non_finite_float_literal_declines_rather_than_folding() {
    let Some(mut folder) = live("a_non_finite_float_literal_declines_rather_than_folding") else {
        return;
    };
    const BODY: &str = "function f(): void {\n\
         \\PHPStan\\dumpType(floor(1e309));\n\
         \\PHPStan\\dumpType(ceil(-1e309));\n\
         \\PHPStan\\dumpType(round(1e309));\n\
         \\PHPStan\\dumpType(floor([1e309][0]));\n\
         \\PHPStan\\dumpType(floor(1.5));\n\
         }\n";
    for (mode, src) in [
        ("strict", format!("<?php\ndeclare(strict_types=1);\n{BODY}")),
        ("weak", format!("<?php\ndeclare(strict_types=0);\n{BODY}")),
    ] {
        let d = dumps(&src, &mut folder);
        for (i, got) in d.iter().take(4).enumerate() {
            assert_ne!(got, "0.0", "{mode}: dump {i} fabricated PHP's `floor(null)` answer");
            assert_ne!(got, "-0.0", "{mode}: dump {i} fabricated a value for an INF argument");
            assert!(
                got.parse::<f64>().is_err(),
                "{mode}: dump {i} folded an unrepresentable argument to {got}"
            );
        }
        assert_eq!(d[4], "1.0", "{mode}: a finite argument next door still folds");
    }
}

// issue #382: the two names wave 2 withdrew, admitted on the footing the seam
// grew for them — a shape gate for the callback, and an `out_params` row the
// engine's own arginfo now countersigns for the by-ref write.

/// `array_filter` folds only with its callback argument **absent or a literal
/// `null`**, and the hazard calls are the test.
///
/// The allowlist gates the callee; a callable argument is a second callee, and
/// the seam hands string arguments to the runner verbatim. Both hazards below
/// were *measured* on a branch that admitted the name with no gate:
/// `array_filter(["a", "b"], "var_dump")` put the callback's output on stdout
/// ahead of the JSON-RPC reply, desynced the NDJSON stream and poisoned the
/// sidecar; `array_filter(["PATH"], "getenv")` folded to `list{'PATH'}`, which
/// is `getenv` running inside the analysis with its answer reaching the value
/// domain.
///
/// The last two dumps are the ones that matter most: a fold *after* the refused
/// calls still answers, which is the "the sidecar was never poisoned" claim
/// stated as a value rather than as an absence.
#[test]
fn array_filter_folds_only_with_no_callback() {
    let Some(mut folder) = live("array_filter_folds_only_with_no_callback") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(array_filter([1, 0, 2, \"\", \"0\", null]));\n\
         \\PHPStan\\dumpType(array_filter([1, 0], null));\n\
         \\PHPStan\\dumpType(array_filter([\"a\", \"b\"], \"var_dump\"));\n\
         \\PHPStan\\dumpType(array_filter([\"PATH\"], \"getenv\"));\n\
         \\PHPStan\\dumpType(array_filter([5 => \"x\", 9 => \"\"]));\n\
         \\PHPStan\\dumpType(strtoupper(\"still alive\"));\n";
    let d = dumps(SRC, &mut folder);
    // PHP's own falsiness decides, and the kept entries keep their keys.
    assert_eq!(d[0], "array{0: 1, 2: 2}", "the falsy family is PHP's, not ours");
    // Renders as a `list` because what survives is keys `0..n` — the same value,
    // and the shape surface says so.
    assert_eq!(d[1], "list{1}", "a literal null callback is the absent one");
    // The hazards: no value at all, and in particular not the one the callback
    // would have produced.
    assert_ne!(d[2], "array{0: 'a', 1: 'b'}", "a callback argument must not fold");
    assert_ne!(d[3], "array{0: 'PATH'}", "`getenv` must not run inside the analysis");
    for (i, got) in [(2, &d[2]), (3, &d[3])] {
        assert!(
            !got.starts_with("array{"),
            "dump {i} folded a call whose callback argument is a string: {got}"
        );
    }
    // `array_filter` PRESERVES keys — it does not renumber.
    assert_eq!(d[4], "array{5: 'x'}", "gaps survive");
    // And the sidecar is still answering, which is the whole point of refusing
    // BEFORE dispatch rather than after.
    assert_eq!(d[5], "'STILL ALIVE'", "the refused calls did not poison the transport");
}

/// `preg_match` is `REFUSED`, not portable: the two PCRE builds disagree on the
/// inline limit verbs, exactly as they do for `preg_split` (one JITs and ignores
/// them, the other honours them). So it folds here, on a 64-bit engine with the
/// project's own PCRE, and declines in the browser.
///
/// Its `$matches` is by-ref at position 2, and that is sound only because
/// ADR-0077's seeding invalidates the argument: the seam passes by value, so the
/// write is lost, and the variable must come back **unknown** rather than
/// keeping its old value. Since issue #382 the engine's arginfo countersigns
/// that the row is there and names position 2.
#[test]
fn preg_match_folds_and_its_matches_argument_is_invalidated() {
    let Some(mut folder) = live("preg_match_folds_and_its_matches_argument_is_invalidated") else {
        return;
    };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(preg_match(\"/a/\", \"abc\"));\n\
         \\PHPStan\\dumpType(preg_match(\"/z/\", \"abc\"));\n\
         \\PHPStan\\dumpType(preg_match(\"/[/\", \"abc\"));\n";
    assert_eq!(
        dumps(SRC, &mut folder),
        vec![
            "1",
            // A miss is `0`, a VALUE the narrowing lane can act on where the
            // declared `int|false` cannot.
            "0",
            // An uncompilable pattern is `false` — the third arm, and a value too.
            "false",
        ]
    );
    assert!(steins_catalog::foldable("preg_match"));
    assert!(!steins_catalog::portable("preg_match"), "the two PCRE builds disagree");
    assert_eq!(
        steins_catalog::refusal("preg_match").map(|r| r.axis),
        Some(steins_catalog::RefusalAxis::BuildOption),
        "refused for how PCRE was compiled, not for the machine word"
    );
}

/// The **untyped variadic tail** is refused at the seam, not only in a catalog
/// test (issue #382's last shape).
///
/// `sprintf` is `REFUSED` for the machine word, so it folds on this 64-bit
/// engine — and its tail is the one the catalog argues carries data, rendered by
/// the format string. That is the half of the rule that has to keep working: a
/// gate which refused every untyped tail would silently stop folding it.
///
/// The other half cannot be exercised end to end today, because no name whose
/// tail hides a comparator is on the allowlist — which is the point of the
/// tripwire beside it. `array_udiff` is asserted off the list here so that
/// admitting it without reading this test fails a different one.
#[test]
fn an_argued_variadic_tail_still_folds() {
    let Some(mut folder) = live("an_argued_variadic_tail_still_folds") else { return };
    const SRC: &str = "<?php\n\
         \\PHPStan\\dumpType(sprintf(\"%s-%d\", \"a\", 7));\n\
         \\PHPStan\\dumpType(sprintf(\"%05.2f\", 1.5));\n\
         \\PHPStan\\dumpType(sprintf(\"no args\"));\n";
    assert_eq!(dumps(SRC, &mut folder), vec!["'a-7'", "'01.50'", "'no args'"]);
    assert!(
        steins_catalog::variadic_tail_is_data("sprintf"),
        "the gate lets it through because the catalog argues the tail is values"
    );
    assert!(
        !steins_catalog::variadic_tail_is_data("array_udiff")
            && !steins_catalog::foldable("array_udiff"),
        "the family the rule exists for is neither argued nor admitted"
    );
}
