//! The `&&`/`||` operand and ternary-arm environment runs the branch lane's whole
//! vocabulary list (ADR-0106, slice S8, #1096): the `@phpstan-assert-if-*`
//! envelopes and the out-parameter seeds narrow a right operand and a ternary arm
//! as they narrow the body of the `if` spelling of the same condition.
//!
//! Each row's PHP outcome was measured on PHP 8.3.6 (`A1`/`B1` arguments, and
//! `'xy'`/`'y'`/`'zz'` for the `preg_match` rows). The class rows expect the id the
//! `if` spelling reports on this base, `call.undefined-method`.

use steins_infer::{CALL_UNDEFINED_METHOD_ID, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A boot surface that makes the absence family available with an empty homonym
/// surface, so `call.undefined-method` can fire on a proven receiver.
struct Boot;
impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

const PRELUDE: &str = "<?php
declare(strict_types=1);
interface Foo { public function fooOnly(): int; }
final class A1 implements Foo { public function fooOnly(): int { return 1; } }
final class B1 { public function other(): int { return 2; } }
/** @phpstan-assert-if-true Foo $o */
function isFoo(mixed $o): bool { return $o instanceof Foo; }
/** @phpstan-assert-if-false Foo $o */
function notFoo(mixed $o): bool { return !($o instanceof Foo); }
";

/// The ids a fixture body reports through the class lane.
fn class_ids(body: &str) -> Vec<String> {
    let src = format!("{PRELUDE}{body}\n");
    let tree = SourceTree::parse(&src);
    check_with(&tree, &[], "demo.php", &mut Boot)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .map(|d| d.id.to_string())
        .collect()
}

#[test]
fn an_assert_if_envelope_narrows_the_right_operand_and_the_ternary_arm() {
    for (name, body) in [
        ("g7 && operand", "function g7(A1|B1 $o): int { return isFoo($o) && $o->other() ? 1 : 0; }"),
        ("g9 then arm", "function g9(A1|B1 $o): int { return isFoo($o) ? $o->other() : 0; }"),
        (
            "g12 || operand (-if-false)",
            "function g12(A1|B1 $o): int { return notFoo($o) || $o->other() ? 1 : 0; }",
        ),
        // The `if` spellings the operand lane now matches.
        (
            "g8 if",
            "function g8(A1|B1 $o): int { if (isFoo($o)) { return $o->other(); } return 0; }",
        ),
        (
            "g13 if",
            "function g13(A1|B1 $o): int { if (!notFoo($o)) { return $o->other(); } return 0; }",
        ),
    ] {
        assert_eq!(class_ids(body), vec![CALL_UNDEFINED_METHOD_ID.to_string()], "{name}");
    }
}

#[test]
fn an_envelope_that_proves_the_member_stays_silent() {
    for (name, body) in [
        (
            "g11 && operand",
            "function g11(A1|B1 $o): int { return isFoo($o) && $o->fooOnly() ? 1 : 0; }",
        ),
        ("then arm", "function g11b(A1|B1 $o): int { return isFoo($o) ? $o->fooOnly() : 0; }"),
    ] {
        assert_eq!(class_ids(body), Vec::<String>::new(), "{name}");
    }
}

#[test]
fn g10_the_negated_envelope_operand_is_recorded() {
    // `!isFoo($o) || $o->other()`: the right operand runs where `isFoo` was true, so
    // `$o` is `A1` there. PHP: `Error` for A1, `1` for B1.
    let body = "function g10(A1|B1 $o): int { return !isFoo($o) || $o->other() ? 1 : 0; }";
    assert_eq!(class_ids(body), vec![CALL_UNDEFINED_METHOD_ID.to_string()], "g10");
}

fn offset_maybe_missing(body: &str) -> usize {
    let src = format!("<?php\ndeclare(strict_types=1);\n{body}\n");
    let tree = SourceTree::parse(&src);
    check(&tree, &[], "demo.php").iter().filter(|d| d.id == "offset.maybe-missing").count()
}

#[test]
fn an_out_parameter_seed_reaches_the_right_operand_and_the_ternary_arm() {
    for (name, body) in [
        (
            "p8 if",
            "function p8(string $s): bool { if (preg_match('/(x)?y/', $s, $m) === 1) \
             { return strlen($m[1]) > 0; } return false; }",
        ),
        (
            "p7 && operand",
            "function p7(string $s): bool { return preg_match('/(x)?y/', $s, $m) === 1 \
             && strlen($m[1]) > 0; }",
        ),
        (
            "p10 then arm",
            "function p10(string $s): int { return preg_match('/(x)?y/', $s, $m) === 1 \
             ? strlen($m[1]) : 0; }",
        ),
        (
            "p11 else arm of the negation",
            "function p11(string $s): int { return preg_match('/(x)?y/', $s, $m) !== 1 \
             ? 0 : strlen($m[1]); }",
        ),
    ] {
        assert_eq!(offset_maybe_missing(body), 1, "{name}");
    }
}

#[test]
fn a_required_group_read_in_an_operand_stays_silent() {
    for (name, body) in [
        (
            "p9 && operand",
            "function p9(string $s): bool { return preg_match('/(x)y/', $s, $m) === 1 \
             && strlen($m[1]) > 0; }",
        ),
        (
            "required group then arm",
            "function p9b(string $s): int { return preg_match('/(x)y/', $s, $m) === 1 \
             ? strlen($m[1]) : 0; }",
        ),
    ] {
        assert_eq!(offset_maybe_missing(body), 0, "{name}");
    }
}

#[test]
fn a_seeded_group_makes_the_operand_verdict_only() {
    // PHP: `$m[1]` is always 'x' after a match of `/(x)/`, so `$m[1] !== 'x'` never
    // holds and `nope()` never runs. The seed is `Asserted`, so the verdict never
    // proves the body dead and no finding surfaces.
    let src = "<?php\ndeclare(strict_types=1);\nfunction nope(): void {}\n\
               function v(string $s): void { if (preg_match('/(x)/', $s, $m) === 1 \
               && $m[1] !== 'x') { nope(); } }\n";
    let tree = SourceTree::parse(src);
    let ids: Vec<String> = check(&tree, &[], "demo.php").into_iter().map(|d| d.id.to_string()).collect();
    assert_eq!(ids, Vec::<String>::new(), "{ids:?}");
}
