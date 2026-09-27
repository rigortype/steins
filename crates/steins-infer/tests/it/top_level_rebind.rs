//! The top-level rebind rule (issue #762): in the frame whose locals are the
//! globals, a statement that runs a userland body forgets every name the frame
//! holds, because that body can rebind any of them through `global $s` or
//! `$GLOBALS['s']` without the statement naming it.
//!
//! The witness, probed at 8.5.10 (PHP exits 0 — `$s` is `5` at the `intdiv`):
//!
//! ```php
//! function bump(): void { global $s; $s = 5; }
//! $s = 'abc';
//! bump();
//! intdiv($s, 1);
//! ```
//!
//! Every fixture below that expects silence has a twin that expects the finding,
//! so none of them passes because the finding was never reachable: the value
//! lane convicts `intdiv('abc', 1)` wherever nothing forgot `$s`.
//!
//! The routes that reach a global without a call are pinned at the bottom, each
//! as what it does today and why.

use std::collections::HashMap;

use steins_infer::{Diagnostic, Folder, ID, check_with};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, SourceTree};

fn p(name: &str, ty: &str) -> BuiltinParam {
    BuiltinParam {
        name: name.to_owned(),
        ty: Some(ty.to_owned()),
        by_ref: false,
        variadic: false,
        optional: false,
    }
}

/// An engine that reflects the two builtins the fixtures judge through. Every
/// other builtin a fixture calls is known by the mined arginfo table, which is
/// what the rule reads first — so these fixtures also stand for `--no-php`.
///
/// `absence` opens the ADR-0049 absence family, which `call.undefined-method`
/// needs; it stays off elsewhere so no fixture reasons about that family.
struct Mock {
    params: HashMap<String, Vec<BuiltinParam>>,
    absence: bool,
}

impl Mock {
    fn engine() -> Mock {
        let mut params = HashMap::new();
        params.insert("intdiv".to_owned(), vec![p("num1", "int"), p("num2", "int")]);
        params.insert("strlen".to_owned(), vec![p("string", "string")]);
        Mock { params, absence: false }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.absence
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        self.absence.then(|| fqn.eq_ignore_ascii_case("DateTimeImmutable"))
    }
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        self.params.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn findings_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped.") && d.id != "statement.no-effect")
        .collect()
}

/// The `type.argument-mismatch` lines a source reports.
fn mismatch_lines(src: &str) -> Vec<u32> {
    findings_with(src, &mut Mock::engine())
        .into_iter()
        .filter(|d| d.id == ID)
        .map(|d| d.line)
        .collect()
}

/// A callee that rebinds the global `$s`, spelled both ways PHP offers.
const BUMP: &str = "function bump(): void { global $s; $s = 5; }\n";
const BUMP_GLOBALS: &str = "function bump(): void { $GLOBALS['s'] = 5; }\n";

/// `prelude`, then `$s = 'abc';`, then `stmt`, then the judged `intdiv($s, 1);`.
fn script(prelude: &str, stmt: &str) -> String {
    format!("<?php\n{prelude}$s = 'abc';\n{stmt}\nintdiv($s, 1);\n")
}

/// The line of the judged `intdiv` in [`script`]'s output.
fn intdiv_line(prelude: &str, stmt: &str) -> u32 {
    let at = script(prelude, stmt).lines().position(|l| l == "intdiv($s, 1);").unwrap();
    u32::try_from(at).unwrap() + 1
}

/// Silence after `stmt` — and, so the silence is `stmt`'s doing, the finding
/// without it.
fn silent_after(prelude: &str, stmt: &str) {
    convicts_after(prelude, "");
    let src = script(prelude, stmt);
    assert_eq!(mismatch_lines(&src), Vec::<u32>::new(), "expected silence for:\n{src}");
}

fn convicts_after(prelude: &str, stmt: &str) {
    let src = script(prelude, stmt);
    let want = vec![intdiv_line(prelude, stmt)];
    assert_eq!(mismatch_lines(&src), want, "expected the finding for:\n{src}");
}

// ---------------------------------------------------------------------------
// The witness and its twin
// ---------------------------------------------------------------------------

#[test]
fn the_finding_is_reachable_without_the_call() {
    convicts_after(BUMP, "");
}

#[test]
fn a_function_that_rebinds_the_global_through_global_forgets_it() {
    silent_after(BUMP, "bump();");
}

#[test]
fn a_function_that_rebinds_the_global_through_dollar_globals_forgets_it() {
    silent_after(BUMP_GLOBALS, "bump();");
}

