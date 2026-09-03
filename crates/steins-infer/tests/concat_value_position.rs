//! Issue #627 — a concatenation in VALUE position answers the predicate table
//! instead of `unknown`.
//!
//! `ArgValue::Concat` has carried the syntax since issue #59, but only the
//! literal seam ever read it, and only when BOTH operands folded. What this file
//! pins is one property per rule, chosen so that **deleting the rule changes the
//! rendering** — the #626 review's finding was that a whole evaluator arm could
//! be removed with the suite still green, because every assertion went through a
//! rung above the lane under test.
//!
//! * **Each predicate cell is pinned by a dump that only that cell decides.**
//!   `$i . 'a'` is `non-falsy-lowercase-string` and nothing else; drop the
//!   casing conjunction and it is `non-falsy-string`.
//! * **The grid is read, not restated.** `$f . ''` answers the float row's
//!   `UPPERCASE ∧ NON_EMPTY` and never a float's decimal spelling, and `$i . ''`
//!   answers the int row — asserted beside `(string) $v`, so the two syntaxes
//!   cannot drift.
//! * **The floor is total.** An array operand, an object operand and a `mixed`
//!   operand all answer `string`, never `unknown` and never the value `'Array'`.
//! * **All four value seams agree.** The dump, the assignment binding, the
//!   projected return and the nested-operand reader answer the same
//!   concatenation identically.
//!
//! # Behavioural witnesses at `PINNED_PHP` (8.5.9, `php -r`)
//!
//! The floor's ruling and the predicate table both rest on measurements, not on
//! recall. The table's cells were brute-forced over a 62-string corpus (every
//! ordered pair, 3,844 concatenations per candidate rule); these are the ones
//! that decided a ruling:
//!
//! * `var_dump([1, 2] . '');` — `Warning: Array to string conversion`, then
//!   `string(5) "Array"`. A warning, not an error: the value IS a string, so the
//!   floor holds. The value `'Array'` is still never stated.
//! * `$f = fopen('php://memory', 'r'); var_dump($f . '');` — `string(14)
//!   "Resource id #5"`.
//! * `new stdClass . 'a'` and `SomeEnum::A . 'a'` both raise `Error: Object of
//!   class … could not be converted to string`. A throw yields no value, so
//!   there is nothing for a floor to be wrong about.
//! * `var_dump('' . '0');` — `string(1) "0"`. `NON_EMPTY` on one side does not
//!   give `NON_FALSY`.
//! * `var_dump('0' . '-1', '0' . '0');` — `'0-1'` and `'00'`. The first is not
//!   numeric at all and the second is not a decimal-int string, which is why
//!   neither `NUMERIC` nor `DECIMAL_INT` composes from the bits (the issue says
//!   the opposite).
//! * `var_dump('-' . '9223372036854775808');` — `'-9223372036854775808'`, a
//!   decimal-int string out of two non-decimal-int operands, so
//!   `NON_DECIMAL_INT` does not compose either.
//! * `var_dump(1.0 . '', -0.0 . '', (1/3) . '', 1e100 . '');` — `'1'`, `'-0'`,
//!   `'0.33333333333333'`, `'1.0E+100'`. Uppercase and non-empty throughout,
//!   never non-falsy (`0.0 . ''` is `'0'`), and `precision`-ini dependent, which
//!   is why no float value is minted.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A mock PHP reflecting only what the fixtures below need: `strlen` for the
/// literal lane (a folded concatenation arrives there as an `ArgValue::Str`, so
/// a fold that did not happen never reaches this function at all).
#[derive(Default)]
struct Mock;

impl Folder for Mock {
    fn fold(
        &mut self,
        name: &str,
        args: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        match (name, args) {
            ("strlen", [steins_syntax::ArgValue::Str(s)]) => {
                Some(steins_syntax::ArgValue::Int(i64::try_from(s.as_bytes().len()).ok()?))
            }
            _ => None,
        }
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        match name.to_ascii_lowercase().as_str() {
            "strlen" => Some("int".to_owned()),
            _ => None,
        }
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        // `(total, required)` — the order that is a silent decline when reversed.
        match name.to_ascii_lowercase().as_str() {
            "strlen" => Some((1, 1)),
            _ => None,
        }
    }
}

