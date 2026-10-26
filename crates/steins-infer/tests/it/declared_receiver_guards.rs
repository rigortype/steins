//! Issue #1095 — the declared-receiver lane reads its guards. A positive class guard
//! (`instanceof`, `@phpstan-assert`, assert-if-true, `assert()`) admits only the
//! candidates of an arm that may be the guard class; an arm with none drops out of the
//! union, and each admitted candidate is judged by its whole chain, so a member it
//! inherits from an intermediate class the guard excludes still counts.
//!
//! Row numbers are those of the decision table in the design comment on #1095; each
//! fixture is the architect's witness file with its runtime driver removed. PHP 8.5's
//! behaviour on each row is in the table.

use std::path::PathBuf;

use steins_db::{Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, FinalKeyword, Folder,
    PHPDOC_UNDEFINED_METHOD_ID, PROPERTY_MAYBE_UNDEFINED_ID, PROPERTY_UNDEFINED_ID,
    check_project_with_postures,
};

/// A ready boot surface: the A9 family gate is open and no name is a resident
/// homonym, so the A2ii leg never silences on its own.
struct Boot;

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
        _strict: bool,
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

fn check(src: &str) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let layout = ProjectLayout::new(PathBuf::from("/proj"), vec![]);
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    check_project_with_postures(&db, project, &mut Boot, true, FinalKeyword::Enforced)
}

/// The declared-receiver findings of `fixture` plus one function, rendered as
/// `id subject` (the subject is the arm list the message names).
fn lane(fixture: &str, f: &str) -> Vec<String> {
    check(&format!("<?php\n{fixture}\n{f}\n"))
        .into_iter()
        .filter(|d| d.message.contains("declared receiver"))
        .map(|d| {
            let subject = match d.id {
                id if id == CALL_UNDEFINED_METHOD_ID || id == PHPDOC_UNDEFINED_METHOD_ID => d
                    .message
                    .trim_start_matches("call to undefined method ")
                    .split("::")
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                _ => String::new(),
            };
            format!("{} {subject}", d.id).trim_end().to_owned()
        })
        .collect()
}

/// Whether the dump inside `f` is emitted: the body it sits in is live.
fn dumps(fixture: &str, f: &str) -> usize {
    check(&format!("<?php\n{fixture}\n{f}\n")).iter().filter(|d| d.id == DEBUG_TYPE_ID).count()
}

/// Assert each `(function, expected findings)` row over `fixture`.
fn rows(fixture: &str, cases: &[(&str, &[&str])]) {
    for (f, want) in cases {
        let got = lane(fixture, f);
        let want: Vec<String> = want.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(got, want, "{f}");
    }
}


/// `d1b_class_arm_clean.php`: a class arm, hierarchy closed, no magic descendant.
const CLASS_CLEAN: &str = "
interface T {} interface U {}
final class Fin {}
class Solo {}
class SoloT extends Solo implements T {}
class Mid extends Solo { public function zz(): int { return 7; } }
class Leaf extends Mid implements U {}
";

#[test]
fn a_class_arm_reads_its_guard() {
    rows(
        CLASS_CLEAN,
        &[
            // Row 1: no guard, `qq()` nowhere.
            ("function c01(Solo $x): int { return $x->qq(); }", &["call.undefined-method Solo"]),
            // Row 4: `T` admits SoloT.
            (
                "function c02(Solo $x): int { if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method Solo"],
            ),
            // Row 12: no Solo is a `Fin`; PHP never enters the branch.
            ("function c03(Solo $x): int { if ($x instanceof Fin) { return $x->qq(); } return 0; }", &[]),
            // Row 6: `U` admits Leaf only, which holds `zz()` through the excluded Mid.
            ("function c04(Solo $x): int { if ($x instanceof U) { return $x->zz(); } return 0; }", &[]),
            // Row 5: `T` admits SoloT, whose chain lacks `zz()`; only the excluded Mid
            // declares it.
            (
                "function c05(Solo $x): int { if ($x instanceof T) { return $x->zz(); } return 0; }",
                &["call.undefined-method Solo"],
            ),
            // Row 16: the guard is the arm's own class.
            (
                "function c06(Solo $x): int { if ($x instanceof Solo) { return $x->qq(); } return 0; }",
                &["call.undefined-method Solo"],
            ),
            // Row 17: a rebinding drops the guard with the lane.
            (
                "function c07(Solo $x, Solo $y): int { if ($x instanceof T) { $x = $y; return $x->qq(); } return 0; }",
                &["call.undefined-method Solo"],
            ),
            // Row 18: after the join the guard holds on no path's arm.
            ("function c08(Solo $x): int { if ($x instanceof T) { echo ''; } return $x->zz(); }", &[]),
            // Row 5 through the else side of `!instanceof`.
            (
                "function c09(Solo $x): int { if (!$x instanceof T) { return 0; } return $x->zz(); }",
                &["call.undefined-method Solo"],
            ),
            // Row 19: a stored guard narrows nothing (A5, #933).
            ("function c10(Solo $x): int { $ok = $x instanceof T; if ($ok) { return $x->zz(); } return 0; }", &[]),
        ],
    );
}

