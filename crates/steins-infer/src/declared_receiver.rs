//! The declared-receiver lane (ADR-0049 §8 / S6): member absence on a receiver the
//! contract layer declares, routed by the minimum stratum of the declaration, so a
//! docblock claim never manufactures a proof-layer finding.

use steins_contract::{ContractTy, normalize};
use steins_syntax::{CallExpr, Callee, ClassDecl, Receiver};

use crate::contract::{IsA, ProjectIsa};
use crate::cx::Cx;
use crate::early_binding::redammed;
use crate::env::{ArmGuard, ContractArm, Store, Stratum};
use crate::project::{Diagnostic, Res};
use crate::fold::Folder;
use crate::{CALL_UNDEFINED_METHOD_ID, PHPDOC_UNDEFINED_METHOD_ID};
use crate::absence::{ChainWalk, magic_obstacles_in_reach, walk_method_chain};

// ---------------------------------------------------------------------------
// The declared-receiver lane (ADR-0049 §8 / S6), routed by minimum stratum
// (ADR-0049 A13): `call.undefined-method` when every arm is `Verified`,
// `phpdoc.undefined-method` when any arm is `Asserted`.
//
// S2 fires on a proven-exact receiver (`class_exact`); S6 fires on a receiver
// whose *declared* type — native `C $o`, phpdoc `@param User|Guest`, narrowed
// by branch analysis (N4) to a surviving contract-arm list — provably lacks
// the method under a stricter ladder ("conditional is not enough", §8): each
// arm must clear the §4 chain legs AND **descendant closure** (a subclass,
// incl. `eval`-minted, could define the method). A13 routes the id: a native
// declaration is runtime-enforced (`Verified`), a docblock claim is not
// (`Asserted`) — ADR-0052 N2's minimum over the lane decides, all `Verified`
// → [`CALL_UNDEFINED_METHOD_ID`] (same id/claim as S2, ADR-0022), any
// `Asserted` → [`PHPDOC_UNDEFINED_METHOD_ID`]. No id renamed or added.
//
// Disjointness from S2 is over sites (S2 owns `class_exact`, S6 requires
// NOT-exact), so the two never double-report. The Asserted half accepts
// Asserted premises (ADR-0052 §5, e.g. a `@param` refinement); both halves
// respect `absence_family_available` and the A11 version-skew demotion of
// descendant closure.
// ---------------------------------------------------------------------------

/// The `(receiver-var, method)` an S6 claim can rest on, or `None` when out
/// of scope (silence). Only a plain `$var->method(...)` qualifies — `?->`,
/// static/`$this`/`new`/dynamic forms, and the first-class-callable shape
/// are excluded exactly as S2 excludes them.
fn phpdoc_undefined_method_receiver(call: &CallExpr) -> Option<(String, String)> {
    // Leg (l): the first-class-callable form builds a Closure, never a call.
    if !call.positional_only && call.args.is_empty() {
        return None;
    }
    match &call.receiver {
        Callee::Method { receiver: Receiver::Var(v), method, nullsafe: false } => {
            Some((v.clone(), method.clone()))
        }
        _ => None,
    }
}

