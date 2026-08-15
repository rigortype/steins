//! Issue #272 — the **count-comparison guard**: `count($x)` / `sizeof($x)` as a
//! comparison operand narrows the argument's `ShapeFact` on the branch the
//! comparison decides (ADR-0052's carrier design, note of 2026-08-09).
//!
//! What is pinned here beyond the spellings:
//!
//! * **Both polarities.** `count($x) > 0` proves a floor on the true arm and a
//!   ceiling on the false one; neither arm is a vacuity trap.
//! * **The sealed/unsealed split.** An unsealed shape can only gain a count
//!   bound; a sealed one whose declared key set the floor exhausts additionally
//!   pins every declared key present (the exact-count pin).
//! * **The refusals.** A mode argument, a shadowing project function, a
//!   non-`count` opaque operand and a `count($a) === count($b)` each decline —
//!   silently, narrowing nothing.
//! * **Zero emission.** As in the S4 suite, no fixture here may produce a
//!   non-debug finding: this slice adds narrowing vocabulary, not a check.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The S3/S4 mock sidecar: the reflected envelopes the ADR-0061 admission gate
/// consults for the `count` / `sizeof` transfers.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        let non_negative =
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false);
        facts.insert("count".to_owned(), non_negative.clone());
        facts.insert("sizeof".to_owned(), non_negative);
        Mock { facts }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
}

/// The single `debug.type` body a one-dump source produces, asserting on the
/// way that the source produced NO other finding.
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "count guards emitted a finding: {other:?}");
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

// The floor, both polarities

#[test]
fn a_positive_count_guard_floors_the_entry_count() {
    assert_eq!(
        guarded("array<int>", "count($v) > 0", "count($v)"),
        "dumped type: int<1, max> (asserted)"
    );
}

#[test]
fn the_else_arm_carries_the_complement() {
    // `count($v) <= 0` meets the domain's own floor at zero, so the complement
    // is the exact size — the else arm is a narrowing, not silence.
    assert_eq!(
        guarded_else("array<int>", "count($v) > 0", "count($v)"),
        "dumped type: 0 (asserted)"
    );
}

#[test]
fn a_yoda_comparison_reads_the_same_guard() {
    assert_eq!(
        guarded("array<int>", "0 < count($v)", "count($v)"),
        "dumped type: int<1, max> (asserted)"
    );
}

#[test]
fn sizeof_is_the_same_guard_as_count() {
    assert_eq!(
        guarded("array<int>", "sizeof($v) >= 3", "count($v)"),
        "dumped type: int<3, max> (asserted)"
    );
}

#[test]
fn an_inequality_against_zero_is_the_one_representable_complement() {
    // `!== 0` is the only point exclusion the interval domain can spell (zero
    // is the domain's own floor); `!== 3` excludes an interior point and narrows nothing.
    assert_eq!(
        guarded("array<int>", "count($v) !== 0", "count($v)"),
        "dumped type: int<1, max> (asserted)"
    );
    assert_eq!(
        guarded("array<int>", "count($v) !== 3", "count($v)"),
        "dumped type: int<0, max> (asserted)"
    );
}

// The ceiling, and the exact pin

#[test]
fn an_upper_comparison_bounds_the_count_from_above() {
    assert_eq!(
        guarded("array<int>", "count($v) < 3", "count($v)"),
        "dumped type: int<0, 2> (asserted)"
    );
}

#[test]
fn an_identity_comparison_pins_the_count() {
    assert_eq!(
        guarded("array<int>", "count($v) === 3", "count($v)"),
        "dumped type: 3 (asserted)"
    );
}

#[test]
fn a_bounded_variable_bounds_the_count_as_a_literal_does() {
    // The other operand need not be a literal: any binding the engine can bound
    // to an int interval works — the claim is the weakest one true over the whole interval.
    assert_eq!(
        one_type(
            "<?php\n/**\n * @param array<int> $v\n * @param int<3, 5> $n\n */\n\
             function f(array $v, int $n): void { if (count($v) === $n) { \\PHPStan\\dumpType(count($v)); } }\n"
        ),
        "dumped type: int<3, 5> (asserted)"
    );
}

