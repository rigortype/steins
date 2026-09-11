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

/// How many findings of one id a source produces.
fn ids(src: &str, id: &str) -> usize {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php").into_iter().filter(|d| d.id == id).count()
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
fn an_alias_wins_over_a_same_named_class() {
    // PHPStan's order, which #472 shipped inverted (issue #670).
    // `ClassReflection::getTypeAliases` merges `array_merge($imported, $local)`
    // and `TypeNodeResolver::resolveIdentifierTypeNode` consults the alias map
    // before the class one, so a declared alias always wins. On the class-first
    // reading this dumped `Row (asserted)`.
    let src = "<?php\nclass Row {}\n/** @phpstan-type Row array{id: int} */\nclass Probe {\n\
        /** @param Row $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n";
    assert_eq!(one_dump(src), "dumped phpdoc type: array{id: int} (asserted)");
}

#[test]
fn the_collision_no_longer_convicts_a_value_the_alias_admits() {
    // Why the order moved rather than being registered. Divergence-registry entry
    // 18 carried this as the one row in that section that was **not** a silence:
    // the class-first tie-break answered a definite `No` on a call the oracle
    // accepts (PHPStan reports the collision as `typeAlias.duplicate` and then
    // resolves the alias anyway). A conviction the oracle admits is not a
    // divergence to register, so the order changed and the two paragraphs
    // recording it are gone.
    let src = "<?php\nclass Foo {}\n/** @phpstan-type Foo array{x: int} */\nclass Probe {\n\
        /** @param Foo $v */\n\
        public function m($v): void {}\n}\n\
        $p = new Probe();\n$p->m(['x' => 1]);\n";
    assert_eq!(param_count(src), 0, "the shape the alias names is admitted");
    assert_eq!(ids(src, "type.argument-mismatch"), 0, "and the proof lane says nothing either");
    // That pair is the shape the issue asks for, and on its own it proves less
    // than it looks: an array literal against a *class* contract was already
    // `Maybe`, so the class-first reading was silent here too. The registry's own
    // example is the sensitive one — a scalar is a definite non-member of a class,
    // so `m(1)` against `@phpstan-type Row int` beside a class `Row` was the
    // `phpdoc.param-mismatch` PHPStan accepts, and it is the row that moved.
    let scalar = "<?php\nclass Row {}\n/** @phpstan-type Row int */\nclass Probe {\n\
        /** @param Row $v */\n\
        public function m($v): void {}\n}\n\
        $p = new Probe();\n$p->m(1);\n";
    assert_eq!(param_count(scalar), 0, "the alias admits `1`, and so does the oracle");
}

#[test]
fn an_alias_is_looked_up_by_its_spelling_not_its_case() {
    // `NameScope::hasTypeAlias` is `array_key_exists($alias, …)` on the exact
    // spelling (issue #670), so beside `class Row {}` an alias `Row` leaves
    // `@param row` naming the class — PHPStan says `class.nameCase` and resolves
    // `App\row` — and only the same-spelled `@param Row` is the alias. Keyed by
    // a case fold, `row` was the alias too, and with the alias now winning the
    // collision `m(new Row())` was convicted against `int`, a `No` the oracle
    // never gives. Both directions, because the fold hid both.
    let src = "<?php\nnamespace App;\nclass Row {}\n/** @phpstan-type Row int */\nclass Probe {\n\
        /** @param row $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n\
        $p = new Probe();\n$p->m(new Row());\n";
    assert_eq!(one_dump(src), "dumped phpdoc type: App\\Row (asserted)");
    assert_eq!(param_count(src), 0, "`row` is the class, which `new Row()` inhabits");
    // The other way round: a lower-case alias does not capture the class-cased
    // spelling either, and the exact spelling still is the alias.
    let mirror = "<?php\nnamespace App;\nclass Row {}\n/** @phpstan-type row int */\nclass Probe {\n\
        /** @param Row $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n\
        /** @param row $v */\n\
        public function n($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n\
        $p = new Probe();\n$p->m(new Row());\n$p->n(new Row());\n";
    assert_eq!(
        dumps(mirror),
        ["dumped phpdoc type: App\\Row (asserted)", "dumped phpdoc type: int (asserted)"]
    );
    assert_eq!(param_count(mirror), 1, "only `n`, whose `row` is the alias, rejects the object");
}

#[test]
fn built_in_vocabulary_cannot_be_aliased() {
    // phpstan-src's own `type-aliases.php` writes `@phpstan-type int
    // ShouldNotHappen` and then asserts that `@param int` is still `int`.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type int ShouldNotHappen", "int")),
        "dumped phpdoc type: int (asserted)"
    );
    assert_eq!(
        one_dump(&probe(" * @phpstan-type array array{id: int}", "array")),
        "dumped phpdoc type: array (asserted)"
    );
    // The reserved words are the easy half. These three are **not** reserved —
    // a class may be named `Integer`, `Number` or `List`, so the shadowing
    // predicate says `true` for all three — and binding them anyway was a
    // manufactured `No`: PHPStan rejects the alias name and keeps reading
    // `@param integer` as `int`, where Steins convicted `1` against
    // `array{x: int}`. The declaration question is "does the vocabulary already
    // own this name", which is `is_type_vocabulary`.
    for (name, spelling) in
        [("integer", "int"), ("number", "int|float"), ("list", "list<mixed>")]
    {
        assert_eq!(
            one_dump(&probe(&format!(" * @phpstan-type {name} array{{x: int}}"), name)),
            format!("dumped phpdoc type: {spelling} (asserted)"),
            "`{name}` is vocabulary and must not be rebound by an alias"
        );
    }
}

