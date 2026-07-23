//! ADR-0047 Slice A zero-behavior gate: threading a [`PartitionMap`] into the
//! planners must not change any decision. Whether `partitions` is `None`, the
//! single-region identity, or even a fully-declared multi-partition map, the
//! [`TransformReport`] is byte-identical — no planner consumes the map this slice
//! (ADR-0047 §6: "with one region the planner degenerates to today's behavior").

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::{
    PartitionMap, TransformReport, VouchSet, plan_phpdoc_honesty, plan_phpdoc_to_native,
};

/// A small multi-file project exercising a promotable free function, a method,
/// and a lying `@return` (so both planners have real work to do).
const FILES: &[(&str, &str)] = &[
    (
        "svc-a.example/src/Calc.php",
        "<?php\n/** @param int $x */\nfunction inc($x) { return $x + 1; }\ninc(1);\n",
    ),
    (
        "lib/Support/Box.php",
        "<?php\nfinal class Box {\n  /** @return int */\n  public function get() { return 'not-an-int'; }\n}\n",
    ),
    ("tests/CalcTest.php", "<?php\ninc(3);\n"),
];

fn project(db: &SteinsDatabase) -> Project {
    let inputs: Vec<SourceFile> = FILES
        .iter()
        .map(|(p, t)| SourceFile::new(db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    Project::new(db, inputs)
}

/// A fully-declared map assigning svc-a, a shared lib, and observers — the richest
/// input the planner could receive. Its presence must still change nothing.
fn declared_map() -> PartitionMap {
    PartitionMap::build(
        [("svc-a".to_owned(), vec!["svc-a.example/**".to_owned()])],
        vec!["tests/**".to_owned()],
    )
    .expect("disjoint partitions")
}

fn promote_with(partitions: Option<&PartitionMap>) -> TransformReport {
    let db = SteinsDatabase::default();
    let project = project(&db);
    plan_phpdoc_to_native(&db, project, &VouchSet::empty(), partitions)
}

fn honesty_with(partitions: Option<&PartitionMap>) -> TransformReport {
    let db = SteinsDatabase::default();
    let project = project(&db);
    plan_phpdoc_honesty(&db, project, &VouchSet::empty(), partitions)
}

#[test]
fn promote_is_byte_identical_across_partition_inputs() {
    let none = promote_with(None);
    let identity = promote_with(Some(&PartitionMap::single_region()));
    let declared = promote_with(Some(&declared_map()));
    assert_eq!(none, identity, "identity map must not change the promotion plan");
    assert_eq!(none, declared, "a declared map must not change the promotion plan");
    // Guard against a vacuously-empty report making the assertion meaningless.
    assert!(none.oracle.enumerated > 0, "the fixture should enumerate candidates");
}

#[test]
fn honesty_is_byte_identical_across_partition_inputs() {
    let none = honesty_with(None);
    let identity = honesty_with(Some(&PartitionMap::single_region()));
    let declared = honesty_with(Some(&declared_map()));
    assert_eq!(none, identity, "identity map must not change the honesty plan");
    assert_eq!(none, declared, "a declared map must not change the honesty plan");
    assert!(none.oracle.enumerated > 0, "the fixture should enumerate candidates");
}

#[test]
fn no_config_is_the_single_region_identity() {
    // The CLI's "no [transform.partitions] section" path yields `None`; assert that
    // `None` and the constructed identity are interchangeable at the planner seam.
    assert_eq!(promote_with(None), promote_with(Some(&PartitionMap::single_region())));
}
