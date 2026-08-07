//! `type.invalid-operand` (ADR-0078, issue #191): an arithmetic, bitwise, shift
//! or unary operator applied to operands PHP's own table refuses with a
//! `TypeError`.
//!
//! Every fixture below is a `php -r`-witnessed row (PHP 8.5.9, runtime variables
//! so nothing constant-folds at compile time), and every *firing* row is paired
//! with the legal counterpart that makes it a real judgement rather than a
//! blanket refusal — `[] + 1` fires, `[] + []` is the array union; `'abc' + 1`
//! fires, `'5' + 5` is `10`; `'abc' & 1` fires, `'abc' & 'abc'` is the byte-wise
//! string operator.
//!
//! No sidecar and no version fork: both moving boundaries (non-numeric-string
//! arithmetic, array arithmetic) became `TypeError` in PHP **8.0**, and the
//! workspace floor is 8.1 (ADR-0011), so every row holds unchanged across
//! 8.1…8.5 and the sound-subset [`NoFold`] folder answers everything.

use steins_infer::{Diagnostic, INVALID_OPERAND_ID, NoFold, check_full};
use steins_syntax::SourceTree;

fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == INVALID_OPERAND_ID)
        .collect()
}

/// `<?php $x = <lhs>; $y = $x <op> <rhs>;` — the commonest shape: one operand
/// proven by a literal assignment, the other written in place.
fn bin(lhs: &str, op: &str, rhs: &str) -> Vec<Diagnostic> {
    diags(&format!("<?php\n$x = {lhs};\n$y = $x {op} {rhs};\n"))
}

/// `<?php $x = <operand>; $y = <op>$x;`
fn un(op: &str, operand: &str) -> Vec<Diagnostic> {
    diags(&format!("<?php\n$x = {operand};\n$y = {op}$x;\n"))
}