#[test]
fn a_redeclared_name_takes_its_phpstan_body_whatever_the_order() {
    // PHPStan's `PhpDocNodeResolver` reads the `@psalm-` spellings into the alias
    // map and lets the `@phpstan-` ones overwrite, so the prefix decides and the
    // source order does not. Two DIFFERENT bodies are what makes the rule
    // observable at all — `aliases_local_type` spells an agreeing pair.
    assert_eq!(
        one_dump(&probe(
            " * @psalm-type Row = string\n * @phpstan-type Row int",
            "Row"
        )),
        "dumped phpdoc type: int (asserted)"
    );
    // Same pair, opposite order: still the `@phpstan-` body.
    assert_eq!(
        one_dump(&probe(
            " * @phpstan-type Row int\n * @psalm-type Row = string",
            "Row"
        )),
        "dumped phpdoc type: int (asserted)"
    );
    // Within one prefix the later line wins, the same overwrite upstream does.
    assert_eq!(
        one_dump(&probe(
            " * @phpstan-type Row int\n * @phpstan-type Row string",
            "Row"
        )),
        "dumped phpdoc type: string (asserted)"
    );
}

#[test]
fn an_alias_body_wrapped_across_lines_is_reassembled() {
    // A one-line scanner is safe for `@param` by accident (a wrapped one loses
    // its `$name` and is dropped) and unsafe here: an alias tail has no trailing
    // anchor, so a wrap at `|` leaves a first line that is itself a valid type.
    // Reading it as the whole body bound `int` for a `int|string` the author
    // wrote — narrower than the declaration, i.e. a manufactured `No`.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row int\n *   |string", "Row")),
        "dumped phpdoc type: int|string (asserted)"
    );
    // The other wrap a line scanner can see: the tail is still unclosed.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{id: int,\n *   name: string}", "Row")),
        "dumped phpdoc type: array{id: int, name: string} (asserted)"
    );
    // `'str'` is admitted by the reassembled union — the conviction this test
    // exists to prevent.
    assert_eq!(
        param_count(&format!(
            "{}\n(new Probe())->m('str');\n",
            probe(" * @phpstan-type Row int\n *   |string", "Row")
        )),
        0
    );
    // The invariant, stated independently of what any one body lowers to: a
    // wrapped declaration and the same declaration on one line agree. An
    // intersection of two shapes floors either way — that is the body's answer,
    // not the scanner's, and before the join the two spellings disagreed.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{id: int}\n *   &array{x: int}", "Row")),
        one_dump(&probe(" * @phpstan-type Row array{id: int}&array{x: int}", "Row"))
    );
    // A wrap that upstream declares invalid (a dangling `|`) floors rather than
    // being completed across it.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row int|\n *   string", "Row")),
        "dumped phpdoc type: no declared contract"
    );
    // Prose after a finished body is not part of the type.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row int\n *\n * Some prose.", "Row")),
        "dumped phpdoc type: int (asserted)"
    );
}

