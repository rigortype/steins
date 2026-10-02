//! Shapes from other analyzers' suites that Steins already answers correctly.
//!
//! mago's analyzer cases and mir's fixtures were run through `steins check`
//! (the survey behind #925–#952). Each test here re-states, in its own
//! minimal source, a shape those suites pin and Steins gets right today, so a
//! regression shows up here and not as a false positive on someone's code.
//! Nothing is copied from either suite; the comment on each test names the
//! fixture it was adapted from.
//!
//! Three kinds of pin:
//!
//! * **silences** — a guard that makes the guarded reference unreachable
//!   discharges the absence proof. Only the forms that discharge *today* are
//!   pinned; the sibling forms that do not are the open issues named beside
//!   them, and their fix adds them here.
//! * **findings** — a runtime failure both suites and Steins report, pinned
//!   with the line it lands on.
//! * **boundaries** — the neighbouring form that must stay quiet next to a
//!   finding, the half a suite usually checks and a test of the finding alone
//!   would not.
//!
//! Every source below runs under `php` 8.5 exactly as its comment says (each
//! silence ends in no warning, each finding ends in the warning or `Error`
//! the finding quotes).

use steins_infer::{
    CALL_INACCESSIBLE_METHOD_ID, CALL_ON_NULL_ID, CALL_UNDEFINED_FUNCTION_ID,
    CALL_UNDEFINED_METHOD_ID, CLASS_EXTENDS_FINAL_ID, CLASS_UNDEFINED_ID, CONSTANT_UNDEFINED_ID,
    Diagnostic, Folder, OFFSET_MISSING_ID, PROPERTY_INACCESSIBLE_ID, VARIABLE_MAYBE_UNDEFINED_ID,
    VARIABLE_UNDEFINED_ID, check_with,
};
use steins_syntax::SourceTree;

/// The boot surface of a runtime with no optional extension: the absence
/// family is available and answers "absent" for every class, function and
/// constant except the handful named in `present`.
struct Boot {
    present: &'static [&'static str],
}

