//! Rebinding a name to another name's heap object (issue #1113): `$x = $y` where `$y`
//! holds an object takes the alias arm of `bind_value`, which must release `$x`'s old
//! binding as every other rebind does. Otherwise a stored `method_exists` guard on
//! `$x`, or a narrowed `Member` class of `$x`, outlives the value it was read from.
//!
//! Also pins issue #1091's witnesses, which do not reproduce: a write through a
//! reference, `extract()` or a by-reference closure poisons the scope, and a by-ref
//! call goes through by-ref invalidation, so the old heap carrier is never read back.
//!
//! Every fixture is a witness probed on PHP 8.5.11: each `strlen` row and each
//! undefined call raises, and each dump prints the class named beside it.

use steins_infer::{Diagnostic, Folder, check_with};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, SourceTree};

/// An engine that folds nothing, has exactly the builtins these fixtures call, and
/// answers `strlen`'s reflected parameter list (`strlen(string $string): int`).
struct Probe;

const BUILTINS: &[&str] = &["method_exists", "strlen", "rand", "extract"];

impl Folder for Probe {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        Some(BUILTINS.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        name.eq_ignore_ascii_case("strlen").then(|| {
            vec![BuiltinParam {
                name: "string".to_owned(),
                ty: Some("string".to_owned()),
                by_ref: false,
                variadic: false,
                optional: false,
            }]
        })
    }
}

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Probe)
}

/// Every `debug.type` answer of `src`, as `line: rendered`.
fn dumps(src: &str) -> Vec<String> {
    run(src)
        .into_iter()
        .filter(|d| d.id == "debug.type")
        .map(|d| {
            let answer = d.message.split_once("dumped type: ").map_or("?", |(_, a)| a);
            format!("{}: {answer}", d.line)
        })
        .collect()
}

/// The findings of `src` other than the dumps and the `untyped.*` surface, as
/// `line id`.
fn findings(src: &str) -> Vec<String> {
    run(src)
        .into_iter()
        .filter(|d| d.id != "debug.type" && !d.id.starts_with("untyped."))
        .map(|d| format!("{} {}", d.line, d.id))
        .collect()
}

/// `v1113b.php`, one function a line from line 5: s2, s2b, s2c, then the controls
/// c1 and c2, then m1, m2, m3.
const V1113: &str = "<?php
declare(strict_types=1);
class A { public function m(): int { return 1; } }
class B { public function b(): int { return 3; } }
function s2(A|B $x, B $y, ?string $n): int { $ok = method_exists($x, 'm'); $x = $y; if ($ok) { return strlen($n); } return 0; }
function s2b(A|B $x, int $y, ?string $n): int { $ok = method_exists($x, 'm'); $x = $y; if ($ok) { return strlen($n); } return 0; }
function s2c(A|B $x, B $y, ?string $n): int { $ok = method_exists($x, 'm'); $z = $y; $x = $z; if ($ok) { return strlen($n); } return 0; }
function c1(A|B $x, B $y, ?string $n): int { $ok = method_exists($x, 'm'); if ($ok) { return strlen($n); } return 0; }
function c2(A|B $x, B $y, ?string $n): int { $ok = (bool) rand(0,1); $x = $y; if ($ok) { return strlen($n); } return 0; }
function m1(A|B $x, B $y): int { if ($x instanceof A) { $x = $y; if (!($x instanceof A)) { nope1(); } } return 0; }
function m2(A|B $x, int $y): int { if ($x instanceof A) { $x = $y; if (!($x instanceof A)) { nope2(); } } return 0; }
function m3(A|B $x, B $y): int { if ($x instanceof A) { $x = $y; \\PHPStan\\dumpType($x); } return 0; }
";

#[test]
fn rebinding_the_receiver_to_an_object_drops_the_stored_method_exists_guard() {
    // s2 and s2c rebind `$x` to `$y`'s object (directly, and through a copy `$z`);
    // the stored guard read `$x`'s old value, so `$ok` no longer says anything about
    // the new one and the body stays live, as it does for s2b (a rebind from `int`)
    // and the controls c1 and c2.
    assert_eq!(
        findings(V1113)
            .into_iter()
            .filter(|f| f.ends_with("type.maybe-argument-mismatch"))
            .collect::<Vec<_>>(),
        vec![
            "5 type.maybe-argument-mismatch",
            "6 type.maybe-argument-mismatch",
            "7 type.maybe-argument-mismatch",
            "8 type.maybe-argument-mismatch",
            "9 type.maybe-argument-mismatch",
        ],
    );
}

#[test]
fn rebinding_a_narrowed_name_to_an_object_drops_its_member_class() {
    // m1: `$x` is now `$y`'s `B`, so `!($x instanceof A)` holds and `nope1()` runs.
    let undefined: Vec<String> = findings(V1113)
        .into_iter()
        .filter(|f| f.ends_with("call.undefined-function"))
        .collect();
    assert_eq!(undefined, vec!["10 call.undefined-function", "11 call.undefined-function"]);
}

#[test]
fn a_dump_after_rebinding_to_an_object_shows_the_new_class() {
    assert_eq!(dumps(V1113), vec!["12: B"]);
}

