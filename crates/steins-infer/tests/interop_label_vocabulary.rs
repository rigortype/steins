//! The interop stratum's **vocabulary** check (`effect.interop-unknown-label`, issue
//! #311): tells a typo'd effect label from a human's prose, where PHPStan reads neither.
//!
//! ADR-0082 settled the *bound*: an unrecognized label makes the whole tag unspecified
//! (matching upstream), so a genuine typo silently checks nothing. This id is the
//! interop spec's fail-open visibility for that cost — suppressible, unlike the
//! attribute stratum's `effect.unknown-label`.
//!
//! The hard half is what stays silent, so that is what most of this file pins: only a
//! near miss, a recognized sibling label, a dot-path shape, or a retired spelling
//! reports — never a lone far-off word.

use std::collections::BTreeMap;

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    Diagnostic, EFFECT_ID, Floor, INTEROP_UNKNOWN_LABEL_ID, Layer, UNKNOWN_LABEL_ID, check, layer,
    surface_floor,
};
use steins_syntax::SourceTree;

/// Parse + check inline PHP, keeping only the diagnostics with `id`.
fn of_id(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == id).collect()
}

/// The one vocabulary finding `src` earns, or a failure naming what came instead.
fn one_vocabulary(src: &str) -> Diagnostic {
    let f = of_id(src, INTEROP_UNKNOWN_LABEL_ID);
    assert_eq!(f.len(), 1, "expected exactly one vocabulary finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

/// Whether the built-in profile `name` shows `d`.
fn surfaced(name: &str, d: &Diagnostic) -> bool {
    ProfileConfigs(BTreeMap::new()).resolve(Some(name)).unwrap().is_surfaced(d)
}

// THE GUARANTEE: prose is never read as a fumbled label.

#[test]
fn a_one_word_note_is_silent_on_every_profile_forever() {
    // The motivating shape: `database` is far from every label, has no dot, is
    // alone in its list, and was never retired — nothing says "label" over "note".
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure database */\n",
        "function save(string $row): void { file_put_contents('/x', $row); }\n",
    );
    let tree = SourceTree::parse(src);
    let all = check(&tree, tree.functions(), "test.php");
    let effectful: Vec<&Diagnostic> = all.iter().filter(|d| d.id.starts_with("effect.")).collect();
    assert!(effectful.is_empty(), "prose earns no effect finding at all: {effectful:#?}");
    // Off every built-in surface — no profile a user can opt into turns this into a finding.
    for name in ["default", "contracts", "strict", "pedantic"] {
        let shown: Vec<&Diagnostic> =
            all.iter().filter(|d| surfaced(name, d) && d.id == INTEROP_UNKNOWN_LABEL_ID).collect();
        assert!(shown.is_empty(), "`{name}` must stay silent on prose: {shown:#?}");
    }
}

// Signal (a): near a known label.

#[test]
fn a_near_miss_reports_with_the_registry_suggestion() {
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.netw */\n",
        "function fetch(): string { return file_get_contents('/x'); }\n",
    );
    let d = one_vocabulary(src);
    assert_eq!(
        d.message,
        "unknown effect label 'io.netw' in @phpstan-impure on fetch() — the whole tag reads as \
         unspecified and bounds nothing; did you mean 'io.net'?"
    );
    assert_eq!(d.line, 3, "anchored at the declaration, like the attribute-side finding");
}

#[test]
fn the_near_miss_is_off_the_default_surface_and_on_at_contracts() {
    // A bare `steins check` cannot start failing over pre-existing docblocks; enabling
    // envelope enforcement is what turns on the check that keeps enforcement honest.
    let d = one_vocabulary(concat!(
        "<?php\n",
        "/** @phpstan-impure io.netw */\n",
        "function fetch(): string { return file_get_contents('/x'); }\n",
    ));
    assert!(!surfaced("default", &d), "a bare check stays quiet");
    assert!(surfaced("contracts", &d), "it rides with the other envelope findings");
    assert!(surfaced("strict", &d), "and the ladder is cumulative");
}

