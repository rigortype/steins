//! ADR-0062 S7 — the two lanes of the positional projections, at fixture level.
//!
//! * **Order-witnessed lane** (§2): an env-resolved array argument reaches the
//!   sidecar fold like a written literal, so order-dependent builtins see the
//!   real insertion order — `$a = ['x', 'y']; count($a)` is `2`, closing §1's gap.
//! * **Order-declared lane** (§2/§4): a `Fact::Shape` base takes the sound
//!   widening and NEVER reads field declaration order — `array_key_first` of
//!   `array{a: int, b: int}` is `'a'|'b'`, the declined import of §7.
//!
//! Zero emission (A-G9) holds on every fixture here: a shape-derived fact never
//! premises a finding.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, ArrayKey, SourceTree};

/// A mock PHP: answers reflected declarations and *executes* the allowlisted
/// builtins the fold-seam tests use, proving arguments arrive as a concrete,
/// order-carrying array rather than as `$a`.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
    types: HashMap<String, String>,
    arities: HashMap<String, (u32, u32)>,
    absence: bool,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        facts.insert(
            "array_is_list".to_owned(),
            Fact::General { base: Base::Bool, nullable: false },
        );
        facts.insert("in_array".to_owned(), Fact::General { base: Base::Bool, nullable: false });
        facts
            .insert("implode".to_owned(), Fact::General { base: Base::String, nullable: false });
        // The reflected return-type declarations of the projection family (PHP
        // 8.5.8, `ReflectionFunction::getReturnType()`).
        let mut types = HashMap::new();
        for f in ["array_values", "array_keys", "array_flip", "array_reverse", "array_slice"] {
            types.insert(f.to_owned(), "array".to_owned());
        }
        for f in ["array_key_first", "array_key_last"] {
            types.insert(f.to_owned(), "string|int|null".to_owned());
        }
        types.insert("count".to_owned(), "int".to_owned());
        // `array_slice` reads its siblings POSITIONALLY (ADR-0064 Amendment B):
        // `array_slice(array $array, int $offset, ?int $length = null, bool
        // $preserve_keys = false)` — four parameters, two required, at PHP 8.5.8.
        let arities = HashMap::from([("array_slice".to_owned(), (4, 2))]);
        Mock { facts, types, arities, absence: true }
    }
}

/// The concrete entries of a fold argument, or `None` when the argument did not
/// arrive as an array literal at all.
fn entries(arg: &ArgValue) -> Option<&[(ArrayKey, ArgValue)]> {
    match arg {
        ArgValue::Array(items) => Some(items),
        _ => None,
    }
}

