//! Contract checking a declaration against its **interop envelope** (ADR-0082
//! role B, issue #303): a function or method whose operative envelope is one of
//! upstream's purity tags is held to it exactly as an attribute-declared one is.
//!
//! Role A (the declared lane at *call sites*) lives in `interop_envelope_lane.rs`
//! and is the half that trusts nothing. This file is the other half: reading the
//! tag is not believing it, it is taking it as a checkable claim. So the
//! diagnostics are the existing ones — `effect.envelope-exceeded` and
//! `effect.unknown-label`, no new ids — and every judgment reads the **proven**
//! lane only (ADR-0067 §5).
//!
//! What *does* vary with the source is the wording, which is not contract
//! (ADR-0023 — the ids are): a finding quotes the declaration back in the syntax
//! its author wrote, so an interop bound is named `@phpstan-impure io.db`, never
//! the `#[\Steins\Effect('io.db')]` the reader would search their file for in
//! vain. Every message below is asserted in full for that reason.
//!
//! Two exclusions are pinned as hard as the inclusions, because they are where a
//! plausible implementation would over-reach: an attribute envelope **shadows**
//! the interop one outright (the checked stratum wins, ADR-0082 §1), and interop
//! envelopes never participate in `effect.liskov-widened` (within that stratum
//! upstream's nearest-wins override is the whole contract, ADR-0082 §5).

use steins_infer::{
    Diagnostic, EFFECT_ID, EFFECT_LISKOV_ID, UNKNOWN_LABEL_ID, check, effect_summary,
};
use steins_syntax::SourceTree;

/// Parse + check inline PHP, keeping only the diagnostics with `id`.
fn of_id(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == id).collect()
}

/// The envelope-exceeded findings, in report order.
fn exceeded(src: &str) -> Vec<Diagnostic> {
    of_id(src, EFFECT_ID)
}

fn one_exceeded(src: &str) -> Diagnostic {
    let f = exceeded(src);
    assert_eq!(f.len(), 1, "expected exactly one envelope finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

fn silent(src: &str) {
    let f = exceeded(src);
    assert!(f.is_empty(), "expected silence, got: {f:#?}");
}

// ---- THE HEADLINE: a docblock bound is a checked bound -----------------------

#[test]
fn a_labeled_impure_tag_is_exceeded_by_a_proven_label_outside_it() {
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.db */\n",
        "function refresh(): int { return time(); }\n",
    );
    let d = one_exceeded(src);
    assert_eq!(d.id, EFFECT_ID, "the existing id — an interop bound earns no id of its own");
    // The clause is the tag as written, label list in the tag's own grammar
    // (comma-space separated dot-paths, unquoted — ADR-0082 §4).
    assert_eq!(
        d.message,
        "time() has effect nondet.time, but refresh() is declared @phpstan-impure io.db — nondet.time exceeds the envelope"
    );
    assert_eq!(d.line, 3, "anchored at the offending call, as the attribute path anchors it");
}

#[test]
fn a_docblock_sourced_finding_never_names_the_attribute_syntax() {
    // The refinement in one assertion: a reader who wrote a docblock tag must not
    // be told about an attribute they never wrote and cannot find in their file.
    // Both role-B diagnostics are checked at once — the bound is exceeded *and*
    // one of its labels is unknown.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.db, io.netw */\n",
        "function refresh(): int { return time(); }\n",
    );
    let mut msgs: Vec<String> =
        of_id(src, EFFECT_ID).into_iter().chain(of_id(src, UNKNOWN_LABEL_ID)).map(|d| d.message).collect();
    msgs.sort();
    assert_eq!(
        msgs,
        vec![
            "time() has effect nondet.time, but refresh() is declared @phpstan-impure io.db, io.netw — nondet.time exceeds the envelope".to_owned(),
            "unknown effect label 'io.netw' in @phpstan-impure on refresh() — did you mean 'io.net'?".to_owned(),
        ],
        "the multi-label list is rendered in the tag's grammar, comma-space separated and unquoted"
    );
    for m in &msgs {
        assert!(!m.contains("Steins"), "no attribute syntax in a docblock-sourced finding: {m}");
    }
}

#[test]
fn a_labeled_impure_tag_admits_the_label_it_names() {
    // The control for the case above: same body, a bound that covers it.
    silent(concat!(
        "<?php\n",
        "/** @phpstan-impure nondet.time */\n",
        "function refresh(): int { return time(); }\n",
    ));
}

