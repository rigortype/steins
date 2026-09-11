//! Issue #646 — a builtin call in **operand** position reads the same
//! builtin-return rung the dump and assignment seams already read.
//!
//! The defect was never a wrong answer; it was one value with two answers.
//! `value_operand_fact` fell through to `transfer_arg_known`, which has a `Var`
//! rung, an array-literal rung and a literal rung and no builtin-return rung at
//! all — so `(string) rand()` took the cast's floor while
//! `$r = rand(); (string) $r` read the catalog through the assignment seam.
//!
//! What this file pins is therefore a **property**, not a table of answers: for
//! every operator reading the shared operand seam, the direct spelling and the
//! hoisted one agree. Five of the eight rows in the issue's table agreed already
//! (their answer is the operator's own total `bool` / `int<-1, 1>`, which no
//! operand fact refines) and they are asserted here too — an arm that made them
//! disagree would be answering more than the two existing seams do, which is
//! exactly what the issue forbids.

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A mock PHP answering for exactly one name, `strlen` — the **`Verified`** rung
/// (ADR-0056 §2: the engine's own arginfo, refined by an admitted curated row).
/// Every other builtin here reaches the `Asserted` declared-return floor
/// (ADR-0069) instead, which needs no engine at all, so both rungs are exercised
/// side by side. `fold` is the third: a call over a proven literal has a *value*,
/// and the rung must not outrank it.
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

    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        (name == "strlen")
            .then(|| Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false))
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

/// Every dumped type a source produces, in order.
fn dumps(ds: &[Diagnostic]) -> Vec<String> {
    ds.iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The single dump a one-dump source produces, asserting no other finding came
/// with it: a catalog-declared return is `Asserted`, and an `Asserted` premise
/// may never reach the proof layer (ADR-0061 §3).
fn one_dump(src: &str) -> String {
    let ds = findings(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "an operand fact premised a finding: {other:?}");
    let d = dumps(&ds);
    assert_eq!(d.len(), 1, "expected exactly one dump, got {d:?}");
    d[0].clone()
}

/// `dumpType(<expr>)` with the call written **directly** in operand position;
/// `@` stands for the operand.
fn direct(op: &str, call: &str) -> String {
    let e = op.replace('@', call);
    one_dump(&format!("<?php\nfunction f(string $s): void {{ \\PHPStan\\dumpType({e}); }}\n"))
}

/// The same expression over the same call **hoisted to a variable** — the
/// spelling that always worked, and the specification for the one that did not.
fn hoisted(op: &str, call: &str) -> String {
    let e = op.replace('@', "$r");
    one_dump(&format!(
        "<?php\nfunction f(string $s): void {{ $r = {call}; \\PHPStan\\dumpType({e}); }}\n"
    ))
}

/// The issue's eight-row table, plus the concatenation its re-measurement added
/// (issue #627 put `.` on this seam after the issue was filed), so the property
/// is pinned on the syntax that makes it visible in ordinary code.
const OPERATORS: &[&str] = &[
    "(string) @",
    "(int) @",
    "(array) @",
    "(bool) @",
    "(float) @",
    "!@",
    "@ <=> 1",
    "@ >= 0",
    "@ . 'x'",
];

#[test]
fn one_value_has_one_answer_however_it_is_spelled() {
    // `rand()` reaches the ADR-0069 floor (`int`, `Asserted`); `strlen($s)`
    // reaches the engine's envelope (`int<0, max>`, `Verified`); `realpath($s)`
    // is a **multi-arm** floor row (`string|false`), which the value lane cannot
    // seed at all and the arm lane carries through ADR-0085's union — the leg
    // that makes the agreement hold for more than a single-arm row. All three
    // ladders, every operator: the acceptance criterion as one loop rather than
    // restated twenty-seven times.
    for call in ["rand()", "strlen($s)", "realpath($s)"] {
        for op in OPERATORS {
            assert_eq!(
                direct(op, call),
                hoisted(op, call),
                "`{}` disagrees with its hoisted twin",
                op.replace('@', call),
            );
        }
    }
}

#[test]
fn the_seams_read_one_ladder() {
    // The dump seam and the assignment seam answered this builtin identically
    // before the rung existed; the operand seam now joins them, and the three
    // are asserted against one another rather than against a transcribed string.
    assert_eq!(direct("@", "rand()"), hoisted("@", "rand()"));
    assert_eq!(direct("@", "rand()"), "int (asserted)");
}

#[test]
fn a_catalog_declared_return_enters_asserted() {
    // The headline row, with its stratum: a catalog row is a declaration, not a
    // runtime answer, so the marker rides the operand fact into the cast.
    assert_eq!(direct("(string) @", "rand()"), "numeric-uncased-string (asserted)");
    // …and it does NOT ride a result that IS the cast's floor: `(int)` of an int
    // is the operator's own guarantee reached the long way round, owed to no
    // operand, so it is `Verified` however the operand was spelled (issue #260's
    // stratum ruling, applied by `eval_cast_fact`). The hoisted twin prints the
    // same, which is the only thing this rung promises.
    assert_eq!(direct("(int) @", "rand()"), "int");
    assert_eq!(direct("(int) @", "rand()"), hoisted("(int) @", "rand()"));
}

#[test]
fn the_engines_own_envelope_stays_verified() {
    // No `(asserted)`: `strlen`'s range is read off the running engine's arginfo
    // (ADR-0056 §2), and the operand seam may not demote it on the way through.
    assert_eq!(direct("(int) @", "strlen($s)"), "int<0, max>");
}

#[test]
fn a_name_nothing_declares_still_declines() {
    // The decline is total, as it was before the rung existed: the operator takes
    // its own floor and nothing invents a fact for a name neither the engine nor
    // the catalog describes. The undefined-function finding is the fixture's.
    let ds = findings("<?php\nfunction f(): void { \\PHPStan\\dumpType((string) nosuchfn()); }\n");
    assert_eq!(dumps(&ds), vec!["string".to_owned()]);
}

#[test]
fn a_folded_call_is_a_value_first() {
    // The rung sits BELOW the literal reader, not in front of it: `strlen('abc')`
    // has a value, and no declared return may outrank it.
    assert_eq!(direct("(int) @", "strlen('abc')"), "3");
    assert_eq!(direct("(int) @", "strlen('abc')"), hoisted("(int) @", "strlen('abc')"));
}
