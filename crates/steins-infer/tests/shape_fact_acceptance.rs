//! ADR-0072 end to end: an abstract **array** fact judged against a declared
//! `@param`/`@return`/`@var` contract.
//!
//! Until this slice `admits_fact(ty, Fact::Shape { … })` answered `Maybe` and
//! every case below was silent. The relation now decides, and the three
//! contract-layer consumers turn a definite `No` into a `phpdoc.*-mismatch` —
//! so the hazard here is **inverted** relative to the rest of the analyzer: a
//! wrong `No` is a user-facing false positive, not a missed finding. Each
//! firing test therefore names the witness that makes every value the fact
//! admits a contract violation, and the silent ones are the FP-killer pins.
//!
//! Where the facts come from: a declared `@param array{…}`/`array<K, V>`
//! seeds the contract-arm shape (ADR-0062 S3), a builtin return seeds one
//! through the functionMap (`explode`), and the #81 floor seeds one for a
//! single-array-arm row (`imagecolorsforindex`). All three arrive at the
//! **`Asserted`** stratum, which the phpdoc contract checks accept by design —
//! only the *native* `type.return-mismatch` demands `Verified`.

use steins_infer::{Diagnostic, PARAM_MISMATCH_ID, RETURN_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn param_count(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == PARAM_MISMATCH_ID).count()
}

fn return_count(src: &str) -> usize {
    findings(src).iter().filter(|d| d.id == RETURN_MISMATCH_ID).count()
}

/// `$x` carries the declared shape; `g()` declares `$ty` for its parameter.
fn param_case(declared: &str, ty: &str) -> String {
    format!(
        "<?php
/** @param {ty} $l */ function g($l): void {{}}
/** @param {declared} $x */ function f($x): void {{ g($x); }}
"
    )
}

// ==========================================================================
// (a) An array-bearing argument against an array-*incapable* contract.
// ==========================================================================

/// The witness is any member at all: every value the fact admits is an array,
/// and no array is a string. `explode()` seeds `list<string>` through the
/// functionMap, so this is the abstract-fact path, not the proven-value one.
#[test]
fn a_list_fact_violates_a_string_param() {
    let src = "<?php
/** @param string $s */ function g($s): void {}
function f(string $x): void { $a = explode(',', $x); g($a); }
";
    assert_eq!(param_count(src), 1, "list<string> fact vs @param string → No");
}

#[test]
fn a_declared_shape_violates_the_scalar_contracts() {
    for ty in ["string", "int", "float", "bool", "null", "positive-int", "numeric-string"] {
        assert_eq!(param_count(&param_case("array{a: int}", ty)), 1, "array fact vs @param {ty}");
    }
}

/// The relation answers `No` for a class contract too, but the ADR-0043 stage-4
/// class valve upstream keeps it silent: an undeclared bare identifier may be a
/// `@template` param or a `@phpstan-type` alias. That gate is unchanged here.
#[test]
fn the_class_valve_still_shuts_on_an_unknown_identifier() {
    assert_eq!(param_count(&param_case("array{a: int}", "SomeUnknownName")), 0);
}

/// …and the same fact against contracts that *can* hold an array stays silent.
#[test]
fn a_declared_shape_is_silent_against_array_capable_contracts() {
    for ty in ["mixed", "array", "iterable", "callable", "array<string, int>", "array{a: int}"] {
        assert_eq!(param_count(&param_case("array{a: int}", ty)), 0, "array fact vs @param {ty}");
    }
}

// ==========================================================================
// (b) The `is_list` witness — the denotational trinary, consumed as given.
// ==========================================================================

/// A required string key means no member of the denotation is a list, and
/// `list<int>` admits lists only: the two are disjoint.
#[test]
fn a_string_keyed_shape_violates_a_list_param() {
    assert_eq!(param_count(&param_case("array{a: int}", "list<int>")), 1);
    assert_eq!(param_count(&param_case("array{a: int}", "list{int}")), 1);
    assert_eq!(param_count(&param_case("associative-array<string, int>", "list<int>")), 1);
}