/// The project-wide descendant enumeration of a union member (ADR-0049 §8 / A4).
pub(crate) enum DescendantClosure<'a> {
    /// `final` (or an enum): no subclass can exist, so the arm is immune —
    /// no descendant scan, no dam needed.
    Immune,
    /// Descendant declarations are **completely enumerated**: every declared
    /// class either provably is-a the member (collected here) or provably is
    /// not, over both halves of an Ambiguous FQN with alias-edge parent
    /// matching. Still requires the dam clear (an `eval`-minted subclass)
    /// before it closes.
    Enumerated(Vec<(usize, &'a ClassDecl)>),
    /// Tainted — Unknown ⇒ silence: an anonymous class could extend the
    /// member (invisible to the index), a candidate's is-a is Unknown
    /// (incomplete hierarchy), the member is Ambiguous/absent, or a
    /// catalog-backed verdict is demoted under a PHP-minor skew (A11).
    Obstacle,
}

/// Whether declaration `cd` (in file `file`) provably **is-a** `target`
/// (lowercase FQN), walking its own inheritance edges directly rather than
/// the deduped index, so an Ambiguous declaration still counts (A4). A
/// direct edge to the *same index site* as `target` counts as `Yes`, folding
/// literal `class_alias` edges into parent matching. Deeper hops defer to
/// the trinary [`Cx::is_a`] oracle, whose `Unknown` taints the enumeration.
fn decl_is_a(cx: &Cx, file: usize, cd: &ClassDecl, target: &str) -> IsA {
    let tree = &cx.units[file].tree;
    let arm_site = match cx.index.resolve_class(target) {
        Res::Unique(s) => Some(s),
        _ => None,
    };
    let mut edges: Vec<String> = Vec::new();
    if let Some(p) = &cd.parent {
        edges.push(tree.resolve_class_fqn(p));
    }
    for i in &cd.implements {
        edges.push(tree.resolve_class_fqn(i));
    }
    if cd.is_enum {
        edges.push("UnitEnum".to_owned());
        if cd.enum_backing.is_some() {
            edges.push("BackedEnum".to_owned());
        }
    }
    let mut any_unknown = false;
    for e in &edges {
        let en = e.trim_start_matches('\\');
        if en.eq_ignore_ascii_case(target) {
            return IsA::Yes;
        }
        // Alias-edge / site-identity parent match (A4).
        if let (Some(a), Res::Unique(es)) = (arm_site, cx.index.resolve_class(en))
            && es == a
        {
            return IsA::Yes;
        }
        match cx.is_a(en, target) {
            IsA::Yes => return IsA::Yes,
            IsA::Unknown => any_unknown = true,
            IsA::No => {}
        }
    }
    if any_unknown { IsA::Unknown } else { IsA::No }
}

/// Enumerate the project-wide descendant set of a union member (ADR-0049 §8 / A4).
/// A query-style whole-universe function (ADR-0048): recomputed per run, no ordering
/// dependence. See [`DescendantClosure`].
pub(crate) fn descendant_closure<'a>(cx: &Cx<'a>, arm_fqn: &str) -> DescendantClosure<'a> {
    // Must resolve Unique — an Ambiguous/absent member cannot be closed.
    let Some((_, arm_cd)) = cx.find_class(arm_fqn) else {
        return DescendantClosure::Obstacle;
    };
    // `final` or an enum has no subclass — extending it is fatal — so the arm
    // is immune (A9 already gated finality via `absence_family_available`).
    if arm_cd.is_final || arm_cd.is_enum {
        return DescendantClosure::Immune;
    }
    // A11: a PHP-minor skew can fake a catalog-backed is-a edge, so descendant
    // closure demotes to Unknown (blanket v1) — silence, never wrong narrowing.
    if cx.a11_demote_catalog() {
        return DescendantClosure::Obstacle;
    }
    // A4: an anon class is invisible to the index, so any one whose
    // extends/implements edge could reach the member taints closure.
    for unit in cx.units {
        for edge in unit.tree.anonymous_class_edges() {
            let refs = edge.parent.iter().chain(edge.implements.iter());
            for r in refs {
                let efqn = unit.tree.resolve_class_fqn(r);
                let en = efqn.trim_start_matches('\\');
                if en.eq_ignore_ascii_case(arm_fqn) {
                    return DescendantClosure::Obstacle;
                }
                // is-a-or-Unknown ⇒ a possible invisible descendant.
                match cx.is_a(en, arm_fqn) {
                    IsA::Yes | IsA::Unknown => return DescendantClosure::Obstacle,
                    IsA::No => {}
                }
            }
        }
    }
    // Over ALL declarations, not the deduped index — both halves of an
    // Ambiguous FQN count (A4). One Unknown candidate taints the whole closure.
    let mut descendants: Vec<(usize, &'a ClassDecl)> = Vec::new();
    for (fi, unit) in cx.units.iter().enumerate() {
        for cd in unit.tree.classes() {
            if cd.fqn.eq_ignore_ascii_case(arm_fqn) {
                continue; // the member itself (Unique — resolved above).
            }
            match decl_is_a(cx, fi, cd, arm_fqn) {
                IsA::Yes => descendants.push((fi, cd)),
                IsA::Unknown => return DescendantClosure::Obstacle,
                IsA::No => {}
            }
        }
    }
    DescendantClosure::Enumerated(descendants)
}


/// What a ladder established about one member on one class-like, on one candidate of
/// an arm, or on a whole arm: the four outcomes of a chain walk ([`ChainWalk`]) after
/// its A2i/A2ii legs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Held {
    /// Every chain is closed and lacks both the member and the magic fallback.
    Absent,
    /// The member is declared on every chain.
    Declared,
    /// Every chain is closed, lacks the member and holds the magic fallback.
    Magic,
    /// A leg refused, or the chains disagree: nothing is proven.
    Unknown,
}