// ---------------------------------------------------------------------------
// Row: `+` with one array operand and one non-array — and its `[] + []` survivor.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_array_plus_int() {
    // php -r '$a = []; $b = 1; var_dump($a + $b);'
    //   → TypeError: Unsupported operand types: array + int
    let d = bin("[]", "+", "1");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 3, "the operator application's own line: {d:#?}");
    assert!(
        d[0].message.contains("Unsupported operand types: array + int"),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_int_plus_array_the_other_way_round() {
    // php -r '$a = 1; $b = []; var_dump($a + $b);'
    //   → TypeError: Unsupported operand types: int + array
    let d = bin("1", "+", "[]");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains("Unsupported operand types: int + array"),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_array_plus_null_bool_float_and_string() {
    // php -r: array + null / + true / + 1.5 / + 'abc' are all
    //   TypeError: Unsupported operand types: array + {null,bool,float,string}
    for rhs in ["null", "true", "false", "1.5", "'abc'", "'5'"] {
        assert_eq!(bin("[]", "+", rhs).len(), 1, "array + {rhs} is a TypeError");
    }
}

#[test]
fn silent_on_array_plus_array() {
    // php -r '$a = []; $b = []; var_dump($a + $b);' → array(0) {} — the UNION.
    assert!(bin("[]", "+", "[]").is_empty(), "`[] + []` is the array union, not arithmetic");
    // php -r '$a = ["a"]; $b = ["b"]; var_dump($a + $b);' → array(1) { [0]=> "a" }
    assert!(bin("['a']", "+", "['b']").is_empty(), "a non-empty union is legal too");
}

// ---------------------------------------------------------------------------
// Row: an array operand in `- * / % ** << >> & | ^` — where `array OP array`
// is fatal too.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_array_in_every_non_additive_operator() {
    // php -r, each with runtime variables: `[] - 1`, `[] * 1`, `[] / 1`,
    // `[] % 1`, `[] ** 1`, `[] & 1`, `[] | 1`, `[] ^ 1`, `[] << 1`, `[] >> 1`
    //   → TypeError: Unsupported operand types: array {op} int
    for op in ["-", "*", "/", "%", "**", "&", "|", "^", "<<", ">>"] {
        let d = bin("[]", op, "1");
        assert_eq!(d.len(), 1, "`[] {op} 1` is a TypeError: {d:#?}");
        assert!(
            d[0].message.contains(&format!("Unsupported operand types: array {op} int")),
            "{}",
            d[0].message
        );
    }
}

#[test]
fn fires_on_array_minus_array() {
    // php -r '$a = []; $b = []; var_dump($a - $b);'
    //   → TypeError: Unsupported operand types: array - array
    // The `+` survivor does NOT generalize: only `+` unions arrays.
    let d = bin("[]", "-", "[]");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains("Unsupported operand types: array - array"),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_int_minus_array_the_other_way_round() {
    // php -r '$a = 1; $b = []; var_dump($a - $b);'
    //   → TypeError: Unsupported operand types: int - array
    assert_eq!(bin("1", "-", "[]").len(), 1);
}

// ---------------------------------------------------------------------------
// Row: a string with no leading numeric prefix, in arithmetic and shifts.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_non_numeric_string_plus_int() {
    // php -r '$a = "abc"; $b = 1; var_dump($a + $b);'
    //   → TypeError: Unsupported operand types: string + int
    let d = bin("'abc'", "+", "1");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains("Unsupported operand types: string + int"),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_non_numeric_string_in_every_arithmetic_and_shift_operator() {
    // php -r, each: `'abc' {op} 1` → TypeError: Unsupported operand types: string {op} int
    for op in ["+", "-", "*", "/", "%", "**", "<<", ">>"] {
        assert_eq!(bin("'abc'", op, "1").len(), 1, "`'abc' {op} 1` is a TypeError");
    }
}

#[test]
fn fires_on_non_numeric_string_against_every_other_operand_kind() {
    // php -r: 'abc' + null / + true / + 1.5 / + '5' / + 'abc' / + '' are ALL
    //   TypeError — unlike the array row, no other-operand kind rescues it.
    for rhs in ["null", "true", "false", "1.5", "'5'", "'abc'", "''"] {
        assert_eq!(bin("'abc'", "+", rhs).len(), 1, "`'abc' + {rhs}` is a TypeError");
    }
}

#[test]
fn fires_on_empty_string_operand() {
    // php -r '$a = ""; $b = 1; var_dump($a + $b);'
    //   → TypeError: Unsupported operand types: string + int
    // The empty string has no numeric prefix, so it is the fatal band, not the
    // warning one.
    assert_eq!(bin("''", "+", "1").len(), 1);
    // php -r '$a = " "; …' → the same TypeError (whitespace alone is no prefix).
    assert_eq!(bin("' '", "+", "1").len(), 1);
}

#[test]
fn silent_on_numeric_string_operands() {
    // php -r '$a = "5"; $b = 5; var_dump($a + $b);' → int(10)
    // …and the whitespace/decimal/leading-zero forms PHP also calls numeric:
    //   ' 5' + 5 → 10, '5 ' + 5 → 10, '5.5' + 5 → 10.5, '017' + 5 → 22,
    //   '+5' + 5 → 10, '.5' + 5 → 5.5, '1e3' + 5 → 1005.0
    for lhs in ["'5'", "' 5'", "'5 '", "'5.5'", "'017'", "'+5'", "'.5'", "'1e3'", "'000123'"] {
        assert!(bin(lhs, "+", "5").is_empty(), "{lhs} is a numeric string: legal");
    }
}

#[test]
fn silent_on_leading_numeric_string_which_is_only_a_warning() {
    // php -r '$a = "5abc"; $b = 1; var_dump($a + $b);'
    //   → Warning: A non-numeric value encountered … int(6)
    // Warning-grade, and this id is fatal rows ONLY — no warning-handler gate,
    // no demotion, simply not this finding.
    for lhs in ["'5abc'", "'.5abc'", "'0x1A'", "'0b11'", "'1_000'", "'1e'"] {
        assert!(bin(lhs, "+", "1").is_empty(), "{lhs} merely warns: not this id");
    }
}

#[test]
fn silent_on_the_legal_scalar_operands() {
    // php -r: null + 1 → 1; true + 1 → 2; false + 1 → 1; 1.5 + 1 → 2.5
    for lhs in ["null", "true", "false", "1.5", "1"] {
        assert!(bin(lhs, "+", "1").is_empty(), "{lhs} + 1 is legal PHP");
    }
}

// ---------------------------------------------------------------------------
// Row: `& | ^` — two operators sharing a spelling.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_non_numeric_string_bitwise_against_a_non_string() {
    // php -r '$a = "abc"; $b = 1; var_dump($a & $b);'
    //   → TypeError: Unsupported operand types: string & int
    for op in ["&", "|", "^"] {
        let d = bin("'abc'", op, "1");
        assert_eq!(d.len(), 1, "`'abc' {op} 1` is a TypeError: {d:#?}");
    }
    // …and against null/bool/float, witnessed the same way.
    for rhs in ["null", "true", "1.5"] {
        assert_eq!(bin("'abc'", "&", rhs).len(), 1, "`'abc' & {rhs}` is a TypeError");
    }
}

#[test]
fn silent_on_string_bitwise_string() {
    // php -r '$a = "abc"; $b = "abc"; var_dump($a & $b);' → string(3) "abc"
    // php -r '$a = "abc"; $b = "5";   var_dump($a | $b);' → string(3) "ubc"
    // Both operands strings ⇒ the byte-wise operator, legal whatever the bytes.
    for op in ["&", "|", "^"] {
        assert!(bin("'abc'", op, "'abc'").is_empty(), "string {op} string is byte-wise");
        assert!(bin("'abc'", op, "'5'").is_empty(), "…including a numeric partner");
        assert!(bin("'abc'", op, "''").is_empty(), "…including the empty string");
    }
}

#[test]
fn fires_on_non_numeric_string_shift_even_against_a_string() {
    // The shifts do NOT have the byte-wise twin:
    // php -r '$a = "abc"; $b = "5"; var_dump($a << $b);'
    //   → TypeError: Unsupported operand types: string << string
    for op in ["<<", ">>"] {
        assert_eq!(bin("'abc'", op, "'5'").len(), 1, "`'abc' {op} '5'` is a TypeError");
    }
    // …while a numeric pair shifts fine: '5' << '1' → int(10).
    for op in ["<<", ">>"] {
        assert!(bin("'5'", op, "'1'").is_empty(), "numeric strings shift legally");
    }
}

// ---------------------------------------------------------------------------
// Rows: the unary operators.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_unary_minus_on_array() {
    // php -r '$a = []; var_dump(-$a);'
    //   → TypeError: Unsupported operand types: array * int
    // (the engine compiles unary minus as `* -1`, hence the `* int` sentence)
    let d = un("-", "[]");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains("Unsupported operand types: array * int"),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_unary_plus_and_minus_on_a_non_numeric_string() {
    // php -r '$a = "abc"; var_dump(-$a);' / '+$a'
    //   → TypeError: Unsupported operand types: string * int
    assert_eq!(un("-", "'abc'").len(), 1);
    assert_eq!(un("+", "'abc'").len(), 1);
    assert_eq!(un("+", "[]").len(), 1);
}

#[test]
fn silent_on_unary_minus_on_the_legal_operands() {
    // php -r: -5 → -5; -1.5 → -1.5; -true → -1; -null → 0; -'5' → -5;
    //         -'5.5' → -5.5; -'5abc' → warning only
    for operand in ["5", "1.5", "true", "false", "null", "'5'", "'5.5'", "'5abc'"] {
        assert!(un("-", operand).is_empty(), "-{operand} is legal (or merely warns)");
    }
}

#[test]
fn fires_on_bitwise_not_on_array_bool_and_null() {
    // php -r '$a = []; var_dump(~$a);'    → TypeError: Cannot perform bitwise not on array
    // php -r '$a = true; var_dump(~$a);'  → TypeError: Cannot perform bitwise not on true
    // php -r '$a = false; var_dump(~$a);' → … on false
    // php -r '$a = null; var_dump(~$a);'  → … on null
    for (operand, word) in [("[]", "array"), ("true", "true"), ("false", "false"), ("null", "null")]
    {
        let d = un("~", operand);
        assert_eq!(d.len(), 1, "~{operand} is a TypeError: {d:#?}");
        assert!(
            d[0].message.contains(&format!("Cannot perform bitwise not on {word}")),
            "{}",
            d[0].message
        );
    }
}

#[test]
fn silent_on_bitwise_not_on_int_float_and_string() {
    // php -r '$a = 1; var_dump(~$a);'      → int(-2)
    // php -r '$a = "abc"; var_dump(~$a);'  → the byte-wise complement, a string
    // php -r '$a = 1.5; var_dump(~$a);'    → deprecation + int(-2), not fatal
    // Note the asymmetry with `-`: `~` accepts every string and refuses
    // bool/null, exactly the other way round.
    for operand in ["1", "1.5", "'abc'", "'5'", "''"] {
        assert!(un("~", operand).is_empty(), "~{operand} is not fatal");
    }
}

#[test]
fn silent_on_logical_not_which_is_total() {
    // php -r '$a = []; var_dump(!$a);' → bool(true). `!` never fatals, on any
    // operand kind — it is not collected as an operand site at all.
    assert!(diags("<?php\n$x = [];\n$y = !$x;\n").is_empty());
}

// ---------------------------------------------------------------------------
// The operand lane: what counts as proof.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_two_proven_variables() {
    // Both operands via the env, not the source text.
    let d = diags("<?php\n$a = [];\n$b = 1;\n$c = $a * $b;\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("$a * $b"), "the message quotes the operands: {}", d[0].message);
}

#[test]
fn silent_on_a_native_string_seed_whose_content_is_unknown() {
    // A native `string` parameter proves the BASE but not the content, and only
    // the content decides between `'5' + 1` (legal) and `'abc' + 1` (fatal) —
    // so the abstract layer is silence. This is the deliberate conservative
    // direction, and it is why the string rows need a literal.
    let src = "<?php\nfunction f(string $s): void { $x = $s + 1; }\n";
    assert!(diags(src).is_empty(), "an abstract string base proves nothing about the content");
    // A native `array` parameter carries no value-domain fact at all (the seed
    // lane is scalars-only), so it is silent for a different reason again.
    let arr = "<?php\nfunction f(array $a): void { $x = $a + 1; }\n";
    assert!(diags(arr).is_empty(), "a native array hint seeds no fact — out of reach, not wrong");
}

#[test]
fn fires_on_a_native_int_seed() {
    // The abstract layer DOES prove the int/float/bool rows, because the base is
    // the whole question there: `int + array` is fatal whatever the int is.
    let src = "<?php\nfunction f(int $n): void { $a = []; $x = $n + $a; }\n";
    assert_eq!(diags(src).len(), 1, "a native scalar seed is a Verified fact: {:#?}", diags(src));
}

#[test]
fn silent_on_a_maybe_union_operand() {
    // One branch an array, the other an int: the merged fact is a heterogeneous
    // `OneOf`, which proves no single operand kind — silence, not a false proof.
    let src = "<?php\nif (rand()) {\n    $a = [];\n} else {\n    $a = 1;\n}\n$b = $a - 1;\n";
    assert!(diags(src).is_empty(), "a Maybe operand is silence: {:#?}", diags(src));
}

#[test]
fn silent_on_a_nullable_operand() {
    // `?array` denotes array-or-null, and those are opposite verdicts under
    // `-` (fatal / legal `0 - 1`), so the layer proves nothing here.
    let src = "<?php\nfunction f(?array $a): void { $x = $a - 1; }\n";
    assert!(diags(src).is_empty(), "a nullable operand is silence");
}

#[test]
fn fires_on_a_homogeneous_union_of_fatal_strings() {
    // The other side of the `OneOf` rule: every member the SAME kind still
    // proves it — `'abc'|'def'` is a proven prefix-less string.
    let src = "<?php\nif (rand()) {\n    $a = 'abc';\n} else {\n    $a = 'def';\n}\n$b = $a - 1;\n";
    assert_eq!(diags(src).len(), 1, "a homogeneous union still proves the kind");
}

#[test]
fn silent_on_an_asserted_premise() {
    // ADR-0052 §5: an `Asserted` fact (a `@phpstan-assert` claim, not a walked
    // value) cannot premise a proof-layer fatal.
    let src = "<?php
/** @phpstan-assert array $x */
function claimArray($x): void {}
function f(mixed $x): void { claimArray($x); $y = $x + 1; }
";
    assert!(diags(src).is_empty(), "an Asserted operand must not premise the proof");
}

#[test]
fn silent_on_an_unproven_operand() {
    // No literal, no seed, no guard: nothing is proven, so nothing is claimed.
    assert!(diags("<?php\nfunction f($x) { $y = $x + 1; }\n").is_empty());
    // A call result is likewise not an operand proof here.
    assert!(diags("<?php\nfunction g() { return []; }\n$y = g() + 1;\n").is_empty());
}

// ---------------------------------------------------------------------------
// The object posture — the GMP-shaped silence.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_an_object_operand_of_an_unknown_class() {
    // PHP has no userland operator overloading, so a *plain* object in `+` is a
    // TypeError (`new stdClass() + 1` → Unsupported operand types: stdClass +
    // int) — but internal classes DO overload, and `GMP` arithmetic is the
    // standard counterexample. The value domain has no object denotation at all,
    // so every object operand is silent by construction rather than by a
    // class-by-class allowlist.
    let unknown = "<?php\nfunction f(SomeExternalClass $o): void { $x = $o + 1; }\n";
    assert!(diags(unknown).is_empty(), "an object of an unknown class stays silent");
    let gmp = "<?php\n$a = gmp_init(1);\n$b = gmp_init(2);\n$c = $a + $b;\n";
    assert!(diags(gmp).is_empty(), "GMP overloads `+`: silence is the correct posture");
    let plain = "<?php\nclass C {}\n$o = new C();\n$x = $o + 1;\n";
    assert!(diags(plain).is_empty(), "even a proven userland object is out of v1's reach");
}

