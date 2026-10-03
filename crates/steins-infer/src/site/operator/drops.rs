//! The drop family's rule (ADR-0100 §7, issue #882): which dropped values may run
//! a destructor. A drop is never an edge: whichever frame releases the last
//! reference runs `__destruct`, and a constructor, a method call or a cycle can
//! move that to another frame or to the collector. So a drop whose value may run
//! user code is a [`GapKind::Destructor`], and one that cannot runs nothing.
//!
//! `reaches_destructor(C)` is the gate (ADR-0100's amendment of 2026-10-03, item
//! 5):
//!
//! 1. `C`'s chain declares `__destruct`, or imports a trait that does (a trait
//!    is read by its member names; one no file declares, two do, a condition
//!    guards or a `class_alias` names cannot be read, and counts), or, for a
//!    class that is only a bound, a class in the universe that can stand in for
//!    it does, an anonymous class's parent counted as the anonymous class when
//!    its body does ([`Index::destructor_ancestors`]). A class declared twice
//!    cannot be read, and counts;
//! 2. or a typed, non-static property on `C`'s chain, or one a trait it imports
//!    declares (a trait's properties are read off its hints), names a project
//!    class that reaches one, recursively, each class once for each exactness it
//!    is asked under.
//!
//! A hint that is `array`, `mixed`, `object`, `iterable`, `callable`, absent or
//! an engine class contributes nothing: those drops are recorded residue, and so
//! the gap is a lower bound on the drops that run user code.
//!
//! [`Index::destructor_ancestors`]: crate::project::Index::destructor_ancestors

use std::collections::HashSet;

use steins_syntax::{EffectRecv, TypeMember};

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
    let reaches = |class: &str, exact: bool| {
        reaches_destructor(cx, class, exact, &mut HashSet::new())
    };
    let bound = |class: &str| reaches(class, cx.class_has_no_subclass(class));
    match receiver {
        // A class the lowering already read a destructor off.
        None => true,
        // `new C`: the value is exactly a `C`.
        Some(EffectRecv::ClassName(name)) => reaches(&cx.class_fqn(name), true),
        // A parameter's hint: the class or any subclass of it.
        Some(EffectRecv::Bound(name)) => bound(&cx.class_fqn(name)),
        // `self` and `parent` hints name the enclosing class and its parent; a trait's
        // `self` is the using class's, which nothing here names.
        Some(EffectRecv::SelfKw) => frame
            .class_fqn
            .filter(|own| !cx.find_class(own).is_some_and(|(_, cd)| cd.is_trait))
            .is_some_and(bound),
        Some(EffectRecv::Parent) => {
            frame.class_fqn.and_then(|own| cx.parent_fqn(own)).is_some_and(|parent| bound(&parent))
        }
        Some(_) => false,
    }
}

/// Whether dropping a value of class `class` may run a destructor: `exact` when
/// the value is that class and no subclass. `seen` holds the (class, exactness)
/// pairs already asked, so a cycle of property types ends, and a class held
/// through a property that names it bound is asked apart from the exact one.
fn reaches_destructor(
    cx: &Cx<'_>,
    class: &str,
    exact: bool,
    seen: &mut HashSet<(String, bool)>,
) -> bool {
    let id = cx.class_identity(class);
    if !seen.insert((id.clone(), exact)) {
        return false;
    }
    chain_declares(cx, &id)
        || (!exact && cx.index.destructor_ancestors(|| destructor_ancestors(cx)).contains(&id))
        || holds_one(cx, &id, seen)
}

/// Whether `class` or an ancestor on its `extends` line is a class the shard
/// lists as running a destructor: declaring one, or importing a trait that may.
/// A class declared twice
/// cannot be read, and may be any of its declarations: it counts.
fn chain_declares(cx: &Cx<'_>, class: &str) -> bool {
    let mut visited: HashSet<String> = HashSet::new();
    let mut current = class.to_owned();
    loop {
        if cx.index.destructor_classes().contains(&current) {
            return true;
        }
        if cx.find_class(&current).is_none() {
            return !cx.class_absent(&current);
        }
        let Some(parent) = cx.parent_fqn(&current) else { return false };
        current = cx.class_identity(&parent);
        if !visited.insert(current.clone()) {
            return false;
        }
    }
}

/// Whether a typed property on `class`'s chain names a class that reaches a
/// destructor (clause 2), a property a trait imports included. An untyped property, or
/// one hinted `array`, `mixed`, `object`, `iterable`, `callable` or an engine class,
/// names none.
fn holds_one(cx: &Cx<'_>, class: &str, seen: &mut HashSet<(String, bool)>) -> bool {
    let own = cx.class_props(class).into_iter().filter_map(|p| p.ty.as_ref()).any(|ty| {
        ty.members.iter().any(|member| match member {
            TypeMember::Instance { fqn, .. } => {
                reaches_destructor(cx, fqn, cx.class_has_no_subclass(fqn), seen)
            }
            TypeMember::InstanceInter(classes) => classes.iter().any(|c| {
                reaches_destructor(cx, &c.fqn, cx.class_has_no_subclass(&c.fqn), seen)
            }),
            TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => false,
        })
    });
    own || trait_held(cx, class).iter().any(|held| {
        reaches_destructor(cx, held, cx.class_has_no_subclass(held), seen)
    })
}

/// The classes the properties that `class`, its ancestors and every trait they import
/// (a trait's imports too) are hinted with, as the traits declare them: a trait's
/// properties are not lowered, so the hint names are read off the declaration. A trait
/// the index cannot place is not read here; the merge counted its users already.
fn trait_held(cx: &Cx<'_>, class: &str) -> Vec<String> {
    let mut held = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending = vec![cx.class_identity(class)];
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some((file, cd)) = cx.find_class(&name) else { continue };
        let tree = cx.units[file].tree;
        held.extend(cd.held_classes.iter().map(|r| tree.resolve_class_fqn(r)));
        let imported = cd.used_traits.iter().chain(&cd.parent);
        pending.extend(imported.map(|r| cx.class_identity(&tree.resolve_class_fqn(r))));
    }
    held
}

/// The names that are, or are an ancestor of, a class whose chain declares a
/// destructor (or imports a trait that may), or the parent of an anonymous class
/// whose body runs one, as identities.
/// A subclass of such a class inherits its destructor, so what it implements is
/// an ancestor too. Ancestors are the project's own declarations and the
/// catalog's, through `extends` and `implements`.
fn destructor_ancestors(cx: &Cx<'_>) -> HashSet<String> {
    let index = cx.index;
    let mut pending: Vec<String> = index
        .destructor_classes()
        .iter()
        .chain(index.anonymous_destructor_parents())
        .map(|name| cx.class_identity(name))
        .collect();
    pending.extend(
        index.class_names().map(|name| cx.class_identity(name)).filter(|id| chain_declares(cx, id)),
    );
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