#[test]
fn an_interop_bound_subsumes_by_prefix() {
    // `io` admits `io.fs.read` — segment-aware prefix subsumption, the substance
    // of the upstream proposal (ADR-0082 §4).
    silent(concat!(
        "<?php\n",
        "/** @phpstan-impure io */\n",
        "function slurp(): string { return file_get_contents('/etc/hosts'); }\n",
    ));
}

#[test]
fn an_interop_bound_does_not_subsume_a_sibling() {
    // The other half of the same rule: `io.net` is not an ancestor of `io.fs.read`.
    let d = one_exceeded(concat!(
        "<?php\n",
        "/** @phpstan-impure io.net */\n",
        "function slurp(): string { return file_get_contents('/etc/hosts'); }\n",
    ));
    assert_eq!(
        d.message,
        "file_get_contents() has effect io.fs.read, but slurp() is declared @phpstan-impure io.net — io.fs.read exceeds the envelope"
    );
}

// ---- `@phpstan-pure`: the empty envelope, and its one tolerance --------------

#[test]
fn a_bare_pure_tag_is_the_empty_envelope() {
    let d = one_exceeded(concat!(
        "<?php\n",
        "/** @phpstan-pure */\n",
        "function slurp(): string { return file_get_contents('/etc/hosts'); }\n",
    ));
    assert_eq!(
        d.message,
        "file_get_contents() has effect io.fs.read, but slurp() is declared @phpstan-pure"
    );
}

#[test]
fn a_pure_tag_tolerates_a_frame_local_by_ref_write() {
    // ADR-0063 §2.3 / ADR-0082 §3: pure is the `{mutate.local}` envelope. Nothing
    // special happens here — `exceeds` tolerates the label under *every* envelope,
    // and the interop bound goes through the same `exceeds`.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-pure */\n",
        "function sorted(array $rows): array { sort($rows); return $rows; }\n",
    );
    // The silence below is a *tolerance*, not an absence: the by-ref write really
    // is proven, and the empty bound really does admit it.
    let tree = SourceTree::parse(src);
    let s = effect_summary(&tree, tree.functions(), tree.classes())
        .into_iter()
        .find(|s| s.symbol == "sorted")
        .expect("a summary for sorted");
    assert_eq!(s.labels, vec!["mutate.local"], "the frame-local write is proven");
    silent(src);
}

// ---- The proven lane, and only it --------------------------------------------

#[test]
fn a_non_exhaustive_body_still_reports_its_proven_label() {
    // An uncatalogued callee taints exhaustiveness; it cannot un-prove `time()`.
    // Non-exhaustiveness hides effects, it never invents or excuses one.
    let d = one_exceeded(concat!(
        "<?php\n",
        "/** @phpstan-pure */\n",
        "function f(): int { unknown_thing(); return time(); }\n",
    ));
    assert_eq!(d.message, "time() has effect nondet.time, but f() is declared @phpstan-pure");
}

// ---- The attribute shadows the interop envelope (ADR-0082 §1) ----------------

#[test]
fn an_attribute_envelope_shadows_the_interop_bound_for_checking() {
    // The docblock bound (empty) would flag `time()`; the attribute bound admits
    // it. The checked stratum wins, so there is no finding at all — the interop
    // envelope is not a second, stricter contract layered underneath.
    silent(concat!(
        "<?php\n",
        "/** @phpstan-pure */\n",
        "#[\\Steins\\Effect('nondet.time')]\n",
        "function f(): int { return time(); }\n",
    ));
}

#[test]
fn an_attribute_envelope_shadows_the_interop_bound_for_label_validation() {
    // Shadowing is total: an unread bound's labels are not validated either.
    // Otherwise a docblock nobody consults could still emit a diagnostic.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.netw */\n",
        "#[\\Steins\\Effect('io')]\n",
        "function f(): string { return file_get_contents('/x'); }\n",
    );
    assert!(of_id(src, UNKNOWN_LABEL_ID).is_empty(), "the shadowed bound is never label-checked");
    silent(src);
}

// ---- Class-level tags: upstream semantics, verbatim (ADR-0082 §5) ------------

#[test]
fn a_class_level_impure_bound_checks_a_method_that_says_nothing() {
    let src = concat!(
        "<?php\n",
        "/** @phpstan-all-methods-impure io.net */\n",
        "class Client {\n",
        "    public function a(): string { return file_get_contents('/x'); }\n",
        "    /** @phpstan-impure io.fs */\n",
        "    public function b(): string { return file_get_contents('/x'); }\n",
        "}\n",
    );
    let f = exceeded(src);
    assert_eq!(f.len(), 1, "only the method with no tag of its own is bounded by the class, got: {f:#?}");
    assert_eq!(
        f[0].message,
        "file_get_contents() has effect io.fs.read, but Client::a() is declared @phpstan-all-methods-impure io.net — io.fs.read exceeds the envelope",
        "and `b`'s own `io.fs` tag REPLACES the class bound (nearest-wins, not conjunction)"
    );
}