// ---------------------------------------------------------------------------
// Excluded operator families, pinned.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_concat_which_is_issue_193s_territory() {
    // php -r '$a = []; $b = "x"; var_dump($a . $b);'
    //   → Warning: Array to string conversion … string(6) "Arrayx"
    // Warning-grade AND another id's family (`string.array-conversion` /
    // `string.non-stringable`, issue #193), so `.` is not collected at all.
    assert!(diags("<?php\n$x = [];\n$y = $x . 'a';\n").is_empty(), "array-in-concat is #193's");
    assert!(diags("<?php\n$x = 'abc';\n$y = $x . 1;\n").is_empty(), "and plain concat is legal");
    // php -r '$o = new stdClass(); echo $o . "";' → Error: Object of class
    // stdClass could not be converted to string — fatal, but still #193's id.
    let obj = "<?php\nclass C {}\n$o = new C();\n$y = $o . 'a';\n";
    assert!(diags(obj).is_empty(), "object-in-concat is #193's id, not this one");
}

#[test]
fn silent_on_every_comparison_operator() {
    // php -r, all legal at 8.5.9 — no comparison of any operand pair fatals:
    //   [] < 1 → false; 1 < [] → true; [] <=> 1 → 1; [] == 1 → false;
    //   [] === 1 → false; 'abc' < 1 → false; [] > 1.5 → true
    // So `InvalidComparisonOperationRule` folds into this id with ZERO rows.
    for op in ["<", ">", "<=", ">=", "<=>", "==", "!=", "===", "!==", "<>"] {
        assert!(bin("[]", op, "1").is_empty(), "`[] {op} 1` is legal PHP");
        assert!(bin("'abc'", op, "1").is_empty(), "`'abc' {op} 1` is legal PHP");
    }
}

