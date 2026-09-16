//! ADR-0062 S5 — KeyCover recording (A-G8) and the `??` right-arm discharge
//! (A-G11): the disjunctive-assert pattern, end to end.
//!
//! What this suite pins, beyond the spellings: **only the disjunction is
//! recorded** (a `||` of presence tests over ONE binding records the claim;
//! a different binding, a non-presence disjunct, or a non-constant key
//! records nothing rather than something partial); **flavor is the weakest
//! disjunct's** (all-`isset` gives an Isset-cover, one `array_key_exists`
//! drags the whole cover to KeyExists); **the premise ladder is fragile on
//! purpose** (a `??` arm that isn't a pure depth-1 projection drops every
//! accumulated `¬isset` premise per A-G11's by-ref/global conservatism);
//! **KeyExists discharges conditionally** (a nullable premise slot means the
//! right arm may truly be missing at runtime, so the chain declines); and
//! **zero emission** (as in S3/S4, a discharge only ever ADDS a value fact).

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The same mock sidecar S3/S4 use.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
    absence: bool,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        facts
            .insert("array_is_list".to_owned(), Fact::General { base: Base::Bool, nullable: false });
        Mock { facts, absence: true }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
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
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding (the zero-emission discipline).
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "cover discharge emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A one-function fixture: `@param <decl> $v`, body `<body>`.
fn fixture(decl: &str, body: &str) -> String {
    format!("<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ {body} }}\n")
}

/// `assert(<cond>); dumpType(<expr>);`
fn asserted(decl: &str, cond: &str, expr: &str) -> String {
    fixture(decl, &format!("assert({cond}); \\PHPStan\\dumpType({expr});"))
}

const TWO: &str = "array{a?: string, b?: string}";

// The motivating pattern (ADR-0062 A-G11, issue #51 L4/L5)

#[test]
fn an_isset_disjunction_discharges_the_coalesce_right_arm() {
    assert_eq!(
        one_type(&asserted(TWO, "isset($v['a']) || isset($v['b'])", "$v['a'] ?? $v['b']")),
        "dumped type: string (asserted)"
    );
}

#[test]
fn a_key_exists_disjunction_discharges_over_non_nullable_slots() {
    assert_eq!(
        one_type(&asserted(
            TWO,
            "array_key_exists('a', $v) || array_key_exists('b', $v)",
            "$v['a'] ?? $v['b']"
        )),
        "dumped type: string (asserted)"
    );
}

/// The `if`-form is the same guard walk: the cover rides the true branch.
#[test]
fn the_if_form_records_the_same_cover() {
    let src = fixture(
        TWO,
        "if (isset($v['a']) || isset($v['b'])) { \\PHPStan\\dumpType($v['a'] ?? $v['b']); }",
    );
    assert_eq!(one_type(&src), "dumped type: string (asserted)");
}

/// Three disjuncts, one cover, a three-arm chain: the premise ladder refutes two
/// members and the third carries the claim.
#[test]
fn a_three_key_cover_discharges_a_three_arm_chain() {
    assert_eq!(
        one_type(&asserted(
            "array{a?: int, b?: int, c?: int}",
            "isset($v['a']) || isset($v['b']) || isset($v['c'])",
            "$v['a'] ?? $v['b'] ?? $v['c']"
        )),
        "dumped type: int (asserted)"
    );
}

/// A three-key cover with only ONE member refuted proves nothing: the middle arm
/// is still an undischarged optional read, so the chain has no value.
#[test]
fn a_three_key_cover_needs_every_other_member_refuted() {
    assert_eq!(
        one_type(&asserted(
            "array{a?: int, b?: int, c?: int}",
            "isset($v['a']) || isset($v['b']) || isset($v['c'])",
            "$v['a'] ?? $v['b']"
        )),
        "dumped type: unknown"
    );
}

/// One `array_key_exists` disjunct still discharges here: the premise slot is
/// non-nullable (see module doc: flavor is the weakest disjunct's).
#[test]
fn a_mixed_flavor_disjunction_reads_as_key_exists() {
    assert_eq!(
        one_type(&asserted(
            TWO,
            "isset($v['a']) || array_key_exists('b', $v)",
            "$v['a'] ?? $v['b']"
        )),
        "dumped type: string (asserted)"
    );
}

// A-G11's refusals

/// A-G11's discharge table: `array_key_exists` is satisfied by a present-**null**
/// key, which makes `isset` false — so `??` falls through and `b` may genuinely
/// be missing. Real semantics, not imprecision.
#[test]
fn a_key_exists_cover_declines_over_a_nullable_premise_slot() {
    assert_eq!(
        one_type(&asserted(
            "array{a?: ?string, b?: string}",
            "array_key_exists('a', $v) || array_key_exists('b', $v)",
            "$v['a'] ?? $v['b']"
        )),
        "dumped type: unknown"
    );
}

/// The same shape under an **isset** disjunction does discharge: `isset` already
/// excludes the present-null case that defeats the KeyExists reading.
#[test]
fn an_isset_cover_discharges_over_a_nullable_slot() {
    assert_eq!(
        one_type(&asserted(
            "array{a?: ?string, b?: string}",
            "isset($v['a']) || isset($v['b'])",
            "$v['a'] ?? $v['b']"
        )),
        "dumped type: string (asserted)"
    );
}

