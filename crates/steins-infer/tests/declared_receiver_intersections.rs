//! Issue #238 — the declared-receiver lane consumes **intersection** arms,
//! under issue #234's inhabitance rule. Before this slice `Svc&Mock` reported
//! nothing, not even a member on neither arm (any arm not a bare
//! `ContractTy::Class` was refused). Pinned: recall that didn't exist, plus
//! the new reach's one false-positive risk.
//!
//! Rules, one test each: either conjunct resolving is silence; neither
//! resolving fires; a `Maybe` conjunct is silence; an intersection issue
//! #234 proves uninhabited is silence (FinalClass&MockObject, DEFAULT surface).
//!
//! The last is the false-positive guard: `final Svc & Mock` naturally
//! collapses to nothing, and a memberless lookup finds everything absent.
//! Under `dg/bypass-finals` the mock subclass really exists — false positive.

use std::path::PathBuf;

use steins_db::{Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    CALL_UNDEFINED_METHOD_ID, Diagnostic, FinalKeyword, Folder, PHPDOC_UNDEFINED_METHOD_ID,
    PROPERTY_UNDEFINED_ID, check_project_with_postures,
};

/// A ready boot surface: the A9 family gate is open and no name is a resident
/// homonym, so the A2ii leg never silences on its own.
struct Boot;

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

fn check_under(src: &str, final_keyword: FinalKeyword) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let layout = ProjectLayout::new(PathBuf::from("/proj"), vec![]);
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    check_project_with_postures(&db, project, &mut Boot, true, final_keyword)
}

/// Every declared-receiver method finding, whichever id A13 routed it to —
/// evidence phrasing distinguishes it from S2's own emissions on the shared id.
fn methods(src: &str, final_keyword: FinalKeyword) -> Vec<Diagnostic> {
    check_under(src, final_keyword)
        .into_iter()
        .filter(|d| {
            (d.id == PHPDOC_UNDEFINED_METHOD_ID || d.id == CALL_UNDEFINED_METHOD_ID)
                && d.message.contains("declared receiver")
        })
        .collect()
}

/// [`methods`] under the absence default — what a `steins.toml` with no
/// `[runtime] final-keyword` key means.
fn default_methods(src: &str) -> Vec<Diagnostic> {
    methods(src, FinalKeyword::Enforced)
}

fn properties(src: &str, final_keyword: FinalKeyword) -> Vec<Diagnostic> {
    check_under(src, final_keyword)
        .into_iter()
        .filter(|d| d.id == PROPERTY_UNDEFINED_ID && d.message.contains("declared receiver"))
        .collect()
}

/// Two ordinary classes: #234's emptiness leg doesn't run, intersection is
/// inhabited. `Svc::run()`/`Mock::spy()` are on one arm each; `gone()` on neither.
const INHABITED: &str = "<?php
class Svc { public function run(): int { return 1; } }
class Mock { public function spy(): int { return 2; } }
";

fn inhabited_calling(body: &str) -> String {
    format!("{INHABITED}/** @param Svc&Mock $o */\nfunction f($o): void {{ {body} }}\n")
}

// Member lookup over an inhabited intersection is the UNION of the arms.

#[test]
fn a_method_on_the_first_conjunct_resolves() {
    let d = default_methods(&inhabited_calling("$o->run();"));
    assert!(d.is_empty(), "run() is declared on Svc — the intersection has it: {d:?}");
}

#[test]
fn a_method_on_the_second_conjunct_resolves() {
    let d = default_methods(&inhabited_calling("$o->spy();"));
    assert!(d.is_empty(), "spy() is declared on Mock — the intersection has it: {d:?}");
}

#[test]
fn a_method_on_neither_conjunct_fires() {
    let d = default_methods(&inhabited_calling("$o->gone();"));
    assert_eq!(d.len(), 1, "gone() is on neither conjunct: {d:?}");
    let m = &d[0].message;
    assert!(m.contains("gone()"), "{m}");
    // Both conjuncts named as the conjunction — reader must see which type closed over.
    assert!(m.contains("Svc&Mock"), "the evidence names both conjuncts: {m}");
}

#[test]
fn a_property_on_either_conjunct_resolves_and_on_neither_fires() {
    // Property lane admits only `Verified` premises (ADR-0078's calibration
    // boundary) — receiver spelled as PHP 8.1's native intersection type.
    let src = "<?php
class Svc { public int $run = 1; }
class Mock { public int $spy = 2; }
function f(Svc&Mock $o): void { $a = $o->run; $b = $o->spy; }
";
    assert!(properties(src, FinalKeyword::Enforced).is_empty(), "both properties resolve");

    let src = "<?php
class Svc { public int $run = 1; }
class Mock { public int $spy = 2; }
function f(Svc&Mock $o): void { $a = $o->gone; }
";
    let d = properties(src, FinalKeyword::Enforced);
    assert_eq!(d.len(), 1, "gone is on neither conjunct: {d:?}");
    assert!(d[0].message.contains("Svc&Mock"), "{}", d[0].message);
}

