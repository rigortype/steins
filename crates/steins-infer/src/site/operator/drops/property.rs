//! The property drops (ADR-0100 §7, issue #1003): a plain write to a property, or an
//! `unset` of one, releases the value it held, and a destructor that value reaches runs
//! at the statement, so a throwing destructor surfaces there.
//!
//! The value's class is the property's **declared hint**: a typed property holds an
//! instance of the classes it names or of a subclass, and a property's type is invariant
//! down a chain, so the declaration found first on the chain is the one that holds. The
//! hint is read as written (`array|D` names `D`, `?self` the declaring class), the way a
//! parameter's is. An untyped property names no class, and is recorded residue as an
//! untyped parameter is: what it holds is whatever the class stores into it.
//!
//! A write drops nothing in three cases:
//!
//! * the property is `readonly`, so a write either initializes it or raises an `Error`;
//! * it has a `set` hook, which is the magic-property family's gap and not a drop;
//! * it is the first thing a constructor does to a property its own class declares, with
//!   no default (an uninitialized typed property) or a default of `null`, and is not
//!   promoted ([`C::DropPropInit`]). Witnessed: writing over an uninitialized typed property
//!   drops nothing, and a typed property's first write in a method runs a destructor only
//!   when something wrote it before. Two shapes stay residue: a second explicit call of
//!   `__construct`, and a subclass constructor that writes an inherited property before
//!   calling the parent's, which the subclass's own write site already marks.
//!
//! The chain is walked class by class: a class's own declaration, then the properties of the
//! traits it imports (read off the trait's member list, a trait's body not being lowered),
//! then its parent. A class declared more than once may be any of its declarations, so each
//! is asked and one that may run a destructor makes the site a gap. The chain leaving the
//! project at a name no file declares and the engine does not may declare the property (the
//! unclosed-chain rule, ADR-0100's S6c item 3), as a trait no file declares may; a property
//! no class of a closed chain declares is dynamic, and untyped.

use std::collections::HashSet;

use steins_syntax::{
    ArgValue, ClassDecl, EffectRecv, NameRef, OperatorConstruct as C, PropertyDecl, SourceTree,
    TraitProp,
};

use super::super::super::engine::declares_engine_class;
use super::super::super::reach::Frame;
use super::reaches_destructor;
use crate::cx::Cx;

/// What a walk of one property's chain is asked.
struct Ask<'c, 'a> {
    cx: &'c Cx<'a>,
    frame: &'c Frame<'c>,
    construct: C,
    member: &'c str,
    is_static: bool,
}

/// Whether the write or `unset` of the property `member` of the class `receiver` names may
/// run a destructor.
pub(super) fn may_run(
    cx: &Cx<'_>,
    frame: &Frame<'_>,
    construct: C,
    receiver: Option<&EffectRecv>,
    member: &str,
) -> bool {
    let Some(class) = holder(cx, frame, receiver) else { return false };
    let is_static = !matches!(receiver, Some(EffectRecv::This));
    let ask = Ask { cx, frame, construct, member, is_static };
    ask.walk(&class, &mut HashSet::new())
}

/// The class a receiver names in this frame, as an identity.
fn holder(cx: &Cx<'_>, frame: &Frame<'_>, receiver: Option<&EffectRecv>) -> Option<String> {
    match receiver? {
        EffectRecv::This | EffectRecv::SelfKw => frame.class_fqn.map(|own| cx.class_identity(own)),
        EffectRecv::Parent => {
            frame.class_fqn.and_then(|own| cx.parent_fqn(own)).map(|p| cx.class_identity(&p))
        }
        EffectRecv::ClassName(name) => Some(cx.class_identity(&cx.class_fqn(name))),
        _ => None,
    }
}

/// Every declaration a class name may stand for: the one of a unique class, each of a class
/// declared more than once, none for a name no file declares.
fn declarations<'a>(cx: &Cx<'a>, name: &str) -> Vec<(usize, &'a ClassDecl)> {
    if let Some(one) = cx.find_class(name) {
        return vec![one];
    }
    if cx.class_absent(name) {
        return Vec::new();
    }
    cx.units
        .iter()
        .enumerate()
        .flat_map(|(file, unit)| {
            unit.tree.classes().iter().filter(|cd| cd.fqn.eq_ignore_ascii_case(name)).map(move |cd| (file, cd))
        })
        .collect()
}

