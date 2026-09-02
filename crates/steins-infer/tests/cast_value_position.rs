//! Issue #626 — a cast in VALUE position answers the cast grid instead of
//! `unknown`.
//!
//! The grid itself ([`php_cast_fact`], issue #595) is unit-tested in `coerce.rs`
//! and not re-listed here. What this file pins is the property that makes the
//! reuse real rather than nominal:
//!
//! * **One grid, two syntaxes.** `settype($v, 'int')` and `(int) $v` answer
//!   identically from the same input — asserted side by side, so a change to
//!   either spelling that does not move the other fails here.
//! * **Every cast is total.** `(int)`, `(float)`, `(bool)`, `(array)` and
//!   `(string)` over an operand with no fact answer the target's base, never
//!   `unknown`. `(object)` is not a cast this vocabulary carries and answers
//!   `unknown` rather than a wrong base.
//! * **The two lanes agree.** The literal seam folds `(int) 5.25` to the value
//!   `5` and the fact seam renders `5` — the same property issues #260 and #579
//!   hold for their own operators.
//!
//! # Behavioral witnesses at `PINNED_PHP` (8.5.9, `php -r`)
//!
//! The `(string)` floor is the one ruling this slice makes rather than inherits,
//! and it rests on these:
//!
//! * `var_dump((string)[1, 2]);` — `Warning: Array to string conversion`, then
//!   `string(5) "Array"`. A warning, not an error: the value IS a string.
//! * `$f = fopen("php://memory", "r"); var_dump((string)$f);` — `string(14)
//!   "Resource id #5"`.
//! * `var_dump((string)new stdClass);` — `Fatal error: Uncaught Error: Object of
//!   class stdClass could not be converted to string`. A throw produces no value,
//!   so there is nothing for a floor to be wrong about.
//! * `var_dump((int)new ArrayObject([1]), (bool)new stdClass);` — `int(1)` and
//!   `bool(true)`, with a warning at most. The other bases never throw at all.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A mock PHP answering exactly the reflected declaration the `settype` row is
/// pinned against (PHP 8.5.9): `settype(mixed &$var, string $type): bool`, two
/// parameters, both required. The cast expression needs nothing from it; the
/// paired assertions below need `settype` to work at all.
#[derive(Default)]
struct Mock;

impl Folder for Mock {
    /// `strlen` over a proven string, which is how the literal lane gets
    /// OBSERVED: the fold resolves each argument through `resolve_literal`
    /// first, so a cast that folds arrives here as an `ArgValue::Str` and one
    /// that does not never reaches this function at all.
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
            "settype" => Some("bool".to_owned()),
            "strlen" => Some("int".to_owned()),
            _ => None,
        }
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        // `(total, required)` — the order that is a silent decline when reversed.
        match name.to_ascii_lowercase().as_str() {
            "settype" => Some((2, 2)),
            "strlen" => Some((1, 1)),
            _ => None,
        }
    }
}