// ---------------------------------------------------------------------------
// Every callee shape the walk cannot resolve to an engine builtin
// ---------------------------------------------------------------------------

#[test]
fn a_static_method_forgets_the_frame() {
    let prelude = "final class Util {\n\
                   public static function bump(): void { global $s; $s = 5; }\n}\n";
    silent_after(prelude, "Util::bump();");
}

#[test]
fn an_instance_method_forgets_the_frame() {
    let prelude = "final class Box { public function bump(): void { global $s; $s = 5; } }\n\
                   $o = new Box();\n";
    silent_after(prelude, "$o->bump();");
}

#[test]
fn a_constructor_that_globals_forgets_the_frame() {
    let prelude = "final class C { public function __construct() { global $s; $s = 5; } }\n";
    silent_after(prelude, "$c = new C();");
}

#[test]
fn a_call_nested_in_a_builtin_argument_forgets_the_frame() {
    let prelude = "function bump2(): string { global $s; $s = 5; return 'x'; }\n";
    silent_after(prelude, "$n = strlen(bump2());");
}

#[test]
fn a_call_hidden_in_a_try_block_forgets_the_frame() {
    // The trace does not walk a `try`: it is an opaque construct whose write set
    // is the names it assigns, and `bump()` assigns none.
    silent_after(BUMP, "try { bump(); } catch (\\Throwable $e) {}");
}

#[test]
fn a_call_in_an_if_condition_forgets_the_frame() {
    let prelude = "function bump2(): bool { global $s; $s = 5; return true; }\n";
    silent_after(prelude, "if (bump2()) { echo 1; }");
}

#[test]
fn a_call_in_a_loop_body_forgets_the_frame_after_the_loop() {
    // A loop already forgets every name it mentions at its entry; `$s` is not
    // one of them, so only the call can reach it.
    silent_after(BUMP, "foreach ([1, 2] as $i) { bump(); }");
    convicts_after("", "foreach ([1, 2] as $i) { $n = strlen('x'); }");
}

// ---------------------------------------------------------------------------
// A callback handed to a builtin
// ---------------------------------------------------------------------------

#[test]
fn a_named_callback_under_array_map_forgets_the_frame() {
    let prelude = "function bump(int $x): int { global $s; $s = 5; return $x; }\n";
    silent_after(prelude, "array_map('bump', [1]);");
}

#[test]
fn a_named_callback_under_call_user_func_forgets_the_frame() {
    silent_after(BUMP, "call_user_func('bump');");
}

#[test]
fn a_user_comparator_under_usort_forgets_the_frame() {
    let prelude = "function cmp(int $a, int $b): int { global $s; $s = 5; return $a <=> $b; }\n\
                   $xs = [2, 1];\n";
    silent_after(prelude, "usort($xs, 'cmp');");
    // A closure is a body the rule does not read, so it counts whatever it does.
    let closure = "usort($xs, function (int $a, int $b): int { return $a <=> $b; });";
    silent_after("$xs = [2, 1];\n", closure);
}

#[test]
fn a_callback_position_holding_no_userland_keeps_the_frame() {
    // `null`, a builtin's name and an absent optional callback name no body.
    convicts_after("", "$r = array_map(null, [1], [2]);");
    convicts_after("", "$r = array_map('intval', ['1']);");
    convicts_after("$xs = [2, 1];\n", "usort($xs, 'strcmp');");
    convicts_after("", "$r = array_filter([1, 0]);");
}

// ---------------------------------------------------------------------------
// The controls: what keeps its facts
// ---------------------------------------------------------------------------

#[test]
fn inside_a_function_body_a_call_keeps_the_locals() {
    // `$s` is a local here: no callee can reach it without the scope's own
    // `global $s`, which poisons the scope by itself.
    let src = format!(
        "<?php\n{BUMP}function f(): void {{\n    $s = 'abc';\n    bump();\n    intdiv($s, 1);\n}}\n"
    );
    assert_eq!(mismatch_lines(&src), vec![6], "{src}");
}

#[test]
fn a_pure_builtin_at_top_level_keeps_the_frame() {
    convicts_after("$t = 'x';\n", "$n = strlen($t);");
}

#[test]
fn a_builtin_known_only_to_the_mined_table_keeps_the_frame() {
    // No engine at all: `strtoupper` is still an engine builtin by the table, so
    // it forgets nothing, and a project parameter still convicts `$s`.
    let src =
        "<?php\nfunction need(int $n): void {}\n$s = 'abc';\n$u = strtoupper('x');\nneed($s);\n";
    let got: Vec<u32> = findings_with(src, &mut Mock { params: HashMap::new(), absence: false })
        .into_iter()
        .filter(|d| d.id == ID)
        .map(|d| d.line)
        .collect();
    assert_eq!(got, vec![5], "{src}");
}