/// Apply the A2i re-dam and the A2ii boot-surface homonym legs to a walk, keeping
/// its verdict only when both clear.
fn walked(cx: &Cx, folder: &mut dyn Folder, walk: &ChainWalk) -> Held {
    let (verdict, fqns, any_conditional) = match walk {
        ChainWalk::Absent { fqns, any_conditional, .. } => (Held::Absent, fqns, *any_conditional),
        ChainWalk::Declared { fqns, any_conditional } => (Held::Declared, fqns, *any_conditional),
        ChainWalk::Magic { fqns, any_conditional } => (Held::Magic, fqns, *any_conditional),
        ChainWalk::Unknown => return Held::Unknown,
    };
    // A2i: a conditional declaration in the chain re-dams the claim.
    if redammed(cx, any_conditional) {
        return Held::Unknown;
    }
    // A2ii homonym: every chain FQN must be answered NOT-present by the boot surface.
    if fqns.iter().all(|fqn| folder.boot_surface_class_like(fqn) == Some(false)) {
        verdict
    } else {
        Held::Unknown
    }
}

/// One candidate's chain walk (issue #1092): a magic-member docblock tag in the reach
/// of `name` (A14) is `Unknown` whatever the question, then [`walk_method_chain`] with
/// the caller's magic fallback.
fn candidate_walk(
    cx: &Cx,
    file: usize,
    cd: &ClassDecl,
    name: &str,
    method: &str,
    magic: Option<&str>,
) -> ChainWalk {
    if !magic_obstacles_in_reach(cx, name).is_empty() {
        return ChainWalk::Unknown;
    }
    walk_method_chain(cx, file, cd, name, method, magic)
}

/// One descendant of an arm, judged by its **whole chain** (issue #1092): an
/// ancestor outside the arm's hierarchy that declares the method, or holds the magic
/// fallback the caller asks about, counts, as it does at runtime. A magic-member
/// docblock tag in its reach (A14), an enum (A3) or a trait, a trait composition that
/// declines, and any obstacle on the chain are `Unknown`.
fn descendant_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    file: usize,
    cd: &ClassDecl,
    method: &str,
    magic: Option<&str>,
) -> Held {
    let walk = candidate_walk(cx, file, cd, &cd.fqn, method, magic);
    walked(cx, folder, &walk)
}

/// The walk of the class-like `fqn` names, under the A14/A2i/A2ii legs, or `Unknown`
/// when the name does not resolve to one project declaration.
fn named_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    fqn: &str,
    method: &str,
    magic: Option<&str>,
) -> Held {
    match cx.find_class(fqn) {
        Some((file, cd)) => walked(cx, folder, &candidate_walk(cx, file, cd, fqn, method, magic)),
        None => Held::Unknown,
    }
}

/// The magic fallback a call `$var->m()` honours, and `is_callable([$var, 'm'])` with it.
pub(crate) const CALL_FALLBACK: Option<&str> = Some("__call");

/// Run the full §8 ladder for one class-like of a narrowed contract arm, with no
/// guard read. The candidates are the class-like and its complete descendant set;
/// the verdict is [`Held::Absent`] when every candidate's chain lacks the method and
/// the `magic` fallback. Instance calls only — the declared-receiver lane is
/// `$var->m()`, which passes [`CALL_FALLBACK`]; `method_exists` passes `None`, since
/// it reports declared methods only and a `__call` never makes it true.
fn arm_member_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    arm_fqn: &str,
    method: &str,
    magic: Option<&str>,
) -> Held {
    // §4 chain closure over the arm's own ancestor chain (S2's walk and legs).
    let anchor = named_verdict(cx, folder, arm_fqn, method, magic);
    match anchor {
        Held::Unknown => return Held::Unknown,
        // Every inhabitant of the arm is-a the arm, and so holds what its chain
        // declares: no descendant can take the method away.
        Held::Declared => return Held::Declared,
        Held::Absent | Held::Magic => {}
    }
    // Descendant closure (A4): the arm is final-immune, or its descendant set
    // is fully enumerated AND the dam is clear (an `eval`-minted subclass),
    // and every descendant's chain agrees with the arm's.
    match descendant_closure(cx, arm_fqn) {
        DescendantClosure::Immune => {}
        DescendantClosure::Obstacle => return Held::Unknown,
        DescendantClosure::Enumerated(descendants) => {
            if !cx.dam.is_clear() {
                return Held::Unknown; // eval could mint a subclass carrying the method.
            }
            for (dfile, dcd) in descendants.iter().filter(|(_, d)| instances_count(d, magic)) {
                if descendant_verdict(cx, folder, *dfile, dcd, method, magic) != anchor {
                    return Held::Unknown;
                }
            }
        }
    }
    anchor
}

/// One arm of the declared-receiver lane: the class FQNs a receiver of the arm
/// satisfies all of, and the positive class guards that passed over the arm on this
/// path ([`ContractArm::guards`]).
pub(crate) struct LaneArm<'s> {
    pub(crate) conjuncts: Vec<String>,
    pub(crate) guards: &'s [ArmGuard],
}