#[test]
fn nondet_tyme_reports_through_the_near_miss_signal_not_the_shape_one() {
    // Pins WHICH signal fires: `tyme` → `time` is one edit, so signal (a) wins over
    // the dot-path shape (signal (c)) — evidence is weighed in order.
    let d = one_vocabulary(concat!(
        "<?php\n",
        "/** @phpstan-impure nondet.tyme */\n",
        "function now(): int { return time(); }\n",
    ));
    assert_eq!(
        d.message,
        "unknown effect label 'nondet.tyme' in @phpstan-impure on now() — the whole tag reads as \
         unspecified and bounds nothing; did you mean 'nondet.time'?"
    );
}

// Signal (b): a recognized sibling in the same list.

#[test]
fn a_recognized_sibling_makes_even_a_prose_shaped_token_report() {
    // The deliberately aggressive edge: `database` is the very token the guarantee
    // above protects, but here it reports because `io.db` sits beside it in the list.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.db, database */\n",
        "function save(string $row): void { file_put_contents('/x', $row); }\n",
    );
    let d = one_vocabulary(src);
    assert_eq!(
        d.message,
        "unknown effect label 'database' in @phpstan-impure on save() — the whole tag reads as \
         unspecified and bounds nothing"
    );
    // No suggestion: nothing in the vocabulary is close enough to name.
    assert!(!d.message.contains("did you mean"));
    // The recognized member is not itself reported.
    assert_eq!(of_id(src, INTEROP_UNKNOWN_LABEL_ID).len(), 1);
    // And the bound is still ⊤: the hygiene rule has no vote on the reading.
    assert!(of_id(src, EFFECT_ID).is_empty(), "an inert tag checks nothing, typo or not");
}

// Signal (c): dot-path shape.

#[test]
fn a_dot_path_reports_though_nothing_is_near_it() {
    // `cache.warmup` is far from every entry and alone in its list — only its shape speaks.
    let d = one_vocabulary(concat!(
        "<?php\n",
        "/** @phpstan-impure cache.warmup */\n",
        "function warm(): void { file_put_contents('/x', 'y'); }\n",
    ));
    assert_eq!(
        d.message,
        "unknown effect label 'cache.warmup' in @phpstan-impure on warm() — the whole tag reads \
         as unspecified and bounds nothing"
    );
}

// Signal (d): a spelling this project retired.

#[test]
fn the_retired_output_root_gets_the_d_v2_migration_and_no_phantom_bound() {
    // `output` → `io.output` is distance 3, past the suggestion cap, hence the
    // retirement table; the tag stays inert regardless (no resurrected bound).
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure output */\n",
        "function render(): void { echo 'hi'; header('X: 1'); }\n",
    );
    let d = one_vocabulary(src);
    assert_eq!(
        d.message,
        "unknown effect label 'output' in @phpstan-impure on render() — the whole tag reads as \
         unspecified and bounds nothing; 'output' was retired, so write io.output.buffer for \
         echo-shaped code, io.output.header for header()/setcookie(), or the umbrella io.output"
    );
    assert!(
        of_id(src, EFFECT_ID).is_empty(),
        "no envelope-exceeded from a phantom bound — the tag reads as ⊤, not as `output`"
    );
}

#[test]
fn the_exactly_replaceable_retirement_names_its_one_replacement() {
    let d = one_vocabulary(concat!(
        "<?php\n",
        "/** @phpstan-impure output.header */\n",
        "function send(): void { header('X: 1'); }\n",
    ));
    assert_eq!(
        d.message,
        "unknown effect label 'output.header' in @phpstan-impure on send() — the whole tag reads \
         as unspecified and bounds nothing; 'output.header' was retired, so write io.output.header"
    );
}

// Class-level tags.

#[test]
fn a_class_level_tag_is_reported_once_on_the_class() {
    // One declaration, one finding — not one per method it would have covered.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-all-methods-impure io.netw */\n",
        "class Client {\n",
        "    public function fetch(): string { return file_get_contents('/x'); }\n",
        "    public function post(): string { return file_get_contents('/y'); }\n",
        "}\n",
    );
    let d = one_vocabulary(src);
    assert_eq!(
        d.message,
        "unknown effect label 'io.netw' in @phpstan-all-methods-impure on class Client — the \
         whole tag reads as unspecified and bounds nothing; did you mean 'io.net'?"
    );
    assert_eq!(d.line, 3, "anchored at the class name");
}

