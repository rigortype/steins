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
fn coalesce_null_compared_with_null_promotes_exactly_as_isset_does() {
    // Issues #933, #1079: `(X ?? null) !== null` is `isset(X)`, both operand orders,
    // and the `===` spelling is its negation.
    let decl = "array{a?: string|null, b?: string}";
    let want = "dumped type: string (asserted)";
    for guard in ["($v['a'] ?? null) !== null", "null !== ($v['a'] ?? null)"] {
        assert_eq!(guarded(decl, guard, "$v['a']"), want, "{guard}");
    }
    for guard in ["($v['a'] ?? null) === null", "null === ($v['a'] ?? null)"] {
        assert_eq!(guarded_else(decl, guard, "$v['a']"), want, "{guard}");
    }
    assert_eq!(
        guarded(decl, "($v['a'] ?? null) !== null", "$v"),
        guarded(decl, "isset($v['a'])", "$v")
    );
}

#[test]
fn a_non_null_coalesce_default_proves_nothing_about_presence() {
    // `?? 0` compared with `null` is always unequal: no presence is learned, and the
    // unguarded read keeps its finding.
    let src = fixture(
        "array{a?: string}",
        "if (($v['a'] ?? 0) !== null) { return $v['a']; }",
    )
    .replace("void", "string");
    let ds = diagnostics(&src);
    assert!(ds.iter().any(|d| d.id == "offset.maybe-missing"), "{ds:?}");
}

#[test]
fn coalesce_null_guard_reads_in_the_absent_branch_are_undeclared_as_under_not_isset() {
    // The lowering makes `=== null` the negation of `isset`, so the absent branch reads
    // the key as `!isset($v['a'])` does: `offset.undeclared`, where the old opaque
    // comparison said `offset.maybe-missing`.
    let read = |body: &str| -> Vec<String> {
        let src = fixture("array{a?: int}", body).replace("void", "int");
        let mut v: Vec<String> = diagnostics(&src)
            .into_iter()
            .map(|d| d.id.to_string())
            .filter(|i| i.starts_with("offset."))
            .collect();
        v.dedup();
        v
    };
    let twin = read("if (!isset($v['a'])) { return $v['a']; } return 0;");
    assert_eq!(twin, ["offset.undeclared"], "the isset twin");
    assert_eq!(read("if (($v['a'] ?? null) === null) { return $v['a']; } return 0;"), twin);
    assert_eq!(read("if (($v['a'] ?? null) !== null) { return 0; } return $v['a'];"), twin);
}

#[test]
fn a_bare_variable_coalesce_null_guard_is_decided_as_isset_is() {
    // `$v = "abc"` is proven non-null, so `($v ?? null) !== null` is true and the branch
    // PHP enters is analysed: the string operand is reported exactly as under `isset($v)`.
    let operand = |guard: &str| -> bool {
        let src = format!(
            "<?php\nfunction f(): int {{ $v = \"abc\"; if ({guard}) {{ return $v + 1; }} return 0; }}\n"
        );
        diagnostics(&src).iter().any(|d| d.id == "type.invalid-operand")
    };
    assert!(operand("isset($v)"), "the isset twin");
    assert!(operand("($v ?? null) !== null"));
}

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
    // A deeper path keeps the `Opaque` lowering — the scope is `isset`'s (A-G4),
    // deliberately.
    assert_eq!(
        guarded("array{a?: array{b?: string}}", "!empty($v['a']['b'])", "$v['a']"),
        "dumped type: unknown"
    );
}

