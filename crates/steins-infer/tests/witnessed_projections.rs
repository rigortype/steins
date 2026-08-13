//! Issue #328 — the positional projections executed on the order-witnessed lane.
//!
//! `array_keys` / `array_values` / `array_reverse` / `array_flip` over a subject
//! whose construction the walk observed answer the *sequence*, where a subject
//! that merely had an order declared at it keeps the key-set widening. That split
//! is the whole content of the slice, so it is tested as a split: almost every
//! positive assertion here has a declared-shape twin asserting the widening did
//! not move.
//!
//! **The line this must not cross** (ADR-0062 §2/§7, phpstan/phpstan#14940): a
//! shape with no order witness is a key *set*. `array_keys(array{a: int, b: int})`
//! must stay `non-empty-list<'a'|'b'>` and must never become `list{'a', 'b'}` —
//! the declaration admits `['b' => …, 'a' => …]` just as well, and claiming the
//! declared order is the upstream false positive this project declines by name.
//!
//! Every expected value was probed at PHP 8.5.9 (`php -r`) and cross-checked
//! against PHPStan 2.2.2; the probe is quoted at each rule. Order claims are
//! measured, not recalled.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A mock sidecar declaring what the family's admission gate reads: the engine's
/// own `array` return declaration for each of the four names. Without it the
/// rule is withheld — the gate working — so the tests need this to see anything.
/// `count`'s reflected envelope is here for the composition test alone.
#[derive(Default)]
struct Mock {
    types: HashMap<String, String>,
    facts: HashMap<String, Fact>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut types = HashMap::new();
        for f in ["array_values", "array_keys", "array_flip", "array_reverse", "array_slice"] {
            types.insert(f.to_owned(), "array".to_owned());
        }
        types.insert("count".to_owned(), "int".to_owned());
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        Mock { types, facts }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect();
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a projection emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A witnessed fixture: statements over two natively-typed parameters, one dump.
fn dump(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(int $x, string $s): void {{ {body} }}\n"))
}

/// A declared fixture: `@param <decl> $v`, one dump of `<expr>`.
fn declared(decl: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

// ---- array_keys ------------------------------------------------------------

#[test]
fn array_keys_of_a_witnessed_literal_is_the_key_sequence() {
    // Probed: `array_keys(['a' => 1, 'b' => 2]) === [0 => 'a', 1 => 'b']`.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_keys(['a' => 1, 'b' => 2]));"),
        "dumped type: list{'a', 'b'}"
    );
    // Probed: `array_keys([-5 => 1, 3 => 2]) === [-5, 3]` — the keys become
    // values, and the result is reindexed from zero.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_keys([-5 => 1, 3 => 2]));"),
        "dumped type: list{-5, 3}"
    );
}

#[test]
fn array_keys_answers_exactly_even_when_no_value_is_known() {
    // **The headline case.** The result's VALUES are the subject's KEYS, so
    // nothing here depended on `$x` at all — and this is the shape real code
    // takes, which a fold could never reach.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_keys(['a' => $x, 'b' => $x]));"),
        "dumped type: list{'a', 'b'}"
    );
}

#[test]
fn the_witnessed_order_survives_a_binding() {
    // The witnessed order is the REVERSE of the canonical key order, so a shape
    // that had lost the witness would answer `list{'a', 'b'}` here. Probed:
    // `array_keys(['b' => 1, 'a' => 2]) === ['b', 'a']`.
    assert_eq!(
        dump("$b = ['b' => 1, 'a' => $x]; \\PHPStan\\dumpType(array_keys($b));"),
        "dumped type: list{'b', 'a'}"
    );
}

// ---- array_values ----------------------------------------------------------

#[test]
fn array_values_reindexes_and_keeps_the_slots() {
    // Probed: `array_values(['a' => 1, 'b' => 2]) === [1, 2]`.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_values(['a' => 1, 'b' => 2]));"),
        "dumped type: list{1, 2}"
    );
    // An unknown slot travels through unread — it costs that element, not the
    // sequence, and not the siblings that were proven.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_values(['a' => $x, 'b' => 'z']));"),
        "dumped type: list{int, 'z'}"
    );
}

// ---- array_reverse ---------------------------------------------------------

#[test]
fn array_reverse_renumbers_int_keys_and_keeps_string_ones() {
    // The probe this rule is written from, verbatim:
    // `array_reverse(['a' => 1, 5 => 2, 'b' => 3, 9 => 4])
    //    === [0 => 4, 'b' => 3, 1 => 2, 'a' => 1]`
    // — reversed, string keys surviving in place, integer keys renumbered `0..`
    // in the NEW order.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_reverse(['a' => 1, 5 => 2, 'b' => 3, 9 => 4]));"),
        "dumped type: array{0: 4, b: 3, 1: 2, a: 1}"
    );
    // Probed: `array_reverse([1, 2, 3]) === [3, 2, 1]`.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_reverse([1, 2, 3]));"),
        "dumped type: list{3, 2, 1}"
    );
}

#[test]
fn array_reverse_carries_unknown_slots_through() {
    assert_eq!(
        dump("$a = ['x', $s, 'z']; \\PHPStan\\dumpType(array_reverse($a));"),
        "dumped type: list{'z', string, 'x'}"
    );
}

