//! ADR-0088 §5 (issue #433): a `default`-less `match` whose arms do not cover
//! the subject's **Verified** domain throws `\UnhandledMatchError` at runtime,
//! and that throw is a direct-origin `throw.undeclared` contribution — no new
//! id, the existing envelope-escape machinery (ADR-0040/0007) asked one more
//! question about one more origin kind.
//!
//! The gate is the whole point: `UnhandledMatchError` extends `Error`, which
//! ADR-0007 keeps unchecked by default (the proof layer's prey, not envelope
//! bookkeeping), and every other `Error`/`LogicException` throw still gets that
//! answer unconditionally (see `throws.rs`'s checked-accounting section). This
//! ONE class is checked, but only where the dataflow walk itself proved the
//! no-match path's residue is a real, narrowed, non-empty leftover — the exact
//! chain-level evidence rule `#428`'s sentinel (`never_sentinel.rs`) already
//! reads off `Store::contract_narrowed`/`Store::contract_emptied` for the
//! opposite verdict. An unproven residue (an untouched lane, an opaque/
//! unstructured `match`, a `switch`'s always-over-approximate one) stays
//! silent, never a finding — ADR-0002's zero-false-positive floor.
//!
//! Enum subjects are where this pays off in practice (ADR-0088 §5's own
//! framing), which is why every fixture below uses one: it is the one place a
//! literal-value `match`'s arm-wise subtraction can actually prove a `Base`-typed
//! domain empty (an enum case dies exactly, where a scalar literal never
//! subsumes a whole scalar base — see the layered-row fixture's comment for
//! why that asymmetry is what makes the docblock fixture land where it does).

use steins_infer::{Diagnostic, Facet, Origin, THROW_UNDECLARED_ID, check};
use steins_syntax::SourceTree;

fn undeclared(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == THROW_UNDECLARED_ID).collect()
}

const SUIT: &str = "enum Suit { case Hearts; case Spades; case Clubs; }";

// ---------------------------------------------------------------------------
// The core verdict: missing a case reports, covering every case is silent
// ---------------------------------------------------------------------------