fn scalar_text(v: &ArgValue) -> Option<String> {
    match v {
        ArgValue::Str(s) => s.as_str().map(ToOwned::to_owned),
        ArgValue::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

impl Folder for Mock {
    /// A miniature PHP for the three array-taking allowlist entries: each reads
    /// the argument's **witnessed order**, what the fold seam must deliver.
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        match (name.to_ascii_lowercase().as_str(), args) {
            ("count", [a]) => Some(ArgValue::Int(i64::try_from(entries(a)?.len()).ok()?)),
            ("implode", [sep, a]) => {
                let sep = scalar_text(sep)?;
                let parts: Option<Vec<String>> =
                    entries(a)?.iter().map(|(_, v)| scalar_text(v)).collect();
                Some(ArgValue::Str(parts?.join(&sep).into()))
            }
            ("in_array", [needle, a]) => {
                let needle = scalar_text(needle)?;
                Some(ArgValue::Bool(
                    entries(a)?.iter().any(|(_, v)| scalar_text(v).as_deref() == Some(&needle)),
                ))
            }
            _ => None,
        }
    }
    fn absence_family_available(&mut self) -> bool {
        self.absence
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        self.arities.get(&name.to_ascii_lowercase()).copied()
    }
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    // Drop `untyped.*` (ADR-0078, #200): it flags the fixtures' own deliberately
    // untyped signatures, not the behavior under test.
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding.
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a projection emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A shape-declared fixture: `@param <decl> $v`, one dump of `<expr>`.
fn dump(decl: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

/// A value-lane fixture: statements, then one dump.
fn dump_body(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(): void {{ {body} }}\n"))
}

/// A shape-declared fixture with a second, natively-typed parameter — the way a
/// call gets an argument carrying no literal for a rule to read.
fn dump_two(decl: &str, extra: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\n\
         function f(array $v, {extra}): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

// The order-witnessed lane: the fold seam (§1's measured gap)

#[test]
fn a_bound_array_folds_exactly_like_a_written_literal() {
    // THE gap ADR-0062 §1 measured: both of these are `2`.
    assert_eq!(dump_body("\\PHPStan\\dumpType(count(['x', 'y']));"), "dumped type: 2");
    assert_eq!(dump_body("$a = ['x', 'y']; \\PHPStan\\dumpType(count($a));"), "dumped type: 2");
}

#[test]
fn an_unproven_binding_still_widens_to_the_envelope() {
    // An unresolved argument retains the reflected envelope.
    let src = "<?php\nfunction f(array $u): void { $a = $u; \\PHPStan\\dumpType(count($a)); }\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}

#[test]
fn a_partly_proven_array_literal_folds_through_its_binding() {
    // The element resolves through the env, so the WHOLE array becomes a fold
    // argument — the same rule, one level down.
    assert_eq!(
        dump_body("$x = 'b'; \\PHPStan\\dumpType(count(['a', $x, 'c']));"),
        "dumped type: 3"
    );
}

#[test]
fn the_witnessed_order_is_what_the_order_dependent_builtins_see() {
    // `implode` over a bound array uses observed insertion order, not canonical order.
    assert_eq!(
        dump_body("$a = ['b', 'a']; \\PHPStan\\dumpType(implode(',', $a));"),
        "dumped type: 'b,a'"
    );
    assert_eq!(
        dump_body("$a = ['x', 'y']; \\PHPStan\\dumpType(in_array('y', $a));"),
        "dumped type: true"
    );
}

#[test]
fn a_folded_binding_keeps_flowing() {
    assert_eq!(
        dump_body("$a = ['x', 'y']; $n = count($a); \\PHPStan\\dumpType($n);"),
        "dumped type: 2"
    );
}

#[test]
fn a_declared_shape_argument_is_not_a_fold_argument() {
    // The fold seam is the VALUE lane only: a declared shape has no witnessed
    // order, so `count` takes the §4 shape transfer (an interval), never a fold.
    assert_eq!(
        dump("array{a: int, b?: string}", "count($v)"),
        "dumped type: int<1, 2> (asserted)"
    );
}

// The order-declared lane: symbolic transfers (§4's row)

#[test]
fn array_values_of_a_shape_is_a_list_of_the_value_union() {
    // Heterogeneous values: `int|string` IS one fact as of issue #339, so the
    // element bound carries it where it used to be the unknown floor. The
    // list-ness and non-emptiness carry as they always did.
    assert_eq!(
        dump("array{a: int, b?: string}", "array_values($v)"),
        "dumped type: non-empty-list<int|string> (asserted)"
    );
    // Homogeneous values: the bound survives.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_values($v)"),
        "dumped type: non-empty-list<int> (asserted)"
    );
    assert_eq!(dump("array<string, int>", "array_values($v)"), "dumped type: list<int> (asserted)");
}

#[test]
fn array_keys_of_a_sealed_shape_enumerates_the_key_set() {
    assert_eq!(
        dump("array{a: int, b?: string}", "array_keys($v)"),
        "dumped type: non-empty-list<'a'|'b'> (asserted)"
    );
    assert_eq!(dump("array<string, int>", "array_keys($v)"), "dumped type: list<string> (asserted)");
    // `array-key` is one fact too (issue #339) — same widening escape as above.
    assert_eq!(dump("array", "array_keys($v)"), "dumped type: list<int|string> (asserted)");
}

#[test]
fn array_key_first_is_some_key_of_the_set_never_the_declared_first() {
    // **Negative test** (§2, §7 declined import 1): PHPStan answers `'a'` here
    // and is wrong on `['b' => 2, 'a' => 1]`, which the shape admits just as well.
    assert_eq!(dump("array{a: int, b: int}", "array_key_first($v)"), "dumped type: 'a'|'b' (asserted)");
    assert_eq!(dump("array{a: int, b: int}", "array_key_last($v)"), "dumped type: 'a'|'b' (asserted)");
    // A possibly-empty shape adds `null` — PHP's own answer for `[]`.
    assert_eq!(
        dump("array{a?: int, b?: int}", "array_key_first($v)"),
        "dumped type: 'a'|'b'|null (asserted)"
    );
    assert_eq!(
        dump("array<string, int>", "array_key_first($v)"),
        "dumped type: string|null (asserted)"
    );
}

#[test]
fn array_reverse_and_array_flip_take_their_stated_widenings() {
    // A required string key survives the reversal, so the result is never a list;
    // the entry count is preserved, so it stays non-empty.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_reverse($v)"),
        "dumped type: non-empty-associative-array<int> (asserted)"
    );
    // All-int keys are renumbered — the result IS a list.
    assert_eq!(dump("list<string>", "array_reverse($v)"), "dumped type: list<string> (asserted)");
    // `array_flip` drops non-emptiness (a non-`int|string` value is skipped) and
    // claims `array-key` unless every value is an `int`.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_flip($v)"),
        "dumped type: array<int, 'a'|'b'> (asserted)"
    );
}

// The SEQUENCE lane (issue #165): isList == Yes is realizable order, so
// order-dependent projections consume it as a semantic guarantee, not the
// declaration artifact PHPStan's divergence doc flags as its real-FP class.

#[test]
fn a_proven_sequence_projects_exactly() {
    // Issue #165's measured table, each row probed at PHP 8.5.9.
    // `array_values(["x", 1]) === ["x", 1]` — identity on a list, element
    // types intact where the set widening dropped them to `mixed`.
    assert_eq!(
        dump("list{string, int}", "array_values($v)"),
        "dumped type: list{string, int} (asserted)"
    );
    // `array_keys(["x", 1, 2.5]) === [0, 1, 2]` — the literal key sequence.
    assert_eq!(
        dump("list{string, int}", "array_keys($v)"),
        "dumped type: list{0, 1} (asserted)"
    );
    // `array_reverse(["a", "b"]) === ["b", "a"]` (probed at lengths 1, 2, 3)
    // — the reversed sequence.
    assert_eq!(
        dump("list{string, int}", "array_reverse($v)"),
        "dumped type: list{int, string} (asserted)"
    );
}

#[test]
fn a_trailing_optional_sequence_keeps_keys_and_declines_reverse() {
    // `list{int, 1?: string}` realizes as `[A]` or `[A, B]`. Probed:
    // `array_keys` answers `[0]` / `[0, 1]` — exactly `list{0, 1?: 1}`,
    // spelled with its keys because an optional key is never positional.
    assert_eq!(
        dump("list{int, 1?: string}", "array_keys($v)"),
        "dumped type: list{0: 0, 1?: 1} (asserted)"
    );
    // `array_values` stays the identity — optionality preserved.
    assert_eq!(
        dump("list{int, 1?: string}", "array_values($v)"),
        "dumped type: list{0: int, 1?: string} (asserted)"
    );
    // Reversal of a variable-length sequence smears every position (probed:
    // `"a"` is index 0 in `array_reverse(["a"])` but index 1 in
    // `array_reverse(["a", "b"])`), so `array_reverse` keeps today's widening.
    assert_eq!(
        dump("list{int, 1?: string}", "array_reverse($v)"),
        "dumped type: non-empty-list<int|string> (asserted)"
    );
}

#[test]
fn an_unsealed_list_keeps_identity_and_gains_exact_keys() {
    // `array_values` identity on the unsealed forms, `non-empty-` included.
    assert_eq!(dump("list<string>", "array_values($v)"), "dumped type: list<string> (asserted)");
    assert_eq!(
        dump("non-empty-list<string>", "array_values($v)"),
        "dumped type: non-empty-list<string> (asserted)"
    );
    // A list's keys are `0..n-1` — never negative, so the element bound
    // sharpens past the bare `int` class.
    assert_eq!(
        dump("list<int>", "array_keys($v)"),
        "dumped type: list<int<0, max>> (asserted)"
    );
    // `array_reverse` on `non-empty-list<T>`: element identity, cardinality
    // (and so `non-empty-`) preserved.
    assert_eq!(
        dump("non-empty-list<int>", "array_reverse($v)"),
        "dumped type: non-empty-list<int> (asserted)"
    );
}

#[test]
fn a_set_subject_keeps_todays_widenings() {
    // The issue's own pinned answer: `array{a: 1, b: 2}` is a key SET —
    // `['b' => 2, 'a' => 1]` is admitted just as well — so `array_values`
    // still answers the value union as a list, never an order (probed:
    // `array_values(["a" => 1, "b" => 2]) === [1, 2]` is one realization
    // among the permutations the shape admits).
    assert_eq!(
        dump("array{a: 1, b: 2}", "array_values($v)"),
        "dumped type: non-empty-list<1|2> (asserted)"
    );
}

#[test]
fn the_declined_projections_say_nothing() {
    // `array_search`'s value side is the family's one remaining v1 decline: honest
    // silence via ADR-0069's Asserted floor (the `(asserted)` marker), not a wrong
    // widening. Multi-base union #73 dropped, #79 admitted — untouched here.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_search(1, $v)"),
        "dumped type: int|string|false (asserted)"
    );
}

// array_slice: the grown argument channel (issue #118, ADR-0062 Amendment B)

#[test]
fn array_slice_keeps_the_element_type_and_the_list_ness() {
    // THE claim that retires the v1 "carries no more than the reflected `array`
    // envelope" argument: the element type survives, and the envelope says `array`.
    assert_eq!(dump("list<string>", "array_slice($v, 1)"), "dumped type: list<string> (asserted)");
    // A sealed shape's values union into the element bound, and its all-int keys are
    // renumbered `0..` — so the result is a list even though the subject is not.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_slice($v, 1)"),
        "dumped type: array<string, int> (asserted)"
    );
    assert_eq!(
        dump("array{0: int, 1: string}", "array_slice($v, 1)"),
        "dumped type: list<int|string> (asserted)"
    );
    // The optional `$length` changes nothing about the claim: every part of the
    // widening is sound for ANY offset and length.
    assert_eq!(
        dump("list<string>", "array_slice($v, 1, 2)"),
        "dumped type: list<string> (asserted)"
    );
}

