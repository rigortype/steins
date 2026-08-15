//! ADR-0064 DR3 — the argument-DISPATCHED symbolic transfers (seam ii), at
//! fixture level.
//!
//! Rules whose answer depends on an argument the S3/S7 rung cannot bind:
//! `explode` separator, `range` bounds, `preg_replace` subject, `var_export`
//! literal flag, `min`/`max` argument list. Each asserted both refined and
//! declined — decline is first-class (ADR-0061 §1).
//!
//! `json_decode` only declines (bare `mixed`, six-base per-flag envelope, no
//! single `Fact`). `min`/`max` (issue #118) declare the same `mixed` but ARE
//! admitted via the ADR-0064 Amendment B arity second leg, which
//! `json_decode`'s envelope still fails.
//!
//! Zero emission is asserted on every fixture (as `shape_projections.rs`).

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// Mock reflected declarations (`ReflectionFunction::getReturnType()` at 8.5.8)
/// the DR3 admission gate consults.
#[derive(Default)]
struct Mock {
    types: HashMap<String, String>,
    facts: HashMap<String, Fact>,
    arities: HashMap<String, (u32, u32)>,
    absence: bool,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut types = HashMap::new();
        for (f, t) in [
            ("explode", "array"),
            ("range", "array"),
            ("preg_replace", "array|string|null"),
            ("var_export", "?string"),
            ("json_decode", "mixed"),
            ("min", "mixed"),
            ("max", "mixed"),
        ] {
            types.insert(f.to_owned(), t.to_owned());
        }
        // `min(mixed $value, mixed ...$values)` at 8.5.8: variadic, 2 declared /
        // 1 required, via `ReflectionFunction::getNumberOfParameters()`.
        let arities = HashMap::from([("min".to_owned(), (2, 1)), ("max".to_owned(), (2, 1))]);
        // `var_export`'s `?string` envelope sits one rung below the transfer, so
        // the null-strip reads as a refinement, not an answer from nowhere.
        let mut facts = HashMap::new();
        facts.insert(
            "var_export".to_owned(),
            Fact::General { base: Base::String, nullable: true },
        );
        Mock { types, facts, arities, absence: true }
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
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        self.arities.get(&name.to_ascii_lowercase()).copied()
    }
}

