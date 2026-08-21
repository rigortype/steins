//! The declared-receiver lane (ADR-0049 §8 / S6): member absence on a receiver the
//! contract layer declares, routed by the minimum stratum of the declaration, so a
//! docblock claim never manufactures a proof-layer finding.

use steins_contract::{ContractTy, normalize};
use steins_syntax::{CallExpr, Callee, ClassDecl, Receiver};

use crate::contract::{IsA, ProjectIsa};
use crate::cx::Cx;
use crate::env::{ContractArm, Store, Stratum};
use crate::project::{Diagnostic, Res};
use crate::{CALL_UNDEFINED_METHOD_ID, Folder, PHPDOC_UNDEFINED_METHOD_ID};
use crate::absence::{ChainWalk, UndefKind, enumerate_method_chain, magic_obstacles_in_reach};

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

/// Whether a descendant declaration could **introduce** `method` (or hide an
/// obstacle to it) below a member whose own chain already lacks it (ADR-0049
/// §8): declares the method, uses a trait, is an enum (A3, methods
/// unlowered), carries `__call`, or is in reach of a magic-member docblock
/// tag (A14). Any such descendant fails the absence claim (silence).
fn descendant_introduces_method(cx: &Cx, cd: &ClassDecl, method: &str) -> bool {
    cd.is_enum
        || cd.is_trait
        || cd.uses_traits
        || cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case("__call"))
        || cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(method))
        || !magic_obstacles_in_reach(cx, &cd.fqn).is_empty()
}

/// Run the full §8 ladder for one narrowed contract arm and return its display
/// simple-name when the method is **provably absent** across the arm's whole
/// hierarchy *and* its complete descendant set, or `None` when any leg fails
/// (silence). Instance calls only — the declared-receiver lane is `$var->m()`.
fn arm_provably_lacks_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    arm_fqn: &str,
    method: &str,
) -> Option<String> {
    // §4 chain closure over the arm's own ancestor chain (reuses S2's walk).
    let ChainWalk::Absent { simple_chain, fqns, any_conditional } =
        enumerate_method_chain(cx, arm_fqn, method, UndefKind::Instance)
    else {
        return None;
    };
    // A2i: a conditional declaration in the chain re-dams the claim.
    if any_conditional && !cx.dam.is_clear() {
        return None;
    }
    // A2ii homonym: every chain FQN must be answered NOT-present by the boot surface.
    for fqn in &fqns {
        if folder.boot_surface_class_like(fqn) != Some(false) {
            return None;
        }
    }
    // Descendant closure (A4): the arm is final-immune, or its descendant set
    // is fully enumerated AND the dam is clear (an `eval`-minted subclass),
    // and no descendant introduces the method.
    match descendant_closure(cx, arm_fqn) {
        DescendantClosure::Immune => {}
        DescendantClosure::Obstacle => return None,
        DescendantClosure::Enumerated(descendants) => {
            if !cx.dam.is_clear() {
                return None; // eval could mint a subclass carrying the method.
            }
            // ADR-0079 §2.5 needs no leg here: a member-incomplete descendant
            // implies an unparsable non-vendor file, itself a dam site already
            // caught above (revisit if §3 ever allows unparsable-without-dam).
            for (_, dcd) in &descendants {
                if descendant_introduces_method(cx, dcd, method) {
                    return None;
                }
                // A homonym descendant may be dead code shadowed by a loaded class.
                if folder.boot_surface_class_like(&dcd.fqn) != Some(false) {
                    return None;
                }
            }
        }
    }
    Some(simple_chain.first().cloned().unwrap_or_else(|| arm_fqn.to_owned()))
}

/// Read a narrowed contract-arm lane as the declared-receiver lane's
/// **conjunct lists** — one inner list per arm, holding the class FQNs a
/// receiver of that arm must satisfy *all* of — or `None` when the lane is
/// out of scope (silence).
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
/// [`Cx::final_keyword`], never assumed: under [`FinalKeyword::Stripped`]
/// the emptiness leg doesn't run and members are looked up as the union.
///
/// [`FinalKeyword::Stripped`]: steins_contract::normalize::FinalKeyword::Stripped
pub(crate) fn declared_receiver_conjuncts(cx: &Cx, arms: &[ContractArm]) -> Option<Vec<Vec<String>>> {
    let oracle = ProjectIsa { cx, demote_catalog: cx.a11_demote_catalog() };
    let mut lane: Vec<Vec<String>> = Vec::with_capacity(arms.len());
    for a in arms {
        match &a.ty {
            ContractTy::Class(f) => lane.push(vec![f.clone()]),
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
                if normalize::provably_uninhabited(&a.ty, &oracle, cx.final_keyword) {
                    return None;
                }
                lane.push(conjuncts);
            }
            _ => return None,
        }
    }
    (!lane.is_empty()).then_some(lane)
}

/// Run the ADR-0049 §8 ladder for one `$var->method()` and emit the
/// declared-receiver finding iff the receiver's narrowed contract-arm lane
/// consists entirely of class arms that **each** provably lack the method
/// under descendant closure. Any leg failure on any arm is silence; the id
/// itself is [`declared_receiver_id`]'s call (A13 routing, see above).
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
    // A13: minimum over the participating (post-narrowing) arms, computed
    // here so it can never drift from the arms the claim rests on.
    let id = declared_receiver_id(arms);
    // A9 (monkey-patch) + A2ii: without a live sidecar, or with a
    // runtime-redefinition extension loaded, the id is silent.
    if !folder.absence_family_available() {
        return;
    }
    // Every arm — and for an intersection, every CONJUNCT — must provably
    // lack the method: member lookup over an inhabited intersection is the
    // union of the arms (issue #234), so only absence from all conjuncts
    // counts. `arm_provably_lacks_method` returns `None` both for "method is
    // there" and "a leg couldn't close", covering both rules in one fold.
    let mut arm_names: Vec<String> = Vec::with_capacity(lane.len());
    for conjuncts in &lane {
        let mut names: Vec<String> = Vec::with_capacity(conjuncts.len());
        for f in conjuncts {
            match arm_provably_lacks_method(cx, folder, f, &method) {
                Some(name) => names.push(name),
                None => return, // any conjunct not provably-absent ⇒ silence.
            }
        }
        arm_names.push(names.join("&"));
    }

    let pos = cx.tree().position(call.span.start);
    let arms_disp = arm_names.join("|");
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
