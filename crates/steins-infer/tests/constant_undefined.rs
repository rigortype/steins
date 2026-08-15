//! ADR-0078 / issue #198: `constant.undefined` — the absence family's
//! **global-constant** member, and a DAMMED absence proof like the rest of it.
//!
//! Fetching a constant nothing defines is a fatal since PHP 8.0. Measured on 8.5.9:
//!
//! ```text
//! php -r 'echo TOTALLY_UNDEFINED_XYZ;' → Undefined constant "TOTALLY_UNDEFINED_XYZ" (fatal)
//! ```
//!
//! There's no hierarchy to enumerate, so the ladder is shorter than the method
//! one but not weaker: every candidate name must be undeclared in the whole
//! universe (`const` and literal `define()` alike), the dam must be clear (*any*
//! dam site closes the valve, including a computed `define()` only this id
//! reads), and the project's own PHP must answer not-defined for every candidate.
//! The builtin catalog is never consulted — never an absence oracle (ADR-0049 §1).
//!
//! Two halves, the arrangement `preg_invalid_pattern.rs` set:
//!
//! * **mocked** — a [`Boot`] folder standing in for the boot surface, for every
//!   ladder leg and every silence, pinned deterministically;
//! * **live** — a real `php` on `PATH` answering a real `defined()`, which is the
//!   only thing that proves the id fires end to end and that a real extension
//!   constant silences it (skipped with a marker when `php` is absent).
//!
//! `X::CONST` is a **class** constant — a different member namespace, issue #197's
//! id — and is pinned here as a non-finding of this one.

use steins_db::{PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    CONSTANT_UNDEFINED_ID, Diagnostic, Folder, SidecarFolder, check, check_project, check_with,
};
use steins_syntax::SourceTree;

/// A boot-surface mock: `available` is the A9/no-sidecar gate; `consts` are the
/// constants the engine reports as defined (extension constants, bootstrap
/// leftovers); `oracle_fails` simulates a mid-run sidecar failure (Unknown for
/// every query).
///
/// `consts` is matched **case-sensitively** — the whole point of a separate oracle
/// from the function/class one.
struct Boot {
    available: bool,
    consts: Vec<String>,
    oracle_fails: bool,
}

impl Boot {
    fn ready() -> Self {
        Boot { available: true, consts: Vec::new(), oracle_fails: false }
    }
    fn with_consts(names: &[&str]) -> Self {
        Boot {
            available: true,
            consts: names.iter().map(|n| (*n).to_owned()).collect(),
            oracle_fails: false,
        }
    }
}

impl Folder for Boot {
    fn fold(
        &mut self,
        _n: &str,
        _a: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_constant(&mut self, name: &str) -> Option<bool> {
        if self.oracle_fails {
            return None;
        }
        Some(self.consts.iter().any(|c| c == name))
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| d.id == CONSTANT_UNDEFINED_ID)
        .collect()
}

fn fires(src: &str) -> Vec<Diagnostic> {
    run(src, &mut Boot::ready())
}

// Firing fixtures.

#[test]
fn fires_on_a_bare_undefined_global_constant() {
    let d = fires("<?php\necho TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined constant TYPO_CONST"), "{}", d[0].message);
    assert!(d[0].message.contains("PHP 8.5.8 (32 extensions)"), "{}", d[0].message);
    assert_eq!(d[0].line, 2, "{d:?}");
}

#[test]
fn fires_on_a_namespaced_unqualified_fetch_when_both_candidates_are_absent() {
    // PHP tries `App\TYPO_CONST` and then the global `TYPO_CONST`; both absent ⇒
    // fatal. The message names the current-ns candidate, PHP's own phrasing.
    let d = fires("<?php\nnamespace App;\necho TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined constant App\\TYPO_CONST"), "{}", d[0].message);
}

#[test]
fn fires_on_a_fully_qualified_absent_name() {
    let d = fires("<?php\nnamespace App;\necho \\TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined constant TYPO_CONST"), "{}", d[0].message);
}