#[test]
fn array_slice_never_keeps_non_emptiness() {
    // A non-empty subject can slice to nothing (`array_slice([1,2,3],10)===[]`,
    // `array_slice([1,2,3],1,0)===[]`), so non-emptiness drops unconditionally.
    assert_eq!(
        dump("non-empty-list<int>", "array_slice($v, 1)"),
        "dumped type: list<int> (asserted)"
    );
    assert_eq!(
        dump("array{a: int}", "array_slice($v, 0)"),
        "dumped type: array<string, int> (asserted)"
    );
}

#[test]
fn preserve_keys_degrades_list_ness_honestly() {
    // A literal `true` keeps every surviving key as it was, so the renumbering that
    // makes the result a list does not happen (`array_slice([1,2,3], 1, null, true)
    // === [1 => 2, 2 => 3]`).
    assert_eq!(
        dump("list<string>", "array_slice($v, 1, null, true)"),
        "dumped type: array<int, string> (asserted)"
    );
    // An explicit literal `false` is the default, and keeps the list-ness.
    assert_eq!(
        dump("list<string>", "array_slice($v, 1, null, false)"),
        "dumped type: list<string> (asserted)"
    );
    // An UNKNOWN flag degrades exactly as a truthy one does — the rule joins the two
    // branches rather than guessing which was passed.
    assert_eq!(
        dump_two("list<string>", "bool $keep", "array_slice($v, 1, null, $keep)"),
        "dumped type: array<int, string> (asserted)"
    );
}

