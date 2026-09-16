//! Issue #168 (slice D of #148) — the `preg_match_all` out-parameter seed and
//! the shape-changing flags.
//!
//! Witness: ADR-0077's `ReturnTruthy` — return is `int|false`, truthy means an
//! int >= 1, proving the pattern compiled and at least one match landed, so on
//! the truthy branch every PATTERN_ORDER column is a written, non-empty list.
//! `ret = 0` also writes (empty columns — measured) but is indistinguishable
//! from `false` on the falsy branch, which stays unseeded.
//!
//! Every shape claim below was measured on PHP 8.5.9. Padding rule:
//! `preg_match_all('/(\d)(a)?/', '1a 2 3a', $m)` gives
//! `[['1a','2','3a'], ['1','2','3'], ['a','','a']]` — every column has exactly
//! `ret` entries; an unmatched group contributes `''` **wherever it sits**
//! (`preg_match`'s trailing-absence rule does not apply to a column element).

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

/// Every `debug.type` message body a source produces, in source order.
fn dumps(src: &str) -> Vec<String> {
    diagnostics(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The single dump a one-dump source produces.
fn one_dump(src: &str) -> String {
    let d = dumps(src);
    assert_eq!(d.len(), 1, "expected exactly one dump, got {d:?}");
    d[0].clone()
}

/// `if (preg_match_all(<pattern>, $s, $m <, flags>)) { dumpType($m); }`.
fn all_shape(pattern: &str, flags: Option<&str>) -> String {
    let flags = flags.map(|f| format!(", {f}")).unwrap_or_default();
    one_dump(&format!(
        "<?php\nfunction f(string $s): void {{\n\
         if (preg_match_all({pattern}, $s, $m{flags})) {{ \\PHPStan\\dumpType($m); }}\n}}\n"
    ))
}

/// The `preg_match` per-match shape for the same pattern/flags — what one
/// SET_ORDER set must equal, from the same run.
fn per_match_shape(pattern: &str, flags: Option<&str>) -> String {
    let flags = flags.map(|f| format!(", {f}")).unwrap_or_default();
    one_dump(&format!(
        "<?php\nfunction f(string $s): void {{\n\
         if (preg_match({pattern}, $s, $m{flags})) {{ \\PHPStan\\dumpType($m); }}\n}}\n"
    ))
}

/// Every refusal spells the same way: no fact at all.
fn refuses(src: &str) {
    let d = one_dump(src);
    assert_eq!(d, "unknown", "expected a silent refusal, got `{d}`");
}

// ---- PATTERN_ORDER: padded columns ------------------------------------------

#[test]
fn pattern_order_writes_one_padded_column_per_group() {
    // Column 0 is the whole-expression refinement (slice E); column 1 keeps its
    // body refinement (group cannot go unmatched); column 2 is padded with `''`
    // — its element is the body unioned with `''`, spelled by the literal
    // enumeration (issue #177) as `''|'a'`.
    assert_eq!(
        all_shape(r"'/(\d)(a)?/'", None),
        "list{non-empty-list<non-empty-string>, non-empty-list<numeric-string>, \
         non-empty-list<''|'a'>} (asserted)"
    );
}

#[test]
fn a_trailing_optional_group_is_still_a_padded_required_column() {
    // THE TRAP (issue #168): under `preg_match`, `(b)?` in `/(a)(b)?/` is an
    // absent-able OPTIONAL key. In a PATTERN_ORDER column trailing-absence
    // doesn't exist: the column is always written, padded with `''` wherever
    // the group sits (measured: `preg_match_all('/(a)(b)?/', 'a', $m)` gives
    // `[['a'], ['a'], ['']]`). Both claims are pinned against the same run's
    // `preg_match` shape, so the divergence itself is the assertion.
    assert_eq!(
        all_shape("'/(a)(b)?/'", None),
        "list{non-empty-list<non-empty-string>, non-empty-list<'a'>, \
         non-empty-list<''|'b'>} (asserted)"
    );
    assert_eq!(
        per_match_shape("'/(a)(b)?/'", None),
        "list{0: non-empty-string, 1: 'a', 2?: 'b'} (asserted)"
    );
    // An interior optional group reads identically in its column — position is
    // not consulted (measured: `'abc ac'` gives `$m[2] === ['b', '']`).
    assert_eq!(
        all_shape("'/(a)(b)?(c)/'", None),
        "list{non-empty-list<non-falsy-string>, non-empty-list<'a'>, \
         non-empty-list<''|'b'>, non-empty-list<'c'>} (asserted)"
    );
}

#[test]
fn an_explicit_pattern_order_flag_is_the_default() {
    // `PREG_PATTERN_ORDER` (= 1, verified via `php -r`) adds no information —
    // the two spellings must produce the identical fact, resolving by value.
    let default = all_shape(r"'/(\d)(a)?/'", None);
    assert_eq!(all_shape(r"'/(\d)(a)?/'", Some("PREG_PATTERN_ORDER")), default);
    assert_eq!(all_shape(r"'/(\d)(a)?/'", Some("1")), default);
}

#[test]
fn named_groups_put_the_name_beside_its_numeric_twin() {
    // Measured: `preg_match_all('/(?<d>\d)(a)?/', '1a 2', $m)` writes columns
    // `0, d, 1, 2` — the name is one more column key beside the numeric twin,
    // both always present, which is also what makes the outer array no list.
    assert_eq!(
        all_shape(r"'/(?<d>\d)(a)?/'", None),
        "array{0: non-empty-list<non-empty-string>, 1: non-empty-list<numeric-string>, \
         2: non-empty-list<''|'a'>, d: non-empty-list<numeric-string>} (asserted)"
    );
}

// ---- SET_ORDER: the per-match constructor, reused ---------------------------

#[test]
fn set_order_is_a_non_empty_list_of_preg_match_success_shapes() {
    // Issue #168 rule 3, pinned by comparing both paths on one pattern IN THE
    // SAME RUN: each SET_ORDER set follows `preg_match`'s success-shape rules
    // (measured: `PREG_SET_ORDER` on `/(\d)(a)?/` over `'1a 2 3a'` gives
    // `[['1a','1','a'], ['2','2'], ['3a','3','a']]`; trailing absence applies per set).
    for (pattern, flags) in [
        (r"'/(\d)(a)?/'", None),
        ("'/(a)(b)?(c)/'", None),
        (r"'/(?<d>\d)(a)?/'", None),
        // Per-set entries follow the flag variants too, through the one
        // constructor: nullability and offset pairs (measured:
        // `PREG_SET_ORDER|PREG_UNMATCHED_AS_NULL` gives `[['1a','1','a'], ['2','2',null]]`).
        (r"'/(\d)(a)?/'", Some("514")),
        (r"'/(\d)(a)?/'", Some("258")),
    ] {
        let per_match_flags = flags.map(|f| {
            // Strip the SET_ORDER bit (2): the remainder is the preg_match
            // spelling of the same entry flags.
            let n: i64 = f.parse().expect("numeric flag fixture");
            (n & !2).to_string()
        });
        let set = all_shape(pattern, flags.or(Some("PREG_SET_ORDER")));
        let per_match = per_match_shape(pattern, per_match_flags.as_deref());
        let body = per_match
            .strip_suffix(" (asserted)")
            .expect("per-match dump carries the asserted mark");
        assert_eq!(
            set,
            format!("non-empty-list<{body}> (asserted)"),
            "one constructor for {pattern} {flags:?}"
        );
    }
}

// ---- The shape-changing flags in PATTERN_ORDER ------------------------------

#[test]
fn unmatched_as_null_turns_the_padding_into_null() {
    // Measured: `PREG_UNMATCHED_AS_NULL` on `/(\d)(a)?/` over `'1a 2'` gives
    // `[['1a','2'], ['1','2'], ['a',null]]` — `''` padding becomes explicit
    // `null`: the element keeps its literal body (issue #177) and gains `|null`.
    assert_eq!(
        all_shape(r"'/(\d)(a)?/'", Some("PREG_UNMATCHED_AS_NULL")),
        "list{non-empty-list<non-empty-string>, non-empty-list<numeric-string>, \
         non-empty-list<'a'|null>} (asserted)"
    );
}

#[test]
fn offset_capture_wraps_column_elements_in_measured_pairs() {
    // Measured: `PREG_OFFSET_CAPTURE` on `/(\d)(a)?/` over `'1a 2'` gives
    // `[[['1a',0],['2',3]], [['1',0],['2',3]], [['a',1],['',-1]]]` — padded entry
    // is `['', -1]`, so `-1` reaches exactly the padded columns, floor 0 elsewhere.
    assert_eq!(
        all_shape(r"'/(\d)(a)?/'", Some("PREG_OFFSET_CAPTURE")),
        "list{non-empty-list<list{non-empty-string, int<0, max>}>, \
         non-empty-list<list{numeric-string, int<0, max>}>, \
         non-empty-list<list{''|'a', int<-1, max>}>} (asserted)"
    );
    // Both flags at once (256 | 512 = 768, a proven int): the pad pair is
    // `[null, -1]` (measured), so the pair text is nullable and the floor is -1.
    assert_eq!(
        all_shape(r"'/(\d)(a)?/'", Some("768")),
        "list{non-empty-list<list{non-empty-string, int<0, max>}>, \
         non-empty-list<list{numeric-string, int<0, max>}>, \
         non-empty-list<list{'a'|null, int<-1, max>}>} (asserted)"
    );
}

// ---- The witness seam -------------------------------------------------------

#[test]
fn the_falsy_branch_and_the_unguarded_call_stay_unseeded() {
    // `0` writes empty columns, `false` writes nothing (both measured); the
    // falsy branch can't tell them apart — ADR-0077's discipline, unchanged here.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match_all('/(a)/', $s, $m)) { } else { \\PHPStan\\dumpType($m); }\n}\n",
    );
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         preg_match_all('/(a)/', $s, $m);\n\\PHPStan\\dumpType($m);\n}\n",
    );
}