/// `d1_class_arm.php`: a class arm whose descendants carry every member source.
const CLASS_SOURCES: &str = "
interface T {} interface U {} interface HasZ { public function zz(): int; }
final class Fin {}
class Solo {}
class SoloT extends Solo implements T {}
class SoloZ extends Solo implements HasZ { public function zz(): int { return 1; } }
class Mid extends Solo { public function zz(): int { return 7; } }
class Leaf extends Mid implements U {}
class MagicMid extends Solo { public function __call(string $n, array $a): int { return 5; } }
class MagicLeaf extends MagicMid implements U {}
class PrivRoot extends Solo { private function pm(): int { return 3; } }
class PrivLeaf extends PrivRoot implements T {}
class StatRoot extends Solo { public static function sm(): int { return 1; } }
class StatLeaf extends StatRoot implements U {}
abstract class AbsMid extends Solo { abstract public function am(): int; }
class AbsLeaf extends AbsMid implements T { public function am(): int { return 9; } }
trait TT { public function tm(): int { return 4; } }
class TraitLeaf extends Solo implements U { use TT; }
class SoloC extends Solo implements \\Countable { public function count(): int { return 0; } }
class SoloTU extends Solo implements T, U { public function tu(): int { return 2; } }
enum En: int implements T { case X = 1; }
";

#[test]
fn an_admitted_candidate_is_judged_by_its_whole_chain() {
    rows(
        CLASS_SOURCES,
        &[
            // Row 2: unguarded, MagicMid's `__call` and Mid's `zz()` leave it open.
            ("function r01(Solo $x): int { return $x->zz(); }", &[]),
            // Row 4 / 14: `T` admits SoloT, PrivLeaf, AbsLeaf (the enum En is no Solo).
            (
                "function r02(Solo $x): int { if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method Solo"],
            ),
            // Row 12.
            ("function r03(Solo $x): int { if ($x instanceof Fin) { return $x->qq(); } return 0; }", &[]),
            ("function r04(Solo $x): int { if ($x instanceof HasZ) { return $x->zz(); } return 0; }", &[]),
            // Rows 6, 7: inherited from an excluded intermediate, `zz()` and `__call`.
            ("function r05(Solo $x): int { if ($x instanceof U) { return $x->zz(); } return 0; }", &[]),
            ("function r06(Solo $x): int { if ($x instanceof U) { return $x->anything(); } return 0; }", &[]),
            // Rows 8-11: private, static, abstract-then-implemented, trait-provided.
            ("function r07(Solo $x): int { if ($x instanceof T) { return $x->pm(); } return 0; }", &[]),
            ("function r08(Solo $x): int { if ($x instanceof U) { return $x->sm(); } return 0; }", &[]),
            ("function r09(Solo $x): int { if ($x instanceof T) { return $x->am(); } return 0; }", &[]),
            ("function r10(Solo $x): int { if ($x instanceof U) { return $x->tm(); } return 0; }", &[]),
            // Row 13: a builtin guard admits SoloC.
            (
                "function r11(Solo $x): int { if ($x instanceof \\Countable) { return $x->qq(); } return 0; }",
                &["call.undefined-method Solo"],
            ),
            // Row 15: nested guards admit SoloTU only.
            (
                "function r13(Solo $x): int { if ($x instanceof T) { if ($x instanceof U) { return $x->tu(); } } return 0; }",
                &[],
            ),
            (
                "function r13b(Solo $x): int { if ($x instanceof T) { if ($x instanceof U) { return $x->qq(); } } return 0; }",
                &["call.undefined-method Solo"],
            ),
        ],
    );
}

/// `d2_interface_arm.php`: an interface arm, hierarchy closed.
const INTERFACE: &str = "
interface A { public function a(): int; } interface T {} interface U {}
interface NoImpl {}
class AT implements A, T { public function a(): int { return 1; } }
class OnlyA implements A { public function a(): int { return 1; } public function zz(): int { return 2; } }
class Mid implements A { public function a(): int { return 1; } public function mm(): int { return 7; } }
class Leaf extends Mid implements T {}
class AC implements A, \\Countable { public function a(): int { return 1; } public function count(): int { return 0; } }
enum En: int implements T { case X = 1; }
class OnlyB {}
";