/// A-G11's conservatism: a call between arms may write through a reference or a
/// global, dropping every accumulated `¬isset` premise (control below: the same
/// chain without the intervening arm discharges fine).
#[test]
fn a_non_projection_arm_invalidates_the_premise_ladder() {
    let src = format!(
        "<?php\n/** @param {TWO} $v */\nfunction f(array $v, string $s): void \
         {{ assert(isset($v['a']) || isset($v['b'])); \
         \\PHPStan\\dumpType($v['a'] ?? $s ?? $v['b']); }}\n"
    );
    assert_eq!(one_type(&src), "dumped type: unknown");
}

/// The control for the test above: the identical chain WITHOUT the intervening
/// arm discharges, so the difference is the invalidation and nothing else.
#[test]
fn the_same_chain_without_the_intervening_arm_discharges() {
    let src = format!(
        "<?php\n/** @param {TWO} $v */\nfunction f(array $v, string $s): void \
         {{ assert(isset($v['a']) || isset($v['b'])); \
         \\PHPStan\\dumpType($v['a'] ?? $v['b']); }}\n"
    );
    assert_eq!(one_type(&src), "dumped type: string (asserted)");
}

/// No guard at all: the final arm is an undischarged optional read, which is
/// exactly the pre-S5 silence.
#[test]
fn an_unguarded_chain_is_unchanged() {
    assert_eq!(
        one_type(&fixture(TWO, "\\PHPStan\\dumpType($v['a'] ?? $v['b']);")),
        "dumped type: unknown"
    );
}

/// An opaque premise records no cover, so the discharge has nothing to consume —
/// the ADR's `fail3` honesty case.
#[test]
fn an_opaque_disjunctive_premise_records_nothing() {
    assert_eq!(
        one_type(&asserted(
            TWO,
            "(bool) array_intersect_key($v, array_flip(['a', 'b']))",
            "$v['a'] ?? $v['b']"
        )),
        "dumped type: unknown"
    );
}

/// One unmodelled disjunct makes the whole claim unrecordable: a disjunction is
/// only as strong as its weakest arm.
#[test]
fn a_non_presence_disjunct_records_nothing() {
    let src = format!(
        "<?php\n/** @param {TWO} $v */\nfunction f(array $v, bool $y): void \
         {{ assert(isset($v['a']) || $y); \\PHPStan\\dumpType($v['a'] ?? $v['b']); }}\n"
    );
    assert_eq!(one_type(&src), "dumped type: unknown");
}

/// A cover is a fact about ONE array: disjuncts over different bindings say
/// nothing about either.
#[test]
fn a_disjunction_over_two_bindings_records_nothing() {
    let src = format!(
        "<?php\n/** @param {TWO} $v\n * @param {TWO} $w */\nfunction f(array $v, array $w): void \
         {{ assert(isset($v['a']) || isset($w['b'])); \\PHPStan\\dumpType($v['a'] ?? $v['b']); }}\n"
    );
    assert_eq!(one_type(&src), "dumped type: unknown");
}

/// A non-constant key is outside A-G11's v1 scope on both sides.
#[test]
fn a_non_constant_key_records_nothing() {
    let src = format!(
        "<?php\n/** @param {TWO} $v */\nfunction f(array $v, string $k): void \
         {{ assert(isset($v[$k]) || isset($v['b'])); \\PHPStan\\dumpType($v['a'] ?? $v['b']); }}\n"
    );
    assert_eq!(one_type(&src), "dumped type: unknown");
}

// Recording composes with S4

/// A cover whose key a guard already promoted normalizes away (S2 invariant via
/// the S5 constructor) — the read discharges through presence, not the cover.
#[test]
fn a_cover_over_an_already_required_key_normalizes_away() {
    let src = fixture(
        TWO,
        "if (isset($v['a'])) { if (isset($v['a']) || isset($v['b'])) \
         { \\PHPStan\\dumpType($v['a'] ?? $v['b']); } }",
    );
    assert_eq!(one_type(&src), "dumped type: string (asserted)");
}

/// De Morgan on the FALSE branch: `¬(isset a ∨ isset b)` is `¬isset a ∧ ¬isset b`,
/// distributed to per-key S4 narrowing — both keys absent, sealed shape empties.
#[test]
fn the_false_branch_marks_every_disjunct_key_absent() {
    let src = fixture(
        TWO,
        "if (isset($v['a']) || isset($v['b'])) { return; } \\PHPStan\\dumpType($v);",
    );
    assert_eq!(one_type(&src), "dumped type: array{} (asserted)");
}

/// A declared-`Required` final arm is proven present without a cover.
#[test]
fn a_required_final_arm_needs_no_cover() {
    assert_eq!(
        one_type(&fixture(
            "array{a?: string, b: string}",
            "\\PHPStan\\dumpType($v['a'] ?? $v['b']);"
        )),
        "dumped type: string (asserted)"
    );
}

/// The whole disjunction being true means some `isset` returned true, and `isset`
/// on an offset of `null` is false — so an all-`isset` disjunction proves the
/// base is non-null too, and the read that needs it goes through.
#[test]
fn an_isset_disjunction_clears_the_bases_nullable_flag() {
    let src = "<?php\n/** @param array{a?: string, b?: string}|null $v */\n\
               function f(?array $v): void { assert(isset($v['a']) || isset($v['b'])); \
               \\PHPStan\\dumpType($v['a'] ?? $v['b']); }\n";
    assert_eq!(one_type(src), "dumped type: string (asserted)");
}
