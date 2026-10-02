//! The strict floor of the effect envelope check (ADR-0100): `effect.maybe-envelope-exceeded`.
//!
//! [`super::report_unit`] holds a declaration to its envelope with what the fixpoint
//! *proved*, and a site the lane cannot see is silent there: `{…?}` is not an
//! occurrence. This module reads the other half, the coverage gaps
//! ([`GapKind`]) behind that silence, so a declared-pure body that calls something
//! the analyzer cannot resolve is named at `strict` instead of being omitted.
//!
//! A **unit** is a declaration [`super::operative_bound`] builds a bound for: the
//! ⊤ envelope (a bare `@phpstan-impure`, `@phpstan-all-methods-impure`) bounds
//! nothing and is no unit (discharge 1). Within a unit:
//!
//! * **Direct.** One finding per own site and gap kind, from the same
//!   [`ResolvedSite`] [`super::report_site`] reads, less the gaps discharges 2, 3
//!   and 6 answer.
//! * **Inherited.** One finding per call edge that propagates the callee's `…?`
//!   (an untainting edge does not) into a project body that is itself `…?`, less
//!   discharges 4 and 6. A callee's discharges are relative to its envelope: a
//!   caller inherits them only through an envelope that fits its own, and never
//!   through a tag-flagged function whose contract the call does not decide.
//!
//! The exhaustiveness bit, the fixpoint and every writer are untouched: nothing
//! here feeds a row back.

use std::collections::{BTreeSet, HashMap, HashSet};

use steins_db::EffectsPolicy;
use steins_phpdoc::{PurityCondition, TagKind, scan_docblock};
use steins_syntax::{DynamicSite, EffectRecv, Param, SiteKind, SiteOrigin};

use super::{
    EffectSet, OperativeBound, interop_envelope, operative_bound, own_interop_envelope,
};
use crate::Sym;
use crate::cx::Cx;
use crate::dispatch::{Resolution, resolve_in_chain};
use crate::project::{Diagnostic, FileUnit, Index};
use crate::site::method::declared_receiver_fqn;
use crate::site::reach::Frame;
use crate::site::{GapKind, ResolvedSite, Target};
use crate::EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID;

/// What the floor knows of a declaration whose envelope bounds something.
pub(super) struct Enveloped {
    /// The labels of its operative bound (empty is the pure envelope): what a caller's
    /// own bound must admit for the callee's discharges to be the caller's (discharge 4).
    labels: Vec<String>,
    /// A free function flagged `@pure-unless-callable-is-impure` on a parameter that
    /// exists: its purity is conditional on the callable bound at the call.
    flagged: bool,
}

/// The declarations whose envelope bounds something, by symbol (discharge 4). A
/// unit's callees owe nothing to a caller that reaches them through an entry here
/// whose bound fits the caller's.
///
/// Closures are never in it: a closure carries no envelope, so a gap in one is
/// reported at the edge that reaches it.
pub(super) fn enveloped_syms(
    units: &[FileUnit],
    index: &Index,
    registry: &steins_catalog::LabelRegistry,
    policy: &EffectsPolicy,
) -> HashMap<Sym, Enveloped> {
    let mut out = HashMap::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let interop = f
                .effect_envelope
                .is_none()
                .then(|| own_interop_envelope(registry, f.docblock.as_ref()).into_bound())
                .flatten();
            if let Some(bound) =
                operative_bound(f.effect_envelope.as_ref(), interop.as_ref(), f.span, policy)
            {
                let flagged = !flagged_params(f.docblock.as_ref(), &f.params).is_empty();
                let labels = bound.labels.to_vec();
                out.insert(Sym::Func(f.fqn.clone()), Enveloped { labels, flagged });
            }
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                let interop = m
                    .effect_envelope
                    .is_none()
                    .then(|| interop_envelope(registry, cx.tree(), c, m).into_bound())
                    .flatten();
                if let Some(bound) =
                    operative_bound(m.effect_envelope.as_ref(), interop.as_ref(), m.span, policy)
                {
                    let labels = bound.labels.to_vec();
                    out.insert(
                        Sym::Method(c.fqn.clone(), m.name.clone()),
                        Enveloped { labels, flagged: false },
                    );
                }
            }
        }
    }
    out
}