#[test]
fn an_interface_arm_reads_its_guard() {
    rows(
        INTERFACE,
        &[
            // Rows 22, 23, 24.
            ("function i01(A $x): int { return $x->qq(); }", &["call.undefined-method A"]),
            ("function i01b(A $x): int { return $x->zz(); }", &[]),
            ("function i02(NoImpl $x): int { return $x->qq(); }", &["call.undefined-method NoImpl"]),
            // Row 25: `T` admits AT and Leaf; only the excluded OnlyA declares `zz()`.
            (
                "function i03(A $x): int { if ($x instanceof T) { return $x->zz(); } return 0; }",
                &["call.undefined-method A"],
            ),
            // Row 26.
            ("function i06(A $x): int { if ($x instanceof T) { return $x->mm(); } return 0; }", &[]),
            // Rows 27, 28: builtin and enum-implemented guards keep their findings.
            (
                "function i07(A $x): int { if ($x instanceof \\Countable) { return $x->qq(); } return 0; }",
                &["call.undefined-method A"],
            ),
            (
                "function i08(A $x): int { if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method A"],
            ),
            // Row 29.
            (
                "function i10(A $x): int { if ($x instanceof A) { return $x->qq(); } return 0; }",
                &["call.undefined-method A"],
            ),
            // Row 30: a class nothing declares, in a clear universe, admits nothing.
            ("function i11(A $x): int { if ($x instanceof NoSuchClass) { return $x->qq(); } return 0; }", &[]),
            // Row 31: no implementor of A is a NoImpl.
            ("function i12(A $x): int { if ($x instanceof NoImpl) { return $x->qq(); } return 0; }", &[]),
            // Row 32: declared on the arm itself.
            ("function i14(A $x): int { if ($x instanceof T) { return $x->a(); } return 0; }", &[]),
            // Row 48: OnlyB is no `T`, so its arm drops out and the finding names A.
            (
                "function i15(A|OnlyB $x): int { if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method A"],
            ),
            ("function i15u(A|OnlyB $x): int { return $x->qq(); }", &["call.undefined-method A|OnlyB"]),
        ],
    );
}

#[test]
fn a_guard_excludes_the_enum_that_held_the_arm_open() {
    // Row 37, 38 (`d2c_enum_candidate.php`).
    let fixture = "
interface A { public function a(): int; } interface T {}
class AT implements A, T { public function a(): int { return 1; } }
enum EA implements A { case Y; public function a(): int { return 1; } }
";
    rows(
        fixture,
        &[
            ("function k01(A $x): int { return $x->qq(); }", &[]),
            (
                "function k02(A $x): int { if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method A"],
            ),
        ],
    );
}

/// `d3_intersection_union.php`: intersection arms, unions, and the assert guards.
const INTER: &str = "
interface A { public function a(): int; } interface B {} interface T { public function tm(): int; } interface U {}
interface P {} interface Q {}
class Both implements A, B { public function a(): int { return 1; } }
class All implements A, B, T { public function a(): int { return 1; } public function tm(): int { return 2; } }
class Solo {}
class SoloT extends Solo implements T { public function tm(): int { return 3; } }
class Foo {}
class FooC extends Foo implements \\Countable { public function count(): int { return 0; } }
/** @phpstan-assert T $v */
function assertT(mixed $v): void { if (!$v instanceof T) { throw new \\InvalidArgumentException('not T'); } }
/** @phpstan-assert U $v */
function assertU(mixed $v): void { if (!$v instanceof U) { throw new \\InvalidArgumentException('not U'); } }
/** @phpstan-assert-if-true T $v */
function isT(mixed $v): bool { return $v instanceof T; }
";

#[test]
fn an_intersection_arm_reads_its_guard() {
    rows(
        INTER,
        &[
            // Rows 39, 40.
            ("function x01(A&B $x): int { return $x->qq(); }", &["call.undefined-method A&B"]),
            ("function x02(P&Q $x): int { return $x->qq(); }", &["call.undefined-method P&Q"]),
            // Row 41: `T` admits All.
            (
                "function x03(A&B $x): int { if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method A&B"],
            ),
            ("function x03b(A&B $x): int { if ($x instanceof T) { return $x->tm(); } return 0; }", &[]),
            // Row 42: no A&B is a U.
            ("function x04(A&B $x): int { if ($x instanceof U) { return $x->qq(); } return 0; }", &[]),
            // Row 43: the guard is a conjunct.
            (
                "function x05(A&B $x): int { if ($x instanceof A) { return $x->qq(); } return 0; }",
                &["call.undefined-method A&B"],
            ),
            // Row 49: a builtin conjunct, with a project inhabitant.
            ("function x10(Foo&\\Countable $x): int { return $x->qq(); }", &["call.undefined-method Foo&Countable"]),
            ("function x10b(Foo&\\Countable $x): int { return $x->count(); }", &[]),
            // Row 50: nested guards admit nothing.
            (
                "function x11(A&B $x): int { if ($x instanceof T) { if ($x instanceof U) { return $x->qq(); } } return 0; }",
                &[],
            ),
            // Row 51: a copy carries the guards.
            (
                "function x12(A&B $x): int { if ($x instanceof T) { $y = $x; return $y->qq(); } return 0; }",
                &["call.undefined-method A&B"],
            ),
            // Row 52: one path unguarded.
            (
                "function x13(A&B $x, bool $b): int { if ($b) { if (!$x instanceof T) { return 0; } } return $x->qq(); }",
                &["call.undefined-method A&B"],
            ),
        ],
    );
}

