//! The five families' rules (ADR-0099 §4.3's table, §4.4), one class at a time.
//!
//! Every rule has the same two outcomes for a class it can place: nothing runs,
//! or the user method the engine runs is an edge. What it cannot place is the
//! family's gap. The difference between an *exact* class and a *bound* is the
//! one the table draws: a method is an edge on a bound only where no subclass can
//! replace it (it is final, or declared in a final class), and "nothing runs" is
//! proven for an exact class only, since a subclass of a bound may add the method.

use steins_syntax::{NativeType, OperatorConstruct as C, OperatorFamily as F, TypeMember};

use super::chain::{Chain, Lookup, PROPERTY_MAGIC, lookup, subclass_adds_property_magic};
use super::{Bound, Operator};
use crate::contract::IsA;

/// The methods an `Iterator` runs over a `foreach`, in the order it calls them.
const ITERATOR_METHODS: [&str; 5] = ["rewind", "valid", "current", "key", "next"];

/// The engine iterator a `getIterator` may return that runs no user code of its
/// own: a generator's body is the `getIterator` body, already an edge, and
/// `Generator` is final. `ArrayIterator` is not here: user code can subclass it,
/// and no table lists the subclasses (anonymous ones included), so it is a gap.
const ENGINE_ITERATOR: &str = "generator";

/// How deep a `getIterator` that returns another iterator-bearing class is
/// followed before the site is a gap.
const ITERATOR_DEPTH: u8 = 2;

impl<'a> Operator<'a, '_> {
    /// Apply the site's family to one class an operand may be.
    pub(super) fn class(&mut self, bound: &Bound, depth: u8) {
        match self.family {
            F::ToString => self.string_conversion(bound),
            F::MagicProp => self.magic_property(bound),
            F::ArrayAccess => self.array_access(bound),
            F::Iterate => self.iterate(bound, depth),
            F::Clone => self.clone_object(bound),
        }
    }

    /// The user method `method` that the engine runs on `bound`, which the class
    /// must have: an edge where it is found and nothing can replace it, else the
    /// gap. For an exact class nothing can replace it; for a bound, only a final
    /// method (or a class that is final) is safe.
    fn required(&mut self, bound: &Bound, method: &str) {
        match lookup(self.cx, None, &bound.fqn, method) {
            Lookup::Found { sym, is_final, .. } if bound.exact || is_final => self.edge(sym),
            _ => self.gap(),
        }
    }

    /// The user method `method` that the engine runs on `bound` if the class
    /// declares it. An exact class on a closed chain with no declaration runs
    /// nothing; a bound never proves that (a subclass may add the method).
    fn optional(&mut self, bound: &Bound, chain: &Chain<'a>, method: &str) {
        match lookup(self.cx, Some(chain), &bound.fqn, method) {
            Lookup::Found { sym, is_final, .. } if bound.exact || is_final => self.edge(sym),
            Lookup::Absent if bound.exact && chain.closed => {}
            _ => self.gap(),
        }
    }

    // ---- ToString ----------------------------------------------------------

    /// `__toString`: an exact class proven not `Stringable` runs nothing; one that
    /// is runs its method; a bound runs it only where it is final.
    fn string_conversion(&mut self, bound: &Bound) {
        if !bound.exact {
            return self.required(bound, "__toString");
        }
        match self.is_a(&bound.fqn, "Stringable") {
            IsA::No => {}
            IsA::Yes => self.required(bound, "__toString"),
            IsA::Unknown => self.gap(),
        }
    }

    // ---- ArrayAccess -------------------------------------------------------

    /// The `offset*` methods of the access's role, for a class that is
    /// `ArrayAccess`; an exact class proven not to be runs nothing.
    fn array_access(&mut self, bound: &Bound) {
        match (self.is_a(&bound.fqn, "ArrayAccess"), bound.exact) {
            (IsA::No, true) => {}
            (IsA::Yes, _) => {
                for method in offset_methods(self.construct) {
                    self.required(bound, method);
                }
            }
            _ => self.gap(),
        }
    }

    // ---- Iterate -----------------------------------------------------------

    /// A class iterated: a non-`Traversable` exact class iterates its properties
    /// (nothing runs, unless one is hooked), an `Iterator` runs its five methods,
    /// and an `IteratorAggregate` its `getIterator` and then the iterator that
    /// returns.
    fn iterate(&mut self, bound: &Bound, depth: u8) {
        match (self.is_a(&bound.fqn, "Traversable"), bound.exact) {
            (IsA::No, true) => {
                let chain = Chain::of(self.cx, &bound.fqn);
                if !chain.closed || chain.hooks(None) {
                    self.gap();
                }
            }
            (IsA::Yes, _) if self.is_a(&bound.fqn, "Iterator") == IsA::Yes => {
                for method in ITERATOR_METHODS {
                    self.required(bound, method);
                }
            }
            (IsA::Yes, _) if self.is_a(&bound.fqn, "IteratorAggregate") == IsA::Yes => {
                self.aggregate(bound, depth);
            }
            _ => self.gap(),
        }
    }

    /// `getIterator`, and the iterator it is shown to return: covered only where
    /// the declared return type names one class this rule can place.
    fn aggregate(&mut self, bound: &Bound, depth: u8) {
        let Lookup::Found { sym, is_final, ret } =
            lookup(self.cx, None, &bound.fqn, "getIterator")
        else {
            return self.gap();
        };
        if !(bound.exact || is_final) {
            return self.gap();
        }
        self.edge(sym);
        match returned_class(ret) {
            Some(ENGINE_ITERATOR) => {}
            Some(fqn) if depth < ITERATOR_DEPTH => {
                let exact = self.cx.class_has_no_subclass(fqn);
                self.iterate(&Bound { fqn: fqn.to_owned(), exact }, depth + 1);
            }
            _ => self.gap(),
        }
    }

    // ---- Clone -------------------------------------------------------------

    /// `__clone` (and, for `clone` with a property list, `__set`): an exact class
    /// on a closed chain with no declaration, hooking no property, runs nothing.
    fn clone_object(&mut self, bound: &Bound) {
        let chain = Chain::of(self.cx, &bound.fqn);
        if bound.exact && (!chain.closed || chain.hooks(None)) {
            return self.gap();
        }
        self.optional(bound, &chain, "__clone");
        if self.construct == C::CloneWith {
            self.optional(bound, &chain, "__set");
        }
    }

    // ---- MagicProp ---------------------------------------------------------

    /// A property access: a hooked property is a gap (a hook body is not lowered
    /// as a body of its own, so there is no edge to draw); a declared property
    /// visible from the accessing scope runs a magic method only where the
    /// property is unset and some class declares one, which an exact class's
    /// chain states and a bound answers with §4.4's universe gate; any other name
    /// reaches `__get`/`__set`/`__isset`/`__unset` and is an edge or a gap.
    fn magic_property(&mut self, bound: &Bound) {
        let chain = Chain::of(self.cx, &bound.fqn);
        if !chain.closed || chain.hooks(self.member) {
            return self.gap();
        }
        let declared = self.member.is_some_and(|member| self.visible_property(&chain, member));
        if declared && !bound.exact {
            return self.declared_property_gate(bound, &chain);
        }
        for method in property_methods(self.construct) {
            if declared || bound.exact {
                self.optional(bound, &chain, method);
            } else {
                self.required(bound, method);
            }
        }
    }

    /// §4.4: the access to a bound class's declared, visible property runs
    /// nothing unless the class's closed chain or some subclass in the universe
    /// declares a property magic method or a hook.
    fn declared_property_gate(&mut self, bound: &Bound, chain: &Chain<'a>) {
        let adds = chain.declares_any(&PROPERTY_MAGIC)
            || subclass_adds_property_magic(self.cx, &bound.fqn, self.member);
        if adds {
            self.gap();
        }
    }

    /// Whether the chain declares the property `member` and the accessing scope
    /// sees it: public, or private to the scope's own class, or protected within
    /// the scope's hierarchy.
    fn visible_property(&self, chain: &Chain<'a>, member: &str) -> bool {
        use steins_syntax::Visibility;
        let Some((prop, owner)) = chain.property(member) else { return false };
        let scope = self.frame.class_fqn;
        match prop.visibility {
            Visibility::Public => true,
            Visibility::Private => scope.is_some_and(|s| s.eq_ignore_ascii_case(&owner.fqn)),
            Visibility::Protected => scope.is_some_and(|s| {
                s.eq_ignore_ascii_case(&owner.fqn)
                    || self.cx.is_a(s, &owner.fqn) == IsA::Yes
                    || self.cx.is_a(&owner.fqn, s) == IsA::Yes
            }),
        }
    }
}