/// What the floor knows about one unit beyond its bound.
pub(super) struct Floor<'a> {
    /// The run's enveloped declarations ([`enveloped_syms`]).
    enveloped: &'a HashMap<Sym, Enveloped>,
    /// The parameters whose `$f()` call discharge 2 answers: all of (a) flagged
    /// `@pure-unless-callable-is-impure` (or its `@phpstan-` spelling), on (b) a free
    /// function (the tag is honoured on free functions only, ADR-0063), (c) by-value
    /// and non-variadic, and (d) with no default, so the slot is never filled by
    /// the declaration itself. Empty for a method. (c)'s "never rebound" half is the
    /// syntax layer's `var` and [`Frame::rebound_by_call`].
    callables: HashSet<String>,
}

impl<'a> Floor<'a> {
    pub(super) fn new(
        enveloped: &'a HashMap<Sym, Enveloped>,
        docblock: Option<&String>,
        params: &[Param],
        free_function: bool,
    ) -> Self {
        let callables =
            if free_function { flagged_params(docblock, params) } else { HashSet::new() };
        Self { enveloped, callables }
    }

    /// Report one resolved site of a unit: its own gaps, then its inherited ones.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn report_site(
        &self,
        out: &mut Vec<Diagnostic>,
        cx: &Cx,
        frame: &Frame,
        site: &SiteOrigin,
        resolved: &ResolvedSite,
        effects: &HashMap<Sym, EffectSet>,
        display: &str,
        bound: OperativeBound<'_>,
    ) {
        let spelled = bound.spelled();
        let pos = cx.tree().position(site.span.start);
        let mut push = |message: String| {
            out.push(Diagnostic {
                id: EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message,
                facet: None,
                fix: None,
            });
        };
        for kind in self.own_gaps(cx, frame, site, resolved, bound) {
            push(format!(
                "effects at this site are unbounded ({}: {}), but {display}() is declared {spelled}",
                kind.as_str(),
                kind.reason()
            ));
        }
        for target in &resolved.targets {
            let Target::Edge(edge) = target else { continue };
            if edge.untainting {
                continue;
            }
            let Some(set) = effects.get(&edge.sym).filter(|set| !set.exhaustive) else { continue };
            // Discharge 6: the project's policy tolerates what reaches this edge, by the
            // attribution the callee carries, as the definite check reads it.
            if set.attribution.iter().any(|a| bound.policy.tolerates(a)) {
                continue;
            }
            let callee = callee_label(cx, &edge.sym);
            let gaps = set.gaps.names().join(", ");
            match self.enveloped.get(&edge.sym) {
                None => push(format!(
                    "{callee} has effects the analysis cannot bound ({gaps}) and declares no \
                     envelope of its own, but {display}() is declared {spelled}"
                )),
                // The callee's gaps are reported at the callee, against its own bound; they
                // are discharged here only if that bound fits this declaration's.
                Some(k) if !k.labels.iter().all(|l| !bound.exceeds(l)) => push(format!(
                    "{callee} bounds its effects ({gaps}) only by an envelope allowing {}, \
                     which {display}() declared {spelled} does not admit",
                    k.labels.join(", ")
                )),
                // A tainting edge into a tag-flagged function: its purity is conditional on
                // the callable bound at this call, and this call does not decide it.
                Some(k) if k.flagged => push(format!(
                    "{callee} is pure only if the callable bound to its parameter is, and this \
                     call does not decide it, but {display}() is declared {spelled}"
                )),
                Some(_) => {}
            }
        }
    }

    /// The gap kinds of one site that no discharge answers, in kind order.
    ///
    /// Every kind not named below stays, `NoEffectRow` included: a catalog name
    /// with no effect row is the lane's own coverage hole, not a contract.
    fn own_gaps(
        &self,
        cx: &Cx,
        frame: &Frame,
        site: &SiteOrigin,
        resolved: &ResolvedSite,
        bound: OperativeBound<'_>,
    ) -> BTreeSet<GapKind> {
        let mut gaps = resolved.gaps.clone();
        // Discharge 2: `$f()` on a flagged parameter of a free function, which the
        // call sites decide, provided nothing in the frame rebinds the parameter.
        if let SiteKind::Dynamic(DynamicSite::Call { var: Some(var) }) = &site.kind
            && self.callables.contains(var)
            && !frame.rebound_by_call(cx, var)
        {
            gaps.remove(&GapKind::DynamicCallee);
        }
        // Discharge 3: an interop envelope answered the call and its imported bound
        // fits this declaration's. The bound is unchecked, so the exhaustiveness bit
        // stays; the floor asks only whether the claim would break this envelope.
        if gaps.contains(&GapKind::InteropEnvelope) && imported_bound_fits(resolved, bound) {
            gaps.remove(&GapKind::InteropEnvelope);
        }
        // Discharge 6: a call on a declared receiver whose class the project's policy
        // attributes to a tolerated label (ADR-0084), for the gaps a declared receiver
        // leaves open.
        if let SiteKind::MethodCall { receiver, method } = &site.kind
            && receiver_tolerated(cx, frame, receiver, method, bound)
        {
            gaps.remove(&GapKind::DeclaredReceiver);
            gaps.remove(&GapKind::InteropEnvelope);
        }
        gaps
    }
}