#[test]
fn fires_on_a_qualified_absent_name() {
    let d = fires("<?php\nnamespace App;\necho Sub\\TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined constant App\\Sub\\TYPO_CONST"), "{}", d[0].message);
}

#[test]
fn fires_on_a_relative_namespace_fetch_a8() {
    // A8: `namespace\TYPO_CONST` in `App` resolves to `App\TYPO_CONST` and has no
    // global fallback — the doubled-prefix bug's twin on the constant side.
    let d = fires("<?php\nnamespace App;\necho namespace\\TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined constant App\\TYPO_CONST"), "{}", d[0].message);
}

#[test]
fn fires_on_a_use_const_import_whose_target_nothing_declares() {
    // The import wins outright: the single candidate is `Other\TYPO_CONST`, and
    // there is no fallback past it.
    let d = fires("<?php\nnamespace App;\nuse const Other\\TYPO_CONST;\necho TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined constant Other\\TYPO_CONST"), "{}", d[0].message);
}

#[test]
fn fires_on_a_case_mismatched_spelling_of_a_declared_constant() {
    // Constants are case-sensitive: `define('Foo', 1); var_dump(defined('FOO'));`
    // prints `bool(false)` on 8.5.9. The case-insensitive third `define()` arg
    // died in PHP 8.0, and the workspace floor is 8.1 (ADR-0011) — no version fork needed.
    let d = fires("<?php\nconst Widget = 1;\necho WIDGET;\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined constant WIDGET"), "{}", d[0].message);
}

