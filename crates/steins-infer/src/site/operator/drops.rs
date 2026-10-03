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
//! A class no file declares and the engine does not (a vendor class this checkout
//! lacks: an `extends`, a parameter hint, a property hint) may declare one, and
//! counts, as it does in every family that reads an unclosed chain; with the vendor
//! tree present the names resolve and it reads as any class does. The one place it
//! does not count is a `new` of such a class itself: that site records an
//! unknown-class gap already, so the body is `…?` through the same name.
//!
//! A hint that is `array`, `mixed`, `object`, `iterable`, `callable`, absent or
//! an engine class contributes nothing: those drops are recorded residue, and so
//! the gap is a lower bound on the drops that run user code.
//!
//! [`Index::destructor_ancestors`]: crate::project::Index::destructor_ancestors

use std::collections::HashSet;

use steins_db::AnonymousClass;
use steins_syntax::{EffectRecv, TypeMember};

use super::super::engine::declares_engine_class;
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
        // `new C`: the value is exactly a `C`. A `C` no file declares already records an
        // unknown-class gap at the `new`, which makes the body `…?` through the same
        // name; a second gap there would only repeat it.
        Some(EffectRecv::ClassName(name)) => {
            let class = cx.class_fqn(name);
            !unseen(cx, &class) && reaches(&class, true)
        }
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

/// Whether `class` is a name no file declares and the engine does not.
fn unseen(cx: &Cx<'_>, class: &str) -> bool {
    let id = cx.class_identity(class);
    cx.find_class(&id).is_none() && cx.class_absent(&id) && !declares_engine_class(&id)
}