// issue #137: the two precision claims on the widening floor

#[test]
fn a_proven_list_sliced_from_offset_zero_keeps_list_ness_under_preserve_keys() {
    // Claim 1: a proven `is_list` subject, offset `0`, flag `true` keep `0..k-1`
    // unchanged for any length sign — still a list
    // (`array_slice([1,2,3], 0, null, true) === [1,2,3]`,
    // `array_slice([1,2,3], 0, -1, true) === [0 => 1, 1 => 2]`).
    assert_eq!(
        dump("list<string>", "array_slice($v, 0, null, true)"),
        "dumped type: list<string> (asserted)"
    );
    assert_eq!(
        dump("list<string>", "array_slice($v, 0, -1, true)"),
        "dumped type: list<string> (asserted)"
    );
    // A witnessed subject with an UNPROVEN length falls off the exact rung to the
    // widening, where the LIFT still proves its list-ness — so the claim fires on
    // the value lane too.
    assert_eq!(
        one_type(
            "<?php\nfunction f(int $n): void { $a = [1, 2, 3]; \\PHPStan\\dumpType(array_slice($a, 0, $n, true)); }\n"
        ),
        "dumped type: list<1|2|3>"
    );
}

#[test]
fn all_int_keys_alone_do_not_earn_the_preserved_prefix_claim() {
    // **Negative test** (issue #137, claim 1's counterexample): all-int keys don't
    // prove `is_list` (`array_slice([5 => 2], 0, null, true) === [5 => 2]`, not a list).
    assert_eq!(
        dump("array{5: int}", "array_slice($v, 0, null, true)"),
        "dumped type: array<int, int> (asserted)"
    );
    // The exact rung agrees end to end: the witnessed probe keeps its key.
    assert_eq!(
        dump_body("$a = [5 => 2]; \\PHPStan\\dumpType(array_slice($a, 0, null, true));"),
        "dumped type: array{5: 2}"
    );
}

