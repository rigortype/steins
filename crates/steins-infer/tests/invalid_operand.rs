//! `type.invalid-operand` (ADR-0078, issue #191): an arithmetic, bitwise, shift
//! or unary operator applied to operands PHP's own table refuses with a `TypeError`.
//!
//! Every fixture is a `php -r`-witnessed row (PHP 8.5.9, runtime variables so
//! nothing constant-folds), paired with a legal counterpart that makes it a real
//! judgement, not a blanket refusal — `[] + 1` fires, `[] + []` unions; `'abc' + 1`
//! fires, `'5' + 5` is `10`; `'abc' & 1` fires, `'abc' & 'abc'` is byte-wise.
//!
//! Both moving boundaries (non-numeric-string and array arithmetic) became
//! `TypeError` in PHP **8.0**; the workspace floor is 8.1 (ADR-0011), so every row
//! holds unchanged 8.1…8.5 and the sound-subset [`NoFold`] folder answers everything.

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

// Row: `+` with one array operand and one non-array — and its `[] + []` survivor.

#[test]
fn fires_on_array_plus_int() {
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
    // array + {null,bool,float,string} are all TypeError: Unsupported operand types.
    for rhs in ["null", "true", "false", "1.5", "'abc'", "'5'"] {
        assert_eq!(bin("[]", "+", rhs).len(), 1, "array + {rhs} is a TypeError");
    }
}

#[test]
fn silent_on_array_plus_array() {
    // → array(0) {} — the UNION, not arithmetic.
    assert!(bin("[]", "+", "[]").is_empty(), "`[] + []` is the array union, not arithmetic");
    // → array(1) { [0]=> "a" } — a non-empty union is legal too.
    assert!(bin("['a']", "+", "['b']").is_empty(), "a non-empty union is legal too");
}

// Row: an array operand in `- * / % ** << >> & | ^` — where `array OP array`
// is fatal too.

#[test]
fn fires_on_array_in_every_non_additive_operator() {
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
    // → TypeError: Unsupported operand types: int - array.
    assert_eq!(bin("1", "-", "[]").len(), 1);
}

// Row: a string with no leading numeric prefix, in arithmetic and shifts.

#[test]
fn fires_on_non_numeric_string_plus_int() {
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
    // Each `'abc' {op} 1` → TypeError: Unsupported operand types: string {op} int.
    for op in ["+", "-", "*", "/", "%", "**", "<<", ">>"] {
        assert_eq!(bin("'abc'", op, "1").len(), 1, "`'abc' {op} 1` is a TypeError");
    }
}

#[test]
fn fires_on_non_numeric_string_against_every_other_operand_kind() {
    // Every kind is fatal here — unlike the array row, nothing rescues a
    // non-numeric string.
    for rhs in ["null", "true", "false", "1.5", "'5'", "'abc'", "''"] {
        assert_eq!(bin("'abc'", "+", rhs).len(), 1, "`'abc' + {rhs}` is a TypeError");
    }
}

#[test]
fn fires_on_empty_string_operand() {
    // Empty string has no numeric prefix: the fatal band, not the warning one.
    assert_eq!(bin("''", "+", "1").len(), 1);
    // Same for whitespace-only " " (no prefix either).
    assert_eq!(bin("' '", "+", "1").len(), 1);
}

#[test]
fn silent_on_numeric_string_operands() {
    // Witnessed (all legal, + 5): "5"→10, " 5"/"5 "→10, "5.5"→10.5, "017"→22,
    // "+5"→10, ".5"→5.5, "1e3"→1005.0 — PHP's numeric-string forms.
    for lhs in ["'5'", "' 5'", "'5 '", "'5.5'", "'017'", "'+5'", "'.5'", "'1e3'", "'000123'"] {
        assert!(bin(lhs, "+", "5").is_empty(), "{lhs} is a numeric string: legal");
    }
}

#[test]
fn silent_on_leading_numeric_string_which_is_only_a_warning() {
    // "5abc" + 1 → Warning: A non-numeric value encountered … int(6) — warning-grade,
    // and this id covers fatal rows only, so it never fires here.
    for lhs in ["'5abc'", "'.5abc'", "'0x1A'", "'0b11'", "'1_000'", "'1e'"] {
        assert!(bin(lhs, "+", "1").is_empty(), "{lhs} merely warns: not this id");
    }
}