#[test]
fn a_default_less_enum_match_missing_a_case_reports_direct() {
    let src = format!(
        "<?php\n{SUIT}\n/** @throws \\RuntimeException */\nfunction f(Suit $s): int {{\n\
         \treturn match ($s) {{\n\
         \t\tSuit::Hearts => 1,\n\
         \t\tSuit::Spades => 2,\n\
         \t}};\n}}\n"
    );
    let ds = undeclared(&src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert_eq!(
        ds[0].message,
        "UnhandledMatchError can escape f() but is not declared (@throws RuntimeException) — proven escape"
    );
    assert_eq!(ds[0].facet, Some(Facet::Origin(Origin::Direct)));
}

#[test]
fn a_default_less_enum_match_covering_every_case_is_silent() {
    // ADR-0088 §5's own framing: this is the shape that breaks loudest if the
    // gate is wrong — the idiomatic exhaustive enum `match` has no `default`.
    let src = format!(
        "<?php\n{SUIT}\n/** @throws \\RuntimeException */\nfunction f(Suit $s): int {{\n\
         \treturn match ($s) {{\n\
         \t\tSuit::Hearts => 1,\n\
         \t\tSuit::Spades => 2,\n\
         \t\tSuit::Clubs => 3,\n\
         \t}};\n}}\n"
    );
    assert_eq!(undeclared(&src), Vec::new(), "every case covered — the construct cannot throw");
}

#[test]
fn a_default_less_native_union_match_covering_every_alternative_is_silent() {
    // The native-union form of the "covers it" fixture: `?Suit` is a genuine
    // multi-alternative Verified union (not one enum's own case set), and
    // `null` plus every case is the whole thing.
    let src = format!(
        "<?php\n{SUIT}\n/** @throws \\RuntimeException */\nfunction f(?Suit $s): int {{\n\
         \treturn match ($s) {{\n\
         \t\tnull => 0,\n\
         \t\tSuit::Hearts => 1,\n\
         \t\tSuit::Spades => 2,\n\
         \t\tSuit::Clubs => 3,\n\
         \t}};\n}}\n"
    );
    assert_eq!(undeclared(&src), Vec::new(), "null plus every case exhausts ?Suit");
}

// ---------------------------------------------------------------------------
// Propagation and damming — exactly like any other throw (ADR-0040)
// ---------------------------------------------------------------------------

#[test]
fn the_contribution_propagates_to_an_undammed_caller() {
    let src = format!(
        "<?php\n{SUIT}\n\
         /** @throws \\RuntimeException */\n\
         function inner(Suit $s): int {{ return match ($s) {{ Suit::Hearts => 1, Suit::Spades => 2, }}; }}\n\
         /** @throws \\RuntimeException */\n\
         function outer(Suit $s): int {{ return inner($s); }}\n"
    );
    let ds = undeclared(&src);
    assert_eq!(ds.len(), 2, "got: {ds:#?}");
    let by_origin: std::collections::HashSet<_> = ds.iter().map(|d| d.facet).collect();
    assert!(by_origin.contains(&Some(Facet::Origin(Origin::Direct))), "inner()'s own contribution");
    assert!(by_origin.contains(&Some(Facet::Origin(Origin::Propagated))), "outer() inherits it up the call edge");
}

#[test]
fn a_try_catch_wrapper_dams_the_contribution() {
    let src = format!(
        "<?php\n{SUIT}\n/** @throws \\RuntimeException */\nfunction f(Suit $s): int {{\n\
         \ttry {{\n\
         \t\treturn match ($s) {{ Suit::Hearts => 1, Suit::Spades => 2, }};\n\
         \t}} catch (\\RuntimeException $e) {{\n\
         \t\treturn 0;\n\
         \t}}\n}}\n"
    );
    assert_eq!(undeclared(&src), Vec::new(), "the try/catch around the match kills the contribution");
}

// ---------------------------------------------------------------------------
// A `default` arm silences the id regardless of coverage
// ---------------------------------------------------------------------------

#[test]
fn a_default_arm_stays_silent_even_missing_two_cases() {
    let src = format!(
        "<?php\n{SUIT}\n/** @throws \\RuntimeException */\nfunction f(Suit $s): int {{\n\
         \treturn match ($s) {{\n\
         \t\tSuit::Hearts => 1,\n\
         \t\tdefault => 0,\n\
         \t}};\n}}\n"
    );
    assert_eq!(undeclared(&src), Vec::new(), "a default arm can never leave \\UnhandledMatchError reachable");
}

// ---------------------------------------------------------------------------
// The Verified grade: a docblock refinement suppresses nothing
// ---------------------------------------------------------------------------

#[test]
fn a_docblock_refinement_narrower_than_native_does_not_suppress_the_throw() {
    // ADR-0088's own layered worked-example row: `int` native, `@param 1|2`
    // docblock. The arms exhaust the DOCBLOCK's claim exactly, and the engine
    // enforces only `int` at the call boundary — a value like `3` genuinely
    // reaches the `match` at runtime, so the throw is real and must not be
    // suppressed by believing the docblock. (The asymmetry that makes this
    // land: `subtract_contract_lane` drops an emptied lane to *absent* rather
    // than *kept-empty* the moment any surviving arm is `Asserted` — so
    // `contract_emptied` never reads "the docblock's claim died" as "the
    // Verified domain died".)
    let src = "<?php\n\
        /**\n * @param 1|2 $x\n * @throws \\RuntimeException\n */\n\
        function f(int $x): int {\n\
        \treturn match ($x) {\n\
        \t\t1 => 10,\n\
        \t\t2 => 20,\n\
        \t};\n}\n";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "believing the docblock's 1|2 is exactly the mistake this finding surfaces");
    assert_eq!(ds[0].facet, Some(Facet::Origin(Origin::Direct)));
}

// ---------------------------------------------------------------------------
// A lane that never narrowed is ignorance, not evidence (ADR-0088 §4's rule,
// read here for the opposite verdict)
// ---------------------------------------------------------------------------

#[test]
fn an_untyped_subject_that_cannot_narrow_at_all_stays_silent() {
    // No native type and no `@param`: `Store::contract` is never seeded for
    // `$x`, so `contract_narrowed` can never read `true` — ignorance, and the
    // safe direction is silence, not a claimed exhaustion failure.
    let src = "<?php\n/** @throws \\RuntimeException */\nfunction f($x): int {\n\
        \treturn match ($x) {\n\
        \t\t'a' => 1,\n\
        \t\t1 => 2,\n\
        \t};\n}\n";
    assert_eq!(undeclared(src), Vec::new());
}

#[test]
fn switch_never_contributes_this_id_regardless_of_its_residue() {
    // `switch`'s residue is an over-approximation the codebase already refuses
    // to read as coverage evidence (`subtract_no_match_path`'s own doc); this
    // gate does not even ask the question for `loose` — `switch` cannot raise
    // `\UnhandledMatchError` at all, `default`-less or not.
    let src = format!(
        "<?php\n{SUIT}\n/** @throws \\RuntimeException */\nfunction f(Suit $s): void {{\n\
         \tswitch ($s) {{\n\
         \t\tcase Suit::Hearts: echo 1; break;\n\
         \t\tcase Suit::Spades: echo 2; break;\n\
         \t}}\n}}\n"
    );
    assert_eq!(undeclared(&src), Vec::new());
}
