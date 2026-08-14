//! `variable.maybe-undefined` (ADR-0081, issue #267): a read of a name bound on
//! only *some* of the paths that reach it.
//!
//! PHP's own consequence on the unbound paths (`php -r`-witnessed, 8.5.9):
//! `Warning: Undefined variable $x`, evaluating to `null` — the same consequence
//! as the definite leg, so the two share a layer and differ only in floor. The
//! claim is weaker (a path, not the whole scope), so the id sits at `strict` and
//! the default profile never shows it.
//!
//! The firing set is pinned in `crates/steins-syntax/tests/binding_presence.rs`
//! (lattice, termination subtraction, loop fixpoint, guard polarities). This
//! file pins the **checker's** half: the warning-handler gate, the out-parameter
//! subtraction with its call-site-forward refinement, the floor, and the
//! disjointness from `variable.undefined` observed through `check_full`.

use steins_infer::profile::ProfileConfigs;
use steins_infer::{
    Diagnostic, Floor, Layer, NoFold, VARIABLE_MAYBE_UNDEFINED_ID, VARIABLE_UNDEFINED_ID,
    check_full, layer, surface_floor,
};
use steins_syntax::SourceTree;

fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == VARIABLE_MAYBE_UNDEFINED_ID)
        .collect()
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

// The registry contract.

