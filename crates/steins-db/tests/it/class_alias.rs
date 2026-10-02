//! Compile-time `class_alias` index edges (ADR-0049 §2 / A2iii).
//!
//! A `class_alias('Target', 'Alias')` whose names are known at compile time — string
//! literals, or the `X::class` constant (issue #36) — makes `Alias` resolve — for
//! existence — to `Target`'s declaration site. The edge shares textual declarations'
//! duplicate-decl ambiguity discipline: a collision with a textual declaration, or
//! two alias edges for one name, is `Ambiguous`. An unresolved target mints no edge, and an
//! alias of an alias resolves (a fixpoint over rounds, issue #926).
//! These tests pin the index machinery directly.

use steins_db::{Project, Resolve, SourceFile, SteinsDatabase, project_index};

/// A `Resolve` label comparable in `assert_eq!` (`Resolve`/`SourceFile` are not
/// `Debug`, so we project to a plain enum for readable failures).
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Absent,
    Unique,
    Ambiguous,
}

fn kind(r: Resolve) -> Kind {
    match r {
        Resolve::Absent => Kind::Absent,
        Resolve::Unique(_) => Kind::Unique,
        Resolve::Ambiguous => Kind::Ambiguous,
    }
}

/// Build a project from `(path, source)` pairs and resolve a class FQN.
fn resolve(files: &[(&str, &str)], fqn: &str) -> Resolve {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs, steins_db::ProjectLayout::fallback(), steins_db::PluginFacts::none());
    project_index(&db, project).resolve_class(fqn)
}

/// Whether two resolutions point at the same unique decl site.
fn same_unique(a: Resolve, b: Resolve) -> bool {
    matches!((a, b), (Resolve::Unique(x), Resolve::Unique(y)) if x == y)
}

#[test]
fn literal_class_alias_resolves_to_its_target() {
    let files = &[("a.php", "<?php\nclass Legacy {}\nclass_alias('Legacy', 'Modern');\n")];
    assert_eq!(kind(resolve(files, "Legacy")), Kind::Unique);
    assert!(same_unique(resolve(files, "Modern"), resolve(files, "Legacy")));
}

#[test]
fn namespaced_alias_edge_resolves() {
    let files = &[(
        "a.php",
        "<?php\nnamespace App;\nclass Legacy {}\nclass_alias('App\\\\Legacy', 'App\\\\Modern');\n",
    )];
    assert_eq!(kind(resolve(files, "App\\Modern")), Kind::Unique);
    assert!(same_unique(resolve(files, "App\\Modern"), resolve(files, "App\\Legacy")));
}

#[test]
fn alias_colliding_with_a_textual_decl_is_ambiguous() {
    // `Modern` is both a real class and an alias target → Ambiguous (both silent).
    let files = &[(
        "a.php",
        "<?php\nclass Legacy {}\nclass Modern {}\nclass_alias('Legacy', 'Modern');\n",
    )];
    assert_eq!(kind(resolve(files, "Modern")), Kind::Ambiguous);
    // The unrelated target is still uniquely resolvable.
    assert_eq!(kind(resolve(files, "Legacy")), Kind::Unique);
}

#[test]
fn two_alias_edges_for_one_name_are_ambiguous() {
    let files = &[(
        "a.php",
        "<?php\nclass A {}\nclass C {}\nclass_alias('A', 'X');\nclass_alias('C', 'X');\n",
    )];
    assert_eq!(kind(resolve(files, "X")), Kind::Ambiguous);
}

#[test]
fn alias_to_an_absent_target_mints_no_edge() {
    // The target `Nope` is undefined, so the alias cannot back an existence claim.
    let files = &[("a.php", "<?php\nclass_alias('Nope', 'B');\n")];
    assert_eq!(kind(resolve(files, "B")), Kind::Absent);
}

// Issue #36: `X::class` arguments resolve like any other class reference.

#[test]
fn class_const_alias_resolves_to_its_target() {
    // The issue's repro shape end to end: `class_alias(Thing::class, 'Legacy_Thing')`
    // makes `Legacy_Thing` resolve to `Thing`'s decl site.
    let files = &[("a.php", "<?php\nclass Thing {}\nclass_alias(Thing::class, 'Legacy_Thing');\n")];
    assert!(same_unique(resolve(files, "Legacy_Thing"), resolve(files, "Thing")));
}

