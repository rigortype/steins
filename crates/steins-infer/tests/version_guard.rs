//! Issue #29 — `PHP_VERSION_ID` compared against an int literal folds against the
//! resolved target range (issue #28's), and the false arm dies through ORDINARY
//! decided-branch pruning: `eval_cond` returns a verdict, `walk_if` marks the
//! skipped side dead, nothing version-specific downstream.
//!
//! The adversarial set the issue names is pinned here: inverted comparisons, Yoda
//! spellings, a straddling target (both arms stay live), and the shadowing/aliasing
//! corners where the fold must decline entirely.

use std::path::PathBuf;

use steins_db::{
    GoverningRoot, PhpTarget, PhpTargetSource, Project, ProjectLayout, SourceFile, SteinsDatabase,
};
use steins_infer::{Diagnostic, NoFold, check_project_with_runtime};

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

/// `^8.1` — floor 8.1, any 8.x minor: decides `< 80000`-style comparisons,
/// straddles `>= 80400`-style ones.
fn caret81() -> ProjectLayout {
    layout_with(Some(require_target("^8.1", (8, 1), Some((8, u16::MAX)))))
}

/// `>=8.1 <8.3` — entirely below 8.4: decides both directions.
fn old_range() -> ProjectLayout {
    layout_with(Some(require_target(">=8.1 <8.3", (8, 1), Some((8, 2)))))
}

fn check_under(src: &str, layout: ProjectLayout) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    check_project_with_runtime(&db, project, &mut NoFold, true)
}

fn dump_under(src: &str, layout: ProjectLayout) -> String {
    check_under(src, layout)
        .into_iter()
        .find(|d| d.id == "debug.type")
        .expect("one dump")
        .message
}

fn on_null_count(src: &str, layout: ProjectLayout) -> usize {
    check_under(src, layout).iter().filter(|d| d.id == "call.on-null").count()
}

// The decided directions

/// The survey's arriving class: `PHP_VERSION_ID < 80000` is `No` for every declared
/// version under a modern floor, the arm is dead, and a proof-layer finding disappears.
#[test]
fn an_old_php_branch_under_a_modern_floor_is_dead() {
    let src = "<?php\nif (PHP_VERSION_ID < 80000) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, caret81()), 0);
    // No target, no runtime: stays live, both arms walked.
    assert_eq!(on_null_count(src, layout_with(None)), 1);
}

#[test]
fn a_future_branch_under_a_bounded_ceiling_is_dead() {
    // `>= 80400` under `>=8.1 <8.3`: No — the then-arm dies.
    let src = "<?php\nif (PHP_VERSION_ID >= 80400) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, old_range()), 0);
}

#[test]
fn the_else_arm_of_a_decided_guard_is_the_live_one() {
    let src =
        "<?php\nif (PHP_VERSION_ID >= 80400) { $v = 1; } else { $v = \"s\"; }\n\\PHPStan\\dumpType($v);\n";
    assert_eq!(dump_under(src, old_range()), "dumped type: 's'");
}

// The adversarial set (the issue's named counterexamples)

/// A straddling target decides NOTHING: `^8.1` spans both sides of 80400, both arms
/// stay live, join is honest.
#[test]
fn a_straddling_target_keeps_both_arms_live() {
    let src =
        "<?php\nif (PHP_VERSION_ID >= 80400) { $v = 1; } else { $v = \"s\"; }\n\\PHPStan\\dumpType($v);\n";
    assert_eq!(dump_under(src, caret81()), "dumped type: 1|'s'");
}

/// The inverted comparison: `!(PHP_VERSION_ID >= 80000)` under `^8.1` is
/// `!(Yes)` = `No` — the guarded arm dies exactly as the un-negated `< 80000`.
#[test]
fn an_inverted_comparison_folds_through_not() {
    let src = "<?php\nif (!(PHP_VERSION_ID >= 80000)) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, caret81()), 0);
}

/// Yoda spelling: `80000 > PHP_VERSION_ID` asks `PHP_VERSION_ID < 80000`.
#[test]
fn a_yoda_comparison_mirrors_the_operator() {
    let src = "<?php\nif (80000 > PHP_VERSION_ID) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, caret81()), 0);
}

/// Equality outside the range is a definite `No`; inside it stays `Maybe` (minor
/// precision never pins one patch id).
#[test]
fn equality_decides_only_outside_the_interval() {
    let out = "<?php\nif (PHP_VERSION_ID === 70400) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(out, caret81()), 0);
    let inside = "<?php\nif (PHP_VERSION_ID === 80112) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(inside, caret81()), 1);
}

/// Liveness is branch-scoped: a decided guard never leaks its arm's env past the
/// construct's tail join.
#[test]
fn the_tail_after_the_guard_sees_the_join() {
    // Guard decided Yes (`>= 80100` under `^8.1`): the dump after the construct must
    // see the later reassignment, not a stale branch snapshot.
    let src = "<?php\nif (PHP_VERSION_ID >= 80100) { $v = 1; } else { $v = \"s\"; }\n$v = 2.5;\n\\PHPStan\\dumpType($v);\n";
    assert_eq!(dump_under(src, caret81()), "dumped type: 2.5");
}

// The corners where the fold must decline

/// A userland `const PHP_VERSION_ID` anywhere in the project disables the fold
/// project-wide — constant resolution is unmodeled, so no reference is safe.
#[test]
fn a_userland_const_twin_disables_the_fold() {
    let src = "<?php\nnamespace Compat;\nconst PHP_VERSION_ID = 70000;\nif (\\PHP_VERSION_ID < 80000) { $x = null; $x->m(); }\n";
    // Even the fully-qualified reference declines: conservative on purpose, one
    // weird file costs the feature, never a wrong branch.
    assert_eq!(on_null_count(src, caret81()), 1);
}

/// A literal `define('…PHP_VERSION_ID', …)` counts as a declaration too.
#[test]
fn a_define_twin_disables_the_fold() {
    let src = "<?php\ndefine('Legacy\\\\PHP_VERSION_ID', 70000);\nif (PHP_VERSION_ID < 80000) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, caret81()), 1);
}

/// A `use const … as PHP_VERSION_ID` alias makes the unqualified reference the
/// import, not the engine constant.
#[test]
fn a_const_import_alias_declines_the_fold() {
    let src = "<?php\nuse const Foo\\BAR as PHP_VERSION_ID;\nif (PHP_VERSION_ID < 80000) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, caret81()), 1);
}

/// A qualified spelling (`Foo\PHP_VERSION_ID`) is a different constant.
#[test]
fn a_qualified_spelling_is_not_the_engine_constant() {
    let src = "<?php\nif (Compat\\PHP_VERSION_ID < 80000) { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, caret81()), 1);
}

/// A comparison against a non-int literal declines (loose numeric-string table unmodeled).
#[test]
fn a_string_literal_comparison_declines() {
    let src = "<?php\nif (PHP_VERSION_ID < \"80000\") { $x = null; $x->m(); }\n";
    assert_eq!(on_null_count(src, caret81()), 1);
}
