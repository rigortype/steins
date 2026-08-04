//! PHPStan data-provider ports for `accepts`, `isSuperTypeOf`, and
//! `TypeCombinator` unions (ADR-0030). Fixture rows retain their source-test and
//! provider-case provenance.
//!
//! Steins has one acceptance relation: `Yes` means subset, `No` disjoint, and
//! `Maybe` partial or undecided. Rows where PHPStan's relations disagree are
//! classified explicitly rather than reconciled.
//!
//! Right-hand types become concrete [`Val`] or abstract [`Fact`] probes; union
//! arms are folded with [`Certainty::all_of`]. Object types, `mixed`, `never`,
//! non-extensional strings, and arrays are outside this fixture surface.

use serde::Deserialize;
use std::path::Path;
use steins_contract::{admits_fact, admits_val, lower_str, ContractTy};
use steins_domain::{Base, Certainty, Fact, Refinement, Val};

#[derive(Debug, Deserialize)]
struct Fixture {
    meta: Meta,
    #[serde(default)]
    row: Vec<Row>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    operation: String,
    source_commit: String,
}

#[derive(Debug, Deserialize)]
struct Row {
    name: String,
    lhs: String,
    rhs: String,
    phpstan: Judgment,
    /// Absent when Steins agrees with `phpstan`.
    #[serde(default)]
    steins: Option<Judgment>,
    #[serde(default)]
    status: Status,
    #[serde(default)]
    adr: Option<String>,
    #[serde(default)]
    note: Option<String>,
    /// Provenance: the source test + data-provider case(s).
    source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Judgment {
    Yes,
    No,
    Maybe,
}

impl Judgment {
    fn certainty(self) -> Certainty {
        match self {
            Judgment::Yes => Certainty::Yes,
            Judgment::No => Certainty::No,
            Judgment::Maybe => Certainty::Maybe,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum Status {
    #[default]
    Ported,
    Divergent,
    NeedsDecision,
}

/// A right-hand type reduced to something Steins' surface can judge.
enum Probe {
    Val(Val),
    Fact(Fact),
}

/// Convert a lowered right-hand contract into probe(s), or `None` when the
/// type is out of Steins' value/fact surface (object world, `mixed`,
/// `never`, non-extensional string provenance, arrays, …). A union yields the
/// concatenation of its arms' probes; any inexpressible arm poisons the whole.
fn probes_of(ty: &ContractTy) -> Option<Vec<Probe>> {
    let one = |p| Some(vec![p]);
    match ty {
        ContractTy::Null => one(Probe::Val(Val::Null)),
        ContractTy::LitInt(i) => one(Probe::Val(Val::Int(*i))),
        ContractTy::LitFloat(f) => one(Probe::Val(Val::Float(*f))),
        ContractTy::LitStr(s) => one(Probe::Val(Val::Str(s.clone()))),
        ContractTy::LitBool(b) => one(Probe::Val(Val::Bool(*b))),
        ContractTy::Base(b) => one(Probe::Fact(Fact::General { base: *b, nullable: false })),
        ContractTy::IntIn(r) => {
            one(Probe::Fact(Fact::refined(Base::Int, Refinement::Int(*r), false)))
        }
        ContractTy::StrWith(p) => {
            one(Probe::Fact(Fact::refined(Base::String, Refinement::Str(*p), false)))
        }
        ContractTy::Union(members) => {
            let mut all = Vec::new();
            for m in members {
                all.extend(probes_of(m)?);
            }
            Some(all)
        }
        // Out of surface: Mixed, Never, StrOpaque, arrays/lists/maps/shapes,
        // Class, ObjectAny, CallableTy, IterableOf, Inter, Opaque.
        _ => None,
    }
}

/// Run Steins' acceptance relation of `lhs` over the probe(s) of `rhs`.
/// `None` when `rhs` is out of surface.
fn eval(lhs: &ContractTy, rhs: &ContractTy) -> Option<Certainty> {
    let probes = probes_of(rhs)?;
    Some(Certainty::all_of(probes.iter().map(|p| match p {
        Probe::Val(v) => admits_val(lhs, v),
        Probe::Fact(f) => admits_fact(lhs, f),
    })))
}

struct Tally {
    ported: usize,
    divergent: usize,
    needs_decision: usize,
}

fn run_fixture(path: &Path) -> Tally {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let fixture: Fixture = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let file = path.file_name().unwrap().to_string_lossy();

    assert!(
        !fixture.meta.source_commit.is_empty() && !fixture.meta.operation.is_empty(),
        "{file}: fixture must pin a source commit and name an operation",
    );

    let mut t = Tally { ported: 0, divergent: 0, needs_decision: 0 };
    for row in &fixture.row {
        let lhs = lower_str(&row.lhs)
            .unwrap_or_else(|| panic!("{file}/{}: lhs {:?} must lower", row.name, row.lhs));
        let rhs = lower_str(&row.rhs)
            .unwrap_or_else(|| panic!("{file}/{}: rhs {:?} must lower", row.name, row.rhs));
        let actual = eval(&lhs, &rhs).unwrap_or_else(|| {
            panic!(
                "{file}/{}: rhs {:?} is out of surface — it must not appear in a fixture row",
                row.name, row.rhs,
            )
        });

        // Pin Steins' output even on explicit divergences.
        let expected = row.steins.unwrap_or(row.phpstan);
        assert_eq!(
            actual,
            expected.certainty(),
            "{file}/{} [{}]: {} {} {} — Steins produced {:?}, fixture asserts {:?}",
            row.name,
            row.source,
            row.lhs,
            fixture.meta.operation,
            row.rhs,
            actual,
            expected,
        );

        match row.status {
            Status::Ported => {
                assert_eq!(
                    expected, row.phpstan,
                    "{file}/{}: a PORTED row must assert PHPStan's own judgment",
                    row.name,
                );
                assert!(
                    row.adr.is_none() && row.note.is_none(),
                    "{file}/{}: a PORTED row carries no divergence/decision tag",
                    row.name,
                );
                t.ported += 1;
            }
            Status::Divergent => {
                assert_ne!(
                    expected, row.phpstan,
                    "{file}/{}: a DIVERGENT row must differ from PHPStan",
                    row.name,
                );
                assert!(
                    row.adr.as_deref().is_some_and(|s| !s.is_empty()),
                    "{file}/{}: a DIVERGENT row must cite an ADR",
                    row.name,
                );
                t.divergent += 1;
            }
            Status::NeedsDecision => {
                assert_ne!(
                    expected, row.phpstan,
                    "{file}/{}: a NEEDS-DECISION row must differ from PHPStan",
                    row.name,
                );
                assert!(
                    row.note.as_deref().is_some_and(|s| !s.is_empty()),
                    "{file}/{}: a NEEDS-DECISION row must carry analysis",
                    row.name,
                );
                t.needs_decision += 1;
            }
        }
    }
    t
}

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phpstan")
}

#[test]
fn accepts_fixture() {
    let t = run_fixture(&fixture_dir().join("accepts.toml"));
    // Prevent silent fixture shrinkage.
    assert!(t.ported >= 18, "accepts: expected >=18 ported rows, got {}", t.ported);
    // Four class-string divergences (ADR-0038) and one narrow-LHS case (ADR-0030).
    assert_eq!(t.divergent, 5, "accepts: divergent count changed");
    assert_eq!(t.needs_decision, 0, "accepts: needs_decision count changed");
}

#[test]
fn is_super_type_of_fixture() {
    let t = run_fixture(&fixture_dir().join("is_super_type_of.toml"));
    assert!(t.ported >= 15, "isSuperTypeOf: expected >=15 ported rows, got {}", t.ported);
    assert_eq!(t.divergent, 2, "isSuperTypeOf: divergent count changed");
    assert_eq!(t.needs_decision, 0, "isSuperTypeOf: needs_decision count changed");
}

#[test]
fn type_combinator_union_fixture() {
    let t = run_fixture(&fixture_dir().join("type_combinator_union.toml"));
    assert!(t.ported >= 14, "union: expected >=14 ported rows, got {}", t.ported);
    assert_eq!(t.divergent, 0);
    assert_eq!(t.needs_decision, 0);
}
