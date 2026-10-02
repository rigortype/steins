//! Issue #871: every class the mined hierarchy lists resolves through
//! `ReflectionClass` on the project's own PHP **under the key it is stored as**.
//!
//! The miner once dropped the namespace of a class declared in a
//! `namespace X` whose brace sits on the next line, so `Random\RandomException`
//! was stored as `randomexception`. The table passed every other test, because
//! nothing asked the engine whether a key named what it said.
//!
//! This asks. Each row is offered to the live engine by its stored key; the
//! engine's own `ReflectionClass::getName()` must lowercase to that key, and
//! must be the casing php-src declares. A class the engine lacks (an extension
//! not loaded, or a class newer than this PHP: the mined stubs are a later
//! php-src than the pinned minor) is not a failure, since a row is a claim about
//! every build a target may run on; but a table in which most rows were absent
//! would mean the keys, not the build, were wrong, so a floor on the resolved
//! share and a handful of namespaced rows that every supported minor has are
//! asserted as well.
//!
//! The converse, that every namespaced class the engine declares has a row, is
//! the catalog's own test (`steins_catalog::builtins`), which needs no engine.
//!
//! Skipped with a marker when no `php` answers.

use steins_infer::{Folder, SidecarFolder};

/// Namespaced classes every supported PHP has (`ext/random` is built in since
/// 8.2): their absence from a live engine is a wrong key, not a missing extension.
const ALWAYS_PRESENT: &[&str] = &[
    "Random\\RandomException",
    "Random\\BrokenRandomEngineError",
    "Random\\RandomError",
    "Random\\Randomizer",
    "Random\\Engine",
    "Random\\Engine\\Mt19937",
];

fn live_or_skip(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if folder.reflected_class("Steins\\Probe871").is_none() {
        eprintln!("SKIP {test}: no PHP engine answered `reflect_class` — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

#[test]
fn every_hierarchy_key_resolves_under_the_key_it_is_stored_as() {
    let Some(mut folder) =
        live_or_skip("every_hierarchy_key_resolves_under_the_key_it_is_stored_as")
    else {
        return;
    };
    let (mut resolved, mut absent) = (Vec::new(), Vec::new());
    for (key, declared) in steins_catalog::engine_class_declarations() {
        assert_eq!(key, declared.to_ascii_lowercase(), "the stored key is the declared name's");
        let answer = folder.reflected_class(key).expect("a live engine answers");
        let Some(class) = answer.declaration else {
            absent.push(declared);
            continue;
        };
        assert_eq!(
            class.name.to_ascii_lowercase(),
            key,
            "`{key}` resolves to `{}`: the key does not name the class it is stored for",
            class.name
        );
        assert_eq!(class.name, declared, "`{key}`: the engine's casing is not the stored one");
        assert!(class.internal, "`{key}` is a class of the engine, not the project's: {class:?}");
        resolved.push(declared);
    }
    let total = resolved.len() + absent.len();
    assert!(total >= 300, "the hierarchy lists only {total} classes");
    assert!(
        resolved.len() * 10 >= total * 8,
        "{} of {total} keys resolve; the rest are absent from this engine: {absent:?}",
        resolved.len()
    );
    for class in ALWAYS_PRESENT {
        assert!(resolved.contains(class), "`{class}` must resolve on every supported PHP");
    }
}

/// The hierarchy's parents are keys of the same table: a parent spelled relative
/// to the declaring namespace and left unqualified would name no row.
#[test]
fn every_parent_a_hierarchy_row_names_is_itself_a_row() {
    for (key, _) in steins_catalog::engine_class_declarations() {
        let Some(supers) = steins_catalog::builtin_class_supers(key) else { continue };
        for parent in supers {
            assert!(
                steins_catalog::builtin_class_display(parent).is_some(),
                "`{key}` extends or implements `{parent}`, which no hierarchy row declares"
            );
        }
    }
}
