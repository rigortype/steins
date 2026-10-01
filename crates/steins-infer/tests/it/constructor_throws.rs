//! `new C(...)` runs `C`'s constructor, so its throws are the caller's (issue
//! #849), the throw-lane twin of `constructor_effects`. A `new` is an edge to
//! the constructor the effects pass resolves it to, an inherited one included,
//! filtered through the guards around the `new`; a class with no constructor
//! contributes nothing; a class the analyzer cannot name or resolve leaves the
//! caller's throw set non-exhaustive. An engine class answers from the
//! catalog's `__construct` throws row, and one without a row taints.

use steins_infer::{
    Diagnostic, EffectSummary, Facet, Origin, THROW_UNDECLARED_ID, check, effect_summary,
};
use steins_syntax::SourceTree;

fn undeclared(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == THROW_UNDECLARED_ID).collect()
}

fn one(src: &str) -> Diagnostic {
    let f = undeclared(src);
    assert_eq!(f.len(), 1, "expected exactly one undeclared throw, got: {f:#?}");
    f.into_iter().next().unwrap()
}

fn silent(src: &str) {
    let f = undeclared(src);
    assert!(f.is_empty(), "expected silence, got: {f:#?}");
}

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// The throw set of `symbol`, and whether it is exhaustive.
fn throws(src: &str, symbol: &str) -> (Vec<String>, bool) {
    let s = summary(src, symbol);
    (s.throws, s.throws_exhaustive)
}

/// The issue's own example: `Port`'s constructor throws, `open()` declares that
/// it throws nothing.
const PORT: &str = "<?php

final class Port
{
    /** @throws InvalidArgumentException */
    public function __construct(int $n)
    {
        if ($n < 0) {
            throw new InvalidArgumentException('negative');
        }
    }
}

/** @throws void */
function open(): Port
{
    return new Port(-1);
}
";

// ---------------------------------------------------------------------------
// A project constructor is an edge
// ---------------------------------------------------------------------------

#[test]
fn the_issue_example_reaches_the_caller() {
    assert_eq!(throws(PORT, "open"), (vec!["InvalidArgumentException".to_owned()], true));
    // `InvalidArgumentException` is a `LogicException`, unchecked (ADR-0007),
    // so it reaches the throw set and never the envelope.
    silent(PORT);
}

#[test]
fn a_checked_constructor_throw_escapes_the_callers_envelope() {
    let src = PORT.replace("InvalidArgumentException", "UnexpectedValueException");
    let d = one(&src);
    assert_eq!(
        d.message,
        "UnexpectedValueException can escape open() but is not declared (@throws void) — proven escape"
    );
    assert_eq!(d.line, 9, "reported at the throw inside the constructor");
    assert_eq!(d.facet, Some(Facet::Origin(Origin::Propagated)), "it arrived up the `new` edge");
}

#[test]
fn a_catch_around_the_new_dams_the_constructors_throw() {
    let src = "<?php\nfinal class Port {\n    \
               public function __construct() { throw new \\UnexpectedValueException(); }\n}\n\
               /** @throws void */\nfunction open(): ?Port {\n    \
               try { return new Port(); } catch (\\RuntimeException $e) { return null; }\n}\n";
    silent(src);
    assert_eq!(throws(src, "open"), (vec![], true));
}

#[test]
fn a_constructor_that_catches_its_own_throw_contributes_nothing() {
    let src = "<?php\nfinal class Safe {\n    public function __construct() {\n        \
               try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) {}\n    }\n}\n\
               /** @throws void */\nfunction make(): Safe { return new Safe(); }\n";
    silent(src);
    assert_eq!(throws(src, "make"), (vec![], true));
}

#[test]
fn an_inherited_constructor_is_the_one_new_runs() {
    let src = "<?php\nclass Base { public function __construct() { throw new \\RuntimeException(); } }\n\
               final class Child extends Base {}\n\
               /** @throws \\LogicException */\nfunction make(): Child { return new Child(); }\n";
    let d = one(src);
    assert!(d.message.starts_with("RuntimeException can escape make()"), "{}", d.message);
}

#[test]
fn a_class_without_a_constructor_contributes_nothing() {
    let src = "<?php\nclass Base {}\nclass Point extends Base { public int $x = 0; }\n\
               /** @throws void */\nfunction make(): Point { return new Point; }\n";
    silent(src);
    assert_eq!(throws(src, "make"), (vec![], true));
}

