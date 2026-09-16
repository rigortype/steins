//! ADR-0063 P4 acceptance tests: `@pure-unless-callable-is-impure` and its by-ref
//! sister, lowered.
//!
//! ADR-0063 decision 1 is that inference beats annotation *while the callback is
//! visible*. Decision 2 is what to do when it is not: honor the upstream-merged
//! declaration. So a tagged userland function is a **userland catalog row** — it
//! runs through the same HigherOrder/edge machinery a builtin invoker does, and
//! the contract's only extra power is to discharge what inference left unknown,
//! never to overrule what inference proved (ADR-0037).
//!
//! Both spellings are taken verbatim from `phpstan/phpdoc-parser` 2.3.3 (the copy
//! vendored at `harness/phpdoc-oracle`), whose grammar for either tag is
//! `parseRequiredVariableName` plus an optional description.

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

/// A userland invoker declaring the callable condition: its body can say nothing
/// about `$cb` (a dynamic call is an opaque taint), which is exactly the blindness
/// the contract answers.
const APPLY: &str = "<?php\n\
    /**\n\
     * @pure-unless-callable-is-impure $cb\n\
     */\n\
    function applyAll(array $xs, callable $cb): array {\n\
        $out = [];\n\
        foreach ($xs as $x) { $out[] = $cb($x); }\n\
        return $out;\n\
    }\n";

// The callable condition

#[test]
fn a_pure_callback_at_the_flagged_position_stays_clean() {
    let src = format!(
        "{APPLY}#[\\Steins\\Pure]\nfunction f(array $xs): array {{ return applyAll($xs, function ($x) {{ return $x + 1; }}); }}\n"
    );
    assert_eq!(effects(&src).len(), 0, "pure callback → the tagged call is pure: {:#?}", effects(&src));
}

#[test]
fn an_echoing_callback_at_the_flagged_position_exceeds_pure() {
    let src = format!(
        "{APPLY}#[\\Steins\\Pure]\nfunction f(array $xs): array {{ return applyAll($xs, function ($x) {{ echo $x; return $x; }}); }}\n"
    );
    let d = one(&src);
    assert!(d.message.contains("io.output.buffer"), "names the callback's color: {}", d.message);
    assert!(d.message.contains("closure"), "closure provenance: {}", d.message);
    assert!(d.message.contains("#[\\Steins\\Pure]"), "{}", d.message);
}

