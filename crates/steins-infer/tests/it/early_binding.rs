//! Early binding and conditional declarations (ADR-0049 A26, issue #1035): one test
//! per row of the rule table in `src/early_binding.rs`, each PHP verdict run on
//! PHP 8.5.11.
//!
//! A lane judging a use that names a class or function declines where PHP raises
//! "Class not found" / "Call to undefined function" instead of the lane's error, and
//! appends the disjunction where either error can happen.

use steins_db::{PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    CALL_INACCESSIBLE_METHOD_ID, CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_METHOD_ID,
    CLASS_CONST_INACCESSIBLE_ID, CLASS_CONST_UNDEFINED_ID, CLASS_UNDEFINED_ID, Diagnostic, Folder,
    PROPERTY_UNDEFINED_ID, check_project_with_runtime, check_with,
};
use steins_syntax::SourceTree;

/// A ready boot surface whose only resident class-likes are `engine`.
struct Boot {
    engine: &'static [&'static str],
}

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
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        Some(self.engine.iter().any(|e| e.eq_ignore_ascii_case(fqn.trim_start_matches('\\'))))
    }
    fn boot_surface_function(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

const ENGINE: &[&str] = &["Exception", "ArrayObject", "Countable"];

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot { engine: ENGINE })
}

/// The findings of a two-file project, `a.php` holding `a` and `b.php` holding `b`.
fn run_two(a: &str, b: &str) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let files = vec![
        SourceFile::new(&db, "/proj/a.php".to_owned(), a.to_owned()),
        SourceFile::new(&db, "/proj/b.php".to_owned(), b.to_owned()),
    ];
    let project = Project::new(&db, files, ProjectLayout::fallback(), PluginFacts::none());
    check_project_with_runtime(&db, project, &mut Boot { engine: ENGINE }, true)
}

fn ids<'a>(d: &'a [Diagnostic], id: &str) -> Vec<&'a Diagnostic> {
    d.iter().filter(|d| d.id == id).collect()
}

const UNDEF_FN: &str = "Call to undefined function";
const NOT_FOUND: &str = "Class not found";

// Row 1: a top-level function is hoisted.

#[test]
fn row1_unconditional_function_used_above_is_judged() {
    let d = run("<?php\nf(1);\nfunction f($a, $b) {}\n");
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(!few[0].message.contains(UNDEF_FN), "{}", few[0].message);
}

#[test]
fn row1_function_exists_guard_keeps_firing() {
    // The #1035 control: PHP raises ArgumentCountError.
    let d = run("<?php\nfunction g(int $a) {}\nif (function_exists('g')) {\n    g();\n}\n");
    assert_eq!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "{d:?}");
}

// Row 2: a conditional function, used below it or in another file.

#[test]
fn row2_conditional_function_below_appends_the_disjunction() {
    // #1035 a): PHP raises "Call to undefined function f()".
    let d = run("<?php\nif (false) {\n    function f(int $a) {}\n}\nf();\n");
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(few[0].message.contains(UNDEF_FN), "{}", few[0].message);
}

#[test]
fn row2_inside_its_own_block_appends_the_disjunction() {
    // PHP raises ArgumentCountError: the declaration above ran.
    let d = run("<?php\nif (true) {\n    function f(int $a) {}\n    f();\n}\n");
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(few[0].message.contains(UNDEF_FN), "{}", few[0].message);
}

#[test]
fn row2_conditional_function_from_another_file_appends_the_disjunction() {
    let d = run_two("<?php\nif (true) {\n    function f(int $a) {}\n}\n", "<?php\nf();\n");
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(few[0].message.contains(UNDEF_FN), "{}", few[0].message);
}

// Row 3: a conditional function used above it in the same file.

