//! ADR-0062 S4 — guards over the array stratum: presence promotion, arm
//! subtraction (presence- and tag-based), the collapse that mints a fact from a
//! subtracted arm lane, and the A-G8 write/unset invalidation table.
//!
//! Disciplines pinned here, not just the spellings: shape facts are `Asserted`
//! (A-G9's corollary) and must never decide a guard verdict or prune a region —
//! the historical FP class, tripwired by
//! [`shape_facts_do_not_decide_guard_verdicts`]; zero emission (as in S3);
//! flavor discipline (A-G8 S2) — `isset` strips null, `array_key_exists` does
//! not, on both lanes; and the write rule is contained — `$x['k'] = v` /
//! `unset($x['k'])` keep pre-S4 barrier semantics for every binding but the
//! base's shape.
//!
//! NB: a call invalidates its argument after the statement (by-ref
//! conservatism), so each fixture dumps a binding once.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The same mock sidecar S3's suite uses (ADR-0061 admission gate: `count`/
/// `array_is_list` transfers, plus the absence-family boot surface).
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
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
}

/// The single `debug.type` body a one-dump source produces (asserts zero-emission too).
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "shape guards emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A one-function fixture: `@param <decl> $v`, body `<body>`.
fn fixture(decl: &str, body: &str) -> String {
    format!("<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ {body} }}\n")
}

/// Guard `<guard>`, then dump `<expr>` inside its true branch.
fn guarded(decl: &str, guard: &str, expr: &str) -> String {
    one_type(&fixture(decl, &format!("if ({guard}) {{ \\PHPStan\\dumpType({expr}); }}")))
}

/// Guard `<guard>`, then dump `<expr>` inside its FALSE branch.
fn guarded_else(decl: &str, guard: &str, expr: &str) -> String {
    one_type(&fixture(
        decl,
        &format!("if ({guard}) {{ return; }} \\PHPStan\\dumpType({expr});"),
    ))
}

// ---- Presence promotion on a single shape (A-G3 / #51 L3) ------------------