#[test]
fn a_guard_that_only_a_docblock_holds_routes_to_the_phpdoc_id() {
    rows(
        INTER,
        &[
            // Row 44: the assert excludes Both; the finding rests on it.
            ("function x06(A&B $x): int { assertT($x); return $x->qq(); }", &["phpdoc.undefined-method A&B"]),
            ("function x06b(A&B $x): int { if (isT($x)) { return $x->qq(); } return 0; }", &["phpdoc.undefined-method A&B"]),
            // Row 45: the assert admits nothing.
            ("function x07(A&B $x): int { assertU($x); return $x->qq(); }", &[]),
            // The same guard held by a runtime test too is Verified.
            (
                "function x06c(A&B $x): int { assertT($x); if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method A&B"],
            ),
            // A join keeps the guard at the weaker stratum.
            (
                "function x06d(A&B $x, bool $b): int { if ($b) { assertT($x); } elseif (!$x instanceof T) { return 0; } return $x->qq(); }",
                &["phpdoc.undefined-method A&B"],
            ),
            // A native `assert()` is a runtime test.
            ("function x06e(A&B $x): int { assert($x instanceof T); return $x->qq(); }", &["call.undefined-method A&B"]),
        ],
    );
}

#[test]
fn a_dead_arm_drops_out_of_the_union() {
    rows(
        INTER,
        &[
            // Row 46: both arms live, both lack `qq()`.
            (
                "function x08((A&B)|Solo $x): int { if ($x instanceof T) { return $x->qq(); } return 0; }",
                &["call.undefined-method A&B|Solo"],
            ),
            // Row 47: both dead.
            ("function x09((A&B)|Solo $x): int { if ($x instanceof U) { return $x->qq(); } return 0; }", &[]),
            // A live arm that declares the member keeps the union silent.
            ("function x08b((A&B)|Solo $x): int { if ($x instanceof T) { return $x->tm(); } return 0; }", &[]),
        ],
    );
    // The k3 residue of #1089: no class is A&B, so that arm is dead under `T`.
    let k3 = "
interface A {} interface B {} interface T {}
class Solo {}
class SoloT extends Solo implements T {}
";
    rows(
        k3,
        &[(
            "function k8((A&B)|Solo $x): int { if ($x instanceof T) { return $x->zz(); } return 0; }",
            &["call.undefined-method Solo"],
        )],
    );
}

/// `g1_guards.php` / `g3_magic_desc.php`: the true positives A4a lost to an
/// excluded descendant's `__call` (#1102), and the silences beside them.
const MAGIC_DESCENDANT: &str = "
interface A { public function a(): int; } interface B {}
interface T { public function tm(): int; } interface U { public function um(): int; }
class Both implements A, B { public function a(): int { return 1; } }
class AT implements A, B, T, U { public function a(): int { return 1; } public function tm(): int { return 2; } public function um(): int { return 3; } }
class Magic { public function __call($n, $a) { return 5; } public function __get($n) { return 6; } }
class MagicA extends Magic implements A { public function a(): int { return 1; } }
class WithP implements A { public int $p = 4; public function a(): int { return 1; } }
interface HasNoP {}
";

#[test]
fn an_excluded_magic_descendant_no_longer_hides_the_finding() {
    rows(
        MAGIC_DESCENDANT,
        &[
            ("function u1(A $x): int { if ($x instanceof T) { return $x->tm(); } return 0; }", &[]),
            // g1:12, g1:15.
            (
                "function u2(A $x): int { if ($x instanceof T) { return $x->zz(); } return 0; }",
                &["call.undefined-method A"],
            ),
            (
                "function n2(A $x): int { if ($x instanceof T) { if ($x instanceof U) { return $x->zz(); } } return 0; }",
                &["call.undefined-method A"],
            ),
            // Unguarded, MagicA's `__call` keeps it open.
            ("function n0(A $x): int { return $x->zz(); }", &[]),
            // The guard admits MagicA itself.
            ("function m1(A $x): int { if ($x instanceof Magic) { return $x->anything(); } return 0; }", &[]),
            ("function b4(A&B $x): int { if ($x instanceof A) { return $x->zz(); } return 0; }", &[]),
        ],
    );
}