/// The mirror: an `is_list == Yes` fact against Phan's `associative-array`,
/// which rejects every list realization.
#[test]
fn a_list_shape_violates_an_associative_array_param() {
    assert_eq!(param_count(&param_case("list{int, int}", "associative-array<int, int>")), 1);
}

/// A required int key `0` is in every member and `array<string, int>` rejects
/// it — the key half of the map rule, refuting through a *required* field.
#[test]
fn a_list_shape_violates_a_string_keyed_map_param() {
    assert_eq!(param_count(&param_case("list{int, int}", "array<string, int>")), 1);
}

// ==========================================================================
// (c) The compatible cases stay silent — `Yes`/`Maybe` both mean no finding.
// ==========================================================================

#[test]
fn a_compatible_shape_against_a_map_contract_is_silent() {
    assert_eq!(param_count(&param_case("array{a: int}", "array<string, int>")), 0);
    assert_eq!(param_count(&param_case("array{a: int}", "array<array-key, int>")), 0);
    assert_eq!(param_count(&param_case("array{a: int}", "iterable<string, int>")), 0);
    assert_eq!(param_count(&param_case("array{a: int}", "non-empty-array")), 0);
    assert_eq!(param_count(&param_case("list{string, string}", "list<string>")), 0);
}

#[test]
fn a_builtin_seeded_list_against_a_matching_list_param_is_silent() {
    let src = "<?php
/** @param list<string> $l */ function g($l): void {}
function f(string $x): void { $a = explode(',', $x); g($a); }
";
    assert_eq!(param_count(src), 0, "list<string> vs @param list<string> → Yes");
}

// ==========================================================================
// (d) The FP-killer: unknown slots and unknown list-ness refute nothing.
// ==========================================================================

/// Plain `array` knows nothing — no field, an untyped unsealed tail,
/// `is_list == Maybe`. It must refute no array-shaped contract whatsoever, or
/// every `array`-typed parameter in the corpus becomes a finding.
#[test]
fn the_degenerate_shape_fires_nothing() {
    for ty in [
        "array",
        "non-empty-array",
        "list<int>",
        "non-empty-list<string>",
        "array<string, int>",
        "associative-array<array-key, int>",
        "iterable<int, string>",
        "array{q: int}",
        "array{q?: int, ...}",
        "list{int, string}",
        "array{}",
        "callable",
    ] {
        assert_eq!(param_count(&param_case("array", ty)), 0, "plain array vs @param {ty}");
    }
}

/// A shape whose *value* slots the fact domain cannot express (`mixed` lowers
/// to no fact, A-G1a) must not refute through them: only the parts the fact
/// actually knows may.
#[test]
fn unknown_value_slots_fire_nothing() {
    for ty in [
        "array<string, int>",
        "array<string, string>",
        "array{a: int}",
        "array{a: string}",
        "array{a?: int, ...}",
        "iterable<string, int>",
    ] {
        assert_eq!(
            param_count(&param_case("array<string, mixed>", ty)),
            0,
            "unknown-valued map vs @param {ty}"
        );
    }
}

/// An unknown `is_list` is the other unknown, and it must not refute either.
#[test]
fn unknown_list_ness_fires_nothing() {
    for ty in ["list<int>", "list{int}", "non-empty-list<int>", "associative-array<int, int>"] {
        assert_eq!(
            param_count(&param_case("array<array-key, int>", ty)),
            0,
            "unknown list-ness vs @param {ty}"
        );
    }
}

/// A contract field the fact's tail merely *may* supply is a "may", not a
/// "must": the member carrying the key is not proven to violate anything.
#[test]
fn a_may_have_key_fires_nothing() {
    assert_eq!(param_count(&param_case("array<string, int>", "array{a: int}")), 0);
    assert_eq!(param_count(&param_case("array<string, int>", "array{}")), 0);
}