#[test]
fn a_bare_non_empty_guard_proves_a_single_optional_key() {
    // A bare `empty($v)` is `!$v` (#1079): its false side is non-empty, which
    // holds the one declared optional key.
    assert_eq!(
        guarded("array{a?: string}", "!empty($v)", "$v['a']"),
        "dumped type: string (asserted)"
    );
    // With two optional keys, non-empty proves neither.
    assert_eq!(
        guarded("array{a?: string, b?: string}", "!empty($v)", "$v['a']"),
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

#[test]
fn a_narrowed_sibling_key_keeps_a_float_fields_spelling() {
    // Issue #424: `steins_contract::to_fact`'s float/int floor leaves a
    // `float` field's value-lane slot `None` from the seed (`float` accepts
    // ints, `Fact::General { Float }` doesn't), so once ANY key of the same
    // array gets narrowed the fact-lane spelling — the ONE speller a
    // flow-refined shape uses — must still fall back to the declared arm for
    // `b` rather than printing `mixed`. Both `isset` and its `!== null` twin
    // (#418/#421) are pinned; the sibling's own presence/value are untouched.
    assert_eq!(
        guarded("array{a?: string, b: float}", "isset($v['a'])", "$v"),
        "dumped type: array{a: string, b: float} (asserted)"
    );
    // The `!== null` twin reads the optional key unguarded, which PHP warns about
    // (`Undefined array key "a"`, witnessed on PHP 8.5): its condition is a read position
    // since issue #1050, and the strict leg reports it there.
    let src = fixture(
        "array{a?: string, b: float}",
        "if ($v['a'] !== null) { \\PHPStan\\dumpType($v); }",
    );
    let ds = diagnostics(&src);
    let ids: Vec<&str> = ds.iter().filter(|d| !d.id.starts_with("debug.")).map(|d| d.id).collect();
    assert_eq!(ids, ["offset.maybe-missing"], "{ds:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "{ds:?}");
    assert_eq!(ty[0].message, "dumped type: array{a: string, b: float} (asserted)");
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
fn a_write_keeps_a_bystander_in_a_frame_that_cannot_alias() {
    // This pinned `unknown` until issue #641: the write rule moved only the shape
    // lane, and the barrier in front of it erased every other binding in the scope.
    // The barrier now opens only as wide as ADR-0063 §2.3's two legs say it must,
    // and `$other` is a name no reference in this frame could have tied to `$v`.
    assert_eq!(
        one_type(&fixture(
            "array{a?: string}",
            "$other = 7; $v['a'] = 'x'; \\PHPStan\\dumpType($other);"
        )),
        "dumped type: 7"
    );
}

#[test]
fn a_write_keeps_barrier_semantics_for_every_other_binding_in_an_aliased_frame() {
    // The other side of the same rule: one `&` anywhere in the frame and the
    // barrier is total again, exactly as it was before issue #641.
    assert_eq!(
        one_type(&fixture(
            "array{a?: string}",
            "$other = 7; $ref = ['k' => &$other]; $v['a'] = 'x'; \\PHPStan\\dumpType($other);"
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

// ---- The falsy side of a guard on a shape is `array{}` (#1105) --------------

/// The `offset.*` ids of `<body>` over `@param <decl> $v` (native type `native`),
/// with the sidecar mock's absence family on.
fn offset_ids(decl: &str, native: &str, body: &str) -> Vec<&'static str> {
    let src = format!(
        "<?php\n/** @param {decl} $v */\nfunction f({native} $v): ?int {{ {body} }}\n"
    );
    let mut ids: Vec<&'static str> = diagnostics(&src)
        .into_iter()
        .map(|d| d.id)
        .filter(|i| i.starts_with("offset."))
        .collect();
    ids.dedup();
    ids
}

#[test]
fn a_falsy_array_reads_as_the_declared_empty_shape() {
    let decl = "array{x?: int}";
    let declared = offset_ids("array{}", "array", "return $v['x'];");
    assert_eq!(declared, ["offset.undeclared"]);
    for guard in ["empty($v)", "!$v"] {
        let body = format!("if ({guard}) {{ return $v['x']; }} return 1;");
        assert_eq!(offset_ids(decl, "array", &body), declared, "{guard}");
    }
    // The `=== []` side keeps its proof-grade id with the sidecar.
    assert_eq!(
        offset_ids(decl, "array", "if ($v === []) { return $v['x']; } return 1;"),
        ["offset.missing"]
    );
}

#[test]
fn a_falsy_array_dumps_as_the_empty_shape_asserted() {
    assert_eq!(guarded("array{x?: int}", "!$v", "$v"), "dumped type: array{} (asserted)");
    assert_eq!(guarded("array{x?: int}", "empty($v)", "$v"), "dumped type: array{} (asserted)");
}

#[test]
fn a_falsy_nullable_array_binds_nothing() {
    // `null` is falsy too.
    assert!(
        offset_ids("array{x?: int}|null", "?array", "if (!$v) { return $v['x']; } return 1;")
            .is_empty()
    );
}

#[test]
fn a_write_after_the_falsy_guard_promotes_the_empty_shape() {
    assert!(
        offset_ids(
            "array{x?: int}",
            "array",
            "if (!$v) { $v['x'] = 1; return $v['x']; } return 1;"
        )
        .is_empty()
    );
}

#[test]
fn a_falsy_array_probe_of_the_key_stays_silent() {
    for body in [
        "if (!$v) { return isset($v['x']) ? $v['x'] : 0; } return 1;",
        "if (!$v) { return $v['x'] ?? 0; } return 1;",
        "if (!$v) { if (array_key_exists('x', $v)) { return $v['x']; } } return 1;",
        "if (!$v) { foreach ($v as $e) { return $e; } } return 1;",
        "if (!$v) { return count($v); } return 1;",
    ] {
        assert!(offset_ids("array{x?: int}", "array", body).is_empty(), "{body}");
    }
}

#[test]
fn a_write_onto_the_falsy_array_reads_the_exact_literal() {
    // The side binds the proven empty array, so a write extends it exactly and a
    // read of another key stays outside it (PHP warns on each).
    for (decl, body) in [
        ("array{x?: int}", "if (!$v) { $v['y'] = 1; return $v['x']; } return 1;"),
        ("array{x?: int}", "if (!$v) { $v['x'] = 1; return $v['y']; } return 1;"),
        ("array{x?: int}", "if (!$v) { $v[] = 1; return $v['x']; } return 1;"),
        ("array<string, int>", "if (!$v) { $v['y'] = 1; return $v['x']; } return 1;"),
    ] {
        assert_eq!(offset_ids(decl, "array", body), ["offset.undeclared"], "{body}");
    }
}

#[test]
fn a_guard_that_contradicts_the_falsy_side_reads_nothing_there() {
    // The inner region is dead in PHP: the empty array is not truthy, has no
    // entry and no key. Nothing in it may report.
    for body in [
        "if (!$v) { if ($v) { return $v['x']; } } return 1;",
        "if (!$v) { if (!empty($v)) { return $v['x']; } } return 1;",
        "if (!$v) { if (count($v) > 0) { return $v['x']; } } return 1;",
        "if (!$v) { if ($v !== []) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_key_exists('x', $v)) { return $v['x']; } } return 1;",
        "if (!$v) { if (isset($v['x'])) { return $v['x']; } } return 1;",
        "if (!$v) { if (in_array(1, $v, true)) { return $v['x']; } } return 1;",
        "if (!$v) { if (sizeof($v)) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_search(1, $v) !== false) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_key_first($v) !== null) { return $v['x']; } } return 1;",
        "if (!$v) { if (current($v) !== false) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_sum($v) > 0) { return $v['x']; } } return 1;",
        "if (!$v) { foreach ($v as $e) { return $v['x']; } } return 1;",
        "if (!$v) { $k = 'x'; if (array_key_exists($k, $v)) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_keys($v)) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_filter($v)) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_chunk($v, 1)) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_combine($v, $v)) { return $v['x']; } } return 1;",
        "if (!$v) { if (array_rand($v) !== null) { return $v['x']; } } return 1;",
    ] {
        assert!(offset_ids("array{x?: int}", "array", body).is_empty(), "{body}");
    }
}

#[test]
fn a_verified_empty_literal_keeps_its_proof_leg_verdict() {
    // Only an `Asserted` empty array reads as a sealed shape; `$row = []` is the
    // proof leg's, so the strict leg adds no second id.
    let src = "<?php\nfunction f(): ?int { $row = []; return $row['x']; }\n";
    let ids: Vec<_> = diagnostics(src).into_iter().map(|d| d.id).collect();
    assert_eq!(ids.iter().filter(|i| **i == "offset.missing").count(), 1, "{ids:?}");
    assert!(!ids.contains(&"offset.undeclared"), "{ids:?}");
}

#[test]
fn a_read_after_a_loop_over_the_falsy_array_still_reports() {
    // The loop body never runs, but the code after it does (PHP warns).
    assert_eq!(
        offset_ids(
            "array{x?: int}",
            "array",
            "if (!$v) { foreach ($v as $e) { echo $e; } return $v['x']; } return 1;"
        ),
        ["offset.undeclared"]
    );
}

#[test]
fn a_contradicted_falsy_side_joins_away() {
    // The dead inner region binds nothing the join can see: the outer read after
    // it is judged on the declared shape exactly, as without the inner guard.
    for inner in ["if ($v) { }", "if (in_array(1, $v, true)) { }", "foreach ($v as $e) { }"] {
        let body = format!("if (!$v) {{ {inner} }} return $v['y'];");
        assert_eq!(offset_ids("array{x?: int}", "array", &body), ["offset.undeclared"], "{inner}");
    }
}

#[test]
fn a_by_reference_call_in_a_helper_argument_is_no_test_of_the_array() {
    // `check(setx($v))` rebinds `$v`; the empty-array claim must not survive it.
    let src = "<?php\n/** @phpstan-assert true $c */\nfunction check(bool $c): void {}\n\
        function setx(mixed &$a): bool { $a = ['x' => 1]; return true; }\n\
        /** @param array{x?: int} $v */\n\
        function f(array $v): ?int { if (!$v) { check(setx($v)); return $v['x']; } return 1; }\n";
    let ids: Vec<_> = diagnostics(src).into_iter().map(|d| d.id).collect();
    assert!(!ids.iter().any(|i| i.starts_with("offset.")), "{ids:?}");
}

#[test]
fn a_comparison_that_cannot_discriminate_keeps_the_falsy_claim() {
    // `implode` never returns false, so the branch is live and the empty array holds.
    assert_eq!(
        offset_ids(
            "array{x?: int}",
            "array",
            "if (!$v) { if (implode(',', $v) !== false) { $v['y'] = 1; return $v['x']; } } return 1;"
        ),
        ["offset.undeclared"]
    );
}

#[test]
fn a_test_through_an_unmodelled_call_does_not_manufacture_non_emptiness() {
    // Neither side of `array_keys($v)` is known dead; the join after it and the
    // outer join must equal the declared shape, so the read still reports.
    for body in [
        "if (!$v) { if (json_encode($v) !== '') { } } return $v['x'];",
        "if (!$v) { if (array_keys($v)) { } $v['y'] = 1; return $v['x']; } return 1;",
    ] {
        assert!(!offset_ids("array{x?: int}", "array", body).is_empty(), "{body}");
    }
}

#[test]
fn a_read_in_a_region_an_unmodelled_truthy_call_decides_is_not_judged() {
    // `array_merge($v, [1])` is truthy on `[]`, so the code after its `return` is
    // dead in PHP; the call is not in the probe table, so the walk cannot tell.
    assert!(
        offset_ids(
            "array{x?: int}",
            "array",
            "if (!$v) { if (array_merge($v, [1])) { return 5; } return $v['y']; } return 1;"
        )
        .is_empty()
    );
}