#[test]
fn the_id_sits_in_the_proof_layer_at_the_strict_floor() {
    assert_eq!(layer(VARIABLE_MAYBE_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(surface_floor(VARIABLE_MAYBE_UNDEFINED_ID), Some(Floor::Strict));
    assert_eq!(surface_floor(VARIABLE_UNDEFINED_ID), Some(Floor::Default));
}

#[test]
fn only_the_strict_profile_surfaces_it() {
    let finding = fires(
        "<?php\nfunction f(bool $c): int {\n    if ($c) {\n        $x = 1;\n    }\n    return $x;\n}\n",
        "x",
    );
    for profile in ["default", "contracts", "throws-direct"] {
        let surface = ProfileConfigs::default().resolve(Some(profile)).expect("built-in");
        assert!(!surface.is_surfaced(&finding), "`{profile}` must not show the weaker claim");
    }
    let strict = ProfileConfigs::default().resolve(Some("strict")).expect("built-in");
    assert!(strict.is_surfaced(&finding), "`strict` is where the some-paths leg lives");
}

// Firing, and the sentence it carries.

#[test]
fn fires_on_the_some_paths_shape() {
    let d = fires(
        "<?php\nfunction f(bool $c): int {\n    if ($c) {\n        $x = 1;\n    }\n    return $x;\n}\n",
        "x",
    );
    assert_eq!(d.line, 6, "the read's own line: {d:#?}");
    assert!(d.message.contains("only some of the paths"), "{}", d.message);
    assert!(
        d.message.contains("PHP warns \"Undefined variable $x\""),
        "the message carries PHP's own sentence: {}",
        d.message
    );
    assert!(d.message.contains("evaluates to null"), "{}", d.message);
}

#[test]
fn fires_on_a_read_before_its_only_assignment() {
    let d = fires(
        "<?php\nfunction f(): int {\n    $y = $x;\n    $x = 1;\n    return $y;\n}\n",
        "x",
    );
    assert_eq!(d.line, 3, "{d:#?}");
}

#[test]
fn fires_inside_a_method_body() {
    fires(
        "<?php\nclass C {\n    public function m(bool $c): int {\n        if ($c) {\n            $x = 1;\n        }\n        return $x;\n    }\n}\n",
        "x",
    );
}

#[test]
fn a_terminating_arm_is_silence() {
    silent(
        "<?php\nfunction f(bool $c): int {\n    if ($c) {\n        $x = 1;\n    } else {\n        return 0;\n    }\n    return $x;\n}\n",
    );
}

// Disjointness from the definite leg, observed end to end.

#[test]
fn the_two_legs_never_both_fire_on_one_read() {
    let src = "<?php
function f(bool $c): int {
    if ($c) {
        $x = 1;
    }
    return $x + $nope;
}
";
    let tree = SourceTree::parse(src);
    let all = check_full(&tree, "test.php", &mut NoFold, true);
    let maybe: Vec<&Diagnostic> =
        all.iter().filter(|d| d.id == VARIABLE_MAYBE_UNDEFINED_ID).collect();
    let definite: Vec<&Diagnostic> =
        all.iter().filter(|d| d.id == VARIABLE_UNDEFINED_ID).collect();
    assert_eq!(maybe.len(), 1, "the some-paths name: {maybe:#?}");
    assert!(maybe[0].message.contains("$x"), "{}", maybe[0].message);
    assert_eq!(definite.len(), 1, "the never-bound name: {definite:#?}");
    assert!(definite[0].message.contains("$nope"), "{}", definite[0].message);
}

#[test]
fn a_name_bound_nowhere_stays_on_the_definite_leg() {
    silent("<?php\nfunction f(bool $c): int {\n    if ($c) {\n        echo 1;\n    }\n    return $nope;\n}\n");
}

// The warning-handler gate (ADR-0049 §7) — inherited verbatim.

#[test]
fn a_declared_null_warning_posture_takes_the_id_off_the_proof_surface() {
    let src = "<?php\nfunction f(bool $c): int {\n    if ($c) {\n        $x = 1;\n    }\n    return $x;\n}\n";
    let tree = SourceTree::parse(src);
    let aborting = check_full(&tree, "test.php", &mut NoFold, true);
    assert!(
        aborting.iter().any(|d| d.id == VARIABLE_MAYBE_UNDEFINED_ID),
        "the default `abort` posture reports"
    );
    let tolerant = check_full(&tree, "test.php", &mut NoFold, false);
    assert!(
        !tolerant.iter().any(|d| d.id == VARIABLE_MAYBE_UNDEFINED_ID),
        "a declared `null` posture tolerates the warning, exactly as the definite leg"
    );
}

// The out-parameter subtraction (ADR-0077), with the call-site-forward refinement.

#[test]
fn an_out_parameter_binds_from_its_call_site_forward() {
    // `preg_match`'s third argument is by-reference, so the read AFTER it is bound
    // on every path the call reaches.
    silent(
        "<?php\nfunction f(bool $c): mixed {\n    if ($c) {\n        $m = [];\n    }\n    preg_match('/a/', 'b', $m);\n    return $m;\n}\n",
    );
}

#[test]
fn a_read_before_the_out_parameter_call_is_not_subtracted() {
    // The binding is at the call site, so a preceding read reaches unbound;
    // subtracting scope-wide (the definite leg's ordering-blind rule) would
    // wrongly silence this.
    let d = fires(
        "<?php\nfunction f(bool $c): mixed {\n    $first = $m;\n    preg_match('/a/', 'b', $m);\n    if ($c) {\n        $m = [];\n    }\n    return $first;\n}\n",
        "m",
    );
    assert_eq!(d.line, 3, "{d:#?}");
}

#[test]
fn a_by_value_argument_binds_nothing() {
    // `strlen`'s parameter is by value, so the call leaves the name unbound: both
    // the argument occurrence and the read after it reach on an unbound path.
    let d = diags(
        "<?php\nfunction f(bool $c): mixed {\n    if ($c) {\n        $s = 'a';\n    }\n    strlen($s);\n    return $s;\n}\n",
    );
    assert_eq!(d.len(), 2, "the argument occurrence and the read after it: {d:#?}");
    assert_eq!(d[0].line, 6, "{d:#?}");
    assert_eq!(d[1].line, 7, "{d:#?}");
}

#[test]
fn a_read_reached_only_through_an_out_parameter_call_stays_on_the_definite_leg() {
    // Recorded obstacle: a name whose ONLY binding form is an out-parameter is
    // bound nowhere in the scope's *text*, so lowering routes reads to
    // `variable.undefined` instead — silent on both legs rather than reported
    // here. Fixing it would mean routing between legs in the checker, the exact
    // coupling the disjoint-by-construction split avoids; the cost is recall,
    // the direction the proof layer always errs.
    silent(
        "<?php\nfunction f(): mixed {\n    $first = $m;\n    preg_match('/a/', 'b', $m);\n    return $first;\n}\n",
    );
}

// Scopes that report nothing at all — the definite leg's universe, verbatim.

#[test]
fn the_top_level_script_scope_never_reports() {
    silent("<?php\nif ($c) {\n    $x = 1;\n}\necho $x;\n");
}

#[test]
fn an_arrow_function_body_never_reports() {
    silent("<?php\nfunction f(bool $c): callable {\n    return fn () => $x;\n}\n");
}

#[test]
fn a_name_dam_blanks_the_scope() {
    silent(
        "<?php\nfunction f(bool $c, array $a): mixed {\n    if ($c) {\n        $x = 1;\n    }\n    extract($a);\n    return $x;\n}\n",
    );
}

#[test]
fn a_guarded_read_is_not_this_finding() {
    silent(
        "<?php\nfunction f(bool $c): mixed {\n    if ($c) {\n        $x = 1;\n    }\n    return $x ?? 'd';\n}\n",
    );
    silent(
        "<?php\nfunction f(bool $c): bool {\n    if ($c) {\n        $x = 1;\n    }\n    return isset($x);\n}\n",
    );
}

#[test]
fn the_defaulting_idiom_is_silent() {
    // The shape defensive house styles produce on purpose (ADR-0081 §5): the
    // then-arm binds and the implicit else-arm holds `isset($x)`.
    silent(
        "<?php\nfunction f(): int {\n    if (!isset($x)) {\n        $x = 1;\n    }\n    return $x;\n}\n",
    );
}
