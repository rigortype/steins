//! Where an `unset` member is **inert** (ADR-0087 §5, issue #397).
//!
//! `unset` is vocabulary in every phpdoc position — lowering is position-blind, so
//! `@param \DateTime|unset $d`, `@return \DateTime|unset`, a property
//! `@var \DateTime|unset`, an inline `@var \DateTime|unset $this->p` and a
//! function-scope `@var \DateTime|unset $x` all reach `ContractTy::Unset` and never
//! a class named `unset`. What none of them has is a *meaning*: "undefined" says
//! nothing coherent about a parameter (always bound), a return (a value or a throw),
//! or a property (whose uninitialized story is native PHP's). So in those positions
//! the member is dropped from the value arms, no presence claim is seeded, and
//! nothing new is reported.
//!
//! **Inert is a two-sided claim, and the second side is the one this file exists
//! for.** Adding the member must not *add* a finding — and must not *delete* one
//! either. `unset`'s acceptance leaf answers `Maybe` (ADR-0087 §2, the floor for a
//! member no value inhabits), so folding it into a union's or-fold would swallow
//! every sibling's `No`: `f(1)` against `@param \DateTime|unset $d` would go silent
//! where `@param \DateTime $d` reports. Every fixture below therefore asserts on the
//! **whole** diagnostic list of both spellings and compares them, rather than
//! checking that some id is absent.
//!
//! The one licensed difference is the message's rendering of the declaration, which
//! quotes the author's own spelling (`(\DateTime | unset)`). [`erase_member`] takes
//! it back out; [`the_message_quotes_the_authors_own_spelling`] pins that it is
//! there.

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    Diagnostic, NoFold, PARAM_MISMATCH_ID, PHPDOC_MAYBE_UNDEFINED_ID, PHPDOC_PROP_MISMATCH_ID,
    RETURN_MISMATCH_ID, UNTYPED_PROPERTY_ID, VARIABLE_MAYBE_UNDEFINED_ID, VARIABLE_UNDEFINED_ID,
    check_full,
};
use steins_syntax::SourceTree;

fn all(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
}

/// The declaration's rendering inside a message quotes what the author wrote, so a
/// union carrying the member renders as `(T | unset)` where the plain spelling
/// renders as `T`. That difference is the *point* — a reader is told which
/// declaration was violated — so it is normalized away here rather than treated as
/// a divergence.
fn erase_member(message: &str) -> String {
    let mut out = message.to_owned();
    for (with, without) in
        [("(\\DateTime | unset)", "\\DateTime"), ("(int | unset)", "int"), ("(int | UNSET)", "int")]
    {
        out = out.replace(with, without);
    }
    out
}

/// A comparable projection of a finding: everything a user sees, with the declared
/// type's spelling normalized.
fn shape(d: &Diagnostic) -> (&'static str, u32, u32, String) {
    (d.id, d.line, d.column, erase_member(&d.message))
}

/// Render `template` twice — once with `{U}` as the `unset` member, once with `{U}`
/// as nothing — and assert the two full diagnostic lists agree.
///
/// Returns the list, so a caller can go on to say what it *is* as well as that the
/// member did not change it.
#[track_caller]
fn inert(template: &str) -> Vec<Diagnostic> {
    let with = all(&template.replace("{U}", "|unset"));
    let without = all(&template.replace("{U}", ""));
    let (a, b): (Vec<_>, Vec<_>) =
        (with.iter().map(shape).collect(), without.iter().map(shape).collect());
    assert_eq!(a, b, "the `unset` member changed the finding list\nwith: {with:#?}\nwithout: {without:#?}");
    // And the member's own id never appears: no position outside a top-level inline
    // `@var` seeds a presence claim (ADR-0087 §5).
    assert!(
        !with.iter().any(|d| d.id == PHPDOC_MAYBE_UNDEFINED_ID),
        "an inert position seeded a presence claim: {with:#?}"
    );
    with
}

/// The ids a source produces, in order.
fn ids(found: &[Diagnostic]) -> Vec<&str> {
    found.iter().map(|d| d.id).collect()
}

// ---------------------------------------------------------------------------
// `@param` — a parameter is always bound, so the member says nothing.
// ---------------------------------------------------------------------------

const PARAM_DECL: &str = "<?php\n/** @param \\DateTime{U} $d */\nfunction f($d): void {}\n";

