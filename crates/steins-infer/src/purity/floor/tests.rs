//! One fixture per [`GapKind`] per lane (ADR-0100 §3): each kind a lane can
//! produce surfaces as a finding of that lane's sibling id, naming the kind, and
//! the kinds a lane cannot produce are listed with the reason. A kind appended
//! to [`GapKind::ALL`] fails the totality test until it has a fixture or an
//! entry in the exclusions.

use std::collections::{BTreeSet, HashMap};

use steins_db::EffectsPolicy;
use steins_syntax::SourceTree;

use super::Floor;
use crate::cx::Cx;
use crate::project::{FileUnit, Index, LazyTree};
use crate::purity::{EnvelopeSpelling, OperativeBound};
use crate::site::reach::Frame;
use crate::site::{GapKind, ResolvedSite};
use crate::{EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID, THROW_MAYBE_UNDECLARED_ID, check};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Effects,
    Throws,
}

/// One declaration whose body raises `kind` in `lanes`. `@@` stands for the
/// lane's envelope: `#[\Steins\Pure]` or `/** @throws \RuntimeException */`.
struct Case {
    kind: GapKind,
    lanes: &'static [Lane],
    src: &'static str,
    display: &'static str,
}

const BOTH: &[Lane] = &[Lane::Effects, Lane::Throws];
const EFFECTS: &[Lane] = &[Lane::Effects];
const THROWS: &[Lane] = &[Lane::Throws];

const fn case(
    kind: GapKind,
    lanes: &'static [Lane],
    src: &'static str,
    display: &'static str,
) -> Case {
    Case { kind, lanes, src, display }
}

const CASES: &[Case] = &[
    case(
        GapKind::DynamicCallee,
        BOTH,
        "@@ function f(callable $c): mixed { return $c(); }",
        "f",
    ),
    case(
        GapKind::UnknownClass,
        BOTH,
        "@@ function f(string $c): object { return new $c(); }",
        "f",
    ),
    case(
        GapKind::UnknownFunction,
        BOTH,
        "@@ function f(): int { return undefined_fn_zzz(); }",
        "f",
    ),
    case(
        GapKind::OpenMethod,
        BOTH,
        "class Pick { private function hidden(): int { return 1; } }
         @@ function f(): int { return (new Pick())->hidden(); }",
        "f",
    ),
    case(
        GapKind::DeclaredReceiver,
        BOTH,
        "interface Repo { public function find(int $id): int; }
         @@ function f(Repo $r): int { return $r->find(1); }",
        "f",
    ),
    case(
        GapKind::InteropEnvelope,
        EFFECTS,
        "interface Gate { /** @phpstan-impure io.db */ public function open(): int; }
         @@ function f(Gate $g): int { return $g->open(); }",
        "f",
    ),
    case(
        GapKind::UnresolvedCallback,
        BOTH,
        "@@ function f(array $a): array { return array_map('undefined_fn_zzz', $a); }",
        "f",
    ),
    case(GapKind::UnseenCode, BOTH, "@@ function f(string $s): mixed { return eval($s); }", "f"),
    case(GapKind::UserCodeReach, BOTH, "@@ function f($x): int { return strlen($x); }", "f"),
    case(
        GapKind::StateConstruct,
        EFFECTS,
        "@@ function f(): int { static $n = 0; return ++$n; }",
        "f",
    ),
    case(GapKind::UnresolvedThrow, THROWS, "@@ function f($e): void { throw $e; }", "f"),
    case(
        GapKind::NoEffectRow,
        EFFECTS,
        "@@ function f(array $a): int { return array_sum($a); }",
        "f",
    ),
    case(GapKind::NoThrowRow, THROWS, "@@ function f(): int { return mb_strlen('x'); }", "f"),
    case(
        GapKind::ArgumentList,
        EFFECTS,
        "@@ function f(array $a): array { return array_keys(...$a); }",
        "f",
    ),
    case(
        GapKind::FlagDependentThrow,
        THROWS,
        "@@ function f(array $a, int $flags): string { return json_encode($a, $flags); }",
        "f",
    ),
    case(
        GapKind::MethodNotFound,
        BOTH,
        "final class Plain {}
         @@ function f(): int { return (new Plain())->nothing(); }",
        "f",
    ),
    case(
        GapKind::NonFinalThis,
        BOTH,
        "class K { @@ public function f(): int { return $this->g(); }
                   public function g(): int { return 1; } }",
        "K::f",
    ),
    case(GapKind::OperatorToString, BOTH, "@@ function f($o): string { return 'a' . $o; }", "f"),
    case(GapKind::OperatorMagicProperty, BOTH, "@@ function f($o): mixed { return $o->prop; }", "f"),
    case(GapKind::OperatorArrayAccess, BOTH, "@@ function f($o): mixed { return $o['k']; }", "f"),
    case(
        GapKind::OperatorIteration,
        BOTH,
        "@@ function f($o): int { $n = 0; foreach ($o as $v) { $n++; } return $n; }",
        "f",
    ),
    case(GapKind::OperatorClone, BOTH, "@@ function f($o): object { return clone $o; }", "f"),
];