#[test]
fn a_method_tag_is_reported_at_the_method() {
    let d = one_vocabulary(concat!(
        "<?php\n",
        "class Client {\n",
        "    /** @phpstan-impure io.netw */\n",
        "    public function fetch(): string { return file_get_contents('/x'); }\n",
        "}\n",
    ));
    assert_eq!(
        d.message,
        "unknown effect label 'io.netw' in @phpstan-impure on Client::fetch() — the whole tag \
         reads as unspecified and bounds nothing; did you mean 'io.net'?"
    );
    assert_eq!(d.line, 4);
}

// Shadowing: a docblock nobody read cannot mislead anybody.

#[test]
fn an_attribute_envelope_shadows_the_docblocks_vocabulary_too() {
    // ADR-0082 §1's shadowing is total: with an attribute present the docblock is
    // never consulted for the bound, so reporting its spelling would report a line
    // the analyzer deliberately did not read.
    let src = concat!(
        "<?php\n",
        "class Client {\n",
        "    /** @phpstan-impure io.netw */\n",
        "    #[\\Steins\\Effect('io')]\n",
        "    public function fetch(): string { return file_get_contents('/x'); }\n",
        "}\n",
    );
    let f = of_id(src, INTEROP_UNKNOWN_LABEL_ID);
    assert!(f.is_empty(), "the shadowed docblock is not vocabulary-checked: {f:#?}");
    // And the attribute stratum is unmoved — its label is known, so nothing at all.
    assert!(of_id(src, UNKNOWN_LABEL_ID).is_empty());
}

// The attribute stratum keeps its own id, layer, and floor.

#[test]
fn the_attribute_spelling_still_earns_its_own_mechanics_id() {
    let src = "<?php\n#[\\Steins\\Effect('io.netw')]\nfunction f(): void {}\n";
    let f = of_id(src, UNKNOWN_LABEL_ID);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].message,
        "unknown effect label 'io.netw' in #[\\Steins\\Effect] on f() — did you mean 'io.net'?",
        "the near-miss wording of the attribute path is untouched"
    );
    assert!(of_id(src, INTEROP_UNKNOWN_LABEL_ID).is_empty(), "one stratum, one id");
    // The two ids are different claims on different surfaces, and stay so.
    assert_eq!(layer(UNKNOWN_LABEL_ID), Some(Layer::Mechanics));
    assert_eq!(surface_floor(UNKNOWN_LABEL_ID), Some(Floor::Default));
    assert_eq!(layer(INTEROP_UNKNOWN_LABEL_ID), Some(Layer::Contract));
    assert_eq!(surface_floor(INTEROP_UNKNOWN_LABEL_ID), Some(Floor::Contracts));
}

#[test]
fn the_attribute_spelling_of_a_retired_label_now_says_what_to_write() {
    // Issue #311's one attribute-side change: before the table this stuck at 'output',
    // since edit-distance can't reach an ADR-0083 rename. Same id, layer, floor.
    let src = "<?php\n#[\\Steins\\Effect('output')]\nfunction render(): void { echo 'hi'; }\n";
    let f = of_id(src, UNKNOWN_LABEL_ID);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].message,
        "unknown effect label 'output' in #[\\Steins\\Effect] on render() — 'output' was retired, \
         so write io.output.buffer for echo-shaped code, io.output.header for \
         header()/setcookie(), or the umbrella io.output"
    );
    assert_eq!(f[0].id, UNKNOWN_LABEL_ID, "still the mechanics id it always was");
}

// Suppression: a contract-layer id a migration can absorb.

#[test]
fn the_id_is_suppressible_by_name() {
    // Why the layer matters: a mid-migration codebase may carry many of these, and
    // the contract layer is what lets a baseline or inline ignore absorb them.
    use steins_infer::apply_inline_ignores;
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.netw */\n",
        "function fetch(): string { return file_get_contents('/x'); } ",
        "// @steins-ignore effect.interop-unknown-label\n",
    );
    let tree = SourceTree::parse(src);
    let raw = check(&tree, tree.functions(), "test.php");
    assert_eq!(raw.iter().filter(|d| d.id == INTEROP_UNKNOWN_LABEL_ID).count(), 1);
    let outcome = apply_inline_ignores(raw, &[("test.php".to_owned(), &tree)]);
    assert_eq!(
        outcome.kept.iter().filter(|d| d.id == INTEROP_UNKNOWN_LABEL_ID).count(),
        0,
        "the registry-governed inline ignore channel reaches this id"
    );
    assert_eq!(outcome.suppressed, 1);
}
