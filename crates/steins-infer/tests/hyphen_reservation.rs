//! ADR-0091 §3 — the hyphen reservation, read from the consumer side.
//!
//! `steins-contract` pins the lowering (`hyphen_reservation_tests`); what this
//! file pins is the property that lowering buys here: **a hyphenated phpdoc
//! identifier is never namespace-resolved**. This crate's class machinery sits
//! behind `matches!(cty, ContractTy::Class(_))` — `accepts_class_name`'s
//! `resolve_pclass`, the contract-arm refiner's `resolve_class` — so a table
//! that cannot produce that variant leaves nothing to resolve. Issue #472 and
//! the cross-tool proposal (§8) both lean on the property, hence a test rather
//! than a comment.
//!
//! The dump surface is the observable: it renders a class arm as its resolved
//! FQN, so what a docblock name resolved *to* is readable off a `dumpType`.

use steins_infer::{DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, PARAM_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php")
}

/// The single `debug.type` body a one-dump source produces.
fn dumped(src: &str) -> String {
    let ds = findings(src);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

fn param_count(src: &str) -> usize {
    findings(src).into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).count()
}

/// **No namespace resolution** (ADR-0091 §3.1). Before this slice each of these
/// resolved the keyword through the file's namespace into a class that cannot
/// exist — PHP's compiler rejects `-` in a class-like name — and the dump
/// surface printed the result back:
///
/// ```text
/// namespace App;  @param non-empy-string  =>  dumped type: app\non-empy-string
/// namespace App;  @param foo-bar          =>  dumped type: app\foo-bar
/// ```
///
/// This is the defect ADR-0091 §2 measures across the conformance suite, where
/// 5 of 16 analyzer configurations reject a valid call for exactly this reason.
#[test]
fn a_hyphenated_name_is_never_resolved_through_the_namespace() {
    let ns = "<?php\nnamespace App\\Sub;\n\
        /** @param non-empy-string $s */\n\
        function f($s): void { \\PHPStan\\dumpType($s); }\n";
    assert_eq!(dumped(ns), "dumped type: unknown", "no app\\sub\\non-empy-string may be invented");

    let imported = "<?php\nnamespace App;\nuse Other\\Pkg;\n\
        /** @param foo-bar $s */\n\
        function f($s): void { \\PHPStan\\dumpType($s); }\n";
    assert_eq!(dumped(imported), "dumped type: unknown");

    let global = "<?php\n/** @param positive-integer $n */\n\
        function f($n): void { \\PHPStan\\dumpType($n); }\n";
    assert_eq!(dumped(global), "dumped type: unknown");

    // The phpdoc lane says the same thing: no declared contract, not a class.
    let phpdoc = "<?php\nnamespace App\\Sub;\n\
        /** @param non-empy-string $s */\n\
        function f($s): void { \\PHPStan\\dumpPhpDocType($s); }\n";
    let ds = findings(phpdoc);
    let dumps: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(dumps.len(), 1, "got: {ds:?}");
    assert_eq!(dumps[0].message, "dumped phpdoc type: no declared contract");
}

/// The control the reservation must not break: a name PHP *can* carry is still
/// resolved against the file's namespace, and modeled hyphenated vocabulary
/// still denotes what it denotes.
#[test]
fn a_resolvable_name_still_resolves() {
    let cls = "<?php\nnamespace App\\Sub;\n\
        /** @param Widget $w */\n\
        function f($w): void { \\PHPStan\\dumpType($w); }\n";
    assert_eq!(dumped(cls), "dumped type: app\\sub\\widget (asserted)");

    let keyword = "<?php\nnamespace App\\Sub;\n\
        /** @param non-empty-string $s */\n\
        function f($s): void { \\PHPStan\\dumpType($s); }\n";
    assert_eq!(dumped(keyword), "dumped type: non-empty-string (asserted)");

    let generic = "<?php\nnamespace App\\Sub;\n\
        /** @param int-range<0, 255> $b */\n\
        function f($b): void { \\PHPStan\\dumpType($b); }\n";
    assert_eq!(dumped(generic), "dumped type: int<0, 255> (asserted)");
}

/// ADR-0091 §4's **ordering**: the reservation speaks for what survives the
/// `@template` shadow, which runs over the parsed type before anything is
/// lowered. So a shadowed name is decided by the shadow — unaffected by this
/// slice in either direction — and the same spelling without the tag keeps the
/// class reading it always had.
#[test]
fn the_template_shadow_still_decides_first() {
    let shadowed = "<?php\nnamespace App;\n/**\n * @template TValue\n * @param TValue $v\n */\n\
        function f($v): void { \\PHPStan\\dumpType($v); }\n";
    assert_eq!(dumped(shadowed), "dumped type: unknown", "the template shadow owns this name");

    let unshadowed = "<?php\nnamespace App;\n/**\n * @param TValue $v\n */\n\
        function f($v): void { \\PHPStan\\dumpType($v); }\n";
    assert_eq!(
        dumped(unshadowed),
        "dumped type: app\\tvalue (asserted)",
        "no tag, no hyphen: an ordinary class reference",
    );

    // A template beside a hyphenated spelling: each is decided by its own rule.
    let both = "<?php\nnamespace App;\n\
        /**\n * @template TValue\n * @param TValue $v\n * @param non-empy-string $s\n */\n\
        function f($v, $s): void { \\PHPStan\\dumpType($s); }\n";
    assert_eq!(dumped(both), "dumped type: unknown");

    let call = "<?php\nnamespace App;\n/**\n * @template TValue\n * @param TValue $v\n */\n\
        function f($v): void {}\nf(1);\n";
    assert_eq!(param_count(call), 0, "a template param denotes anything → silent");
}

/// A contract nothing denotes rejects nothing. Each argument below was measured
/// against the manufactured `Class(\"non-empy-string\")` contract this slice
/// removes, whose acceptance leg answers a definite `No` for every non-object
/// value (ADR-0091 §1).
#[test]
fn a_typo_keyword_rejects_nothing() {
    let f = "<?php\nnamespace App;\n/** @param non-empy-string $s */\nfunction f($s): void {}\n";
    for arg in ["'x'", "1", "[]", "null", "1.5", "true"] {
        assert_eq!(param_count(&format!("{f}f({arg});")), 0, "{arg} must not be refuted");
    }
}

/// `phpdoc_advanced_int_range_keyword` (ADR-0091 §2), the fixture 5 of 16
/// analyzer configurations fail: a namespaced `int-range<0, 255>` must accept
/// the valid call. Steins passes it because the generic table models the
/// spelling — and now also because the reservation stands behind it, which is
/// what the wrong-arity half checks.
#[test]
fn the_conformance_int_range_fixture_holds() {
    let f = "<?php\nnamespace Conformance\\Tests;\n\
        /** @param int-range<0, 255> $b */\nfunction acceptsByte($b): void {}\n";
    assert_eq!(param_count(&format!("{f}acceptsByte(200);")), 0, "200 is a byte");
    assert_eq!(param_count(&format!("{f}acceptsByte(300);")), 1, "300 is not, and still reports");

    // The unmodeled arity of the same keyword: silence, not a class contract.
    let wrong = "<?php\nnamespace Conformance\\Tests;\n\
        /** @param int-range<0> $b */\nfunction acceptsByte($b): void {}\n";
    assert_eq!(param_count(&format!("{wrong}acceptsByte(300);")), 0);
}
