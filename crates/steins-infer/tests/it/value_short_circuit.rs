//! Statement-level short-circuit values (#1098, bullet 2): a value-position `&&`/`||`
//! is a condition, so the left operand narrows the right one, an existence guard in it
//! folds, and a dead operand is marked before the statement's positions are judged.
//!
//! Each case names a bare undefined call (`nopeN()`) in the operand a decided guard
//! never reaches; the kept shapes (`s5`..`s7`, the undecided twins) must still report.
//! [`Boot`] mocks the boot surface as in `undefined_function.rs`.

use steins_infer::{CALL_UNDEFINED_FUNCTION_ID, Folder, check_with};
use steins_syntax::SourceTree;

struct Boot;

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
        true
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        let guards = ["function_exists", "is_callable", "method_exists"];
        Some(guards.iter().any(|g| g.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

/// The names of the undefined functions reported for `body` after the `N` class.
fn reported(body: &str) -> Vec<String> {
    let src = format!(
        "<?php\ndeclare(strict_types=1);\nclass N {{ public function go(): int {{ return 1; }} }}\n{body}\n"
    );
    let tree = SourceTree::parse(&src);
    let mut out: Vec<String> = check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| d.id == CALL_UNDEFINED_FUNCTION_ID)
        .map(|d| {
            let after = d.message.split(" function ").nth(1);
            after.and_then(|r| r.split_whitespace().next()).unwrap_or("").to_owned()
        })
        .collect();
    out.sort();
    out
}

fn silent(body: &str) {
    assert_eq!(reported(body), Vec::<String>::new(), "{body}");
}

fn reports(body: &str, name: &str) {
    assert_eq!(reported(body), vec![format!("{name}()")], "{body}");
}

#[test]
fn s2_if_twin_is_silent() {
    silent("function s2(?N $n): bool { if ($n !== null && method_exists($n, 'gone') && nope2()) { return true; } return false; }");
}

#[test]
fn s1_assigned_and_chain_folds_the_existence_guard() {
    silent("function s1(?N $n): bool { $r = $n !== null && method_exists($n, 'gone') && nope1(); return $r; }");
}

#[test]
fn s3_ternary_with_logical_guard_never_takes_the_then_arm() {
    silent("function s3(?N $n): int { return $n !== null && method_exists($n, 'gone') ? nope3() : 0; }");
}

#[test]
fn s4_returned_and_chain_is_silent() {
    silent("function s4(?N $n): bool { return $n !== null && method_exists($n, 'gone') && nope4(); }");
}

#[test]
fn s8_non_null_receiver_folds_the_existence_guard() {
    silent("function s8(N $n): bool { $r = $n !== null && method_exists($n, 'gone') && nope8(); return $r; }");
}

#[test]
fn s5_declared_method_keeps_the_right_operand_live() {
    reports("function s5(?N $n): bool { $r = $n !== null && method_exists($n, 'go') && nope5(); return $r; }", "nope5");
}

#[test]
fn s6_or_over_nullable_keeps_the_right_operand_live() {
    reports("function s6(?N $n): bool { $r = $n === null || nope6(); return $r; }", "nope6");
}

#[test]
fn s7_or_over_non_null_keeps_the_right_operand_live() {
    reports("function s7(N $n): bool { $r = $n === null || nope7(); return $r; }", "nope7");
}

// Near-miss true positives.

#[test]
fn nullable_and_undefined_call_still_reports() {
    reports("function n1(?N $n): bool { $r = $n !== null && nope(); return $r; }", "nope");
}

#[test]
fn unknown_bool_or_undefined_call_still_reports() {
    reports("function n2(bool $x): bool { $r = $x || nope(); return $r; }", "nope");
}

#[test]
fn ternary_arm_dead_under_a_false_existence_guard_is_silent() {
    silent("function n3(N $n): int { $r = method_exists($n, 'gone') ? nope() : 1; return $r; }");
}

#[test]
fn ternary_arm_live_under_a_true_existence_guard_reports() {
    reports("function n4(N $n): int { $r = method_exists($n, 'go') ? nope() : 1; return $r; }", "nope");
}

#[test]
fn call_argument_position_is_silent_under_a_dead_guard() {
    silent("function n6(N $n): void { var_dump($n !== null && method_exists($n, 'gone') && nope()); }");
}

#[test]
fn if_twin_over_non_null_receiver_is_silent() {
    silent("function p1(N $n): bool { if ($n !== null && method_exists($n, 'gone') && nope2()) { return true; } return false; }");
}

#[test]
fn if_twin_without_null_test_is_silent() {
    silent("function p2(N $n): bool { if (method_exists($n, 'gone') && nope2()) { return true; } return false; }");
}

#[test]
fn xor_keeps_the_truthiness_path_and_reports() {
    reports("function n7(bool $x): bool { $r = $x xor nope(); return $r; }", "nope");
}
