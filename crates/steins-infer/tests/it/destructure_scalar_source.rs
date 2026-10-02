//! Issue #931 — destructuring a scalar has its own rule, not the plain read's.
//!
//! `[$x] = $v;` forwarded to the plain-read rule, which says "reads null with
//! `Trying to access array offset on <type>`". That is the wording of `$v[0]`; a
//! destructure is a different operation, and PHP treats it differently.
//!
//! # Behavioral witnesses (`php -d error_reporting=-1`, scalar source)
//!
//! ```text
//!                    8.4.25                    8.5.11
//! null source        silent                    silent
//! int/float/bool     silent                    Warning: Cannot use <type> as array
//! string source      silent                    Warning: Cannot use string as array
//! object source      Error: Cannot use object of type stdClass as array (both)
//! $v[0] on int       Warning: Trying to access array offset on int (both)
//! ```
//!
//! PHP warns once per element it fetches: `[$a, $b] = 42;` warns twice, a hole warns
//! only for the indices it binds, and a nested pattern warns for its outer reads only
//! (the inner source is the `null` the failed fetch produced). The finding is one per
//! statement: every read of the statement shares its span, and same-site findings
//! collapse.
//!
//! So a scalar source reports only where the whole analysed interval is at least 8.5:
//! the declared target's floor when the project declares a target, else the sidecar's
//! minor. A null source is silent everywhere, and an interval that straddles 8.5 (or
//! names no version) is silent.

use std::path::PathBuf;

use steins_db::{
    GoverningRoot, PhpTarget, PhpTargetSource, Project, ProjectLayout, SourceFile, SteinsDatabase,
};
use steins_infer::{
    Diagnostic, Folder, OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID, check_project_with_runtime,
};
use steins_syntax::ArgValue;

/// A live, monkey-patch-free sidecar reporting `minor` as its PHP minor.
struct Sidecar {
    minor: Option<(u16, u16)>,
}

impl Folder for Sidecar {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        self.minor
    }
}

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

fn require(raw: &str, floor: (u16, u16), ceiling: Option<(u16, u16)>) -> PhpTarget {
    PhpTarget { floor, ceiling, source: PhpTargetSource::Require, raw: raw.to_owned() }
}

/// The `offset.*` proof findings of `src` under `layout` on a sidecar at `minor`.
fn offsets(src: &str, minor: Option<(u16, u16)>, layout: ProjectLayout) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    check_project_with_runtime(&db, project, &mut Sidecar { minor }, true)
        .into_iter()
        .filter(|d| d.id == OFFSET_ON_UNSUPPORTED_ID || d.id == OFFSET_MISSING_ID)
        .collect()
}

fn on(minor: (u16, u16), src: &str) -> Vec<Diagnostic> {
    offsets(src, Some(minor), layout_with(None))
}

const V84: (u16, u16) = (8, 4);
const V85: (u16, u16) = (8, 5);

fn body(stmts: &str) -> String {
    format!("<?php\nfunction f(): void {{ {stmts} }}\nf();\n")
}

// Null is silent on every version

#[test]
fn a_null_source_is_silent_on_every_minor() {
    let src = body("$v = null; [$x] = $v;");
    for minor in [Some((8, 1)), Some(V84), Some(V85), Some((9, 0)), None] {
        let d = offsets(&src, minor, layout_with(None));
        assert!(d.is_empty(), "null source on {minor:?}: {d:#?}");
    }
}

#[test]
fn a_null_source_is_silent_under_a_target_at_or_above_8_5() {
    let src = body("$v = null; [$x] = $v;");
    let layout = layout_with(Some(require(">=8.5", V85, None)));
    assert!(offsets(&src, Some(V85), layout).is_empty());
}

#[test]
fn list_and_keyed_patterns_on_a_null_source_are_silent_too() {
    for stmts in [
        "$v = null; list($x) = $v;",
        "$v = null; ['k' => $x] = $v;",
        "$v = null; [$a, [$b]] = $v;",
    ] {
        let d = on(V85, &body(stmts));
        assert!(d.is_empty(), "{stmts}: {d:#?}");
    }
}

// A scalar source reports on an 8.5 interval

#[test]
fn each_scalar_type_reports_php_8_5s_own_wording() {
    for (init, word) in [
        ("42", "int"),
        ("1.5", "float"),
        ("true", "bool"),
        ("false", "bool"),
        ("'ab'", "string"),
    ] {
        let d = on(V85, &body(&format!("$v = {init}; [$x] = $v;")));
        assert_eq!(d.len(), 1, "{init}: {d:#?}");
        assert_eq!(d[0].id, OFFSET_ON_UNSUPPORTED_ID, "{init}");
        assert!(
            d[0].message.contains(&format!("\"Cannot use {word} as array\"")),
            "{init}: {}",
            d[0].message
        );
        assert!(
            !d[0].message.contains("Trying to access array offset"),
            "{init}: the plain read's wording is not this operation's: {}",
            d[0].message
        );
    }
}

#[test]
fn the_message_names_the_source_the_type_and_the_version_gate() {
    let d = on(V85, &body("$v = 42; [$x] = $v;"));
    assert_eq!(d.len(), 1, "{d:#?}");
    let m = &d[0].message;
    assert!(m.contains("destructuring $v"), "{m}");
    assert!(m.contains("provably int"), "{m}");
    assert!(m.contains("PHP 8.5"), "the finding says it holds from 8.5: {m}");
}

