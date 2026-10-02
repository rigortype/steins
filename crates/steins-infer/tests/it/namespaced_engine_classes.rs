//! Issue #871: the engine's namespaced classes are keyed and resolved by FQN.
//!
//! The mined hierarchy once keyed `Random\RandomException` as `randomexception`,
//! because the miner read a `namespace X` only when its brace was on the same
//! line. A namespaced engine class could then never be rowed: the chain exit
//! (`engine_exit`) refused any name with a backslash, and the catalog had no key
//! for it. The exit is now gated on the hierarchy declaring the FQN, so
//! `Random\RandomException` takes `Exception::__construct`'s rows and a class a
//! user's namespace made up (`App\PDO`) still refuses.
//!
//! Two lanes are read for each case: the effect summary's `exhaustive` and the
//! throw summary's `throws_exhaustive`.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// `(effects exhaustive, throws exhaustive)` of `symbol`.
fn exhaustive(src: &str, symbol: &str) -> (bool, bool) {
    let s = summary(src, symbol);
    (s.exhaustive, s.throws_exhaustive)
}

/// The engine's namespaced exceptions the issue names, constructed by `new` in a
/// function that returns the object.
#[test]
fn a_new_of_a_namespaced_engine_exception_is_exhaustive_in_both_lanes() {
    for class in [
        "\\Random\\RandomException",
        "\\Random\\RandomError",
        "\\Random\\BrokenRandomEngineError",
        "\\Uri\\WhatWg\\InvalidUrlException",
        "\\Uri\\InvalidUriException",
        "\\FFI\\ParserException",
        "\\Filter\\FilterFailedException",
    ] {
        let src = format!("<?php\nfunction make(): object {{ return new {class}('x'); }}\n");
        assert_eq!(exhaustive(&src, "make"), (true, true), "new {class}");
        let s = summary(&src, "make");
        assert!(s.labels.is_empty() && s.throws.is_empty(), "new {class}: {s:?}");
    }
}

#[test]
fn a_throw_new_of_a_namespaced_engine_exception_is_exhaustive_and_names_the_class() {
    let src = "<?php\nfunction fail(): never { throw new \\Random\\RandomException('no'); }\n";
    let s = summary(src, "fail");
    assert_eq!(s.throws, vec!["RandomException".to_owned()], "{s:?}");
    assert!(s.throws_exhaustive && s.exhaustive, "{s:?}");

    let src = "<?php\nfunction fail(): never { throw new \\Uri\\WhatWg\\InvalidUrlException('no', []); }\n";
    assert_eq!(exhaustive(src, "fail"), (true, true));
}

/// A namespaced engine exception answers its accessors exactly as a global one
/// does, whatever that answer is in each lane.
#[test]
fn a_namespaced_engine_exception_answers_its_accessors_as_a_global_one_does() {
    let twin = |namespaced: &str, global: &str| {
        let body = |class: &str| {
            format!(
                "<?php\nfunction msg({class} $e): string {{ return $e->getMessage(); }}\n\
                 function made(): int {{ return (new {class}('x'))->getCode(); }}\n"
            )
        };
        let (n, g) = (body(namespaced), body(global));
        for symbol in ["msg", "made"] {
            assert_eq!(exhaustive(&n, symbol), exhaustive(&g, symbol), "{namespaced}: {symbol}");
        }
        exhaustive(&n, "made")
    };
    // The effect lane rows the accessors; the throw lane has no method-shaped rows.
    assert_eq!(twin("\\Random\\RandomException", "\\Exception"), (true, false));
    assert_eq!(twin("\\Random\\BrokenRandomEngineError", "\\Error"), (true, false));
}

/// A project exception that extends a namespaced engine one runs the engine's
/// constructor, as it does for a global one.
#[test]
fn a_project_exception_extending_a_namespaced_engine_exception_reaches_the_engine_row() {
    let bare = "<?php\nnamespace App;\nclass Oops extends \\Random\\RandomException {}\n\
                function fail(): never { throw new Oops('no'); }\n";
    let s = summary(bare, "fail");
    assert_eq!(s.throws, vec!["Oops".to_owned()], "{s:?}");
    assert!(s.exhaustive && s.throws_exhaustive, "{s:?}");

    let forwarding = "<?php\nnamespace App;\nclass Oops extends \\Random\\RandomException {\n    \
                      public function __construct(string $m) { parent::__construct($m, 7); }\n}\n\
                      function fail(): never { throw new Oops('no'); }\n";
    assert_eq!(exhaustive(forwarding, "Oops::__construct"), (true, true));
    assert_eq!(exhaustive(forwarding, "fail"), (true, true));
}

/// The key is the resolved FQN, not the spelling: an unimported name in a
/// namespace is the user's class, an imported one is the engine's.
#[test]
fn an_unimported_spelling_in_a_namespace_is_not_the_engines_class() {
    let unimported = "<?php\nnamespace App;\n\
                      function make(): object { return new RandomException('x'); }\n";
    assert_eq!(exhaustive(unimported, "make"), (false, false), "App\\RandomException");

    let imported = "<?php\nnamespace App;\nuse Random\\RandomException;\n\
                    function make(): object { return new RandomException('x'); }\n";
    assert_eq!(exhaustive(imported, "make"), (true, true));
}

/// The bare name the miner used to emit is no engine class either.
#[test]
fn the_global_spelling_of_a_namespaced_engine_class_is_no_engine_class() {
    let src = "<?php\nfunction make(): object { return new \\RandomException('x'); }\n";
    assert_eq!(exhaustive(src, "make"), (false, false), "\\RandomException is not declared");
    let src = "<?php\nfunction make(): object { return new \\Engine(); }\n";
    assert_eq!(exhaustive(src, "make"), (false, false), "\\Engine is not declared");
}

/// A name no hierarchy row declares stays a refusal in a namespace, as it was.
#[test]
fn a_class_the_users_namespace_made_up_still_refuses() {
    let pdo = "<?php\nnamespace App;\nfunction f(): void { (new PDO('x'))->query('SELECT 1'); }\n";
    assert_eq!(exhaustive(pdo, "f"), (false, false), "App\\PDO is not PDO");
    let twin = "<?php\nfunction make(): object { return new \\App\\Random\\RandomException('x'); }\n";
    assert_eq!(exhaustive(twin, "make"), (false, false));
}

/// A project class the user declared under the engine's FQN is the project's: its
/// body is the truth, whatever the hierarchy says of the name.
#[test]
fn a_project_class_named_like_a_namespaced_engine_class_shadows_it() {
    let src = "<?php\nnamespace Random;\nclass RandomException extends \\Exception {\n    \
               public function __construct() { parent::__construct('x'); }\n}\n\
               function make(): object { return new RandomException(); }\n";
    assert_eq!(exhaustive(src, "make"), (true, true));
}

/// A namespaced engine class the catalog has no row for is blind, as a global one
/// is: declared by the hierarchy, not thereby rowed.
#[test]
fn a_namespaced_engine_class_without_a_row_is_a_gap_of_its_own() {
    let src = "<?php\nfunction make(): object { return new \\Random\\Randomizer(); }\n";
    let s = summary(src, "make");
    assert!(!s.exhaustive && !s.throws_exhaustive, "{s:?}");
    assert_eq!(s.gaps, vec!["no-effect-row"], "{s:?}");
    assert_eq!(s.throws_gaps, vec!["no-throw-row"], "{s:?}");
}
