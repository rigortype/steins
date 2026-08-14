//! PHP strings are byte strings (ADR-0080, issue #208).
//!
//! Lowering a string literal through `String::from_utf8_lossy` turned every
//! invalid-UTF-8 byte into one U+FFFD, so `"\xC0"` and `"\xD0"` — two distinct
//! PHP values — compared **equal** in the value lane. Equality there is a proof
//! premise, so the collapse manufactured wrong answers in both directions.
//!
//! These are the three defects measured against `steins check` before the fix,
//! each with the PHP runtime as the oracle. The literals are spelled as PHP
//! escapes (`"\xC0"` in the PHP source, `\\xC0` in the Rust string), which the
//! parser decodes to a raw byte — the same shape as
//! `corpus/symfony__console/Helper/QuestionHelper.php:356`.

use steins_infer::{
    CALL_ON_NULL_ID, DEBUG_TYPE_ID, Diagnostic, Folder, OFFSET_MISSING_ID, SidecarFolder, check,
    check_with,
};
use steins_syntax::{ArgValue, SourceTree};

/// The `debug.type` body for `$x = <expr>;` — the surface that observes a fold.
fn dumped(expr: &str) -> String {
    let src = format!("<?php\n$x = {expr};\n\\PHPStan\\dumpType($x);\n");
    let tree = SourceTree::parse(&src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one dump for `{expr}`, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

/// A boot surface that is present and monkey-patch-free, so the value-domain
/// absence proofs may fire (ADR-0049 A9). It never folds — every claim below
/// rests on the lowered literals alone, not on the sidecar.
struct Ready;

impl Folder for Ready {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
}

fn of_id(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Ready)
        .into_iter()
        .filter(|d| d.id == id)
        .collect()
}

/// The false positive. `$s` is one of two DISTINCT byte strings, so the two
/// guards below are mutually exclusive and no path dereferences null. While
/// both literals decoded to U+FFFD the ternary join collapsed `OneOf` to a
/// `Singleton`, `php_identical` decided **both** guards `Yes`, and the walk
/// took the null assignment and the dereference as one proven path — a
/// proof-layer `call.on-null` on a state the program cannot reach, which is
/// exactly what ADR-0002 forbids.
///
/// Verified on PHP 8.5: `$c = true` prints the year, `$c = false` enters
/// neither branch body that would break.
#[test]
fn distinct_byte_strings_do_not_forge_a_proven_null_path() {
    let src = "<?php\n\
        function demo(bool $c): void {\n\
            $s = $c ? \"\\xC0\" : \"\\xD0\";\n\
            $obj = new DateTimeImmutable();\n\
            if ($s === \"\\xD0\") {\n\
                $obj = null;\n\
            }\n\
            if ($s === \"\\xC0\") {\n\
                echo $obj->format('Y');\n\
            }\n\
        }\n";
    let d = of_id(src, CALL_ON_NULL_ID);
    assert!(d.is_empty(), "no path dereferences null: {d:#?}");
}

/// The same shape with two spellings of the *same* byte string still decides,
/// so the fix bought soundness without giving up the true positive: `$s` is
/// proven `"\xC0"`, the guard holds, and the dereference is proven.
#[test]
fn one_byte_string_still_proves_the_null_path() {
    let src = "<?php\n\
        function demo(): void {\n\
            $s = \"\\xC0\";\n\
            $obj = new DateTimeImmutable();\n\
            if ($s === \"\\xC0\") {\n\
                $obj = null;\n\
            }\n\
            echo $obj->format('Y');\n\
        }\n";
    let d = of_id(src, CALL_ON_NULL_ID);
    assert_eq!(d.len(), 1, "{d:#?}");
}

/// The false negative. PHP warns `Undefined array key` here; while the keys
/// collapsed, `array_has_key` found the read key "present" and the finding was
/// suppressed.
#[test]
fn a_byte_string_key_that_is_absent_is_proven_absent() {
    let src = "<?php\n$a = [\"\\xC0\" => 1];\n$b = $a[\"\\xD0\"];\n";
    let d = of_id(src, OFFSET_MISSING_ID);
    assert_eq!(d.len(), 1, "{d:#?}");
}

/// …and the key that IS there stays silent, so the fix did not simply make the
/// lane noisy: the same two literals decide both ways.
#[test]
fn a_byte_string_key_that_is_present_stays_silent() {
    let src = "<?php\n$a = [\"\\xC0\" => 1];\n$b = $a[\"\\xC0\"];\n";
    let d = of_id(src, OFFSET_MISSING_ID);
    assert!(d.is_empty(), "{d:#?}");
}

/// A diagnostic naming a byte string spells it the way PHP source does. The
/// old rendering printed the lossy replacement character, which named nothing
/// the reader could search for.
#[test]
fn a_diagnostic_spells_byte_strings_as_php_escapes() {
    let src = "<?php\n$a = [\"\\xC0\" => 1];\n$b = $a[\"\\xD0\"];\n";
    let d = of_id(src, OFFSET_MISSING_ID);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains(r"\xD0"), "{}", d[0].message);
    assert!(d[0].message.contains(r"\xC0"), "{}", d[0].message);
    assert!(!d[0].message.contains('\u{FFFD}'), "{}", d[0].message);
}

