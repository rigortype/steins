//! What a `foreach` header binds at the body entry (issue #652, completing the
//! ADR-0027 amendment of 2026-09-10).
//!
//! A `foreach` header binds rather than tests, and what it binds is already in
//! hand: the subject's own element type. `$v` and `$k` stay ordinary members of the
//! construct's `writes` — the entry forgetting drops them first — and the binding is
//! applied to the forgotten env afterwards, from the subject as that env holds it.
//! That order is the soundness: the entry stays iteration-count-agnostic, a body
//! that reassigns `$v` cannot reach the next entry through it, and the code after
//! the loop is untouched (issue #651 owns that half).
//!
//! Two lanes answer, in ADR-0037's trust order. A proven array answers exactly, at
//! its own stratum. A declared one answers from its arms, at the arms' — `Asserted`
//! for a docblock, so the element fact never premises a proof-layer finding
//! (ADR-0052 §5, the A-G9 corollary). Where neither states an element type, nothing
//! is bound: a bare `array` leaves both targets defined but untyped, which is what
//! `untyped.iterable-value` exists to ask an author to fix.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` dump a source produces, as `line: rendered fact`.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == DEBUG_TYPE_ID)
        .map(|d| format!("{}: {}", d.line, d.message))
        .collect()
}

/// The rendered dumps alone, line numbers dropped — the answer, not where it sits.
fn answers(src: &str) -> Vec<String> {
    dumps(src)
        .into_iter()
        .map(|d| d.split_once(": dumped type: ").expect("a dump line").1.to_owned())
        .collect()
}

/// A function wrapping `body`, with `doc` above a single `array $xs` parameter.
fn subject(doc: &str, body: &str) -> String {
    format!("<?php\nclass Foo {{}}\n/**\n * @param {doc} $xs\n */\nfunction f($xs): void {{\n{body}\n}}\n")
}

#[test]
fn a_list_subject_binds_the_element_and_the_index() {
    // `list<T>`'s keys are exactly `0..n-1` (#14939), so the key is the
    // non-negative ints — sharper than `array<int, T>`'s bare `int`, and the one
    // thing the list spelling buys a reader of the body.
    let src = subject("list<int>", "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["int<0, max> (asserted)".to_owned(), "int (asserted)".to_owned()],
        "a list types both halves"
    );
}

#[test]
fn a_map_subject_binds_a_class_element_through_the_arm_lane() {
    // `array<string, Foo>` states both halves, and the value half is a CLASS: the
    // value domain has no object inhabitant (ADR-0035/0043), so `Foo` has no `Fact`
    // to be and exists only as a declared arm. Binding both lanes is what makes the
    // dump answer `Foo` rather than `unknown`.
    let src = subject("array<string, Foo>", "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["string (asserted)".to_owned(), "Foo (asserted)".to_owned()],
        "the class element rides the arm lane"
    );
}

#[test]
fn a_bracket_array_subject_binds_array_key_and_the_element() {
    // `T[]` is `array<array-key, T>`: the key half is PHP's `int|string`, which the
    // abstract union layer (ADR-0085) can hold, so it is stated rather than dropped.
    let src = subject("int[]", "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["int|string (asserted)".to_owned(), "int (asserted)".to_owned()],
        "an unstated key contract is `array-key`, not silence"
    );
}

#[test]
fn a_sealed_shape_binds_the_union_of_its_keys_and_its_values() {
    // A sealed shape declares the WHOLE array, so the union of its fields is the
    // element type exactly — the keys as literals, the values as their union.
    let src = subject("array{a: int, b: string}", "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["'a'|'b' (asserted)".to_owned(), "int|string (asserted)".to_owned()],
        "a sealed shape is its own element type"
    );
}

#[test]
fn an_unsealed_shape_with_an_untyped_tail_binds_nothing() {
    // The refusal the sealed case is worth stating: an unsealed tail admits keys and
    // values the field list never mentions, so the union of the declared fields is a
    // SUBSET of the element type, not the element type. Binding it would be the
    // invented fact this slice exists not to invent.
    let src = subject("array{a: int, ...}", "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["unknown".to_owned(), "unknown".to_owned()],
        "an open tail is not a stated element type"
    );
}

#[test]
fn a_typed_tail_joins_the_declared_fields() {
    // A tail that IS typed states what the rest of the array holds, so it joins the
    // declared fields as one more alternative — which is exactly what it admits.
    //
    // The key half renders `string`, not `'a'|string`: the join is the domain's own
    // (`Fact::join`), and a literal joined with its base widens to the base. That
    // is the answer to state — the tail admits every string key, `'a'` included.
    let src = subject("array{a: int, ...<string, bool>}", "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["string (asserted)".to_owned(), "int|bool (asserted)".to_owned()],
        "the tail is an alternative, not a replacement"
    );
}