#[test]
fn all_methods_pure_skips_a_void_method_and_checks_the_rest() {
    let src = concat!(
        "<?php\n",
        "/** @phpstan-all-methods-pure */\n",
        "class C {\n",
        "    public function log(): void { error_log('x'); }\n",
        "    public function read(): string { return file_get_contents('/x'); }\n",
        "}\n",
    );
    let f = exceeded(src);
    assert_eq!(f.len(), 1, "the void method is not covered — upstream's quirk, got: {f:#?}");
    assert_eq!(
        f[0].message,
        "file_get_contents() has effect io.fs.read, but C::read() is declared @phpstan-all-methods-pure"
    );
}

#[test]
fn a_bare_all_methods_impure_tag_constrains_nothing() {
    // The ⊤ bound (ADR-0082 §3). Its label list is empty, but an empty list means
    // *pure* everywhere else in this pass, so reading it as a bound would turn
    // upstream's widest claim into its narrowest and flag every method in the
    // class. It builds no bound at all.
    silent(concat!(
        "<?php\n",
        "/** @phpstan-all-methods-impure */\n",
        "class C {\n",
        "    public function read(): string { return file_get_contents('/x'); }\n",
        "}\n",
    ));
}

// ---- Unknown labels: the same loop, the same suggestion ----------------------

#[test]
fn an_unknown_label_in_an_interop_bound_is_reported_with_its_suggestion() {
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.netw */\n",
        "function f(): void {}\n",
    );
    let f = of_id(src, UNKNOWN_LABEL_ID);
    assert_eq!(f.len(), 1, "the registry is source-agnostic, got: {f:#?}");
    assert_eq!(
        f[0].message,
        "unknown effect label 'io.netw' in @phpstan-impure on f() — did you mean 'io.net'?"
    );
    // An interop envelope has no attribute span to point at, so the finding
    // anchors on the declaration's own name — where declaration-level effect
    // findings already land.
    assert_eq!(f[0].line, 3, "anchored at the declaration");
}

#[test]
fn the_attribute_spelling_still_names_the_attribute_in_its_unknown_label() {
    let f = of_id(
        "<?php\n#[\\Steins\\Effect('io.netw')]\nfunction f(): void {}\n",
        UNKNOWN_LABEL_ID,
    );
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].message,
        "unknown effect label 'io.netw' in #[\\Steins\\Effect] on f() — did you mean 'io.net'?"
    );
}

// ---- No Liskov participation (ADR-0082 §5) -----------------------------------

#[test]
fn an_interop_envelope_on_an_abstraction_never_widens_liskov() {
    // The implementation's proven `io.fs.read` blows through the interface's
    // docblock claim of purity — and stays silent. Within the interop stratum
    // there is no conjunction rule to violate: upstream's tags do not propagate
    // from an interface to its implementations at all.
    let src = concat!(
        "<?php\n",
        "interface Reader {\n",
        "    /** @phpstan-pure */\n",
        "    public function read(): string;\n",
        "}\n",
        "final class FileReader implements Reader {\n",
        "    public function read(): string { return file_get_contents('/x'); }\n",
        "}\n",
    );
    assert!(of_id(src, EFFECT_LISKOV_ID).is_empty(), "no Liskov judgment from an interop bound");
    silent(src);
}

#[test]
fn the_attribute_spelling_of_the_same_abstraction_does_widen_liskov() {
    // The contrast control: byte-identical but for the spelling. This asymmetry
    // IS the stratification, and it is the reason the exclusion above is a
    // decision rather than an oversight.
    let src = concat!(
        "<?php\n",
        "interface Reader {\n",
        "    #[\\Steins\\Pure]\n",
        "    public function read(): string;\n",
        "}\n",
        "final class FileReader implements Reader {\n",
        "    public function read(): string { return file_get_contents('/x'); }\n",
        "}\n",
    );
    let f = of_id(src, EFFECT_LISKOV_ID);
    assert_eq!(f.len(), 1, "the checked stratum conjoins across the hierarchy, got: {f:#?}");
    assert_eq!(
        f[0].message,
        "FileReader::read() has proven effect io.fs.read but Reader::read() (its abstraction) is declared #[\\Steins\\Pure] — Liskov effect widening"
    );
}