/// The class a `getIterator` return type names, when it names exactly one.
fn returned_class(ret: Option<&NativeType>) -> Option<&str> {
    match ret?.members.as_slice() {
        [TypeMember::Instance { fqn, .. }] => Some(fqn),
        _ => None,
    }
}

/// The magic methods the engine runs for a property access of this role. An
/// `isset` chain and `empty` ask `__isset` and then `__get`; a compound
/// assignment reads and then writes.
fn property_methods(construct: C) -> &'static [&'static str] {
    match construct {
        C::Read | C::Reference => &["__get"],
        C::Write => &["__set"],
        C::ReadWrite | C::CoalesceAssign => &["__get", "__set", "__isset"],
        C::Isset | C::Empty | C::Coalesce => &["__isset", "__get"],
        C::Unset => &["__unset"],
        _ => &PROPERTY_MAGIC,
    }
}

/// The `ArrayAccess` methods the engine runs for an offset access of this role.
fn offset_methods(construct: C) -> &'static [&'static str] {
    match construct {
        C::Read | C::Reference | C::Destructure => &["offsetGet"],
        C::Write => &["offsetSet"],
        C::ReadWrite => &["offsetGet", "offsetSet"],
        C::Isset | C::Empty | C::Coalesce => &["offsetExists", "offsetGet"],
        C::CoalesceAssign => &["offsetExists", "offsetGet", "offsetSet"],
        C::Unset => &["offsetUnset"],
        _ => &["offsetGet", "offsetSet", "offsetExists", "offsetUnset"],
    }
}
