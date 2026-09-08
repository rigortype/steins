//! Issue #472 — `@phpstan-type` / `@psalm-type` aliases resolve where envelopes
//! are built, so an alias names a type instead of only recording that a name was
//! declared.
//!
//! One rewrite over the parsed type, no second evaluator and no `ContractTy`
//! variant (ADR-0030's one-relation discipline): everything below is measured
//! through surfaces that already existed — the declared-contract relation
//! (`phpdoc.param-mismatch`) and the phpdoc dump (ADR-0053 D3).
//!
//! The dump is what pins the **floors**, and it is the sharper instrument of the
//! two. An unresolved alias that reached [`steins_contract::lower`]'s class
//! catch-all would dump as a lowercased class name and answer a definite `No` for
//! every non-object value the moment `Cx::is_known_class`'s valve stopped holding
//! it back; `no declared contract` is the `Opaque` floor a `@template` name gets,
//! and the only acceptable answer for a name Steins could not resolve.

use steins_infer::{DEBUG_PHPDOC_TYPE_ID, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

/// The `debug.phpdoc-type` message bodies a source produces, in source order.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID)
        .map(|d| d.message.clone())
        .collect()
}

/// The single dumped phpdoc type of a one-dump source.
fn one_dump(src: &str) -> String {
    let ds = dumps(src);
    assert_eq!(ds.len(), 1, "expected exactly one phpdoc dump, got {ds:?}");
    ds[0].clone()
}

/// The `phpdoc.param-mismatch` count of a source.
fn param_count(src: &str) -> usize {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php").into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).count()
}

/// A class declaring `$body` as its docblock, with one method that dumps its
/// `@param $ty`.
fn probe(body: &str, ty: &str) -> String {
    format!(
        "<?php\n/**\n{body}\n */\nclass Probe {{\n\
         /** @param {ty} $v */\n\
         public function m($v): void {{ \\PHPStan\\dumpPhpDocType($v); }}\n}}\n"
    )
}

// 1. The local alias (#472 scope 1).

#[test]
fn local_alias_expands_where_it_is_used() {
    // Round-trip: the resolved node spells back as the type it names, not as the
    // alias. On master this dumped `userrow` — the normalized class spelling.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type UserRow array{id: int, name: string}", "UserRow")),
        "dumped phpdoc type: array{id: int, name: string} (asserted)"
    );
}

#[test]
fn local_alias_is_enforced_at_a_call_site() {
    // The whole point: the alias is judged as the shape it names, by the relation
    // that already judges a spelled shape (nothing in ADR-0030 relation #1 moves).
    let src = "<?php\n/** @phpstan-type UserRow array{id: int, name: string} */\n\
        class Repo {\n /** @param UserRow $row */\n public function save($row): void {}\n}\n\
        $r = new Repo();\n";
    assert_eq!(param_count(&format!("{src}$r->save(['id' => 1, 'name' => 'Ada']);")), 0);
    assert_eq!(param_count(&format!("{src}$r->save(['id' => 1]);")), 1, "`name` is missing");
}

#[test]
fn psalm_equals_spelling_reads_as_one_dialect() {
    // Psalm writes `Name = <type>`, PHPStan writes `Name <type>`; both prefixes
    // reach both spellings (ADR-0029 governs the prefix, not the punctuation).
    assert_eq!(
        one_dump(&probe(" * @psalm-type Row = array{id: int}", "Row")),
        "dumped phpdoc type: array{id: int} (asserted)"
    );
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row = array{id: int}", "Row")),
        "dumped phpdoc type: array{id: int} (asserted)"
    );
}

#[test]
fn an_alias_resolves_inside_a_composite() {
    // The rewrite is a walk, not a top-level swap.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{id: int}", "list<Row>")),
        "dumped phpdoc type: list<array{id: int}> (asserted)"
    );
}

#[test]
fn a_qualified_reference_is_never_an_alias() {
    // The `\`-qualified rule the `@template` shadow already keeps: `\Row` names a
    // class, whatever the docblock aliases.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{id: int}", "\\Row")),
        "dumped phpdoc type: row (asserted)"
    );
}

// 2. The imported alias (#472 scope 2).