#[test]
fn a_bracket_inside_a_shape_key_does_not_decide_where_the_body_ends() {
    // Issue #666. The continuation rule counts brackets, and a shape key is a
    // string literal that may spell one. Both repros are the same defect read in
    // opposite directions, and both are what `is_unclosed` now refuses to read.
    //
    // The `>` cancelled the `{`, so the body looked finished and bound the half
    // before the wrap — the narrowing this whole join exists to prevent.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{a: 'x>',\n *   b: int}", "Row")),
        "dumped phpdoc type: array{a: 'x>', b: int} (asserted)"
    );
    assert_eq!(
        param_count(&format!(
            "{}\n(new Probe())->m(['a' => 'x>', 'b' => 1]);\n",
            probe(" * @phpstan-type Row array{a: 'x>',\n *   b: int}", "Row")
        )),
        0,
        "the shape the author wrote across the wrap admits its own value"
    );
    // And the other way: the `{` in the key left a finished shape looking open,
    // so the prose below joined it and the whole declaration floored.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{'{': int}\n * Some prose.", "Row")),
        one_dump(&probe(" * @phpstan-type Row array{'{': int}", "Row"))
    );
    // Spelled out as well as paired, because two floors also agree: what the
    // prose used to cost was the whole declaration.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{'{': int}\n * Some prose.", "Row")),
        "dumped phpdoc type: array{'{': int} (asserted)"
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

#[test]
fn an_imported_body_keeps_naming_a_class_the_owner_could_not_resolve_either() {
    // The other half of the same rule (issue #665), and the half #472 got wrong:
    // a name the *owner's* scope cannot resolve is an unknown class in the
    // owner's namespace, never a class the importer happens to have. Qualifying
    // only known classes left `Thing` relative, so it resolved again on arrival
    // and this dumped `list<app\thing>` — a definite contract over a class
    // `Vendor\Geo` never named. Upstream cannot make the mistake: `TypeAlias`
    // carries the owner's `NameScope`, and an unknown identifier resolves through
    // it to `Vendor\Thing`.
    let src = "<?php\nnamespace Vendor;\n/** @phpstan-type Rows list<Thing> */\nclass Geo {}\n";
    let user = "namespace App;\nclass Thing {}\n\
        /** @phpstan-import-type Rows from \\Vendor\\Geo */\nclass Probe {\n\
        /** @param Rows $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n";
    assert_eq!(
        one_dump(&format!("{src}{user}")),
        "dumped phpdoc type: list<vendor\\thing> (asserted)"
    );
}

#[test]
fn an_importers_own_class_does_not_answer_for_a_name_the_owner_left_unresolved() {
    // The same defect at the relation that pays for it. `Vendor\Geo` names
    // `Thing`, which does not exist in `Vendor`; the importing file has an
    // `App\Thing` that has nothing to do with it. A relative name arriving here
    // resolved to `App\Thing`, which IS a known class, so the contract stopped
    // being silent and convicted every other object — a manufactured definite
    // `No`, the outcome issue #472 exists to prevent. Qualified in the owner's
    // scope the name is an unknown class and `Cx::is_known_class`'s valve holds.
    let src = "<?php\nnamespace Vendor;\n/** @phpstan-type Row Thing */\nclass Geo {}\n";
    let user = "namespace App;\nclass Thing {}\nclass Other {}\n\
        /** @phpstan-import-type Row from \\Vendor\\Geo */\nclass Probe {\n\
        /** @param Row $v */\n\
        public function m($v): void {}\n}\n\
        $p = new Probe();\n$p->m(new Other());\n";
    assert_eq!(param_count(&format!("{src}{user}")), 0);
}

#[test]
fn a_range_bound_is_not_a_class_name_to_qualify() {
    // Qualifying every non-vocabulary identifier (issue #665) must stop at the
    // bound position of `int<…>`: `min`/`max` are words `lower_int_range` reads
    // by spelling, as upstream's `TypeNodeResolver` does, and `\App\max` is no
    // bound. Spelled that way the range floored to `Opaque`, and a local body —
    // `expand_alias` qualifies those too — dumped `no declared contract` and
    // admitted `-1` against `int<1, max>`.
    let src = "<?php\nnamespace App;\n/**\n * @phpstan-type Pos int<1, max>\n\
         * @phpstan-type Neg int<min, -1>\n */\nclass Probe {\n\
        /** @param Pos $v */\n\
        public function m($v): void { \\PHPStan\\dumpPhpDocType($v); }\n\
        /** @param Neg $v */\n\
        public function n($v): void { \\PHPStan\\dumpPhpDocType($v); }\n}\n\
        $p = new Probe();\n$p->m(-1);\n$p->n(1);\n";
    assert_eq!(
        dumps(src),
        ["dumped phpdoc type: int<1, max> (asserted)", "dumped phpdoc type: int<min, -1> (asserted)"]
    );
    assert_eq!(param_count(src), 2, "-1 is below `int<1, max>` and 1 above `int<min, -1>`");
}