#[test]
fn a_conforming_argument_is_silent_with_the_member_as_without_it() {
    let found = inert(&format!("{PARAM_DECL}f(new \\DateTime());\n"));
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn a_violating_argument_reports_exactly_what_the_plain_spelling_reports() {
    // The regression the member could cause: `unset`'s `Maybe` leaf swallowing the
    // sibling's `No` in the union's or-fold, deleting the finding.
    let found = inert(&format!("{PARAM_DECL}f(1);\n"));
    assert_eq!(ids(&found), vec![PARAM_MISMATCH_ID], "{found:#?}");
}

#[test]
fn the_abstract_fact_lane_reports_it_too() {
    // The other half of `check_phpdoc_param`: an argument that resolves to a fact
    // rather than a proven value is judged by `admits_fact` on the lowered contract,
    // a different union fold from the proven-value one.
    let found = inert(
        "<?php\n/** @param int{U} $d */\nfunction f($d): void {}\nfunction g(string $s): void { f($s); }\n",
    );
    assert_eq!(ids(&found), vec![PARAM_MISMATCH_ID], "{found:#?}");
}

#[test]
fn the_parameter_seeds_the_body_with_what_the_plain_spelling_seeds() {
    let found = inert(
        "<?php\n/** @param int{U} $d */\nfunction f($d): void { \\PHPStan\\dumpType($d); }\n",
    );
    assert_eq!(found[0].message, "dumped type: int (asserted)", "{found:#?}");
}

#[test]
fn the_message_quotes_the_authors_own_spelling() {
    // The one licensed difference `erase_member` normalizes away. A reader chasing
    // the finding is shown the declaration as written, `unset` member included —
    // the alternative would be quoting a docblock that is not in the file.
    let found = all("<?php\n/** @param \\DateTime|unset $d */\nfunction f($d): void {}\nf(1);\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].message.contains("\\DateTime | unset"),
        "the declaration is quoted as written: {}",
        found[0].message
    );
}

#[test]
fn a_bare_unset_param_states_no_envelope_and_reports_nothing() {
    // ADR-0087 §2.4: a bare `unset` lowers to no value arm at all, which is the
    // existing "no envelope, seed nothing" outcome — not a contract that refuses
    // every argument.
    let found = all("<?php\n/** @param unset $d */\nfunction f($d): void {}\nf(1);\n");
    assert!(found.is_empty(), "{found:#?}");
}

// ---------------------------------------------------------------------------
// `@return` — a function returns a value or does not return.
// ---------------------------------------------------------------------------

#[test]
fn the_returned_types_dump_identically() {
    let found = inert(
        "<?php\n/** @return \\DateTime{U} */\nfunction f() { return new \\DateTime(); }\n\\PHPStan\\dumpType(f());\n",
    );
    assert_eq!(found[0].message, "dumped type: DateTime (asserted)", "{found:#?}");
}

#[test]
fn a_violating_return_reports_exactly_what_the_plain_spelling_reports() {
    let found = inert("<?php\n/** @return \\DateTime{U} */\nfunction f() { return 1; }\n");
    assert_eq!(ids(&found), vec![RETURN_MISMATCH_ID], "{found:#?}");
}

// ---------------------------------------------------------------------------
// A property `@var`, and an inline `@var` on `$this->p`.
// ---------------------------------------------------------------------------

#[test]
fn a_property_var_accepts_and_refuses_what_the_plain_spelling_does() {
    let found = inert(
        "<?php\nclass C {\n    /** @var int{U} */\n    public $p;\n    function m(): void { $this->p = 'x'; }\n}\n",
    );
    assert_eq!(ids(&found), vec![PHPDOC_PROP_MISMATCH_ID], "{found:#?}");
}

#[test]
fn a_conforming_property_assignment_stays_silent() {
    let found = inert(
        "<?php\nclass C {\n    /** @var int{U} */\n    public $p;\n    function m(): void { $this->p = 1; }\n}\n",
    );
    assert!(found.is_empty(), "the `@var` still counts as a type claim: {found:#?}");
    assert!(!ids(&found).contains(&UNTYPED_PROPERTY_ID), "{found:#?}");
}

#[test]
fn an_inline_var_on_a_property_target_behaves_as_the_plain_spelling() {
    // ADR-0087 §5's fourth position. `$this->p` is a property, not a local, so the
    // tag speaks about a slot that is always *there* — a presence claim has no
    // subject.
    inert(
        "<?php\nclass C {\n    public $p;\n    function m(): void {\n        /** @var int{U} $this->p */\n        \\PHPStan\\dumpType($this->p);\n    }\n}\n",
    );
}

// ---------------------------------------------------------------------------
// Function-scope inline `@var`: the proof-layer pair keeps its rules, and the
// docblock neither manufactures a binding nor silences a proof.
// ---------------------------------------------------------------------------

#[test]
fn a_never_bound_function_local_keeps_the_definite_id() {
    let found =
        inert("<?php\nfunction f(): string {\n    /** @var \\DateTime{U} $x */\n    return $x->format('c');\n}\n");
    assert_eq!(ids(&found), vec![VARIABLE_UNDEFINED_ID], "{found:#?}");
}

#[test]
fn a_conditionally_bound_function_local_keeps_the_possibly_id() {
    let found = inert(
        "<?php\nfunction f(bool $c): string {\n    if ($c) { $x = new \\DateTime(); }\n    /** @var \\DateTime{U} $x */\n    return $x->format('c');\n}\n",
    );
    assert_eq!(ids(&found), vec![VARIABLE_MAYBE_UNDEFINED_ID], "{found:#?}");
    // …and it is still the `strict` rung's finding, not one the declaration moved
    // down to `contracts`. The two claims meet without merging (ADR-0087 §4.5).
    let contracts = ProfileConfigs::default().resolve(Some("contracts")).expect("built-in");
    let strict = ProfileConfigs::default().resolve(Some("strict")).expect("built-in");
    assert!(!contracts.is_surfaced(&found[0]), "{:#?}", found[0]);
    assert!(strict.is_surfaced(&found[0]), "{:#?}", found[0]);
}

#[test]
fn a_bound_function_local_is_silent_and_dumps_the_declared_type() {
    let found = inert(
        "<?php\nfunction f(): void {\n    $x = 1;\n    /** @var int{U} $x */\n    \\PHPStan\\dumpType($x);\n}\n",
    );
    assert_eq!(found[0].message, "dumped type: int (asserted)", "{found:#?}");
}

#[test]
fn a_closure_body_behaves_as_a_function_body() {
    let never =
        inert("<?php\n$f = function (): string {\n    /** @var \\DateTime{U} $x */\n    return $x->format('c');\n};\n");
    assert_eq!(ids(&never), vec![VARIABLE_UNDEFINED_ID], "{never:#?}");
    let maybe = inert(
        "<?php\n$f = function (bool $c): string {\n    if ($c) { $x = new \\DateTime(); }\n    /** @var \\DateTime{U} $x */\n    return $x->format('c');\n};\n",
    );
    assert_eq!(ids(&maybe), vec![VARIABLE_MAYBE_UNDEFINED_ID], "{maybe:#?}");
}

#[test]
fn a_method_body_behaves_as_a_function_body() {
    let never = inert(
        "<?php\nclass C {\n    function m(): string {\n        /** @var \\DateTime{U} $x */\n        return $x->format('c');\n    }\n}\n",
    );
    assert_eq!(ids(&never), vec![VARIABLE_UNDEFINED_ID], "{never:#?}");
    let maybe = inert(
        "<?php\nclass C {\n    function m(bool $c): string {\n        if ($c) { $x = new \\DateTime(); }\n        /** @var \\DateTime{U} $x */\n        return $x->format('c');\n    }\n}\n",
    );
    assert_eq!(ids(&maybe), vec![VARIABLE_MAYBE_UNDEFINED_ID], "{maybe:#?}");
}

#[test]
fn an_arrow_function_body_stays_silent_as_that_scope_always_does() {
    // An arrow function's body is an expression, so a statement-adjacent inline
    // `@var` sits in the *enclosing* scope and the body's own reads are judged by
    // nothing — the documented silence for the scope, unchanged by the member.
    let found = inert(
        "<?php\nfunction h(): callable {\n    /** @var \\DateTime{U} $x */\n    return fn (): string => $x->format('c');\n}\n",
    );
    assert!(found.is_empty(), "{found:#?}");
    let inline = inert(
        "<?php\n$g = fn (): string => (/** @var \\DateTime{U} $x */ $x)->format('c');\n",
    );
    assert!(inline.is_empty(), "{inline:#?}");
}

// ---------------------------------------------------------------------------
// The declaration never leaks a presence claim out of an inert position.
// ---------------------------------------------------------------------------

#[test]
fn no_inert_position_ever_emits_the_top_level_id() {
    // The seed pass scans a syntactic superset — every `$name` of a docblock whose
    // text contains `unset` (ADR-0087 §8.1) — so each of these docblocks *does*
    // produce a candidate. This is the assertion that the checker drops every one
    // of them, which is what makes the superset free.
    for src in [
        "<?php\n/** @param \\DateTime|unset $d */\nfunction f($d): void { echo $d->format('c'); }\n",
        "<?php\n/** @return \\DateTime|unset */\nfunction f() { return new \\DateTime(); }\necho f()->format('c');\n",
        "<?php\nclass C {\n    /** @var \\DateTime|unset */\n    public $p;\n}\n",
        "<?php\nclass C {\n    public $p;\n    function m(): void {\n        /** @var \\DateTime|unset $this->p */\n        echo $this->p->format('c');\n    }\n}\n",
        "<?php\nfunction f(): void {\n    $x = new \\DateTime();\n    /** @var \\DateTime|unset $x */\n    echo $x->format('c');\n}\n",
        "<?php\n$f = function (): void {\n    $x = new \\DateTime();\n    /** @var \\DateTime|unset $x */\n    echo $x->format('c');\n};\n",
        "<?php\nclass C {\n    function m(): void {\n        $x = new \\DateTime();\n        /** @var \\DateTime|unset $x */\n        echo $x->format('c');\n    }\n}\n",
    ] {
        let found = all(src);
        assert!(
            !found.iter().any(|d| d.id == PHPDOC_MAYBE_UNDEFINED_ID),
            "an inert position emitted the top-level id:\n{src}\n{found:#?}"
        );
    }
}

#[test]
fn the_member_is_read_case_blind_and_through_a_backslash_in_an_inert_position() {
    // The same one-table reading the top-level lane uses: no position gets its own
    // spelling rules.
    for spelling in ["\\DateTime|UNSET", "\\DateTime|\\unset", "unset|\\DateTime"] {
        let src = format!("<?php\n/** @param {spelling} $d */\nfunction f($d): void {{}}\nf(1);\n");
        let found = all(&src);
        assert_eq!(ids(&found), vec![PARAM_MISMATCH_ID], "`{spelling}`: {found:#?}");
    }
}