/// Read a narrowed contract-arm lane as the declared-receiver lane's arms — per
/// arm, the **conjunct list** of class FQNs a receiver of that arm must satisfy
/// *all* of, with the arm's guards — or `None` when the lane is out of scope
/// (silence).
///
/// Three arm shapes, and only three (issue #238's intersection consumption):
/// `Class(f)` (one conjunct, pre-#238 behaviour); `Inter([Class, …])` (the
/// conjuncts of a declared `Foo&Bar` receiver); anything else — silence (a
/// scalar/array/null arm means the receiver may not be an object; an
/// intersection with a non-class member, `Foo&callable` or a template arm,
/// names a constraint this lane cannot close over).
///
/// **#234: an uninhabited arm is SILENCE.** An intersection the posture
/// proves empty ([`normalize::provably_uninhabited`]) takes the whole lane
/// out — no value inhabits it, so every claim about it is vacuous. FP-safe:
/// `final Svc & MockObject` naturally collapses to nothing, and a
/// no-conjunct lane would make every method call on it provably-absent —
/// under `dg/bypass-finals`, where the mock subclass genuinely exists, a
/// false positive on the proof layer (ADR-0049 A13). Posture is read from
/// the context's [`RuntimePostures::final_keyword`], never assumed: under
/// [`FinalKeyword::Stripped`] the emptiness leg doesn't run and members are
/// looked up as the union.
///
/// [`RuntimePostures::final_keyword`]: crate::RuntimePostures::final_keyword
/// [`FinalKeyword::Stripped`]: steins_contract::normalize::FinalKeyword::Stripped
pub(crate) fn declared_receiver_conjuncts<'s>(
    cx: &Cx,
    arms: &'s [ContractArm],
) -> Option<Vec<LaneArm<'s>>> {
    let oracle = ProjectIsa { cx, demote_catalog: cx.a11_demote_catalog() };
    let lane = arms.iter().map(|a| lane_arm(cx, &oracle, a)).collect::<Option<Vec<_>>>()?;
    (!lane.is_empty()).then_some(lane)
}

/// One arm of [`declared_receiver_conjuncts`], or `None` when the lane cannot read it.
fn lane_arm<'s>(cx: &Cx, oracle: &ProjectIsa, a: &'s ContractArm) -> Option<LaneArm<'s>> {
    let conjuncts = match &a.ty {
        ContractTy::Class(f) => vec![f.clone()],
        ContractTy::Inter(members) => {
            // A member this lane cannot close over refuses the whole arm.
            let mut conjuncts: Vec<String> = Vec::with_capacity(members.len());
            for m in members {
                let ContractTy::Class(f) = m else { return None };
                conjuncts.push(f.clone());
            }
            if conjuncts.is_empty() {
                return None;
            }
            // #234: an arm no value can inhabit is not a receiver.
            if normalize::provably_uninhabited(&a.ty, oracle, cx.postures.final_keyword) {
                return None;
            }
            conjuncts
        }
        _ => return None,
    };
    Some(LaneArm { conjuncts, guards: &a.guards })
}

/// The member questions [`judge_arm`] asks, one implementation per member kind: the
/// method lane here, the property lane in `absence.rs`.
pub(crate) trait MemberLadder {
    /// The unguarded §8 verdict of one conjunct: its own chain and its whole
    /// descendant closure, each candidate judged by its chain.
    fn conjunct(&mut self, cx: &Cx, fqn: &str) -> Held;
    /// Whether the class-like `fqn` names provably holds the member on its own chain,
    /// so every value that is-a it holds it.
    fn declares(&mut self, cx: &Cx, fqn: &str) -> bool;
    /// Whether the legs that judge a name rather than a chain clear on the anchor
    /// (A14 reach, A2i, A2ii), so its descendants may speak for it.
    fn anchor_clear(&mut self, cx: &Cx, fqn: &str) -> bool;
    /// One candidate declaration, judged by its full chain.
    fn candidate(&mut self, cx: &Cx, file: usize, cd: &ClassDecl) -> Held;
    /// Whether an admitted candidate's verdict is read at all: one with no instances of
    /// its own may be passed over when its concrete descendants are candidates too.
    fn speaks_for(&self, _cd: &ClassDecl) -> bool {
        true
    }
}

/// What the lane established about one arm (issue #1095).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArmFate {
    /// The guards admit no candidate of the arm, in a closed universe: no value
    /// reaches this site through it, so the arm drops out of the union.
    Dead,
    /// The arm has inhabitants here; what they hold.
    Live(Held),
}