/// A `non-empty-array` fact forces every member to carry *some* entry, but the
/// contract may declare the key it lands on — `['a' => 1]` satisfies
/// `array{a: int}`. Only a contract with no field for it to land in refutes.
#[test]
fn a_forced_entry_does_not_refute_a_contract_that_declares_its_key() {
    assert_eq!(param_count(&param_case("non-empty-array", "array{a: int}")), 0);
    assert_eq!(param_count(&param_case("non-empty-array", "array{a?: int}")), 0);
    assert_eq!(param_count(&param_case("non-empty-array", "array{}")), 1);
}

/// The ADR-0071 §2 union haircut, imported by ADR-0072 §3: an or-fold that
/// ends at `No` degrades unless every member is array-incapable.
#[test]
fn a_union_with_an_array_capable_member_fires_nothing() {
    assert_eq!(param_count(&param_case("array{a: int}", "string|list<int>")), 0);
    assert_eq!(param_count(&param_case("array{a: int}", "list<int>|non-empty-array")), 0);
    // Every member array-incapable → the witness is shared, and the fold's
    // `No` stands.
    assert_eq!(param_count(&param_case("array{a: int}", "string|int")), 1);
}

/// A nullable shape fact denotes the shape's members ∪ `{null}`, and the two
/// halves are judged separately — exactly the split every scalar fact uses.
#[test]
fn a_nullable_shape_fact_splits_its_two_halves() {
    // The array half escapes the non-nullable contract → floor, silent.
    assert_eq!(param_count(&param_case("array{a: int}|null", "array{a: int}")), 0);
    assert_eq!(param_count(&param_case("array{a: int}|null", "?array")), 0);
    // `null` is admitted by `?string` while no array is → mixed → silent.
    assert_eq!(param_count(&param_case("array{a: int}|null", "?string")), 0);
    // Both halves refuted: no array is a string and neither is `null`.
    assert_eq!(param_count(&param_case("array{a: int}|null", "string")), 1);
}

// ==========================================================================
// (e) The #81 floor's shape at the return check — the `Asserted` stratum.
// ==========================================================================

/// `imagecolorsforindex` has a single array arm, so the ADR-0069 floor seeds a
/// `Fact::Shape` for `$r`. The phpdoc return check consumes facts at the
/// `Asserted` stratum by design (the `Verified` gate above it belongs to the
/// *native* `type.return-mismatch`), so the shape reaches the contract layer
/// and the string contract refutes every member of it.
#[test]
fn the_floor_seeded_shape_violates_a_string_return() {
    let src = "<?php
/** @return string */
function f($im, int $i) { $r = imagecolorsforindex($im, $i); return $r; }
";
    assert_eq!(return_count(src), 1, "floor-seeded shape vs @return string → No");
}

/// Its keys are strings, so it is not a list either.
#[test]
fn the_floor_seeded_shape_violates_a_list_param() {
    let src = "<?php
/** @param list<int> $l */ function g($l): void {}
function f($im, int $i): void { $r = imagecolorsforindex($im, $i); g($r); }
";
    assert_eq!(param_count(src), 1, "floor-seeded shape vs @param list<int> → No");
}

/// …and the contract it actually satisfies stays silent.
#[test]
fn the_floor_seeded_shape_is_silent_against_a_matching_map() {
    for ty in ["array<string, int>", "array", "non-empty-array", "iterable<string, int>", "mixed"] {
        let src = format!(
            "<?php
/** @param {ty} $m */ function g($m): void {{}}
function f($im, int $i): void {{ $r = imagecolorsforindex($im, $i); g($r); }}
"
        );
        assert_eq!(param_count(&src), 0, "floor-seeded shape vs @param {ty}");
    }
}

/// The declared shape rides the return check too, and the message names the
/// fact through the ONE speller rather than the old generic "value".
#[test]
fn a_returned_shape_names_itself_in_the_message() {
    let src = "<?php
/** @param array{a: int} $x
 *  @return string */
function f($x) { return $x; }
";
    let out = findings(src);
    let hit = out.iter().find(|d| d.id == RETURN_MISMATCH_ID).expect("fires");
    assert!(
        hit.message.contains("non-empty-array{a: int}"),
        "the shape should spell itself: {}",
        hit.message
    );
}
