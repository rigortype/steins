//! `foreach.non-iterable` (ADR-0078, issue #192): a `foreach` subject proven — in
//! the same value-domain lane `offset.missing` reads — a non-array scalar/`null`.
//! PHP's own consequence (`php -r`-witnessed, 8.5.9): `foreach() argument must be
//! of type array|object, {type} given`, and the loop body is skipped entirely.
//!
//! No sidecar/boot-surface dependency here (unlike the offset family): the check
//! consults only the env's own value-domain fact, so every fixture below uses the
//! sound-subset [`NoFold`] folder. The one gate that DOES apply is the
//! `warning-handler` posture (ADR-0049 §7), exercised at the bottom.

use steins_infer::{Diagnostic, FOREACH_NON_ITERABLE_ID, NoFold, check_full};
use steins_syntax::SourceTree;

fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == FOREACH_NON_ITERABLE_ID)
        .collect()
}


// Firing fixtures — one per scalar kind (Singleton).


#[test]
fn fires_on_int_subject() {
    let d = diags("<?php\n$a = 42;\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 3, "the foreach statement's own line: {d:#?}");
    assert!(d[0].message.contains("provably 42"), "{}", d[0].message);
    assert!(
        d[0].message.contains(
            "foreach() argument must be of type array|object, int given"
        ),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_float_subject() {
    let d = diags("<?php\n$a = 1.5;\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains(
            "foreach() argument must be of type array|object, float given"
        ),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_string_subject() {
    let d = diags("<?php\n$a = 'hello';\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains(
            "foreach() argument must be of type array|object, string given"
        ),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_bool_true_subject() {
    let d = diags("<?php\n$a = true;\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains(
            "foreach() argument must be of type array|object, true given"
        ),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_bool_false_subject() {
    let d = diags("<?php\n$a = false;\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains(
            "foreach() argument must be of type array|object, false given"
        ),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_null_subject() {
    let d = diags("<?php\n$a = null;\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains(
            "foreach() argument must be of type array|object, null given"
        ),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_key_value_and_by_ref_forms_too() {
    // The Singleton proof is orthogonal to the binding shape (`$k => $v`, `&$v`).
    let kv = diags("<?php\n$a = 7;\nforeach ($a as $k => $v) {\n    echo $v;\n}\n");
    assert_eq!(kv.len(), 1, "{kv:#?}");
    let by_ref = diags("<?php\n$a = 7;\nforeach ($a as &$v) {\n    echo $v;\n}\n");
    assert_eq!(by_ref.len(), 1, "{by_ref:#?}");
}


// Firing fixture — the abstract (`Refined`/`General`) leg, via a type-predicate
// guard (DR2/ADR-0064 §5) rather than a literal assignment.


#[test]
fn fires_on_is_int_narrowed_subject() {
    // `is_int($x)` mints `Fact::General { base: Int, nullable: false }` at the
    // Verified stratum for a parameter that otherwise carries no fact — a proven
    // scalar BASE, not a concrete value, which is the "value-domain fact proving
    // scalar" leg the id also fires on.
    let src = "<?php\nfunction f($x) {\n    if (is_int($x)) {\n        foreach ($x as $v) {\n            echo $v;\n        }\n    }\n}\n";
    let d = diags(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("type int"), "{}", d[0].message);
    assert!(
        d[0].message.contains(
            "foreach() argument must be of type array|object, int given"
        ),
        "{}",
        d[0].message
    );
}

#[test]
fn fires_on_is_bool_narrowed_subject_without_a_literal_quote() {
    // The `General{Bool}` case: proven non-iterable (a scalar base), but no
    // single warning word can be attributed (PHP names the concrete
    // `true`/`false`, never the bare word `bool`) — so this fixture pins that the
    // finding still fires, just without a fabricated literal quote.
    let src = "<?php\nfunction f($x) {\n    if (is_bool($x)) {\n        foreach ($x as $v) {\n            echo $v;\n        }\n    }\n}\n";
    let d = diags(src);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("bool"), "{}", d[0].message);
    assert!(
        !d[0].message.contains("given\""),
        "no single word is attributable for a bare bool base: {}",
        d[0].message
    );
}


// Silence fixtures — every pinned leg.


#[test]
fn silent_on_array_literal_subject() {
    let d = diags("<?php\n$a = [1, 2, 3];\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert!(d.is_empty(), "an array is iterable: {d:#?}");
}

#[test]
fn silent_on_empty_array_subject() {
    let d = diags("<?php\n$a = [];\nforeach ($a as $v) {\n    echo $v;\n}\n");
    assert!(d.is_empty(), "an empty array is still iterable (zero iterations): {d:#?}");
}

#[test]
fn silent_on_plain_object_subject() {
    // Plain object iteration (accessible properties) is legal PHP — never fires,
    // regardless of the class.
    let d = diags(
        "<?php\nclass Foo {}\n$a = new Foo();\nforeach ($a as $v) {\n    echo $v;\n}\n",
    );
    assert!(d.is_empty(), "plain object iteration is legal: {d:#?}");
}

#[test]
fn silent_on_traversable_subject() {
    let d = diags(
        "<?php\nclass It implements Iterator {\n    public function current(): mixed { return null; }\n    public function key(): mixed { return null; }\n    public function next(): void {}\n    public function rewind(): void {}\n    public function valid(): bool { return false; }\n}\n$a = new It();\nforeach ($a as $v) {\n    echo $v;\n}\n",
    );
    assert!(d.is_empty(), "a Traversable implementor is iterable: {d:#?}");
}

#[test]
fn silent_on_unenumerable_class_subject() {
    // A class-typed parameter whose hierarchy is not enumerable here (it could
    // implement `Traversable` out of view) — an object carries no value-domain
    // fact at all, so this is silence by the same construction as every other
    // object case, never a special-cased hierarchy walk.
    let d = diags(
        "<?php\nfunction f(SomeExternalClass $x) {\n    foreach ($x as $v) {\n        echo $v;\n    }\n}\n",
    );
    assert!(d.is_empty(), "an unenumerable class hierarchy stays silent: {d:#?}");
}

#[test]
fn silent_on_generator_subject() {
    let d = diags(
        "<?php\nfunction gen(): Generator {\n    yield 1;\n}\n$a = gen();\nforeach ($a as $v) {\n    echo $v;\n}\n",
    );
    assert!(d.is_empty(), "a Generator is iterable: {d:#?}");
}

#[test]
fn silent_on_iterable_declared_param() {
    let d = diags(
        "<?php\nfunction f(iterable $x) {\n    foreach ($x as $v) {\n        echo $v;\n    }\n}\n",
    );
    assert!(d.is_empty(), "an `iterable`-declared parameter seeds no fact: {d:#?}");
}

#[test]
fn silent_on_unannotated_param() {
    // No guard, no assignment: the parameter carries no fact at all — unknown,
    // not proven — so the default "Maybe ⇒ silence" floor applies.
    let d = diags("<?php\nfunction f($x) {\n    foreach ($x as $v) {\n        echo $v;\n    }\n}\n");
    assert!(d.is_empty(), "an unproven subject stays silent: {d:#?}");
}

#[test]
fn silent_on_union_subject() {
    // A `Maybe`-typed join (one branch an int, the other an array): the merged
    // fact is a heterogeneous `OneOf`, deliberately out of this id's scope (no
    // single warning word to attribute) — silence, not a false proof.
    let d = diags(
        "<?php\nif (rand()) {\n    $a = 1;\n} else {\n    $a = [1, 2];\n}\nforeach ($a as $v) {\n    echo $v;\n}\n",
    );
    assert!(d.is_empty(), "a Maybe/union-with-iterable subject stays silent: {d:#?}");
}

#[test]
fn silent_on_dynamic_subject_expression() {
    // The subject is not a bare `$var` (`ForeachSite::subject` is `None` for
    // anything else) — no env lookup is even attempted.
    let d = diags(
        "<?php\nfunction f(): int { return 42; }\nforeach (f() as $v) {\n    echo $v;\n}\n",
    );
    assert!(d.is_empty(), "a non-variable subject never reaches the env lookup: {d:#?}");
}


// The `warning-handler` gate (ADR-0049 §7) — both postures, `readonly.reassigned`'s
// single-id-family precedent judged the same way `offset.missing` is.


#[test]
fn warning_handler_null_silences() {
    let tree = SourceTree::parse("<?php\n$a = 42;\nforeach ($a as $v) {\n    echo $v;\n}\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut NoFold, false)
        .into_iter()
        .filter(|d| d.id == FOREACH_NON_ITERABLE_ID)
        .collect();
    assert!(d.is_empty(), "\"null\" posture silences the warning-grade finding: {d:#?}");
}

#[test]
fn warning_handler_abort_emits() {
    let tree = SourceTree::parse("<?php\n$a = 42;\nforeach ($a as $v) {\n    echo $v;\n}\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == FOREACH_NON_ITERABLE_ID)
        .collect();
    assert_eq!(d.len(), 1, "the default \"abort\" posture emits: {d:#?}");
}
