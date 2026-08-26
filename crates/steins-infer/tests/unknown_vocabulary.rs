//! ADR-0091 §6 — `phpdoc.unknown-vocabulary`, read off a real docblock.
//!
//! `steins-contract` pins the recognition allowlist against the tables that own
//! it (`unknown_vocabulary_tests`); what this file pins is the walk: which type
//! positions are asked, how often a spelling is reported, and — the property
//! the whole id rests on — that asking changes nothing about what the type
//! means (#478's guarantee, ADR-0091 §6).

use steins_infer::{
    Diagnostic, PARAM_MISMATCH_ID, PHPDOC_UNKNOWN_VOCABULARY_ID, RETURN_MISMATCH_ID, check,
};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php")
}

/// The `phpdoc.unknown-vocabulary` messages a source produces, in emission order.
fn vocab(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == PHPDOC_UNKNOWN_VOCABULARY_ID)
        .map(|d| d.message)
        .collect()
}

fn vocab_count(src: &str) -> usize {
    vocab(src).len()
}

/// The firing case, and the two things ADR-0091 §5 says it can be: a
/// misspelling of vocabulary, and vocabulary from a tool Steins does not model.
/// The id does not distinguish them — the edit-distance refinement is §6's
/// deliberately-optional second slice — so both report at one floor.
#[test]
fn a_hyphenated_name_that_is_not_vocabulary_reports() {
    let typo = "<?php\n/** @param non-empy-string $s */\nfunction f($s): void {}\n";
    assert_eq!(vocab(typo).len(), 1);
    assert!(vocab(typo)[0].contains("non-empy-string"), "{:?}", vocab(typo));

    let unmodeled = "<?php\n/** @param some-psalm-thing $s */\nfunction f($s): void {}\n";
    assert_eq!(vocab_count(unmodeled), 1);
}

/// Every type-carrying tag is a type position. `@throws` included: a class
/// cannot be named with a `-` there either, so the spelling names nothing that
/// could ever be thrown.
#[test]
fn every_type_carrying_tag_is_asked() {
    assert_eq!(vocab_count("<?php\n/** @param foo-bar $s */\nfunction f($s): void {}\n"), 1);
    assert_eq!(vocab_count("<?php\n/** @return foo-bar */\nfunction f() {}\n"), 1);
    assert_eq!(vocab_count("<?php\nclass C {\n/** @var foo-bar */\npublic $p;\n}\n"), 1);
    assert_eq!(vocab_count("<?php\n/** @throws foo-bar */\nfunction f(): void {}\n"), 1);
    assert_eq!(
        vocab_count("<?php\n/** @phpstan-assert foo-bar $s */\nfunction f($s): void {}\n"),
        1,
    );
}

/// Nested positions are reached, and a generic's **base** is a name like any
/// other — the arity-blind reading both lowering tables already take.
#[test]
fn a_nested_position_is_reached() {
    for src in [
        "<?php\n/** @param list<foo-bar> $s */\nfunction f($s): void {}\n",
        "<?php\n/** @param foo-bar<int> $s */\nfunction f($s): void {}\n",
        "<?php\n/** @param int|foo-bar $s */\nfunction f($s): void {}\n",
        "<?php\n/** @param foo-bar[] $s */\nfunction f($s): void {}\n",
        "<?php\n/** @param array{a: foo-bar} $s */\nfunction f($s): void {}\n",
        "<?php\n/** @param callable(foo-bar): int $s */\nfunction f($s): void {}\n",
        "<?php\n/** @param ?foo-bar $s */\nfunction f($s): void {}\n",
    ] {
        assert_eq!(vocab_count(src), 1, "not reached: {src}");
    }
}

/// One defect written twice is one finding: the remedy is one edit, and a
/// docblock repeating a misspelling should not out-shout one that made it once.
/// Across *tags* the findings are separate — those are separate edits.
#[test]
fn a_spelling_reports_once_per_tag() {
    let twice_in_one = "<?php\n/** @param foo-bar|list<foo-bar> $s */\nfunction f($s): void {}\n";
    assert_eq!(vocab_count(twice_in_one), 1);

    let two_tags = "<?php\n/** @param foo-bar $a\n * @param foo-bar $b */\n\
        function f($a, $b): void {}\n";
    assert_eq!(vocab_count(two_tags), 2);

    let two_spellings = "<?php\n/** @param foo-bar|baz-qux $s */\nfunction f($s): void {}\n";
    assert_eq!(vocab_count(two_spellings), 2);
}