#[test]
fn guard_spellings_meet_as_class_identities() {
    // `g2_losttp.php`, `g5_builtin_guard.php`, `h2_guard_rule.php`.
    let fixture = "
interface A { public function a(): int; }
interface T { public function tm(): int; }
class AT implements A, T { public function a(): int { return 1; } public function tm(): int { return 2; } }
class OnlyA implements A { public function a(): int { return 1; } }
class C2 { public function c(): int { return 1; } }
class Sub2 extends C2 implements A { public function a(): int { return 1; } }
final class FinA implements A { public function a(): int { return 1; } }
class AStr implements A { public function a(): int { return 1; } public function __toString(): string { return ''; } }
class RealG { public function rg(): int { return 5; } }
class_alias(RealG::class, 'AliasG');
class DR extends RealG implements A { public function a(): int { return 1; } }
class GS { public static function sm(): int { return 1; } }
class DS extends GS implements A { public function a(): int { return 1; } }
";
    rows(
        fixture,
        &[
            ("function c1(A $x): int { if ($x instanceof C2) { return $x->zz(); } return 0; }", &["call.undefined-method A"]),
            ("function s2(A $x): int { if ($x instanceof OnlyA) { return $x->zz(); } return 0; }", &["call.undefined-method A"]),
            ("function s3(A $x): int { if ($x instanceof FinA) { return $x->zz(); } return 0; }", &["call.undefined-method A"]),
            ("function l1(A $x): int { if ($x instanceof t) { return $x->zz(); } return 0; }", &["call.undefined-method A"]),
            // `Stringable` through `__toString`, implicit.
            (
                "function st(A $x): int { if ($x instanceof \\Stringable) { return $x->zz(); } return 0; }",
                &["call.undefined-method A"],
            ),
            // Through the alias: DR holds `rg()`, lacks `nope()`.
            ("function c1a(A $x): int { if ($x instanceof AliasG) { return $x->rg(); } return 0; }", &[]),
            ("function c2a(A $x): int { if ($x instanceof \\AliasG) { return $x->nope(); } return 0; }", &["call.undefined-method A"]),
            // A static method on the guard class.
            ("function s1(A $x): int { if ($x instanceof GS) { return $x->sm(); } return 0; }", &[]),
        ],
    );
}

#[test]
fn a_dead_intersection_stays_silent_on_every_guard_site() {
    // `k1_assert_dead.php`, `m1_marker.php`, `m3_loop_join.php`: no class is A&B&T.
    let fixture = format!(
        "{}{}",
        "
interface A { public function a(): int; } interface B {}
interface T { public function tm(): int; }
final class Fin { public function tm(): int { return 3; } }
class Both implements A, B { public function a(): int { return 1; } }
/** @phpstan-assert T $v */
function assertT(mixed $v): void { if (!$v instanceof T) { throw new \\InvalidArgumentException(); } }
/** @phpstan-assert Fin $v */
function assertFin(mixed $v): void { if (!$v instanceof Fin) { throw new \\InvalidArgumentException(); } }
",
        "function idem($v) { return $v; }"
    );
    rows(
        &fixture,
        &[
            ("function k1(A&B $x): int { assertT($x); return $x->tm(); }", &[]),
            ("function k2(A&B $x): int { assertFin($x); return $x->tm(); }", &[]),
            ("function k3((A&B)|Both $x): int { assertT($x); return $x->tm(); }", &[]),
            ("function k4((A&B)|Both $x): int { if ($x instanceof T) { return $x->tm(); } return 0; }", &[]),
            ("function c1(A&B $x): int { if ($x instanceof T) { $y = $x; return $y->tm(); } return 0; }", &[]),
            ("function c3(A&B $x): int { if ($x instanceof T) { $f = function () use ($x) { return $x->tm(); }; return $f(); } return 0; }", &[]),
            ("function c5(A&B $x): int { if ($x instanceof T) { [$y] = [$x]; return $y->tm(); } return 0; }", &[]),
            ("function c6(A&B $x): int { assertT($x); $y = idem($x); return $y->tm(); }", &[]),
            ("function c7(A&B $x): int { return match (true) { $x instanceof T => $x->tm(), default => 0 }; }", &[]),
            ("function c8(A&B $x): int { return $x instanceof T ? $x->tm() : 0; }", &[]),
            ("function c11(A&B $x): int { assert($x instanceof T); return $x->tm(); }", &[]),
            ("function c14(A&B $x): int { if ($x instanceof Fin) { return $x->tm(); } return 0; }", &[]),
            ("function l1(A&B $x, int $n): int { $s = 0; while ($n-- > 0) { if ($x instanceof T) { $s += $x->tm(); } } return $s; }", &[]),
            ("function j1(A&B $x, bool $b): int { if ($b) { if (!$x instanceof T) { return 0; } } else { if (!$x instanceof T) { return 1; } } return $x->tm(); }", &[]),
            // Kept: a stored guard, a rebinding, an unguarded path, an unguarded copy.
            ("function c13(A&B $x): int { $ok = $x instanceof T; if ($ok) { return $x->tm(); } return 0; }", &["call.undefined-method A&B"]),
            ("function s1(A&B $x, A&B $z): int { if ($x instanceof T) { $x = $z; return $x->zz(); } return 0; }", &["call.undefined-method A&B"]),
            ("function j2(A&B $x, bool $b): int { if ($b) { if (!$x instanceof T) { return 0; } } return $x->zz(); }", &["call.undefined-method A&B"]),
            ("function j3(A&B $x): int { $y = $x; if ($x instanceof T) { return $y->zz(); } return 0; }", &["call.undefined-method A&B"]),
            ("function j4(A&B $x): int { if ($x instanceof T) { $y = $x; } else { $y = $x; } return $y->zz(); }", &["call.undefined-method A&B"]),
        ],
    );
}

/// `d4_property_lane.php`: the property lane under the same reading.
const PROPS: &str = "
class Root {}
interface Q {} interface HasNoP {}
class WithP extends Root { public int $p = 4; }
class Plain extends Root implements Q {}
class PlainKid extends Plain {}
class Magic extends Root implements HasNoP { public function __get(string $n): int { return 6; } }
class HasP implements Q { public int $p = 1; }
/** @phpstan-assert Q $v */
function assertQ(mixed $v): void { if (!$v instanceof Q) { throw new \\InvalidArgumentException(); } }
interface Nobody {}
";