#[test]
fn row3_conditional_function_used_above_declines() {
    // PHP: "Call to undefined function f()" — the block has not run yet.
    let d = run("<?php\nf();\nif (true) {\n    function f(int $a) {}\n}\n");
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

#[test]
fn row3b_conditional_function_used_from_a_body_above_appends_the_disjunction() {
    // Review witness s10: `function g() { f(); } if (…) { function f(int $a) {} } g();`
    // is ArgumentCountError on PHP 7.4 to 8.5 — the body runs when called, below.
    let d = run(
        "<?php\nfunction g() { f(); }\nif (!function_exists('h')) {\n    function f(int $a) {}\n}\ng();\n",
    );
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(few[0].message.contains(UNDEF_FN), "{}", few[0].message);
}

#[test]
fn row3b_body_called_above_the_declaration_still_names_both_errors() {
    // `function a() { inner(); } a(); if (true) { function inner(int $a) {} }` is the
    // undefined-function Error on PHP 8.5.11: a body called from top-level code above
    // the declaration is not modelled, and the disjunction names that error.
    let d = run(
        "<?php\nfunction a() { inner(); }\na();\nif (true) {\n    function inner(int $a) {}\n}\n",
    );
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(few[0].message.contains(UNDEF_FN), "{}", few[0].message);
}

// Row 4: a plain top-level class is hoisted.

#[test]
fn row4_plain_class_used_above_is_judged() {
    // The #1035 control `new E(); class E { __construct(int) }`: ArgumentCountError.
    let d = run("<?php\nnew E();\nclass E { public function __construct(int $a) {} }\n");
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(!few[0].message.contains(NOT_FOUND), "{}", few[0].message);
}

#[test]
fn row4_hoisted_in_a_braced_namespace_and_a_bare_block() {
    for src in [
        "<?php\nnamespace N {\n    new X();\n    final class X { public function __construct(int $a) {} }\n}\n",
        "<?php\n{\n    new X();\n    readonly class X { public function __construct(int $a) {} }\n}\n",
    ] {
        let d = run(src);
        assert_eq!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "{src}: {d:?}");
    }
}

#[test]
fn row4_static_call_above_a_plain_class_is_judged() {
    let d = run("<?php\nX::m();\nclass X { public static function m(int $a) {} }\n");
    assert_eq!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "{d:?}");
}

// Row 5: `extends` an earlier hoisted class, or an engine class.

#[test]
fn row5_parent_declared_earlier_is_judged() {
    let d = run(
        "<?php\nnew X();\nclass B {}\nclass X extends B { public function __construct(int $a) {} }\n",
    );
    assert_eq!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "{d:?}");
}

#[test]
fn row5_engine_parent_is_judged() {
    let d = run(
        "<?php\nnew X();\nclass X extends ArrayObject { public function __construct(int $a) {} }\n",
    );
    assert_eq!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "{d:?}");
}

