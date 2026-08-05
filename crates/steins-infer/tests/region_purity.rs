//! The region purity probe (ADR-0076 §2): what the effect and throw fixpoints
//! prove about a byte span inside a function body.
//!
//! This is the seam the loop→`array_map` transform's precondition is spelled in,
//! so the properties that make the precondition *stricter than `Pure`* are
//! pinned here rather than only at the transform's own boundary:
//!
//! - only the **proven** lane is reported (a declared `≤` bound answers nothing
//!   and leaves the region non-exhaustive, ADR-0067);
//! - throw origins are evaluated with the **enclosing** `try`/`catch` guards
//!   stripped, because an enclosing `catch` is the observer that distinguishes
//!   partial accumulation from an unassigned accumulator.

use steins_db::{PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{RegionPurity, region_purity_project};

/// The purity of the region between the two `// region` markers of `source`.
fn region(source: &str) -> RegionPurity {
    let start = source.find("// region-start").expect("no start marker") as u32;
    let end = source.find("// region-end").expect("no end marker") as u32;
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "lib.php".to_owned(), source.to_owned());
    let project =
        Project::new(&db, vec![file], ProjectLayout::fallback(), PluginFacts::none());
    let mut answers =
        region_purity_project(&db, project, &[("lib.php".to_owned(), start, end)]);
    answers.pop().expect("one answer per region")
}

#[test]
fn a_pure_region_proves_nothing_and_stays_exhaustive() {
    let src = "<?php\nfunction f(int $n): int {\n    // region-start\n    $y = strlen((string) $n);\n    // region-end\n    return $y;\n}\n";
    let r = region(src);
    assert!(r.labels.is_empty(), "labels: {:?}", r.labels);
    assert!(r.exhaustive);
    assert!(r.throws.is_empty(), "throws: {:?}", r.throws);
    assert!(r.throws_exhaustive);
}

#[test]
fn an_effect_outside_the_region_does_not_count() {
    let src = "<?php\nfunction f(int $n): int {\n    echo $n;\n    // region-start\n    $y = $n + 1;\n    // region-end\n    return $y;\n}\n";
    let r = region(src);
    assert!(r.labels.is_empty(), "an echo before the region leaked in: {:?}", r.labels);
}

#[test]
fn an_effect_inside_the_region_is_proven_and_named() {
    let src = "<?php\nfunction f(int $n): int {\n    // region-start\n    echo $n;\n    // region-end\n    return $n;\n}\n";
    let r = region(src);
    assert_eq!(r.labels, vec!["output".to_owned()]);
}

#[test]
fn a_callees_effect_propagates_into_the_region() {
    let src = "<?php\nfunction shout(int $n): int { echo $n; return $n; }\nfunction f(int $n): int {\n    // region-start\n    $y = shout($n);\n    // region-end\n    return $y;\n}\n";
    let r = region(src);
    assert_eq!(r.labels, vec!["output".to_owned()], "callee effect did not propagate");
}

#[test]
fn a_dynamic_callee_taints_exhaustiveness() {
    let src = "<?php\nfunction f(callable $g): int {\n    // region-start\n    $y = $g(1);\n    // region-end\n    return $y;\n}\n";
    let r = region(src);
    assert!(r.labels.is_empty(), "an unresolved call must not manufacture a label");
    assert!(!r.exhaustive, "an unresolved call must taint exhaustiveness");
}

#[test]
fn a_declared_bound_lands_in_its_own_lane_and_never_in_the_proven_one() {
    // ADR-0067's lane wall. A `#[\Steins\Effect]` envelope on an interface method
    // is a CAP imported through an injected receiver, never an occurrence proof:
    // "at most io, and possibly more" is not "provably nothing".
    //
    // Note which bit does NOT carry it. The effect pass *discharges* the
    // exhaustiveness taint here — an envelope is a checked contract, so the call
    // site is no longer unknown — which is exactly why the declared lane has to
    // be reported separately: a proven-purity gate reading `exhaustive` alone
    // would admit this call.
    let src = "<?php\ninterface Repo {\n    #[\\Steins\\Effect('io')]\n    public function load(int $id): int;\n}\nfunction f(Repo $r): int {\n    // region-start\n    $y = $r->load(1);\n    // region-end\n    return $y;\n}\n";
    let r = region(src);
    assert!(r.labels.is_empty(), "a declared bound leaked into the proven lane: {r:?}");
    assert_eq!(r.declared, vec!["io".to_owned()], "declared lane: {r:?}");
}

#[test]
fn a_throw_inside_the_region_is_proven() {
    let src = "<?php\nfunction f(int $n): int {\n    // region-start\n    throw new RuntimeException('no');\n    // region-end\n}\n";
    let r = region(src);
    assert_eq!(r.throws, vec!["RuntimeException".to_owned()]);
}

#[test]
fn an_enclosing_catch_does_not_absorb_the_regions_throw() {
    // The load-bearing asymmetry (ADR-0076 §2.3). The whole function's escaping
    // throw set is empty — the `catch` absorbs it — but the REGION still throws,
    // and that is exactly what the enclosing `catch` would observe as partial
    // accumulation. Stripping the guards is what makes the two answers differ.
    let src = "<?php\nfunction f(int $n): int {\n    try {\n        // region-start\n        throw new RuntimeException('no');\n        // region-end\n    } catch (RuntimeException $e) {\n        return 0;\n    }\n    return $n;\n}\n";
    let r = region(src);
    assert_eq!(
        r.throws,
        vec!["RuntimeException".to_owned()],
        "an enclosing catch absorbed the region's throw"
    );
}

#[test]
fn an_unknown_region_answers_nothing_rather_than_guessing() {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "lib.php".to_owned(), "<?php\n".to_owned());
    let project =
        Project::new(&db, vec![file], ProjectLayout::fallback(), PluginFacts::none());
    let answers =
        region_purity_project(&db, project, &[("nowhere.php".to_owned(), 0, 10)]);
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0], RegionPurity::default());
}