#[test]
fn the_property_lane_reads_its_guard() {
    let ids = |f: &str| -> Vec<&'static str> {
        check(&format!("<?php\n{PROPS}\n{f}\n"))
            .into_iter()
            .filter(|d| d.id == PROPERTY_UNDEFINED_ID || d.id == PROPERTY_MAYBE_UNDEFINED_ID)
            .map(|d| d.id)
            .collect()
    };
    // Row 53.
    assert_eq!(ids("function p01(Root $x): ?int { return $x->p; }"), Vec::<&str>::new());
    // Row 54: `Q` admits Plain and PlainKid.
    assert_eq!(
        ids("function p02(Root $x): ?int { if ($x instanceof Q) { return $x->p; } return 0; }"),
        vec![PROPERTY_UNDEFINED_ID]
    );
    // Row 55: Root's admitted lack it, HasP declares it.
    assert_eq!(
        ids("function p03(Root|HasP $x): ?int { if ($x instanceof Q) { return $x->p; } return 0; }"),
        vec![PROPERTY_MAYBE_UNDEFINED_ID]
    );
    // Row 56: `__get` on the admitted candidate.
    assert_eq!(
        ids("function p04(Root $x): ?int { if ($x instanceof HasNoP) { return $x->p; } return 0; }"),
        Vec::<&str>::new()
    );
    // Row 57: a `property_exists` vouch.
    assert_eq!(
        ids("function p05(Root $x): ?int { if ($x instanceof Q && property_exists($x, 'p')) { return $x->p; } return 0; }"),
        Vec::<&str>::new()
    );
    // A dead arm drops out: HasP is no `Nobody`, nor is any Root.
    assert_eq!(
        ids("function p06(Root|HasP $x): ?int { if ($x instanceof Nobody) { return $x->p; } return 0; }"),
        Vec::<&str>::new()
    );
    // The property family has no contract-layer twin: an assert-excluded candidate
    // leaves it silent.
    assert_eq!(ids("function p07(Root $x): ?int { assertQ($x); return $x->p; }"), Vec::<&str>::new());
}

#[test]
fn the_method_exists_fold_drops_dead_arms_and_never_answers_from_an_assert() {
    let fixture = "
interface A { public function a(): int; } interface T {} interface Nobody {}
class AT implements A, T { public function a(): int { return 1; } }
class OnlyA implements A { public function a(): int { return 1; } public function zz(): int { return 2; } }
class OnlyB {}
/** @phpstan-assert T $v */
function assertT(mixed $v): void { if (!$v instanceof T) { throw new \\InvalidArgumentException(); } }
";
    // Unguarded, OnlyA declares `zz()`: the body is live.
    let f = "function f(A $x): void { if (method_exists($x, 'zz')) { \\PHPStan\\dumpType($x); } }";
    assert_eq!(dumps(fixture, f), 1);
    // Under `T` only AT is admitted, which lacks `zz()`: the guard is false.
    let f = "function f(A|OnlyB $x): void { if ($x instanceof T) { if (method_exists($x, 'zz')) { \\PHPStan\\dumpType($x); } } }";
    assert_eq!(dumps(fixture, f), 0);
    // Every arm dead: nothing is proven, the body stays live.
    let f = "function f(A $x): void { if ($x instanceof Nobody) { if (method_exists($x, 'zz')) { \\PHPStan\\dumpType($x); } } }";
    assert_eq!(dumps(fixture, f), 1);
    // An assert-excluded candidate: the fold may not kill a region on a docblock.
    let f = "function f(A $x): void { assertT($x); if (method_exists($x, 'zz')) { \\PHPStan\\dumpType($x); } }";
    assert_eq!(dumps(fixture, f), 1);
}

/// `assert()` reads as `if (!cond) throw`, `Verified` (ADR-0052's 2026-07-25
/// amendment, an owner ruling): the guard it records premises proof-layer findings,
/// unlike a `@phpstan-assert` tag. Under `zend.assertions=-1` the call would run on
/// `OtherZ`; the amendment assigns that to the operator who disabled the check.
#[test]
fn an_assert_statement_guard_premises_proof_layer_findings() {
    let src = "<?php
interface A {} interface T {}
class AT implements A, T {}
class OtherZ implements A { public function zz(): int { return 8; } public int $p = 1; }
function b1(A $x): int { assert($x instanceof T); return $x->zz(); }
function b2(A $x): int { assert($x instanceof T); return $x->p; }
";
    let found: Vec<(u32, &str)> = check(src)
        .into_iter()
        .filter(|d| d.message.contains("declared receiver"))
        .map(|d| (d.line, d.id))
        .collect();
    assert_eq!(found, vec![(5, CALL_UNDEFINED_METHOD_ID), (6, PROPERTY_UNDEFINED_ID)]);
}