/// `Geo` exports `Coordinates`; `Map` imports it and uses it, `$import` spelling
/// its import tag.
fn import_probe(import: &str, used: &str) -> String {
    format!(
        "<?php\n/** @phpstan-type Coordinates array{{lat: float, lng: float}} */\n\
         class Geo {{}}\n/** {import} */\nclass Map {{\n\
         /** @param {used} $v */\n\
         public function pin($v): void {{ \\PHPStan\\dumpPhpDocType($v); }}\n}}\n"
    )
}

#[test]
fn imported_alias_resolves_through_the_owners_table() {
    assert_eq!(
        one_dump(&import_probe("@phpstan-import-type Coordinates from Geo", "Coordinates")),
        "dumped phpdoc type: array{lat: float, lng: float} (asserted)"
    );
    assert_eq!(
        one_dump(&import_probe("@psalm-import-type Coordinates from Geo", "Coordinates")),
        "dumped phpdoc type: array{lat: float, lng: float} (asserted)"
    );
}

#[test]
fn an_import_can_rename_what_it_imports() {
    assert_eq!(
        one_dump(&import_probe("@phpstan-import-type Coordinates from Geo as Point", "Point")),
        "dumped phpdoc type: array{lat: float, lng: float} (asserted)"
    );
}

#[test]
fn an_unresolvable_import_owner_floors_and_never_becomes_a_class() {
    // The finding this slice upgrades from hygiene to a fix: on master this dumped
    // `coords (asserted)`, i.e. `ContractTy::Class` over a name that is not a
    // class, one `is_known_class` valve away from a manufactured `No`.
    assert_eq!(
        one_dump(&import_probe("@phpstan-import-type Coords from NoSuchClass", "Coords")),
        "dumped phpdoc type: no declared contract"
    );
}

#[test]
fn an_import_of_a_name_the_owner_does_not_declare_floors() {
    assert_eq!(
        one_dump(&import_probe("@phpstan-import-type Missing from Geo", "Missing")),
        "dumped phpdoc type: no declared contract"
    );
}

#[test]
fn an_import_with_no_from_clause_floors() {
    // A malformed tail still *declares the name*, so it must floor rather than fall
    // out of the table and back to the class catch-all.
    assert_eq!(
        one_dump(&import_probe("@phpstan-import-type Coordinates", "Coordinates")),
        "dumped phpdoc type: no declared contract"
    );
}

#[test]
fn an_import_of_an_import_floors() {
    // One hop across the class boundary, not a chain: re-exporting an import is the
    // walk ADR-0032's amendment declines for `template-type`.
    let src = "<?php\n/** @phpstan-type Row array{id: int} */\nclass A {}\n\
        /** @phpstan-import-type Row from A */\nclass B {}\n\
        /** @phpstan-import-type Row from B */\nclass C {\n\
        /** @param Row $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n";
    assert_eq!(one_dump(src), "dumped phpdoc type: no declared contract");
}

// 3. One level of alias-names-alias, and the cycle floor (#472 scope 3).

#[test]
fn an_alias_body_naming_another_alias_resolves_one_level() {
    assert_eq!(
        one_dump(&probe(
            " * @phpstan-type Row array{id: int}\n * @phpstan-type Rows list<Row>",
            "Rows"
        )),
        "dumped phpdoc type: list<array{id: int}> (asserted)"
    );
}

#[test]
fn a_second_level_of_alias_naming_alias_floors() {
    // The bound is stated, not discovered: `A -> B -> C` stops at `C`, and the name
    // still standing there floors rather than reaching the class catch-all.
    assert_eq!(
        one_dump(&probe(
            " * @phpstan-type C array{id: int}\n * @phpstan-type B C\n * @phpstan-type A B",
            "A"
        )),
        "dumped phpdoc type: no declared contract"
    );
}

#[test]
fn an_alias_cycle_floors_and_terminates() {
    // Termination is the assertion — the test hanging is the failure mode.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type A B\n * @phpstan-type B A", "A")),
        "dumped phpdoc type: no declared contract"
    );
    assert_eq!(
        one_dump(&probe(" * @phpstan-type A A", "A")),
        "dumped phpdoc type: no declared contract"
    );
}

#[test]
fn an_unparsable_alias_body_floors() {
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{id:", "Row")),
        "dumped phpdoc type: no declared contract"
    );
}

// 4. Precedence: what an alias name loses to.

