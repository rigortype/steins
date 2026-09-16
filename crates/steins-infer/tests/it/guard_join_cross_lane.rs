//! A guard's join reads BOTH carriers of a phpdoc-narrowed variable (issue #589).
//!
//! `if ($i === 1) {}` over `@param 1|2` splits the knowledge across lanes: the
//! then branch holds `Singleton(1)` in the value lane alone (`Refine::Exact`
//! mints it and unbinds the arm lane), the else branch holds the residue `2` in
//! the arm lane alone (a phpdoc-only parameter never seeds an env fact). Each
//! lane-local join then failed its present-in-every-branch test and BOTH
//! carriers dropped, so the variable rendered `unknown` for the rest of the
//! function — under any strict-equality guard, for every phpdoc-narrowed
//! variable.
//!
//! The fix lets a branch with no env fact contribute the lowering of its own
//! arm lane to the env join. These fixtures pin the witness matrix from the
//! issue: the four guard shapes that must survive, the two bystanders that must
//! not move, and the rebind case that must never resurrect the pre-branch union.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` message body of `src`, in source order.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// The dump after `body`, inside `function f($i)` under `@param 1|2 $i`.
fn after_guard(body: &str) -> String {
    let src = format!(
        "<?php\n/** @param 1|2 $i */\nfunction f($i): void {{\n{body}\n\\PHPStan\\dumpType($i);\n}}\n"
    );
    let ds = dumps(&src);
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].clone()
}

#[test]
fn an_empty_then_branch_keeps_the_declared_union() {
    assert_eq!(after_guard("if ($i === 1) { }"), "dumped type: 1|2 (asserted)");
}

#[test]
fn a_non_empty_then_branch_keeps_the_declared_union() {
    assert_eq!(after_guard("if ($i === 1) { $x = 'side'; }"), "dumped type: 1|2 (asserted)");
}

#[test]
fn the_mirrored_guard_keeps_the_declared_union() {
    // `!==` mirrors the carriers: the FIRST fall-through branch is the lane-only
    // one, so this pins the union-of-keys half of the fix.
    assert_eq!(after_guard("if ($i !== 1) { }"), "dumped type: 1|2 (asserted)");
}

#[test]
fn an_explicit_empty_else_keeps_the_declared_union() {
    assert_eq!(after_guard("if ($i === 1) { } else { }"), "dumped type: 1|2 (asserted)");
}

#[test]
fn a_bystander_variable_is_untouched() {
    let src = "<?php\n/** @param 1|2 $i */\nfunction f($i): void {\n$b = 'kept';\nif ($i === 1) { }\n\\PHPStan\\dumpType($b);\n}\n";
    assert_eq!(dumps(src), vec!["dumped type: 'kept'"]);
}

#[test]
fn a_native_typed_parameter_is_untouched() {
    let src =
        "<?php\nfunction f(int $i): void {\nif ($i === 1) { }\n\\PHPStan\\dumpType($i);\n}\n";
    assert_eq!(dumps(src), vec!["dumped type: int"]);
}

#[test]
fn a_surviving_shape_union_lane_keeps_its_arm_precision() {
    // A count guard subtracts the lane WITHOUT unbinding it, so the lane
    // survives every branch and `join_stores`' arm union is the render. The
    // fallback must not mint a value fact beside it: the fact would be the two
    // shapes' blur (`array{0: string, 1?: ''}`) and would outrank the lane at
    // every fact read (the narrow-tagged-union rows of the #589 calibration).
    let src = "<?php\n/** @param array{string, ''}|array{string} $arr */\nfunction f($arr): void {\nif (count($arr) === 1) { }\n\\PHPStan\\dumpType($arr);\n}\n";
    assert_eq!(dumps(src), vec!["dumped type: list{string}|array{string, ''} (asserted)"]);
}

#[test]
fn a_rebind_in_the_branch_never_resurrects_the_pre_branch_union() {
    // The then branch's carrier is the NEW binding's `5`; the else branch's is
    // the lane residue `2`. `1|2` holds on no fall-through path and must not
    // reappear.
    assert_eq!(after_guard("if ($i === 1) { $i = 5; }"), "dumped type: 2|5 (asserted)");
}