#[test]
fn the_arguments_are_still_the_callers_own() {
    let src = "<?php\nfinal class Box { public function __construct(public int $v) {} }\n\
               function risky(): int { throw new \\RuntimeException(); }\n\
               /** @throws void */\nfunction make(): Box { return new Box(risky()); }\n";
    let d = one(src);
    assert!(d.message.starts_with("RuntimeException can escape make()"), "{}", d.message);
}

// ---------------------------------------------------------------------------
// `self`, `static`, `parent`
// ---------------------------------------------------------------------------

#[test]
fn new_self_and_new_parent_resolve_exactly() {
    let src = "<?php\nclass Base { public function __construct() { throw new \\RuntimeException(); } }\n\
               class Factory extends Base {\n    \
               public function __construct() { throw new \\UnexpectedValueException(); }\n    \
               public static function me(): self { return new self(); }\n    \
               public static function up(): Base { return new parent(); }\n}\n";
    assert_eq!(throws(src, "Factory::me"), (vec!["UnexpectedValueException".to_owned()], true));
    assert_eq!(throws(src, "Factory::up"), (vec!["RuntimeException".to_owned()], true));
}

#[test]
fn new_static_resolves_only_where_no_subclass_can_replace_the_constructor() {
    let body = "    public function __construct() { throw new \\RuntimeException(); }\n    \
                public static function make(): static { return new static(); }\n}\n";
    let open = format!("<?php\nclass Node {{\n{body}");
    assert_eq!(throws(&open, "Node::make"), (vec![], false), "a subclass may bring its own");
    let sealed = format!("<?php\nfinal class Node {{\n{body}");
    let thrown = vec!["RuntimeException".to_owned()];
    assert_eq!(throws(&sealed, "Node::make"), (thrown.clone(), true), "a final class has no subclass");
    let pinned_body = body.replacen("public function", "final public function", 1);
    let pinned = format!("<?php\nclass Node {{\n{pinned_body}");
    assert_eq!(throws(&pinned, "Node::make"), (thrown, true), "a final constructor is every subclass's");
}

// ---------------------------------------------------------------------------
// What cannot be resolved is non-exhaustive, never silently throwless
// ---------------------------------------------------------------------------

#[test]
fn a_dynamic_class_marks_the_caller_non_exhaustive() {
    let src = "<?php\n/** @throws void */\nfunction make(string $cls): object { return new $cls(); }\n";
    silent(src);
    assert_eq!(throws(src, "make"), (vec![], false));
}

#[test]
fn an_undeclared_class_marks_the_caller_non_exhaustive_and_reports_nothing() {
    let src = "<?php\nnamespace App;\n/** @throws void */\nfunction make(): object { return new Missing(); }\n";
    silent(src);
    assert_eq!(throws(src, "make"), (vec![], false));
}

#[test]
fn a_trait_using_class_may_get_its_constructor_from_the_trait() {
    let src = "<?php\ntrait T {}\nclass C { use T; }\nfunction make(): C { return new C(); }\n";
    assert_eq!(throws(src, "make"), (vec![], false));
}

#[test]
fn a_thrown_unknown_class_keeps_its_throw_and_loses_exhaustiveness() {
    let src = "<?php\nnamespace App;\n/** @throws void */\nfunction fail(): never { throw new Missing(); }\n";
    assert_eq!(throws(src, "fail"), (vec!["Missing".to_owned()], false));
}

// ---------------------------------------------------------------------------
// Engine classes answer from the catalog
// ---------------------------------------------------------------------------

#[test]
fn a_pdo_connection_can_throw_pdo_exception() {
    let src = "<?php\n/** @throws \\LogicException */\n\
               function connect(string $dsn): \\PDO { return new \\PDO($dsn); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "PDOException can escape connect() but is not declared (@throws LogicException) — proven escape"
    );
    assert_eq!(d.facet, Some(Facet::Origin(Origin::Direct)), "the `new` is the body's own");
    assert_eq!(throws(src, "connect"), (vec!["PDOException".to_owned()], true));
}

#[test]
fn pdo_exception_is_a_runtime_exception() {
    let declared = "<?php\n/** @throws \\RuntimeException */\n\
                    function connect(string $dsn): \\PDO { return new \\PDO($dsn); }\n";
    silent(declared);
    let caught = "<?php\n/** @throws void */\nfunction connect(string $dsn): ?\\PDO {\n    \
                  try { return new \\PDO($dsn); } catch (\\RuntimeException $e) { return null; }\n}\n";
    silent(caught);
    assert_eq!(throws(caught, "connect"), (vec![], true));
}

