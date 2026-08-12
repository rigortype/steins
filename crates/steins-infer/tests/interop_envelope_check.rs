//! Contract checking a declaration against its **interop envelope** (ADR-0082
//! role B, issue #303): a function or method whose operative envelope is one of
//! upstream's purity tags is held to it exactly as an attribute-declared one is.
//!
//! Role A (the declared lane at *call sites*) lives in `interop_envelope_lane.rs`
//! and is the half that trusts nothing. This file is the other half: reading the
//! tag is not believing it, it is taking it as a checkable claim. So the
//! diagnostic is the existing `effect.envelope-exceeded`, no new id, and every
//! judgment reads the **proven** lane only (ADR-0067 §5).
//!
//! What *does* vary with the source is the wording, which is not contract
//! (ADR-0023 — the ids are): a finding quotes the declaration back in the syntax
//! its author wrote, so an interop bound is named `@phpstan-impure io.db`, never
//! the `#[\Steins\Effect('io.db')]` the reader would search their file for in
//! vain. Every message below is asserted in full for that reason.
//!
//! Three exclusions are pinned as hard as the inclusions, because they are where a
//! plausible implementation would over-reach: an attribute envelope **shadows**
//! the interop one outright (the checked stratum wins, ADR-0082 §1); interop
//! envelopes never participate in `effect.liskov-widened` (within that stratum
//! upstream's nearest-wins override is the whole contract, ADR-0082 §5); and a tag
//! naming any label the registry does not know is **inert** rather than
//! diagnosed or narrowed (owner ruling 2026-08-12), because upstream discards the
//! text after `@phpstan-impure` and wild docblocks put prose there.

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
    // A reader who wrote a docblock tag must not be told about an attribute they
    // never wrote and cannot find in their file.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.db, nondet.random */\n",
        "function refresh(): int { return time(); }\n",
    );
    let msgs: Vec<String> = of_id(src, EFFECT_ID).into_iter().map(|d| d.message).collect();
    assert_eq!(
        msgs,
        vec![
            "time() has effect nondet.time, but refresh() is declared @phpstan-impure io.db, nondet.random — nondet.time exceeds the envelope".to_owned(),
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

/// Issue #318, in the stratum the upper-bound contract is *written* for: the
/// review's finding was that `@phpstan-impure io.fs.read` admitted a network
/// read, because the catalog row was `io.fs.read` whatever the argument said.
/// Both halves are asserted here — the call site that proves a URL, and the one
/// that proves a local path and must stay as silent as it always was.
#[test]
fn an_interop_bound_is_exceeded_by_a_proven_wrapper_target() {
    let d = one_exceeded(concat!(
        "<?php\n",
        "/** @phpstan-impure io.fs.read */\n",
        "function fetch(): string { return file_get_contents('https://example.com/rates'); }\n",
    ));
    assert_eq!(
        d.message,
        "file_get_contents() has effect io.net.http, but fetch() is declared @phpstan-impure io.fs.read — io.net.http exceeds the envelope"
    );
    silent(concat!(
        "<?php\n",
        "/** @phpstan-impure io.fs.read */\n",
        "function load(): string { return file_get_contents('/etc/hosts'); }\n",
    ));
}

/// The same bound over a call whose target is *not* provable. This is the
/// honest cost of the fix, and the reason it is a behavior change rather than a
/// pure win: an envelope written against the old precise row now reports.
#[test]
fn an_interop_bound_is_exceeded_by_an_unprovable_stream_resource() {
    let d = one_exceeded(concat!(
        "<?php\n",
        "/** @phpstan-impure io.fs.read */\n",
        "function pull($handle): string { return fread($handle, 8); }\n",
    ));
    assert_eq!(
        d.message,
        "fread() has effect io, but pull() is declared @phpstan-impure io.fs.read — io exceeds the envelope"
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
fn a_docblock_tag_changes_nothing_about_the_attribute_path() {
    // The attribute here is *wrong* (`io.netw` is no label) and the docblock bound
    // beside it would have been satisfied by this body. Both findings still come
    // out of the attribute, in the attribute's wording: the checked stratum is
    // judged, quoted and typo-checked exactly as it is with no docblock present.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io */\n",
        "#[\\Steins\\Effect('io.netw')]\n",
        "function f(): string { return file_get_contents('/x'); }\n",
    );
    let unknown = of_id(src, UNKNOWN_LABEL_ID);
    assert_eq!(unknown.len(), 1, "the attribute's typo diagnostic is untouched, got: {unknown:#?}");
    assert_eq!(
        unknown[0].message,
        "unknown effect label 'io.netw' in #[\\Steins\\Effect] on f() — did you mean 'io.net'?"
    );
    assert_eq!(
        one_exceeded(src).message,
        "file_get_contents() has effect io.fs.read, but f() is declared #[\\Steins\\Effect('io.netw')] — io.fs.read exceeds the envelope",
        "the docblock's `io` would have admitted this — the attribute won"
    );
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

// ---- Unknown labels make the tag inert (owner ruling, 2026-08-12) ------------
//
// Current PHPStan discards everything after `@phpstan-impure`, so wild code
// legitimately carries one-word prose there. An unrecognized label is therefore
// read as *unspecified*, and the whole tag with it — never as a diagnostic, and
// never as the bound its known labels alone would spell.

#[test]
fn an_unknown_label_makes_the_whole_tag_inert() {
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure io.netw */\n",
        "function f(): int { return time(); }\n",
    );
    assert!(of_id(src, UNKNOWN_LABEL_ID).is_empty(), "typo reporting is not this rule's job");
    silent(src);
    // And nothing reaches the declared lane either (role A's half of the ruling):
    // no bound imported, and the taint an unchecked claim never discharges stays.
    let iface = concat!(
        "<?php\n",
        "interface Repo {\n",
        "    /** @phpstan-impure io.netw */\n",
        "    public function find(int $id): string;\n",
        "}\n",
        "function g(Repo $r): string { return $r->find(1); }\n",
    );
    let tree = SourceTree::parse(iface);
    let s = effect_summary(&tree, tree.functions(), tree.classes())
        .into_iter()
        .find(|s| s.symbol == "g")
        .expect("a summary for g");
    assert!(s.declared.is_empty(), "an inert tag imports no bound, got: {:?}", s.declared);
    assert!(!s.exhaustive, "and the call site keeps its taint");
}

#[test]
fn a_one_word_description_after_impure_is_harmless() {
    // The motivating shape: `@phpstan-impure database` is prose under current
    // PHPStan, which reads the tag name and throws the rest away. It must not
    // fail a run on any surface.
    let src = concat!(
        "<?php\n",
        "/** @phpstan-impure database */\n",
        "function save(string $row): void { file_put_contents('/x', $row); echo 'ok'; }\n",
    );
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let all: Vec<Diagnostic> = check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == EFFECT_ID || d.id == EFFECT_LISKOV_ID || d.id == UNKNOWN_LABEL_ID)
        .collect();
    assert!(all.is_empty(), "a one-word description is not a bound and not a typo: {all:#?}");
}

#[test]
fn a_typoed_label_disables_the_whole_bound_not_part_of_it() {
    // `io.db, io.netw` is NOT a claim of `io.db`. Checking the body against the
    // known subset would hold the author to a narrower bound than they wrote and
    // manufacture the finding below; the tag goes ⊤ instead.
    silent(concat!(
        "<?php\n",
        "/** @phpstan-impure io.db, io.netw */\n",
        "function refresh(): int { return time(); }\n",
    ));
}

#[test]
fn an_inert_method_tag_does_not_fall_back_to_the_class_tag() {
    // "As if nothing was written" stops at the bound, not at the precedence: the
    // method-level tag still WON (upstream's nearest-wins, ADR-0082 §5), so the
    // class's claim does not get to speak for a method whose author explicitly
    // declared it impure.
    silent(concat!(
        "<?php\n",
        "/** @phpstan-all-methods-pure */\n",
        "class C {\n",
        "    /** @phpstan-impure database */\n",
        "    public function save(): void { file_put_contents('/x', 'y'); }\n",
        "}\n",
    ));
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