#[test]
fn an_iterable_subject_binds_what_its_declaration_states() {
    // `iterable<K, V>` states both contracts outright, and its array realization
    // iterates exactly like the `array<K, V>` one. `Traversable`/`Generator` are the
    // separate lane the issue puts out of scope: their value type rides
    // `@implements`/`@extends` template arguments, not a declaration read here.
    let src = subject("iterable<string, int>", "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["string (asserted)".to_owned(), "int (asserted)".to_owned()],
        "a declared iterable is read like the map it spells"
    );
}

#[test]
fn a_traversable_subject_binds_nothing() {
    // The floor every non-array declaration falls to, and the issue's own
    // out-of-scope ruling: a `Traversable`/`Generator` value type rides
    // `@implements`/`@extends` template arguments, a lane this reader does not
    // consult. The declaration names a class, a class states no element type here,
    // and nothing is bound rather than something being guessed.
    let src = subject(
        "\\Traversable",
        "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }",
    );
    assert_eq!(
        answers(&src),
        vec!["unknown".to_owned(), "unknown".to_owned()],
        "a class declaration states no element type"
    );
}

#[test]
fn a_proven_array_binds_its_elements_at_the_verified_stratum() {
    // The value lane answers first (ADR-0037: proven beats declared) and answers
    // exactly — the keys and the values as they are, with no `(asserted)` marker, so
    // the fact is fit to premise a proof-layer finding.
    let src = "<?php
function f(): void {
    $xs = ['a' => 1, 'b' => 2];
    foreach ($xs as $k => $v) {
        \\PHPStan\\dumpType($k);
        \\PHPStan\\dumpType($v);
    }
}
";
    assert_eq!(
        answers(src),
        vec!["'a'|'b'".to_owned(), "1|2".to_owned()],
        "a witnessed array answers exactly, and at the proof stratum"
    );
}

#[test]
fn an_empty_proven_array_binds_nothing() {
    // There is no element, so there is no element type — and a zero-iteration loop
    // needs no invented one. The value lane declines and the declared lane, which
    // has only the bare native envelope, declines with it.
    let src = "<?php
function f(): void {
    $xs = [];
    foreach ($xs as $v) {
        \\PHPStan\\dumpType($v);
    }
}
";
    assert_eq!(answers(src), vec!["unknown".to_owned()], "an empty array has no element");
}

#[test]
fn a_by_reference_target_refuses_the_binding() {
    // Issue #677: `as &$v` writes THROUGH the subject, and the trace models neither
    // that write nor the alias it installs. Typing `$v` from the subject would let
    // iteration 2 read an element iteration 1 overwrote — a stale fact, and the one
    // refusal this slice must not weaken.
    let src = subject("list<int>", "    foreach ($xs as &$v) {\n        \\PHPStan\\dumpType($v);\n    }");
    assert_eq!(
        answers(&src),
        vec!["unknown".to_owned()],
        "a by-ref target keeps the untyped binding"
    );
}

#[test]
fn a_by_reference_loop_writes_its_subject_so_a_nested_by_value_loop_binds_nothing() {
    // Issue #677's live shape, and the reason the refusal above is not enough on
    // its own. The outer loop rewrites `$a`'s elements through `&$v`; the inner
    // loop reads `$a` by value. If the outer construct did not count `$a` as a
    // write, the inner entry env would still hold the literal `[1, 2]` and bind
    // `$w` to `1|2` at Verified — and `$w + []` would be a proven `TypeError` on a
    // program that never throws (the guard holds only once `$a[0]` is an array).
    let src = "<?php
function f(): void {
    $a = [1, 2];
    $n = 0;
    foreach ($a as &$v) {
        foreach ($a as $i => $w) {
            \\PHPStan\\dumpType($w);
            if ($i < $n) { $z = $w + []; }
        }
        $v = ['x' => 1];
        $n++;
    }
}
";
    let tree = SourceTree::parse(src);
    let all = check(&tree, &[], "t.php");
    assert!(
        !all.iter().any(|d| d.id == "type.invalid-operand"),
        "no finding on a program that never throws: {all:?}"
    );
    assert_eq!(answers(src), vec!["unknown".to_owned()], "the aliased subject is a write of the outer loop");
}