#[test]
fn a_fact_established_after_the_call_is_fresh() {
    silent_after(BUMP, "bump();");
    let src = format!("<?php\n{BUMP}$s = 5;\nbump();\n$s = 'x';\nintdiv($s, 1);\n");
    assert_eq!(mismatch_lines(&src), vec![6], "{src}");
}

#[test]
fn the_statement_that_calls_keeps_its_own_binding() {
    // `$s = mk();` forgets the frame and then binds `$s` from `mk()`'s summary.
    let src = "<?php\nfunction mk(): string { return 'abc'; }\n$s = mk();\nintdiv($s, 1);\n";
    assert_eq!(mismatch_lines(src), vec![4], "{src}");
}

#[test]
fn the_arguments_of_the_call_are_judged_as_they_were_passed() {
    // The call's own checks run before the frame is forgotten.
    let src = "<?php\nfunction need(int $n): void { global $s; $s = 5; }\n$s = 'abc';\nneed($s);\n";
    assert_eq!(mismatch_lines(src), vec![4], "{src}");
}

// ---------------------------------------------------------------------------
// The object and resource carriers read the same answer
// ---------------------------------------------------------------------------

fn undefined_method_lines(src: &str) -> Vec<u32> {
    findings_with(src, &mut Mock { absence: true, ..Mock::engine() })
        .into_iter()
        .filter(|d| d.id == "call.undefined-method")
        .map(|d| d.line)
        .collect()
}

#[test]
fn a_heap_object_is_forgotten_by_the_same_rule() {
    // `bump()` points `$o` at a `DateTimeImmutable`, whose `format()` exists.
    let rebind = "<?php\nfinal class Box {}\n\
                  function bump(): void { global $o; $o = new DateTimeImmutable(); }\n\
                  $o = new Box();\nbump();\necho $o->format('Y');\n";
    assert_eq!(undefined_method_lines(rebind), Vec::<u32>::new());
    let control =
        "<?php\nfinal class Box {}\n$o = new Box();\n$n = strlen('x');\necho $o->format('Y');\n";
    assert_eq!(undefined_method_lines(control), vec![5]);
}

// ---------------------------------------------------------------------------
// The routes that do not go through a call
// ---------------------------------------------------------------------------

#[test]
fn a_direct_write_through_dollar_globals_takes_the_total_clear() {
    // Known behaviour, not this rule: a superglobal base is never frame-private
    // (issue #641's target leg), so the barrier in front of it stays total.
    silent_after("", "$GLOBALS['s'] = 5;");
}

#[test]
fn include_extract_variable_variables_and_references_are_silent_by_the_poisoned_scope() {
    // Known silence, not this rule: each is on the ADR-0001 give-up list, so the
    // top-level scope is poisoned and holds no fact to convict with at all —
    // before the route runs as much as after it.
    silent_after("", "include __DIR__ . '/assigns.php';");
    silent_after("", "extract(['s' => 5]);");
    silent_after("", "$n = 's';\n$$n = 5;");
    silent_after("", "$t = &$s;\n$t = 5;");
}

#[test]
fn a_call_earlier_in_the_same_statement_is_not_ordered_against_a_later_argument() {
    // Known behaviour: the statement's checks judge every argument against the
    // facts it was entered with, and the frame is forgotten after them. Here PHP
    // evaluates `bump2()` before it reads `$s`, so `$s` is `5` when `need2()`
    // receives it — this is a residue of the rule, reported, not a proof.
    let src = "<?php\nfunction bump2(): int { global $s; $s = 5; return 1; }\n\
               function need2(int $a, int $b): void {}\n$s = 'abc';\nneed2(bump2(), $s);\n";
    assert_eq!(mismatch_lines(src), vec![5], "{src}");
}

#[test]
fn userland_the_engine_runs_behind_a_data_signature_is_not_seen() {
    // Known behaviour, the calibration ADR-0070 already accepts at top level:
    // `strlen($t)` runs `__toString`, which can `global $s` — probed at 8.5.10,
    // this exits 0 — but `strlen` takes no callee, so nothing is forgotten.
    let src = "<?php\nfinal class T {\n\
               public function __toString(): string { global $s; $s = 5; return ''; }\n}\n\
               $t = new T();\n$s = 'abc';\n$n = strlen($t);\nintdiv($s, 1);\n";
    assert_eq!(mismatch_lines(src), vec![8], "{src}");
}
