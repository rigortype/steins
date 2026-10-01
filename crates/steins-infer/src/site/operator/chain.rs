//! A class's chain as the operator resolver reads it (ADR-0099 §4.3): what the
//! project declares along the parent line and the interfaces, whether that
//! picture is *closed*, and the lookups on it.
//!
//! A chain is **closed** when every ancestor, trait and interface is something
//! this analysis can read: a project class-like (one unambiguous declaration, in
//! a file that parsed, using no trait, since a trait's body is not lowered
//! against its user) or a class the engine's catalog knows and the project does
//! not declare. The engine's own classes contribute no *project* code to a
//! property fetch, a clone or a method lookup, so a chain that ends at one is
//! closed for those questions; the questions where an engine class does run
//! code (a string conversion, an offset access, an iteration) are the
//! catalog's is-a verdicts and are asked of [`Cx::supertype_walk`] instead.

use std::collections::HashSet;

use steins_syntax::{ClassDecl, MethodDecl, NativeType, PropertyDecl};

use crate::Sym;
use crate::cx::Cx;
use crate::dispatch::{Resolution, resolve_in_chain};

/// The four methods the engine runs on a property access.
pub(super) const PROPERTY_MAGIC: [&str; 4] = ["__get", "__set", "__isset", "__unset"];

/// One class's chain: the project declarations on it and whether it is closed.
pub(super) struct Chain<'a> {
    /// The class and each ancestor along `extends`, nearest first. For an
    /// interface the "parent" is the first interface it extends.
    lineage: Vec<&'a ClassDecl>,
    /// Every other project interface the walk reached through an `implements`
    /// list or a further `extends`.
    interfaces: Vec<&'a ClassDecl>,
    /// Whether every node of the walk is readable (see the module doc).
    pub(super) closed: bool,
}

impl<'a> Chain<'a> {
    pub(super) fn of(cx: &Cx<'a>, fqn: &str) -> Self {
        let mut chain = Self { lineage: Vec::new(), interfaces: Vec::new(), closed: true };
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending: Vec<String> = Vec::new();
        let mut next = Some(fqn.to_owned());
        while let Some(name) = next.take() {
            if seen.insert(name.to_ascii_lowercase()) {
                next = chain.visit(cx, &name, true, &mut pending);
            }
        }
        while let Some(name) = pending.pop() {
            if seen.insert(name.to_ascii_lowercase())
                && let Some(parent) = chain.visit(cx, &name, false, &mut pending)
            {
                pending.push(parent);
            }
        }
        chain
    }

    /// Record `name`, queue its interfaces, and return its parent.
    fn visit(
        &mut self,
        cx: &Cx<'a>,
        name: &str,
        on_lineage: bool,
        pending: &mut Vec<String>,
    ) -> Option<String> {
        let Some((file, cd)) = cx.find_class(name) else {
            self.closed &= is_engine_class(cx, name);
            return None;
        };
        self.closed &= !cx.member_incomplete(file) && !cd.uses_traits;
        if on_lineage { self.lineage.push(cd) } else { self.interfaces.push(cd) }
        let tree = cx.units[file].tree;
        pending.extend(cd.implements.iter().map(|r| tree.resolve_class_fqn(r)));
        cd.parent.as_ref().map(|r| tree.resolve_class_fqn(r))
    }

    fn all(&self) -> impl Iterator<Item = &'a ClassDecl> + '_ {
        self.lineage.iter().chain(&self.interfaces).copied()
    }

    /// Whether any project class-like of the chain declares one of `names`.
    pub(super) fn declares_any(&self, names: &[&str]) -> bool {
        self.all().any(|cd| declares_any(cd, names))
    }

    /// Whether any project class-like of the chain hooks the property `member`,
    /// or, for a name the site cannot read, any property at all.
    pub(super) fn hooks(&self, member: Option<&str>) -> bool {
        self.all().any(|cd| hooks(cd, member))
    }