/// PHP's `.` joins **bytes**, so two halves that are each invalid UTF-8 can
/// concatenate to a perfectly valid string: `"\xC3" . "\xA9"` is `"é"`. While
/// the halves decoded lossily this folded to two replacement characters — a
/// third string, equal to neither the source nor the truth.
#[test]
fn concatenation_joins_bytes_not_decoded_text() {
    assert_eq!(dumped(r#""\xC3" . "\xA9""#), "'é'");
}

/// A byte string survives concatenation as itself when the join does not
/// complete a code point.
#[test]
fn a_byte_string_survives_concatenation() {
    assert_eq!(dumped(r#""\xC0" . """#), r#""\xC0""#);
    assert_ne!(dumped(r#""\xC0" . """#), dumped(r#""\xD0" . """#));
}

/// The fold wire is JSON and cannot carry arbitrary bytes, so a byte-string
/// argument is not sent at all (ADR-0080 §2.6): `strlen` falls back to its
/// declared return envelope instead of folding the **wrong** `3` that a lossy
/// three-byte U+FFFD used to produce. PHP's answer is `1`; restoring that exact
/// fold is ADR-0080 §3.1.
#[test]
fn a_byte_string_is_not_sent_to_the_fold_wire() {
    let got = dumped(r#"strlen("\xC0")"#);
    assert_ne!(got, "3", "the lossy byte length must not be folded");
    assert!(
        got.starts_with("int<"),
        "an int envelope, not a constant: {got}"
    );
}

/// The `dumpType` body for `$x = <expr>;` against a **live** sidecar, or `None`
/// when `php` cannot be reached.
///
/// The two pins below need the real engine, not [`dumped`]'s `NoFold`: their
/// subject is what the *runner* does when a call whose arguments are perfectly
/// sendable returns bytes JSON cannot carry. Only a request that actually reaches
/// PHP exercises that branch.
fn live_dumped(test: &str, expr: &str) -> Option<String> {
    let mut folder = SidecarFolder::enabled();
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())]).is_none() {
        eprintln!("SKIP {test}: no folding engine — is `php` on PATH?");
        return None;
    }
    let src = format!("<?php\n$x = {expr};\n\\PHPStan\\dumpType($x);\n");
    let tree = SourceTree::parse(&src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check_with(&tree, &functions, "test.php", &mut folder)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one dump for `{expr}`, got {ds:?}");
    Some(ds[0].message.replace("dumped type: ", ""))
}

/// The mirror of the test above, on the **result** side: the argument here is
/// plain ASCII and is sent, and it is the *answer* that JSON cannot carry.
/// `base64_decode('wA==')` is the single byte `\xC0`, so the runner widens rather
/// than reporting a lossy U+FFFD, and the call falls back to its declared envelope.
///
/// This branch existed in the runner from the start and was pinned nowhere, in
/// either result form. Restoring the exact fold is ADR-0080 §3.1's tagged bytes.
#[test]
fn a_scalar_byte_string_result_widens_rather_than_folding_lossily() {
    let Some(got) = live_dumped(
        "a_scalar_byte_string_result_widens_rather_than_folding_lossily",
        r#"base64_decode("wA==")"#,
    ) else {
        return;
    };
    // `unknown` is this harness's envelope: a declined fold leaves the call with
    // whatever the surface declares, and a one-file check with no project
    // reflection declares nothing for `base64_decode`. The pin that matters is
    // negative — no lossy value appears where PHP produced a byte.
    assert_eq!(got, "unknown", "the envelope, not a lossy value");
    // The sibling that IS representable folds, so the widen above is the value's
    // bytes and not a disabled folder.
    let ascii = live_dumped("a_scalar_byte_string_result_widens_rather_than_folding_lossily", r#"base64_decode("YWJj")"#);
    assert_eq!(ascii.as_deref(), Some("'abc'"));
}

/// And the array form, now that array results cross the seam (ADR-0028's
/// 2026-08-14 amendment, issue #330): one byte string **anywhere** inside widens
/// the whole result, because a partial array would be a wrong value rather than a
/// wider one.
///
/// `substr_replace` slices **bytes**, so cutting one byte off `"À"` (`C3 80`)
/// leaves a lone `\x80` — an array result that no argument had to be binary to
/// produce. The wave-0 name reaching its own new result path is the point.
#[test]
fn a_byte_string_inside_an_array_result_widens_the_whole_array() {
    let Some(got) = live_dumped(
        "a_byte_string_inside_an_array_result_widens_the_whole_array",
        r#"substr_replace(["Àb"], "", 0, 1)"#,
    ) else {
        return;
    };
    assert!(!got.starts_with("list{"), "no array value survives a binary element: {got}");
    // The same call over a subject whose bytes DO survive JSON folds, so the widen
    // above is the encoding and not the array path.
    let clean = live_dumped(
        "a_byte_string_inside_an_array_result_widens_the_whole_array",
        r#"substr_replace(["ab"], "", 0, 1)"#,
    );
    assert_eq!(clean.as_deref(), Some("list{'b'}"));
}
