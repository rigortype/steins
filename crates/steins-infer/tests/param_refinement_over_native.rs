//! A `@param` that NARROWS a native type reaches the value lane (issue #242).
//!
//! `@param non-empty-string $s` on a `string $s` is one of the commonest shapes
//! in typed PHP, and every such refinement used to be dropped. The arm lane was
//! never at fault — `subsumes(string, lowercase-string)` is `Yes`, so ADR-0052
//! §9 kept the arm and the declared-side surface rendered it all along. The loss
//! was one lane over: the native-type parameter pass seeds `Fact::General` from
//! the native type BEFORE the arm lane is built, and the value lane outranks the
//! arm lane at every fact read, so the coarse `string` shadowed the refinement
//! beside it.
//!
//! The array vocabulary was exempt for a mechanical reason, not a principled
//! one: the native pass seeds a lone native *scalar* member only, so a native
//! `array $a` left the value lane free for the shape seed to fill from the same
//! arms — that asymmetry localized the defect.
//!
//! Two properties are load-bearing and each is pinned below:
//!
//! * the subset filter stays ON — a narrowing the native type does not cover is
//!   still dropped, in both directions;
//! * the refinement is `Asserted` (a docblock claim): it may premise a
//!   contract-layer finding but never a proof-layer one (ADR-0037, ADR-0052 N2).
//!
//! Issue #240 added the **declared conjunction** to the same seam: `A&B` over
//! string refinements folds to the one closed `StrPreds` set it denotes, so it
//! narrows through this lane exactly as a single keyword does.

use steins_infer::{DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, Layer, check, layer};
use steins_syntax::SourceTree;

/// The single `debug.type` message body of a one-dump source.
fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// The single `debug.phpdoc-type` message body of a one-dump source.
fn one_phpdoc(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    let pd: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(pd.len(), 1, "expected exactly one debug.phpdoc-type dump, got {ds:?}");
    pd[0].message.clone()
}

/// `@param {declared} $v` over the native declaration `{native} $v`, dumped.
fn param_dump(declared: &str, native: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {declared} $v */\nfunction f({native}$v): void {{ \\PHPStan\\dumpType($v); }}\n"
    ))
}

// ---- The issue's measured witness table ------------------------------------