fn diagnostics_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding.
fn one_type_with(src: &str, folder: &mut dyn Folder) -> String {
    let ds = diagnostics_with(src, folder);
    // `untyped.*` is contract-layer claim-absence (issue #200), orthogonal here.
    let other: Vec<&Diagnostic> =
        ds.iter().filter(|d| !d.id.starts_with("debug.") && !d.id.starts_with("untyped.")).collect();
    assert!(other.is_empty(), "a transfer emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

fn one_type(src: &str) -> String {
    one_type_with(src, &mut Mock::sidecar())
}

/// A fixture with a declared signature and one dump in the body.
fn dump(sig: &str, expr: &str) -> String {
    one_type(&format!("<?php\nfunction f({sig}): void {{ \\PHPStan\\dumpType({expr}); }}\n"))
}

/// A fixture whose parameter carries a phpdoc declaration (an `Asserted` premise).
fn dump_doc(doc: &str, sig: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** {doc} */\nfunction f({sig}): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

// explode: non-empty separator ⇒ non-empty-list<string>

#[test]
fn explode_on_a_literal_separator_is_a_non_empty_list_of_strings() {
    // PHP 8 drops the `false` arm; splitting on a non-empty separator always
    // yields at least one piece (`explode(',', '') === ['']`).
    assert_eq!(
        dump("string $s", "explode(',', $s)"),
        "dumped type: non-empty-list<string>"
    );
    assert_eq!(
        dump("string $s", "explode('::', $s)"),
        "dumped type: non-empty-list<string>"
    );
}

#[test]
fn explode_takes_the_separators_own_predicate_not_just_a_literal() {
    // Separator need not be literal: `non-empty-string` is the premise the rule
    // needs; `Refine::Truthy` adds `NON_FALSY`, closing over `NON_EMPTY`.
    let src = "<?php\nfunction f(string $sep, string $s): void {\n\
               if ($sep) { \\PHPStan\\dumpType(explode($sep, $s)); }\n}\n";
    assert_eq!(one_type(src), "dumped type: non-empty-list<string>");
}

#[test]
fn explode_declines_an_empty_or_unknown_separator() {
    // `explode('', 'abc')` throws `ValueError` at 8.5.8 — no return to describe.
    // The floor answer here is the ADR-0069 FLOOR (ADR-0071's `explode` row):
    // coarse `list<string> (asserted)`. The marker proves the decline held —
    // the rule's own answer (`non-empty-list<string>`) carries none.
    assert_eq!(dump("string $s", "explode('', $s)"), "dumped type: list<string> (asserted)");
    assert_eq!(
        dump("string $sep, string $s", "explode($sep, $s)"),
        "dumped type: list<string> (asserted)"
    );
}

#[test]
fn explode_declines_the_limit_form_because_a_limit_can_empty_the_result() {
    // Load-bearing decline: `explode(',', 'a,b,c', -5)` returns `[]` at 8.5.8,
    // so carrying `non-empty` across a limit arg would be a false premise. The
    // floor's `list<string>` stands instead, correct on the empty case.
    assert_eq!(dump("string $s", "explode(',', $s, 2)"), "dumped type: list<string> (asserted)");
    assert_eq!(dump("string $s", "explode(',', $s, -5)"), "dumped type: list<string> (asserted)");
}

// range: always a non-empty list; integral arguments sharpen the element

#[test]
fn range_of_integral_bounds_is_a_non_empty_list_of_ints() {
    assert_eq!(dump("", "range(1, 3)"), "dumped type: non-empty-list<int>");
    // Equal (`range(1,1)===[1]`) and descending bounds still produce a
    // non-empty list; an integral step keeps the element bound; non-literal
    // bounds work the same since the rule reads facts, not text.
    assert_eq!(dump("", "range(5, 5)"), "dumped type: non-empty-list<int>");
    assert_eq!(dump("", "range(3, 1)"), "dumped type: non-empty-list<int>");
    assert_eq!(dump("", "range(1, 9, 2)"), "dumped type: non-empty-list<int>");
    assert_eq!(dump("int $a, int $b", "range($a, $b)"), "dumped type: non-empty-list<int>");
}

#[test]
fn range_keeps_the_list_and_the_non_emptiness_when_the_element_is_unknown() {
    // Fractional step (float array) or string bounds (`range('a','c')===
    // ['a','b','c']`) drop the element bound, but never emptiness or list-ness.
    assert_eq!(dump("", "range(1, 2, 0.5)"), "dumped type: non-empty-list<mixed>");
    assert_eq!(dump("", "range('a', 'c')"), "dumped type: non-empty-list<mixed>");
    assert_eq!(dump("float $a, float $b", "range($a, $b)"), "dumped type: non-empty-list<mixed>");
}

#[test]
fn range_declines_an_arity_php_itself_rejects() {
    // 1 or 4 args is `ArgumentCountError` — PHP won't make the call, so the
    // seam declines. ADR-0069 §2's floor is arity-blind, answering bare
    // `array` (asserted) instead of the withheld `non-empty-list<int>`.
    assert_eq!(dump("", "range(1)"), "dumped type: array (asserted)");
    assert_eq!(dump("", "range(1, 2, 3, 4)"), "dumped type: array (asserted)");
}

// preg_replace: the subject's base splits the multi-base declaration

#[test]
fn preg_replace_of_a_string_subject_is_string_or_null() {
    // Reflected `array|string|null` is multi-base and seeds no fact; the
    // subject's own base splits it — `$pattern` shape and trailing $limit/
    // $count don't change the answer, only `$subject` governs.
    assert_eq!(
        dump("string $s", "preg_replace('/a/', 'b', $s)"),
        "dumped type: string|null"
    );
    assert_eq!(
        dump("string $s", "preg_replace(['/a/', '/b/'], 'z', $s)"),
        "dumped type: string|null"
    );
    assert_eq!(
        dump("string $s", "preg_replace('/a/', 'b', $s, 1)"),
        "dumped type: string|null"
    );
}

#[test]
fn preg_replace_of_an_array_subject_is_array_or_null() {
    assert_eq!(
        dump_doc("@param array{a: string} $v", "array $v", "preg_replace('/a/', 'b', $v)"),
        "dumped type: array|null (asserted)"
    );
}

#[test]
fn preg_replace_declines_a_subject_it_cannot_place() {
    // Unknown or nullable subject base declines the split: the floor's
    // unsplit `string|null|array (asserted)` states both arms instead of
    // choosing one.
    assert_eq!(
        // `$u`: bound but fact-less, i.e. the unknown base this declines on.
        dump("$u", "preg_replace('/a/', 'b', $u)"),
        "dumped type: string|null|array (asserted)"
    );
    assert_eq!(
        dump("?string $s", "preg_replace('/a/', 'b', $s)"),
        "dumped type: string|null|array (asserted)"
    );
}

// var_export: the literal `true` flag strips the envelope's null arm

#[test]
fn var_export_with_a_literal_true_flag_is_a_string() {
    // `$value` doesn't matter: `var_export(null, true) === 'NULL'` (a string).
    assert_eq!(dump("int $v", "var_export($v, true)"), "dumped type: string");
    assert_eq!(dump("", "var_export(null, true)"), "dumped type: string");
}

#[test]
fn var_export_without_the_flag_falls_back_to_its_own_envelope() {
    // One-arg / literal-`false` forms decline to the reflected `?string` envelope.
    assert_eq!(dump("int $v", "var_export($v)"), "dumped type: string|null");
    assert_eq!(dump("int $v", "var_export($v, false)"), "dumped type: string|null");
    assert_eq!(dump("int $v, bool $b", "var_export($v, $b)"), "dumped type: string|null");
}

// min / max: the argument-fact union, and the interval that sharpens it

#[test]
fn min_and_max_compose_the_intervals_of_int_arguments() {
    // `min(a,b) ∈ [min(lo),min(hi)]`, `max` dually — interval arithmetic,
    // either argument order.
    assert_eq!(
        dump_doc("@param int<0, max> $r", "int $r", "min($r, 100)"),
        "dumped type: int<0, 100> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<0, max> $r", "int $r", "min(100, $r)"),
        "dumped type: int<0, 100> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<min, 100> $r", "int $r", "max($r, 0)"),
        "dumped type: int<0, 100> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<1, 6> $r", "int $r", "min($r, 4)"),
        "dumped type: int<1, 4> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<1, 6> $r", "int $r", "max(4, $r)"),
        "dumped type: int<4, 6> (asserted)"
    );
    // A bare `int` param is the full interval; composition still bounds one side.
    assert_eq!(dump("int $i", "min($i, 5)"), "dumped type: int<min, 5>");
    assert_eq!(dump("int $i", "max($i, 5)"), "dumped type: int<5, max>");
    // 3 args compose left-to-right; a collapse to a point spells the point —
    // `min`/`max` aren't on the folding allowlist, so nothing else would answer.
    assert_eq!(dump("int $i", "min(3, 1, 2)"), "dumped type: 1");
    assert_eq!(dump("int $i", "max(3, 1, 2)"), "dumped type: 3");
}