#[test]
fn the_preserved_prefix_claim_declines_without_its_premises() {
    // An UNKNOWN offset keeps today's floor exactly…
    assert_eq!(
        dump_two("list<string>", "int $n", "array_slice($v, $n, null, true)"),
        "dumped type: array<int, string> (asserted)"
    );
    // …as does a proven but NONZERO offset (`array_slice([1,2,3], 1, null, true)
    // === [1 => 2, 2 => 3]`, not a list)…
    assert_eq!(
        dump("list<string>", "array_slice($v, 1, null, true)"),
        "dumped type: array<int, string> (asserted)"
    );
    // …and an UNKNOWN flag, which joins both branches rather than guessing.
    assert_eq!(
        dump_two("list<string>", "bool $keep", "array_slice($v, 0, null, $keep)"),
        "dumped type: array<int, string> (asserted)"
    );
}

#[test]
fn a_proven_zero_length_slice_is_the_empty_array() {
    // Claim 2: a proven `$length = 0` empties the window for ANY subject, offset,
    // and flag (`array_slice(['a' => 1], 0, 0) === []`,
    // `array_slice([1,2,3], -2, 0, true) === []`), so the answer is the sealed
    // empty `array{}`, not the unsealed floor.
    assert_eq!(dump("array{a: int}", "array_slice($v, 0, 0)"), "dumped type: array{} (asserted)");
    assert_eq!(
        dump("list<int>", "array_slice($v, -2, 0, true)"),
        "dumped type: array{} (asserted)"
    );
}

#[test]
fn the_zero_length_claim_wins_over_the_preserved_prefix_claim() {
    // Both claims apply (`array_slice([1,2,3], 0, 0, true) === []`), and the
    // sealed `array{}` is the sharper fact — itself a list, so nothing is lost.
    assert_eq!(
        dump("list<int>", "array_slice($v, 0, 0, true)"),
        "dumped type: array{} (asserted)"
    );
}

#[test]
fn the_zero_length_claim_declines_without_a_proven_zero() {
    // An UNKNOWN length keeps today's floor…
    assert_eq!(
        dump_two("array{a: int}", "int $n", "array_slice($v, 0, $n)"),
        "dumped type: array<string, int> (asserted)"
    );
    // …and so does a literal `null` — the documented "to the end" spelling, which
    // is NOT `0` (`array_slice(['a' => 1], 0, null) === ['a' => 1]`).
    assert_eq!(
        dump("array{a: int}", "array_slice($v, 0, null)"),
        "dumped type: array<string, int> (asserted)"
    );
}

#[test]
fn array_slice_of_a_witnessed_array_projects_exactly() {
    // **The §2 boundary at its sharpest**: an order-witnessed subject with Singleton
    // offset/length EXECUTES the projection rather than widening it.
    assert_eq!(
        dump_body("$a = ['x', 'y', 'z']; \\PHPStan\\dumpType(array_slice($a, 1));"),
        "dumped type: list{'y', 'z'}"
    );
    // Negative offsets and lengths take php-src's own clamping
    // (`array_slice([1,2,3,4,5], 1, -1) === [2,3,4]`).
    assert_eq!(
        dump_body("$a = [1, 2, 3, 4, 5]; \\PHPStan\\dumpType(array_slice($a, 1, -1));"),
        "dumped type: list{2, 3, 4}"
    );
    assert_eq!(
        dump_body("$a = [1, 2, 3]; \\PHPStan\\dumpType(array_slice($a, 10));"),
        "dumped type: array{}"
    );
    // `$preserve_keys = true` keeps the keys; the default renumbers integers and
    // leaves strings alone.
    assert_eq!(
        dump_body("$a = ['a' => 1, 5 => 2, 'b' => 3]; \\PHPStan\\dumpType(array_slice($a, 1));"),
        "dumped type: array{0: 2, b: 3}"
    );
    assert_eq!(
        dump_body(
            "$a = ['a' => 1, 5 => 2, 'b' => 3]; \\PHPStan\\dumpType(array_slice($a, 1, null, true));"
        ),
        "dumped type: array{5: 2, b: 3}"
    );
}