#[test]
fn a_sealed_shape_pins_its_optional_keys_once_the_floor_exhausts_them() {
    // `array{0: string, 1?: string}` with >=2 entries has no room for key `1`
    // to be absent — the exact-count pin, on the *presence* lane rather than count.
    assert_eq!(
        guarded("array{0: string, 1?: string}", "count($v) > 1", "$v[1]"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn an_unsealed_shape_gains_a_floor_but_never_an_exact_count() {
    // Complement of the pin: an unsealed tail admits arbitrarily many
    // undeclared keys, so a floor stays a floor — no ceiling to close against.
    assert_eq!(
        guarded("array<string, string>", "count($v) > 1", "count($v)"),
        "dumped type: int<2, max> (asserted)"
    );
}

// The assert lane

#[test]
fn assert_routes_through_the_same_guard_path() {
    // `assert()` is a throw-guard whose argument lowers to a `CondExpr`
    // (ADR-0052's 2026-07-25 amendment) — no assert-specific plumbing needed.
    assert_eq!(
        one_type(&fixture(
            "array<int>",
            "assert(count($v) > 0); \\PHPStan\\dumpType(count($v));"
        )),
        "dumped type: int<1, max> (asserted)"
    );
}

#[test]
fn the_guard_distributes_through_a_conjunction() {
    assert_eq!(
        guarded("array<int>", "count($v) > 0 && count($v) < 4", "count($v)"),
        "dumped type: int<1, 3> (asserted)"
    );
}

#[test]
fn a_negated_guard_flips_polarity() {
    assert_eq!(
        guarded("array<int>", "!(count($v) > 0)", "count($v)"),
        "dumped type: 0 (asserted)"
    );
}

// The refusals

#[test]
fn the_mode_argument_declines() {
    // `count($x, COUNT_RECURSIVE)` counts nested entries — a different number,
    // so the guard is not this guard.
    assert_eq!(
        guarded("array<int>", "count($v, COUNT_RECURSIVE) > 0", "count($v)"),
        "dumped type: int<0, max> (asserted)"
    );
}

#[test]
fn a_shadowing_project_function_declines() {
    // A project `sizeof()` in the file's namespace is a different function —
    // `global_function_callee` (the rule every builtin recognizer opens with)
    // says so. The dump reads the *global* `count`: silence here is the
    // guard's decline, not a lost binding.
    let src = "<?php\nnamespace App;\n/** @param array<int> $a */\n\
               function sizeof(array $a): int { return 7; }\n\
               /** @param array<int> $v */\n\
               function f(array $v): void { if (sizeof($v) > 0) { \\PHPStan\\dumpType(\\count($v)); } }\n";
    assert_eq!(one_type(src), "dumped type: int<0, max> (asserted)");
}

#[test]
fn a_count_to_count_comparison_bounds_neither_side() {
    assert_eq!(
        one_type(
            "<?php\n/**\n * @param array<int> $v\n * @param array<int> $w\n */\n\
             function f(array $v, array $w): void { if (count($v) > count($w)) { \\PHPStan\\dumpType(count($v)); } }\n"
        ),
        "dumped type: int<0, max> (asserted)"
    );
}

#[test]
fn an_unbounded_operand_declines() {
    // The comparison is only as good as the bound: an `int` with no interval
    // narrows nothing, rather than being read as its domain ends.
    assert_eq!(
        one_type(
            "<?php\n/** @param array<int> $v */\n\
             function f(array $v, int $n): void { if (count($v) > $n) { \\PHPStan\\dumpType(count($v)); } }\n"
        ),
        "dumped type: int<0, max> (asserted)"
    );
}

#[test]
fn an_opaque_neighbour_still_falls_back_to_the_old_lowering() {
    // The ordering-comparison fallback (`CondExpr::Opaque`) is lifted only for
    // a `count()` operand: comparing a count against another opaque expression keeps it.
    assert_eq!(
        one_type(
            "<?php\n/** @param array<int> $v */\n\
             function f(array $v, object $o): void { if (count($v) > $o->n) { \\PHPStan\\dumpType(count($v)); } }\n"
        ),
        "dumped type: int<0, max>"
    );
}

// Invalidation

#[test]
fn an_offset_write_keeps_the_floor_and_drops_the_ceiling() {
    // A write can only add an entry, so "at most 2" doesn't survive it but
    // "at least 1" does (the write's own key supplies that floor either way).
    assert_eq!(
        one_type(&fixture(
            "array<int, int>",
            "if (count($v) < 3) { $v[9] = 1; \\PHPStan\\dumpType(count($v)); }"
        )),
        "dumped type: int<1, max> (asserted)"
    );
    assert_eq!(
        one_type(&fixture(
            "array<int, int>",
            "if (count($v) > 0) { $v[9] = 1; \\PHPStan\\dumpType(count($v)); }"
        )),
        "dumped type: int<1, max> (asserted)"
    );
}

#[test]
fn an_unset_drops_the_floor_and_keeps_the_ceiling() {
    assert_eq!(
        one_type(&fixture(
            "array<int, int>",
            "if (count($v) > 0) { unset($v[9]); \\PHPStan\\dumpType(count($v)); }"
        )),
        "dumped type: int<0, max> (asserted)"
    );
    assert_eq!(
        one_type(&fixture(
            "array<int, int>",
            "if (count($v) < 3) { unset($v[9]); \\PHPStan\\dumpType(count($v)); }"
        )),
        "dumped type: int<0, 2> (asserted)"
    );
}
// Value-lane coherence (the fp-gate FP class): the lowering lift keeps a count
// comparison as a real `Cmp` — before it, such a comparison lowered to
// `CondExpr::Opaque`, whose path dropped the guard call's read set, so a
// proven `[]` never survived into the arm. It survives now; if the count guard
// moved only the shape lane, the contract checker would convict on the stale
// literal — the two corpus false positives this section reproduces.
// `refuted_array_value` closes it.

/// A body calling a `non-empty-list<int>` contract, with its declaration.
fn contract_fixture(body: &str) -> String {
    format!(
        "<?php\n\
         /** @param non-empty-list<int> $ids */\n\
         function dao(array $ids): void {{}}\n\
         function f(): void {{ {body} }}\n"
    )
}

fn contract_findings(body: &str) -> Vec<String> {
    diagnostics(&contract_fixture(body))
        .into_iter()
        .filter(|d| !d.id.starts_with("debug."))
        .map(|d| format!("{}: {}", d.id, d.message))
        .collect()
}

#[test]
fn a_proven_empty_array_does_not_ride_into_the_arm_the_guard_refutes() {
    // Corpus shape (a): the true arm of `count($x) > 0`.
    assert_eq!(
        contract_findings("$ids = []; if (count($ids) > 0) { dao($ids); }"),
        Vec::<String>::new()
    );
    // Corpus shape (b): the fall-through of an early `count($x) < 1` return.
    assert_eq!(
        contract_findings("$ids = []; if (count($ids) < 1) { return; } dao($ids);"),
        Vec::<String>::new()
    );
}

#[test]
fn a_literal_that_can_genuinely_reach_the_call_still_convicts() {
    // The positive twin: no guard stands between literal and call, so the
    // violation is real — value-lane coherence narrows, it isn't blanket amnesty.
    let found = contract_findings("$ids = []; dao($ids);");
    assert_eq!(found.len(), 1, "expected the genuine violation, got {found:?}");
    assert!(found[0].starts_with("phpdoc.param-mismatch"), "{found:?}");
    // …and the arm a count guard does NOT refute keeps its literal too: `[]`
    // satisfies `count($x) <= 0`, so the else arm still convicts.
    let kept = contract_findings("$ids = []; if (count($ids) > 0) { return; } dao($ids);");
    assert_eq!(kept.len(), 1, "expected the surviving literal to convict, got {kept:?}");
}

#[test]
fn a_surviving_literal_keeps_its_proven_value() {
    // Inside the bound, the Singleton is sharper than any shape and is left
    // exactly as it was — `count($v)` still reads the literal's own size.
    assert_eq!(
        one_type(
            "<?php\nfunction f(): void { $v = [1, 2]; if (count($v) > 1) { \\PHPStan\\dumpType(count($v)); } }\n"
        ),
        "dumped type: 2"
    );
    // Outside it, the literal is refuted and what remains is the honest floor:
    // an array whose entry count the guard proved.
    assert_eq!(
        one_type(
            "<?php\nfunction f(): void { $v = [1, 2]; if (count($v) > 5) { \\PHPStan\\dumpType(count($v)); } }\n"
        ),
        "dumped type: int<6, max>"
    );
}

// `nullable` clearing (issue #289): `count(null)` raises a `TypeError` — it
// doesn't answer `false`, doesn't fall through, reaches neither branch. So
// reaching a branch at all already proves the subject wasn't null on entry.
// Mirrors `array_key_exists` (whose false arm answers `false` — real evidence)
// except a raised exception isn't an answer, so that analogy never carried.

#[test]
fn a_count_guard_clears_nullable_on_the_true_arm() {
    assert_eq!(
        guarded("array<int>|null", "count($v) > 0", "$v"),
        "dumped type: non-empty-array<int> (asserted)"
    );
}

#[test]
fn a_count_guard_clears_nullable_on_the_false_arm_too() {
    // The whole point: unlike a truthiness guard (whose false arm keeps doubt,
    // since null is falsy too), both arms here reach only after `count()` returns.
    assert_eq!(
        guarded_else("array<int>|null", "count($v) > 0", "$v"),
        "dumped type: array<int> (asserted)"
    );
}

#[test]
fn a_countable_receiver_clears_nullable_identically() {
    // The argument is about `count()`'s own null case, not arrays: `count()`
    // accepts any `Countable`, and a `Countable` arm survives a count
    // comparison just as an array arm does (no shape to discriminate) — but `Null` still dies.
    assert_eq!(
        one_type(
            "<?php\n/** @param \\Countable|null $v */\nfunction f(?\\Countable $v): void { if (count($v) > 0) { \\PHPStan\\dumpType($v); } }\n"
        ),
        "dumped type: Countable"
    );
}

#[test]
fn a_count_guard_on_an_unbound_subject_still_clears_nothing() {
    // The mode argument (`COUNT_RECURSIVE`) makes this a different guard
    // (issue #272's own refusal) — `count_guard` declines, so no
    // `ShapeGuard::Count` is built for the null-arm lift to reach; `nullable` stays as declared.
    assert_eq!(
        guarded("array<int>|null", "count($v, COUNT_RECURSIVE) > 0", "$v"),
        "dumped type: null|array<int> (asserted)"
    );
}

#[test]
fn a_catch_that_could_have_caught_the_guards_own_typeerror_proves_nothing() {
    // `try`/`catch` stays `StmtKind::Opaque` (ADR-0027): the trace doesn't
    // model its control flow, so the whole construct forgets what it reads
    // and writes on entry rather than letting a catch body inherit a
    // preceding guard's narrowing, and the catch body's statements aren't
    // walked at all. A `catch (\TypeError)` right after this guard could be
    // catching the guard's own `count($v)` failing on a null `$v`, so `$v`
    // must not read as proven non-null inside it — pinned here as a
    // regression (issue #289): a receiver call inside the catch, which
    // `call.on-null`/`call.on-non-object` would convict if `$v` were wrongly
    // cleared, produces no finding (the catch body isn't analyzed at all).
    let src = "<?php\nclass C { public function m(): void {} }\n\
               /** @param array<int>|null $v */\n\
               function f(array $v = null): void {\n\
               try {\n    if (count($v) > 0) {\n    }\n} catch (\\TypeError $e) {\n    $v->m();\n}\n\
               }\n";
    let ds: Vec<Diagnostic> =
        diagnostics(src).into_iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(ds.is_empty(), "the catch body must prove nothing about $v: {ds:?}");
}
