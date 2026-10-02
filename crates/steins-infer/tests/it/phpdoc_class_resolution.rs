//! Issue #699 — a class a docblock names resolves against the docblock's own
//! namespace wherever it stands in the type, and a name written fully qualified
//! stays fully qualified.
//!
//! Two defects, one section each:
//!
//! * **A class nested in another type** (`list<User>`, `array<string, User>`,
//!   `iterable<User>`, a shape field, an intersection inside an element) used to
//!   be left as written, so in `namespace App` it read as the global `user`, and
//!   the element a `foreach` bound was that global class.
//! * **A leading `\`** used to be dropped when the docblock type was lowered, and
//!   the bare name was then resolved as relative: `@param \Foo|false` in
//!   `namespace App` read as `App\Foo`, and a call on it reported a missing method
//!   of `App\Foo`, a class the docblock never named.

use steins_infer::{
    CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, Folder, PHPDOC_UNDEFINED_METHOD_ID,
    check, check_with,
};
use steins_syntax::SourceTree;

/// Every `debug.type` body in `src`, in source order.
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check(&tree, &functions, "test.php");
    ds.into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// A ready boot surface: the absence family is available and no project class is
/// a runtime homonym, so the declared-receiver lane is free to fire.
struct Boot;

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

/// The declared-receiver findings, whichever id they were routed to.
fn undefined_method(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| d.id == PHPDOC_UNDEFINED_METHOD_ID || d.id == CALL_UNDEFINED_METHOD_ID)
        .collect()
}

// ---------------------------------------------------------------------------
// A class nested in another type
// ---------------------------------------------------------------------------

#[test]
fn a_list_element_class_resolves_in_the_docblock_namespace() {
    assert_eq!(
        types(
            "<?php\nnamespace App;\nfinal class User {}\n\
             /** @param list<User>|false $l */\n\
             function f($l): void {\n\
             \\PHPStan\\dumpType($l);\n\
             if ($l === false) { return; }\n\
             foreach ($l as $u) { \\PHPStan\\dumpType($u); }\n}\n"
        ),
        ["false|list<App\\User> (asserted)", "App\\User (asserted)"]
    );
}

#[test]
fn every_nested_class_position_resolves() {
    // A map value, a shape field and an intersection inside an element.
    assert_eq!(
        types(
            "<?php\nnamespace App;\ninterface Named {}\nfinal class User implements Named {}\n\
             /**\n * @param array<string, User> $a\n\
             * @param array{u: User, n: list<User&Named>} $s\n */\n\
             function f($a, $s): void {\n\
             \\PHPStan\\dumpType($a);\n\\PHPStan\\dumpType($s);\n}\n"
        ),
        [
            "array<string, App\\User> (asserted)",
            "array{n: list<App\\User&App\\Named>, u: App\\User} (asserted)",
        ]
    );
}

// ---------------------------------------------------------------------------
// A fully-qualified name
// ---------------------------------------------------------------------------

#[test]
fn a_fully_qualified_class_is_not_re_resolved_as_relative() {
    assert_eq!(
        types(
            "<?php\nnamespace App;\nclass Foo {}\n\
             /**\n * @param \\Foo|false $ff\n * @param \\DateTime|int $d\n\
             * @param list<\\Foo> $l\n */\n\
             function f($ff, $d, $l): void {\n\
             \\PHPStan\\dumpType($ff);\n\\PHPStan\\dumpType($d);\n\\PHPStan\\dumpType($l);\n}\n"
        ),
        // The global `Foo` is declared nowhere, so it prints as stored (lowercased)
        // rather than in a declaration's casing; `DateTime` recovers php-src's.
        ["foo|false (asserted)", "int|DateTime (asserted)", "list<foo> (asserted)"]
    );
}

#[test]
fn an_imported_alias_keeps_naming_the_owners_class() {
    // An imported alias body is qualified against its owner's scope before it
    // travels (`\Vendor\Row`), so the importer's namespace must not re-resolve
    // it. The bare arm used to read `app\vendor\row`, the dropped `\` letting
    // `namespace App` prefix itself onto an already-qualified name.
    assert_eq!(
        types(
            "<?php\nnamespace Vendor;\nclass Row {}\n\
             /**\n * @phpstan-type One Row\n * @phpstan-type Rows list<Row>\n */\nclass Geo {}\n\
             namespace App;\nclass Row {}\n\
             /**\n * @phpstan-import-type One from \\Vendor\\Geo\n\
             * @phpstan-import-type Rows from \\Vendor\\Geo\n */\nclass Probe {\n\
             /**\n * @param One $o\n * @param Rows $l\n */\n\
             public function m($o, $l): void { \\PHPStan\\dumpType($o); \\PHPStan\\dumpType($l); }\n}\n"
        ),
        ["Vendor\\Row (asserted)", "list<Vendor\\Row> (asserted)"]
    );
}

#[test]
fn a_fully_qualified_receiver_never_reports_a_method_of_the_namespaced_homonym() {
    // Only `App\Foo` is declared, and it is final and lacks `nope()`. The docblock
    // names the global `\Foo`, which the project does not declare: nothing is
    // known about its methods, so the call is silent.
    let src = "<?php\nnamespace App;\nfinal class Foo {}\n\
               /** @param \\Foo|null $ff */\n\
               function f($ff): void { if ($ff !== null) { $ff->nope(); } }\n";
    assert_eq!(undefined_method(src), Vec::<Diagnostic>::new());
    // The control: written relative, the same docblock does name `App\Foo`.
    let relative = src.replace("@param \\Foo|null", "@param Foo|null");
    let d = undefined_method(&relative);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("Foo::nope()"), "{}", d[0].message);
}