#[test]
fn fires_once_per_fetch() {
    let d = fires("<?php\necho TYPO_CONST;\necho TYPO_CONST;\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

// Silence matrix — one fixture per ladder leg.

#[test]
fn silent_when_the_family_is_unavailable() {
    let mut b = Boot { available: false, consts: Vec::new(), oracle_fails: false };
    assert!(run("<?php\necho TYPO_CONST;\n", &mut b).is_empty());
}

#[test]
fn silent_under_no_php() {
    // `--no-php` / sound subset: `check` folds with `NoFold`, whose
    // `absence_family_available` defaults to `false` — the whole family is
    // silent (A9's honest consequence — ADR-0004).
    let tree = SourceTree::parse("<?php\necho TYPO_CONST;\n");
    let d: Vec<_> = check(&tree, &[], "test.php")
        .into_iter()
        .filter(|d| d.id == CONSTANT_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_the_oracle_is_unanswerable() {
    let mut b = Boot { available: true, consts: Vec::new(), oracle_fails: true };
    assert!(run("<?php\necho TYPO_CONST;\n", &mut b).is_empty());
}

#[test]
fn silent_on_a_const_statement_declaration() {
    assert!(fires("<?php\nconst TYPO_CONST = 1;\necho TYPO_CONST;\n").is_empty());
}

#[test]
fn silent_on_a_literal_define() {
    assert!(fires("<?php\ndefine('TYPO_CONST', 1);\necho TYPO_CONST;\n").is_empty());
}

#[test]
fn silent_on_the_conditional_define_idiom() {
    // The common shape and the acceptance criterion: the `define` sits inside a
    // branch precisely because the constant may already exist, but it declares
    // the name for absence purposes either way — conditionality isn't recorded.
    let d = fires(
        "<?php\nif (!defined('TYPO_CONST')) {\n    define('TYPO_CONST', 1);\n}\necho TYPO_CONST;\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_defined_guarded_fetch() {
    // The guard leg costs no second mechanism: `defined('X')` folds to `false`
    // under the SAME closure this ladder rests on (undeclared + dam clear + boot
    // surface says no), so the guarded branch is proven dead and the fetch inside
    // it is never judged — exactly how `class.undefined` handles `class_exists`.
    let d = fires("<?php\nif (defined('TYPO_CONST')) {\n    echo TYPO_CONST;\n}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_namespaced_const_declaration_reached_from_its_own_namespace() {
    let d = fires("<?php\nnamespace App;\nconst TYPO_CONST = 1;\necho TYPO_CONST;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_the_global_fallback_finds_the_declaration() {
    // Acceptance criterion for the fallback: `App\TYPO_CONST` is absent, but an
    // unqualified fetch falls back to global, as a function call does — witnessed
    // on 8.5.9: `define('G','g')` in `App` writes the GLOBAL name; `echo G` prints `g`.
    let d = fires("<?php\nnamespace App;\ndefine('TYPO_CONST', 1);\necho TYPO_CONST;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_fully_qualified_fetch_does_not_see_a_namespaced_declaration() {
    // Converse of the fallback, and why the candidate list isn't just "any
    // spelling": `\TYPO_CONST` is the global name; `App\TYPO_CONST` doesn't answer it.
    let d = fires("<?php\nnamespace App;\nconst TYPO_CONST = 1;\necho \\TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

/// Build a real multi-file salsa project and return findings carrying `id` — the
/// whole-universe evidence a single-file `check_with` cannot exercise.
fn project_of(files: &[(&str, &str)], id: &str) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs, ProjectLayout::fallback(), PluginFacts::none());
    check_project(&db, project, &mut Boot::ready())
        .into_iter()
        .filter(|d| d.id == id)
        .collect()
}

#[test]
fn silent_on_a_declaration_in_another_file_of_the_universe() {
    // Absence is a whole-universe claim, so the evidence is too — this path also
    // proves `Index::from_db` carries the constant table, not just `from_units`.
    let user = ("src/user.php", "<?php\necho TYPO_CONST;\n");
    // Control: alone, the fetch is a proven fatal.
    assert_eq!(project_of(&[user], CONSTANT_UNDEFINED_ID).len(), 1);
    let d = project_of(&[user, ("src/decl.php", "<?php\ndefine('TYPO_CONST', 1);\n")], CONSTANT_UNDEFINED_ID);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_vendor_package_declaration() {
    // A package's constant is as real as the project's own: ADR-0046 §2's vendor
    // presumption is about unproven dynamism, not ignoring plain declarations.
    let user = ("src/user.php", "<?php\necho TYPO_CONST;\n");
    let d = project_of(
        &[user, ("vendor/pkg/c.php", "<?php\nconst TYPO_CONST = 1;\n")],
        CONSTANT_UNDEFINED_ID,
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_boot_surface_homonym() {
    // The extension-constant leg, mocked: the name is undeclared in the project but
    // the engine has it.
    let mut b = Boot::with_consts(&["FICTIONAL_EXT_CONST"]);
    assert!(run("<?php\necho FICTIONAL_EXT_CONST;\n", &mut b).is_empty());
}

#[test]
fn the_boot_surface_query_is_case_sensitive() {
    // A homonym differing only in case is a DIFFERENT constant and must not silence
    // the fetch — the oracle is asked with the case as written.
    let mut b = Boot::with_consts(&["Fictional_Ext_Const"]);
    let d = run("<?php\necho FICTIONAL_EXT_CONST;\n", &mut b);
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn silent_under_a_computed_define_dam() {
    // The acceptance criterion: a `define()` whose name is only known at run time
    // can mint any constant, so it dams the whole check universe-wide — the
    // constant-side twin of a runtime-name `class_alias`.
    let d = fires("<?php\ndefine($name, 1);\necho TYPO_CONST;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_under_a_computed_define_dam_from_a_concatenation() {
    let d = fires("<?php\ndefine('PREFIX_' . $suffix, 1);\necho TYPO_CONST;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_computed_define_does_not_dam_the_function_and_class_ids() {
    // The narrower blast radius, pinned: `define()` mints a constant and nothing
    // else, so reading it as a universe-wide NAME dam would silence
    // `call.undefined-function` over something that cannot touch it.
    let tree = SourceTree::parse("<?php\ndefine($name, 1);\ntyop();\n");
    let mut folder = FnBoot;
    let d: Vec<_> = check_with(&tree, &[], "test.php", &mut folder)
        .into_iter()
        .filter(|d| d.id == steins_infer::CALL_UNDEFINED_FUNCTION_ID)
        .collect();
    assert_eq!(d.len(), 1, "the function id keeps firing beside a computed define: {d:?}");
}

/// A boot surface for the cross-id test above: it knows `define` (as every engine
/// does) and nothing else, so `tyop` is provably absent while the `define` call
/// itself is not a second finding.
struct FnBoot;

impl Folder for FnBoot {
    fn fold(
        &mut self,
        _n: &str,
        _a: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        Some(fqn == "define")
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

#[test]
fn silent_under_a_standing_eval_dam() {
    // Every ordinary dam kind closes the constant valve too: `eval` can mint one.
    let d = fires("<?php\neval('define(\"TYPO_CONST\", 1);');\necho TYPO_CONST;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_under_a_standing_include_dam() {
    let d = fires("<?php\ninclude 'config.php';\necho TYPO_CONST;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_in_a_dead_branch() {
    let d = fires("<?php\nif (false) {\n    echo TYPO_CONST;\n}\n");
    assert!(d.is_empty(), "{d:?}");
}

// Verified NON-findings: shapes outside this id's namespace entirely, excluded
// at COLLECTION rather than by a ladder leg.

#[test]
fn a_class_constant_is_not_collected() {
    // `X::CONST` is a class constant — issue #197's id, not this one. Pinned with
    // the class present so nothing else about the snippet is in doubt.
    let d = fires("<?php\nclass Widget { const SIZE = 1; }\necho Widget::MISSING;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn the_class_magic_constant_is_not_collected() {
    let d = fires("<?php\nclass Widget {}\necho Widget::class;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_class_like_constant_declaration_is_not_a_global_declaration() {
    // The other direction of the same boundary: a `const` inside a class body must
    // not silence a global fetch of the same name.
    let d = fires("<?php\nclass Widget { const TYPO_CONST = 1; }\necho TYPO_CONST;\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn the_reserved_literals_are_not_collected() {
    let d = fires("<?php\nvar_dump(true, false, null, TRUE, False, NULL);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn the_magic_constant_family_is_not_collected() {
    let d = fires("<?php\necho __LINE__, __FILE__, __DIR__, __FUNCTION__, __NAMESPACE__;\n");
    assert!(d.is_empty(), "{d:?}");
}

// Live: the project's own PHP answers.

/// Spawn a real folder, or print a skip marker. The probe name is one no snippet
/// below uses, so the per-run memo cannot answer a later question from it.
fn live_or_skip(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if !folder.absence_family_available()
        || folder.boot_surface_constant("STEINS_PROBE_CONSTANT_198").is_none()
    {
        eprintln!("SKIP {test}: no PHP engine answered `defined` — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

/// The acceptance criterion, end to end on the real engine.
#[test]
fn fires_against_a_real_engine() {
    let Some(mut folder) = live_or_skip("fires_against_a_real_engine") else { return };
    let d = run("<?php\necho STEINS_NO_SUCH_CONSTANT_198;\n", &mut folder);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains("undefined constant STEINS_NO_SUCH_CONSTANT_198"),
        "{}",
        d[0].message
    );
}

/// The extension-constant leg on the real engine: `JSON_THROW_ON_ERROR` is provided
/// by ext-json, is in no project index, and is in no Steins catalog — the sidecar is
/// the only thing that can know it, which is the whole point of ADR-0049 §1.
#[test]
fn silent_on_a_real_extension_constant() {
    let Some(mut folder) = live_or_skip("silent_on_a_real_extension_constant") else { return };
    let d = run("<?php\necho JSON_THROW_ON_ERROR;\n", &mut folder);
    assert!(d.is_empty(), "an ext-json constant must be silent: {d:#?}");
}

/// A core engine constant, same leg: nothing in the project declares `PHP_EOL`.
#[test]
fn silent_on_a_real_engine_constant() {
    let Some(mut folder) = live_or_skip("silent_on_a_real_engine_constant") else { return };
    let d = run("<?php\necho PHP_EOL;\n", &mut folder);
    assert!(d.is_empty(), "{d:#?}");
}