#[test]
fn list_and_keyed_patterns_follow_the_same_rule() {
    for stmts in [
        "$v = 42; list($x) = $v;",
        "$v = 42; ['k' => $x] = $v;",
        "$v = false; list('k' => $x) = $v;",
    ] {
        let d = on(V85, &body(stmts));
        assert_eq!(d.len(), 1, "{stmts}: {d:#?}");
        assert_eq!(d[0].id, OFFSET_ON_UNSUPPORTED_ID, "{stmts}");
    }
    for stmts in ["$v = 42; list($x) = $v;", "$v = 42; ['k' => $x] = $v;"] {
        assert!(on(V84, &body(stmts)).is_empty(), "{stmts} on 8.4");
    }
}

#[test]
fn a_statement_is_one_finding_however_many_elements_it_fetches() {
    // PHP warns per element on 8.5.11 (`[$a, $b, $c] = 42` three times, a hole skips
    // its index, `[[$a], $b] = 42` twice), but the finding sits on the statement and
    // same-site reads collapse to one, as they did for a null source.
    for stmts in [
        "$v = 42; [$a, $b, $c] = $v;",
        "$v = 42; ['x' => $a, 'y' => $b] = $v;",
        "$v = 42; [, $b] = $v;",
        "$v = 42; [[$a], $b] = $v;",
    ] {
        assert_eq!(on(V85, &body(stmts)).len(), 1, "{stmts}");
    }
}

// ... and is silent below it, or when the interval does not prove it

#[test]
fn an_8_4_sidecar_is_silent() {
    for init in ["42", "1.5", "true", "'ab'"] {
        let d = on(V84, &body(&format!("$v = {init}; [$x] = $v;")));
        assert!(d.is_empty(), "{init}: {d:#?}");
    }
}

#[test]
fn no_sidecar_minor_and_no_target_is_silent() {
    let d = offsets(&body("$v = 42; [$x] = $v;"), None, layout_with(None));
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn a_target_that_admits_8_4_is_silent_even_on_an_8_5_sidecar() {
    // `^8.1`: the declared range, not the sidecar, is what the code must run on.
    let src = body("$v = 42; [$x] = $v;");
    for t in [
        require("^8.1", (8, 1), Some((8, u16::MAX))),
        require(">=8.1", (8, 1), None),
        require(">=8.4 <8.6", (8, 4), Some((8, 5))),
    ] {
        let raw = t.raw.clone();
        let d = offsets(&src, Some(V85), layout_with(Some(t)));
        assert!(d.is_empty(), "{raw}: {d:#?}");
    }
}

#[test]
fn a_target_wholly_at_or_above_8_5_reports_whatever_the_sidecar_says() {
    let src = body("$v = 42; [$x] = $v;");
    for (t, minor) in [
        (require(">=8.5", V85, None), Some(V84)),
        (require("^8.5", V85, Some((8, u16::MAX))), None),
        (require(">=8.5 <8.7", V85, Some((8, 6))), Some(V85)),
    ] {
        let raw = t.raw.clone();
        let d = offsets(&src, minor, layout_with(Some(t)));
        assert_eq!(d.len(), 1, "{raw} on {minor:?}: {d:#?}");
        assert!(d[0].message.contains("Cannot use int as array"), "{}", d[0].message);
    }
}

#[test]
fn a_userland_version_id_constant_unpins_the_interval() {
    // The version fold is off project-wide once `PHP_VERSION_ID` is user-declared.
    let src = "<?php\nconst PHP_VERSION_ID = 1;\nfunction f(): void { $v = 42; [$x] = $v; }\n";
    assert!(on(V85, src).is_empty());
}

// What stays as it was

#[test]
fn the_plain_read_keeps_its_wording_on_every_minor() {
    for minor in [V84, V85] {
        let d = on(minor, &body("$v = 42; $x = $v[0];"));
        assert_eq!(d.len(), 1, "{minor:?}: {d:#?}");
        assert_eq!(d[0].id, OFFSET_ON_UNSUPPORTED_ID);
        assert!(d[0].message.contains("Trying to access array offset on int"), "{}", d[0].message);
    }
}

#[test]
fn a_plain_read_of_null_still_reports() {
    let d = on(V84, &body("$v = null; $x = $v[0];"));
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("Trying to access array offset on null"), "{}", d[0].message);
}

#[test]
fn an_array_source_still_reports_a_missing_key_on_every_minor() {
    for minor in [V84, V85] {
        let d = on(minor, &body("$v = ['a' => 1]; [$p, $q] = $v;"));
        assert_eq!(d.len(), 2, "{minor:?}: {d:#?}");
        assert!(d.iter().all(|d| d.id == OFFSET_MISSING_ID), "{d:#?}");
        assert!(d[0].message.contains("Undefined array key 0"), "{}", d[0].message);
    }
}

#[test]
fn an_object_source_is_out_of_scope_and_silent() {
    // A fatal on every version, but the offset family has no object case yet.
    let d = on(V85, &body("$v = new stdClass; [$x] = $v;"));
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn the_warning_handler_null_posture_still_silences_the_warning() {
    let db = SteinsDatabase::default();
    let src = body("$v = 42; [$x] = $v;");
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src);
    let project =
        Project::new(&db, vec![file], layout_with(None), steins_db::PluginFacts::none());
    let ds = check_project_with_runtime(&db, project, &mut Sidecar { minor: Some(V85) }, false);
    assert!(ds.iter().all(|d| d.id != OFFSET_ON_UNSUPPORTED_ID), "{ds:#?}");
}