/// One judged arm: its fate, its declared spelling (`A`, `A&B`), and whether a
/// candidate was excluded by an `Asserted` guard alone, so the verdict rests on a
/// docblock claim (ADR-0049 A13).
pub(crate) struct ArmJudgement {
    pub(crate) fate: ArmFate,
    pub(crate) display: String,
    pub(crate) rests_on_assert: bool,
}

/// The declared spelling of one conjunct: the declaration's own name, or the last
/// segment of a name no project file declares, in its builtin casing.
fn conjunct_display(cx: &Cx, fqn: &str) -> String {
    match cx.find_class(fqn) {
        Some((_, cd)) => cd.name.clone(),
        None => {
            let display = cx.class_display_fqn(fqn);
            display.rsplit('\\').next().unwrap_or(&display).to_owned()
        }
    }
}

/// Whether declaration `cd` (in `file`) is-a `target`, itself included: the name, its
/// identity (a literal `class_alias`), the implicit `Stringable` a `__toString` gives,
/// then its own edges ([`decl_is_a`]).
fn candidate_is_a(cx: &Cx, file: usize, cd: &ClassDecl, target: &str) -> IsA {
    let target = target.trim_start_matches('\\');
    if cd.fqn.eq_ignore_ascii_case(target)
        || cx.class_identity(&cd.fqn).eq_ignore_ascii_case(&cx.class_identity(target))
    {
        return IsA::Yes;
    }
    match decl_is_a(cx, file, cd, target) {
        IsA::No if cx.class_identity(target) == "stringable" => {
            match cx.held_method(file, cd, "__toString") {
                Ok(Some(_)) => IsA::Yes,
                Ok(None) => IsA::No,
                Err(_) => IsA::Unknown,
            }
        }
        verdict => verdict,
    }
}

/// The strongest stratum among the guards that provably exclude a candidate, or
/// `None` when every guard may hold on it (an `Unknown` is-a admits).
fn excluded_by(guards: &[ArmGuard], is_a: impl Fn(&str) -> IsA) -> Option<Stratum> {
    let mut by: Option<Stratum> = None;
    for g in guards {
        if is_a(&g.fqn) == IsA::No {
            by = Some(match (by, g.stratum) {
                (Some(Stratum::Verified), _) | (_, Stratum::Verified) => Stratum::Verified,
                _ => Stratum::Asserted,
            });
        }
    }
    by
}

/// Judge one arm of the declared-receiver lane against its guards (issue #1095).
///
/// The **declared type is its own witness**: a value spelled by the arm is admitted
/// when every guard may hold on some conjunct, and the arm is then read as before,
/// conjunct by conjunct over each one's whole descendant closure. Otherwise the
/// **anchor**, the first conjunct whose descendant set is closed, supplies the
/// candidates: its descendants that may be every other conjunct and every guard. None
/// admitted, in a clear universe, is [`ArmFate::Dead`]; a member some conjunct or
/// guard declares is held by every candidate; else each candidate is judged by its
/// full chain, so a member inherited from an intermediate class the guard excludes
/// still counts.
pub(crate) fn judge_arm(cx: &Cx, arm: &LaneArm, ladder: &mut dyn MemberLadder) -> ArmJudgement {
    let display: Vec<String> = arm.conjuncts.iter().map(|c| conjunct_display(cx, c)).collect();
    let judged = |fate: ArmFate, rests_on_assert: bool| ArmJudgement {
        fate,
        display: display.join("&"),
        rests_on_assert,
    };
    let declared_by = excluded_by(arm.guards, |g| {
        let mut any_unknown = false;
        for c in &arm.conjuncts {
            match cx.is_a(c, g) {
                IsA::Yes => return IsA::Yes,
                IsA::Unknown => any_unknown = true,
                IsA::No => {}
            }
        }
        if any_unknown { IsA::Unknown } else { IsA::No }
    });
    let Some(anchor_excluded_by) = declared_by else {
        return judged(ArmFate::Live(unguarded_verdict(cx, arm, ladder)), false);
    };
    // A11: a PHP-minor skew can fake a catalog-backed is-a edge, so an exclusion
    // proves nothing.
    if cx.a11_demote_catalog() || !cx.dam.is_clear() {
        return judged(ArmFate::Live(Held::Unknown), false);
    }
    let Some((anchor, descendants)) = guarded_candidates(cx, arm) else {
        return judged(ArmFate::Live(Held::Unknown), false);
    };
    if !ladder.anchor_clear(cx, anchor) {
        return judged(ArmFate::Live(Held::Unknown), false);
    }
    let mut rests_on_assert = anchor_excluded_by == Stratum::Asserted;
    let mut admitted: Vec<(usize, &ClassDecl)> = Vec::new();
    for (file, cd) in descendants {
        match excluded_by(arm.guards, |g| candidate_is_a(cx, file, cd, g)) {
            None => admitted.push((file, cd)),
            Some(Stratum::Asserted) => rests_on_assert = true,
            Some(Stratum::Verified) => {}
        }
    }
    if admitted.is_empty() {
        return judged(ArmFate::Dead, rests_on_assert);
    }
    let guard_names = arm.guards.iter().map(|g| g.fqn.as_str());
    if arm.conjuncts.iter().map(String::as_str).chain(guard_names).any(|n| ladder.declares(cx, n)) {
        return judged(ArmFate::Live(Held::Declared), rests_on_assert);
    }
    let mut fold: Option<Held> = None;
    for (file, cd) in admitted {
        if !ladder.speaks_for(cd) {
            continue;
        }
        let held = ladder.candidate(cx, file, cd);
        if held == Held::Unknown || fold.is_some_and(|f| f != held) {
            return judged(ArmFate::Live(Held::Unknown), rests_on_assert);
        }
        fold = Some(held);
    }
    judged(ArmFate::Live(fold.unwrap_or(Held::Unknown)), rests_on_assert)
}

