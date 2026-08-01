//! ADR-0064 DR3 — the argument-DISPATCHED symbolic transfers (seam ii), at
//! fixture level.
//!
//! These are the rules whose answer depends on an argument the S3/S7 rung cannot
//! even bind: `explode`'s separator, `range`'s bounds, `preg_replace`'s subject,
//! `var_export`'s literal flag. Each one is asserted from both sides — the
//! refinement it lands, and the decline it takes when its premise is missing.
//! Declining is a first-class outcome (ADR-0061 §1), so every rule here owns at
//! least one `unknown` fixture.
//!
//! `json_decode` appears only in the decline section: its reflected declaration
//! is bare `mixed` and its soundest per-flag envelope is a six-base union the
//! domain has no single `Fact` for. That is a measured refusal, not a gap.
//!
//! Zero emission is asserted on every fixture, as in `shape_projections.rs`: a
//! transfer-derived fact never premises a finding.

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A mock PHP answering the reflected *declarations* the DR3 admission gate
/// consults — verbatim `ReflectionFunction::getReturnType()` renderings at
/// `PINNED_PHP` (8.5.8), captured by probe.
#[derive(Default)]
struct Mock {
    types: HashMap<String, String>,
    facts: HashMap<String, Fact>,
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
        ] {
            types.insert(f.to_owned(), t.to_owned());
        }
        // `var_export`'s envelope is representable (`?string`) and is the rung
        // BELOW the transfer — its presence is what makes the null-strip visible
        // as a refinement rather than as an answer out of nowhere.
        let mut facts = HashMap::new();
        facts.insert(
            "var_export".to_owned(),
            Fact::General { base: Base::String, nullable: true },
        );
        Mock { types, facts, absence: true }
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
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
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
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
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

// ---------------------------------------------------------------------------
// explode: non-empty separator ⇒ non-empty-list<string>
// ---------------------------------------------------------------------------

#[test]
fn explode_on_a_literal_separator_is_a_non_empty_list_of_strings() {
    // PHP 8 has no `false` arm and `explode(',', '')` is `['']` — the split of any
    // string on a non-empty separator has at least one piece.
    assert_eq!(
        dump("string $s", "explode(',', $s)"),
        "dumped type: non-empty-list<string>"
    );
    // A multi-character separator is no different.
    assert_eq!(
        dump("string $s", "explode('::', $s)"),
        "dumped type: non-empty-list<string>"
    );
}

#[test]
fn explode_takes_the_separators_own_predicate_not_just_a_literal() {
    // The separator need not be literal: `non-empty-string` is exactly the premise
    // the rule needs, and a truthiness guard is one place the domain computes it
    // (`Refine::Truthy` adds `NON_FALSY`, which closes over `NON_EMPTY`).
    let src = "<?php\nfunction f(string $sep, string $s): void {\n\
               if ($sep) { \\PHPStan\\dumpType(explode($sep, $s)); }\n}\n";
    assert_eq!(one_type(src), "dumped type: non-empty-list<string>");
}

#[test]
fn explode_declines_an_empty_or_unknown_separator() {
    // `explode('', 'abc')` is a `ValueError` at 8.5.8 — there is no return value
    // to describe, and an unknown separator might be that call.
    //
    // What the dump shows is the ADR-0069 FLOOR two rungs below, which since
    // ADR-0071 carries `explode`'s array row: the coarse catalog `list<string>`,
    // marked `(asserted)`. The decline this test is about is intact and legible in
    // that marker — the rule's own answer is `non-empty-list<string>` and carries no
    // marker, so a transfer that leaked would be visible here, not hidden by it.
    assert_eq!(dump("string $s", "explode('', $s)"), "dumped type: list<string> (asserted)");
    assert_eq!(
        dump("string $sep, string $s", "explode($sep, $s)"),
        "dumped type: list<string> (asserted)"
    );
}

#[test]
fn explode_declines_the_limit_form_because_a_limit_can_empty_the_result() {
    // THE load-bearing decline of this rule: `explode(',', 'a,b,c', -5)` returns
    // `[]` at 8.5.8, so carrying `non-empty` across a limit argument would be a
    // false premise rather than a lost refinement. The floor's `list<string>` is
    // what stands instead, and it is exactly right about the empty case.
    assert_eq!(dump("string $s", "explode(',', $s, 2)"), "dumped type: list<string> (asserted)");
    assert_eq!(dump("string $s", "explode(',', $s, -5)"), "dumped type: list<string> (asserted)");
}

// ---------------------------------------------------------------------------
// range: always a non-empty list; integral arguments sharpen the element
// ---------------------------------------------------------------------------

#[test]
fn range_of_integral_bounds_is_a_non_empty_list_of_ints() {
    assert_eq!(dump("", "range(1, 3)"), "dumped type: non-empty-list<int>");
    // Equal bounds still produce one entry (`range(1, 1) === [1]`), and a
    // descending pair is a list just the same.
    assert_eq!(dump("", "range(5, 5)"), "dumped type: non-empty-list<int>");
    assert_eq!(dump("", "range(3, 1)"), "dumped type: non-empty-list<int>");
    // An integral step keeps the element bound.
    assert_eq!(dump("", "range(1, 9, 2)"), "dumped type: non-empty-list<int>");
    // Non-literal int bounds work identically — the rule reads facts, not text.
    assert_eq!(dump("int $a, int $b", "range($a, $b)"), "dumped type: non-empty-list<int>");
}

