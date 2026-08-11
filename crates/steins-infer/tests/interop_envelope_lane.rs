//! Interop envelopes in the **declared lane** (ADR-0082 role A, issue #303): a
//! call through an abstraction whose method carries one of upstream's purity tags
//! — `@phpstan-impure io.db`, `@phpstan-pure`, `@phpstan-all-methods-impure` —
//! contributes that bound to the caller's declared lane.
//!
//! The whole point of these tests is the **stratum asymmetry**. A checked
//! `#[\Steins\Effect]` envelope answers its call site: bound imported, taint
//! discharged. An interop envelope is a docblock nobody has verified here, so it
//! follows ADR-0068's plugin discipline instead: bound imported, taint kept. Every
//! case below pins one or the other, and the contrast pair pins both at once.
//!
//! Contract-checking the *declaring* function against its own interop envelope
//! (role B, `effect.envelope-exceeded`) is a later slice; nothing here emits
//! a diagnostic.

use steins_infer::{EffectSummary, FactKind, NoFold, annotate_facts, effect_summary};
use steins_syntax::SourceTree;

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// Every `annotate` effect-margin body in a source (ADR-0020 rendering).
fn effect_margins(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    annotate_facts(&tree, &functions, &classes, "test.php", &mut NoFold)
        .into_iter()
        .filter(|f| matches!(f.kind, FactKind::Effects { .. }))
        .map(|f| f.body())
        .collect()
}

// ---- THE HEADLINE: the same shape, two strata, two answers -------------------

#[test]
fn an_interop_impure_tag_contributes_its_bound_without_discharging_the_taint() {
    let src = concat!(
        "<?php\n",
        "interface Repo {\n",
        "    /** @phpstan-impure io.db */\n",
        "    public function find(int $id): string;\n",
        "}\n",
        "function f(Repo $r): string { return $r->find(1); }\n",
    );
    let s = summary(src, "f");
    assert!(s.labels.is_empty(), "nothing is PROVEN here, got: {:?}", s.labels);
    assert_eq!(s.declared, vec!["io.db"], "the docblock's label enters the declared lane");
    assert!(!s.exhaustive, "but an unchecked claim never claims exhaustiveness (ADR-0068)");
}

#[test]
fn the_attribute_spelling_of_the_same_bound_does_discharge_the_taint() {
    // The contrast control for the case above, byte-identical but for the
    // spelling. This asymmetry IS the trust stratification (ADR-0082 §1).
    let src = concat!(
        "<?php\n",
        "interface Repo {\n",
        "    #[\\Steins\\Effect('io.db')]\n",
        "    public function find(int $id): string;\n",
        "}\n",
        "function f(Repo $r): string { return $r->find(1); }\n",
    );
    let s = summary(src, "f");
    assert_eq!(s.declared, vec!["io.db"]);
    assert!(s.exhaustive, "a checked envelope answers its call site");
}

// ---- Class-level tags (ADR-0082 §5, upstream semantics verbatim) -------------

#[test]
fn a_class_level_all_methods_impure_tag_bounds_a_method_that_says_nothing() {
    let src = concat!(
        "<?php\n",
        "/** @phpstan-all-methods-impure io.net */\n",
        "interface Client {\n",
        "    public function get(string $url): string;\n",
        "}\n",
        "function f(Client $c): string { return $c->get('/x'); }\n",
    );
    let s = summary(src, "f");
    assert_eq!(s.declared, vec!["io.net"], "the class tag distributes over its own methods");
    assert!(!s.exhaustive, "still the unchecked stratum");
}

#[test]
fn a_method_level_tag_replaces_the_class_level_one_rather_than_joining_it() {
    // Upstream's nearest-wins rule, which is where the interop stratum
    // deliberately parts ways with the checked stratum's Liskov conjunction:
    // `io.net` from the class tag is GONE, not conjoined.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-all-methods-impure io.net */\n",
        "interface Client {\n",
        "    /** @phpstan-impure io.fs */\n",
        "    public function cache(string $k): string;\n",
        "}\n",
        "function f(Client $c): string { return $c->cache('k'); }\n",
    );
    let s = summary(src, "f");
    assert_eq!(s.declared, vec!["io.fs"], "the nearer tag wins outright");
    assert!(!s.exhaustive);
}

// ---- The empty bound ---------------------------------------------------------