/// Every finding a source produces, `untyped.*` dropped (ADR-0078, #200 — it
/// flags the fixtures' own deliberately untyped signatures).
fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// The single dump a one-dump source produces, asserting no other finding came
/// with it — a concatenation's fact must never premise one.
fn one_dump(src: &str) -> String {
    let ds = findings(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a concatenation emitted a finding: {other:?}");
    let d: Vec<String> = ds
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect();
    assert_eq!(d.len(), 1, "expected exactly one dump, got {d:?}");
    d[0].clone()
}

/// `dumpType(<e>);` — a whole expression, no environment at all.
fn dumped(e: &str) -> String {
    one_dump(&format!("<?php\nfunction f(): void {{ \\PHPStan\\dumpType({e}); }}\n"))
}

/// `function f(<params>): void { dumpType(<e>); }` — the declared lane, where
/// each operand reaches the `.` with only its declaration.
fn over(params: &str, e: &str) -> String {
    one_dump(&format!(
        "<?php\nfunction f({params}): void {{ \\PHPStan\\dumpType({e}); }}\n"
    ))
}

/// The same, with phpdoc `@param` lines so a refined param spelling is
/// available. `docs` are joined into one block, one tag per line.
fn over_doc(docs: &[&str], params: &str, e: &str) -> String {
    let block =
        docs.iter().map(|d| format!(" * {d}\n")).collect::<Vec<_>>().join("");
    one_dump(&format!(
        "<?php\n/**\n{block} */\nfunction f({params}): void {{ \\PHPStan\\dumpType({e}); }}\n"
    ))
}

// ---------------------------------------------------------------------------
// The witness
// ---------------------------------------------------------------------------

#[test]
fn the_issue_witness_answers_the_operators_floor() {
    // `literal-string.php:25` — `$string . ''`, the operator's own floor,
    // asserted by phpstan-src and answered `unknown` before this slice.
    assert_eq!(over("string $s", "$s . ''"), "string");
    assert_eq!(over("string $s", "'' . $s"), "string");
}

// ---------------------------------------------------------------------------
// One cell per rule, each pinned by a rendering only that cell decides
// ---------------------------------------------------------------------------

#[test]
fn non_empty_comes_from_either_operand_alone() {
    // The bool row projects to `''|'1'`, whose predicate intersection carries no
    // `NON_EMPTY`; the int row's does. So this dump is `NON_EMPTY` **only** by
    // the disjunction, and the casing conjunction is what is left without it.
    assert_eq!(over("bool $b, int $i", "$b . $i"), "non-empty-uncased-string");
}

#[test]
fn non_falsy_comes_from_one_non_falsy_operand() {
    // `'foo'` is non-falsy and `$s` knows nothing, so only the `nf(a) ∨ nf(b)`
    // disjunct can decide this; without it the answer is `non-empty-string`.
    assert_eq!(over("string $s", "$s . 'foo'"), "non-falsy-string");
    assert_eq!(over("string $s", "'foo' . $s"), "non-falsy-string");
}

#[test]
fn non_falsy_comes_from_two_non_empty_operands() {
    // Neither operand is non-falsy — an `int`'s string may be `'0'` and the
    // literal IS `'0'` — so this is the `ne(a) ∧ ne(b)` disjunct alone. PHP:
    // `var_dump('0' . '0')` is `'00'`, length 2, neither `''` nor `'0'`.
    assert_eq!(over("int $i", "'0' . $i"), "non-falsy-uncased-string");
    assert_eq!(over("int $i", "$i . $i"), "non-falsy-uncased-string");
}

#[test]
fn lowercase_is_conjunctive() {
    // A lowercase-only literal keeps `LOWERCASE` and drops `UPPERCASE`, so this
    // dump is decided by the `LOWERCASE` cell alone; without it, `non-falsy-string`.
    assert_eq!(over("int $i", "$i . 'a'"), "non-falsy-lowercase-string");
}

#[test]
fn uppercase_is_conjunctive() {
    // The mirror, pinned apart from `LOWERCASE` so deleting either cell fails
    // exactly one test.
    assert_eq!(over("int $i", "$i . 'A'"), "non-falsy-uppercase-string");
}

#[test]
fn casing_is_identity_under_the_case_function_not_having_letters() {
    // `''` and `'123'` are both lowercase AND uppercase (`preds.rs:43-49`), so
    // two int spellings compose to `uncased-`, not to nothing.
    assert_eq!(over("int $i, int $j", "$i . $j"), "non-falsy-uncased-string");
}

#[test]
fn an_empty_operand_leaves_the_other_operands_projection_alone() {
    // The identity law, and the reason it is a rung rather than a table cell:
    // "empty" is the absence of `NON_EMPTY`, which a positive-literal bitset
    // cannot state. Both spellings must agree, and both must equal the cast.
    for e in ["$v . ''", "'' . $v"] {
        assert_eq!(over("int $v", e), over("int $v", "(string) $v"), "{e}");
    }
    assert_eq!(over("int $v", "'' . $v"), "numeric-uncased-string");
    assert_eq!(over("string $v", "'' . $v"), "string");
}

#[test]
fn numeric_needs_the_right_operands_bytes_and_a_decimal_int_left() {
    // The one cell that reads actual bytes. `is_numeric('1' . '0')` holds, so
    // every decimal-int operand keeps the result numeric.
    assert_eq!(over("int $i", "$i . '0'"), "non-falsy-numeric-uncased-string");
    // phpstan-src's own `bug-11129.php` rows, each decided by the same probe:
    // `'1.0'`, `'10e-3'` and `'10E3'` keep it, `'-1'`, `'1.1.1'` and `'10eE3'`
    // do not.
    assert_eq!(over("int $i", "$i . '1.0'"), "non-falsy-numeric-uncased-string");
    assert_eq!(over("int $i", "$i . '10E3'"), "non-falsy-numeric-uppercase-string");
    assert_eq!(over("int $i", "$i . '-1'"), "non-falsy-uncased-string");
    assert_eq!(over("int $i", "$i . '1.1.1'"), "non-falsy-uncased-string");
    assert_eq!(over("int $i", "$i . '10eE3'"), "non-falsy-string");
}

#[test]
fn numeric_does_not_compose_from_the_bits_in_either_direction() {
    // The premise the issue gets wrong, pinned as a *decline*. `'0' . '-1'` is
    // `'0-1'`, so a decimal-int left operand and an unknown-bytes decimal-int
    // right operand cannot give `NUMERIC`; and the mirror (`'0' . $positiveInt`)
    // needs a sign the bitset does not record.
    assert_eq!(over("int $i, int $j", "$i . $j"), "non-falsy-uncased-string");
    assert_eq!(
        over_doc(&["@param positive-int $p"], "$p", "'0' . $p"),
        "non-falsy-uncased-string (asserted)"
    );
    // Nor does `DECIMAL_INT` itself: `'0' . '0'` is `'00'`, which is numeric but
    // is not how PHP writes an int back, so the array-key-cast bit is absent and
    // the spelling stays the grid's own `numeric-` rung rather than a value.
    assert_eq!(over("int $i", "$i . '0'"), "non-falsy-numeric-uncased-string");
}

// ---------------------------------------------------------------------------
// The grid is read, not restated
// ---------------------------------------------------------------------------

#[test]
fn the_operand_columns_are_the_cast_grids_own() {
    // `$x . ''` is the identity on `$x`'s string projection, so each of these
    // dumps IS one cell of `php_cast_fact`'s string column, reached through `.`
    // instead of through `(string)`. Asserted side by side so a change to either
    // reader that does not move the other fails here.
    for param in ["int $v", "float $v", "bool $v", "string $v"] {
        let by_concat = over(param, "$v . ''");
        let by_cast = over(param, "(string) $v");
        assert_eq!(by_concat, by_cast, "`$v . ''` vs `(string) $v` on `{param}`");
    }
}

#[test]
fn a_float_operand_contributes_the_measured_row_and_never_a_value() {
    // `float_string_fact()`: `UPPERCASE ∧ NON_EMPTY`, never `NUMERIC`
    // (`is_numeric('NAN')` is false) and never a value (`precision`-ini). So a
    // float literal does not fold even though both operands are literals —
    // phpstan-src's `binary.php:320-321` are declined on purpose.
    assert_eq!(over("float $f", "$f . ''"), "non-empty-uppercase-string");
    assert_eq!(dumped("1.0 . 'b'"), "non-falsy-string");
    assert_eq!(dumped("1.0 . 2.0"), "non-falsy-uppercase-string");
}

// ---------------------------------------------------------------------------
// The value lane, and the bound that declines rather than truncates
// ---------------------------------------------------------------------------

#[test]
fn two_literals_still_fold_to_the_value() {
    // The rung that already worked, kept working: `concat_cast`'s inputs are
    // `Str`/`Int`/`Bool`/`Null` and every one of them folds.
    assert_eq!(dumped("'a' . 'b'"), "'ab'");
    assert_eq!(dumped("1 . 'b'"), "'1b'");
    assert_eq!(dumped("true . 'x'"), "'1x'");
    assert_eq!(dumped("null . 'x'"), "'x'");
}

#[test]
fn a_finite_operand_set_becomes_the_cross_product() {
    // The rung that did not exist: the literal seam can only answer one value,
    // so a union operand answered nothing at all. 1 × 3 and 3 × 1 are inside the
    // cap. These are `bug-11129.php:33-34` verbatim.
    let d = &["@param '0'|'1'|'2' $d"];
    assert_eq!(over_doc(d, "$d", "'0' . $d"), "'00'|'01'|'02' (asserted)");
    assert_eq!(over_doc(d, "$d", "$d . '0'"), "'00'|'10'|'20' (asserted)");
    // 2 × 2, `binary.php:538`'s shape.
    assert_eq!(
        over_doc(&["@param 'foo'|'bar' $u"], "$u", "$u . $u"),
        "'barbar'|'barfoo'|'foobar'|'foofoo' (asserted)"
    );
}

#[test]
fn a_product_over_the_cap_declines_to_the_predicate_answer() {
    // `CAP` is 8 and the bound is charged BEFORE the product is built: 3 × 3 is
    // 9, so the enumeration never happens and the predicate rung answers. A
    // truncated `OneOf` would be a value set the value is not in — unsound in
    // the one direction that matters — where the predicate is merely wider
    // (ADR-0028 §3, the issue #74 ruling).
    assert_eq!(
        over_doc(&["@param 'a'|'b'|'c' $t"], "$t", "$t . $t"),
        "non-falsy-lowercase-string (asserted)"
    );
    // The sharp case, and the reason "before" is not a stylistic preference: an
    // OVERLAPPING product collapses under dedup, so a bound charged on the
    // finished list would let these nine combinations through as the five
    // distinct values `'aa'|'aaa'|'aaaa'|'aaaaa'|'aaaaaa'`. The bound is on the
    // combination count, so it declines instead.
    assert_eq!(
        over_doc(&["@param 'a'|'aa'|'aaa' $t"], "$t", "$t . $t"),
        "non-falsy-lowercase-string (asserted)"
    );
    // Exactly at the cap it still enumerates, so the bound is `> CAP` and not
    // `>= CAP`: 2 × 4 is 8.
    assert_eq!(
        over_doc(
            &["@param 'a'|'b' $p", "@param 'w'|'x'|'y'|'z' $q"],
            "$p, $q",
            "$p . $q"
        ),
        "'aw'|'ax'|'ay'|'az'|'bw'|'bx'|'by'|'bz' (asserted)"
    );
}

// ---------------------------------------------------------------------------
// Totality: every operand the operator completes on
// ---------------------------------------------------------------------------

#[test]
fn the_floor_holds_for_every_operand_and_never_states_the_array_value() {
    // `[1, 2] . ''` is `'Array'` at PINNED_PHP — a warning, not an error, so the
    // floor is right. The grid still declines an array input, so the value
    // `'Array'` is never stated and `settype($v, 'string')` is untouched.
    assert_eq!(over("array $a", "$a . ''"), "string");
    assert_eq!(over("array $a", "$a . 'x'"), "non-falsy-string");
    // An object operand either throws (no value, nothing to be wrong about) or
    // runs `__toString`, which PHP forces to return a string.
    assert_eq!(over("\\stdClass $o", "$o . ''"), "string");
    // And `mixed`, which admits every one of them.
    assert_eq!(over_doc(&["@param mixed $m"], "$m", "$m . ''"), "string");
}

// ---------------------------------------------------------------------------
// Compositionality and the four seams
// ---------------------------------------------------------------------------

#[test]
fn a_nested_concatenation_is_an_operand_that_answers() {
    // `a . b . c` lowers left-nested, so the outer `.`'s left operand IS a
    // `Concat`. Without the operand-reader arm it contributes nothing and the
    // casing conjunction cannot fire, which is exactly the difference asserted
    // here: `non-falsy-lowercase-string` needs the inner `$i . $j` to have
    // reported `LOWERCASE`.
    assert_eq!(over("int $i, int $j", "$i . $j . 'a'"), "non-falsy-lowercase-string");
    // And a cast over a concatenation, the other direction of the same
    // composition.
    // The composed answer is stronger than the floor: a non-falsy string is
    // truthy, so the bool cast DECIDES rather than flooring.
    assert_eq!(over("int $i", "(bool) ($i . '0')"), "true");
}

#[test]
fn the_assignment_and_the_dump_of_one_concatenation_agree() {
    // The assign seam: the binding a `$s = $a . $b;` leaves must be the fact the
    // dump of the same expression renders, or the two surfaces disagree about
    // one expression.
    let direct = over("int $i", "$i . '0'");
    let bound = one_dump(
        "<?php\nfunction f(int $i): void { $s = $i . '0'; \\PHPStan\\dumpType($s); }\n",
    );
    assert_eq!(bound, direct, "assignment binding vs dump of the same `.`");
    assert_eq!(bound, "non-falsy-numeric-uncased-string");
}

#[test]
fn a_projected_return_carries_the_concatenations_fact() {
    // The descent seam: an undeclared return whose operand is a concatenation
    // used to leave the call site factless.
    // A `float` operand is the sharp shape: the grid gives it a predicate set
    // and never a value, so the literal rung below cannot answer this exit and
    // only the concatenation arm can.
    assert_eq!(
        one_dump(
            "<?php\nfunction g(float $f) { return $f . 'X'; }\n\
             function f(): void { \\PHPStan\\dumpType(g(1.5)); }\n"
        ),
        "non-falsy-uppercase-string"
    );
}

#[test]
fn the_literal_lane_reads_the_fold_the_value_lane_would() {
    // The fourth seam, `Cx::resolve_literal_under`, observed through a reader
    // that only a *value* can satisfy: `strlen` folds only when its argument
    // arrived as an `ArgValue::Str`, so a `5` here proves the concatenation
    // resolved to the value `'a1234'` rather than to a predicate set.
    assert_eq!(dumped("strlen('a' . 1234)"), "5");
}

// ---------------------------------------------------------------------------
// Stratum
// ---------------------------------------------------------------------------

#[test]
fn the_floor_is_verified_and_a_derived_answer_keeps_the_operands_stratum() {
    // The #260 ruling as `eval_cast_fact` applies it: the bare floor is the
    // operator's own claim and enters Verified, so it renders without the
    // marker; a predicate set rests on the operands and keeps their `min`, so a
    // phpdoc-only operand renders `(asserted)` and can never premise a
    // proof-layer finding (ADR-0061 §3).
    assert_eq!(over("string $s", "$s . ''"), "string");
    assert_eq!(
        over_doc(&["@param non-empty-string $n"], "$n", "$n . 'x'"),
        "non-falsy-string (asserted)"
    );
}
