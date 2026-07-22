//! Stage B/C-runtime acceptance tests (ADR-0033): effects/throws through
//! higher-order builtins (the invocation-shape catalog — the "array_map
//! redemption") and through direct `$fn()` closure calls, plus the honest `…?`
//! taint for unknown callables.

use steins_infer::{check, effect_summary, Diagnostic, EffectSummary, EFFECT_ID};
use steins_syntax::SourceTree;

fn effects(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn one(src: &str) -> Diagnostic {
    let f = effects(src);
    assert_eq!(f.len(), 1, "expected exactly one effect finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

// ---- THE HEADLINE: Pure + array_map(inline impure closure) ------------------

#[test]
fn pure_array_map_inline_impure_closure_fires_with_callback_provenance() {
    // The final Steins answer to the PHPStan conditional-purity saga: array_map's
    // own base is pure, but the inline callback echoes → the Pure envelope is
    // exceeded, reported with the callback's own origin (echo) in the provenance.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array {\n    return array_map(function ($x) { echo $x; return $x; }, $xs);\n}\n";
    let d = one(src);
    assert_eq!(d.id, EFFECT_ID);
    assert!(d.message.contains("output"), "names the output effect: {}", d.message);
    assert!(d.message.contains("closure"), "names the closure in provenance: {}", d.message);
    assert!(d.message.contains("#[\\Steins\\Pure]"), "{}", d.message);
}

#[test]
fn pure_array_map_pure_closure_is_silent() {
    // A pure inline callback → array_map contributes nothing → Pure holds.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array {\n    return array_map(function ($x) { return $x + 1; }, $xs);\n}\n";
    assert_eq!(effects(src).len(), 0, "pure callback → silent");
}

#[test]
fn pure_array_map_unknown_callable_is_silent_but_taints() {
    // A `$var` callback is unresolvable → NO effect finding (…? only), and the
    // function's effect set is marked non-exhaustive.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(callable $cb, array $xs): array {\n    return array_map($cb, $xs);\n}\n";
    assert_eq!(effects(src).len(), 0, "unknown callback → no proven finding");
    assert!(!summary(src, "f").exhaustive, "unknown callback taints exhaustiveness (…?)");
}

// ---- Reversed-argument shape: array_filter ---------------------------------

#[test]
fn array_filter_reversed_args_finds_callback_at_position_1() {
    // array_filter($xs, $cb) — callback is the SECOND argument. The impure closure
    // there must still be found.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array {\n    return array_filter($xs, function ($x) { echo $x; return true; });\n}\n";
    let d = one(src);
    assert!(d.message.contains("output"), "{}", d.message);
}

#[test]
fn array_filter_one_arg_form_has_no_callback() {
    // The 1-arg form array_filter($xs) has no callback → nothing to join, silent,
    // and (array_filter being a shaped pure base) exhaustive.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_filter($xs); }\n";
    assert_eq!(effects(src).len(), 0);
}

// ---- Deferred invoker still propagates effects -----------------------------

#[test]
fn register_shutdown_function_deferred_effects_propagate() {
    // register_shutdown_function is DEFERRED, but its callback's effects still join
    // the caller's set (ADR-0033: Deferred claims nothing about WHEN, not whether).
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void {\n    register_shutdown_function(function () { echo \"bye\"; });\n}\n";
    let d = one(src);
    assert!(d.message.contains("output"), "deferred callback effect propagates: {}", d.message);
}

// ---- Named / string callables ----------------------------------------------

#[test]
fn array_map_string_builtin_callback_is_pure() {
    // 'strtolower' is a catalogued-pure builtin callback → silent.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_map('strtolower', $xs); }\n";
    assert_eq!(effects(src).len(), 0);
}

#[test]
fn array_map_user_impure_named_callback_fires() {
    // A user function callback that echoes → its effect joins.
    let src = "<?php\nfunction shout($x) { echo $x; return $x; }\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_map('shout', $xs); }\n";
    let d = one(src);
    assert!(d.message.contains("output"), "{}", d.message);
}

#[test]
fn array_map_first_class_callable_callback_fires() {
    // A first-class callable `shout(...)` as the callback.
    let src = "<?php\nfunction shout($x) { echo $x; return $x; }\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_map(shout(...), $xs); }\n";
    let d = one(src);
    assert!(d.message.contains("output"), "{}", d.message);
}

// ---- Direct $fn() closure effect feeding -----------------------------------

#[test]
fn direct_fn_call_on_local_closure_feeds_effects_with_provenance() {
    // $fn() on a body-local single-assignment closure feeds the closure's effects
    // to the enclosing Pure function, with the closure definition in provenance.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void {\n    $log = function () { echo \"x\"; };\n    $log();\n}\n";
    let d = one(src);
    assert!(d.message.contains("output"), "{}", d.message);
    assert!(d.message.contains("closure"), "closure provenance: {}", d.message);
}

#[test]
fn direct_fn_call_reassigned_is_opaque_not_resolved() {
    // A variable assigned two different closures is ambiguous → $fn() stays an
    // honest opaque taint (no proven effect, non-exhaustive).
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(bool $c): void {\n    $log = function () { echo \"a\"; };\n    $log = function () { return 1; };\n    $log();\n}\n";
    assert_eq!(effects(src).len(), 0, "ambiguous closure → no proven finding");
    assert!(!summary(src, "f").exhaustive, "ambiguous $fn() taints (…?)");
}

// ---- Throws through higher-order builtins ----------------------------------

#[test]
fn array_map_callback_throws_propagate_to_summary() {
    // A callback that throws intdiv's DivisionByZeroError propagates the throw fact
    // to the enclosing function's inferred throw set.
    let src = "<?php\nfunction f(array $xs): array {\n    return array_map(function ($x) { return intdiv(1, $x); }, $xs);\n}\n";
    let s = summary(src, "f");
    assert!(
        s.throws.iter().any(|t| t.contains("DivisionByZeroError")),
        "callback throw propagates: {:?}",
        s.throws
    );
}