#[test]
fn a_bare_pure_tag_is_the_empty_bound_and_still_taints() {
    // `@phpstan-pure` is a real claim (the empty envelope), not a missing one —
    // but it is still a claim nobody checked here, so the taint stands and the
    // declared lane gains nothing to show for it.
    let src = concat!(
        "<?php\n",
        "interface Calc {\n",
        "    /** @phpstan-pure */\n",
        "    public function add(int $a, int $b): int;\n",
        "}\n",
        "function f(Calc $c): int { return $c->add(1, 2); }\n",
    );
    let s = summary(src, "f");
    assert!(s.labels.is_empty());
    assert!(s.declared.is_empty(), "pure declares no label, got: {:?}", s.declared);
    assert!(!s.exhaustive);
}

// ---- `all-methods-pure`'s void quirk ----------------------------------------

/// A two-level hierarchy that makes the void quirk *observable*: `Base` bounds
/// both members at `io.db` with method-level tags, and `Child` claims
/// `all-methods-pure` over its own redeclarations. Wherever the class tag covers a
/// method, the walk stops at `Child` with the empty bound; wherever it does not,
/// the walk falls through to `Base`'s `io.db`. Without the second level the two
/// outcomes would render identically (`{…?}`).
const VOID_QUIRK: &str = concat!(
    "<?php\n",
    "interface Base {\n",
    "    /** @phpstan-impure io.db */\n",
    "    public function log(string $m): void;\n",
    "    /** @phpstan-impure io.db */\n",
    "    public function get(string $k): string;\n",
    "    /** @phpstan-impure io.db */\n",
    "    public function __construct();\n",
    "}\n",
    "/** @phpstan-all-methods-pure */\n",
    "interface Child extends Base {\n",
    "    public function log(string $m): void;\n",
    "    public function get(string $k): string;\n",
    // PHP forbids a native return hint on a constructor, so the only way to make
    // it *look* void to the return-type test — and thereby pin the constructor
    // carve-out rather than the void test's fallback — is the docblock.
    "    /** @return void */\n",
    "    public function __construct();\n",
    "}\n",
);

#[test]
fn all_methods_pure_does_not_cover_a_void_returning_method() {
    let src = format!("{VOID_QUIRK}function f(Child $c): void {{ $c->log('x'); }}\n");
    let s = summary(&src, "f");
    assert_eq!(
        s.declared,
        vec!["io.db"],
        "the class tag skipped the void method, so Base's bound is the nearest one"
    );
    assert!(!s.exhaustive);
}

#[test]
fn all_methods_pure_covers_a_non_void_returning_method() {
    let src = format!("{VOID_QUIRK}function f(Child $c): string {{ return $c->get('k'); }}\n");
    let s = summary(&src, "f");
    assert!(s.declared.is_empty(), "Child's empty bound stopped the walk, got: {:?}", s.declared);
    assert!(!s.exhaustive);
}

#[test]
fn all_methods_pure_covers_the_constructor_even_though_it_returns_nothing() {
    // Upstream includes the constructor (its fixtures bless a property-initializing
    // pure constructor), so the void test must not swallow it.
    let src = format!("{VOID_QUIRK}function f(Child $c): void {{ $c->__construct(); }}\n");
    let s = summary(&src, "f");
    assert!(s.declared.is_empty(), "the constructor IS covered, got: {:?}", s.declared);
    assert!(!s.exhaustive);
}

// ---- Stratum precedence ------------------------------------------------------

#[test]
fn an_attribute_envelope_beats_an_interop_tag_on_the_same_declaration() {
    // Checked beats unchecked (ADR-0082 §1): the attribute's labels, and the
    // attribute's taint discharge.
    let src = concat!(
        "<?php\n",
        "interface Repo {\n",
        "    /** @phpstan-impure io.net */\n",
        "    #[\\Steins\\Effect('io.db')]\n",
        "    public function find(int $id): string;\n",
        "}\n",
        "function f(Repo $r): string { return $r->find(1); }\n",
    );
    let s = summary(src, "f");
    assert_eq!(s.declared, vec!["io.db"], "the docblock did not get a vote");
    assert!(s.exhaustive);
}

// ---- Rendering rides the existing declared lane, unchanged -------------------

#[test]
fn the_margin_renders_an_interop_bound_with_the_declared_lanes_own_prefix() {
    // No rendering code knows this bound came from a docblock: `≤` and the `…?`
    // taint marker are the declared lane's existing vocabulary (ADR-0067).
    let src = concat!(
        "<?php\n",
        "interface Repo {\n",
        "    /** @phpstan-impure io.db */\n",
        "    public function find(int $id): string;\n",
        "}\n",
        "function f(Repo $r): string { echo 'x'; return $r->find(1); }\n",
    );
    let margins = effect_margins(src);
    assert!(
        margins.contains(&"effects: {output, ≤io.db, …?}".to_owned()),
        "got: {margins:?}"
    );
}
