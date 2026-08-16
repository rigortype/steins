//! `phpdoc.maybe-undefined` (ADR-0087 §4, issue #396): a read of a top-level
//! variable the author declared `/** @var T|unset $x */`, at a point where nothing
//! has discharged the possibly-undefined state that declaration states.
//!
//! The idiom is the Blade-view / included-partial one: the template is handed `$x`
//! by whatever included it, so `$x` is either a `\DateTime` or the variable is not
//! defined at all. ADR-0081 §6 silences the binding-presence pass over a script
//! scope for exactly that reason — an included file inherits the includer's symbol
//! table, so the CST cannot claim absence — and an explicit `|unset` is the
//! declaration that lifts the silence, for the declared name and nothing else.
//!
//! The presence engine is ADR-0081's, unchanged: same three-valued lattice, same
//! polarity-consuming guards, same terminating-arm subtraction, same loop fixpoint.
//! What this file pins is the slice's own vocabulary — which declarations seed, what
//! discharges, what stays silent, and the registry contract.

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    Diagnostic, Floor, Layer, NoFold, PHPDOC_MAYBE_UNDEFINED_ID, PHPDOC_MISPLACED_VAR_ID,
    PHPDOC_STALE_VAR_ID, VARIABLE_MAYBE_UNDEFINED_ID, VARIABLE_UNDEFINED_ID, check_full, layer,
    surface_floor,
};
use steins_syntax::SourceTree;

fn all(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
}

fn diags(src: &str) -> Vec<Diagnostic> {
    all(src).into_iter().filter(|d| d.id == PHPDOC_MAYBE_UNDEFINED_ID).collect()
}

fn silent(src: &str) {
    let d = diags(src);
    assert!(d.is_empty(), "expected silence, got: {d:#?}");
}

fn fires(src: &str, name: &str) -> Diagnostic {
    let d = diags(src);
    assert_eq!(d.len(), 1, "expected exactly one finding: {d:#?}");
    assert!(d[0].message.contains(&format!("${name}")), "{}", d[0].message);
    d[0].clone()
}

/// The declaration, then one unguarded read of it. The shape every fixture below
/// varies.
fn decl_then(body: &str) -> String {
    format!("<?php\n/** @var \\DateTime|unset $x */\n{body}")
}

// ---------------------------------------------------------------------------
// The registry contract (ADR-0081 §8's four coordinated edits).
// ---------------------------------------------------------------------------