#[test]
fn a_named_function_callback_joins_too() {
    let src = format!(
        "{APPLY}function shout(string $s): string {{ echo $s; return $s; }}\n\
         #[\\Steins\\Pure]\nfunction f(array $xs): array {{ return applyAll($xs, 'shout'); }}\n"
    );
    let d = one(&src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

#[test]
fn an_opaque_callable_at_the_flagged_position_keeps_the_taint() {
    // The contract cannot resolve what the analyzer cannot see either: an opaque
    // `callable` in the flagged slot leaves the call non-exhaustive (`…?`) and silent.
    let src = format!(
        "{APPLY}#[\\Steins\\Pure]\nfunction f(array $xs, callable $g): array {{ return applyAll($xs, $g); }}\n"
    );
    assert_eq!(effects(&src).len(), 0, "an unknown callback proves nothing");
    assert!(!summary(&src, "f").exhaustive, "an opaque callable taints exhaustiveness");
}

#[test]
fn a_resolved_callback_discharges_the_callees_opaque_taint() {
    // The substantive gain: `applyAll`'s body is permanently tainted by its
    // `$cb($x)` dynamic call, but a decided callable discharges that unknown.
    let src = format!(
        "{APPLY}function f(array $xs): array {{ return applyAll($xs, function ($x) {{ return $x + 1; }}); }}\n"
    );
    assert!(!summary(&src, "applyAll").exhaustive, "the callee itself stays honest about `$cb()`");
    assert!(summary(&src, "f").exhaustive, "the contract discharges it at a decided call site");
}

#[test]
fn proven_effects_of_the_tagged_function_still_propagate() {
    // ADR-0037: a declaration may answer the unknown, never overrule the proven.
    // `logAll` is tagged AND echoes on its own — the echo is not laundered.
    let src = "<?php\n\
        /** @pure-unless-callable-is-impure $cb */\n\
        function logAll(array $xs, callable $cb): void { echo 'start'; foreach ($xs as $x) { $cb($x); } }\n\
        #[\\Steins\\Pure]\n\
        function f(array $xs): void { logAll($xs, function ($x) { return $x; }); }\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "the callee's own echo survives: {}", d.message);
}

#[test]
fn an_absent_flagged_argument_makes_the_condition_vacuous() {
    // `@pure-unless-callable-is-impure $cb` on an optional parameter: no callable
    // supplied, nothing to be impure about.
    let src = "<?php\n\
        /** @pure-unless-callable-is-impure $cb */\n\
        function maybeApply(array $xs, ?callable $cb = null): array { return $cb === null ? $xs : array_map($cb, $xs); }\n\
        function f(array $xs): array { return maybeApply($xs); }\n";
    assert!(summary(src, "f").exhaustive, "no callable passed → the contract is satisfied");
}

// The by-ref sister

/// A userland out-parameter writer whose write the effect scan cannot see: the
/// assignment to `&$out` is not an effect origin, so the *only* thing that can
/// color a call to it is the declaration. This is the sister tag's real habitat.
const MATCHER: &str = "/** @pure-unless-parameter-passed $out */\n\
    function matches(string $re, string $s, &$out = null): bool { $out = []; return $re === $s; }\n";

#[test]
fn pure_unless_parameter_passed_is_a_userland_out_param_row() {
    // The sister tag turns a userland function into exactly what P2's catalog
    // rows are: a conditional by-ref color, resolved against the argument.
    let omitted =
        format!("<?php\n{MATCHER}function f(string $s): bool {{ return matches('/a/', $s); }}\n");
    assert!(
        summary(&omitted, "f").labels.is_empty(),
        "parameter not passed → pure: {:?}",
        summary(&omitted, "f").labels
    );

    let local =
        format!("<?php\n{MATCHER}function f(string $s): bool {{ return matches('/a/', $s, $m); }}\n");
    assert_eq!(
        summary(&local, "f").labels,
        vec!["mutate.local".to_owned()],
        "passed into a frame local → the tolerated color"
    );
}

#[test]
fn the_sister_tag_respects_the_target_leg_too() {
    // A frame-local target is tolerated by Pure...
    let local = format!("<?php\n{MATCHER}#[\\Steins\\Pure]\nfunction f(string $s): bool {{ return matches('/a/', $s, $m); }}\n");
    assert_eq!(effects(&local).len(), 0, "local target → clean: {:#?}", effects(&local));

    // ...a superglobal one is not.
    let global = format!("<?php\n{MATCHER}#[\\Steins\\Pure]\nfunction f(string $s): bool {{ return matches('/a/', $s, $_SESSION['m']); }}\n");
    let d = {
        let f = effects(&global);
        assert_eq!(f.len(), 1, "expected one finding, got {f:#?}");
        f.into_iter().next().unwrap()
    };
    assert!(d.message.contains("global.write"), "{}", d.message);
    assert!(d.message.contains("matches()"), "named by the tagged callee: {}", d.message);
}

#[test]
fn a_visible_by_ref_write_inside_the_helper_is_proven_and_wins() {
    // ADR-0037 boundary: when the helper's body passes its own by-ref parameter to
    // a catalog out-param builtin, P2 proves the write escapes the frame (`mutate`,
    // not `mutate.local`) — the declaration refines the unknown, not the known.
    let src = "<?php\n\
        /** @pure-unless-parameter-passed $out */\n\
        function matches(string $re, string $s, &$out = null): bool { return (bool) preg_match($re, $s, $out); }\n\
        #[\\Steins\\Pure]\n\
        function f(string $s): bool { return matches('/a/', $s, $m); }\n";
    let d = one(src);
    assert!(d.message.contains("mutate"), "{}", d.message);
    assert!(!d.message.contains("mutate.local"), "the escape is proven, not local: {}", d.message);
}

// Spelling fidelity

#[test]
fn the_phpstan_prefixed_spelling_is_honored() {
    let src = "<?php\n\
        /** @phpstan-pure-unless-callable-is-impure $cb */\n\
        function applyAll(array $xs, callable $cb): array { $o = []; foreach ($xs as $x) { $o[] = $cb($x); } return $o; }\n\
        #[\\Steins\\Pure]\n\
        function f(array $xs): array { return applyAll($xs, function ($x) { echo $x; return $x; }); }\n";
    let d = one(src);
    assert!(d.message.contains("io.output.buffer"), "{}", d.message);
}

#[test]
fn a_tag_naming_an_unknown_parameter_costs_only_itself() {
    // A stale tag (renamed parameter) is dropped, not diagnosed — and the call
    // falls back to the plain edge, so the callee's honest taint survives.
    let src = "<?php\n\
        /** @pure-unless-callable-is-impure $callback */\n\
        function applyAll(array $xs, callable $cb): array { $o = []; foreach ($xs as $x) { $o[] = $cb($x); } return $o; }\n\
        function f(array $xs): array { return applyAll($xs, function ($x) { return $x; }); }\n";
    assert_eq!(effects(src).len(), 0, "no diagnostic for a stale tag");
    assert!(!summary(src, "f").exhaustive, "and no discharge from it either");
}

#[test]
fn an_untagged_invoker_is_unchanged() {
    // Control: the identical function without the tag keeps the pre-P4 behavior —
    // a plain edge, the callee's taint inherited, the callback ignored.
    let src = "<?php\n\
        function applyAll(array $xs, callable $cb): array { $o = []; foreach ($xs as $x) { $o[] = $cb($x); } return $o; }\n\
        #[\\Steins\\Pure]\n\
        function f(array $xs): array { return applyAll($xs, function ($x) { echo $x; return $x; }); }\n";
    assert_eq!(effects(src).len(), 0, "without the tag nothing is proven about $cb");
    assert!(!summary(src, "f").exhaustive, "and the taint stands");
}