#[test]
fn row5_engine_parent_the_boot_surface_lacks_declines() {
    // An extension class that is not loaded: X cannot bind at all.
    let d = run(
        "<?php\nnew X();\nclass X extends NotLoaded { public function __construct(int $a) {} }\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

// Row 6: a parent declared later, absent, conditional, or not itself hoisted.

#[test]
fn row6_parent_declared_later_declines() {
    // #1035 d): PHP raises Class "D" not found.
    let d = run(
        "<?php\nnew D();\nclass D extends Later { public function __construct(int $a) {} }\nclass Later {}\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

#[test]
fn row6_missing_parent_declines_arity_and_keeps_class_undefined() {
    // #1035 c): PHP raises Class "C" not found; `Missing` stays `class.undefined`.
    let d = run(
        "<?php\nnew C();\nclass C extends Missing { public function __construct(int $a) {} }\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
    assert_eq!(ids(&d, CLASS_UNDEFINED_ID).len(), 1, "{d:?}");
}

#[test]
fn row6_parent_not_itself_hoisted_declines() {
    let d = run(
        "<?php\nnew X();\nclass B extends Later {}\nclass X extends B { public function __construct(int $a) {} }\nclass Later {}\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

#[test]
fn row6_conditional_parent_declines() {
    let d = run(
        "<?php\nnew X();\nclass X extends B { public function __construct(int $a) {} }\nif (true) {\n    class B {}\n}\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

#[test]
fn row6_parent_in_another_file_declines_above() {
    // The parent binds X early only if its file already ran; under an autoloader it
    // has not (PHP 8.5.11: Class "X" not found).
    let d = run_two(
        "<?php\nclass B {}\n",
        "<?php\nnew X();\nclass X extends B { public function __construct(int $a) {} }\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

// Row 7: `implements`, `use T`, `enum`, an interface that extends.

#[test]
fn row7_implements_declines_every_naming_lane() {
    let decl = "interface I {}\nclass X implements I {\n    public function __construct(int $a) {}\n    public static function m(int $a) {}\n    private static function p() {}\n    private const K = 1;\n}\n";
    for (use_site, id) in [
        ("new X();", CALL_TOO_FEW_ARGUMENTS_ID),
        ("X::m();", CALL_TOO_FEW_ARGUMENTS_ID),
        ("X::nope();", CALL_UNDEFINED_METHOD_ID),
        ("(new X(1))->nope();", CALL_UNDEFINED_METHOD_ID),
        ("echo X::NOPE;", CLASS_CONST_UNDEFINED_ID),
        ("X::p();", CALL_INACCESSIBLE_METHOD_ID),
        ("echo X::K;", CLASS_CONST_INACCESSIBLE_ID),
    ] {
        let above = run(&format!("<?php\n{use_site}\n{decl}"));
        assert!(ids(&above, id).is_empty(), "above, {use_site}: {above:?}");
        let below = run(&format!("<?php\n{decl}{use_site}\n"));
        assert_eq!(ids(&below, id).len(), 1, "below, {use_site}: {below:?}");
    }
}

#[test]
fn row7_engine_interface_declines() {
    // `new X(); class X implements Countable {…}`: Class "X" not found.
    let d = run(
        "<?php\nnew X();\nclass X implements Countable {\n    public function __construct(int $a) {}\n    public function count(): int { return 0; }\n}\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

#[test]
fn row7_trait_use_declines() {
    let d = run(
        "<?php\nnew X();\ntrait T {}\nclass X { use T; public function __construct(int $a) {} }\n",
    );
    assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{d:?}");
}

#[test]
fn row7_extending_interface_and_enum_decline() {
    for src in [
        "<?php\necho I::NOPE;\ninterface J {}\ninterface I extends J {}\n",
        "<?php\necho I::NOPE;\ninterface I extends Countable {}\n",
        "<?php\necho E::NOPE;\nenum E { case A; }\n",
    ] {
        let d = run(src);
        assert!(ids(&d, CLASS_CONST_UNDEFINED_ID).is_empty(), "{src}: {d:?}");
    }
}

// Row 8: a non-hoisted class used below it, or from another file.

#[test]
fn row8_non_hoisted_class_used_below_or_elsewhere_is_judged() {
    let decl = "interface I {}\nclass X implements I { public function __construct(int $a) {} }\n";
    let below = run(&format!("<?php\n{decl}new X();\n"));
    assert_eq!(ids(&below, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "{below:?}");
    let other = run_two(&format!("<?php\n{decl}"), "<?php\nnew X();\n");
    let few = ids(&other, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{other:?}");
    assert!(!few[0].message.contains(NOT_FOUND), "{}", few[0].message);
}

#[test]
fn row8_uses_inside_bodies_above_a_late_bound_class_are_judged() {
    // Review witnesses s02, s02b, s03b: the bodies run when called, below the
    // declaration; PHP 7.4 to 8.5 raise the judged error.
    let s02 = run(
        "<?php\nclass Factory { function make(): X { return new X(1); } function go() { (new X(1, 2))->nope(); } }\ninterface I {}\nclass X implements I { function __construct(int $a, int $b) {} }\n(new Factory)->make();\n",
    );
    assert_eq!(ids(&s02, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "s02: {s02:?}");
    assert_eq!(ids(&s02, CALL_UNDEFINED_METHOD_ID).len(), 1, "s02: {s02:?}");
    let s02b = run(
        "<?php\nfunction go() { (new X)->nope(); }\ninterface I {}\nclass X implements I {}\ngo();\n",
    );
    assert_eq!(ids(&s02b, CALL_UNDEFINED_METHOD_ID).len(), 1, "s02b: {s02b:?}");
    let s03b = run(
        "<?php\nfunction use_it() { return X::NOPE; }\ninterface I {}\nclass X implements I {}\nuse_it();\n",
    );
    assert_eq!(ids(&s03b, CLASS_CONST_UNDEFINED_ID).len(), 1, "s03b: {s03b:?}");
    for d in [&s02, &s02b, &s03b] {
        assert!(d.iter().all(|f| !f.message.contains(NOT_FOUND)), "{d:?}");
    }
}

#[test]
fn row8_closure_and_arrow_bodies_above_are_judged() {
    let d = run(
        "<?php\n$go = function () { return new X(); };\n$f = fn () => X::NOPE;\ninterface I {}\nclass X implements I { function __construct(int $a) {} }\n",
    );
    assert_eq!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).len(), 1, "{d:?}");
    assert_eq!(ids(&d, CLASS_CONST_UNDEFINED_ID).len(), 1, "{d:?}");
}

// Row 9: a conditional class (or ancestor), dam clear, used below or elsewhere.

#[test]
fn row9_conditional_class_appends_the_disjunction() {
    // #1035 b): PHP raises Class "K" not found in the world the branch is not taken.
    let d = run("<?php\nif (rand(0, 1)) {\n    class K {}\n}\n(new K())->nope();\n");
    let undef = ids(&d, CALL_UNDEFINED_METHOD_ID);
    assert_eq!(undef.len(), 1, "{d:?}");
    assert!(undef[0].message.contains(NOT_FOUND), "{}", undef[0].message);
}

#[test]
fn row9_conditional_ancestor_appends_the_disjunction_to_arity() {
    let d = run_two(
        "<?php\nif (true) {\n    class B { public function __construct(int $a) {} }\n}\n",
        "<?php\nclass X extends B {}\nnew X();\n",
    );
    let few = ids(&d, CALL_TOO_FEW_ARGUMENTS_ID);
    assert_eq!(few.len(), 1, "{d:?}");
    assert!(few[0].message.contains(NOT_FOUND), "{}", few[0].message);
}

#[test]
fn row9_braced_namespace_conditional_class() {
    let d = run(
        "<?php\nnamespace N {\n    if (rand(0, 1)) {\n        class K {}\n    }\n    (new K())->nope();\n}\n",
    );
    let undef = ids(&d, CALL_UNDEFINED_METHOD_ID);
    assert_eq!(undef.len(), 1, "{d:?}");
    assert!(undef[0].message.contains(NOT_FOUND), "{}", undef[0].message);
}

// Row 10: a conditional class used above it in the same file.

#[test]
fn row10_conditional_class_used_above_declines() {
    let d = run("<?php\n(new K())->nope();\nif (true) {\n    class K {}\n}\n");
    assert!(ids(&d, CALL_UNDEFINED_METHOD_ID).is_empty(), "{d:?}");
}

// Row 11: the dam stands (A2i, unchanged).

#[test]
fn row11_dam_standing_declines() {
    for src in [
        "<?php\nif (true) {\n    function f(int $a) {}\n}\neval('');\nf();\n",
        "<?php\nif (true) {\n    class K {}\n}\neval('');\n(new K())->nope();\n",
    ] {
        let d = run(src);
        assert!(ids(&d, CALL_TOO_FEW_ARGUMENTS_ID).is_empty(), "{src}: {d:?}");
        assert!(ids(&d, CALL_UNDEFINED_METHOD_ID).is_empty(), "{src}: {d:?}");
    }
}

// Row 12: an interface with no parents is hoisted.

#[test]
fn row12_plain_interface_is_hoisted() {
    let d = run("<?php\necho I::NOPE;\ninterface I { const K = 1; }\n");
    assert_eq!(ids(&d, CLASS_CONST_UNDEFINED_ID).len(), 1, "{d:?}");
}

// Value lanes keep their A2i leg only: an object exists once its class ran.

#[test]
fn value_lane_property_read_is_unchanged() {
    // The base reports `property.undefined` here too: the property lanes ask only
    // the dam leg, since a read on an object runs after its class bound.
    let d = run_two(
        "<?php\ninterface I {}\n",
        "<?php\n$o = new X();\necho $o->nope;\nclass X implements I {}\n",
    );
    assert_eq!(ids(&d, PROPERTY_UNDEFINED_ID).len(), 1, "{d:?}");
}
