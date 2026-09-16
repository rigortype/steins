//! Interop envelopes in the **declared lane** (ADR-0082 role A, issue #303): a call
//! through an abstraction carrying an upstream purity tag — `@phpstan-impure io.db`,
//! `@phpstan-pure`, `@phpstan-all-methods-impure` — contributes that bound to the
//! caller's declared lane.
//!
//! The point is the **stratum asymmetry**: a checked `#[\Steins\Effect]` envelope
//! answers its call site (bound imported, taint discharged); an unverified interop
//! docblock follows ADR-0068's plugin discipline instead (bound imported, taint kept).
//!
//! Contract-checking the *declaring* function against its own envelope (role B,
//! `effect.envelope-exceeded`) is a later slice; nothing here emits a diagnostic.

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

// THE HEADLINE: the same shape, two strata, two answers

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
    // Contrast control, byte-identical but for spelling — this IS the trust
    // stratification (ADR-0082 §1).
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

// Class-level tags (ADR-0082 §5, upstream semantics verbatim)

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
    // Upstream's nearest-wins rule: the interop stratum parts ways with the checked
    // stratum's Liskov conjunction — `io.net` from the class tag is GONE, not conjoined.
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

// The ADR-0083 vocabulary migration

/// A docblock spelling the retired `output` root is *unreadable*, not wrong: one
/// unknown label unspecifies the whole tag (ADR-0082), so nothing arrives and no
/// finding fires — unlike the attribute spelling, which earns `effect.unknown-label`.
#[test]
fn a_retired_output_bound_in_an_interop_tag_is_inert() {
    let src = concat!(
        "<?php\n",
        "interface Printer {\n",
        "    /** @phpstan-impure output */\n",
        "    public function emit(string $s): void;\n",
        "}\n",
        "function f(Printer $p): void { $p->emit(\"hi\"); }\n",
    );
    let s = summary(src, "f");
    assert!(s.labels.is_empty(), "nothing proven, got: {:?}", s.labels);
    assert!(s.declared.is_empty(), "the unreadable tag contributes no bound: {:?}", s.declared);
    assert!(!s.exhaustive, "and the call is still an unresolved claim");
}

/// The migrated spelling of the same tag does arrive.
#[test]
fn the_migrated_output_bound_in_an_interop_tag_is_read() {
    let src = concat!(
        "<?php\n",
        "interface Printer {\n",
        "    /** @phpstan-impure io.output.buffer */\n",
        "    public function emit(string $s): void;\n",
        "}\n",
        "function f(Printer $p): void { $p->emit(\"hi\"); }\n",
    );
    let s = summary(src, "f");
    assert_eq!(s.declared, vec!["io.output.buffer"]);
    assert!(!s.exhaustive);
}

// The empty bound

#[test]
fn a_bare_pure_tag_is_the_empty_bound_and_still_taints() {
    // `@phpstan-pure` is a real claim (the empty envelope), not a missing one — but
    // nobody checked it here, so the taint stands and the declared lane gains nothing.
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

// `all-methods-pure`'s void quirk

/// Two-level hierarchy making the void quirk *observable*: `Base` bounds both
/// members at `io.db`; `Child` claims `all-methods-pure` over its redeclarations.
/// Where the class tag covers a method, the walk stops at `Child`'s empty bound;
/// otherwise it falls to `Base`'s `io.db` — else both outcomes render identically.
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
    // PHP forbids a native return hint on a constructor, so the docblock `@return
    // void` is the only way to make it *look* void and pin the constructor carve-out.
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

// Stratum precedence

#[test]
fn an_attribute_envelope_beats_an_interop_tag_on_the_same_declaration() {
    // Checked beats unchecked (ADR-0082 §1): the attribute's labels and taint discharge win.
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

// Rendering rides the existing declared lane, unchanged

#[test]
fn the_margin_renders_an_interop_bound_with_the_declared_lanes_own_prefix() {
    // No rendering code knows this bound came from a docblock: `≤` and `…?` are
    // the declared lane's existing vocabulary (ADR-0067).
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
        margins.contains(&"effects: {io.output.buffer, ≤io.db, …?}".to_owned()),
        "got: {margins:?}"
    );
}
