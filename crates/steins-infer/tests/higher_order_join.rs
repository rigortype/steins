//! Semantic higher-order effect join (ADR-0063 P1).
//!
//! A cataloged higher-order builtin has its own color joined with the envelope of
//! a callback it immediately invokes, through the via-provenance fixpoint
//! (ADR-0033, ADR-0018). While a callback body is visible, annotations are not
//! consulted. Fixtures cover catalog rows, the opaque-callback floor, deferred
//! invokers, and unnameable callables.

use steins_infer::{check, effect_summary, Diagnostic, EffectSummary, EFFECT_ID, PARAM_MISMATCH_ID};
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

// The new immediately-invoked rows

#[test]
fn php84_search_predicates_join_an_impure_callback() {
    // array_find/array_find_key/array_any/array_all run their predicate during the
    // call: an echoing predicate exceeds a Pure envelope, named by its own color
    // (`io.output.buffer`), not a generic maybe.
    for f in ["array_find", "array_find_key", "array_any", "array_all"] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): mixed {{\n    return {f}($xs, function ($x) {{ echo $x; return true; }});\n}}\n"
        );
        let d = one(&src);
        assert!(d.message.contains("io.output.buffer"), "{f}: names the output effect: {}", d.message);
        assert!(d.message.contains("closure"), "{f}: closure provenance: {}", d.message);
        assert!(d.message.contains("#[\\Steins\\Pure]"), "{f}: {}", d.message);
    }
}

#[test]
fn php84_search_predicates_stay_silent_for_a_pure_callback() {
    for f in ["array_find", "array_find_key", "array_any", "array_all"] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): mixed {{\n    return {f}($xs, function ($x) {{ return $x > 1; }});\n}}\n"
        );
        assert_eq!(effects(&src).len(), 0, "{f}: pure predicate → silent");
    }
}

#[test]
fn array_walk_recursive_joins_its_callback() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): void {\n    array_walk_recursive($xs, function ($v) { echo $v; });\n}\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

#[test]
fn iterator_apply_joins_its_callback() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(\\Iterator $it): void {\n    iterator_apply($it, function () { echo \"x\"; return true; });\n}\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

#[test]
fn a_new_row_carries_a_named_user_callback_too() {
    // The join is not closure-only: a named user function at the cataloged
    // position edges into the fixpoint the same way.
    let src = "<?php\nfunction shout($x) { echo $x; return true; }\n#[\\Steins\\Pure]\nfunction f(array $xs): mixed { return array_find($xs, 'shout'); }\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

// The opaque-callback floor

#[test]
fn opaque_callback_at_a_cataloged_position_keeps_the_builtins_own_color() {
    // An unresolvable `callable` argument proves nothing about the callback, so no
    // finding is manufactured — but the builtin's own color is untouched (today
    // these invokers are uncolored, so the observable floor is: silent + `…?`).
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(callable $cb, array $xs): mixed {\n    return array_find($xs, $cb);\n}\n";
    assert_eq!(effects(src).len(), 0, "unknown callback → no proven finding");
    assert!(!summary(src, "f").exhaustive, "unknown callback taints exhaustiveness (…?)");
}

// Deliberate exclusions contribute nothing new

#[test]
fn deferred_invokers_contribute_nothing_new() {
    // set_error_handler & friends STORE the callable; the engine invokes it later.
    // They have no row, so P1 adds no finding for them.
    for f in [
        "set_error_handler",
        "set_exception_handler",
        "spl_autoload_register",
        "register_tick_function",
    ] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\nfunction f(): void {{\n    {f}(function () {{ echo \"x\"; }});\n}}\n"
        );
        assert_eq!(effects(&src).len(), 0, "{f}: non-immediate position adds nothing");
    }
}

#[test]
fn register_shutdown_function_is_unchanged_by_p1() {
    // The one grandfathered Deferred row still propagates its callback's effects
    // (ADR-0033: Deferred claims nothing about WHEN, not whether) — P1 neither
    // widens nor narrows it.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void {\n    register_shutdown_function(function () { echo \"bye\"; });\n}\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

#[test]
fn preg_replace_callback_array_is_excluded() {
    // The callables live as values inside the array at position 0 — not a
    // positional callback argument, so the catalog cannot name them and the call
    // contributes only preg_replace_callback_array's own (uncolored) base.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): string {\n    return preg_replace_callback_array(['/a/' => function ($m) { echo $m[0]; return \"\"; }], $s);\n}\n";
    assert_eq!(effects(src).len(), 0, "unexpressible callback position stays silent");
}

// The join is a join, not a replacement

#[test]
fn a_declared_envelope_admits_the_matching_callback_color() {
    // #[\Steins\Effect('io.output')] admits the echoing callback; the join is checked
    // against the envelope with ADR-0018 prefix subsumption, and the nondet.random
    // sibling still exceeds.
    let admitted = "<?php\n#[\\Steins\\Effect('io.output')]\nfunction f(array $xs): array {\n    return array_map(function ($x) { echo $x; return $x; }, $xs);\n}\n";
    assert_eq!(effects(admitted).len(), 0, "declared io.output admits the callback's output");
    let exceeded = "<?php\n#[\\Steins\\Effect('io.output')]\nfunction f(array $xs): array {\n    return array_map(function ($x) { return $x + rand(); }, $xs);\n}\n";
    let d = one(exceeded);
    assert!(d.message.contains("nondet.random"), "{}", d.message);
}

// The C9 pure-callable consumer sees the join automatically

fn param_mismatches(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).collect()
}

#[test]
fn pure_callable_obligation_reads_the_higher_order_join() {
    // C9's PurityOracle runs on the same fixpoint, so a closure whose only impurity
    // arrives *through* a cataloged invoker's callback is judged impure — no extra
    // wiring, and the pure counterpart stays silent.
    const TAKES: &str = "<?php /** @param pure-callable $cb */ function takes($cb): void {}\n";
    let impure = format!(
        "{TAKES}takes(static function (array $xs): mixed {{ return array_find($xs, function ($x) {{ echo $x; return true; }}); }});"
    );
    let d = param_mismatches(&impure);
    assert_eq!(d.len(), 1, "joined callback impurity violates pure-callable: {d:#?}");
    assert!(d[0].message.contains("not pure"), "{}", d[0].message);

    let pure = format!(
        "{TAKES}takes(static function (array $xs): mixed {{ return array_find($xs, function ($x) {{ return $x > 1; }}); }});"
    );
    assert_eq!(param_mismatches(&pure).len(), 0, "pure callback → obligation holds");
}

#[test]
fn nested_higher_order_calls_join_transitively() {
    // array_map over a closure that itself calls array_find over an echoing
    // closure — the fixpoint carries the color up through both hops.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $xs): array {\n    return array_map(function ($x) { return array_find($x, function ($y) { echo $y; return true; }); }, $xs);\n}\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}