#[test]
fn a_comparison_witness_rides_the_existing_machinery() {
    // `=== 2` admits no falsy value, so the #162 comparison witness proves the
    // write — with no preg_match_all-specific code (issue #168 rule 1).
    let src = "<?php\nfunction f(string $s): void {\n\
               if (preg_match_all('/(a)/', $s, $m) === 2) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(
        one_dump(src),
        "list{non-empty-list<non-empty-string>, non-empty-list<'a'>} (asserted)"
    );
    // `!== 0` does not: `false` also satisfies it, and `false` wrote nothing.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match_all('/(a)/', $s, $m) !== 0) { \\PHPStan\\dumpType($m); }\n}\n",
    );
    // `> 0` is truthiness-equivalent for `int|false`, but the ordering
    // comparison declines before a witness is asked (ADR-0077 #162: admitting
    // orderings is a separate change) — pinned so this line moves the day it does.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match_all('/(a)/', $s, $m) > 0) { \\PHPStan\\dumpType($m); }\n}\n",
    );
}

// ---- The refusals (each one silent, each one today's behavior) --------------

#[test]
fn the_flag_gate_declines_anything_unmodeled() {
    // Both order bits together: measured `ValueError`, nothing written.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match_all('/(a)/', $s, $m, 3)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
    // An unknown bit poisons the whole value, however many known bits ride along.
    for flags in ["4", "260", "$flags"] {
        refuses(&format!(
            "<?php\nfunction f(string $s, int $flags): void {{\n\
             if (preg_match_all('/(a)/', $s, $m, {flags})) {{ \\PHPStan\\dumpType($m); }}\n}}\n"
        ));
    }
    // A `|` of constants is an expression the lowering does not fold, so it is
    // not a proven int — a silent decline today, not a wrong value.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match_all('/(a)/', $s, $m, PREG_OFFSET_CAPTURE|PREG_UNMATCHED_AS_NULL)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
}

#[test]
fn the_shared_refusals_apply_to_the_second_name_too() {
    // Unproven pattern.
    refuses(
        "<?php\nfunction f(string $s, string $re): void {\n\
         if (preg_match_all($re, $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
    // Reader decline (`x` changes what counts as a group).
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match_all('/(a)/x', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
    // A non-variable out-parameter (ADR-0077 §3.6).
    let src = "<?php\nclass C {\npublic array $m = [];\n\
               public function f(string $s): void {\n\
               if (preg_match_all('/(a)/', $s, $this->m)) { \\PHPStan\\dumpType($this->m); }\n}\n}\n";
    let d = one_dump(src);
    assert!(!d.contains("non-empty-list"), "a property out-parameter must not be seeded: {d}");
}