#[test]
fn the_union_is_the_answer_where_the_interval_is_not() {
    // Load-bearing: min/max return one of their ARGUMENTS, so the union of
    // argument facts is sound with no comparison-semantics premise. Witnessed
    // at 8.5.8: `min('a', 1) === 1`, the 2nd argument verbatim.
    assert_eq!(dump_doc("@param 'a'|'b' $s", "string $s", "min($s, 'c')"), "dumped type: 'a'|'b'|'c' (asserted)");
    // Two facts of the same base with no interval between them join in the domain.
    assert_eq!(dump("string $s", "max($s, 'c')"), "dumped type: string");
}

#[test]
fn the_unary_array_form_answers_from_the_shape() {
    // 1-arg ARRAY form: result is one of the array's elements, so the shape's
    // value union is the claim. `min([])` throws — no `non_empty` premise needed.
    assert_eq!(
        dump_doc("@param array{a: int, b: int} $v", "array $v", "max($v)"),
        "dumped type: int (asserted)"
    );
    assert_eq!(
        dump_doc("@param list<string> $v", "array $v", "min($v)"),
        "dumped type: string (asserted)"
    );
    // A witnessed array lifts first; which element wins is declined, union claims.
    assert_eq!(
        one_type("<?php\nfunction f(): void { \\PHPStan\\dumpType(min([1, 2, 3])); }\n"),
        "dumped type: 1|2|3"
    );
}

