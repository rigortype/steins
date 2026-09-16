//! Issue #28 — the declared **target PHP range** drives version-sensitive decisions through
//! the layout: a `composer.json` target reaches ADR-0049 A12's next-int rule via
//! `check_project_with_runtime` ("the range must agree, else decline").
//!
//! The absence-family and curated-admission legs live behind a live sidecar; this file pins
//! the layout→Cx seam, which needs no PHP at all.

use std::path::PathBuf;

use steins_db::{
    GoverningRoot, PhpTarget, PhpTargetSource, Project, ProjectLayout, SourceFile, SteinsDatabase,
};
use steins_infer::{NoFold, check_project_with_runtime};

fn layout_with(target: Option<PhpTarget>) -> ProjectLayout {
    let root = GoverningRoot::new(
        PathBuf::from("/proj/composer.json"),
        PathBuf::from("/proj"),
        vec![PathBuf::from("/proj/vendor")],
        vec![],
    )
    .with_php_target(target);
    ProjectLayout::new(PathBuf::from("/proj"), vec![root])
}

fn require_target(raw: &str, floor: (u16, u16), ceiling: Option<(u16, u16)>) -> PhpTarget {
    PhpTarget { floor, ceiling, source: PhpTargetSource::Require, raw: raw.to_owned() }
}

/// The dumped type of `$a = [-5 => "a", "b"];` under `layout` — the
/// boundary-sensitive literal whose keys the 8.3 rule change moved.
fn dump_under(layout: ProjectLayout) -> String {
    let db = SteinsDatabase::default();
    let src = "<?php\n$a = [-5 => \"a\", \"b\"];\n\\PHPStan\\dumpType($a);\n";
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    let ds = check_project_with_runtime(&db, project, &mut NoFold, true);
    ds.into_iter().find(|d| d.id == "debug.type").expect("one dump").message
}

#[test]
fn a_straddling_target_declines_the_boundary_sensitive_literal() {
    // `^8.1` spans both sides of the 8.3 next-int change: no single key assignment holds for
    // the whole declared range, so the value is honestly unknown (A12's unknown leg).
    let layout = layout_with(Some(require_target("^8.1", (8, 1), Some((8, u16::MAX)))));
    assert_eq!(dump_under(layout), "dumped type: unknown");
}

#[test]
fn a_target_below_the_boundary_resolves_with_the_old_rule() {
    // `>=8.1 <8.3` sits entirely before the change: `"b"` lands at key 0.
    let layout = layout_with(Some(require_target(">=8.1 <8.3", (8, 1), Some((8, 2)))));
    let dumped = dump_under(layout);
    assert!(dumped.contains("-5"), "keeps the explicit key: {dumped}");
    assert!(!dumped.contains("-4"), "must NOT use the 8.3+ rule: {dumped}");
}

#[test]
fn a_target_above_the_boundary_resolves_with_the_new_rule() {
    // `^8.3`: every declared minor uses max+1, so `"b"` lands at -4.
    let layout = layout_with(Some(require_target("^8.3", (8, 3), Some((8, u16::MAX)))));
    let dumped = dump_under(layout);
    assert!(dumped.contains("-4"), "the 8.3+ rule applies: {dumped}");
}

#[test]
fn no_target_and_no_runtime_still_declines() {
    // No declaration and no sidecar answer: the straddling literal declines rather than guessing.
    assert_eq!(dump_under(layout_with(None)), "dumped type: unknown");
    // And a version-independent literal still resolves under the same view.
    let db = SteinsDatabase::default();
    let src = "<?php\n$a = [1, 2];\n\\PHPStan\\dumpType($a);\n";
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![file], layout_with(None), steins_db::PluginFacts::none());
    let ds = check_project_with_runtime(&db, project, &mut NoFold, true);
    let msg = ds.into_iter().find(|d| d.id == "debug.type").expect("one dump").message;
    assert_ne!(msg, "dumped type: unknown", "version-independent literals resolve");
}