#[test]
fn range_keeps_the_list_and_the_non_emptiness_when_the_element_is_unknown() {
    // A fractional step makes the result a float array, so the element bound is
    // dropped — but `range` never returns `[]` and never returns a non-list.
    assert_eq!(dump("", "range(1, 2, 0.5)"), "dumped type: non-empty-list<mixed>");
    // `range('a', 'c') === ['a','b','c']` — a non-empty list of strings.
    assert_eq!(dump("", "range('a', 'c')"), "dumped type: non-empty-list<mixed>");
    assert_eq!(dump("float $a, float $b", "range($a, $b)"), "dumped type: non-empty-list<mixed>");
}

#[test]
fn range_declines_an_arity_php_itself_rejects() {
    // One argument, or four, is an `ArgumentCountError` — the seam refuses to
    // describe a call PHP will not make. The floor below is arity-blind by design
    // (ADR-0069 §2 imports return envelopes and nothing else), so it answers
    // `range`'s bare catalog `array`; the refined `non-empty-list<int>` this rule
    // would have produced is what the decline withholds, and the marker says so.
    assert_eq!(dump("", "range(1)"), "dumped type: array (asserted)");
    assert_eq!(dump("", "range(1, 2, 3, 4)"), "dumped type: array (asserted)");
}

// ---------------------------------------------------------------------------
// preg_replace: the subject's base splits the multi-base declaration
// ---------------------------------------------------------------------------

#[test]
fn preg_replace_of_a_string_subject_is_string_or_null() {
    // The reflected `array|string|null` is multi-base and seeds NO fact today;
    // the subject's own base is what splits it.
    assert_eq!(
        dump("string $s", "preg_replace('/a/', 'b', $s)"),
        "dumped type: string|null"
    );
    // An array `$pattern` does not change the answer — `$subject` alone governs.
    assert_eq!(
        dump("string $s", "preg_replace(['/a/', '/b/'], 'z', $s)"),
        "dumped type: string|null"
    );
    // `$limit` and `$count` may follow.
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
    // A subject of unknown base could be either arm, and a nullable one is a
    // case the rule was never probed against. Withholding the SPLIT is the point:
    // what stands is the floor's unsplit `string|null|array`, which states both
    // arms rather than choosing one, and carries the `(asserted)` marker the rule's
    // own `string|null` would not.
    assert_eq!(
        dump("", "preg_replace('/a/', 'b', $u)"),
        "dumped type: string|null|array (asserted)"
    );
    assert_eq!(
        dump("?string $s", "preg_replace('/a/', 'b', $s)"),
        "dumped type: string|null|array (asserted)"
    );
}

// ---------------------------------------------------------------------------
// var_export: the literal `true` flag strips the envelope's null arm
// ---------------------------------------------------------------------------

#[test]
fn var_export_with_a_literal_true_flag_is_a_string() {
    assert_eq!(dump("int $v", "var_export($v, true)"), "dumped type: string");
    // Nothing about `$value` matters — `var_export(null, true)` is the string
    // `'NULL'`, not `null`.
    assert_eq!(dump("", "var_export(null, true)"), "dumped type: string");
}

#[test]
fn var_export_without_the_flag_falls_back_to_its_own_envelope() {
    // The one-argument and literal-`false` forms decline; the reflected `?string`
    // envelope already describes them exactly, and it is what stands.
    assert_eq!(dump("int $v", "var_export($v)"), "dumped type: string|null");
    assert_eq!(dump("int $v", "var_export($v, false)"), "dumped type: string|null");
    assert_eq!(dump("int $v, bool $b", "var_export($v, $b)"), "dumped type: string|null");
}

// ---------------------------------------------------------------------------
// json_decode: the batch's measured decline
// ---------------------------------------------------------------------------

#[test]
fn json_decode_declines_in_every_form() {
    // Reflected declaration: bare `mixed`. Even the `$assoc = true` form admits
    // `array|int|float|string|bool|null` — six bases, no single `Fact`. A rule
    // that cannot state its own answer declines rather than guessing an arm.
    assert_eq!(dump("string $s", "json_decode($s)"), "dumped type: unknown");
    assert_eq!(dump("string $s", "json_decode($s, true)"), "dumped type: unknown");
    assert_eq!(dump("string $s", "json_decode($s, false)"), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// The admission gate (ADR-0061 §2), on the new rules
// ---------------------------------------------------------------------------

#[test]
fn without_the_reflected_declaration_every_transfer_is_withheld() {
    struct NoPhp;
    impl Folder for NoPhp {
        fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
            None
        }
    }
    // Every one of these used to read `unknown`. None of them does any more, and the
    // reason is never this rung: ADR-0069's declared-return FLOOR answers underneath
    // it with the name's own catalog declaration, marked `(asserted)` because a
    // catalog row is not a runtime answer. The ADR-0061 §2 gate this test is about is
    // untouched — the *transfer* is still withheld, and what reaches the dump is the
    // coarse declaration, never the rule's output. The two are distinguishable at a
    // glance: every refined answer here is `non-empty-*` or a split, and none of them
    // appears below.
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
    // Widening staleness (ADR-0061 §2): this engine declares something else for
    // `explode`, so the rule's claim is not countersigned and is discarded. The
    // engine's own `array|false` seeds no fact either (multi-base), so the rung
    // yields `None` and the ADR-0069 floor speaks — the coarse `list<string>`, and
    // never the discarded `non-empty-list<string>`.
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