/// What a lane can never produce, and why: the exclusions the totality test reads.
fn excluded(lane: Lane) -> &'static [GapKind] {
    match lane {
        // The throw rows, and the thrown class the throw lane alone names; and the
        // destructor, which no site records yet (#882): the effect lane's reading of
        // it is `a_destructor_gap_reports_at_strict`'s.
        Lane::Effects => &[
            GapKind::UnresolvedThrow,
            GapKind::NoThrowRow,
            GapKind::FlagDependentThrow,
            GapKind::Destructor,
        ],
        // The effect rows, the declared lane's interop import (the throw lane keeps
        // every declared receiver a gap), the state constructs and the arity a
        // certification needs.
        Lane::Throws => &[
            GapKind::InteropEnvelope,
            GapKind::StateConstruct,
            GapKind::NoEffectRow,
            GapKind::ArgumentList,
            // Nothing records it yet, and the throw lane resolves its own sites, so a
            // gap cannot be handed to it; it reads the same `resolved.gaps`.
            GapKind::Destructor,
        ],
    }
}

fn findings(src: &str, id: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let mut out: Vec<String> = check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == id)
        .map(|d| d.message)
        .collect();
    out.sort();
    out
}

#[test]
fn every_gap_kind_surfaces_in_every_lane_that_can_produce_it() {
    for case in CASES {
        for &lane in case.lanes {
            let (id, envelope, name) = match lane {
                Lane::Effects => (EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID, "#[\\Steins\\Pure]", "effect"),
                Lane::Throws => {
                    (THROW_MAYBE_UNDECLARED_ID, "/** @throws \\RuntimeException */", "throw")
                }
            };
            let src = format!("<?php\n{}", case.src.replace("@@", envelope));
            let marker = format!("({}:", case.kind.as_str());
            let unit = format!("{}()", case.display);
            let found = findings(&src, id);
            assert!(
                found.iter().any(|m| m.contains(&marker) && m.contains(&unit)),
                "{:?} in the {name} lane: expected a `{marker}` finding of `{id}`, got {found:#?}",
                case.kind,
            );
        }
    }
}

/// A gap of a kind no site records yet still reports at `strict`: the floor reads
/// whatever a resolved site carries, so the destructor kind needs no decision of its
/// own beyond being a [`GapKind`] (ADR-0100 §7.4). The gap is handed to
/// [`Floor::report_site`] directly, over the one site of a pure function. Once a site
/// records the kind this case moves into [`CASES`] and the exclusions drop it.
#[test]
fn a_destructor_gap_reports_at_strict() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): int { return strlen('x'); }";
    let tree = SourceTree::parse(src);
    let lazy = LazyTree::borrowed(&tree);
    let units = [FileUnit { path: "test.php", tree: &lazy }];
    let index = Index::from_units(&units);
    let cx = Cx::new(&units, &index, 0);
    let f = &tree.functions()[0];
    let frame = Frame::new(None, &f.params, &f.sites);
    let enveloped = HashMap::new();
    let floor = Floor::new(&enveloped, f.docblock.as_ref(), &f.params, true);
    let policy = EffectsPolicy::none();
    let bound = OperativeBound {
        labels: &[],
        span: f.span,
        spelling: EnvelopeSpelling::Attribute,
        policy: &policy,
    };
    let site = f.sites.first().expect("the call is a site");
    let resolved =
        ResolvedSite { gaps: BTreeSet::from([GapKind::Destructor]), ..Default::default() };
    let mut out = Vec::new();
    floor.report_site(&mut out, &cx, &frame, site, &resolved, &HashMap::new(), "f", bound);
    assert_eq!(out.len(), 1, "one finding for the one gap: {out:#?}");
    assert_eq!(out[0].id, EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID);
    assert!(out[0].message.contains("(destructor: a dropped value may run `__destruct`)"));
    assert!(out[0].message.contains("f() is declared #[\\Steins\\Pure]"));
}

#[test]
fn the_fixtures_and_exclusions_cover_every_gap_kind_per_lane() {
    for lane in [Lane::Effects, Lane::Throws] {
        let mut covered: BTreeSet<GapKind> =
            CASES.iter().filter(|c| c.lanes.contains(&lane)).map(|c| c.kind).collect();
        covered.extend(excluded(lane).iter().copied());
        let all: BTreeSet<GapKind> = GapKind::ALL.into_iter().collect();
        assert_eq!(covered, all, "a gap kind has neither a fixture nor an exclusion");
    }
}

#[test]
fn every_gap_kind_has_a_reason_a_finding_can_quote() {
    for kind in GapKind::ALL {
        assert!(kind.reason().len() > 10, "{kind:?} has no reason");
    }
}