/// The anchor of a guarded arm and its descendants that may be every other conjunct
/// (an `Unknown` is-a admits), or `None` when no conjunct's descendant set closes.
fn guarded_candidates<'a, 'n>(
    cx: &Cx<'a>,
    arm: &'n LaneArm,
) -> Option<(&'n str, Vec<(usize, &'a ClassDecl)>)> {
    for (i, anchor) in arm.conjuncts.iter().enumerate() {
        let descendants = match descendant_closure(cx, anchor) {
            DescendantClosure::Obstacle => continue,
            DescendantClosure::Immune => Vec::new(),
            DescendantClosure::Enumerated(ds) => ds,
        };
        let others = || arm.conjuncts.iter().enumerate().filter(move |(j, _)| *j != i);
        let inhabitants = descendants
            .into_iter()
            .filter(|(file, cd)| {
                others().all(|(_, c)| candidate_is_a(cx, *file, cd, c) != IsA::No)
            })
            .collect();
        return Some((anchor, inhabitants));
    }
    None
}

/// An arm the guards do not exclude, read conjunct by conjunct as the unguarded lane
/// reads it: member lookup over an intersection is the union of its conjuncts (issue
/// #234), so one that declares the member is enough, and every conjunct must close
/// for absence. A conjunct no project file declares (a builtin interface) has no
/// closure to read: it is passed over when the arm has a project inhabitant whose
/// own chain speaks for it, and makes the arm `Unknown` otherwise.
fn unguarded_verdict(cx: &Cx, arm: &LaneArm, ladder: &mut dyn MemberLadder) -> Held {
    let mut opaque = false;
    let mut declared = false;
    let mut magic = false;
    for c in &arm.conjuncts {
        if arm.conjuncts.len() > 1 && cx.class_absent(c) {
            opaque = true;
            continue;
        }
        match ladder.conjunct(cx, c) {
            Held::Unknown => return Held::Unknown,
            Held::Declared => declared = true,
            Held::Magic => magic = true,
            Held::Absent => {}
        }
    }
    if declared {
        return Held::Declared;
    }
    if opaque && !has_project_inhabitant(cx, arm) {
        return Held::Unknown;
    }
    if magic { Held::Magic } else { Held::Absent }
}

/// Whether some project declaration inhabits every conjunct of `arm` (an `Unknown`
/// is-a counts), read off the first project conjunct's closed descendant set.
fn has_project_inhabitant(cx: &Cx, arm: &LaneArm) -> bool {
    let unguarded = LaneArm { conjuncts: arm.conjuncts.clone(), guards: &[] };
    guarded_candidates(cx, &unguarded).is_some_and(|(_, inhabitants)| !inhabitants.is_empty())
}

/// The method questions of [`judge_arm`]: `method` under the `magic` fallback.
struct MethodLadder<'f, 'm> {
    folder: &'f mut dyn Folder,
    method: &'m str,
    magic: Option<&'m str>,
}

impl MemberLadder for MethodLadder<'_, '_> {
    fn conjunct(&mut self, cx: &Cx, fqn: &str) -> Held {
        arm_member_verdict(cx, self.folder, fqn, self.method, self.magic)
    }
    fn declares(&mut self, cx: &Cx, fqn: &str) -> bool {
        named_verdict(cx, self.folder, fqn, self.method, self.magic) == Held::Declared
    }
    fn anchor_clear(&mut self, cx: &Cx, fqn: &str) -> bool {
        named_verdict(cx, self.folder, fqn, self.method, self.magic) != Held::Unknown
    }
    fn candidate(&mut self, cx: &Cx, file: usize, cd: &ClassDecl) -> Held {
        descendant_verdict(cx, self.folder, file, cd, self.method, self.magic)
    }
    fn speaks_for(&self, cd: &ClassDecl) -> bool {
        instances_count(cd, self.magic)
    }
}

