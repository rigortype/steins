//! The drop family's rule (ADR-0100 §7, issue #882): which dropped values may run
//! a destructor. A drop is never an edge: whichever frame releases the last
//! reference runs `__destruct`, and a constructor, a method call or a cycle can
//! move that to another frame or to the collector. So a drop whose value may run
//! user code is a [`GapKind::Destructor`], and one that cannot runs nothing.
//!
//! `reaches_destructor(C)` is the gate (ADR-0100's amendment of 2026-10-03, item
//! 5):
//!
//! 1. `C`'s chain declares `__destruct` (or imports a trait, whose body is not
//!    lowered), or, for a class that is only a bound, a class in the universe
//!    that can stand in for it does, an anonymous class's parent counted as the
//!    anonymous class ([`Index::destructor_ancestors`]);
//! 2. or a typed, non-static property on `C`'s chain names a project class that
//!    reaches one, recursively, each class once.
//!
//! A hint that is `array`, `mixed`, `object`, `iterable`, `callable`, absent or
//! an engine class contributes nothing: those drops are recorded residue, and so
//! the gap is a lower bound on the drops that run user code.
//!
//! [`Index::destructor_ancestors`]: crate::project::Index::destructor_ancestors

use std::collections::HashSet;

use steins_syntax::{EffectRecv, NativeType, TypeMember};

use super::super::reach::Frame;
use super::super::{GapKind, ResolvedSite};
use crate::cx::Cx;

/// Resolve a drop site: a gap when some class the dropped variable may hold
/// reaches a destructor. `receivers` has one entry per such class.
pub(super) fn resolve(
    cx: &Cx<'_>,
    frame: &Frame<'_>,
    receivers: &[Option<EffectRecv>],
) -> ResolvedSite {
    let mut out = ResolvedSite::default();
    if receivers.iter().any(|receiver| may_run(cx, frame, receiver.as_ref())) {
        out.gaps.insert(GapKind::Destructor);
    }
    out
}

/// Whether the value one receiver names may run a destructor when dropped.
fn may_run(cx: &Cx<'_>, frame: &Frame<'_>, receiver: Option<&EffectRecv>) -> bool {
    match receiver {
        // A class the lowering already read a destructor off.
        None => true,
        // `new C`: the value is exactly a `C`.
        Some(EffectRecv::ClassName(name)) => {
            reaches_destructor(cx, &cx.class_fqn(name), true, &mut HashSet::new())
        }
        // A parameter, by its declared hint, whatever the frame writes into it.
        Some(EffectRecv::Var(name)) => frame
            .params
            .iter()
            .find(|p| &p.name == name)
            .and_then(|p| p.ty.as_ref())
            .is_some_and(|ty| declared_reaches(cx, ty)),
        Some(_) => false,
    }
}

/// Whether a value of declared type `ty` may run a destructor: some class member
/// does. A class that is not final is only a bound.
fn declared_reaches(cx: &Cx<'_>, ty: &NativeType) -> bool {
    let mut seen = HashSet::new();
    ty.members.iter().any(|member| match member {
        TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => false,
        TypeMember::Instance { fqn, .. } => {
            reaches_destructor(cx, fqn, cx.class_has_no_subclass(fqn), &mut seen)
        }
        TypeMember::InstanceInter(classes) => classes
            .iter()
            .any(|c| reaches_destructor(cx, &c.fqn, cx.class_has_no_subclass(&c.fqn), &mut seen)),
    })
}

/// Whether dropping a value of class `class` may run a destructor: `exact` when
/// the value is that class and no subclass. `seen` holds the classes already
/// asked, so a cycle of property types ends.
fn reaches_destructor(cx: &Cx<'_>, class: &str, exact: bool, seen: &mut HashSet<String>) -> bool {
    let id = cx.class_identity(class);
    if !seen.insert(id.clone()) {
        return false;
    }
    chain_declares(cx, &id)
        || (!exact && cx.index.destructor_ancestors(|| destructor_ancestors(cx)).contains(&id))
        || holds_one(cx, &id, seen)
}

/// Whether `class` or an ancestor on its `extends` line is a class the shard
/// lists as declaring a destructor or importing a trait.
fn chain_declares(cx: &Cx<'_>, class: &str) -> bool {
    let mut visited: HashSet<String> = HashSet::new();
    let mut current = class.to_owned();
    loop {
        if cx.index.destructor_classes().contains(&current) {
            return true;
        }
        let Some(parent) = cx.parent_fqn(&current) else { return false };
        current = cx.class_identity(&parent);
        if !visited.insert(current.clone()) {
            return false;
        }
    }
}

/// Whether a typed property on `class`'s chain names a class that reaches a
/// destructor (clause 2). An untyped property, or one hinted `array`, `mixed`,
/// `object`, `iterable`, `callable` or an engine class, names none.
fn holds_one(cx: &Cx<'_>, class: &str, seen: &mut HashSet<String>) -> bool {
    cx.class_props(class).into_iter().filter_map(|p| p.ty.as_ref()).any(|ty| {
        ty.members.iter().any(|member| match member {
            TypeMember::Instance { fqn, .. } => {
                reaches_destructor(cx, fqn, cx.class_has_no_subclass(fqn), seen)
            }
            TypeMember::InstanceInter(classes) => classes.iter().any(|c| {
                reaches_destructor(cx, &c.fqn, cx.class_has_no_subclass(&c.fqn), seen)
            }),
            TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => false,
        })
    })
}

/// The names that are, or are an ancestor of, a class in the shard's destructor
/// table or an anonymous class's parent, as identities. Ancestors are the
/// project's own declarations and the catalog's, through `extends` and
/// `implements`.
fn destructor_ancestors(cx: &Cx<'_>) -> HashSet<String> {
    let index = cx.index;
    let mut pending: Vec<String> = index
        .destructor_classes()
        .iter()
        .chain(index.anonymous_subclass_parents())
        .map(|name| cx.class_identity(name))
        .collect();
    let mut out: HashSet<String> = HashSet::new();
    while let Some(name) = pending.pop() {
        if !out.insert(name.clone()) {
            continue;
        }
        if let Some(supers) = cx.ancestors_of(&name) {
            pending.extend(supers.iter().map(|s| cx.class_identity(s)));
        }
    }
    out
}