/// Whether the body a declared-receiver call runs is attributed, by the project's
/// policy, to a label it tolerates (discharge 6).
///
/// The definite lane attributes by the body that **runs**: the resolved
/// `Sym::Method`, keyed by the class that declares it, with no inheritance. A declared
/// receiver names an abstraction, so this reads the same key only where dispatch is
/// exact: the declared class is final, or the resolved method is final or declared in
/// a final class. Otherwise a subclass or an implementation may run another body (a
/// `LoudLogger::info` behind a `Logger` attributed `telemetry`), and the gaps stay.
fn receiver_tolerated(
    cx: &Cx,
    frame: &Frame,
    receiver: &EffectRecv,
    method: &str,
    bound: OperativeBound<'_>,
) -> bool {
    let Some(fqn) = declared_receiver_fqn(cx, frame.class_fqn, frame.params, receiver) else {
        return false;
    };
    let Resolution::Found(r) = resolve_in_chain(cx, &fqn, method) else { return false };
    let declared_final = cx.find_class(&fqn).is_some_and(|(_, c)| c.is_final);
    let exact = declared_final || r.method.is_final || r.declaring_class.is_final;
    exact
        && bound
            .policy
            .method_attribution(&r.declaring_class.fqn, &r.method.name)
            .iter()
            .any(|a| bound.policy.tolerates(a))
}

/// How an inherited finding names the callee: `f()`, `C::m()`, `closure (line 3)`.
fn callee_label(cx: &Cx, sym: &Sym) -> String {
    let display = cx.sym_display(sym);
    match sym {
        Sym::Closure(..) => display,
        _ => format!("{display}()"),
    }
}

/// Whether every label the site imported into the declared lane fits `bound`. An
/// empty import (`@phpstan-pure`: the empty bound) fits any envelope.
fn imported_bound_fits(resolved: &ResolvedSite, bound: OperativeBound<'_>) -> bool {
    resolved.targets.iter().all(|target| match target {
        Target::Declared(labels) => labels.iter().all(|label| !bound.exceeds(label)),
        _ => true,
    })
}

/// The parameter names (no `$`) a declaration flags `@pure-unless-callable-is-impure`
/// (or its `@phpstan-` spelling) that are by-value, non-variadic and have no default:
/// the tag half of [`Floor::callables`]. A typed `pure-callable` is deliberately not
/// read here: the call-site obligation check proves impurity only of a closure or a
/// first-class callable, so it does not decide what a string or array callable runs.
fn flagged_params(docblock: Option<&String>, params: &[Param]) -> HashSet<String> {
    let mut out = HashSet::new();
    // Every spelling contains `pure-unless`; a docblock without it is not scanned,
    // which is nearly all of them.
    let Some(text) = docblock.filter(|t| t.contains("pure-unless")) else { return out };
    for tag in scan_docblock(text) {
        if let TagKind::ConditionalPurity(PurityCondition::CallableIsImpure) = tag.kind
            && let Some(var) = &tag.var_name
        {
            let name = var.trim_start_matches('$');
            if params.iter().any(|p| p.name == name && !p.has_default && !p.variadic && !p.by_ref) {
                out.insert(name.to_owned());
            }
        }
    }
    out
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