impl Boot {
    fn bare() -> Self {
        Boot { present: &["RuntimeException", "stdClass"] }
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
        true
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        Some(self.present.iter().any(|p| p.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        Some(self.present.iter().any(|p| p.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_constant(&mut self, name: &str) -> Option<bool> {
        Some(self.present.contains(&name))
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.11 (0 extensions)".to_owned())
    }
}

/// Every finding except the requested debug dumps, as `(id, line)`.
fn findings(src: &str) -> Vec<(&'static str, u32)> {
    let tree = SourceTree::parse(src);
    let all: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Boot::bare());
    all.into_iter().filter(|d| !d.id.starts_with("debug.")).map(|d| (d.id, d.line)).collect()
}

/// `src` reports nothing that `ids` names.
#[track_caller]
fn silent_on(src: &str, ids: &[&str]) {
    let hit: Vec<_> = findings(src).into_iter().filter(|(id, _)| ids.contains(id)).collect();
    assert!(hit.is_empty(), "expected no {ids:?}, got {hit:?}\n{src}");
}

/// `src` reports `id` on exactly `lines`, in order.
#[track_caller]
fn fires_on(src: &str, id: &str, lines: &[u32]) {
    let got: Vec<u32> =
        findings(src).into_iter().filter(|(i, _)| *i == id).map(|(_, l)| l).collect();
    assert_eq!(got, lines, "{id} lines\n{src}");
}

// The control every silence below leans on: with no guard, the same
// references fire under this boot surface. Without it a silence could pass
// only because the mock never let the absence family run.

#[test]
fn unguarded_references_fire_under_the_mock_boot_surface() {
    let src = "<?php
class N { public function real(): int { return 1; } }
function f(N $n): void {
    echo OPT_C;
    opt_fn();
    new OptC();
    $n->go();
    print($x);
}
";
    fires_on(src, CONSTANT_UNDEFINED_ID, &[4]);
    fires_on(src, CALL_UNDEFINED_FUNCTION_ID, &[5]);
    fires_on(src, CLASS_UNDEFINED_ID, &[6]);
    fires_on(src, CALL_UNDEFINED_METHOD_ID, &[7]);
    fires_on(src, VARIABLE_UNDEFINED_ID, &[8]);
}

// Silences: existence guards (mago symbol_existence_edge_cases; the ternary,
// `&&`, `switch (true)` and `extension_loaded()` forms are #928).

#[test]
fn defined_in_an_if_block_discharges_the_constant() {
    silent_on(
        "<?php\nfunction f(): mixed { if (defined('OPT_C')) { return OPT_C; } return null; }\n",
        &[CONSTANT_UNDEFINED_ID],
    );
}

#[test]
fn a_negated_defined_early_return_discharges_the_rest_of_the_body() {
    silent_on(
        "<?php\nfunction f(): mixed { if (!defined('OPT_C')) { return null; } return OPT_C; }\n",
        &[CONSTANT_UNDEFINED_ID],
    );
}

#[test]
fn defined_as_a_match_true_arm_discharges_that_arm() {
    silent_on(
        "<?php
function f(): mixed {
    return match (true) { defined('OPT_C') => OPT_C, default => null };
}
",
        &[CONSTANT_UNDEFINED_ID],
    );
}

#[test]
fn function_exists_discharges_in_every_position() {
    // The guard family #928 holds up as the model for the others: the `if`
    // block, the negated early return, the `?:` arm and the `&&` operand.
    silent_on(
        "<?php
function a(): void { if (function_exists('opt_fn')) { opt_fn(); } }
function b(): void { if (!function_exists('opt_fn')) { return; } opt_fn(); }
function c(): mixed { return function_exists('opt_fn') ? opt_fn() : null; }
function d(): void { function_exists('opt_fn') && opt_fn(); }
",
        &[CALL_UNDEFINED_FUNCTION_ID],
    );
}

#[test]
fn class_exists_in_an_if_block_or_before_an_early_return_discharges_the_class() {
    silent_on(
        "<?php
function a(): void { if (class_exists('OptC')) { new OptC(); } }
function b(): void { if (!class_exists('OptC')) { return; } new OptC(); }
function c(): void { if (class_exists(\\OptC::class)) { new \\OptC(); } }
",
        &[CLASS_UNDEFINED_ID],
    );
}

// Silences: isset() on a variable (mir undefined_variable; the `&&`/`||`
// operand and negated early-return forms are #929).

#[test]
fn isset_in_an_if_block_discharges_a_never_bound_variable() {
    silent_on(
        "<?php\nfunction f(): void { if (isset($x)) { print($x); } }\n",
        &[VARIABLE_UNDEFINED_ID],
    );
}

#[test]
fn isset_as_a_ternary_condition_discharges_a_never_bound_variable() {
    silent_on(
        "<?php\nfunction f(): mixed { return isset($x) ? $x : null; }\n",
        &[VARIABLE_UNDEFINED_ID],
    );
}

#[test]
fn isset_and_its_conjunct_discharge_a_maybe_bound_variable() {
    silent_on(
        "<?php\nfunction f(bool $c): void { if ($c) { $y = 1; } if (isset($y) && $y > 0) {} }\n",
        &[VARIABLE_UNDEFINED_ID, VARIABLE_MAYBE_UNDEFINED_ID],
    );
}

// Silences: method_exists() as a ternary (mir undefined_method; the `if`,
// early-return and is_callable() forms are #930).

#[test]
fn method_exists_as_a_ternary_condition_discharges_the_call() {
    silent_on(
        "<?php
class N { public function real(): int { return 1; } }
function f(N $n): mixed { return method_exists($n, 'go') ? $n->go() : null; }
",
        &[CALL_UNDEFINED_METHOD_ID],
    );
}

// Findings and their boundaries.

#[test]
fn extending_a_final_class_fires_once_per_direct_child_and_never_on_a_grandchild() {
    // mir final_class_extended: only the direct child is a load-time fatal;
    // `Leaf` extends the non-final `Middle` and must stay quiet.
    let src = "<?php
final class Base {}
class Middle extends Base {}
class Other extends Base {}
class Leaf extends Middle {}
";
    fires_on(src, CLASS_EXTENDS_FINAL_ID, &[3, 4]);
}

#[test]
fn a_docblock_shape_does_not_hide_an_empty_array_literal() {
    // mago issue_2289: the `@var` claims a key the value provably lacks; PHP
    // warns `Undefined array key "k0"` on the read.
    let src = "<?php
function f(): void {
    /** @var array{k0: ?int, k1: ?int} $row */
    $row = [];
    $v0 = $row['k0'];
}
";
    fires_on(src, OFFSET_MISSING_ID, &[5]);
}

#[test]
fn a_docblock_class_does_not_hide_a_null_returning_call() {
    // mir type_check_mismatch var_annotation_type_hint: the container's body
    // returns null, so the call through the `@var`-typed variable is
    // `Error: Call to a member function find() on null`.
    let src = "<?php
class Repo { public function find(int $id): ?object { return null; } }
class Container { public function get(string $c): mixed { return null; } }
$container = new Container();
/** @var Repo $repo */
$repo = $container->get(Repo::class);
$repo->find(1);
";
    fires_on(src, CALL_ON_NULL_ID, &[7]);
}

#[test]
fn a_read_of_a_variable_never_bound_in_its_function_fires() {
    // The unguarded counterpart of the isset() silences above.
    fires_on("<?php\nfunction f(): void {\n    print($x);\n}\n", VARIABLE_UNDEFINED_ID, &[3]);
}

// Visibility boundaries (mir inaccessible_method / inaccessible_property; the
// redeclared-member false positive beside these is #942).

#[test]
fn a_protected_member_of_the_common_ancestor_is_visible_from_a_sibling() {
    // PHP checks a protected member against the class that first declared it,
    // and `B` is a `Base`. Runs clean: prints `11`.
    silent_on(
        "<?php
class Base { protected function h(): int { return 1; } protected int $p = 1; }
class A extends Base {}
class B extends Base {
    public function t(): int { $a = new A(); $x = $a->h(); return $x; }
    public function u(): int { $a = new A(); $y = $a->p; return $y; }
}
",
        &[CALL_INACCESSIBLE_METHOD_ID, PROPERTY_INACCESSIBLE_ID],
    );
}

#[test]
fn a_private_member_is_visible_on_another_instance_of_the_same_class() {
    silent_on(
        "<?php
final class Money {
    private int $cents = 0;
    private function raw(): int { return $this->cents; }
    public function same(Money $o): bool { $a = $o->raw(); $b = $o->cents; return $a === $b; }
}
",
        &[CALL_INACCESSIBLE_METHOD_ID, PROPERTY_INACCESSIBLE_ID],
    );
}

#[test]
fn a_protected_member_first_declared_on_a_sibling_is_invisible() {
    // The firing half of the boundary: `A::h()` has no declaration on `Base`,
    // so `B` is outside its hierarchy — `Error: Call to protected method
    // A::h() from scope B`.
    let src = "<?php
class Base {}
class A extends Base { protected function h(): int { return 3; } }
class B extends Base {
    public function t(): int { $a = new A(); $x = $a->h(); return $x; }
}
";
    fires_on(src, CALL_INACCESSIBLE_METHOD_ID, &[5]);
}
