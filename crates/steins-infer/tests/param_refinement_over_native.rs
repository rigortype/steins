//! A `@param` that NARROWS a native type reaches the value lane (issue #242).
//!
//! `@param non-empty-string $s` on a `string $s` is one of the commonest shapes in
//! typed PHP, and every such refinement used to be dropped. The arm lane was never
//! at fault — `subsumes(string, lowercase-string)` is `Yes`, so ADR-0052 §9 kept the
//! arm and the declared-side surface (`dumpPhpDocType`) rendered it all along. The
//! loss was one lane over: the native-type parameter pass seeds `Fact::General` from
//! the parameter's own native type BEFORE the arm lane is built, and the value lane
//! outranks the arm lane at every fact read, so the coarse `string` shadowed the
//! refinement that had survived beside it.
//!
//! The array vocabulary was exempt for a mechanical reason, not a principled one:
//! the native pass seeds a lone native *scalar* member only, so a native `array $a`
//! left the value lane free and the shape seed filled it from the very same arms.
//! That asymmetry is what localized the defect.
//!
//! Two properties are load-bearing here and each is pinned below:
//!
//! * the subset filter stays ON — a narrowing the native type does not cover is
//!   still dropped, in both directions (`@param int` on `string`, `@param string` on
//!   `int`);
//! * the refinement is `Asserted` — it is a docblock claim, so it may narrow a
//!   report and premise a contract-layer finding, but it can never premise a
//!   proof-layer one (ADR-0037, ADR-0052 N2).

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
    // An intersection of predicates narrows just as one does — the reach half of
    // the #235 probe, where a declared conjunction over a native `string` was
    // being dropped twice over.
    assert_eq!(
        param_dump("non-empty-lowercase-string", "string "),
        "dumped type: non-empty-lowercase-string (asserted)"
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
    // The subset discipline (ADR-0052 §9): the docblock never widens past the
    // runtime-enforced native type. `@param int` on a `string $v` is a
    // contradiction — no arm survives, so nothing narrows and the native fact
    // stands, ungraded.
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
    // `@param int $v` on a `float $v` is subsumption-clean (`float` accepts ints,
    // PHPStan core semantics), so the arm survives — but PHP coerces the argument
    // at entry, so the value really is a float whatever the docblock says. Only a
    // same-base REFINEMENT may reach the value lane; a re-based `Fact::General`
    // may not.
    assert_eq!(param_dump("int", "float "), "dumped type: float");
}

#[test]
fn an_implicitly_nullable_parameter_keeps_its_null() {
    // `string $v = null` is implicitly nullable, but the arm lane is built from the
    // written native type alone and so cannot see the default. Narrowing the value
    // lane from those arms would drop a `null` the runtime really admits, so the
    // seed declines and the native fact stands.
    let src = "<?php\n/** @param non-empty-string $v */\n\
               function f(string $v = null): void { \\PHPStan\\dumpType($v); }\n";
    assert_eq!(one_type(src), "dumped type: string|null");
    // An explicitly nullable native type has no such blind spot: the arms drop the
    // `null` because the docblock does, and that narrowing is honest.
    assert_eq!(
        param_dump("non-empty-string", "?string "),
        "dumped type: non-empty-string (asserted)"
    );
}

// ---- The proof-layer firewall (ADR-0037, ADR-0052 N2) ----------------------

#[test]
fn a_lying_param_cannot_forge_a_proof_layer_claim() {
    // The trust discipline at its mechanism: a refinement that arrives from a
    // docblock is `Asserted`, and the proof layer's all-Verified rule refuses it.
    // These docblocks are LIES — nothing stops a caller passing `''` or `-1` — so
    // if the claim could be laundered to Verified anywhere, a proof-layer finding
    // premised on it would be a false positive against real code.
    //
    // The assertion is over the WHOLE diagnostic set rather than one id, so a
    // future proof-layer consumer of abstract facts breaks this test on purpose.
    let sources = [
        // The refinement, bound and dumped.
        "<?php\n/** @param non-empty-string $v */\nfunction f(string $v): void { \\PHPStan\\dumpType($v); }\n",
        // The refinement flowing into a guard, where the interval does real work.
        "<?php\n/** @param positive-int $n */\n\
         function f(int $n): void { if ($n > 0) { \\PHPStan\\dumpType($n); } }\n",
        // The refinement flowing into arithmetic and a division, the shape a
        // proof-layer consumer would most want to reason from.
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
    // The stratum bit itself, read straight off the binding through the dump
    // surface. Same fact domain, two grades — and the grade is the firewall's
    // mechanism, so pinning it here pins the firewall rather than one symptom.
    assert_eq!(param_dump("non-empty-string", "string "), "dumped type: non-empty-string (asserted)");
    assert_eq!(param_dump("string", "string "), "dumped type: string");
}