#[test]
fn silent_on_increment_and_decrement() {
    // php -r '$a = []; $a++;' → TypeError: Cannot increment array — genuinely
    // fatal, but a mutation statement rather than an operand expression, so it
    // is out of v1's reach and deliberately not collected.
    for src in ["<?php\n$x = [];\n$x++;\n", "<?php\n$x = [];\n++$x;\n", "<?php\n$x = [];\n$x--;\n"]
    {
        assert!(diags(src).is_empty(), "++/-- is not this id in v1");
    }
}

#[test]
fn silent_on_division_by_zero() {
    // php -r '$z = 0; var_dump(1 / $z);' → DivisionByZeroError — a *value*
    // question, not an operand-TYPE one; no row of this table covers it.
    assert!(diags("<?php\n$z = 0;\n$y = 1 / $z;\n").is_empty());
    assert!(diags("<?php\n$z = 0;\n$y = 1 % $z;\n").is_empty());
}

// ---------------------------------------------------------------------------
// Reach limits — the silences the env-correctness rules buy.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_a_closure_body_reading_its_own_binding() {
    // The site `$s + 1` lies inside the creating statement's span, but `$s` there
    // is the closure's parameter, NOT the outer `'abc'`. The site's
    // `enclosing_body` is what keeps the outer env from being asked.
    let src = "<?php\n$s = 'abc';\n$f = function (int $s) { return $s + 1; };\n";
    assert!(diags(src).is_empty(), "a closure's operand is judged in the closure's scope");
    let arrow = "<?php\n$s = 'abc';\n$f = fn (int $s) => $s + 1;\n";
    assert!(diags(arrow).is_empty(), "…and the same for an arrow function");
}