#[test]
fn a_target_named_for_both_key_and_value_holds_the_value() {
    // PHP assigns the key first and the value last, so `$x` is the element. The
    // key binding is skipped for that name rather than overwritten, so the env and
    // the arm lane cannot disagree about it.
    let src = subject("array<int, Foo>", "    foreach ($xs as $x => $x) {\n        \\PHPStan\\dumpType($x);\n    }");
    assert_eq!(answers(&src), vec!["Foo (asserted)".to_owned()], "the value, not the key");
}

#[test]
fn a_destructuring_target_binds_nothing() {
    // Deferred, and pinned as deferred: `as [$a, $b]` binds names the statement does
    // not name, so there is nothing for the header to bind and the two stay writes
    // alone. The KEY beside it is a plain variable and binds as it always would.
    let src = subject("list<array{int, string}>", "    foreach ($xs as $k => [$a, $b]) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($a);\n        \\PHPStan\\dumpType($b);\n    }");
    assert_eq!(
        answers(&src),
        vec!["int<0, max> (asserted)".to_owned(), "unknown".to_owned(), "unknown".to_owned()],
        "the key still binds; the destructured halves do not"
    );
}

#[test]
fn a_property_target_stays_a_write_alone() {
    // `as $this->x` is a write to a property, not a binding of a variable, so the
    // header names nothing this statement can bind.
    let src = "<?php
class Holder {
    public $x;
    /** @param list<int> $xs */
    public function f($xs): void {
        foreach ($xs as $this->x) {
            \\PHPStan\\dumpType($this->x);
        }
    }
}
";
    assert_eq!(answers(src), vec!["unknown".to_owned()], "a property target binds no variable");
}

#[test]
fn the_binding_re_establishes_from_the_subject_at_every_entry() {
    // The iteration-count-agnosticism pin. The body reassigns `$v` to a string, and
    // the entry still says `int`: the binding is derived from the SUBJECT in the
    // forgotten env, not carried from the previous iteration's value. Invert the
    // order of the forgetting and the binding and this reads `'str'` — or, worse,
    // `int|string`, a join over an iteration count nothing here knows.
    let src = subject(
        "list<int>",
        "    foreach ($xs as $v) {\n        \\PHPStan\\dumpType($v);\n        $v = 'str';\n    }",
    );
    assert_eq!(
        answers(&src),
        vec!["int (asserted)".to_owned()],
        "the entry re-derives from the subject"
    );
}

#[test]
fn a_subject_the_body_rebinds_binds_nothing() {
    // The same rule from the other side: a subject in `writes` is gone from the
    // entry env, so there is nothing to bind from. Reading the subject from the env
    // as it stands BEFORE the construct would state iteration 1's element type as
    // every iteration's.
    let src = subject(
        "list<int>",
        "    foreach ($xs as $v) {\n        \\PHPStan\\dumpType($v);\n        $xs = [];\n    }",
    );
    assert_eq!(
        answers(&src),
        vec!["unknown".to_owned()],
        "a rebound subject types nothing"
    );
}

#[test]
fn a_union_of_two_array_arms_declines() {
    // The A-G3 rule, which the arm lane already keeps for the subject itself: two
    // array arms carry a discrimination a union fold would blur, and the element of
    // a blur is the element of neither arm.
    let src = subject(
        "list<int>|array<string, string>",
        "    foreach ($xs as $k => $v) {\n        \\PHPStan\\dumpType($k);\n        \\PHPStan\\dumpType($v);\n    }",
    );
    assert_eq!(
        answers(&src),
        vec!["unknown".to_owned(), "unknown".to_owned()],
        "two array arms state no single element type"
    );
}

#[test]
fn a_nullable_declared_subject_still_binds_its_element() {
    // A `null` arm says the subject may not be an array at all, which is
    // `foreach.non-iterable`'s question (ADR-0078). It is not an element-type
    // question, so it is skipped rather than allowed to suppress the one array arm.
    let src = subject(
        "list<int>|null",
        "    foreach ($xs as $v) {\n        \\PHPStan\\dumpType($v);\n    }",
    );
    assert_eq!(
        answers(&src),
        vec!["int (asserted)".to_owned()],
        "the null arm is not an array arm"
    );
}

#[test]
fn the_fall_through_after_the_loop_is_unchanged() {
    // The boundary issue #651 owns. Nothing the body computes escapes it, and the
    // binding is part of the body's entry env, not the construct's fall-through: `$v`
    // is in `writes`, so after the loop it is exactly what the write set leaves it.
    let src = subject(
        "list<int>",
        "    foreach ($xs as $v) {\n    }\n    \\PHPStan\\dumpType($v);",
    );
    assert_eq!(
        answers(&src),
        vec!["unknown".to_owned()],
        "the entry binding does not ride the fall-through"
    );
}