/// Whether a descendant candidate's own chain verdict is read. Under `method_exists`
/// (no magic fallback) an abstract class or an interface below the anchor is passed
/// over (issue #1103's review): it has no instances of its own, every concrete class
/// that is-a it is a candidate in its own right, and its interface leg, which can reach
/// a builtin interface the walk cannot enumerate, would otherwise leave the whole arm
/// `Unknown`. The call lanes keep reading every candidate, as before.
fn instances_count(cd: &ClassDecl, magic: Option<&str>) -> bool {
    magic.is_some() || !(cd.is_abstract || cd.is_interface)
}

/// Run the ADR-0049 §8 ladder for one `$var->method()` and emit the
/// declared-receiver finding iff every arm of the receiver's narrowed contract-arm
/// lane the guards leave alive **provably lacks** the method under descendant
/// closure. Any leg failure on any live arm is silence; the id itself is
/// [`declared_receiver_id`]'s call (A13 routing, see above).
pub(crate) fn check_phpdoc_undefined_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    if poisoned {
        return;
    }
    let Some((var, method)) = phpdoc_undefined_method_receiver(call) else {
        return;
    };
    // Disjointness with S2: an exact receiver is S2's, never S6's.
    if store.is_exact(&var) {
        return;
    }
    // The narrowed declared-type arm lane (N4's accessor). No lane ⇒ nothing
    // declared to close over.
    let Some(arms) = store.contract_arms(&var) else {
        return;
    };
    if arms.is_empty() {
        return;
    }
    // Every surviving arm must be a class/interface arm, or an intersection
    // of them (issue #238); a scalar/array/null arm means the runtime
    // receiver may be a non-object, so absence doesn't hold here — silence.
    let Some(lane) = declared_receiver_conjuncts(cx, arms) else {
        return;
    };
    // A9 (monkey-patch) + A2ii: without a live sidecar, or with a
    // runtime-redefinition extension loaded, the id is silent.
    if !folder.absence_family_available() {
        return;
    }
    let mut ladder = MethodLadder { folder, method: &method, magic: CALL_FALLBACK };
    let Some(absence) = lane_lacks_member(cx, &lane, &mut ladder) else {
        return;
    };
    // A13: minimum over the participating (post-narrowing) arms, computed
    // here so it can never drift from the arms the claim rests on; a dead arm or a
    // candidate only a positive `@phpstan-assert` excluded leaves the claim resting
    // on that docblock.
    let id = if absence.rests_on_assert {
        PHPDOC_UNDEFINED_METHOD_ID
    } else {
        declared_receiver_id(arms)
    };

    let pos = cx.tree().position(call.span.start);
    let arms_disp = absence.arms.join("|");
    let message = format!(
        "call to undefined method {arms_disp}::{method}() — declared receiver ${var} narrowed to {{{arms_disp}}}, \
         hierarchy and descendants fully enumerated, no __call, no @method/@property/@mixin"
    );
    out.push(Diagnostic {
        id,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}

/// What [`lane_lacks_member`] proved: the display names of the live arms, and
/// whether any judged arm rests on an `Asserted` guard.
pub(crate) struct LaneAbsence {
    pub(crate) arms: Vec<String>,
    pub(crate) rests_on_assert: bool,
}

/// The live arms of `lane` when **every** arm the guards leave alive provably lacks
/// the member and at least one is alive, or `None` (issue #1095). A dead arm drops out
/// of the union; any other verdict on a live arm — declared, the magic fallback, a
/// leg that couldn't close — is silence.
///
/// The one ladder both readers of the declared-receiver method lane share: the
/// finding ([`check_phpdoc_undefined_method`]) and the `method_exists`/`is_callable`
/// fold ([`declared_receiver_lacks_method`]), so a guard is dead exactly where the
/// lane would have reported inside it.
pub(crate) fn lane_lacks_member(
    cx: &Cx,
    lane: &[LaneArm],
    ladder: &mut dyn MemberLadder,
) -> Option<LaneAbsence> {
    let mut absence = LaneAbsence { arms: Vec::with_capacity(lane.len()), rests_on_assert: false };
    for arm in lane {
        let judged = judge_arm(cx, arm, ladder);
        match judged.fate {
            ArmFate::Dead => {}
            ArmFate::Live(Held::Absent) => absence.arms.push(judged.display),
            ArmFate::Live(_) => return None,
        }
        absence.rests_on_assert |= judged.rests_on_assert;
    }
    (!absence.arms.is_empty()).then_some(absence)
}

/// Whether the declared-receiver lane of `$var` **proves** `method` absent, under the
/// ladder [`check_phpdoc_undefined_method`] reports with, restricted to a lane that
/// reports on the proof layer: not poisoned, not an exact receiver (that is the exact
/// lane's), a non-empty narrowed lane every arm of which is `Verified`, a lane the
/// declared-receiver reading admits, a verdict no `@phpstan-assert` guard carries, and
/// a live absence family.
///
/// A `method_exists($var, 'm')` or `is_callable([$var, 'm'])` over such a receiver is
/// provably false (issue #930), so the body it guards is dead. `magic` is the fallback
/// the predicate honours: `None` for `method_exists`, which a `__call` never makes true,
/// so a chain holding only `__call` lacks the method for it; [`CALL_FALLBACK`] for
/// `is_callable`, which a `__call` does make true.
pub(crate) fn declared_receiver_lacks_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    store: &Store,
    poisoned: bool,
    var: &str,
    method: &str,
    magic: Option<&str>,
) -> bool {
    if poisoned || store.is_exact(var) {
        return false;
    }
    let Some(arms) = store.contract_arms(var) else { return false };
    if arms.iter().any(|a| a.stratum != Stratum::Verified) {
        return false;
    }
    let Some(lane) = declared_receiver_conjuncts(cx, arms) else { return false };
    if !folder.absence_family_available() {
        return false;
    }
    let mut ladder = MethodLadder { folder, method, magic };
    lane_lacks_member(cx, &lane, &mut ladder).is_some_and(|a| !a.rests_on_assert)
}