#[test]
fn the_id_sits_in_the_contract_layer_at_the_contracts_floor() {
    // Not `Layer::Proof`: the premise is the author's own declaration, which is
    // unverifiable by definition, so it cannot sit behind a proof-layer id
    // (ADR-0052 §5). Not `Floor::Strict` either — the possibly grade answers
    // uncertainty about the *premise*, and a declaration has none.
    assert_eq!(layer(PHPDOC_MAYBE_UNDEFINED_ID), Some(Layer::Contract));
    assert_eq!(surface_floor(PHPDOC_MAYBE_UNDEFINED_ID), Some(Floor::Contracts));
    // Its proof-layer near-twin is untouched by this slice.
    assert_eq!(layer(VARIABLE_MAYBE_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(surface_floor(VARIABLE_MAYBE_UNDEFINED_ID), Some(Floor::Strict));
    // The ADR-0022 kebab-case spelling reaches users' baselines.
    assert_eq!(PHPDOC_MAYBE_UNDEFINED_ID, "phpdoc.maybe-undefined");
}

#[test]
fn contracts_and_strict_surface_it_and_default_does_not() {
    let finding = fires(&decl_then("echo $x->format('Y-m-d');\n"), "x");
    let default = ProfileConfigs::default().resolve(Some("default")).expect("built-in");
    assert!(!default.is_surfaced(&finding), "a bare `check` stays silent on a docblock claim");
    for profile in ["contracts", "strict"] {
        let surface = ProfileConfigs::default().resolve(Some(profile)).expect("built-in");
        assert!(surface.is_surfaced(&finding), "`{profile}` shows the declared-contract family");
    }
}

// ---------------------------------------------------------------------------
// The conformance fixture, `regressions_unset_pseudo_type.php` (issue #396).
// ---------------------------------------------------------------------------

/// The fixture's own body, minus the prose docblock: two `// V` controls under a
/// plain `\DateTime`, two `// E?` reads under `\DateTime|unset`, and three `// Q`
/// lines inside an `isset()` guard.
const CONFORMANCE: &str = r#"<?php

declare(strict_types=1);

namespace Conformance\Tests\RegressionsUnsetPseudoType;

/** @var \DateTime $defined */
echo $defined->format('Y-m-d'); // V
echo date_format($defined, 'Y-m-d'); // V

/** @var \DateTime|unset $read */
echo $read->format('Y-m-d'); // E?

/** @var \DateTime|unset $passed */
echo date_format($passed, 'Y-m-d'); // E?

/** @var \DateTime|unset $guarded */
if (isset($guarded)) { // Q
    echo $guarded->format('Y-m-d'); // Q
    echo date_format($guarded, 'Y-m-d'); // Q
}
"#;

#[test]
fn the_conformance_fixture_fires_on_both_e_lines_and_nowhere_else() {
    let d = diags(CONFORMANCE);
    let lines: Vec<u32> = d.iter().map(|d| d.line).collect();
    assert_eq!(lines, vec![12, 15], "exactly the two `// E?` reads: {d:#?}");
    assert!(d[0].message.contains("$read"), "{}", d[0].message);
    assert!(d[1].message.contains("$passed"), "{}", d[1].message);
}

#[test]
fn the_conformance_fixture_says_what_was_declared() {
    let d = diags(CONFORMANCE);
    assert!(
        d[0].message.contains("\\DateTime|unset"),
        "the message quotes the author's own spelling: {}",
        d[0].message
    );
    assert!(d[0].message.contains("isset($read)"), "and names the fix: {}", d[0].message);
}

/// Nothing else moves on the fixture — in particular `phpdoc.stale-var`, whose
/// whole shape is a `@var` naming a variable assigned nowhere, which is precisely
/// what this idiom writes on purpose.
#[test]
fn the_conformance_fixture_moves_no_other_id() {
    let others: Vec<Diagnostic> =
        all(CONFORMANCE).into_iter().filter(|d| d.id != PHPDOC_MAYBE_UNDEFINED_ID).collect();
    assert!(others.is_empty(), "no other id fires on the idiom: {others:#?}");
}

// ---------------------------------------------------------------------------
// The seed: which declarations state the possibly-undefined claim.
// ---------------------------------------------------------------------------

#[test]
fn a_plain_var_without_the_member_seeds_nothing() {
    silent("<?php\n/** @var \\DateTime $x */\necho $x->format('Y-m-d');\n");
}

#[test]
fn the_member_is_read_case_insensitively_and_through_a_leading_backslash() {
    for spelling in ["\\DateTime|UNSET", "\\DateTime|\\unset", "unset|\\DateTime"] {
        let src = format!("<?php\n/** @var {spelling} $x */\necho $x->format('Y-m-d');\n");
        assert_eq!(diags(&src).len(), 1, "`{spelling}` states the claim");
    }
}

#[test]
fn a_bare_unset_declaration_seeds_too() {
    // ADR-0087 §2.4 leaves a bare `@var unset $x` stating no value envelope; what it
    // states about the *binding* is this slice's own question, and it is the whole
    // claim the word carries.
    fires("<?php\n/** @var unset $x */\necho $x;\n", "x");
}

#[test]
fn a_nested_unset_is_a_different_claim_and_seeds_nothing() {
    // `array<int, unset>` speaks about an array's values, not about whether `$x` is
    // bound. ADR-0087 §5 has not decided that spelling, so it stays silent.
    silent("<?php\n/** @var array<int, unset> $x */\necho $x[0];\n");
}

#[test]
fn a_property_target_tag_speaks_about_a_property_not_a_local() {
    silent("<?php\n/** @var \\DateTime|unset $this->x */\necho $x;\n");
}

#[test]
fn a_prefixed_tag_displaces_the_plain_one_for_the_same_variable() {
    // ADR-0029 precedence: the `@phpstan-var` wins, and it does not carry the member.
    silent(
        "<?php\n/**\n * @var \\DateTime|unset $x\n * @phpstan-var \\DateTime $x\n */\necho $x->format('c');\n",
    );
    fires(
        "<?php\n/**\n * @var \\DateTime $x\n * @phpstan-var \\DateTime|unset $x\n */\necho $x->format('c');\n",
        "x",
    );
}

#[test]
fn a_comment_in_the_gap_breaks_the_adoption() {
    // The ADR-0073 adjacency rule, inherited whole.
    silent("<?php\n/** @var \\DateTime|unset $x */\n// nope\necho $x->format('c');\n");
}

#[test]
fn a_read_before_the_declaration_is_silent() {
    // The declaration is the premise, so a read that precedes it has none. Scope
    // entry stays `Bound`, which is ADR-0081 §6's script-scope silence kept literally.
    let d = diags("<?php\necho $x;\n/** @var \\DateTime|unset $x */\necho $x;\n");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 4, "only the read after the declaration: {d:#?}");
}

#[test]
fn the_declaration_re_declares_over_a_prior_binding() {
    // An inline `@var` is a cast, not a narrowing (ADR-0073 §2): it re-declares what
    // the name holds, and here what it re-declares is presence.
    let d = fires(
        "<?php\n$x = new \\DateTime();\n/** @var \\DateTime|unset $x */\necho $x->format('c');\n",
        "x",
    );
    assert_eq!(d.line, 4, "{d:#?}");
}

// ---------------------------------------------------------------------------
// The guard vocabulary (ADR-0081 §5), each with a firing and a silent spelling.
// ---------------------------------------------------------------------------

#[test]
fn isset_discharges_on_its_true_continuation_only() {
    silent(&decl_then("if (isset($x)) {\n    echo $x->format('c');\n}\n"));
    let d = fires(
        &decl_then("if (isset($x)) {\n    echo 'ok';\n}\necho $x->format('c');\n"),
        "x",
    );
    assert_eq!(d.line, 6, "the read after the guard is outside it: {d:#?}");
}

#[test]
fn a_negated_isset_early_exit_discharges_the_fall_through() {
    silent(&decl_then("if (!isset($x)) {\n    return;\n}\necho $x->format('c');\n"));
    // …and the arm the guard sends control into is the one where the name is NOT
    // set, so a read there still reports. No polarity ever refines toward absence
    // (ADR-0081 §5), which is why this is the guard's own arm reporting rather than
    // the pass proving anything new.
    fires(&decl_then("if (!isset($x)) {\n    echo $x->format('c');\n}\n"), "x");
}

#[test]
fn empty_discharges_on_its_false_continuation() {
    silent(&decl_then("if (empty($x)) {\n    return;\n}\necho $x->format('c');\n"));
    fires(&decl_then("if (empty($x)) {\n    echo 'no';\n}\necho $x->format('c');\n"), "x");
}

#[test]
fn null_coalesce_consumes_the_read() {
    silent(&decl_then("echo $x ?? 'default';\n"));
    fires(&decl_then("echo ($x ?? 'default') . $x->format('c');\n"), "x");
}

#[test]
fn coalesce_assign_binds_from_that_point() {
    silent(&decl_then("$x ??= new \\DateTime();\necho $x->format('c');\n"));
}

#[test]
fn an_assignment_binds_from_that_point() {
    silent(&decl_then("$x = new \\DateTime();\necho $x->format('c');\n"));
    let d = fires(&decl_then("echo $x->format('c');\n$x = new \\DateTime();\n"), "x");
    assert_eq!(d.line, 3, "the read precedes the binding: {d:#?}");
}

#[test]
fn the_defaulting_idiom_leaves_every_later_read_silent() {
    // The then-arm binds and the implicit else-arm holds `isset($x)` true, so the
    // join is `Bound` — the shape ADR-0081 §5 calls the reason polarity is
    // load-bearing rather than an optimization.
    silent(&decl_then("if (!isset($x)) {\n    $x = new \\DateTime();\n}\necho $x->format('c');\n"));
}

#[test]
fn a_guard_reads_through_an_offset_chain_to_its_root() {
    silent(
        "<?php\n/** @var array|unset $x */\nif (!isset($x['a']['b'])) {\n    return;\n}\necho $x['a']['b'];\n",
    );
}

#[test]
fn a_guard_reads_through_a_property_chain_to_its_root() {
    silent(&decl_then("if (!isset($x->date)) {\n    return;\n}\necho $x->format('c');\n"));
}

#[test]
fn a_statement_position_assert_discharges_everything_after_it() {
    silent(&decl_then("assert(isset($x));\necho $x->format('c');\n"));
    // The polarity is the guard vocabulary's own: `assert(!isset($x))` refines nothing.
    fires(&decl_then("assert(!isset($x));\necho $x->format('c');\n"), "x");
}

#[test]
fn a_conditional_binding_leaves_the_join_at_maybe() {
    let d = fires(
        &decl_then("if (date('N') === '1') {\n    $x = new \\DateTime();\n}\necho $x->format('c');\n"),
        "x",
    );
    assert_eq!(d.line, 6, "{d:#?}");
}

// ---------------------------------------------------------------------------
// Independence, and the constraint that the guard is never redundant.
// ---------------------------------------------------------------------------

#[test]
fn two_declared_variables_never_narrow_each_other() {
    let src = "<?php
/** @var \\DateTime|unset $a */
/** @var \\DateTime|unset $b */
if (isset($a)) {
    echo $a->format('c');
    echo $b->format('c');
}
";
    let d = diags(src);
    assert_eq!(d.len(), 1, "guarding `$a` says nothing about `$b`: {d:#?}");
    assert!(d[0].message.contains("$b"), "{}", d[0].message);
}

/// ADR-0087 §4.3: Steins has no redundant-`isset` id today, and this slice adds
/// nothing that could report the guard. Asserted over the **whole** diagnostic list,
/// at every built-in profile, so a future redundancy id inherits the constraint
/// rather than discovering it.
#[test]
fn a_guarded_declared_variable_produces_nothing_at_all() {
    let src = decl_then("if (isset($x)) {\n    echo $x->format('c');\n}\n");
    let found = all(&src);
    assert!(found.is_empty(), "the guard is meaningful, never redundant: {found:#?}");
    for profile in ["default", "contracts", "strict", "pedantic"] {
        let surface = ProfileConfigs::default().resolve(Some(profile)).expect("built-in");
        let shown: Vec<&Diagnostic> =
            found.iter().filter(|d| surface.is_surfaced(d)).collect();
        assert!(shown.is_empty(), "`{profile}`: {shown:#?}");
    }
}

// ---------------------------------------------------------------------------
// The name dams, and the `goto` dam.
// ---------------------------------------------------------------------------

#[test]
fn an_include_silences_the_reads_after_it_and_not_those_before() {
    // A top-level template routinely includes partials, so the ADR-0081 §6 rule —
    // a dam blanks the whole scope — would kill the feature. The dam moves every
    // declared name to `Bound` from that point instead: no claim after it, and the
    // reads before it are still judged.
    let d = diags(&decl_then("echo $x->format('c');\ninclude 'partial.php';\necho $x->format('c');\n"));
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].line, 3, "only the read before the dam: {d:#?}");
}