#[test]
fn isset_promotes_an_optional_key_to_its_declared_value() {
    assert_eq!(
        guarded("array{a?: string, b?: string}", "isset($v['a'])", "$v['a']"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn isset_promotion_does_not_leak_to_the_sibling_key() {
    assert_eq!(
        guarded("array{a?: string, b?: string}", "isset($v['a'])", "$v['b']"),
        "dumped type: unknown"
    );
}

#[test]
fn isset_strips_null_from_the_value_slot_but_array_key_exists_does_not() {
    // A-G8 S2 flavor correction: `isset` is false on a present-null entry,
    // `array_key_exists` is true.
    assert_eq!(
        guarded("array{a?: string|null}", "isset($v['a'])", "$v['a']"),
        "dumped type: string (asserted)"
    );
    assert_eq!(
        guarded("array{a?: string|null}", "array_key_exists('a', $v)", "$v['a']"),
        "dumped type: string|null (asserted)"
    );
}

#[test]
fn isset_on_an_unsealed_tail_promotes_the_tail_bound() {
    assert_eq!(
        guarded("array<string, int>", "isset($v['k'])", "$v['k']"),
        "dumped type: int (asserted)"
    );
}

#[test]
fn isset_on_a_key_a_sealed_shape_forbids_narrows_nothing() {
    // The guard is runtime-impossible; the operator widens rather than claiming
    // a key the declaration excludes.
    assert_eq!(
        guarded("array{a?: string}", "isset($v['zzz'])", "$v['zzz']"),
        "dumped type: unknown"
    );
}

#[test]
fn assert_isset_routes_through_the_same_guard_path() {
    // `assert()` is already a throw-guard whose argument lowers to a `CondExpr`
    // (ADR-0052's 2026-07-25 amendment), so the S4 narrowing needs no
    // assert-specific plumbing.
    assert_eq!(
        one_type(&fixture(
            "array{a?: string}",
            "assert(isset($v['a'])); \\PHPStan\\dumpType($v['a']);"
        )),
        "dumped type: string (asserted)"
    );
}

// ---- `empty()` (S6-residue): PHP's own definition, lowered ------------------
//
// `empty(e)` ≡ `!isset(e) || !e`, lowered exactly that way with no
// `empty`-aware narrowing code, so both polarities below fall out of the
// compositional walk: TRUE is a disjunction of two negations (records
// nothing — the key may be absent, or present and falsy); FALSE De Morgan's
// to `isset(e) && e`, whose `isset` half is ordinary presence promotion.

#[test]
fn not_empty_promotes_the_key_exactly_as_isset_does() {
    assert_eq!(
        guarded("array{a?: string, b?: string}", "!empty($v['a'])", "$v['a']"),
        "dumped type: string (asserted)"
    );
    // The whole-array reading agrees with the hand-written equivalent.
    assert_eq!(
        guarded("array{a?: string, b?: string}", "!empty($v['a'])", "$v"),
        guarded("array{a?: string, b?: string}", "isset($v['a']) && $v['a']", "$v")
    );
}

#[test]
fn not_empty_strips_null_from_the_slot() {
    // Inherited from the `isset` half: a present-null entry is still `empty`.
    assert_eq!(
        guarded("array{a?: string|null}", "!empty($v['a'])", "$v['a']"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn the_empty_true_branch_promotes_nothing() {
    // Absent-or-falsy decides nothing about presence — silence is correct, not a gap.
    assert_eq!(guarded("array{a?: string}", "empty($v['a'])", "$v['a']"), "dumped type: unknown");
}

#[test]
fn the_empty_false_branch_promotes_the_key() {
    assert_eq!(
        guarded_else("array{a?: string, b?: string}", "empty($v['a'])", "$v['a']"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn empty_promotion_does_not_leak_to_the_sibling_key() {
    assert_eq!(
        guarded("array{a?: string, b?: string}", "!empty($v['a'])", "$v['b']"),
        "dumped type: unknown"
    );
}

#[test]
fn empty_outside_the_depth_one_projection_scope_narrows_nothing() {
    // `empty($v)` on a bare variable and a deeper path keep the pre-existing
    // `Opaque` lowering — the scope is `isset`'s (A-G4), deliberately.
    assert_eq!(guarded("array{a?: string}", "!empty($v)", "$v['a']"), "dumped type: unknown");
    assert_eq!(
        guarded("array{a?: array{b?: string}}", "!empty($v['a']['b'])", "$v['a']"),
        "dumped type: unknown"
    );
}

#[test]
fn empty_does_not_decide_a_guard_verdict_over_a_declared_shape() {
    // The S4 tripwire, restated for the new lowering: `empty($v['a'])` on a
    // declared-required non-null key would be decidable from the shape fact —
    // and deciding it would prune a region from an `Asserted` premise.
    let src = "<?php\n/** @param array{a: int} $v */\nfunction f(array $v): void \
               { if (empty($v['a'])) { \\PHPStan\\dumpType(1); } else { \\PHPStan\\dumpType(2); } }\n";
    let ds = diagnostics(src);
    let dumps: Vec<&str> =
        ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).map(|d| d.message.as_str()).collect();
    assert_eq!(dumps, vec!["dumped type: 1", "dumped type: 2"]);
    assert!(ds.iter().all(|d| d.id.starts_with("debug.")), "empty() emitted a finding: {ds:?}");
}

// ---- False branches (v1-conservative) --------------------------------------

#[test]
fn not_isset_on_an_optional_non_nullable_key_proves_absence() {
    // `isset` can only be false here if the key is absent, so the read proves it.
    assert_eq!(
        guarded_else("array{a?: string}", "isset($v['a'])", "$v['a']"),
        "dumped type: unknown"
    );
}

#[test]
fn not_isset_on_a_required_non_nullable_key_leaves_the_env_alone() {
    // Runtime-impossible but deliberately NOT marked dead (§2: death is the
    // verdict's business); the declared value survives as "nothing was learned".
    assert_eq!(
        guarded_else("array{a: string}", "isset($v['a'])", "$v['a']"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn not_array_key_exists_proves_absence_whatever_the_declared_value() {
    assert_eq!(
        guarded_else("array{a?: string|null}", "array_key_exists('a', $v)", "$v['a']"),
        "dumped type: unknown"
    );
}

// ---- Truthiness (fact lane only — never reachability) ----------------------

#[test]
fn a_truthy_array_guard_sets_non_empty_and_sharpens_count() {
    // `non-empty` + sealed one-optional is an exact size — §4's `count` row.
    assert_eq!(guarded("array{a?: int}", "$v", "count($v)"), "dumped type: 1 (asserted)");
}

#[test]
fn a_falsy_array_guard_proves_the_empty_array() {
    // Spelled by the one speller (ADR-0062 §6 / RFC D4): a sealed shape with no
    // fields is the empty array, and issue #159 spells it `array{}`.
    assert_eq!(guarded_else("array{a?: int}", "$v", "$v"), "dumped type: array{} (asserted)");
}

#[test]
fn a_falsy_guard_on_a_nullable_base_proves_nothing() {
    // `null` is falsy too, so the else branch cannot claim the empty array.
    assert_eq!(
        one_type(
            "<?php\n/** @param array{a?: int}|null $v */\nfunction f(?array $v): void \
             { if ($v) { return; } \\PHPStan\\dumpType($v); }\n"
        ),
        "dumped type: null|array{a?: int} (asserted)"
    );
}

// ---- array_is_list (the RFC's C1 flag flip) --------------------------------

#[test]
fn array_is_list_flips_the_flag_on_both_branches() {
    assert_eq!(
        guarded("array<int, string>", "array_is_list($v)", "array_is_list($v)"),
        "dumped type: true (asserted)"
    );
    assert_eq!(
        guarded_else("array<int, string>", "array_is_list($v)", "array_is_list($v)"),
        "dumped type: false (asserted)"
    );
}

// ---- array_all / array_any (A8, PHP 8.4, ADR-0062 §4) ----------------------
//
// Only ONE leg of each is unconditional: `array_all([], f)` is vacuously
// true (only its FALSY branch proves non-emptiness); `array_any([], f)` is
// vacuously false (only its TRUTHY branch does). The opposite branch of each
// is the §4 vacuity trap — it must narrow NOTHING, pinned by matching the
// unguarded read exactly.

#[test]
fn array_all_falsy_proves_non_empty_and_sharpens_count() {
    // The mirror of `a_truthy_array_guard_sets_non_empty_and_sharpens_count`:
    // non-empty + sealed one-optional collapses `count` to the exact `1`.
    assert_eq!(
        guarded_else("array{a?: int}", "array_all($v, fn ($x) => $x > 0)", "count($v)"),
        "dumped type: 1 (asserted)"
    );
}

#[test]
fn array_any_truthy_proves_non_empty_and_sharpens_count() {
    assert_eq!(
        guarded("array{a?: int}", "array_any($v, fn ($x) => $x > 0)", "count($v)"),
        "dumped type: 1 (asserted)"
    );
}

#[test]
fn array_all_truthy_is_the_vacuity_trap_and_narrows_nothing() {
    // The vacuity leg (see section note): `count` must read exactly as an
    // unguarded read would, `int<0, 1>`.
    assert_eq!(
        guarded("array{a?: int}", "array_all($v, fn ($x) => $x > 0)", "count($v)"),
        one_type(&fixture("array{a?: int}", "\\PHPStan\\dumpType(count($v));"))
    );
}

#[test]
fn array_any_falsy_is_the_vacuity_trap_and_narrows_nothing() {
    // The vacuity leg's other half (see section note).
    assert_eq!(
        guarded_else("array{a?: int}", "array_any($v, fn ($x) => $x > 0)", "count($v)"),
        one_type(&fixture("array{a?: int}", "\\PHPStan\\dumpType(count($v));"))
    );
}

#[test]
fn array_all_truthy_leaves_the_shape_fact_itself_unchanged() {
    // Same vacuity pin at the fact-lane level: no `non-empty-` modifier leaks in.
    assert_eq!(
        guarded("array{a?: int}", "array_all($v, fn ($x) => $x > 0)", "$v"),
        one_type(&fixture("array{a?: int}", "\\PHPStan\\dumpType($v);"))
    );
}

#[test]
fn array_any_falsy_leaves_the_shape_fact_itself_unchanged() {
    assert_eq!(
        guarded_else("array{a?: int}", "array_any($v, fn ($x) => $x > 0)", "$v"),
        one_type(&fixture("array{a?: int}", "\\PHPStan\\dumpType($v);"))
    );
}

#[test]
fn not_array_all_proves_non_empty_via_the_negation_route() {
    // `if (!array_all($x, $f))` — the ADR's own worked example: the outer `!`
    // flips polarity through `CondExpr::Not`, landing on the same falsy leg.
    assert_eq!(
        guarded("array{a?: int}", "!array_all($v, fn ($x) => $x > 0)", "count($v)"),
        "dumped type: 1 (asserted)"
    );
}

#[test]
fn array_all_any_do_not_special_case_the_concrete_lane() {
    // The pure-guard-call exemption only spares the shape lane: a proven concrete
    // array keeps ordinary by-ref-conservative invalidation.
    let guarded = one_type(
        "<?php\nfunction f(): void { $v = ['a' => 1]; \
         if (array_any($v, fn ($x) => $x > 0)) { \\PHPStan\\dumpType($v); } }\n",
    );
    let unguarded_but_still_a_call_argument = one_type(
        "<?php\nfunction f(): void { $v = ['a' => 1]; \
         some_unrelated_call($v); \\PHPStan\\dumpType($v); }\n",
    );
    assert_eq!(guarded, "dumped type: unknown");
    assert_eq!(guarded, unguarded_but_still_a_call_argument);
}

// ---- Recognition discipline (issue #153) -----------------------------------
//
// One helper answers "does this reference denote the global function `name`?" for
// every recognizer in the file; these pin the answer on the array-predicate side.
// Each PHP claim below was measured on php 8.5.9.

/// The one dump a namespaced fixture's guarded read produces.
fn ns_guarded(guard: &str) -> String {
    one_type(&format!(
        "<?php\nnamespace App;\n/** @param array{{a?: string}} $v */\n\
         function f(array $v): void {{ if ({guard}) {{ \\PHPStan\\dumpType($v['a']); }} }}\n"
    ))
}

#[test]
fn a_fully_qualified_array_predicate_is_the_global_builtin() {
    // `\array_key_exists(...)` is the global function whatever the namespace —
    // the spelling a namespaced file uses when it wants exactly that.
    assert_eq!(
        ns_guarded("\\array_key_exists('a', $v)"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn a_namespaced_array_predicate_is_a_different_function() {
    assert_eq!(ns_guarded("\\App\\array_key_exists('a', $v)"), "dumped type: unknown");
}

#[test]
fn a_namespace_relative_array_predicate_is_a_different_function() {
    // `namespace\array_key_exists` reaches `App\array_key_exists` ONLY — no global
    // fallback, a fatal at runtime when undefined; the stored raw name strips the
    // `namespace\` prefix, so only the reference kind distinguishes it.
    assert_eq!(ns_guarded("namespace\\array_key_exists('a', $v)"), "dumped type: unknown");
}

#[test]
fn an_aliased_import_is_a_different_array_predicate() {
    // `use function Other\thing as array_key_exists;` sends the call to
    // `Other\thing`, with no fallback to the builtin.
    let src = "<?php\nnamespace App;\nuse function Other\\thing as array_key_exists;\n\
               /** @param array{a?: string} $v */\n\
               function f(array $v): void \
               { if (array_key_exists('a', $v)) { \\PHPStan\\dumpType($v['a']); } }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

// ---- Arm subtraction + the collapse mint (A-G3 / A-G4) ---------------------

/// A two-array-arm union (A-G3 keeps it in the arm lane) until a guard subtracts it to one.
fn union_guarded(decl: &str, guard: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\nfunction f(array $v): void \
         {{ if ({guard}) {{ \\PHPStan\\dumpType({expr}); }} }}\n"
    ))
}

fn union_guarded_else(decl: &str, guard: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\nfunction f(array $v): void \
         {{ if ({guard}) {{ return; }} \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

#[test]
fn isset_subtracts_the_arm_that_cannot_hold_the_key_and_mints_the_survivor() {
    assert_eq!(
        union_guarded("array{foo: int}|array{bar: string}", "isset($v['foo'])", "$v['foo']"),
        "dumped type: int (asserted)"
    );
}

#[test]
fn not_isset_subtracts_the_arm_whose_required_key_is_non_nullable() {
    assert_eq!(
        union_guarded_else("array{foo: int}|array{bar: string}", "isset($v['foo'])", "$v['bar']"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn a_match_on_a_constant_key_discriminates_a_tagged_union() {
    // The ADR's acceptance fixture (A-G4), neutral tags.
    let src = "<?php\n/** @param array{kind: 'circle', radius: int}|\
               array{kind: 'square', side: int} $v */\nfunction f(array $v): void \
               { match ($v['kind']) { 'circle' => \\PHPStan\\dumpType($v['radius']), \
               'square' => null }; }\n";
    assert_eq!(one_type(src), "dumped type: int (asserted)");
    let other = "<?php\n/** @param array{kind: 'circle', radius: int}|\
                 array{kind: 'square', side: int} $v */\nfunction f(array $v): void \
                 { match ($v['kind']) { 'circle' => null, \
                 'square' => \\PHPStan\\dumpType($v['side']) }; }\n";
    assert_eq!(one_type(other), "dumped type: int (asserted)");
}

#[test]
fn an_identity_comparison_on_a_constant_key_discriminates_too() {
    assert_eq!(
        union_guarded(
            "array{kind: 'circle', radius: int}|array{kind: 'square', side: int}",
            "$v['kind'] === 'circle'",
            "$v['radius']"
        ),
        "dumped type: int (asserted)"
    );
}

#[test]
fn a_tag_guard_kills_only_the_arm_whose_slot_provably_cannot_match() {
    // The `'square'` tag slot rules `'circle'` out and dies; the `string`-tagged
    // arm could still match, survives, and collapses as the only survivor — literal
    // exclusivity is what A-G4 runs on.
    assert_eq!(
        union_guarded(
            "array{kind: string, radius: int}|array{kind: 'square', side: int}",
            "$v['kind'] === 'circle'",
            "$v['radius']"
        ),
        "dumped type: int (asserted)"
    );
    // With no exclusivity anywhere, nothing collapses and the read stays honest.
    assert_eq!(
        union_guarded(
            "array{kind: string, radius: int}|array{kind: string, side: int}",
            "$v['kind'] === 'circle'",
            "$v['radius']"
        ),
        "dumped type: unknown"
    );
}

#[test]
fn a_flow_refined_fact_outspells_its_declared_arm() {
    // The S3 deviation, flipped: an unrefined shape spells from the arm lane, a
    // refined one from the fact — sharper on two counts below (`Required` key,
    // implied non-emptiness) that the declared arm alone never states.
    assert_eq!(
        one_type(&fixture("array{a?: string}", "\\PHPStan\\dumpType($v);")),
        "dumped type: array{a?: string} (asserted)"
    );
    assert_eq!(
        guarded("array{a?: string}", "isset($v['a'])", "$v"),
        "dumped type: array{a: string} (asserted)"
    );
}

// ---- Invalidation (A-G8's table) -------------------------------------------

#[test]
fn a_constant_key_write_promotes_the_key_and_takes_the_value_fact() {
    assert_eq!(
        one_type(&fixture("array{a?: string}", "$v['a'] = 'x'; \\PHPStan\\dumpType($v['a']);")),
        "dumped type: 'x' (asserted)"
    );
}

#[test]
fn a_nested_write_autovivifies_the_outer_key_and_leaves_the_inner_slot_unknown() {
    assert_eq!(
        one_type(&fixture(
            "array{a?: array<string, int>}",
            "$v['a']['b'] = 1; \\PHPStan\\dumpType($v['a']);"
        )),
        "dumped type: unknown"
    );
}

#[test]
fn a_write_to_an_undeclared_key_under_a_sealed_tail_unseals_it() {
    // A-G5 lift: the write is order-witnessed truth, so the field is added AND the
    // tail unseals — `Sealed` would otherwise reject the array just built.
    assert_eq!(
        one_type(&fixture("array{a: string}", "$v['zz'] = 1; \\PHPStan\\dumpType($v['zz']);")),
        "dumped type: 1 (asserted)"
    );
}

#[test]
fn unset_marks_the_key_absent_and_stays_silent() {
    // No `offset.missing`: the Asserted world never premises a proof-layer
    // finding (A-G9's corollary) — `one_type` asserts that for us.
    assert_eq!(
        one_type(&fixture("array{a: string, b?: int}", "unset($v['a']); \\PHPStan\\dumpType($v['a']);")),
        "dumped type: unknown"
    );
}

#[test]
fn a_write_keeps_barrier_semantics_for_every_other_binding() {
    // Containment: the write rule may move only the shape lane, so an unrelated
    // proven binding is still forgotten exactly as pre-S4 `Barrier` forgot it.
    assert_eq!(
        one_type(&fixture(
            "array{a?: string}",
            "$other = 7; $v['a'] = 'x'; \\PHPStan\\dumpType($other);"
        )),
        "dumped type: unknown"
    );
}

#[test]
fn a_by_ref_builtin_drops_the_shape_fact_before_a_write_can_restore_it() {
    // The S3 fence, extended to writes: drop precedes restore, so an earlier
    // by-ref exposure leaves nothing to carry across the write.
    assert_eq!(
        one_type(&fixture(
            "array{a?: string}",
            "sort($v); $v['a'] = 'x'; \\PHPStan\\dumpType($v);"
        )),
        "dumped type: unknown"
    );
}

// ---- The reachability tripwire ---------------------------------------------

/// **Tripwire.** `Fact::truthy`, `is_null`, `int_in` and `satisfies_str` are all
/// *decisive* on `Fact::Shape` (`truthy` reads `non_empty`; the other three
/// answer `No` outright), so the first caller routing a shape fact into a guard
/// verdict re-opens the emission question this contract keeps closed —
/// observably: a guard over a shape-facted binding must never prune a region,
/// so both dumps below must survive.
#[test]
fn shape_facts_do_not_decide_guard_verdicts() {
    // `array{a: int}` is non-empty for every admitted value, so a `truthy`
    // verdict read off the fact would wrongly prune the else branch.
    let src = "<?php\n/** @param array{a: int} $v */\nfunction f(array $v): void \
               { if ($v) { \\PHPStan\\dumpType(1); } else { \\PHPStan\\dumpType(2); } }\n";
    let ds = diagnostics(src);
    let dumps: Vec<&str> = ds
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        dumps,
        vec!["dumped type: 1", "dumped type: 2"],
        "a shape fact decided a guard verdict — re-read the emission question \
         before letting it (ADR-0062 A-G9, S4)"
    );
    assert!(
        ds.iter().all(|d| d.id.starts_with("debug.")),
        "shape guards emitted a finding: {ds:?}"
    );
}

/// The same claim from the other side: `isset` on a shape-facted binding is
/// `Maybe`, so both branches stay live.
#[test]
fn an_isset_guard_over_a_declared_shape_prunes_nothing() {
    let src = "<?php\n/** @param array{a: int} $v */\nfunction f(array $v): void \
               { if (isset($v['a'])) { \\PHPStan\\dumpType(1); } else { \\PHPStan\\dumpType(2); } }\n";
    let ds = diagnostics(src);
    let dumps: Vec<&str> = ds
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(dumps, vec!["dumped type: 1", "dumped type: 2"]);
}
