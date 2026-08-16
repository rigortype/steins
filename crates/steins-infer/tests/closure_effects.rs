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

// THE HEADLINE: Pure + array_map(inline impure closure)

#[test]
fn pure_array_map_inline_impure_closure_fires_with_callback_provenance() {
    // array_map's own base is pure, but the inline callback echoes → the Pure
    // envelope is exceeded, reported with the callback's own origin in provenance.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array {\n    return array_map(function ($x) { echo $x; return $x; }, $xs);\n}\n";
    let d = one(src);
    assert_eq!(d.id, EFFECT_ID);
    assert!(d.message.contains("io.output.buffer"), "names the output effect: {}", d.message);
    assert!(d.message.contains("closure"), "names the closure in provenance: {}", d.message);
    assert!(d.message.contains("#[\\Steins\\Pure]"), "{}", d.message);
}

#[test]
fn pure_array_map_pure_closure_is_silent() {
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

// Reversed-argument shape: array_filter

#[test]
fn array_filter_reversed_args_finds_callback_at_position_1() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array {\n    return array_filter($xs, function ($x) { echo $x; return true; });\n}\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

#[test]
fn array_filter_one_arg_form_has_no_callback() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_filter($xs); }\n";
    assert_eq!(effects(src).len(), 0);
}

// Deferred invoker still propagates effects

#[test]
fn register_shutdown_function_deferred_effects_propagate() {
    // register_shutdown_function is DEFERRED, but its callback's effects still join
    // the caller's set (ADR-0033: Deferred claims nothing about WHEN, not whether).
    //
    // TWO findings, and the pair is the point. Registering a handler writes the
    // engine's dispatch table (`global.write`, effects_gaps.md §5), and what the
    // registered callback does is a second, independent effect carried by the
    // invocation shape. Reporting only the first would lose the deferred
    // propagation this test is about; reporting only the second would hide that
    // the registration itself is a write.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void {\n    register_shutdown_function(function () { echo \"bye\"; });\n}\n";
    let found = effects(src);
    assert_eq!(found.len(), 2, "the registration AND the callback, got: {found:#?}");
    assert!(
        found.iter().any(|d| d.message.contains("io.output.buffer") && d.message.contains("closure")),
        "deferred callback effect propagates: {found:#?}"
    );
    assert!(
        found.iter().any(|d| d.message.contains("global.write")
            && d.message.contains("register_shutdown_function")),
        "…and registering it is itself a write: {found:#?}"
    );
}

// Named / string callables

#[test]
fn array_map_string_builtin_callback_is_pure() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_map('strtolower', $xs); }\n";
    assert_eq!(effects(src).len(), 0);
}

#[test]
fn array_map_user_impure_named_callback_fires() {
    let src = "<?php\nfunction shout($x) { echo $x; return $x; }\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_map('shout', $xs); }\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

#[test]
fn array_map_first_class_callable_callback_fires() {
    let src = "<?php\nfunction shout($x) { echo $x; return $x; }\n#[\\Steins\\Pure]\nfunction f(array $xs): array { return array_map(shout(...), $xs); }\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

// Direct $fn() closure effect feeding

#[test]
fn direct_fn_call_on_local_closure_feeds_effects_with_provenance() {
    // $fn() on a body-local single-assignment closure feeds the closure's effects
    // to the enclosing Pure function, with the closure definition in provenance.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void {\n    $log = function () { echo \"x\"; };\n    $log();\n}\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
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

// Throws through higher-order builtins

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

// Issue #279: HigherOrder dispatch through an aliased builtin import. Fixed by
// routing the call through `Cx::resolve_invoker_function`'s resolved catalog name
// instead of the call's own spelling, so an alias no longer misses invoker treatment.

#[test]
fn an_aliased_usort_import_dispatches_and_colors_like_the_spelled_call() {
    let aliased = "<?php\nuse function usort as u;\n\
                   function f(array $xs): array {\n    \
                   u($xs, function ($a, $b) { echo $a; return $a <=> $b; });\n    \
                   return $xs;\n}\n";
    let spelled = "<?php\nfunction f(array $xs): array {\n    \
                   usort($xs, function ($a, $b) { echo $a; return $a <=> $b; });\n    \
                   return $xs;\n}\n";
    let sa = summary(aliased, "f");
    let ss = summary(spelled, "f");
    assert_eq!(sa.labels, ss.labels, "aliased usort colors identically to the spelled call");
    assert!(
        sa.labels.iter().any(|l| l == "io.output.buffer"),
        "the comparator's echo propagates through the alias: {:?}",
        sa.labels
    );
}

#[test]
fn an_aliased_usort_import_propagates_callback_throws_like_the_spelled_call() {
    // Throws-pass twin of the effects test above: only reached if
    // `resolve_invoker_function` finds the comparator's callback slot through the alias.
    let aliased = "<?php\nuse function usort as u;\n\
                   function f(array $xs): array {\n    \
                   u($xs, function ($a, $b) { return intdiv($a, $b); });\n    \
                   return $xs;\n}\n";
    let spelled = "<?php\nfunction f(array $xs): array {\n    \
                   usort($xs, function ($a, $b) { return intdiv($a, $b); });\n    \
                   return $xs;\n}\n";
    let sa = summary(aliased, "f");
    let ss = summary(spelled, "f");
    assert_eq!(sa.throws, ss.throws, "aliased usort's callback throws identically to the spelled call");
    assert!(
        sa.throws.iter().any(|t| t.contains("DivisionByZeroError")),
        "callback throw propagates through the alias: {:?}",
        sa.throws
    );
}

#[test]
fn an_aliased_import_of_a_shape_named_project_function_is_unaffected() {
    // Negative twin: a project function sharing a name with a catalog invocation
    // shape (`usort`) is never a catalog invoker (a `Site`, not a spelling), so an
    // aliased import must join the shadowing function's OWN effects, never `usort`'s.
    let unaliased = "<?php\nnamespace App\\Sorting;\n\
                      function usort(array $a, callable $cmp): array { echo \"shadow\"; return $a; }\n\
                      namespace App;\nuse function App\\Sorting\\usort;\n\
                      function f(array $xs): array { return usort($xs, function ($a, $b) { return $a <=> $b; }); }\n";
    let aliased = "<?php\nnamespace App\\Sorting;\n\
                   function usort(array $a, callable $cmp): array { echo \"shadow\"; return $a; }\n\
                   namespace App;\nuse function App\\Sorting\\usort as u;\n\
                   function f(array $xs): array { return u($xs, function ($a, $b) { return $a <=> $b; }); }\n";
    let su = summary(unaliased, "f");
    let sa = summary(aliased, "f");
    assert_eq!(su.labels, sa.labels, "aliasing a shadowing project function changes nothing");
    assert!(
        su.labels.iter().any(|l| l == "io.output.buffer"),
        "the shadow's own echo joins, not usort's shape table: {:?}",
        su.labels
    );
}