/// Whether `class` or an ancestor on its `extends` line is a class the shard
/// lists as running a destructor: declaring one, or importing a trait that may.
/// A class declared twice
/// cannot be read, and may be any of its declarations: it counts. So does a name no
/// file declares unless the engine does (the chain leaves the project at a class this
/// checkout lacks, a vendor class whose destructor nothing here can read): the
/// unclosed-chain rule every family reads (ADR-0099 §4.3). An engine class declares
/// none that a project can observe, so a chain that ends at one is closed.
fn chain_declares(cx: &Cx<'_>, class: &str) -> bool {
    let mut visited: HashSet<String> = HashSet::new();
    let mut current = class.to_owned();
    loop {
        if cx.index.destructor_classes().contains(&current) {
            return true;
        }
        if cx.find_class(&current).is_none() {
            return !cx.class_absent(&current) || !declares_engine_class(&current);
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
/// properties are not lowered, so the hint names are read off the declaration. A hint
/// `self` in a trait names the class that imports it and `parent` that class's parent. A
/// trait the index cannot place is not read here; the merge counted its users already.
fn trait_held(cx: &Cx<'_>, class: &str) -> Vec<String> {
    let id = cx.class_identity(class);
    held_names(cx, vec![(id.clone(), Some(id))])
}

/// [`trait_held`] from several class-likes, each with the class that imports it: itself
/// for a class, the importer for a trait, none for an anonymous class's.
fn held_names(cx: &Cx<'_>, mut pending: Vec<(String, Option<String>)>) -> Vec<String> {
    let mut held = Vec::new();
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    while let Some((name, importer)) = pending.pop() {
        if !seen.insert((name.clone(), importer.clone())) {
            continue;
        }
        let Some((file, cd)) = cx.find_class(&name) else { continue };
        let tree = cx.units[file].tree;
        let importer = if cd.is_trait { importer } else { Some(name) };
        held.extend(cd.held_classes.iter().map(|r| tree.resolve_class_fqn(r)));
        if cd.holds_self {
            held.extend(importer.clone());
        }
        if cd.holds_parent {
            held.extend(importer.as_deref().and_then(|own| cx.parent_fqn(own)));
        }
        let identity = |r| cx.class_identity(&tree.resolve_class_fqn(r));
        pending.extend(cd.used_traits.iter().map(|r| (identity(r), importer.clone())));
        pending.extend(cd.parent.iter().map(|r| (identity(r), Some(identity(r)))));
    }
    held
}

/// What the closure of [`destructor_ancestors`] holds while it grows.
#[derive(Default)]
struct Closure {
    /// The classes that, as exactly themselves, may run a destructor on a drop.
    reach: HashSet<String>,
    /// Every name that is, or is an ancestor of, a class of `reach`, or a parent or
    /// interface of an anonymous class that reaches one.
    ancestors: HashSet<String>,
}

impl Closure {
    /// Add `name` and every ancestor of it.
    fn add_ancestors(&mut self, cx: &Cx<'_>, name: String) {
        let mut pending = vec![name];
        while let Some(name) = pending.pop() {
            if !self.ancestors.insert(name.clone()) {
                continue;
            }
            if let Some(supers) = cx.ancestors_of(&name) {
                pending.extend(supers.iter().map(|s| cx.class_identity(s)));
            }
        }
    }

    fn add_reaching(&mut self, cx: &Cx<'_>, class: String) {
        self.reach.insert(class.clone());
        self.add_ancestors(cx, class);
    }

    /// Whether a value hinted `class` may run a destructor as far as the closure knows:
    /// its chain does, or, as the exact class when nothing extends it, it reaches one, or,
    /// as a bound, it is a class that does or an ancestor of one.
    fn hit(&self, cx: &Cx<'_>, class: &str) -> bool {
        let id = cx.class_identity(class);
        chain_declares(cx, &id)
            || if cx.class_has_no_subclass(class) {
                self.reach.contains(&id)
            } else {
                self.ancestors.contains(&id)
            }
    }

    /// Whether a class holds, in a property of its chain or one a trait imports, a class
    /// that the closure so far says may run a destructor.
    fn holds(&self, cx: &Cx<'_>, class: &str) -> bool {
        let typed = cx.class_props(class).into_iter().filter_map(|p| p.ty.as_ref());
        let named = typed.flat_map(|ty| ty.members.iter()).flat_map(|member| match member {
            TypeMember::Instance { fqn, .. } => vec![fqn.clone()],
            TypeMember::InstanceInter(classes) => classes.iter().map(|c| c.fqn.clone()).collect(),
            TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => Vec::new(),
        });
        named.chain(trait_held(cx, class)).any(|held| self.hit(cx, &held))
    }

    /// Whether an anonymous class reaches a destructor: it declares one, its parent's
    /// chain does, a trait it imports may, or a property of it holds one.
    fn anonymous_reaches(&self, cx: &Cx<'_>, anon: &AnonymousClass) -> bool {
        let identity = |name: &String| cx.class_identity(name);
        let traits: Vec<String> = anon.traits.iter().map(identity).collect();
        anon.declares_destructor
            || anon.parent.as_ref().is_some_and(|parent| {
                let id = identity(parent);
                chain_declares(cx, &id) || self.reach.contains(&id)
            })
            || traits.iter().any(|t| chain_declares(cx, t))
            || anon.held.iter().any(|held| self.hit(cx, held))
            || held_names(cx, traits.into_iter().map(|t| (t, None)).collect())
                .iter()
                .any(|held| self.hit(cx, held))
    }
}

/// The names that are, or are an ancestor of, a class that reaches a destructor, or a
/// parent or interface of an anonymous class that does, as identities. A subclass of such
/// a class inherits its destructor, so what it implements is an ancestor too. A class
/// reaches one when its chain declares it, or imports a trait that may, or holds in a
/// typed property, its own or a trait's, a class that reaches one; an anonymous class by
/// the same reading of its own body and its parent. Ancestors are the project's own
/// declarations and the catalog's, through `extends` and `implements`.
///
/// A fixpoint: what a class holds may itself be an ancestor, so the closure grows until
/// a round adds nothing. It only grows over a finite universe, so it ends, and it is a
/// function of the universe's declarations, not of the order they are read in.
fn destructor_ancestors(cx: &Cx<'_>) -> HashSet<String> {
    let index = cx.index;
    let mut ids: Vec<String> = index.class_names().map(|name| cx.class_identity(name)).collect();
    ids.sort();
    ids.dedup();
    let mut closure = Closure::default();
    for name in index.destructor_classes() {
        closure.add_reaching(cx, cx.class_identity(name));
    }
    for id in &ids {
        if chain_declares(cx, id) {
            closure.add_reaching(cx, id.clone());
        }
    }
    let mut anonymous: Vec<&AnonymousClass> = index.anonymous_classes().iter().collect();
    loop {
        let before = (closure.reach.len(), closure.ancestors.len());
        for id in &ids {
            if !closure.reach.contains(id) && closure.holds(cx, id) {
                closure.add_reaching(cx, id.clone());
            }
        }
        let (reaching, rest): (Vec<_>, Vec<_>) =
            anonymous.into_iter().partition(|anon| closure.anonymous_reaches(cx, anon));
        anonymous = rest;
        for anon in reaching {
            for name in anon.parent.iter().chain(&anon.interfaces) {
                closure.add_ancestors(cx, cx.class_identity(name));
            }
        }
        if before == (closure.reach.len(), closure.ancestors.len()) {
            return closure.ancestors;
        }
    }
}
