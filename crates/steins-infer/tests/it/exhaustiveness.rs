//! Effects-exhaustiveness tests (ADR-0005 / ADR-0020): the `annotate` margin
//! exposes the inferred effect set for *every* function/method, and marks it
//! **non-exhaustive** (the `…?` marker) the moment the body reaches a call the
//! analyzer cannot classify — an uncatalogued builtin, a dynamic call, or an
//! unresolved method. Non-exhaustiveness taints callers through the call graph.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

/// The summary for a named function/method (`f` or `Foo::bar`).
fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

// ---- A fully-catalogued body is exhaustive {} ----------------------------

#[test]
fn effect_free_catalogued_body_is_exhaustive_empty() {
    let src = "<?php\nfunction f(string $s): string { $x = 1 + 2; return strtolower($s); }\n";
    let s = summary(src, "f");
    assert!(s.labels.is_empty(), "no proven effects, got: {:?}", s.labels);
    assert!(s.exhaustive, "all calls classified → exhaustive");
}

#[test]
fn empty_body_is_exhaustive_empty() {
    let s = summary("<?php\nfunction f(): void {}\n", "f");
    assert!(s.labels.is_empty());
    assert!(s.exhaustive);
}

#[test]
fn proven_effect_is_still_exhaustive() {
    let src = "<?php\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let s = summary(src, "f");
    assert_eq!(s.labels, vec!["io.fs.write"]);
    assert!(s.exhaustive, "a catalogued effect is fully known");
}

// ---- An uncatalogued call makes the body non-exhaustive ------------------

#[test]
fn uncatalogued_builtin_makes_body_non_exhaustive() {
    let src = "<?php\nfunction f(): void { some_unknown_fn(); }\n";
    let s = summary(src, "f");
    assert!(s.labels.is_empty(), "uncatalogued → no proven effect");
    assert!(!s.exhaustive, "uncatalogued builtin → not exhaustive (…?)");
}

#[test]
fn dynamic_call_makes_body_non_exhaustive() {
    let src = "<?php\nfunction f(callable $cb): void { $cb(); }\n";
    let s = summary(src, "f");
    assert!(!s.exhaustive, "a dynamic call cannot be proven effect-free");
}

#[test]
fn unresolved_method_call_makes_body_non_exhaustive() {
    let src = "<?php\nfunction f(object $obj): void { $obj->m(); }\n";
    let s = summary(src, "f");
    assert!(!s.exhaustive, "an unresolved method call → not exhaustive");
}

// ---- Non-exhaustiveness taints callers through the fixpoint ---------------

#[test]
fn non_exhaustiveness_taints_callers() {
    // f → g → some_unknown_fn(): g is non-exhaustive, and that taints f even
    // though f's own body only makes a fully-resolved same-file call.
    let src = "<?php\nfunction f(): void { g(); }\nfunction g(): void { some_unknown_fn(); }\n";
    let f = summary(src, "f");
    let g = summary(src, "g");
    assert!(!g.exhaustive, "g calls an uncatalogued builtin");
    assert!(!f.exhaustive, "f's callee is non-exhaustive → f is too");
}

#[test]
fn exhaustive_callee_does_not_taint() {
    // f → g → file_put_contents: g is fully known, so f stays exhaustive and
    // carries the propagated label.
    let src = "<?php\nfunction f(): void { g(); }\nfunction g(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let f = summary(src, "f");
    assert!(f.exhaustive, "a fully-known callee does not taint");
    assert_eq!(f.labels, vec!["io.fs.write"], "the effect propagates up the edge");
}

#[test]
fn labels_are_sorted_and_deduplicated() {
    let src = "<?php\nfunction f(): void { file_put_contents(\"/x\", \"y\"); unlink(\"/z\"); time(); }\n";
    let s = summary(src, "f");
    assert_eq!(s.labels, vec!["io.fs.write", "nondet.time"], "sorted + deduped");
    assert!(s.exhaustive);
}

// ---- Methods are summarized too (abstract ones omitted) -------------------

#[test]
fn method_effects_are_summarized() {
    let src = "<?php\nfinal class Svc {\n  public function run(): void { rand(); }\n}\n";
    let s = summary(src, "Svc::run");
    assert_eq!(s.labels, vec!["nondet.random"]);
    assert!(s.exhaustive);
}

#[test]
fn abstract_method_has_no_summary() {
    let src = "<?php\nabstract class A {\n  abstract public function m(): void;\n}\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    let all = effect_summary(&tree, &functions, &classes);
    assert!(all.iter().all(|s| s.symbol != "A::m"), "abstract method omitted (no body to prove)");
}

// ---- Gap kinds name the cause of each `…?` (ADR-0099 §5) ------------------

/// A body that is exhaustive carries no kinds, and one that is not carries the
/// kinds its sites produced; a caller inherits its callee's, so every `…?` names
/// a cause even where the caller's own body is clean.
#[test]
fn gap_kinds_are_empty_exactly_when_exhaustive_and_propagate_to_callers() {
    let src = "<?php
function clean(): int { return 1; }
function open(callable $cb): void { $cb(); some_unknown_fn(); }
function caller(): void { open(fn() => 1); }
function fine(): int { return clean(); }
";
    let clean = summary(src, "clean");
    assert!(clean.exhaustive && clean.gaps.is_empty(), "got: {:?}", clean.gaps);
    assert!(clean.throws_exhaustive && clean.throws_gaps.is_empty());

    let open = summary(src, "open");
    assert_eq!(open.gaps, vec!["dynamic-callee", "unknown-function"]);
    assert_eq!(open.throws_gaps, vec!["dynamic-callee", "unknown-function"]);

    // `caller` has no gap of its own and inherits both through the call edge.
    let caller = summary(src, "caller");
    assert!(!caller.exhaustive);
    assert_eq!(caller.gaps, open.gaps);
    assert_eq!(caller.throws_gaps, open.throws_gaps);

    let fine = summary(src, "fine");
    assert!(fine.exhaustive && fine.gaps.is_empty(), "a clean callee leaves no kind");
}