#[test]
fn silent_on_the_legal_scalar_operands() {
    // null + 1 → 1; true + 1 → 2; false + 1 → 1; 1.5 + 1 → 2.5 — all legal.
    for lhs in ["null", "true", "false", "1.5", "1"] {
        assert!(bin(lhs, "+", "1").is_empty(), "{lhs} + 1 is legal PHP");
    }
}

// Row: `& | ^` — two operators sharing a spelling.

#[test]
fn fires_on_non_numeric_string_bitwise_against_a_non_string() {
    // 'abc' {&,|,^} against 1, and against null/true/1.5, all → TypeError:
    // Unsupported operand types: string {op} {rhs}.
    for op in ["&", "|", "^"] {
        let d = bin("'abc'", op, "1");
        assert_eq!(d.len(), 1, "`'abc' {op} 1` is a TypeError: {d:#?}");
    }
    for rhs in ["null", "true", "1.5"] {
        assert_eq!(bin("'abc'", "&", rhs).len(), 1, "`'abc' & {rhs}` is a TypeError");
    }
}

#[test]
fn silent_on_string_bitwise_string() {
    // "abc" & "abc" → "abc"; "abc" | "5" → "ubc" — byte-wise, legal whatever the bytes.
    for op in ["&", "|", "^"] {
        assert!(bin("'abc'", op, "'abc'").is_empty(), "string {op} string is byte-wise");
        assert!(bin("'abc'", op, "'5'").is_empty(), "…including a numeric partner");
        assert!(bin("'abc'", op, "''").is_empty(), "…including the empty string");
    }
}

#[test]
fn fires_on_non_numeric_string_shift_even_against_a_string() {
    // Shifts have no byte-wise twin: "abc" << "5" → TypeError: Unsupported operand
    // types: string << string. Numeric pairs shift fine: '5' << '1' → int(10).
    for op in ["<<", ">>"] {
        assert_eq!(bin("'abc'", op, "'5'").len(), 1, "`'abc' {op} '5'` is a TypeError");
    }
    for op in ["<<", ">>"] {
        assert!(bin("'5'", op, "'1'").is_empty(), "numeric strings shift legally");
    }
}

// Rows: the unary operators.

#[test]
fn fires_on_unary_minus_on_array() {
    // The engine compiles unary minus as `* -1`, hence the `* int` sentence.
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
    // -"abc"/+"abc" → TypeError: Unsupported operand types: string * int (same
    // `* int` sentence as the array case).
    assert_eq!(un("-", "'abc'").len(), 1);
    assert_eq!(un("+", "'abc'").len(), 1);
    assert_eq!(un("+", "[]").len(), 1);
}

#[test]
fn silent_on_unary_minus_on_the_legal_operands() {
    // -5→-5; -1.5→-1.5; -true→-1; -null→0; -'5'→-5; -'5.5'→-5.5; -'5abc'→warns only.
    for operand in ["5", "1.5", "true", "false", "null", "'5'", "'5.5'", "'5abc'"] {
        assert!(un("-", operand).is_empty(), "-{operand} is legal (or merely warns)");
    }
}

#[test]
fn fires_on_bitwise_not_on_array_bool_and_null() {
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
    // ~1 → int(-2); ~"abc" → byte-wise complement; ~1.5 → deprecation + int(-2), not
    // fatal. Asymmetric with `-`: `~` accepts every string, refuses bool/null.
    for operand in ["1", "1.5", "'abc'", "'5'", "''"] {
        assert!(un("~", operand).is_empty(), "~{operand} is not fatal");
    }
}

#[test]
fn silent_on_logical_not_which_is_total() {
    // ![] → bool(true): `!` never fatals on any operand kind, not collected as a site.
    assert!(diags("<?php\n$x = [];\n$y = !$x;\n").is_empty());
}

// The operand lane: what counts as proof.