#[test]
fn an_imported_bodys_const_fetch_names_the_owners_class() {
    // Issue #665's acceptance criterion, at the one node the identifier walk
    // did not reach: the class of a const fetch. `key-of<Geo::MAP>` written in
    // `Vendor` resolves its `Geo` where the operand is read
    // (`const_operand_shape`), so left relative it found the importer's
    // `App\Geo::MAP = ['b' => 2]` and convicted `'a'`, the key the owner's map
    // has. PHPStan dumps `'a'` and accepts.
    let src = "<?php\nnamespace Vendor;\n\
        /**\n * @phpstan-type Ko key-of<Geo::MAP>\n * @phpstan-type Vo value-of<Geo::MAP>\n */\n\
        class Geo { const MAP = ['a' => 1]; }\n";
    let user = "namespace App;\nclass Geo { const MAP = ['b' => 2]; }\n\
        /**\n * @phpstan-import-type Ko from \\Vendor\\Geo\n\
         * @phpstan-import-type Vo from \\Vendor\\Geo\n */\nclass Probe {\n\
        /** @param Ko $v */\n\
        public function ko($v): void {}\n\
        /** @param Vo $v */\n\
        public function vo($v): void {}\n}\n\
        $p = new Probe();\n";
    let with = |calls: &str| format!("{src}{user}{calls}");
    assert_eq!(param_count(&with("$p->ko('a');\n$p->vo(1);\n")), 0, "the owner's key and value");
    // Not a floor: the owner's map is what judges, so the importer's own key is
    // the one rejected.
    assert_eq!(param_count(&with("$p->ko('b');\n$p->vo(2);\n")), 2, "the importer's key and value");
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

#[test]
fn a_blank_gutter_line_does_not_end_a_wrapped_body() {
    // `TypeParser::parse` consumes every `PHPDOC_EOL` after an atomic before it
    // looks for `|`, blank lines included, so this is one union upstream.
    // Stopping at the blank bound `int` and convicted every string.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row int\n *\n *   |string", "Row")),
        "dumped phpdoc type: int|string (asserted)"
    );
    // Inside brackets too.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row array{id: int,\n *\n *   name: string}", "Row")),
        "dumped phpdoc type: array{id: int, name: string} (asserted)"
    );
    // The closer still ends it — an empty line and `*/` both leave nothing on
    // the line, and they are not the same answer.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row int", "Row")),
        "dumped phpdoc type: int (asserted)"
    );
}

#[test]
fn a_local_declaration_beats_an_import_of_the_same_name() {
    // `ClassReflection` merges as `array_merge($imported, $local)`, so the local
    // wins whatever the prefixes or the line order. Keyed on dialect alone, an
    // import beat a local and convicted `1` against the imported `string`.
    let src = |tags: &str| {
        format!(
            "<?php\n/** @phpstan-type Row string */\nclass Owner {{}}\n/**\n{tags}\n */\nclass Probe {{\n\
             /** @param Row $v */\n\
             public function m($v): void {{ \\PHPStan\\dumpPhpDocType($v); }}\n}}\n"
        )
    };
    assert_eq!(
        one_dump(&src(" * @phpstan-import-type Row from Owner\n * @psalm-type Row = int")),
        "dumped phpdoc type: int (asserted)"
    );
    assert_eq!(
        one_dump(&src(" * @phpstan-type Row int\n * @phpstan-import-type Row from Owner")),
        "dumped phpdoc type: int (asserted)"
    );
}

#[test]
fn a_body_with_a_trailing_remainder_declares_nothing() {
    // `parseTypeAliasTagValue` requires the type to run to the end of the tag —
    // unlike `@return int description`, an alias body has no description slot.
    // Prefix-parsing bound `int` and rejected what the alias admits.
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row int the row", "Row")),
        "dumped phpdoc type: no declared contract"
    );
    assert_eq!(
        one_dump(&probe(" * @phpstan-type Row int\n *   |string the second half", "Row")),
        "dumped phpdoc type: no declared contract"
    );
}