/// Which arms of `arms` a `method_exists($var, 'm')` test proves no value of `$var`
/// reaches on the side where the call returned `holds` (issue #981), by the verdict of
/// the lane's ladder on each arm alone ([`judge_arm`], no magic fallback: a `__call`
/// never makes `method_exists` true).
///
/// Where the call held, an arm every inhabitant of which lacks `m` (`Absent`) cannot be
/// here; where it failed, an arm every inhabitant of which declares `m` (`Declared`)
/// cannot. A `Dead` arm, one the guards admit nothing of, is deleted on both sides. Any
/// other arm stays: an undecided verdict, a value that may not be an object (a scalar,
/// a `null`, a class-string), an arm the lane cannot read, and a verdict that rests on
/// a class only a `@phpstan-assert` excluded, which a native test must not turn into a
/// deletion. The caller checks the absence family and the receiver's exactness.
pub(crate) fn method_exists_dead_arms(
    cx: &Cx,
    folder: &mut dyn Folder,
    arms: &[ContractArm],
    method: &str,
    holds: bool,
) -> Vec<bool> {
    let oracle = ProjectIsa { cx, demote_catalog: cx.a11_demote_catalog() };
    let mut ladder = MethodLadder { folder, method, magic: None };
    arms.iter()
        .map(|a| {
            let Some(arm) = lane_arm(cx, &oracle, a) else { return false };
            let judged = judge_arm(cx, &arm, &mut ladder);
            if judged.rests_on_assert {
                return false;
            }
            match judged.fate {
                ArmFate::Dead => true,
                ArmFate::Live(Held::Absent) => holds,
                ArmFate::Live(Held::Declared) => !holds,
                ArmFate::Live(Held::Magic | Held::Unknown) => false,
            }
        })
        .collect()
}

/// The declared-receiver lane's id for a narrowed arm lane (ADR-0049 A13):
/// proof-layer [`CALL_UNDEFINED_METHOD_ID`] when EVERY arm is `Verified`
/// (native, PHP-enforced), contract-layer [`PHPDOC_UNDEFINED_METHOD_ID`] as
/// soon as one arm is `Asserted`. [`Stratum::min`] folded over the lane —
/// ADR-0052 N2's rule, order-independent (ADR-0048). An `Asserted` arm never
/// launders into `Verified` upstream ([`refine_declared_arms`]).
///
/// [`refine_declared_arms`]: crate::refine_declared_arms
fn declared_receiver_id(arms: &[ContractArm]) -> &'static str {
    let min = arms.iter().fold(Stratum::Verified, |acc, a| acc.min(a.stratum));
    match min {
        Stratum::Verified => CALL_UNDEFINED_METHOD_ID,
        Stratum::Asserted => PHPDOC_UNDEFINED_METHOD_ID,
    }
}
