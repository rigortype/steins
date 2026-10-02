//! The strict floor of the `@throws` envelope check (ADR-0100): `throw.maybe-undeclared`.
//!
//! [`super::emit_undeclared`] reports what the throw fixpoint *proved* escapes a
//! declaration's `@throws`; a throw set marked `…?` is silent there, because an
//! unknown is not an escape. This module names the gaps behind that mark, on the
//! same units the proven check judges and on the same shape as the effect lane's
//! floor ([`crate::purity`]'s `floor` module).
//!
//! A **unit** is a function or method whose docblock declares `@throws`; a declared
//! `\Throwable` is ⊤ (every throw is covered) and no unit (discharge 1). Within a unit:
//!
//! * **Direct.** One finding per own site and gap kind, from the same
//!   [`ResolvedSite`] the throw lane folds into its row.
//! * **Inherited.** One finding per call edge into a project body whose own throw
//!   set is `…?` and which declares no `@throws` of its own (discharge 4: a
//!   declaring callee is a unit, and reports its gaps once, itself).
//!
//! A site inside a `try` with a `catch (\Throwable)` is discharged whole
//! (discharge 5): whatever it throws is caught there. A catch of anything narrower
//! discharges nothing, since the unknown may be outside it.
//!
//! Neither the throw fixpoint nor the exhaustiveness bit is touched.

use std::collections::{HashMap, HashSet};

use super::{Guards, ThrowSet, declared_throws, last_segment, resolve_guards};
use crate::Sym;
use crate::cx::Cx;
use crate::project::Diagnostic;
use crate::site::reach::Frame;
use crate::site::{Knowledge, Lane, Target, resolve_site};
use crate::{Fixpoints, Gate, THROW_MAYBE_UNDECLARED_ID};

/// Whether a declared `@throws` set is ⊤: it names `\Throwable`, which every throw
/// is a subtype of, so it bounds nothing.
pub(super) fn declares_top(declared: &[String]) -> bool {
    declared.iter().any(|d| d.trim_start_matches('\\').eq_ignore_ascii_case("Throwable"))
}

/// The declarations whose `@throws` bounds something: a caller's inherited gap is
/// owed to the callee's own report when the callee is in this set (discharge 4).
///
/// Read from the files that spell `@throws`, which is all of them that can declare
/// one: the rest stay undecoded, as in [`super::throw_diagnostics`] itself.
pub(super) fn enveloped_syms(fx: &Fixpoints<'_>) -> HashSet<Sym> {
    let (units, index) = (fx.units(), fx.index());
    let mut out = HashSet::new();
    for fi in 0..units.len() {
        if !fx.spells(fi, Gate::Throws) {
            continue;
        }
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            if bounds_something(&declared_throws(&cx, f.span.start, f.docblock.as_deref())) {
                out.insert(Sym::Func(f.fqn.clone()));
            }
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                if bounds_something(&declared_throws(&cx, m.span.start, m.docblock.as_deref())) {
                    out.insert(Sym::Method(c.fqn.clone(), m.name.clone()));
                }
            }
        }
    }
    out
}

/// Whether a declaration's `@throws` is a bound: non-empty, and not ⊤.
fn bounds_something(declared: &[String]) -> bool {
    !declared.is_empty() && !declares_top(declared)
}

/// Whether the guard stack catches everything a site can throw: some clause names
/// `\Throwable` itself.
fn absorbs_throwable(guards: &Guards) -> bool {
    guards.iter().flatten().any(|clause| {
        clause.classes.iter().any(|c| c.trim_start_matches('\\').eq_ignore_ascii_case("Throwable"))
    })
}

/// Report one `@throws` unit's gaps: `declared` is its resolved declared set (not ⊤:
/// the caller skips a ⊤ unit through [`declares_top`]).
pub(super) fn report_unit(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    frame: &Frame,
    display: &str,
    declared: &[String],
    judged: (&HashMap<Sym, ThrowSet>, &HashSet<Sym>),
) {
    if declares_top(declared) {
        return;
    }
    let (throws, enveloped) = judged;
    let list = declared.iter().map(|d| last_segment(d)).collect::<Vec<_>>().join("|");
    let knowledge = Knowledge::Catalog { lane: Lane::Throws, plugins: None };
    for site in frame.sites {
        let resolved = resolve_site(cx, frame, site, &knowledge);
        if resolved.gaps.is_empty() && resolved.targets.is_empty() {
            continue;
        }
        if absorbs_throwable(&resolve_guards(cx, &site.guards)) {
            continue;
        }
        let pos = cx.tree().position(site.span.start);
        let mut push = |message: String| {
            out.push(Diagnostic {
                id: THROW_MAYBE_UNDECLARED_ID,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message,
                facet: None,
                fix: None,
            });
        };
        for kind in &resolved.gaps {
            push(format!(
                "what can be thrown at this site is unbounded ({}: {}), but {display}() declares \
                 only @throws {list}",
                kind.as_str(),
                kind.reason()
            ));
        }
        for target in &resolved.targets {
            let Target::Edge(edge) = target else { continue };
            if enveloped.contains(&edge.sym) {
                continue;
            }
            let Some(set) = throws.get(&edge.sym).filter(|set| !set.exhaustive) else { continue };
            push(format!(
                "{} can throw what the analysis cannot bound ({}) and declares no @throws of its \
                 own, but {display}() declares only @throws {list}",
                callee_label(cx, &edge.sym),
                set.gaps.names().join(", ")
            ));
        }
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