/// Builtin vocabulary is never convicted, sampled across every table the
/// allowlist reads: `KNOWN_UNENFORCED`, `DERIVED_OPERATORS`, the identifier
/// table's own hyphenated arms, the refined-string grid, and the generic
/// table's bases. The exhaustive per-table iteration lives in `steins-contract`
/// (`no_table_entry_is_ever_unknown_vocabulary`); this pins that the walk
/// consults it rather than a second list.
#[test]
fn no_builtin_spelling_is_ever_reported() {
    for ty in [
        "int-mask<1, 2>",
        "int-mask-of<Foo::M_*>",
        "properties-of<Foo>",
        "class-string-map<Foo, Bar>",
        "non-empty-literal-string",
        "arraylike-object",
        "stringable-object",
        "template-type<Foo, Bar, 'T'>",
        "key-of<array{a: int}>",
        "value-of<array{a: int}>",
        "non-nullable<int|null>",
        "return-type<Closure(): int>",
        "parameters-of<Closure(int): int>",
        "exclude-from<int|string, string>",
        "extract-from<int|string, string>",
        "array-key",
        "positive-int",
        "negative-int",
        "non-negative-int",
        "non-positive-int",
        "non-zero-int",
        "non-empty-string",
        "numeric-string",
        "non-falsy-string",
        "truthy-string",
        "class-string",
        "interface-string",
        "enum-string",
        "trait-string",
        "literal-string",
        "callable-string",
        "numeric-int-string",
        "decimal-int-string",
        "non-decimal-int-string",
        "non-empty-mixed",
        "non-null-mixed",
        "non-empty-scalar",
        "open-resource",
        "closed-resource",
        "never-return",
        "no-return",
        "callable-object",
        "lowercase-string",
        "uppercase-string",
        "uncased-string",
        "non-empty-lowercase-string",
        "non-falsy-uppercase-string",
        "non-falsy-numeric-string",
        "numeric-uncased-string",
        "int-range<0, 255>",
        "int-range",
        "non-empty-array<string, int>",
        "non-empty-list<int>",
        "associative-array<string, int>",
        "non-empty-associative-array<string, int>",
        "class-string<Foo>",
    ] {
        let src = format!("<?php\n/** @param {ty} $s */\nfunction f($s): void {{}}\n");
        assert_eq!(vocab(&src), Vec::<String>::new(), "convicted builtin vocabulary: {ty}");
    }
}

/// A name PHP could actually carry is not this id's business, whether or not
/// the index can see it. That is the class-reference question, and it keeps the
/// silence ADR-0091 §5 says it must: the name could be a class, a `@template`
/// name, or an alias, and none of the three can be ruled out.
#[test]
fn a_hyphen_free_name_is_never_reported() {
    for ty in ["Foo", "\\App\\Missing", "NonEmpyString", "TValue", "int", "self"] {
        let src = format!("<?php\n/** @param {ty} $s */\nfunction f($s): void {{}}\n");
        assert_eq!(vocab_count(&src), 0, "{ty}");
    }
}

/// **The `@template` shadow decides first** (ADR-0091 §4), and here it does so
/// by construction: the tag scanner reads a template name with `is_ident_byte`,
/// which excludes `-`, so no shadow set can hold a hyphenated name and every
/// hyphenated identifier that reaches the check has survived one.
///
/// `@template T-of-X` therefore declares a template named `T`, and `@param
/// T-of-X` names nothing the docblock declared — which is exactly §4.1's
/// ruling read forward: a name in the reserved space is a refusal, never a
/// declaration, so the reference to it reports. A scanner that later admitted
/// `-` would fail this test before it changed any behaviour silently.
#[test]
fn a_truncated_template_name_does_not_shadow() {
    let shadowed = "<?php\n/** @template T\n * @param T $s */\nfunction f($s): void {}\n";
    assert_eq!(vocab_count(shadowed), 0, "a hyphen-free template name shadows as it always did");

    let reserved = "<?php\n/** @template T-of-X\n * @param T-of-X $s */\n\
        function f($s): void {}\n";
    assert_eq!(vocab_count(reserved), 1, "a hyphenated template name declares nothing (§4.1)");
}

/// A payload the parser rejects is `phpdoc.unparsable`'s finding and not two:
/// this id reads the same parsed type the envelopes are lowered from, so an
/// annotation that declares nothing at all is never also convicted of naming
/// the wrong thing.
#[test]
fn an_unparsable_payload_is_not_this_ids_finding() {
    let src = "<?php\n/** @param |foo-bar $s */\nfunction f($s): void {}\n";
    assert_eq!(vocab_count(src), 0);
}

/// **#478's guarantee is not weakened** (ADR-0091 §6): the id adds a finding
/// and removes none. The same sources, judged with and without the id in play —
/// the value judgment is `Opaque`/`Maybe` either way, so every *other* finding
/// is identical and the reported docblock still rejects nothing.
///
/// Read as a partition rather than by running the engine twice, which is the
/// same claim and the only one available: the id has no switch inside the
/// engine, so "disabled" is exactly "the rest of the findings".
#[test]
fn the_value_judgment_is_identical_with_the_id_and_without_it() {
    // A call whose argument the pre-#478 class reading answered a definite `No`
    // for. It must stay silent, and the docblock must be reported exactly once.
    let src = "<?php\n/** @param non-empy-string $s */\nfunction f($s): void {}\nf(\"x\");\n";
    let ds = findings(src);
    let (mine, rest): (Vec<&Diagnostic>, Vec<&Diagnostic>) =
        ds.iter().partition(|d| d.id == PHPDOC_UNKNOWN_VOCABULARY_ID);
    assert_eq!(mine.len(), 1);
    assert!(
        !rest.iter().any(|d| d.id == PARAM_MISMATCH_ID),
        "the spelling still admits every value as Maybe: {rest:?}",
    );

    // The same on the return side, and with a value of a different base type —
    // a manufactured `No` would have convicted both.
    let ret = "<?php\n/** @return positive-integer */\nfunction f() { return 1; }\n";
    let rs = findings(ret);
    assert_eq!(rs.iter().filter(|d| d.id == PHPDOC_UNKNOWN_VOCABULARY_ID).count(), 1);
    assert!(!rs.iter().any(|d| d.id == RETURN_MISMATCH_ID), "{rs:?}");
}