/// Every finding a source produces, `untyped.*` dropped (ADR-0078, #200 — it
/// flags the fixtures' own deliberately untyped signatures, not the behavior
/// under test).
fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// The single dump a one-dump source produces, asserting no other finding came
/// with it — a cast's fact must never premise one.
fn one_dump(src: &str) -> String {
    let ds = findings(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a cast emitted a finding: {other:?}");
    only_dump(&ds)
}

/// The single dump, without the no-finding assertion — for the one fixture that
/// is *supposed* to be reported.
fn only_dump(ds: &[Diagnostic]) -> String {
    let d: Vec<String> = ds
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect();
    assert_eq!(d.len(), 1, "expected exactly one dump, got {d:?}");
    d[0].clone()
}

/// `function f(<param>): void { dumpType(<cast> $v); }` — the declared-lane
/// fixture, where `$v` reaches the cast with only its declaration.
fn declared(param: &str, cast: &str) -> String {
    one_dump(&format!(
        "<?php\nfunction f({param}): void {{ \\PHPStan\\dumpType({cast} $v); }}\n"
    ))
}

/// `dumpType(<e>);` — a whole expression, no environment at all.
fn dumped(e: &str) -> String {
    one_dump(&format!("<?php\nfunction f(): void {{ \\PHPStan\\dumpType({e}); }}\n"))
}

/// `dumpType(<cast> <value>);` — the value-lane fixture.
fn expr(cast: &str, value: &str) -> String {
    dumped(&format!("{cast} {value}"))
}

/// `function f(<param>): void { dumpType(<e>); }` — a whole expression over a
/// declared `$v`.
fn dumped_over(param: &str, e: &str) -> String {
    one_dump(&format!(
        "<?php\n/** @param {param} $v */\nfunction f(int $v): void {{ \\PHPStan\\dumpType({e}); }}\n"
    ))
}

/// The same input through the `settype` spelling: `settype($v, <type>)` then a
/// dump of `$v`.
fn via_settype(param: &str, ty: &str) -> String {
    one_dump(&format!(
        "<?php\nfunction f({param}): void {{ settype($v, {ty}); \\PHPStan\\dumpType($v); }}\n"
    ))
}

// The witness

#[test]
fn the_issue_witness_folds_the_literal() {
    // The headline: `(int) 5.25` dumps `5` where it dumped `unknown`, with no
    // environment at all to read it from.
    assert_eq!(expr("(int)", "5.25"), "5");
}

// One grid, two syntaxes

#[test]
fn a_cast_and_a_settype_answer_the_same_grid() {
    // The acceptance property, run side by side rather than asserted twice: the
    // cast expression and the `settype` call are two readers over ONE table, so
    // any input either can spell must answer identically.
    for (param, cast, ty) in [
        ("string $v", "(int)", "'int'"),
        ("string $v", "(float)", "'float'"),
        ("int $v", "(string)", "'string'"),
        ("string $v", "(bool)", "'bool'"),
        ("string $v", "(array)", "'array'"),
        ("bool $v", "(int)", "'int'"),
        ("bool $v", "(float)", "'float'"),
        ("bool $v", "(string)", "'string'"),
        ("float $v", "(string)", "'string'"),
        ("float $v", "(int)", "'int'"),
    ] {
        let by_cast = declared(param, cast);
        let by_settype = via_settype(param, ty);
        assert_eq!(by_cast, by_settype, "`{cast} $v` vs `settype($v, {ty})` on `{param}`");
    }
    // And the cell the settype slice already pinned reaches the other syntax:
    // `(string)` of an int is the `numeric-uncased-string` row.
    assert_eq!(declared("int $v", "(string)"), "numeric-uncased-string");
}

// Totality

#[test]
fn every_cast_answers_a_base_for_an_operand_with_no_fact() {
    // The defect this slice closes: `$v` is untyped, so nothing is known about
    // it, and the cast still answers — the base is the OPERATOR's guarantee.
    assert_eq!(declared("$v", "(int)"), "int");
    assert_eq!(declared("$v", "(float)"), "float");
    assert_eq!(declared("$v", "(bool)"), "bool");
    assert_eq!(declared("$v", "(string)"), "string");
    assert_eq!(declared("$v", "(array)"), "array");
}

#[test]
fn an_operand_the_vocabulary_cannot_spell_still_answers() {
    // Totality has to survive an operand that is `ArgValue::Other`, which is the
    // whole reason the lowering keeps the cast node instead of widening.
    assert_eq!(
        one_dump("<?php\nfunction f($v): void { \\PHPStan\\dumpType((int) $v->a->b); }\n"),
        "int"
    );
    assert_eq!(
        one_dump("<?php\nfunction f($v): void { \\PHPStan\\dumpType((string) $v->a->b); }\n"),
        "string"
    );
}

#[test]
fn the_object_cast_answers_unknown_rather_than_a_wrong_base() {
    // `(object)1` is `object(stdClass)#1 { ["scalar"]=> int(1) }` at
    // `PINNED_PHP` — a value the four-layer domain has no member for. Naming any
    // base here would be wrong, so the cast is not carried at all.
    assert_eq!(declared("$v", "(object)"), "unknown");
    assert_eq!(expr("(object)", "1"), "unknown");
}

#[test]
fn a_binary_cast_is_a_string_cast() {
    // `(binary)"a"` is `"a"` at `PINNED_PHP`, deprecated but converting — where
    // `settype($v, 'binary')` is a `ValueError`. The two readers share the enum,
    // not the spelling set.
    assert_eq!(declared("int $v", "(binary)"), declared("int $v", "(string)"));
    assert_eq!(declared("$v", "(binary)"), "string");
}

// The `(string)` floor ruling

#[test]
fn a_string_cast_floors_like_the_others_but_never_states_array() {
    // The ruling: `(string)` is total for the same reason the other four are — a
    // cast that produces a value produces one of its target's base, and the only
    // alternative is a throw, which produces no value. See the module witnesses.
    assert_eq!(declared("$v", "(string)"), "string");
    // And the grid's array cell is NOT overturned: it still refuses to state the
    // value `'Array'`, so a PROVEN array casts to the floor and no further —
    // while the program that wrote it keeps being REPORTED, by the rule that has
    // owned this site since issue #193. Stating the base blesses nothing.
    let ds = findings(
        "<?php\nfunction f(): void { \\PHPStan\\dumpType((string) [1, 2, 3]); }\n",
    );
    assert_eq!(only_dump(&ds), "string");
    assert!(
        ds.iter().any(|d| d.id == "string.array-conversion"),
        "the array-in-string-context finding is still raised: {ds:?}"
    );
    // The proof that the refusal is the grid's and not this seam's: `settype`
    // reads the same declining cell and keeps its by-ref invalidation, which is
    // `unknown` — the two lanes differ here exactly because a floor is the
    // operator's claim and `settype` has no operator to make one.
    assert_eq!(via_settype("array $v", "'string'"), "unknown");
}

// The grid over a proven operand

#[test]
fn a_proven_operand_casts_value_precisely() {
    // The dump surface's own answer for each literal row of the grid. This
    // exercises the FACT seam only — `best_dump_type`'s `Cast` rung sits above
    // the literal fold, so nothing here says anything about the literal lane;
    // that is `the_literal_lane_folds_a_cast_into_what_reads_it`'s job.
    for (cast, value, want) in [
        ("(int)", "true", "1"),
        ("(int)", "false", "0"),
        ("(int)", "5.25", "5"),
        ("(int)", "'5'", "5"),
        ("(int)", "null", "0"),
        ("(float)", "5", "5.0"),
        ("(float)", "'5'", "5.0"),
        ("(float)", "null", "0.0"),
        ("(string)", "true", "'1'"),
        ("(string)", "false", "''"),
        ("(string)", "5", "'5'"),
        ("(string)", "-5", "'-5'"),
        ("(bool)", "'some-string'", "true"),
        ("(bool)", "'0'", "false"),
        ("(array)", "null", "array{}"),
    ] {
        assert_eq!(expr(cast, value), want, "`{cast} {value}`");
    }
    // The assignment seam binds the same value the dump renders.
    assert_eq!(
        one_dump("<?php\nfunction f(): void { $x = (int) 5.25; \\PHPStan\\dumpType($x); }\n"),
        "5"
    );
}

#[test]
fn the_literal_lane_folds_a_cast_into_what_reads_it() {
    // What `Cx::resolve_literal_under`'s cast arm buys, stated as the difference
    // it makes rather than as an agreement it shares.
    //
    // A dump of a cast proves NOTHING about this lane: `best_dump_type` matches
    // `ArgValue::Cast` before it reaches any literal rung, so `dumpType((int)
    // 5.25)` still answers `5` with the literal arm deleted. The lane is
    // observable only where the folded VALUE is handed to something else — and
    // there its absence is the difference between a value and a base type.
    //
    // Asserted as one table so a deletion reports every reader it breaks, not
    // just the first:
    //
    // * a **call argument** — the fold resolves each argument through
    //   `resolve_literal` first, so `(string) 12345` reaches the folder as
    //   `'12345'` and `strlen` folds; without the arm the argument is unproven
    //   and the call answers its declared `int`;
    // * a **comparison operand** — `cmp_candidates_under` sends a non-variable
    //   through this lane and nothing else, so a decided verdict is proof the
    //   cast folded; without it the comparison is undecided and floors to `bool`;
    // * a **ternary arm** — the chosen arm resolves through the same lane.
    let cases = [
        ("strlen((string) 12345)", "5"),
        ("(int) '5' === 5", "true"),
        ("(float) '5' === 5.0", "true"),
        ("true ? (int) '5' : 0", "5"),
    ];
    let got: Vec<(&str, String)> = cases.iter().map(|(e, _)| (*e, dumped(e))).collect();
    let want: Vec<(&str, String)> =
        cases.iter().map(|(e, w)| (*e, (*w).to_owned())).collect();
    assert_eq!(got, want);
}

#[test]
fn a_non_numeric_string_casts_to_the_zero_it_actually_is() {
    // Measured at `PINNED_PHP`: `(int)'blabla'` is `0` and `(float)'blabla'` is
    // `0.0`, because the numeric PREFIX is empty. A string that could have a
    // prefix is still declined to the base — `(int)'12abc'` is `12`, a rule this
    // grid does not author.
    assert_eq!(expr("(int)", "'blabla'"), "0");
    assert_eq!(expr("(float)", "'blabla'"), "0.0");
    assert_eq!(expr("(int)", "''"), "0");
    assert_eq!(expr("(int)", "'  blabla'"), "0");
    assert_eq!(expr("(int)", "'12abc'"), "int");
    // A leading `.` cannot be claimed: `(float)'.5abc'` is `0.5`, not `0.0`.
    assert_eq!(expr("(float)", "'.5abc'"), "float");
}

#[test]
fn a_narrow_int_range_casts_to_the_literals_it_spells() {
    // `(string)5` … `(string)10` are `'5'` … `'10'` at `PINNED_PHP`, so an
    // interval that narrow IS a finite set of strings — and both syntaxes say so,
    // being one grid.
    let want = "'10'|'5'|'6'|'7'|'8'|'9'";
    assert_eq!(
        one_dump(
            "<?php\n/** @param int<5, 10> $v */\nfunction f(int $v): void { \\PHPStan\\dumpType((string) $v); }\n"
        ),
        format!("{want} (asserted)")
    );
    assert_eq!(
        one_dump(
            "<?php\n/** @param int<5, 10> $v */\nfunction f(int $v): void { settype($v, 'string'); \\PHPStan\\dumpType($v); }\n"
        ),
        format!("{want} (asserted)")
    );
    // Above the cap it declines to the predicate rather than truncating.
    assert_eq!(
        one_dump(
            "<?php\n/** @param int<1, 9> $v */\nfunction f(int $v): void { \\PHPStan\\dumpType((string) $v); }\n"
        ),
        "numeric-uncased-string (asserted)"
    );
}

// Composition and stratum

#[test]
fn a_cast_composes_with_the_operator_family() {
    // The cast reads its operand through the same reader the logical family
    // does, so an operator node inside one folds rather than bottoming out.
    assert_eq!(expr("(int)", "(1 === 1)"), "1");
    assert_eq!(expr("(string)", "(1 === 2)"), "''");
    assert_eq!(expr("(int)", "(int) 5.25"), "5");
    // And a cast is itself readable as a truthiness operand.
    assert_eq!(dumped("!(bool) 0"), "true");
}

#[test]
fn a_cast_is_readable_as_another_expressions_operand() {
    // What `value_operand_fact`'s own `Cast` arm buys — the composition the
    // three lines above cannot see, because every operand there is a LITERAL
    // and the literal lane answers those whichever road is taken.
    //
    // Over a subject that carries a FACT and no value, only this arm can read
    // an inner cast: without it the inner cast is not a `Var`, not an array and
    // not a literal, so `transfer_arg_known` declines and the outer expression
    // falls to its own floor.
    //
    // Asserted as one table, for the reason above:
    //
    // * a **truthiness** operand — `int<5, 10>` excludes `0`, so the `(bool)` is
    //   decided and the negation decides with it; without the arm the negation
    //   sees no fact and renders the `bool` floor;
    // * another **cast's** operand, both ways round — without the arm each
    //   answers the outer operator's own base instead;
    // * a **connective's** operand, for the same reason.
    let cases = [
        ("!(bool) $v", "false (asserted)"),
        ("(int) (string) $v", "5|6|7|8|9|10 (asserted)"),
        ("(string) (int) $v", "'10'|'5'|'6'|'7'|'8'|'9' (asserted)"),
        ("(bool) $v && true", "true (asserted)"),
    ];
    let got: Vec<(&str, String)> =
        cases.iter().map(|(e, _)| (*e, dumped_over("int<5, 10>", e))).collect();
    let want: Vec<(&str, String)> =
        cases.iter().map(|(e, w)| (*e, (*w).to_owned())).collect();
    assert_eq!(got, want);
}

#[test]
fn the_stratum_is_the_operands_when_the_grid_answered_and_the_operators_when_it_floored() {
    // `(int)` of an int is the identity, interval included — and the interval
    // came from a docblock, so the fact stays `Asserted` (the dump says so) and
    // can never premise a proof-layer finding, per the #260 ruling.
    assert_eq!(
        one_dump(
            "<?php\n/** @param int<5, 10> $v */\nfunction f(int $v): void { \\PHPStan\\dumpType((int) $v); }\n"
        ),
        "int<5, 10> (asserted)"
    );
    // The floor over the SAME asserted operand is a claim about the operator,
    // owed to no docblock, so it enters `Verified` — no `(asserted)` marker.
    assert_eq!(
        one_dump(
            "<?php\n/** @param int<5, 10> $v */\nfunction f(int $v): void { \\PHPStan\\dumpType((float) $v); }\n"
        ),
        "float"
    );
}
