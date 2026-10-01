//! A class's chain as the operator resolver reads it (ADR-0099 §4.3): what the
//! project declares along the parent line and the interfaces, whether that
//! picture is *closed*, and the lookups on it.
//!
//! A chain is **closed** when every ancestor class and trait is something this
//! analysis can read: a project class-like (one unambiguous declaration, using
//! no trait, since a trait's body is not lowered against its user) or a class
//! the engine's catalog knows and the project does not declare. An *interface*
//! the project cannot read does not open a chain: it has no body to run. The
//! engine's own classes contribute no *project* code to a property fetch, a
//! clone or a method lookup, so a chain that ends at one is closed for those
//! questions (`ArrayObject` and `ArrayIterator` excepted: their property access
//! can be an offset access); the questions where an engine class does run
//! code (a string conversion, an offset access, an iteration) are the
//! catalog's is-a verdicts and are asked of [`Cx::supertype_walk`] instead.

use std::collections::HashSet;

use steins_syntax::{ClassDecl, MethodDecl, NativeType, PropertyDecl};

use crate::Sym;
use crate::cx::Cx;
use crate::dispatch::{Resolution, resolve_in_chain};

/// The four methods the engine runs on a property access.
pub(super) const PROPERTY_MAGIC: [&str; 4] = ["__get", "__set", "__isset", "__unset"];

/// A name the chain walk reaches, and whether the syntax that named it makes it
/// an interface (an `implements` entry, or what an interface extends).
struct Ancestor {
    name: String,
    interface: bool,
}

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
        let mut pending: Vec<Ancestor> = Vec::new();
        let mut next = Some(Ancestor { name: fqn.to_owned(), interface: false });
        while let Some(node) = next.take() {
            if seen.insert(node.name.to_ascii_lowercase()) {
                next = chain.visit(cx, &node, true, &mut pending);
            }
        }
        while let Some(node) = pending.pop() {
            if seen.insert(node.name.to_ascii_lowercase())
                && let Some(parent) = chain.visit(cx, &node, false, &mut pending)
            {
                pending.push(parent);
            }
        }
        chain
    }

    /// Record `node`, queue its interfaces, and return its parent.
    fn visit(
        &mut self,
        cx: &Cx<'a>,
        node: &Ancestor,
        on_lineage: bool,
        pending: &mut Vec<Ancestor>,
    ) -> Option<Ancestor> {
        let Some((file, cd)) = cx.find_class(&node.name) else {
            // An interface the project cannot read carries no body: nothing it
            // declares can run on a property access, a clone or a method lookup.
            self.closed &= node.interface || is_engine_class(cx, &node.name);
            return None;
        };
        self.closed &= !cd.uses_traits;
        if on_lineage { self.lineage.push(cd) } else { self.interfaces.push(cd) }
        let tree = cx.units[file].tree;
        pending.extend(
            cd.implements
                .iter()
                .map(|r| Ancestor { name: tree.resolve_class_fqn(r), interface: true }),
        );
        let parent = cd.parent.as_ref()?;
        Some(Ancestor { name: tree.resolve_class_fqn(parent), interface: cd.is_interface })
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

/// Whether `name` is an engine class a chain may end at: one no project file
/// declares, the catalog's hierarchy states, and that runs no project code on a
/// property access. `ArrayObject` and `ArrayIterator` (and what extends them)
/// do: with `ARRAY_AS_PROPS` a property fetch is an offset access, which is the
/// user `offset*` of a subclass.
fn is_engine_class(cx: &Cx, name: &str) -> bool {
    cx.class_absent(name)
        && steins_catalog::builtin_class_supers(name).is_some()
        && !routes_properties_to_offsets(name)
}

/// Whether the engine class `name` is, or extends, `ArrayObject` or `ArrayIterator`.
fn routes_properties_to_offsets(name: &str) -> bool {
    let mut pending = vec![name.to_owned()];
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(cur) = pending.pop() {
        let key = cur.trim_start_matches('\\').to_ascii_lowercase();
        if key == "arrayobject" || key == "arrayiterator" {
            return true;
        }
        if seen.insert(key) {
            pending.extend(
                steins_catalog::builtin_class_supers(&cur)
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned),
            );
        }
    }
    false
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
/// Three shapes of subclass count, all read off tables the shard builder fills
/// (so no tree is loaded to ask):
///
/// * a class declaring a magic method, or hooking `member`, whose chain reaches
///   `class`;
/// * a class importing a trait, whose body is not lowered, so what it brings
///   (`__get` among it) is unknown;
/// * an anonymous class, which no index lists: any one extending or implementing
///   `class` or something under it counts, whatever its body declares, as
///   `declared_receiver`'s descendant closure reads it (ADR-0049 A4).
///
/// A declaring class the index holds under more than one declaration cannot be
/// placed in the hierarchy, so it counts.
///
/// Warm runs stay correct because the generation's affected set reaches the files
/// involved: its inheritance leg closes over the supertypes of every changed file,
/// so editing a subclass re-resolves the file of the class it extends and the
/// files naming that class.
pub(super) fn subclass_adds_property_magic(cx: &Cx, class: &str, member: Option<&str>) -> bool {
    let declaring = cx.index.magic_property_classes().iter().any(|sub| match cx.find_class(sub) {
        None => true,
        Some((_, cd)) => {
            (cd.uses_traits || declares_any(cd, &PROPERTY_MAGIC) || hooks(cd, member))
                && Chain::of(cx, sub).has(class)
        }
    });
    declaring
        || cx.index.anonymous_subclass_parents().iter().any(|parent| {
            parent.eq_ignore_ascii_case(class) || Chain::of(cx, parent).has(class)
        })
}