// A `Maybe` on any conjunct is silence.

#[test]
fn an_unresolvable_conjunct_is_silence() {
    // `Unknown` is undeclared — hierarchy can't be enumerated, could declare `gone()`.
    let src = "<?php
class Svc { public function run(): int { return 1; } }
/** @param Svc&Unknown $o */
function f($o): void { $o->gone(); }
";
    let d = default_methods(src);
    assert!(d.is_empty(), "an unclosable conjunct silences the lane: {d:?}");
}

#[test]
fn a_conjunct_with_an_open_descendant_set_is_silence() {
    // `Mock` is non-final; descendant `SubMock` declares `gone()`, so §8's
    // descendant-closure leg refuses the conjunct — refusing one refuses the arm.
    let src = "<?php
class Svc { public function run(): int { return 1; } }
class Mock { public function spy(): int { return 2; } }
class SubMock extends Mock { public function gone(): int { return 3; } }
/** @param Svc&Mock $o */
function f($o): void { $o->gone(); }
";
    let d = default_methods(src);
    assert!(d.is_empty(), "a descendant introducing the method silences the lane: {d:?}");
}

#[test]
fn a_non_class_conjunct_is_silence() {
    // `Svc&callable`: receiver may be a closure, so absence can't be claimed.
    let src = "<?php
class Svc { public function run(): int { return 1; } }
/** @param Svc&callable $o */
function f($o): void { $o->gone(); }
";
    let d = default_methods(src);
    assert!(d.is_empty(), "a non-class conjunct silences the lane: {d:?}");
}

// Issue #234's inhabitance rule — the false-positive guard.

/// Issue #234's shape: `final Svc` + unrelated `Mock`. Enforced `final` means
/// every `Svc&Mock` value has exact class `Svc`, so `is_a(Svc, Mock)` is
/// provably `No` — the intersection is empty.
const FINAL_MOCK: &str = "<?php
interface Mock { public function spy(): int; }
final class Svc { public function run(): int { return 1; } }
/** @param Svc&Mock $o */
function f($o): void { $o->gone(); }
";

/// **Regression guard.** `gone()` is on neither conjunct so the ladder would
/// fire, but the arm is provably uninhabited under the default posture — no
/// value, no receiver, silence. Fails if `final Svc & Mock` collapses to
/// nothing and a memberless lookup fires on everything.
#[test]
fn an_uninhabited_intersection_is_silent_on_the_default_surface() {
    let d = default_methods(FINAL_MOCK);
    assert!(
        d.is_empty(),
        "FinalClass&MockObject is uninhabited under the enforced default — a claim about \
         it is vacuous, and firing is the false positive #234 guards: {d:?}"
    );
}

/// Same source under `final-keyword = "stripped"`: the loader removes the
/// keyword, the mock subclass exists, so the intersection is INHABITED and
/// the either-arm rule applies again — `gone()` fires. Proves the posture is
/// observable, not decorative: silence above comes from inhabitance, not a
/// blanket intersection refusal.
#[test]
fn the_stripped_posture_makes_the_same_intersection_fire() {
    let d = methods(FINAL_MOCK, FinalKeyword::Stripped);
    assert_eq!(
        d.len(),
        1,
        "under a final-stripping loader the intersection is inhabited and gone() is absent \
         from both conjuncts: {d:?}"
    );
    assert!(d[0].message.contains("gone()"), "{}", d[0].message);
}

/// Posture withdraws an emptiness proof; never adds a claim. A resolvable
/// member stays silent under BOTH postures.
#[test]
fn the_stripped_posture_never_fires_on_a_resolvable_member() {
    let src = "<?php
interface Mock { public function spy(): int; }
final class Svc { public function run(): int { return 1; } }
/** @param Svc&Mock $o */
function f($o): void { $o->run(); $o->spy(); }
";
    for posture in [FinalKeyword::Enforced, FinalKeyword::Stripped] {
        let d = methods(src, posture);
        assert!(d.is_empty(), "{posture:?}: run()/spy() are on the conjuncts: {d:?}");
    }
}

/// A lone `final` arm isn't self-conflicting (`is_a(F,F)` = `Yes`); two finals
/// in one hierarchy aren't either — emptiness must not swallow mere redundancy.
#[test]
fn a_final_arm_that_conflicts_with_nothing_still_fires() {
    // `Svc` implements `Mock`: `is_a(Svc,Mock)` = `Yes`, no conflict, inhabited as `Svc`.
    let src = "<?php
interface Mock { public function spy(): int; }
final class Svc implements Mock { public function run(): int { return 1; } public function spy(): int { return 2; } }
/** @param Svc&Mock $o */
function f($o): void { $o->gone(); }
";
    let d = default_methods(src);
    assert_eq!(d.len(), 1, "a final arm that is-a the other arm is no conflict: {d:?}");
}