#[test]
fn extract_compact_and_variable_variables_dam_the_same_way() {
    for dam in ["extract($GLOBALS);", "compact('x');", "$$name = 1;", "eval('1;');"] {
        let src = decl_then(&format!("{dam}\necho $x->format('c');\n"));
        assert!(diags(&src).is_empty(), "`{dam}` must silence what follows it");
    }
}

#[test]
fn a_goto_anywhere_dams_the_pass() {
    // ADR-0081's non-goal: every other construct's exit edges are bounded by the
    // traversal, and a jump to an arbitrary label is not.
    silent(&decl_then("goto end;\nend:\necho $x->format('c');\n"));
}

// ---------------------------------------------------------------------------
// What this slice deliberately leaves alone.
// ---------------------------------------------------------------------------

#[test]
fn function_scope_keeps_the_definite_id_and_gains_nothing() {
    let src = "<?php
function f(): string {
    /** @var \\DateTime|unset $x */
    return $x->format('c');
}
";
    assert!(diags(src).is_empty(), "the new id is a top-level emitter in this slice");
    let ids: Vec<&str> = all(src).iter().map(|d| d.id).collect();
    assert!(
        ids.contains(&VARIABLE_UNDEFINED_ID),
        "a never-bound local keeps the definite id — a docblock cannot manufacture \
         a binding the scope proves absent: {ids:?}"
    );
}

#[test]
fn stale_var_and_misplaced_var_stay_silent_on_the_idiom() {
    let ids: Vec<&str> = all(CONFORMANCE).iter().map(|d| d.id).collect();
    assert!(!ids.contains(&PHPDOC_STALE_VAR_ID), "the tag names the point, not a typo: {ids:?}");
    assert!(!ids.contains(&PHPDOC_MISPLACED_VAR_ID), "{ids:?}");
}

#[test]
fn a_declaration_inside_a_function_does_not_leak_to_the_script_scope() {
    silent(
        "<?php\nfunction f(\\DateTime $x): string {\n    /** @var \\DateTime|unset $x */\n    return $x->format('c');\n}\necho $x;\n",
    );
}

#[test]
fn a_file_without_the_word_pays_nothing_and_reports_nothing() {
    silent("<?php\n$a = 1;\nif ($a) {\n    $b = 2;\n}\necho $b;\n");
}
