//! ADR-0078 / issue #188: `call.printf-too-few-arguments`.
//!
//! A `printf`-family call whose FOLDED LITERAL format string demands more
//! placeholders than the call proves it supplies is a fatal in PHP 8 — but
//! the evidence is the format string, not a resolved callee signature, so
//! this is a distinct id from `call.too-few-arguments` (ADR-0078's naming
//! decision), keeping `call.too-many-arguments` (the M2 internal-arity slot)
//! clean of format-derived claims.
//!
//! Runtime claims below were `php -r`-checked against PHP 8.5.9; witnesses
//! also live next to the checker in `crates/steins-infer/src/lib.rs`. The
//! ADR-0002/0049 §6 asymmetry rule applies here too: too FEW proven
//! placeholder-arguments is a finding, too MANY never is — this file pins
//! both directions.

use steins_infer::{CALL_PRINTF_TOO_FEW_ARGUMENTS_ID, Diagnostic, check};
use steins_syntax::SourceTree;

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

/// Every `call.printf-too-few-arguments` finding a source produces.
fn fires(src: &str) -> Vec<Diagnostic> {
    diagnostics(src).into_iter().filter(|d| d.id == CALL_PRINTF_TOO_FEW_ARGUMENTS_ID).collect()
}

/// Silence assertion: no `call.printf-too-few-arguments` finding at all.
fn silent(src: &str) {
    let d = fires(src);
    assert!(d.is_empty(), "expected silence, got {d:?}");
}

// Firing fixtures.

#[test]
fn printf_plain_too_few() {
    // ArgumentCountError: 3 arguments are required, 2 given.
    let d = fires("<?php\nprintf(\"%s %s\", \"one\");\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "too few arguments to printf(): format needs 2 placeholder argument(s), \
         1 supplied — provable ArgumentCountError: 3 arguments are required, 2 given"
    );
}

#[test]
fn sprintf_positional_reference_needs_three() {
    // ArgumentCountError: 4 arguments are required, 3 given. Positional
    // references count by MAX position, not additively (`%1$s` alone needs 1).
    let d = fires("<?php\nsprintf(\"%1\\$s %3\\$s\", \"a\", \"b\");\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "too few arguments to sprintf(): format needs 3 placeholder argument(s), \
         2 supplied — provable ArgumentCountError: 4 arguments are required, 3 given"
    );
}

#[test]
fn fprintf_format_at_offset_one() {
    // ArgumentCountError: 4 arguments are required, 3 given. `fprintf`'s
    // format is the SECOND argument (offset 1); the stream at offset 0 is
    // never consulted.
    let d = fires("<?php\nfprintf(STDOUT, \"%s %s\", \"one\");\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "too few arguments to fprintf(): format needs 2 placeholder argument(s), \
         1 supplied — provable ArgumentCountError: 4 arguments are required, 3 given"
    );
}

#[test]
fn vsprintf_fires_on_a_proven_literal_array() {
    // ValueError: The arguments array must contain 2 items, 1 given.
    let d = fires("<?php\nvsprintf(\"%s %s\", [\"one\"]);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "too few arguments to vsprintf(): format needs 2 placeholder argument(s), \
         array holds 1 — provable ValueError: The arguments array must contain 2 items, \
         1 given"
    );
}

#[test]
fn vprintf_fires_on_a_proven_literal_array() {
    // ValueError: The arguments array must contain 2 items, 1 given (same
    // shape as vsprintf).
    let d = fires("<?php\nvprintf(\"%s %s\", [\"one\"]);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("vprintf()"), "{}", d[0].message);
}

#[test]
fn flags_width_precision_are_parsed_and_too_few_fires() {
    // ArgumentCountError: 4 arguments are required, 2 given. Three specifiers
    // survive width/precision/flags/custom-pad parsing: `%05.2f`, `%-10s`,
    // `%'x10d`.
    let d = fires("<?php\nsprintf(\"%05.2f %-10s %'x10d\", 1.0);\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("needs 3 placeholder argument(s)"), "{}", d[0].message);
    assert!(d[0].message.contains("1 supplied"), "{}", d[0].message);
}

// Silence fixtures — the §10-style silence matrix: one fixture per failing ladder leg.

#[test]
fn enough_args_is_silent() {
    // Runs clean.
    silent("<?php\nsprintf(\"%s %s\", \"one\", \"two\");\n");
}

#[test]
fn too_many_args_is_never_a_finding() {
    // string(3) "one" — ADR-0002/0049 §6 asymmetry: too MANY is never a finding.
    silent("<?php\nsprintf(\"%s\", \"one\", \"two\");\n");
}

#[test]
fn percent_percent_is_not_a_placeholder() {
    // string(4) "100%" — no argument required at all.
    silent("<?php\nsprintf(\"100%%\");\n");
}

#[test]
fn unknown_conversion_char_declines_the_whole_format() {
    // ValueError: Unknown format specifier "z". Steins never guesses an
    // unrecognized specifier's requirement — whole format is UNPROVEN, so this
    // stays silent even though the call fatals at runtime for another reason.
    silent("<?php\nsprintf(\"%z\");\n");
}

#[test]
fn non_folded_format_is_silent() {
    // A format string that doesn't fold to a proven `Singleton` (an ordinary,
    // unbound function parameter here) is silence — never guessed.
    silent("<?php\nfunction f($fmt) {\n    sprintf($fmt, \"one\");\n}\n");
}

#[test]
fn argument_unpacking_is_silent() {
    // `...$args` makes the supplied count a runtime value — unproven, so the
    // `positional_only` call-site gate declines before the format is consulted.
    silent("<?php\nfunction f(array $args) {\n    sprintf(\"%s %s\", ...$args);\n}\n");
}

#[test]
fn namespaced_userland_printf_is_not_the_builtin() {
    // PHP's global-fallback rule: inside `namespace App;`, unqualified
    // `printf(...)` resolves to `App\printf` (userland shadow) FIRST when
    // declared, never the global builtin — mirrors how `global_function_callee`
    // / `denotes_global_function` refuse a namespaced shadow for every builtin.
    silent(
        "<?php\nnamespace App;\nfunction printf($fmt, ...$args) {}\nprintf(\"%s %s\", \"one\");\n",
    );
}

#[test]
fn vsprintf_unknown_size_array_is_silent() {
    // `$a`'s shape is never proven to a concrete literal size (ordinary
    // param), so `v*` stays silent (ADR-0078: "report only against a proven
    // array shape of known size").
    silent("<?php\nfunction f(array $a) {\n    vsprintf(\"%s %s\", $a);\n}\n");
}

#[test]
fn vsprintf_too_many_array_items_is_never_a_finding() {
    // string(1) "a" — extra array items are ignored, never a finding
    // (asymmetry rule applies to the array-length lane too).
    silent("<?php\nvsprintf(\"%s\", [\"a\", \"b\", \"c\"]);\n");
}