#[test]
fn class_const_alias_resolves_through_use_imports_not_the_raw_spelling() {
    // Target is `Vendor\Pkg\Thing`, call writes the imported short name. Edge
    // must key on the RESOLVED FQN — raw `thing` would miss or collide with an
    // unrelated global `Thing`.
    let files = &[
        ("lib.php", "<?php\nnamespace Vendor\\Pkg;\nclass Thing {}\n"),
        ("boot.php", "<?php\nuse Vendor\\Pkg\\Thing;\nclass_alias(Thing::class, 'Legacy_Thing');\n"),
    ];
    assert!(same_unique(resolve(files, "Legacy_Thing"), resolve(files, "Vendor\\Pkg\\Thing")));
    // The grouped-use spelling of the same import resolves identically.
    let grouped = &[
        ("lib.php", "<?php\nnamespace Vendor\\Pkg;\nclass Thing {}\n"),
        ("boot.php", "<?php\nuse Vendor\\{Pkg\\Thing};\nclass_alias(Thing::class, 'Legacy_Thing');\n"),
    ];
    assert!(same_unique(resolve(grouped, "Legacy_Thing"), resolve(grouped, "Vendor\\Pkg\\Thing")));
}

#[test]
fn class_const_alias_to_an_absent_target_mints_no_edge() {
    // `X::class` never requires `X` to exist; an unresolvable target backs no
    // existence claim — same discipline as the literal form.
    let files = &[("a.php", "<?php\nclass_alias(NeverDeclared::class, 'Legacy');\n")];
    assert_eq!(kind(resolve(files, "Legacy")), Kind::Absent);
}

#[test]
fn class_const_alias_under_a_class_exists_guard_still_mints_its_edge() {
    // A conditionally-executed alias call still mints the edge, like the literal
    // form: over-approximating existence is the FP-safe direction.
    let files = &[(
        "a.php",
        "<?php\nclass Thing {}\nif (!class_exists('Legacy')) { class_alias(Thing::class, 'Legacy'); }\n",
    )];
    assert!(same_unique(resolve(files, "Legacy"), resolve(files, "Thing")));
}

#[test]
fn class_const_alias_to_a_conditionally_declared_target_resolves() {
    // The TARGET is conditionally declared (inside a function body); it's still
    // in the index as a decl site, so the alias resolves to it like any other target.
    let files = &[(
        "a.php",
        "<?php\nfunction boot(): void { class Thing {} }\nclass_alias(Thing::class, 'Legacy');\n",
    )];
    assert!(same_unique(resolve(files, "Legacy"), resolve(files, "Thing")));
}

#[test]
fn alias_edge_folds_across_files() {
    // Target in one file, alias call in another — the whole-project index joins them.
    let files = &[
        ("lib.php", "<?php\nclass Legacy {}\n"),
        ("boot.php", "<?php\nclass_alias('Legacy', 'Modern');\n"),
    ];
    assert_eq!(kind(resolve(files, "Modern")), Kind::Unique);
    assert!(same_unique(resolve(files, "Modern"), resolve(files, "Legacy")));
}

// Issue #926: the fold is a fixpoint, so an alias of an alias names the class too.

#[test]
fn a_chain_of_aliases_resolves_to_the_class_in_either_order() {
    for calls in [
        "class_alias('Legacy', 'A1'); class_alias('A1', 'A2'); class_alias('A2', 'A3');",
        "class_alias('A2', 'A3'); class_alias('A1', 'A2'); class_alias('Legacy', 'A1');",
    ] {
        let src = format!("<?php\nclass Legacy {{}}\n{calls}\n");
        let files = &[("a.php", src.as_str())];
        for name in ["A1", "A2", "A3"] {
            assert!(
                same_unique(resolve(files, name), resolve(files, "Legacy")),
                "{name} after `{calls}`"
            );
        }
    }
}

#[test]
fn a_chain_folds_across_files() {
    let files = &[
        ("lib.php", "<?php\nclass Legacy {}\n"),
        ("one.php", "<?php\nclass_alias('Legacy', 'Mid');\n"),
        ("two.php", "<?php\nclass_alias('Mid', 'Top');\n"),
    ];
    assert!(same_unique(resolve(files, "Top"), resolve(files, "Legacy")));
}

#[test]
fn a_cycle_of_aliases_mints_nothing_and_ends() {
    let files = &[("a.php", "<?php\nclass_alias('B1', 'B2');\nclass_alias('B2', 'B1');\n")];
    assert_eq!(kind(resolve(files, "B1")), Kind::Absent);
    assert_eq!(kind(resolve(files, "B2")), Kind::Absent);
}