// ---------------------------------------------------------------------------
// Issue #981: `method_exists($x, 'm')` narrows the receiver's arms by the lane's
// per-arm verdict. Where the call held, an arm every inhabitant of which lacks `m`
// (`Absent`) is deleted; where it failed, one every inhabitant of which declares `m`
// (`Declared`); a `Dead` arm on both sides; every other arm stays.
// ---------------------------------------------------------------------------

/// The dumped types inside `f`, in source order.
fn dumped(fixture: &str, f: &str) -> Vec<String> {
    check(&format!("<?php\n{fixture}\n{f}\n"))
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.trim_start_matches("dumped type: ").to_owned())
        .collect()
}

/// Assert each `(function, dumped types, lane findings)` row over `fixture`.
fn narrows(fixture: &str, cases: &[(&str, &[&str], &[&str])]) {
    for (f, types, findings) in cases {
        let types: Vec<String> = types.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(dumped(fixture, f), types, "dumps of {f}");
        let findings: Vec<String> = findings.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(lane(fixture, f), findings, "findings of {f}");
    }
}

/// `d5_method_exists.php` and `w984.php` from #981's witness table, with #1103's
/// abstract class.
const MEMBER_SOURCES: &str = "
class N { public function go(): string { return 'go'; } }
class M { public function other(): string { return 'o'; } }
class W { public function __call(string $n, array $a): string { return 'magic'; } }
class S { public static function sm(): string { return 'static'; } }
class P { private function pm(): string { return 'private'; } }
abstract class Ab { abstract public function am(): string; }
class AbK extends Ab { public function am(): string { return 'abstract-impl'; } }
trait TG { public function tg(): string { return 'trait'; } }
class UsesT { use TG; }
class Open {}
class OpenKid extends Open { public function go(): string { return 'kid'; } }
interface I {} interface T {}
class Mid implements I { public function zz(): int { return 7; } }
class Leaf extends Mid implements T {}
interface HasM { public function m(): string; }
abstract class Abs implements HasM {}
class AbsK extends Abs { public function m(): string { return 'k'; } }
interface Nobody {}
";

/// `function f(<sig> $x)` testing `method_exists($x, '<m>')`, with a dump on each side.
fn guarded(sig: &str, m: &str, then: &str, other: &str) -> String {
    let d = "\\PHPStan\\dumpType($x);";
    format!("function f({sig} $x): string {{ if (method_exists($x, '{m}')) {{ {d} {then} }} {d} {other} }}")
}

#[test]
fn method_exists_narrows_each_arm_by_its_verdict() {
    let rows: Vec<(String, &[&str])> = vec![
        // e01: N declares `go`, M lacks it.
        (guarded("N|M", "go", "return $x->go();", "return $x->other();"), &["N", "M"]),
        // e04: a non-final arm a descendant declares `go` on is kept on both sides.
        (guarded("Open|M", "go", "return 'y';", "return 'n';"), &["Open", "Open|M"]),
        // e06: `__call` does not make `method_exists` true: no arm holds `go`, the fold
        // proves the guard false and the then side is dead.
        (guarded("W|M", "go", "return 'y';", "return 'n';"), &["W|M"]),
        // e07-e10: static, private, abstract and trait-provided methods are declared.
        (guarded("S|M", "sm", "return 'y';", "return 'n';"), &["S", "M"]),
        (guarded("P|M", "pm", "return 'y';", "return 'n';"), &["P", "M"]),
        (guarded("Ab|M", "am", "return 'y';", "return 'n';"), &["Ab", "M"]),
        (guarded("UsesT|M", "tg", "return 'y';", "return 'n';"), &["UsesT", "M"]),
        // #1103: an abstract class that declares `m` only through its interface, and the
        // interface itself.
        (guarded("Abs|M", "m", "return 'y';", "return 'n';"), &["Abs", "M"]),
        (guarded("HasM|M", "m", "return 'y';", "return 'n';"), &["HasM", "M"]),
        // A null arm stays on both sides: `method_exists(null, …)` throws.
        (guarded("?N", "go", "return 'y';", "return 'n';"), &["N|null", "null"]),
    ];
    let cases: Vec<(&str, &[&str], &[&str])> =
        rows.iter().map(|(f, types)| (f.as_str(), *types, &[] as &[&str])).collect();
    narrows(MEMBER_SOURCES, &cases);
    // An anonymous subclass of Q may declare `go`: Q is kept on both sides, M only
    // where the call failed.
    let fixture = format!(
        "{MEMBER_SOURCES}class Q {{}}\n$anon = new class extends Q {{ public function go(): string {{ return 'a'; }} }};"
    );
    let f = "function s16(Q|M $x): string { if (method_exists($x, 'go')) { \\PHPStan\\dumpType($x); return 'y'; } \\PHPStan\\dumpType($x); return 'n'; }";
    assert_eq!(dumped(&fixture, f), ["Q", "Q|M"]);
}