#[test]
fn a_self_alias_keeps_the_object() {
    let src = "<?php
class O {}
function t(): void { $a = new O(); $a = $a; \\PHPStan\\dumpType($a); }
";
    // The source's id is read before the release, so `$a` still names its object.
    assert_eq!(dumps(src), vec!["3: O"]);
}

/// The reviewer's `selffp.php` rows: an alias of the same object keeps its narrowed
/// class. f1, f2, f3 and f5 from line 6; PHP reaches none of the `nope*()` calls.
const SAME_OBJECT: &str = "<?php
declare(strict_types=1);
interface I {}
class Foo implements I { public function fooMethod(): int { return 1; } }
class Baz implements I {}
function f1(I $a): int { if ($a instanceof Foo) { $a = $a; if (!($a instanceof Foo)) { nope1(); } } return 0; }
function f2(I $a): int { if ($a instanceof Foo) { $b = $a; $a = $b; if (!($a instanceof Foo)) { nope2(); } } return 0; }
function f3(I $a): int { if ($a instanceof Foo) { $b = $a; if (!($b instanceof Foo)) { nope3(); } } return 0; }
function f5(I $a): int { if (!($a instanceof Foo)) { return 0; } $a = $a; if ($a instanceof Foo) { return 1; } nope5(); return 0; }
function d1(I $a): int { if ($a instanceof Foo) { $a = $a; \\PHPStan\\dumpType($a); } return 0; }
";

#[test]
fn an_alias_of_the_same_object_keeps_its_narrowed_class() {
    // f3 (`$b = $a` under `instanceof Foo`) was already reported on the base: the
    // copy never carried the narrowed class.
    assert_eq!(findings(SAME_OBJECT), Vec::<String>::new());
    assert_eq!(dumps(SAME_OBJECT), vec!["10: Foo"]);
}

#[test]
fn rebinding_through_a_copy_of_another_object_drops_the_narrowed_class() {
    // `rebinds.php` r9: `$x` now holds `$y`'s `B`, so `nope9()` runs (PHP: Error).
    let src = "<?php
class A { public function m(): int { return 1; } }
class B { public function b(): int { return 3; } }
function r9(A|B $x, B $y): int { if ($x instanceof A) { $z = $y; $x = $z; if (!($x instanceof A)) { nope9(); } } return 0; }
";
    assert_eq!(findings(src), vec!["4 call.undefined-function"]);
}

/// `v1091a.php` (f0-f7) and `v1091c.php` (h1-h3) in one file, from line 4.
const V1091: &str = "<?php
class Animal {} class Dog extends Animal {} class Cat extends Animal {}
function byref(&$x): void { $x = new Cat(); }
function f0(): void { $d = new Dog(); \\PHPStan\\dumpType($d); }
function f1(): void { $d = new Dog(); $r = &$d; $r = new Cat(); \\PHPStan\\dumpType($d); }
function f2(): void { $d = new Dog(); extract(['d' => new Cat()]); \\PHPStan\\dumpType($d); }
function f3(): void { $d = new Dog(); $w = function () use (&$d) { $d = new Cat(); }; $w(); \\PHPStan\\dumpType($d); }
function f4(): void { $d = new Dog(); $r = &$d; $r = new Cat(); $d->bark(); }
function f5(): void { $d = new Dog(); extract(['d' => new Cat()]); $d->bark(); }
function f6(): void { $d = new Dog(); $w = function () use (&$d) { $d = new Cat(); }; $w(); $d->bark(); }
function f7(): void { $d = new Dog(); $d->bark(); }
function h1(): void { $d = new Dog(); byref($d); \\PHPStan\\dumpType($d); }
function h2(): void { $d = new Dog(); byref($d); $d->bark(); }
function h3(): void { $d = new Dog(); $d->bark(); }
";

#[test]
fn a_write_through_a_reference_extract_a_closure_or_a_by_ref_call_forgets_the_object() {
    // Issue #1091: PHP prints `Cat` for f1-f3 and h1. Steins never reads the old
    // `Dog` carrier back; the unwritten twin f0 still dumps it.
    assert_eq!(dumps(V1091), vec!["4: Dog", "5: unknown", "6: unknown", "7: unknown", "12: unknown"]);
}

#[test]
fn bark_after_the_write_is_silent_and_on_the_unwritten_twin_reports() {
    // f4-f6 and h2 call `bark()` on what PHP holds as a `Cat`: silent here, since
    // the class is no longer known. f7 and h3 never write `$d`.
    assert_eq!(findings(V1091), vec!["11 call.undefined-method", "14 call.undefined-method"]);
}

#[test]
fn the_issue_1091_witness_dumps_unknown() {
    let src = "<?php
class Animal {} class Dog extends Animal {} class Cat extends Animal {}
function f(): void {
    $d = new Dog();
    $r = &$d; $r = new Cat();
    \\PHPStan\\dumpType($d);
}
function g(): void {
    $d = new Dog();
    extract(['d' => new Cat()]);
    \\PHPStan\\dumpType($d);
}
function h(): void {
    $d = new Dog();
    $w = function () use (&$d) { $d = new Cat(); }; $w();
    \\PHPStan\\dumpType($d);
}
";
    assert_eq!(dumps(src), vec!["6: unknown", "11: unknown", "16: unknown"]);
}