#[test]
fn fires_inside_a_closure_body_on_its_own_proof() {
    // The mirror image: the closure's own scope walk judges its own sites.
    let src = "<?php\n$f = function () { $a = []; return $a + 1; };\n";
    assert_eq!(diags(src).len(), 1, "a closure's own proof still fires");
}

#[test]
fn silent_inside_an_unmodelled_loop_body() {
    // A `while` body is an ADR-0027 `Opaque` construct: the entry env is not the
    // env its statements run under (`$x` is reassigned inside), so no site inside
    // it is judged from here. Out of reach, never a wrong claim.
    let src = "<?php\n$x = 1;\nwhile (rand()) {\n    $x = [];\n    $y = $x + 1;\n}\n";
    assert!(diags(src).is_empty(), "a loop body is out of reach: {:#?}", diags(src));
}

#[test]
fn fires_in_an_if_branch_exactly_once() {
    // A structured `if` IS modelled (ADR-0031), so its branch statements are
    // walked with the branch env — and the containing `if` statement must not
    // report the same site a second time.
    let src = "<?php\nfunction f(): void {\n    if (rand()) {\n        $a = [];\n        $b = $a + 1;\n    }\n}\n";
    let d = diags(src);
    assert_eq!(d.len(), 1, "exactly one report, from the branch's own statement: {d:#?}");
    assert_eq!(d[0].line, 5, "{d:#?}");
}

