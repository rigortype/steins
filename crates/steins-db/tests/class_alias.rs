//! Literal `class_alias` index edges (ADR-0049 §2 / A2iii).
//!
//! A literal `class_alias('Target', 'Alias')` makes `Alias` resolve — for
//! existence — to `Target`'s declaration site. The edge folds into the project
//! index after every textual declaration, sharing the duplicate-decl ambiguity
//! discipline: an alias colliding with a textual decl of the same FQN, or two alias
//! edges for one name, is `Ambiguous`. An alias whose target does not resolve mints
//! no edge. Consumed by nothing in S1 — this pins the index machinery directly.

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
    let project = Project::new(&db, inputs);
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
    // The alias resolves to exactly the target's decl site.
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