#[test]
fn min_and_max_decline_where_they_cannot_state_an_answer() {
    // An arg with no usable fact declines the whole rule — the missing one
    // could hold the winner. `$u` carries nothing.
    let src = "<?php\nfunction f(int $i): void { $u = frobnicate(); \\PHPStan\\dumpType(min($i, $u)); }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
    // No longer declines (issue #339): `min($int, $string)` returns one of its
    // args, so `int|string` is the answer. Previously declined for lack of a
    // two-base form (ADR-0062 Amendment B deviation); `Fact::Union` discharges it.
    assert_eq!(dump("int $i, string $s", "min($i, $s)"), "dumped type: int|string");
    // Nullable int leaves the INTERVAL path (`min(null,5) === NULL` at 8.5.8)
    // for the union, which carries the null side correctly.
    assert_eq!(dump("?int $i", "min($i, 5)"), "dumped type: int|null");
    // 1-arg call whose fact isn't an array declines — `min(5)` is a `TypeError`.
    assert_eq!(dump("int $i", "min($i)"), "dumped type: unknown");
    assert_eq!(dump("int $i", "min()"), "dumped type: unknown");
}

#[test]
fn an_engine_that_answers_no_arity_withholds_min_and_max() {
    // ADR-0064 Amendment B: bare `mixed` pins nothing, so the arity signature
    // is what countersigns min/max. A runner unable to state it withholds it.
    let mut mock = Mock::sidecar();
    mock.arities.clear();
    let src = "<?php\nfunction f(int $i): void { \\PHPStan\\dumpType(min($i, 5)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
    // A signature that has MOVED withholds it too — the rule would be stale.
    let mut mock = Mock::sidecar();
    mock.arities.insert("min".to_owned(), (3, 2));
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
}

// json_decode: the batch's measured decline

#[test]
fn json_decode_declines_in_every_form() {
    // Bare `mixed` declaration; `$assoc=true` form admits 6 bases
    // (`array|int|float|string|bool|null`), no single `Fact` — declines
    // rather than guess an arm.
    assert_eq!(dump("string $s", "json_decode($s)"), "dumped type: unknown");
    assert_eq!(dump("string $s", "json_decode($s, true)"), "dumped type: unknown");
    assert_eq!(dump("string $s", "json_decode($s, false)"), "dumped type: unknown");
}

// The admission gate (ADR-0061 §2), on the new rules

#[test]
fn without_the_reflected_declaration_every_transfer_is_withheld() {
    struct NoPhp;
    impl Folder for NoPhp {
        fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
            None
        }
    }
    // These used to read `unknown`; now ADR-0069's declared-return FLOOR
    // answers underneath with the catalog declaration, marked `(asserted)`.
    // The ADR-0061 §2 gate is untouched — the *transfer* is still withheld,
    // only the coarse declaration reaches the dump (never a refined
    // `non-empty-*` or split answer).
    for (expr, floor) in [
        ("explode(',', $s)", "list<string>"),
        ("range(1, 3)", "array"),
        ("preg_replace('/a/', 'b', $s)", "string|null|array"),
        ("var_export($s, true)", "string|null"),
    ] {
        let src =
            format!("<?php\nfunction f(string $s): void {{ \\PHPStan\\dumpType({expr}); }}\n");
        assert_eq!(
            one_type_with(&src, &mut NoPhp),
            format!("dumped type: {floor} (asserted)"),
            "no-PHP run must withhold the transfer for {expr} and fall to the floor"
        );
    }
}

#[test]
fn a_declaration_the_rule_was_not_written_against_withholds_it() {
    // Widening staleness (ADR-0061 §2): engine declares `array|false` for
    // `explode` (not what the rule expects), so the claim is discarded.
    // `array|false` also seeds no fact (multi-base), so the ADR-0069 floor
    // speaks: coarse `list<string>`, never the discarded `non-empty-list<string>`.
    let mut mock = Mock::sidecar();
    mock.types.insert("explode".to_owned(), "array|false".to_owned());
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(explode(',', $s)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: list<string> (asserted)");
}

#[test]
fn a_project_function_shadowing_the_name_declines() {
    let src = "<?php\nfunction explode(string $a, string $b): array { return []; }\n\
               function f(string $s): void { \\PHPStan\\dumpType(explode(',', $s)); }\n";
    let ds = diagnostics_with(src, &mut Mock::sidecar());
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: unknown");
}