impl Ask<'_, '_> {
    /// Whether the property of `class`, or of the chain above it, may hold a value that runs
    /// a destructor when dropped.
    fn walk(&self, class: &str, seen: &mut HashSet<String>) -> bool {
        if !seen.insert(class.to_owned()) {
            return false;
        }
        let decls = declarations(self.cx, class);
        if decls.is_empty() {
            // The chain leaves the project: a name nobody declares may declare the property.
            return !self.cx.class_absent(class) || !declares_engine_class(class);
        }
        decls.iter().any(|&(file, cd)| self.in_declaration(class, file, cd, seen))
    }

    fn in_declaration(
        &self,
        class: &str,
        file: usize,
        cd: &ClassDecl,
        seen: &mut HashSet<String>,
    ) -> bool {
        let tree = self.cx.units[file].tree;
        if let Some(prop) =
            cd.properties.iter().find(|p| p.name == self.member && p.is_static == self.is_static)
        {
            return self.declared(prop, class, file);
        }
        if cd.hooked_properties.iter().any(|h| h == self.member) {
            return false;
        }
        if let Some(may) = self.in_traits(class, tree, &cd.used_traits, &mut HashSet::new()) {
            return may;
        }
        match cd.parent.as_ref() {
            Some(parent) => {
                let parent = self.cx.class_identity(&tree.resolve_class_fqn(parent));
                self.walk(&parent, seen)
            }
            None => false,
        }
    }

    /// The property as a trait the chain imports declares it: `None` when none does, and
    /// `Some(true)` when a trait cannot be read at all.
    fn in_traits(
        &self,
        class: &str,
        tree: &SourceTree,
        used: &[NameRef],
        seen: &mut HashSet<String>,
    ) -> Option<bool> {
        for r in used {
            let name = self.cx.class_identity(&tree.resolve_class_fqn(r));
            if !seen.insert(name.clone()) {
                continue;
            }
            let decls = declarations(self.cx, &name);
            if decls.is_empty() {
                return Some(true);
            }
            let mut found = false;
            let mut may = false;
            for (file, td) in decls {
                let inner = self.cx.units[file].tree;
                let own = td.trait_props.iter().find(|p| p.name == self.member && p.is_static == self.is_static);
                if let Some(prop) = own {
                    found = true;
                    may |= self.imported(prop, class, inner);
                } else if let Some(deeper) = self.in_traits(class, inner, &td.used_traits, seen) {
                    found = true;
                    may |= deeper;
                }
            }
            if found {
                return Some(may);
            }
        }
        None
    }

    /// A property a class declares: dropping its value runs a destructor when its hint may.
    fn declared(&self, prop: &PropertyDecl, owner: &str, file: usize) -> bool {
        if prop.readonly || prop.hooked {
            return false;
        }
        if self.construct == C::DropPropInit && self.initializes(prop, owner) {
            return false;
        }
        let tree = self.cx.units[file].tree;
        self.hint_reaches(&prop.hint_classes, prop.hint_self, prop.hint_parent, owner, tree)
    }

    /// A property a trait declares, imported by `importer`.
    fn imported(&self, prop: &TraitProp, importer: &str, tree: &SourceTree) -> bool {
        !prop.readonly
            && !prop.hooked
            && self.hint_reaches(&prop.hint_classes, prop.hint_self, prop.hint_parent, importer, tree)
    }

    /// Whether this constructor write is the property's initialization: its own class
    /// declares it, with nothing in it to drop.
    fn initializes(&self, prop: &PropertyDecl, owner: &str) -> bool {
        let own = self.frame.class_fqn.map(|own| self.cx.class_identity(own));
        own.as_deref() == Some(owner)
            && !prop.is_static
            && !prop.promoted
            && (!prop.has_default || matches!(prop.default, Some(ArgValue::Null)))
    }

    /// Whether a hint, written in `tree`'s file for a property of `owner`, names a class that
    /// may run a destructor when a value of it is dropped: the class or any subclass.
    fn hint_reaches(
        &self,
        names: &[NameRef],
        has_self: bool,
        has_parent: bool,
        owner: &str,
        tree: &SourceTree,
    ) -> bool {
        let mut classes: Vec<String> = names.iter().map(|r| tree.resolve_class_fqn(r)).collect();
        if has_self {
            classes.push(owner.to_owned());
        }
        if has_parent {
            classes.extend(self.cx.parent_fqn(owner));
        }
        classes.iter().any(|c| {
            reaches_destructor(self.cx, c, self.cx.class_has_no_subclass(c), &mut HashSet::new())
        })
    }
}