#[test]
fn an_alias_of_an_ambiguous_alias_is_ambiguous_not_absent() {
    // `X` is minted by two edges to different classes, so it is ambiguous, and so is
    // everything aliasing it: the name exists, and which class it is cannot be said.
    let files = &[(
        "a.php",
        "<?php\nclass A {}\nclass C {}\nclass_alias('A', 'X');\nclass_alias('C', 'X');\nclass_alias('X', 'Y');\n",
    )];
    assert_eq!(kind(resolve(files, "X")), Kind::Ambiguous);
    assert_eq!(kind(resolve(files, "Y")), Kind::Ambiguous);
}

#[test]
fn two_edges_to_one_class_name_it_uniquely() {
    // The second call fails at run time and the name is still `A`: one candidate.
    let files = &[("a.php", "<?php\nclass A {}\nclass_alias('A', 'X');\nclass_alias('A', 'X');\n")];
    assert!(same_unique(resolve(files, "X"), resolve(files, "A")));
}

/// A name reached through two routes to different classes is ambiguous: a round
/// snapshot used to mint `X` as the first class alone.
#[test]
fn a_name_with_two_candidates_through_a_chain_is_ambiguous() {
    let files = &[(
        "a.php",
        "<?php\nclass T {}\nclass W {}\nclass_alias('W', 'Z');\nclass_alias('Z', 'Y');\nclass_alias('T', 'Y');\nclass_alias('Y', 'X');\n",
    )];
    assert_eq!(kind(resolve(files, "Y")), Kind::Ambiguous);
    assert_eq!(kind(resolve(files, "X")), Kind::Ambiguous);
    assert!(same_unique(resolve(files, "Z"), resolve(files, "W")));
}

/// A second call that names the first's alias back (`'B'` to `'A'`) leaves one candidate.
#[test]
fn a_loop_through_a_declared_class_stays_unique() {
    let files = &[(
        "a.php",
        "<?php\nclass Real {}\nclass_alias('Real', 'A');\nclass_alias('A', 'B');\nclass_alias('B', 'A');\n",
    )];
    assert!(same_unique(resolve(files, "A"), resolve(files, "Real")));
    assert!(same_unique(resolve(files, "B"), resolve(files, "Real")));
}

/// PHP refuses to redeclare an engine class, so `class_alias` returns false and the
/// catalog's name keeps its identity: the fold mints nothing for it.
#[test]
fn an_alias_named_for_a_catalog_class_mints_nothing() {
    for name in ["Stringable", "stringable", "ArrayAccess", "DOMException", "Dom\\DOMException"] {
        let src = format!("<?php\nclass Legacy {{}}\nclass_alias('Legacy', '{name}');\n");
        let files = &[("a.php", src.as_str())];
        assert_eq!(kind(resolve(files, name)), Kind::Absent, "{name}");
    }
}

/// A textual declaration a call also names (the BC shim: `class_alias(New, Old)` beside
/// `if (false) { class Old extends New {} }`) is ambiguous.
#[test]
fn a_textual_name_an_alias_also_names_is_ambiguous() {
    let files = &[
        (
            "new.php",
            "<?php\nnamespace Lib;\nclass NewItem {}\nclass_alias(NewItem::class, OldItem::class);\n",
        ),
        ("old.php", "<?php\nnamespace Lib;\nif (false) { class OldItem extends NewItem {} }\n"),
    ];
    assert_eq!(kind(resolve(files, "Lib\\OldItem")), Kind::Ambiguous);
    assert_eq!(kind(resolve(files, "Lib\\NewItem")), Kind::Unique);
}

/// An alias of a declared name an edge also names may be any class that name can be:
/// `Next` is ambiguous, with both candidates, not `Shadow`'s declaration alone.
#[test]
fn an_alias_of_a_shadowed_alias_is_ambiguous() {
    let files = &[(
        "a.php",
        "<?php\nclass Real {}\nclass_alias('Real', 'Shadow');\nif (false) { class Shadow {} }\nclass_alias('Shadow', 'Next');\n",
    )];
    assert_eq!(kind(resolve(files, "Shadow")), Kind::Ambiguous);
    assert_eq!(kind(resolve(files, "Next")), Kind::Ambiguous);
}
