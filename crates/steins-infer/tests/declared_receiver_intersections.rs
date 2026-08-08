//! Issue #238 — the declared-receiver lane consumes **intersection** arms, under
//! issue #234's inhabitance rule.
//!
//! Before this slice a `Svc&Mock` receiver reported nothing at all, not even a
//! member that exists on neither arm: `check_phpdoc_undefined_method` refused any
//! arm that was not a bare `ContractTy::Class`, so the whole lane fell out. So what
//! is pinned here is **recall that did not exist**, plus the one place the new
//! reach could manufacture a false positive.
//!
//! The rules, one test each:
//!
//! * a member on **either** conjunct resolves — silence;
//! * a member on **neither** conjunct fires;
//! * a `Maybe` on any conjunct (an unresolvable class, an open hierarchy) is
//!   silence;
//! * an intersection issue #234's posture proves **uninhabited** is silence —
//!   the FinalClass&MockObject shape, on the DEFAULT surface.
//!
//! The last is the false-positive guard, and it is the one that fails if the
//! collapse ships: the natural implementation of `final Svc & Mock` is "this
//! collapses to nothing", and a lane with no conjunct to look a method up on finds
//! *every* method absent. Under a project running `dg/bypass-finals` — where the
//! mock subclass genuinely exists — that is a false positive on the proof layer.

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

/// Every declared-receiver method finding, whichever id A13 routed it to. The
/// evidence phrasing is what tells the lane apart from S2's own emissions on the
/// shared proof-layer id.
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

/// Two ordinary (non-final) classes, so the #234 emptiness leg does not run and the
/// intersection is plainly inhabited. `Svc::run()` is on one conjunct, `Mock::spy()`
/// on the other, and `gone()` is on neither.
const INHABITED: &str = "<?php
class Svc { public function run(): int { return 1; } }
class Mock { public function spy(): int { return 2; } }
";

fn inhabited_calling(body: &str) -> String {
    format!("{INHABITED}/** @param Svc&Mock $o */\nfunction f($o): void {{ {body} }}\n")
}

// ---------------------------------------------------------------------------
// Member lookup over an inhabited intersection is the UNION of the arms.
// ---------------------------------------------------------------------------

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
    // Both conjuncts are named, joined as the conjunction they are — a reader has to
    // be able to see WHICH type the claim closed over.
    assert!(m.contains("Svc&Mock"), "the evidence names both conjuncts: {m}");
}

#[test]
fn a_property_on_either_conjunct_resolves_and_on_neither_fires() {
    // The property lane admits only `Verified` premises (ADR-0078's calibration
    // boundary), so the receiver is spelled as PHP 8.1's NATIVE intersection type —
    // runtime-enforced, which is exactly what that boundary asks for.
    // The property lane admits only `Verified` premises (ADR-0078's calibration
    // boundary), so the receiver is spelled as PHP 8.1's NATIVE intersection type —
    // runtime-enforced, which is exactly what that boundary asks for.
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

// ---------------------------------------------------------------------------
// A `Maybe` on any conjunct is silence.
// ---------------------------------------------------------------------------

#[test]
fn an_unresolvable_conjunct_is_silence() {
    // `Unknown` is not declared anywhere, so its hierarchy cannot be enumerated and
    // its descendant set cannot be closed. It could declare `gone()`, so no claim
    // about the intersection holds.
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
    // `Mock` is non-final and has a descendant that DOES declare `gone()`, so the
    // §8 descendant-closure leg refuses the conjunct — and refusing one conjunct
    // refuses the arm.
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
    // `Svc&callable` names a constraint this lane cannot close over: the receiver
    // may be a closure, so a method-absence claim does not hold.
    let src = "<?php
class Svc { public function run(): int { return 1; } }
/** @param Svc&callable $o */
function f($o): void { $o->gone(); }
";
    let d = default_methods(src);
    assert!(d.is_empty(), "a non-class conjunct silences the lane: {d:?}");
}

// ---------------------------------------------------------------------------
// Issue #234's inhabitance rule — the false-positive guard.
// ---------------------------------------------------------------------------

/// The shape issue #234 is about: a `final` service class and an unrelated mock
/// marker. Under an *enforced* `final`, `Svc` admits no subtype, so every value of
/// `Svc&Mock` would have exact class `Svc` and would therefore already be a `Mock`
/// — and `is_a(Svc, Mock)` is provably `No`. The intersection is empty.
const FINAL_MOCK: &str = "<?php
interface Mock { public function spy(): int; }
final class Svc { public function run(): int { return 1; } }
/** @param Svc&Mock $o */
function f($o): void { $o->gone(); }
";

/// **The regression guard.** `gone()` is on neither conjunct, so the ladder would
/// fire — but the arm is provably uninhabited under the default posture, and a type
/// no value inhabits is no receiver. Silence.
///
/// This test fails if the collapse ships: an implementation that folds
/// `final Svc & Mock` to nothing and then looks members up on nothing finds every
/// member absent and fires here.
#[test]
fn an_uninhabited_intersection_is_silent_on_the_default_surface() {
    let d = default_methods(FINAL_MOCK);
    assert!(
        d.is_empty(),
        "FinalClass&MockObject is uninhabited under the enforced default — a claim about \
         it is vacuous, and firing is the false positive #234 guards: {d:?}"
    );
}

/// The same source under a declared `final-keyword = "stripped"`: the loader really
/// does remove the keyword, the mock subclass exists, so the intersection is
/// INHABITED and the ordinary either-arm rule applies again. `gone()` is on neither
/// conjunct, so it fires.
///
/// This is what makes the posture observable rather than decorative — and it is the
/// direction that proves the silence above comes from the inhabitance judgment and
/// not from the lane quietly refusing every intersection.
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

/// The posture withdraws an emptiness proof; it never adds a claim. A member that
/// resolves on a conjunct stays silent under BOTH postures — `Stripped` must not
/// turn into a licence to fire on things that are there.
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

/// A lone `final` arm is not a conflict with itself (`is_a(F, F)` is reflexively
/// `Yes`), and two final classes in the same hierarchy are not a conflict either.
/// The emptiness leg must not swallow an intersection that is merely redundant.
#[test]
fn a_final_arm_that_conflicts_with_nothing_still_fires() {
    // `Svc` is final and implements `Mock`, so `is_a(Svc, Mock)` is `Yes` — no
    // conflict, the intersection is inhabited (it is just `Svc`), and `gone()` is
    // absent from both.
    let src = "<?php
interface Mock { public function spy(): int; }
final class Svc implements Mock { public function run(): int { return 1; } public function spy(): int { return 2; } }
/** @param Svc&Mock $o */
function f($o): void { $o->gone(); }
";
    let d = default_methods(src);
    assert_eq!(d.len(), 1, "a final arm that is-a the other arm is no conflict: {d:?}");
}