    /// Whether `fqn` is on the chain (the class itself included).
    pub(super) fn has(&self, fqn: &str) -> bool {
        self.all().any(|cd| cd.fqn.eq_ignore_ascii_case(fqn))
    }

    /// The nearest non-static property named `name` the lineage declares, with
    /// the class declaring it.
    pub(super) fn property(&self, name: &str) -> Option<(&'a PropertyDecl, &'a ClassDecl)> {
        self.lineage.iter().find_map(|cd| {
            cd.properties.iter().find(|p| !p.is_static && p.name == name).map(|p| (p, *cd))
        })
    }

    /// The nearest declaration of `method` on the lineage, abstract or not.
    fn method(&self, method: &str) -> Option<&'a MethodDecl> {
        self.lineage
            .iter()
            .find_map(|cd| cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)))
    }
}

fn declares_any(cd: &ClassDecl, names: &[&str]) -> bool {
    cd.methods.iter().any(|m| names.iter().any(|n| m.name.eq_ignore_ascii_case(n)))
}

fn hooks(cd: &ClassDecl, member: Option<&str>) -> bool {
    match member {
        Some(p) => {
            cd.hooked_properties.iter().any(|h| h == p)
                || cd.properties.iter().any(|q| q.hooked && q.name == p)
        }
        None => !cd.hooked_properties.is_empty() || cd.properties.iter().any(|q| q.hooked),
    }
}

/// Whether `name` is an engine class: one no project file declares and the
/// catalog's hierarchy states.
fn is_engine_class(cx: &Cx, name: &str) -> bool {
    cx.class_absent(name) && steins_catalog::builtin_class_supers(name).is_some()
}

/// What a lookup of a magic or interface method on a class finds.
pub(super) enum Lookup<'a> {
    /// A project body the engine runs: the edge, whether no subclass can replace
    /// it, and its declared return type.
    Found { sym: Sym, is_final: bool, ret: Option<&'a NativeType> },
    /// The project declares no such method anywhere on a closed chain: nothing
    /// of the project's answers (the engine's own class may).
    Absent,
    /// The chain cannot be read, or the method has no body to run.
    Unknown,
}

/// Look `method` up on `class`'s chain, as a call to it would resolve. `chain`
/// is the class's chain when the caller holds it; a caller that treats
/// [`Lookup::Absent`] and [`Lookup::Unknown`] alike passes `None`.
pub(super) fn lookup<'a>(
    cx: &Cx<'a>,
    chain: Option<&Chain<'a>>,
    class: &str,
    method: &str,
) -> Lookup<'a> {
    match resolve_in_chain(cx, class, method) {
        Resolution::Found(r) => Lookup::Found {
            sym: Sym::Method(r.declaring_class.fqn.clone(), r.method.name.clone()),
            is_final: r.method.is_final || r.declaring_class.is_final || r.declaring_class.is_enum,
            ret: r.method.ret.as_ref(),
        },
        Resolution::NotFoundChainComplete => Lookup::Absent,
        // The walk leaves the project, or meets an abstract declaration, or a
        // trait user. A closed chain with no declaration of the name on it left
        // the project at an engine class.
        Resolution::Unknown
            if chain.is_some_and(|c| c.closed && c.method(method).is_none()) =>
        {
            Lookup::Absent
        }
        Resolution::Unknown => Lookup::Unknown,
    }
}

/// Whether a class in the universe other than `class`'s own chain, but extending
/// it, declares a property magic method or hooks `member` (ADR-0099 §4.4): such
/// a subclass can stand in for a bound `class` at the access.
///
/// A declaring class the index holds under more than one declaration cannot be
/// placed in the hierarchy, so it counts.
pub(super) fn subclass_adds_property_magic(cx: &Cx, class: &str, member: Option<&str>) -> bool {
    cx.index.magic_property_classes().iter().any(|sub| match cx.find_class(sub) {
        None => true,
        Some((_, cd)) => {
            (declares_any(cd, &PROPERTY_MAGIC) || hooks(cd, member))
                && Chain::of(cx, sub).has(class)
        }
    })
}
