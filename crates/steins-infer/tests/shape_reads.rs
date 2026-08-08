//! ADR-0062 S3 — the first construction sites for `Fact::Shape`: seeding a
//! declared array `@param` into the value lane, constant-key reads against it,
//! and the `count`/`array_is_list` transfers (§4).
//!
//! Two disciplines are pinned here, not just the spellings:
//!
//! * **A-G9** — a read is never null-poisoned. An undischarged optional key
//!   yields NO fact, never the declared value ∪ null.
//! * **Zero emission (A-G9's corollary)** — shape reads must not produce a
//!   finding. Every test that exercises a shape asserts the non-debug diagnostic
//!   list is empty, and [`no_findings_from_shape_reads`] sweeps the matrix.
//!
//! NB: a variable handed to a call is invalidated after that statement
//! (pre-existing by-ref conservatism), so each fixture dumps a binding once.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A mock sidecar answering the reflected envelopes these transfers consult,
/// plus the absence-family boot surface (there is no PHP in a unit test). Without an envelope the type
/// rung is *withheld*, which is the gate working — so a transfer test needs
/// this mock to observe anything at all.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
    absence: bool,
}

impl Mock {
    /// `count` reflects `int` refined by the curated `int<0, max>` row;
    /// `array_is_list` reflects bare `bool` (ADR-0056's own R3 rows).
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        facts.insert("array_is_list".to_owned(), Fact::General { base: Base::Bool, nullable: false });
        Mock { facts, absence: true }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.absence
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    // The `untyped.*` family (ADR-0078, issue #200) reports on the FIXTURES' own
    // declarations — deliberately untyped here — not on the behaviour under test.
    // Dropped so every assertion below keeps meaning what it meant before it landed.
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding (the zero-emission discipline).
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "shape facts emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A one-function fixture: `@param <decl> $v`, body `<body>`.
fn fixture(decl: &str, body: &str) -> String {
    format!("<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ {body} }}\n")
}

fn dump(decl: &str, expr: &str) -> String {
    one_type(&fixture(decl, &format!("\\PHPStan\\dumpType({expr});")))
}

// ---- Seeding (the shape reaches the value lane) ----------------------------

#[test]
fn a_declared_shape_param_spells_its_shape() {
    assert_eq!(
        dump("array{a: string, b?: int}", "$v"),
        "dumped type: array{a: string, b?: int} (asserted)"
    );
}

#[test]
fn a_declared_list_param_spells_the_generic_form() {
    // The degenerate form (A-G1) spells as the generic vocabulary it lowered
    // from — never as a brace shape holding only a tail.
    assert_eq!(dump("list<string>", "$v"), "dumped type: list<string> (asserted)");
}

// ---- Reads (§4's read row) -------------------------------------------------

#[test]
fn a_required_field_reads_its_value_slot() {
    assert_eq!(dump("array{a: string, b?: int}", "$v['a']"), "dumped type: string (asserted)");
}

#[test]
fn an_optional_field_reads_unknown_and_is_never_null_poisoned() {
    // A-G9: NOT `int|null`. The missing-ness hazard is S6's strict-leg finding,
    // never a type pollution.
    assert_eq!(dump("array{a: string, b?: int}", "$v['b']"), "dumped type: unknown");
}

#[test]
fn an_undeclared_key_under_a_sealed_shape_reads_unknown_and_emits_nothing() {
    // The `ShapeRead::DeclaredAbsent` outcome — S6 wires `offset.undeclared`
    // here; S3 spells unknown and says nothing.
    assert_eq!(dump("array{a: string, b?: int}", "$v['zzz']"), "dumped type: unknown");
}

#[test]
fn an_unsealed_tail_supplies_the_value_bound_for_an_undeclared_key() {
    assert_eq!(dump("array<string, int>", "$v['k']"), "dumped type: int (asserted)");
}

#[test]
fn a_key_the_tail_key_class_rejects_reads_unknown() {
    // `array<string, int>` cannot hold the int key 3 — declared absence, not a
    // tail read.
    assert_eq!(dump("array<string, int>", "$v[3]"), "dumped type: unknown");
}

#[test]
fn a_nested_array_field_reads_a_nested_shape() {
    assert_eq!(dump("array{inner: list<int>}", "$v['inner']"), "dumped type: list<int> (asserted)");
}

#[test]
fn an_unrepresentable_slot_reads_unknown() {
    // A class-typed field has no fact form (A-G1a's honest floor); the declared
    // fidelity stays in the arm lane.
    assert_eq!(dump("array{a: Foo}", "$v['a']"), "dumped type: unknown");
}

#[test]
fn a_nullable_base_declines_the_read() {
    // The base may be null, so no field is guaranteed — narrowing that is S4's.
    let src = "<?php\n/** @param array{a: string}|null $v */\n\
               function f(?array $v): void { \\PHPStan\\dumpType($v['a']); }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn an_unproven_key_declines_the_read() {
    // The key resolution is the offset family's: a proven single value or
    // nothing.
    let src = "<?php\n/** @param array{a: string} $v */\n\
               function f(array $v, string $k): void { \\PHPStan\\dumpType($v[$k]); }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn a_read_binds_into_the_env_for_later_use() {
    // The read is a value-lane binding, not a dump-only rendering.
    let src = "<?php\n/** @param array{a: string} $v */\n\
               function f(array $v): void { $x = $v['a']; \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: string (asserted)");
}

#[test]
fn php_key_normalization_is_the_offset_familys() {
    // `'5'` denotes the int key 5 (the ONE canonicalization, ADR-0049 A10).
    assert_eq!(dump("array{5: string}", "$v['5']"), "dumped type: string (asserted)");
}

