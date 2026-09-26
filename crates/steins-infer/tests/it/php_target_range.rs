//! Issue #28 — the declared **target PHP range** drives version-sensitive decisions through
//! the layout: a `composer.json` target reaches ADR-0049 A22's append index via
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

/// An append onto an array that began as `[]`, after a negative write — the one
/// shape whose landing index PHP 8.3 moved (php-src GH-11154). `php -r` gives
/// `-5, 0` on 8.1.32 and 8.2.33, and `-5, -4` from 8.3.33.
const APPEND: &str = "<?php\n$a = [];\n$a[-5] = \"a\";\n$a[] = \"b\";\n\\PHPStan\\dumpType($a);\n";

/// The literal spelling of the same keys, which lands `"b"` on `-4` on every
/// minor from 8.0 up (`php -r` on 8.1.32, 8.2.33, 8.5.10).
const LITERAL: &str = "<?php\n$a = [-5 => \"a\", \"b\"];\n\\PHPStan\\dumpType($a);\n";

/// [`APPEND`] when its index declines: Amendment J's write at an unnamed integer key,
/// which may even have landed on `-5`.
const DECLINED: &str = "dumped type: non-empty-array{-5: 'a'|'b', ...<int, 'b'>}";

/// The dumped type of `$a` in `src` under `layout`.
fn dump_under(layout: ProjectLayout, src: &str) -> String {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    let ds = check_project_with_runtime(&db, project, &mut NoFold, true);
    ds.into_iter().find(|d| d.id == "debug.type").expect("one dump").message
}

fn every_layout() -> [(&'static str, ProjectLayout); 4] {
    [
        ("^8.1", layout_with(Some(require_target("^8.1", (8, 1), Some((8, u16::MAX)))))),
        (">=8.1 <8.3", layout_with(Some(require_target(">=8.1 <8.3", (8, 1), Some((8, 2)))))),
        ("^8.3", layout_with(Some(require_target("^8.3", (8, 3), Some((8, u16::MAX)))))),
        ("none", layout_with(None)),
    ]
}

#[test]
fn a_straddling_target_declines_the_negative_append_index() {
    // `^8.1` spans both sides of the 8.3 change: no single landing index holds for the
    // whole declared range, so the append names no key.
    let layout = layout_with(Some(require_target("^8.1", (8, 1), Some((8, u16::MAX)))));
    let dumped = dump_under(layout, APPEND);
    assert_eq!(dumped, DECLINED, "names no landing key");
}

#[test]
fn a_target_below_the_boundary_declines_the_negative_append_index() {
    // `>=8.1 <8.3` lands `"b"` on 0 here, but on -4 for `[-5 => "a"]` built as a literal,
    // and the two arrays witness the same key sequence.
    let layout = layout_with(Some(require_target(">=8.1 <8.3", (8, 1), Some((8, 2)))));
    let dumped = dump_under(layout, APPEND);
    assert_eq!(dumped, DECLINED, "names no landing key");
}

#[test]
fn a_target_above_the_boundary_resolves_the_negative_append_index() {
    // `^8.3`: every declared minor counts the negative key, so `"b"` lands at -4.
    let layout = layout_with(Some(require_target("^8.3", (8, 3), Some((8, u16::MAX)))));
    assert_eq!(dump_under(layout, APPEND), "dumped type: array{-5: 'a', -4: 'b'}");
}

#[test]
fn no_target_and_no_runtime_still_declines() {
    // No declaration and no sidecar answer: the negative append index declines.
    let dumped = dump_under(layout_with(None), APPEND);
    assert_eq!(dumped, DECLINED, "names no landing key");
    // An append that lands at 0 or above resolves under the same view.
    let src = "<?php\n$a = [1, 2];\n$a[] = 3;\n\\PHPStan\\dumpType($a);\n";
    assert_eq!(dump_under(layout_with(None), src), "dumped type: list{1, 2, 3}");
}

#[test]
fn a_negative_key_literal_resolves_under_every_target() {
    // The literal has one landing on every supported minor, so no target can split it.
    for (target, layout) in every_layout() {
        let dumped = dump_under(layout, LITERAL);
        assert_eq!(dumped, "dumped type: array{-5: 'a', -4: 'b'}", "target {target}");
    }
}