#[test]
fn the_else_side_of_method_exists_reports_the_arm_that_lacks_the_method() {
    narrows(
        MEMBER_SOURCES,
        &[
            // e03 / `tp_else`: PHP raises `Call to undefined method M::go()`.
            (
                "function e03(N|M $x): string { if (method_exists($x, 'go')) { return ''; } return $x->go(); }",
                &[],
                &["call.undefined-method M"],
            ),
            // e02 / `u_neg`: the negated guard's then side is the else side.
            (
                "function e02(N|M $x): string { if (!method_exists($x, 'go')) { \\PHPStan\\dumpType($x); return $x->other(); } return $x->go(); }",
                &["M"],
                &[],
            ),
            // A docblock arm narrows too; the finding stays on the contract layer.
            (
                "/** @param N|M $x */ function s11($x): string { if (method_exists($x, 'go')) { return ''; } return $x->go(); }",
                &[],
                &["phpdoc.undefined-method M"],
            ),
            // Kept: `m12` and `m12b` stay silent.
            (
                "function m12(N|null $n): string { if ($n !== null && method_exists($n, 'go')) { return $n->go(); } return ''; }",
                &[],
                &[],
            ),
            (
                "function m12b(N|null $n): string { if ($n === null || !method_exists($n, 'go')) { return ''; } return $n->go(); }",
                &[],
                &[],
            ),
        ],
    );
}

#[test]
fn method_exists_narrows_at_every_condition_site() {
    narrows(
        MEMBER_SOURCES,
        &[
            // assert()
            (
                "function s02(N|M $x): string { assert(method_exists($x, 'go')); \\PHPStan\\dumpType($x); return $x->go(); }",
                &["N"],
                &[],
            ),
            // The `&&` operand: past `method_exists($x, 'go')` the right operand reads N,
            // which lacks `other`, so the condition folds false and the body is dead.
            (
                "function o1(N|M $x): string { if (method_exists($x, 'go') && method_exists($x, 'other')) { \\PHPStan\\dumpType($x); return 'dead'; } return 'n'; }",
                &[],
                &[],
            ),
            (
                "function o2(N|M $x): string { if (method_exists($x, 'go') && method_exists($x, 'go')) { \\PHPStan\\dumpType($x); return 'y'; } return 'n'; }",
                &["N"],
                &[],
            ),
            // A stored condition (ADR-0106 §2.3).
            (
                "function s03(N|M $x): string { $ok = method_exists($x, 'go'); if ($ok) { \\PHPStan\\dumpType($x); return 'y'; } \\PHPStan\\dumpType($x); return $x->go(); }",
                &["N", "M"],
                &["call.undefined-method M"],
            ),
            // `||` through the path driver: the path on which M would pass needs `zz`,
            // which M lacks too, so the then side is N alone.
            (
                "function s05(N|M $x): string { if (method_exists($x, 'go') || method_exists($x, 'zz')) { \\PHPStan\\dumpType($x); return 'y'; } \\PHPStan\\dumpType($x); return $x->go(); }",
                &["N", "M"],
                &["call.undefined-method M"],
            ),
            // e11: under `instanceof T` only Leaf is admitted, which holds `zz` through
            // Mid: the else side holds no arm.
            (
                "function e11(I $x): int { if ($x instanceof T) { if (method_exists($x, 'zz')) { \\PHPStan\\dumpType($x); return 1; } \\PHPStan\\dumpType($x); return 2; } return 0; }",
                &["T", "*NEVER*"],
                &[],
            ),
            // A guard no class passes leaves its arms dead, deleted on the then side.
            (
                "function s15(N|M $x): string { if ($x instanceof Nobody && method_exists($x, 'go')) { \\PHPStan\\dumpType($x); return 'y'; } return 'n'; }",
                &["*NEVER*"],
                &[],
            ),
        ],
    );
}

#[test]
fn what_method_exists_does_not_narrow() {
    narrows(
        MEMBER_SOURCES,
        &[
            // e05: `is_callable` is declined (a `__call` makes it true, visibility false).
            (
                "function e05(N|M $x): string { if (is_callable([$x, 'go'])) { \\PHPStan\\dumpType($x); return 'y'; } \\PHPStan\\dumpType($x); return 'n'; }",
                &["N|M", "N|M"],
                &[],
            ),
            // `property_exists` asks about another member kind.
            (
                "function s18(N|M $x): string { if (property_exists($x, 'go')) { \\PHPStan\\dumpType($x); return 'y'; } \\PHPStan\\dumpType($x); return 'n'; }",
                &["N|M", "N|M"],
                &[],
            ),
            // A method name the walk cannot read.
            (
                "function s13(N|M $x, string $m): string { if (method_exists($x, $m)) { \\PHPStan\\dumpType($x); return 'y'; } \\PHPStan\\dumpType($x); return 'n'; }",
                &["N|M", "N|M"],
                &[],
            ),
            // A rebinding inside the branch drops the narrowing with the lane.
            (
                "function s17(N|M $x, N|M $y): string { if (method_exists($x, 'go')) { $x = $y; \\PHPStan\\dumpType($x); return 'y'; } return 'n'; }",
                &["N|M"],
                &[],
            ),
        ],
    );
}