#[test]
fn an_in_project_class_wins_over_a_same_named_alias() {
    // The pseudo-type/class precedence question again, with the same answer: the
    // declaration wins, and the identifier is left exactly as written.
    let src = "<?php\nclass Row {}\n/** @phpstan-type Row array{id: int} */\nclass Probe {\n\
        /** @param Row $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n";
    assert_eq!(one_dump(src), "dumped phpdoc type: Row (asserted)");
}

#[test]
fn built_in_vocabulary_cannot_be_aliased() {
    // phpstan-src's own `type-aliases.php` writes `@phpstan-type int
    // ShouldNotHappen` and then asserts that `@param int` is still `int`. A name
    // no class could shadow is a name no alias may bind either — one predicate,
    // `is_shadowable_pseudo_type`, answers both.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type int ShouldNotHappen", "int")),
        "dumped phpdoc type: int (asserted)"
    );
    assert_eq!(
        one_dump(&probe(" * @phpstan-type array array{id: int}", "array")),
        "dumped phpdoc type: array (asserted)"
    );
}

#[test]
fn a_template_name_is_not_captured_by_a_same_named_alias() {
    // The ordering claim, from the outside: aliases expand *after* both `@template`
    // shadow stages, so by then a declared template name is no longer an identifier
    // for the table to match. Class-level template…
    assert_eq!(
        one_dump(&probe(" * @template T\n * @phpstan-type T array{x: int}", "T")),
        "dumped phpdoc type: no declared contract"
    );
    // …and the member's own, shadowed all the way back in `parse_envelopes`.
    let src = "<?php\n/** @phpstan-type T array{x: int} */\nclass Probe {\n\
        /** @template T\n * @param T $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n";
    assert_eq!(one_dump(src), "dumped phpdoc type: no declared contract");
}

#[test]
fn an_alias_body_still_gets_the_declaring_classs_template_treatment() {
    // The body is docblock text of the declaring class-like, so the rewrites it
    // owes are applied there rather than skipped: a class-level `T` inside a body
    // must not survive the splice as a class named `T`.
    assert_eq!(
        one_dump(&probe(" * @template T\n * @phpstan-type Rows list<T>", "Rows")),
        // `mixed` is how the shadow's opaque node renders in a nested position —
        // the point is that it is not `list<t>`, a list of a class named `T`.
        "dumped phpdoc type: list<mixed> (asserted)"
    );
}

#[test]
fn an_alias_body_resolves_its_own_template_type_node() {
    // `template-type<…>` is resolved by `Cx::envelopes_of`, which ran before the
    // alias was spliced in; the body gets its own pass at its declaring site.
    let src = "<?php\n/** @template T */\nclass Box {}\n\
        /** @phpstan-type Unboxed template-type<Box<int>, Box, 'T'> */\nclass Probe {\n\
        /** @param Unboxed $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n";
    assert_eq!(one_dump(src), "dumped phpdoc type: int (asserted)");
}

#[test]
fn an_imported_body_keeps_naming_the_classes_it_named() {
    // A body written against the owner's `use` scope is qualified before it travels
    // (issue #361's `qualify_class_names`), so an import does not silently re-point
    // a class name at whatever the importing file imported.
    let src = "<?php\nnamespace Vendor;\nclass Row {}\n\
        /** @phpstan-type Rows list<Row> */\nclass Geo {}\n";
    let user = "namespace App;\nclass Row {}\n\
        /** @phpstan-import-type Rows from \\Vendor\\Geo */\nclass Probe {\n\
        /** @param Rows $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n";
    assert_eq!(
        one_dump(&format!("{src}{user}")),
        "dumped phpdoc type: list<vendor\\row> (asserted)"
    );
}

// 5. What stays where it was.

#[test]
fn a_name_no_class_like_aliases_is_unchanged() {
    // The negative control for the whole slice: without a `@phpstan-type` line the
    // same `@param` still reads as a class, exactly as before.
    assert_eq!(
        one_dump(&probe(" * nothing here", "UserRow")),
        "dumped phpdoc type: userrow (asserted)"
    );
}

#[test]
fn an_alias_is_not_in_force_outside_the_class_like_that_declares_it() {
    // #472 scope 1 is "on a class-like, used within that class-like". A free
    // function's `@param` is a bounded gap, recorded in
    // `docs/type-specification/not-implemented.md`, not an accident.
    let src = "<?php\n/** @phpstan-type Row array{id: int} */\nclass Owner {}\n\
        /** @param Row $v */\nfunction m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n";
    assert_eq!(one_dump(src), "dumped phpdoc type: row (asserted)");
}