#[test]
fn a_subclass_forwarding_to_pdo_carries_its_throw() {
    let src = "<?php\nclass Db extends \\PDO {\n    \
               public function __construct() { parent::__construct('sqlite::memory:'); }\n}\n\
               /** @throws void */\nfunction open(): Db { return new Db(); }\n";
    assert_eq!(throws(src, "Db::__construct"), (vec!["PDOException".to_owned()], true));
    let d = one(src);
    assert!(d.message.starts_with("PDOException can escape open()"), "{}", d.message);
}

#[test]
fn an_engine_exception_constructs_without_throwing() {
    let src = "<?php\nfunction fail(): never { throw new \\InvalidArgumentException('no'); }\n";
    assert_eq!(throws(src, "fail"), (vec!["InvalidArgumentException".to_owned()], true));
}

#[test]
fn a_project_exception_inherits_or_forwards_to_the_engine_constructor() {
    let bare = "<?php\nnamespace App;\nclass Oops extends \\RuntimeException {}\n\
                function fail(): never { throw new Oops('no'); }\n";
    assert_eq!(throws(bare, "fail"), (vec!["Oops".to_owned()], true));
    let forwarding = "<?php\nnamespace App;\nclass Oops extends \\RuntimeException {\n    \
                      public function __construct(string $m) { parent::__construct($m, 7); }\n}\n\
                      function fail(): never { throw new Oops('no'); }\n";
    assert_eq!(throws(forwarding, "Oops::__construct"), (vec![], true));
    assert_eq!(throws(forwarding, "fail"), (vec!["Oops".to_owned()], true));
}

#[test]
fn a_datetime_has_no_row_and_marks_the_caller_non_exhaustive() {
    // Throws only for an argument that does not parse, and a different class
    // from PHP 8.3 on, so the catalog does not guess either.
    let src = "<?php\n/** @throws void */\n\
               function at(string $s): \\DateTimeImmutable { return new \\DateTimeImmutable($s); }\n";
    silent(src);
    assert_eq!(throws(src, "at"), (vec![], false));
}

#[test]
fn an_uncatalogued_engine_class_is_non_exhaustive_and_silent() {
    let src = "<?php\n/** @throws void */\n\
               function open(): \\SplFileObject { return new \\SplFileObject('x'); }\n";
    silent(src);
    assert_eq!(throws(src, "open"), (vec![], false));
}

#[test]
fn a_project_class_shadows_the_engine_row() {
    let src = "<?php\nclass PDO { public function __construct() {} }\n\
               /** @throws void */\nfunction open(): PDO { return new PDO(); }\n";
    silent(src);
    assert_eq!(throws(src, "open"), (vec![], true));
}

// ---------------------------------------------------------------------------
// `throw new X(...)` runs `X`'s constructor too
// ---------------------------------------------------------------------------

#[test]
fn a_thrown_class_whose_constructor_throws_something_else_throws_both() {
    let src = "<?php\nclass Wrapped extends \\RuntimeException {\n    \
               public function __construct(string $m) {\n        \
               if ($m === '') { throw new \\JsonException('empty'); }\n        \
               parent::__construct($m);\n    }\n}\n\
               /** @throws Wrapped */\nfunction fail(string $m): never { throw new Wrapped($m); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "JsonException can escape fail() but is not declared (@throws Wrapped) — proven escape"
    );
    let expected = vec!["JsonException".to_owned(), "Wrapped".to_owned()];
    assert_eq!(throws(src, "fail"), (expected, true));
}

// ---------------------------------------------------------------------------
// Anonymous classes
// ---------------------------------------------------------------------------

#[test]
fn an_anonymous_class_runs_its_parents_constructor_and_its_arguments() {
    let src = "<?php\nclass Open { public function __construct() { throw new \\RuntimeException(); } }\n\
               function risky(): int { throw new \\UnexpectedValueException(); }\n\
               function inherit(): object { return new class extends Open {}; }\n\
               function args(): object { return new class(risky()) {}; }\n";
    assert_eq!(throws(src, "inherit"), (vec!["RuntimeException".to_owned()], true));
    assert_eq!(throws(src, "args"), (vec!["UnexpectedValueException".to_owned()], true));
}

#[test]
fn an_anonymous_class_constructor_is_unseen() {
    let src = "<?php\nfunction make(): object { return new class { public function __construct() {} }; }\n";
    assert_eq!(throws(src, "make"), (vec![], false));
    let plain = "<?php\nfunction make(): object { return new class { public int $x = 1; }; }\n";
    assert_eq!(throws(plain, "make"), (vec![], true));
}
