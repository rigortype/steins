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
//!   [`ResolvedSite`] [`super::report_site`] reads, less the gaps discharges 2 and
//!   3 answer.
//! * **Inherited.** One finding per call edge that propagates the callee's `…?`
//!   (an untainting edge does not) into a project body that is itself `…?` and
//!   carries no envelope of its own (discharge 4: an enveloped callee is a unit,
//!   and its own gaps are reported there, once).
//!
//! The exhaustiveness bit, the fixpoint and every writer are untouched: nothing
//! here feeds a row back.

use std::collections::{BTreeSet, HashMap, HashSet};

use steins_contract::ContractTy;
use steins_db::EffectsPolicy;
use steins_phpdoc::{PurityCondition, TagKind, scan_docblock};
use steins_syntax::{DynamicSite, SiteKind, SiteOrigin};

use super::{
    EffectSet, OperativeBound, interop_envelope, operative_bound, own_interop_envelope,
};
use crate::Sym;
use crate::contract::parse_envelopes;
use crate::cx::Cx;
use crate::project::{Diagnostic, FileUnit, Index};
use crate::site::reach::Frame;
use crate::site::{GapKind, ResolvedSite, Target};
use crate::EFFECT_MAYBE_ENVELOPE_EXCEEDED_ID;

/// The declarations whose envelope bounds something: a unit's callees owe nothing to
/// a caller that reaches them when they are in this set (discharge 4).
///
/// Closures are never in it: a closure carries no envelope, so a gap in one is
/// reported at the edge that reaches it.
pub(super) fn enveloped_syms(
    units: &[FileUnit],
    index: &Index,
    registry: &steins_catalog::LabelRegistry,
    policy: &EffectsPolicy,
) -> HashSet<Sym> {
    let mut out = HashSet::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let interop = f
                .effect_envelope
                .is_none()
                .then(|| own_interop_envelope(registry, f.docblock.as_ref()).into_bound())
                .flatten();
            if operative_bound(f.effect_envelope.as_ref(), interop.as_ref(), f.span, policy)
                .is_some()
            {
                out.insert(Sym::Func(f.fqn.clone()));
            }
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                let interop = m
                    .effect_envelope
                    .is_none()
                    .then(|| interop_envelope(registry, cx.tree(), c, m).into_bound())
                    .flatten();
                if operative_bound(m.effect_envelope.as_ref(), interop.as_ref(), m.span, policy)
                    .is_some()
                {
                    out.insert(Sym::Method(c.fqn.clone(), m.name.clone()));
                }
            }
        }
    }
    out
}

/// What the floor knows about one unit beyond its bound.
pub(super) struct Floor<'a> {
    /// The run's enveloped declarations ([`enveloped_syms`]).
    enveloped: &'a HashSet<Sym>,
    /// The parameters whose contract already speaks for a `$f()` call in this body
    /// (discharge 2): typed `pure-callable`, `pure-closure` or `static-pure-closure`,
    /// or flagged `@pure-unless-callable-is-impure`. The caller's argument is held
    /// to purity at every call site the analyzer sees (ADR-0063), so the call
    /// through the parameter is answered there.
    callables: HashSet<String>,
}

impl<'a> Floor<'a> {
    pub(super) fn new(enveloped: &'a HashSet<Sym>, docblock: Option<&String>) -> Self {
        Self { enveloped, callables: pure_callable_params(docblock) }
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
            if edge.untainting || self.enveloped.contains(&edge.sym) {
                continue;
            }
            let Some(set) = effects.get(&edge.sym).filter(|set| !set.exhaustive) else { continue };
            push(format!(
                "{} has effects the analysis cannot bound ({}) and declares no envelope of its \
                 own, but {display}() is declared {spelled}",
                callee_label(cx, &edge.sym),
                set.gaps.names().join(", ")
            ));
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
        // Discharge 2: `$f()` on a parameter whose purity contract the call sites
        // enforce, provided nothing in the frame rebinds the parameter.
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
        gaps
    }
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

/// The parameter names (no `$`) of a declaration whose contract bounds a call made
/// through them: [`Floor::callables`].
fn pure_callable_params(docblock: Option<&String>) -> HashSet<String> {
    let mut out = HashSet::new();
    // Every spelling contains `pure-`: `pure-callable`, `pure-closure`,
    // `static-pure-closure`, `pure-unless-callable-is-impure`. A docblock without it
    // is not scanned, which is nearly all of them.
    let Some(text) = docblock.filter(|t| t.contains("pure-")) else { return out };
    for tag in scan_docblock(text) {
        if let TagKind::ConditionalPurity(PurityCondition::CallableIsImpure) = tag.kind
            && let Some(var) = &tag.var_name
        {
            out.insert(var.trim_start_matches('$').to_owned());
        }
    }
    if let Some(envelopes) = parse_envelopes(Some(text)) {
        for (name, ty) in &envelopes.params {
            if let ContractTy::CallableTy { obl, .. } = steins_contract::lower(ty)
                && obl.pure
            {
                out.insert(name.clone());
            }
        }
    }
    out
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
