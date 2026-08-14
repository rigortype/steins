//! Compile-time `class_alias` index edges (ADR-0049 §2 / A2iii).
//!
//! A `class_alias('Target', 'Alias')` whose names are known at compile time — string
//! literals, or the `X::class` constant (issue #36) — makes `Alias` resolve — for
//! existence — to `Target`'s declaration site. The edge shares textual declarations'
//! duplicate-decl ambiguity discipline: a collision with a textual declaration, or
//! two alias edges for one name, is `Ambiguous`. An unresolved target mints no edge.
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