#[test]
fn fires_on_two_proven_variables() {
    // Both operands via the env, not the source text.
    let d = diags("<?php\n$a = [];\n$b = 1;\n$c = $a * $b;\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("$a * $b"), "the message quotes the operands: {}", d[0].message);
}

#[test]
fn silent_on_a_native_string_seed_whose_content_is_unknown() {
    // A native `string` param proves the BASE but not the content, and only the
    // content decides `'5'+1` (legal) vs `'abc'+1` (fatal) — deliberate silence.
    let src = "<?php\nfunction f(string $s): void { $x = $s + 1; }\n";
    assert!(diags(src).is_empty(), "an abstract string base proves nothing about the content");
    // A native `array` param carries no value-domain fact at all (scalars-only seed).
    let arr = "<?php\nfunction f(array $a): void { $x = $a + 1; }\n";
    assert!(diags(arr).is_empty(), "a native array hint seeds no fact — out of reach, not wrong");
}

#[test]
fn fires_on_a_native_int_seed() {
    // The abstract layer DOES prove int/float/bool rows: `int + array` is fatal
    // whatever the int is.
    let src = "<?php\nfunction f(int $n): void { $a = []; $x = $n + $a; }\n";
    assert_eq!(diags(src).len(), 1, "a native scalar seed is a Verified fact: {:#?}", diags(src));
}

#[test]
fn silent_on_a_maybe_union_operand() {
    // One branch an array, the other an int: the merged fact is a heterogeneous
    // `OneOf`, proving no single operand kind — silence, not a false proof.
    let src = "<?php\nif (rand()) {\n    $a = [];\n} else {\n    $a = 1;\n}\n$b = $a - 1;\n";
    assert!(diags(src).is_empty(), "a Maybe operand is silence: {:#?}", diags(src));
}

#[test]
fn silent_on_a_nullable_operand() {
    // `?array` is array-or-null — opposite verdicts under `-` (fatal / legal `0-1`).
    let src = "<?php\nfunction f(?array $a): void { $x = $a - 1; }\n";
    assert!(diags(src).is_empty(), "a nullable operand is silence");
}

#[test]
fn fires_on_a_homogeneous_union_of_fatal_strings() {
    // OneOf's other side: every member the SAME kind still proves it (`'abc'|'def'`).
    let src = "<?php\nif (rand()) {\n    $a = 'abc';\n} else {\n    $a = 'def';\n}\n$b = $a - 1;\n";
    assert_eq!(diags(src).len(), 1, "a homogeneous union still proves the kind");
}

#[test]
fn silent_on_an_asserted_premise() {
    // ADR-0052 §5: an `Asserted` fact (a `@phpstan-assert` claim) cannot premise a fatal.
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

// The object posture — the GMP-shaped silence.

#[test]
fn silent_on_an_object_operand_of_an_unknown_class() {
    // Plain objects fatal in `+` (stdClass + 1 → Unsupported operand types: stdClass
    // + int), but internal classes like GMP overload arithmetic — so every object
    // operand is silent by construction (no object denotation), not an allowlist.
    let unknown = "<?php\nfunction f(SomeExternalClass $o): void { $x = $o + 1; }\n";
    assert!(diags(unknown).is_empty(), "an object of an unknown class stays silent");
    let gmp = "<?php\n$a = gmp_init(1);\n$b = gmp_init(2);\n$c = $a + $b;\n";
    assert!(diags(gmp).is_empty(), "GMP overloads `+`: silence is the correct posture");
    let plain = "<?php\nclass C {}\n$o = new C();\n$x = $o + 1;\n";
    assert!(diags(plain).is_empty(), "even a proven userland object is out of v1's reach");
}

// Excluded operator families, pinned.

#[test]
fn silent_on_concat_which_is_issue_193s_territory() {
    // Array-in-concat → Warning: Array to string conversion (issue #193's
    // `string.array-conversion`/`string.non-stringable`, not `.`'s own id).
    assert!(diags("<?php\n$x = [];\n$y = $x . 'a';\n").is_empty(), "array-in-concat is #193's");
    assert!(diags("<?php\n$x = 'abc';\n$y = $x . 1;\n").is_empty(), "and plain concat is legal");
    // Object-in-concat → fatal (Object of class C could not be converted to
    // string), but still #193's id, not this one.
    let obj = "<?php\nclass C {}\n$o = new C();\n$y = $o . 'a';\n";
    assert!(diags(obj).is_empty(), "object-in-concat is #193's id, not this one");
}

#[test]
fn silent_on_every_comparison_operator() {
    // All legal at 8.5.9 — no comparison ever fatals: []<1→false; 1<[]→true;
    // []<=>1→1; []==1→false; []===1→false; 'abc'<1→false; []>1.5→true. Zero rows.
    for op in ["<", ">", "<=", ">=", "<=>", "==", "!=", "===", "!==", "<>"] {
        assert!(bin("[]", op, "1").is_empty(), "`[] {op} 1` is legal PHP");
        assert!(bin("'abc'", op, "1").is_empty(), "`'abc' {op} 1` is legal PHP");
    }
}

#[test]
fn silent_on_increment_and_decrement() {
    // `$a++` on array → TypeError: Cannot increment array — genuinely fatal, but a
    // mutation statement, not an operand expression; out of v1's reach.
    for src in ["<?php\n$x = [];\n$x++;\n", "<?php\n$x = [];\n++$x;\n", "<?php\n$x = [];\n$x--;\n"]
    {
        assert!(diags(src).is_empty(), "++/-- is not this id in v1");
    }
}

#[test]
fn silent_on_division_by_zero() {
    // 1/0 → DivisionByZeroError — a value question, not operand-TYPE; no row covers it.
    assert!(diags("<?php\n$z = 0;\n$y = 1 / $z;\n").is_empty());
    assert!(diags("<?php\n$z = 0;\n$y = 1 % $z;\n").is_empty());
}

// Reach limits — the silences the env-correctness rules buy.

#[test]
fn silent_on_a_closure_body_reading_its_own_binding() {
    // `$s + 1` sits inside the creating statement's span, but `$s` is the closure's
    // own param, not the outer `'abc'` — `enclosing_body` keeps the outer env unasked.
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
    // A `while` body is an ADR-0027 `Opaque` construct: the entry env isn't the env
    // its statements run under, so no site inside it is judged — out of reach.
    let src = "<?php\n$x = 1;\nwhile (rand()) {\n    $x = [];\n    $y = $x + 1;\n}\n";
    assert!(diags(src).is_empty(), "a loop body is out of reach: {:#?}", diags(src));
}

#[test]
fn fires_in_an_if_branch_exactly_once() {
    // A structured `if` IS modelled (ADR-0031): branch statements walk with the
    // branch env, and the containing `if` must not report the site twice.
    let src = "<?php\nfunction f(): void {\n    if (rand()) {\n        $a = [];\n        $b = $a + 1;\n    }\n}\n";
    let d = diags(src);
    assert_eq!(d.len(), 1, "exactly one report, from the branch's own statement: {d:#?}");
    assert_eq!(d[0].line, 5, "{d:#?}");
}

#[test]
fn fires_once_per_site_in_a_nested_expression() {
    // Nested applications are separate sites; only the one whose BOTH operands are
    // proven fires — `(1+2) + []`'s outer left operand lowers to `Other`, so silence.
    assert!(diags("<?php\n$a = [];\n$b = (1 + 2) + $a;\n").is_empty());
    assert_eq!(diags("<?php\n$a = [];\n$b = $a + 1;\n").len(), 1);
}

#[test]
fn silent_at_the_documented_reach_limits() {
    // Recorded silences — positions the entry-env rule cannot reach in v1:
    // - an `if`/`while` CONDITION (branches are where the walk descends);
    let cond = "<?php\n$a = [];\nif ($a + 1) {\n    echo 'x';\n}\n";
    assert!(diags(cond).is_empty(), "a condition operand is out of reach: {:#?}", diags(cond));
    // - an arrow fn's by-value capture (real fatal at runtime, but walked with its
    //   own env);
    let capture = "<?php\n$s = 'abc';\n$f = fn () => $s + 1;\n";
    assert!(diags(capture).is_empty(), "a captured operand is out of reach");
    // - a class-constant or property-default expression (no statement in any scope
    //   trace).
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

// The `warning-handler` posture does NOT apply: fatal rows only.

#[test]
fn the_warning_handler_posture_does_not_demote_this_id() {
    // Unlike `foreach.non-iterable`/`offset.missing`, every row here is a fatal
    // `TypeError`, so `warning-handler = "null"` changes nothing (ADR-0049 §7).
    let tree = SourceTree::parse("<?php\n$a = [];\n$b = $a + 1;\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut NoFold, false)
        .into_iter()
        .filter(|d| d.id == INVALID_OPERAND_ID)
        .collect();
    assert_eq!(d.len(), 1, "a fatal row never demotes: {d:#?}");
}