// ---- array_flip ------------------------------------------------------------

#[test]
fn array_flip_swaps_keys_and_values() {
    // Probed: `array_flip(['a', 'b']) === ['a' => 0, 'b' => 1]`.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_flip(['a', 'b']));"),
        "dumped type: array{a: 0, b: 1}"
    );
    // Probed: `array_flip(['x' => '1', 'y' => 2]) === [1 => 'x', 2 => 'y']` —
    // the flipped VALUE goes through PHP's own key normalization, so the string
    // `'1'` lands as the integer key `1`.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_flip(['x' => '1', 'y' => 2]));"),
        "dumped type: array{1: 'x', 2: 'y'}"
    );
    // Probed: `array_flip(['a', 'a']) === ['a' => 1]` — last wins, in place.
    assert_eq!(dump("\\PHPStan\\dumpType(array_flip(['a', 'a']));"), "dumped type: array{a: 1}");
}

#[test]
fn array_flip_declines_on_an_unproven_value() {
    // The one name whose result KEYS come from the subject's VALUES, so an
    // unproven value is an unproven key and there is no honest partial answer.
    // It falls to the widening the shape rung computes, not to silence.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_flip(['a' => $x]));"),
        "dumped type: array<int, 'a'>"
    );
}

// ---- The negative pin: a declared shape is a key SET -----------------------

#[test]
fn a_declared_shape_keeps_every_widening_it_had() {
    // **The line this slice must not cross.** `array{a: int, b: int}` admits
    // `['b' => …, 'a' => …]` just as well, so its field order is not runtime
    // order — claiming the sequence here is phpstan/phpstan#14940's FP class.
    assert_eq!(
        declared("array{a: int, b: int}", "array_keys($v)"),
        "dumped type: non-empty-list<'a'|'b'> (asserted)"
    );
    assert_eq!(
        declared("array{a: int, b: int}", "array_values($v)"),
        "dumped type: non-empty-list<int> (asserted)"
    );
    assert_eq!(
        declared("array{a: int, b?: string}", "array_keys($v)"),
        "dumped type: non-empty-list<'a'|'b'> (asserted)"
    );
    assert_eq!(
        declared("array<string, int>", "array_keys($v)"),
        "dumped type: list<string> (asserted)"
    );
    assert_eq!(
        declared("array{a: int, b?: int}", "array_reverse($v)"),
        "dumped type: non-empty-associative-array<int> (asserted)"
    );
    assert_eq!(
        declared("array{a: int, b?: int}", "array_flip($v)"),
        "dumped type: array<int, 'a'|'b'> (asserted)"
    );
}

#[test]
fn an_optional_field_has_no_single_sequence_to_execute_over() {
    // A witness alone is not enough: `list{int, 1?: string}` realizes as one
    // entry or two, so no single sequence describes every admitted value.
    // `witnessed_order` declines and the key-set widening stands.
    assert_eq!(
        declared("list{int, 1?: string}", "array_reverse($v)"),
        "dumped type: non-empty-list<mixed> (asserted)"
    );
}

// ---- Out of the rung -------------------------------------------------------

#[test]
fn a_second_argument_is_a_different_function() {
    // `array_keys($x, $search)` filters by value and `array_reverse($x, true)`
    // preserves keys — neither is the rule, so both decline to the envelope.
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_keys(['a' => 1], 1));"),
        "dumped type: list<int|string> (asserted)"
    );
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_reverse(['a' => 1], true));"),
        "dumped type: array (asserted)"
    );
}

#[test]
fn a_silent_engine_withholds_the_whole_family() {
    // The admission gate (ADR-0061 §2): no reflected `array` declaration, no
    // rule — however well the subject's order is witnessed. What answers
    // instead is the rung strictly below, ADR-0069's declared-return floor, and
    // it says so with `(asserted)`: a catalog declaration, not a runtime
    // answer. Precision is lost with the engine, never soundness.
    struct Silent;
    impl Folder for Silent {
        fn fold(&mut self, _n: &str, _a: &[ArgValue]) -> Option<ArgValue> {
            None
        }
    }
    let src = "<?php\nfunction f(): void { \\PHPStan\\dumpType(array_keys(['a' => 1])); }\n";
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Silent)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].message, "dumped type: list<int|string> (asserted)");
}

// ---- Composition -----------------------------------------------------------

#[test]
fn an_exact_projection_flows_on() {
    // A `Singleton` result is a value like any other: it binds, it counts, and
    // it decides an identity.
    assert_eq!(
        dump("$k = array_keys(['a' => 1, 'b' => 2]); \\PHPStan\\dumpType($k);"),
        "dumped type: list{'a', 'b'}"
    );
    assert_eq!(
        dump("$k = array_keys(['a' => $x, 'b' => $x]); \\PHPStan\\dumpType(count($k));"),
        "dumped type: 2"
    );
    assert_eq!(
        dump("$k = array_keys(['a' => 1, 'b' => 2]); \\PHPStan\\dumpType($k === ['a', 'b']);"),
        "dumped type: true"
    );
    assert_eq!(
        dump("$k = array_keys(['a' => 1, 'b' => 2]); \\PHPStan\\dumpType($k === ['b', 'a']);"),
        "dumped type: false"
    );
}