// ---- Transfers (§4's count / array_is_list rows) ---------------------------

#[test]
fn count_of_a_sealed_all_required_shape_is_exact() {
    assert_eq!(dump("array{x: int, y: int}", "count($v)"), "dumped type: 2 (asserted)");
}

#[test]
fn count_with_an_optional_key_is_the_required_to_declared_range() {
    assert_eq!(dump("array{a: string, b?: int}", "count($v)"), "dumped type: int<1, 2> (asserted)");
}

#[test]
fn count_of_an_optional_only_shape_starts_at_zero() {
    assert_eq!(dump("array{a?: string}", "count($v)"), "dumped type: int<0, 1> (asserted)");
}

#[test]
fn count_of_a_non_empty_generic_floors_at_one() {
    // An unsealed tail has no ceiling, so the range IS `positive-int`.
    assert_eq!(dump("non-empty-list<string>", "count($v)"), "dumped type: int<1, max> (asserted)");
}

#[test]
fn array_is_list_answers_the_denotational_flag() {
    assert_eq!(dump("list<string>", "array_is_list($v)"), "dumped type: true (asserted)");
    assert_eq!(dump("array{x: int, y: int}", "array_is_list($v)"), "dumped type: false (asserted)");
}

#[test]
fn array_is_list_declines_on_maybe() {
    // `array{a?: string}` admits both `[]` (a list) and `['a' => …]` (not one),
    // so the flag answers nothing and the envelope rung stands.
    assert_eq!(dump("array{a?: string}", "array_is_list($v)"), "dumped type: bool");
}

#[test]
fn a_second_argument_declines_the_count_rule() {
    // `count($x, COUNT_RECURSIVE)` counts something else entirely.
    assert_eq!(
        dump("array{x: int, y: int}", "count($v, COUNT_RECURSIVE)"),
        "dumped type: int<0, max>"
    );
}

#[test]
fn a_transfer_binds_into_the_env_for_later_use() {
    let src = "<?php\n/** @param array{x: int, y: int} $v */\n\
               function f(array $v): void { $n = count($v); \\PHPStan\\dumpType($n); }\n";
    assert_eq!(one_type(src), "dumped type: 2 (asserted)");
}

// ---- Emission discipline (A-G9's corollary) --------------------------------

#[test]
fn shape_reads_feed_nothing_but_the_strict_leg() {
    // A-G9: a shape-derived fact may produce only the contract-layer offset ids.
    // No proof-layer id may appear; enumerating the two allowed ids prevents a new
    // contract finding from slipping in unnoticed.
    let strict_leg = ["offset.undeclared", "offset.maybe-missing"];
    let bodies = [
        "$x = $v['a']; return;",
        "$x = $v['zzz']; return;",
        "$x = $v['b']; return;",
        "$n = count($v); $b = array_is_list($v); return;",
        "if ($v) { return; }",
        "if (count($v) > 0) { return; }",
        "foreach ($v as $k => $e) { $y = $e; }",
        "$x = $v['a'] ?? 'd';",
        "unset($v['a']);",
        "$v['c'] = 1;",
    ];
    for body in bodies {
        for decl in ["array{a: string, b?: int}", "list<string>", "array<string, int>", "array"] {
            let src = fixture(decl, body);
            let ds = diagnostics(&src);
            let found: Vec<&Diagnostic> = ds
                .iter()
                .filter(|d| !d.id.starts_with("debug.") && !strict_leg.contains(&d.id))
                .collect();
            assert!(found.is_empty(), "`{decl}` + `{body}` emitted {found:?}");
        }
    }
}

#[test]
fn a_shape_seed_does_not_disturb_the_proof_leg() {
    // The PROOF-layer offset check judges proven whole values only; a shape base is
    // silent there, and the `Asserted` seed is invisible to its Verified-only
    // operand gate. The contract-layer leg may report `$v['nope']` as a declared
    // absence, but the proof leg must remain silent.
    let shaped = "<?php\n/** @param array{a: string} $v */\n\
                  function f(array $v): void { $x = $v['nope']; }\n";
    let ds = diagnostics(shaped);
    assert!(
        ds.iter().all(|d| d.id != "offset.missing" && d.id != "offset.on-unsupported"),
        "a shape base must never reach the PROOF-layer offset emitters: {ds:?}"
    );
    assert!(
        ds.iter().any(|d| d.id == "offset.undeclared"),
        "…while the contract-layer strict leg does judge it (S6): {ds:?}"
    );
    let concrete = "<?php\nfunction g(): void { $a = ['a' => 1]; $x = $a['nope']; }\n";
    assert!(
        diagnostics(concrete).iter().any(|d| d.id == "offset.missing"),
        "the proven-value offset proof must still fire"
    );
}

#[test]
fn a_multi_array_arm_lane_seeds_no_shape_fact() {
    // A-G3: a shape∪shape union lives in the arm lane until a guard subtracts it
    // to one (S4). No fact ⇒ no read, no transfer.
    let src = "<?php\n/** @param array{a: string}|array{b: int} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(count($v)); }\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}

#[test]
fn a_mixed_union_seeds_no_shape_fact() {
    // A-G2: mixed-base unions stay un-facted, exactly as for scalars.
    let src = "<?php\n/** @param array{a: string}|string $v */\n\
               function f($v): void { \\PHPStan\\dumpType(count($v)); }\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}

#[test]
fn a_nullable_shape_keeps_the_null_arm_in_the_fact() {
    // A-G2: `nullable` is the side-flag, never a field inside the shape — and a
    // nullable base declines every read until S4 narrows it.
    let src = "<?php\n/** @param array{a: string}|null $v */\n\
               function f(?array $v): void { \\PHPStan\\dumpType(count($v)); }\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}