#[test]
fn a_string_predicate_narrows_a_native_string() {
    assert_eq!(
        param_dump("lowercase-string", "string "),
        "dumped type: lowercase-string (asserted)"
    );
    assert_eq!(
        param_dump("non-empty-string", "string "),
        "dumped type: non-empty-string (asserted)"
    );
    // An intersection narrows just as one does — reach half of the #235 probe,
    // where a declared conjunction over a native `string` was dropped twice over.
    assert_eq!(
        param_dump("non-empty-lowercase-string", "string "),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
}

// ---- The declared conjunction (issue #240, piece 2) ------------------------

#[test]
fn a_declared_conjunction_seeds_the_single_set_it_denotes() {
    // `A&B` over string refinements isn't an intersection to *represent*:
    // `StrPreds` is already a conjunction, so the two arms fold to one closed set
    // (`steins_contract::inter_str_preds`). Before #240 the fold didn't exist and
    // lowering returned `None`, so a declared conjunction seeded nothing at all.
    assert_eq!(
        param_dump("lowercase-string&non-empty-string", "string "),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    assert_eq!(
        param_dump("lowercase-string&non-falsy-string", "string "),
        "dumped type: non-falsy-lowercase-string (asserted)"
    );
    // Three arms, and the closure inside the fold: `numeric` entails `non-empty`.
    assert_eq!(
        param_dump("numeric-string&uppercase-string", "string "),
        "dumped type: numeric-uppercase-string (asserted)"
    );
    // The set PHPStan spells with two casing arms is one word here (ADR-0030).
    assert_eq!(
        param_dump("lowercase-string&uppercase-string", "string "),
        "dumped type: uncased-string (asserted)"
    );
}

#[test]
fn an_untyped_parameter_takes_the_conjunction_through_the_arm_lane() {
    // No native type means no value-lane seed to replace — the arm lane answers,
    // through the same fold and the same speller, so the two paths agree.
    assert_eq!(
        param_dump("lowercase-string&non-empty-string", ""),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    assert_eq!(
        param_dump("non-falsy-string&numeric-string", ""),
        "dumped type: non-falsy-numeric-string (asserted)"
    );
}

#[test]
fn a_conjunction_the_fold_refuses_keeps_the_honest_floor() {
    // `literal-string` is provenance (ADR-0038), not a predicate set, so the
    // intersection folds to nothing — arm lane keeps and judges it, value lane
    // stays as it was. Same for an object intersection (issue #234, untouched here).
    assert_eq!(param_dump("literal-string&non-falsy-string", "string "), "dumped type: string");
    assert_eq!(param_dump("literal-string&non-falsy-string", ""), "dumped type: unknown");
    assert_eq!(param_dump("Countable&Traversable", ""), "dumped type: unknown");
}

#[test]
fn a_class_string_conjunction_folds_and_keeps_its_contextual_reading() {
    // The bit is CONTEXTUAL (issue #236, decided against the class table, never
    // `StrPreds::of`) — it folds like any other bit and outranks the grid at the speller.
    assert_eq!(
        param_dump("class-string&non-empty-string", "string "),
        "dumped type: class-string (asserted)"
    );
}

#[test]
fn an_int_range_narrows_a_native_int() {
    // The int-range side of the vocabulary, not just the string predicates.
    assert_eq!(param_dump("positive-int", "int "), "dumped type: int<1, max> (asserted)");
    assert_eq!(param_dump("negative-int", "int "), "dumped type: int<min, -1> (asserted)");
    assert_eq!(param_dump("int<1, 5>", "int "), "dumped type: int<1, 5> (asserted)");
    assert_eq!(
        param_dump("non-negative-int", "int "),
        "dumped type: int<0, max> (asserted)"
    );
}

#[test]
fn the_untyped_row_is_unchanged() {
    // The control the issue measured as already correct: with no native type there
    // is no value-lane seed to shadow the arm, and the arm lane renders it.
    assert_eq!(param_dump("lowercase-string", ""), "dumped type: lowercase-string (asserted)");
    assert_eq!(param_dump("positive-int", ""), "dumped type: int<1, max> (asserted)");
}

#[test]
fn the_array_rows_keep_refining() {
    // No regression in the path that already worked — the shape seed still fills
    // the value lane from the arms, and its `(asserted)` grade is unchanged.
    assert_eq!(param_dump("array{a: int}", "array "), "dumped type: array{a: int} (asserted)");
    assert_eq!(param_dump("list<int>", "array "), "dumped type: list<int> (asserted)");
    assert_eq!(
        param_dump("non-empty-list<string>", "array "),
        "dumped type: non-empty-list<string> (asserted)"
    );
}

// ---- The filter is NOT turned off ------------------------------------------

#[test]
fn a_narrowing_the_native_type_cannot_cover_still_drops() {
    // Subset discipline (ADR-0052 §9): the docblock never widens past the
    // runtime-enforced native type. `@param int` on `string $v` is a contradiction
    // — no arm survives, so nothing narrows and the native fact stands, ungraded.
    assert_eq!(param_dump("int", "string "), "dumped type: string");
    assert_eq!(param_dump("string", "int "), "dumped type: int");
    // The declared-side surface agrees: the arm was dropped, not merely shadowed.
    let pd = "<?php\n/** @param int $v */\nfunction f(string $v): void { \\PHPStan\\dumpPhpDocType($v); }\n";
    assert_eq!(one_phpdoc(pd), "dumped phpdoc type: no declared contract");
    // A refinement of the WRONG base is a contradiction too, not a narrowing.
    assert_eq!(param_dump("positive-int", "string "), "dumped type: string");
    assert_eq!(param_dump("non-empty-string", "int "), "dumped type: int");
}

#[test]
fn a_restatement_of_the_native_type_stays_verified() {
    // `@param string` on `string $v` refines nothing, so it must not re-grade the
    // runtime-enforced fact down to a claim: no `(asserted)` marker.
    assert_eq!(param_dump("string", "string "), "dumped type: string");
    assert_eq!(param_dump("int", "int "), "dumped type: int");
}

#[test]
fn a_coerced_float_parameter_is_not_re_based_by_its_docblock() {
    // `@param int $v` on `float $v` is subsumption-clean (`float` accepts ints,
    // PHPStan semantics) so the arm survives, but PHP coerces at entry — only a
    // same-base REFINEMENT may reach the value lane, not a re-based `Fact::General`.
    assert_eq!(param_dump("int", "float "), "dumped type: float");
}

#[test]
fn an_implicitly_nullable_parameter_keeps_its_null() {
    // `string $v = null` is implicitly nullable, but the arm lane is built only
    // from the written native type and can't see the default — narrowing from
    // those arms would drop a `null` the runtime admits, so the seed declines.
    let src = "<?php\n/** @param non-empty-string $v */\n\
               function f(string $v = null): void { \\PHPStan\\dumpType($v); }\n";
    assert_eq!(one_type(src), "dumped type: string|null");
    // An explicit `?string` has no such blind spot: its arms drop `null` too, honestly.
    assert_eq!(
        param_dump("non-empty-string", "?string "),
        "dumped type: non-empty-string (asserted)"
    );
}

// ---- The proof-layer firewall (ADR-0037, ADR-0052 N2) ----------------------

#[test]
fn a_lying_param_cannot_forge_a_proof_layer_claim() {
    // Trust discipline: a docblock refinement is `Asserted`, and the proof
    // layer's all-Verified rule refuses it — these docblocks are LIES (nothing
    // stops a caller passing `''` or `-1`), so a laundered claim would be a false
    // positive against real code. The assertion covers the WHOLE diagnostic set,
    // not one id, so a future proof-layer consumer breaks this test on purpose.
    let sources = [
        // The refinement, bound and dumped.
        "<?php\n/** @param non-empty-string $v */\nfunction f(string $v): void { \\PHPStan\\dumpType($v); }\n",
        // The refinement flowing into a guard, where the interval does real work.
        "<?php\n/** @param positive-int $n */\n\
         function f(int $n): void { if ($n > 0) { \\PHPStan\\dumpType($n); } }\n",
        // Flowing into arithmetic and division, what a proof-layer consumer
        // would most want to reason from.
        "<?php\n/** @param positive-int $n */\nfunction f(int $n): int { return 10 / $n; }\n",
        // The refinement reaching a builtin argument position.
        "<?php\n/** @param non-empty-string $v */\nfunction f(string $v): void { echo strlen($v); }\n",
    ];
    for src in sources {
        let tree = SourceTree::parse(src);
        let ds = check(&tree, &[], "t.php");
        let proof: Vec<&Diagnostic> =
            ds.iter().filter(|d| layer(d.id) == Some(Layer::Proof)).collect();
        assert!(proof.is_empty(), "an asserted refinement reached the proof layer in {src:?}: {proof:?}");
    }
}

#[test]
fn the_refinement_carries_the_asserted_marker_and_the_native_base_does_not() {
    // The stratum bit itself, read off the binding through the dump surface.
    // Same fact domain, two grades — the grade IS the firewall's mechanism.
    assert_eq!(param_dump("non-empty-string", "string "), "dumped type: non-empty-string (asserted)");
    assert_eq!(param_dump("string", "string "), "dumped type: string");
}