#[test]
fn fires_once_per_site_in_a_nested_expression() {
    // Nested applications are separate sites; only the one whose BOTH operands
    // are proven fires. `(1 + 2) + []` → the inner is legal, the outer has an
    // unproven left operand (a nested application lowers to `Other`), so the
    // whole expression is silent — while `$a + 1` alone fires.
    assert!(diags("<?php\n$a = [];\n$b = (1 + 2) + $a;\n").is_empty());
    assert_eq!(diags("<?php\n$a = [];\n$b = $a + 1;\n").len(), 1);
}

#[test]
fn silent_at_the_documented_reach_limits() {
    // Not false negatives to be surprised by later — recorded silences, each a
    // position the entry-env rule cannot reach in v1:
    //   * an `if`/`while` CONDITION (the `if` statement is not a leaf, and its
    //     branches are where the walk descends);
    let cond = "<?php\n$a = [];\nif ($a + 1) {\n    echo 'x';\n}\n";
    assert!(diags(cond).is_empty(), "a condition operand is out of reach: {:#?}", diags(cond));
    //   * an arrow function's by-value capture of an enclosing binding (a real
    //     fatal at run time, but the closure's scope is walked with its own env);
    let capture = "<?php\n$s = 'abc';\n$f = fn () => $s + 1;\n";
    assert!(diags(capture).is_empty(), "a captured operand is out of reach");
    //   * a class-constant or property-default expression, which is no
    //     statement in any scope trace.
    let decl = "<?php\nclass C {\n    const X = 'abc';\n    public array $p = [];\n}\n";
    assert!(diags(decl).is_empty(), "declaration-position expressions are out of reach");
}

#[test]
fn fires_in_a_return_and_an_echo_and_a_call_argument() {
    // The whitelisted leaf statements, each reading the same entry env.
    assert_eq!(diags("<?php\nfunction f() { $a = []; return $a + 1; }\n").len(), 1);
    assert_eq!(diags("<?php\n$a = [];\necho $a + 1;\n").len(), 1);
    assert_eq!(diags("<?php\n$a = [];\nvar_dump($a + 1);\n").len(), 1);
}

// ---------------------------------------------------------------------------
// The `warning-handler` posture does NOT apply: fatal rows only.
// ---------------------------------------------------------------------------

#[test]
fn the_warning_handler_posture_does_not_demote_this_id() {
    // Unlike `foreach.non-iterable` / `offset.missing`, every row here is a
    // fatal `TypeError`, so a declared `warning-handler = "null"` posture
    // changes nothing (ADR-0049 §7 applies to warning-grade ids only).
    let tree = SourceTree::parse("<?php\n$a = [];\n$b = $a + 1;\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut NoFold, false)
        .into_iter()
        .filter(|d| d.id == INVALID_OPERAND_ID)
        .collect();
    assert_eq!(d.len(), 1, "a fatal row never demotes: {d:#?}");
}