#[test]
fn a_witnessed_subject_with_an_unproven_offset_falls_to_the_widening() {
    // The exact rung's premises are not met, so the LIFT of the same entries takes
    // the widening — a value-lane subject is never worse off than a declared one.
    assert_eq!(
        one_type(
            "<?php\nfunction f(int $n): void { $a = ['x', 'y']; \\PHPStan\\dumpType(array_slice($a, $n)); }\n"
        ),
        "dumped type: list<'x'|'y'>"
    );
}

#[test]
fn the_contract_lane_never_projects_positionally() {
    // **Negative test** (§2, §7 declined import 1): a declared `array{a: int, b:
    // string}` is a key SET (`['b' => 's', 'a' => 1]` admitted too), so the grown
    // seam still cannot spell field order — only a flag and an offset.
    assert_eq!(
        dump("array{a: int, b: string}", "array_slice($v, 1, 1)"),
        "dumped type: array<string, int|string> (asserted)"
    );
    assert_eq!(
        dump("array{a: int, b: string}", "array_slice($v, 1, 1, true)"),
        "dumped type: array<string, int|string> (asserted)"
    );
}

#[test]
fn array_slice_declines_outside_its_measured_signature() {
    // PHP itself rejects a missing-argument call (`ArgumentCountError`), and a 5th
    // argument is outside the pinned `(4, 2)` signature — both decline.
    assert_eq!(dump("list<string>", "array_slice($v)"), "dumped type: array (asserted)");
    assert_eq!(
        dump("list<string>", "array_slice($v, 1, 2, true, 5)"),
        "dumped type: array (asserted)"
    );
}

#[test]
fn an_engine_that_answers_no_arity_withholds_the_slice() {
    // ADR-0064 Amendment B's second leg: a runner that cannot state `array_slice`'s
    // signature withholds the rule, same as one silent on the declaration.
    let mut mock = Mock::sidecar();
    mock.arities.clear();
    let src = "<?php\n/** @param list<string> $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_slice($v, 1)); }\n";
    let tree = SourceTree::parse(src);
    let ds = check_with(&tree, &[], "t.php", &mut mock);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: array (asserted)");
}

#[test]
fn a_second_argument_or_a_nullable_base_declines() {
    // The seam is single-argument; a nullable base may be `null` (a TypeError, not
    // a projection) — both withhold the PROJECTION, leaving the argument-blind floor.
    assert_eq!(dump("array{a: int}", "array_reverse($v, true)"), "dumped type: array (asserted)");
    let src = "<?php\n/** @param array{a: int}|null $v */\n\
               function f(?array $v): void { \\PHPStan\\dumpType(array_values($v)); }\n";
    assert_eq!(one_type(src), "dumped type: list<mixed> (asserted)");
}

#[test]
fn a_project_function_shadowing_the_name_declines() {
    // The same shadow rule the fold and the envelope carry: a user `array_values`
    // is not the builtin.
    let src = "<?php\n/** @param array{a: int} $v */\n\
               function array_values(array $x): array { return $x; }\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_values($v)); }\n";
    let ds = diagnostics(src);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: unknown");
}

#[test]
fn without_the_reflected_declaration_the_rule_is_withheld() {
    // ADR-0061 §2 admission gate: no live PHP (or a monkey-patched extension) means
    // the declaration is unavailable, so the transfer is withheld — the catalog's
    // `list<mixed>` floor stands in for the withheld element-type transfer.
    struct NoPhp;
    impl Folder for NoPhp {
        fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
            None
        }
    }
    let src = "<?php\n/** @param array{a: int} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_values($v)); }\n";
    let tree = SourceTree::parse(src);
    let ds = check_with(&tree, &[], "t.php", &mut NoPhp);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: list<mixed> (asserted)");
}
