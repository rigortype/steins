//! The operator sites' resolution (ADR-0099 §4.3, §4.4, issue #859): what user
//! code the engine runs at an operator through an operand's class, and the
//! coverage gap when the site cannot say.
//!
//! The operator syntax lowers to a [`SiteKind::Operator`] naming a *family* (a
//! string conversion, a property access, an offset access, an iteration, a
//! clone) and, per operand, the shape the operand is shown to hold and what names
//! its class. This module answers the family's question for each operand:
//!
//! 1. An operand shown **not to be an object** ([`Frame::held`]) runs nothing.
//! 2. Otherwise the operand names a class, *exactly* (a `new Foo`, `$this` or a
//!    declared type in a final class: no subclass can stand in) or as a *bound*
//!    (anything a subclass may stand in for). A declared union is ruled out only
//!    when every member is; a member the lowering does not model (`array|Foo`,
//!    `mixed`, `iterable`) is an operand nothing is known of, and an operand
//!    nothing is known of is a gap.
//! 3. The family's rule decides from the class: nothing runs, the user method it
//!    runs is an edge (carrying its labels and throws like any method call's),
//!    or the site is a gap ([`GapKind::OperatorToString`] and its siblings).
//!
//! The answer is shared by both lanes: what runs at `"a" . $o` does not depend on
//! which axis reads it. The engine's own `Error`-family raises at operators stay
//! out of scope (ADR-0099 §7.4). A drop is the one family that is not an operand's
//! use: [`drops`] answers whether the value a frame releases may run a destructor.

mod chain;
mod coerce;
mod drops;
mod family;

pub(super) use coerce::{
    Signature, arguments as coerce_arguments, callback_signature, forwarded_arguments,
    method_signature,
};

use steins_syntax::{
    ArgShape, EffectRecv, NativeType, OperatorConstruct as C, OperatorFamily as F, SiteOrigin,
    TypeMember,
};

use super::reach::{Frame, Held};
use super::{Edge, GapKind, ResolvedSite};
use crate::Sym;
use crate::contract::IsA;
use crate::cx::Cx;

/// Resolve one operator site: the same answer in either lane.
pub(super) fn resolve<'a>(
    cx: &Cx<'a>,
    frame: &Frame<'a>,
    site: &SiteOrigin,
    (family, construct): (F, C),
    receivers: &[Option<EffectRecv>],
    member: Option<&str>,
) -> ResolvedSite {
    if family == F::Drop {
        return drops::resolve(cx, frame, construct, receivers, member);
    }
    // The Coerce row's value sites: a return and a constructor's typed-property write.
    if matches!(construct, C::Return | C::PropertyValue) {
        return coerce::value(cx, frame, site, (construct, receivers, member));
    }
    let mut op = Operator {
        cx,
        frame,
        gap: gap_kind(family),
        family,
        construct,
        member,
        out: ResolvedSite::default(),
    };
    let mut shapes = site.operands.as_deref().unwrap_or(&[]);
    if let (C::OffsetValue, [_, container, ..]) = (construct, shapes) {
        // The value converts only into a string container, which an offset write on
        // anything else (an array, an `ArrayAccess` object) does not make of it.
        if frame.container_not_string(cx, container) {
            return op.out;
        }
        shapes = &shapes[..1];
    }
    if op.two_objects_compared(shapes) {
        op.gap();
    }
    for (position, shape) in shapes.iter().enumerate() {
        match op.subject(shape, receivers.get(position).and_then(Option::as_ref)) {
            Subject::NoObject => {}
            Subject::Opaque => op.gap(),
            Subject::Classes(bounds) => bounds.iter().for_each(|bound| op.class(bound, 0)),
        }
    }
    op.out
}

/// Whether the engine's code run on an object of `class` may run a project
/// property hook ([`chain::hooks_property`]): a hook on the chain, and for a
/// `class` that is only a bound, one on a class that may stand in for it.
pub(super) fn engine_chain_hooks(cx: &Cx<'_>, class: &str, exact: bool) -> bool {
    chain::hooks_property(cx, class, exact)
}

/// The `__call` and `__callStatic` bodies a method call on an *exact* class the
/// chain does not declare the method on runs (ADR-0099 §4.3's Call family):
/// the edges that replace the [`GapKind::MethodNotFound`] of a class that
/// declares one. Empty for a bound receiver, whose subclass may declare the
/// missing method itself, and for a class declaring neither.
///
/// A class-name receiver is `new Foo`, `Foo::` or `parent::`, which this record
/// does not tell apart, so the named class's `__call` and `__callStatic` both
/// count. A call inside a class that is, or may be, an instance of the named
/// class (`parent::missing()` in an instance method) runs `$this`'s own
/// `__call`, which a subclass may override: that one is an edge only where the
/// enclosing class is final or its `__call` is, and otherwise the whole answer is
/// empty, the gap staying. A frame whose `$this` cannot be an instance of the
/// named class runs the named class's methods.
pub(super) fn magic_call_edges(
    cx: &Cx<'_>,
    enclosing: Option<&str>,
    receiver: &EffectRecv,
) -> Vec<Sym> {
    let class = match receiver {
        EffectRecv::ClassName(name) => cx.class_fqn(name),
        EffectRecv::Parent => match enclosing.and_then(|e| cx.parent_fqn(e)) {
            Some(parent) => parent,
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let mut edges: Vec<Sym> = ["__call", "__callStatic"]
        .into_iter()
        .filter_map(|method| match chain::lookup(cx, None, &class, method) {
            chain::Lookup::Found { sym, .. } => Some(sym),
            _ => None,
        })
        .collect();
    if let Some(enclosing) = enclosing.filter(|e| cx.is_a(e, &class) != IsA::No) {
        let exact = cx.class_has_no_subclass(enclosing);
        match chain::lookup(cx, None, enclosing, "__call") {
            chain::Lookup::Found { sym, is_final, .. } if exact || is_final => edges.push(sym),
            chain::Lookup::Absent if exact => {}
            _ => return Vec::new(),
        }
    }
    edges
}

/// The coverage gap a family's unpinned site records.
const fn gap_kind(family: F) -> GapKind {
    match family {
        F::ToString => GapKind::OperatorToString,
        F::MagicProp => GapKind::OperatorMagicProperty,
        F::ArrayAccess => GapKind::OperatorArrayAccess,
        F::Iterate => GapKind::OperatorIteration,
        F::Clone => GapKind::OperatorClone,
        F::Drop => GapKind::Destructor,
    }
}

/// A class an operand may be, and whether the operand is exactly that class (no
/// subclass can stand in) or only bounded by it.
struct Bound {
    fqn: String,
    exact: bool,
}

/// What an operand is, for the family's rule.
enum Subject {
    /// Shown not to be an object.
    NoObject,
    /// May be an object of any of these classes.
    Classes(Vec<Bound>),
    /// May be an object nothing names.
    Opaque,
}

struct Operator<'a, 'f> {
    cx: &'f Cx<'a>,
    frame: &'f Frame<'a>,
    family: F,
    construct: C,
    member: Option<&'f str>,
    gap: GapKind,
    out: ResolvedSite,
}

impl<'a> Operator<'a, '_> {
    fn gap(&mut self) {
        self.out.gaps.insert(self.gap);
    }

    fn edge(&mut self, sym: Sym) {
        self.out.targets.push(Edge::call(sym));
    }

    /// Whether a comparison sets two operands against each other that may both be
    /// objects (a `switch` compares its subject with each `case` in turn). Two
    /// objects of one class compare property by property, recursively through
    /// arrays, and any property pair may convert an object to a string, so the
    /// site is a gap whatever the classes are. An operand's own class answers only
    /// against an operand shown to hold no object at any depth.
    fn two_objects_compared(&self, shapes: &[ArgShape]) -> bool {
        let comparison = matches!(self.construct, C::LooseCompare | C::OrderCompare | C::Switch);
        let may_be_object =
            |shape: &ArgShape| self.frame.held(self.cx, shape) != Held::ObjectFree;
        if !comparison || shapes.len() < 2 || !may_be_object(&shapes[0]) {
            return false;
        }
        match self.construct {
            C::Switch => shapes[1..].iter().any(may_be_object),
            _ => may_be_object(&shapes[1]),
        }
    }

    /// What the operand `shape`, written `receiver`, may be.
    fn subject(&self, shape: &ArgShape, receiver: Option<&EffectRecv>) -> Subject {
        let comparison = matches!(self.construct, C::LooseCompare | C::OrderCompare | C::Switch);
        // A class constant is a scalar, an array or an enum case, which cannot declare
        // `__toString`: nothing is converted. A comparison is as it was, and so is every other
        // family (an enum may be `Countable` or `ArrayAccess`).
        if self.family == F::ToString && !comparison && *shape == ArgShape::ClassConst {
            return Subject::NoObject;
        }
        match self.frame.held(self.cx, shape) {
            Held::ObjectFree => return Subject::NoObject,
            // An array is not an object; a comparison still compares its elements.
            Held::NonObject if !comparison => return Subject::NoObject,
            _ => {}
        }
        let (cx, class) = (self.cx, self.frame.class_fqn);
        match receiver {
            Some(EffectRecv::This) => match class.and_then(|c| cx.find_class(c)) {
                Some((_, cd)) if !cd.is_trait => Subject::Classes(vec![Bound {
                    fqn: cd.fqn.clone(),
                    exact: cd.is_final || cd.is_enum,
                }]),
                _ => Subject::Opaque,
            },
            Some(EffectRecv::ClassName(name)) => {
                Subject::Classes(vec![Bound { fqn: cx.class_fqn(name), exact: true }])
            }
            Some(recv @ (EffectRecv::Var(_) | EffectRecv::PropRead(_))) => {
                match super::method::declared_receiver_type(cx, class, self.frame.params, recv) {
                    Some(ty) => self.declared(ty),
                    None => Subject::Opaque,
                }
            }
            _ => Subject::Opaque,
        }
    }

    /// The classes a declared type admits: its scalar members hold no object and
    /// `null` holds none, a class member is a bound (exact when the class is
    /// final), and an intersection is not modeled.
    fn declared(&self, ty: &NativeType) -> Subject {
        let mut bounds = Vec::new();
        for member in &ty.members {
            match member {
                TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => {}
                TypeMember::Instance { fqn, .. } => bounds.push(Bound {
                    fqn: fqn.clone(),
                    exact: self.cx.class_has_no_subclass(fqn),
                }),
                TypeMember::InstanceInter(_) => return Subject::Opaque,
            }
        }
        if bounds.is_empty() { Subject::NoObject } else { Subject::Classes(bounds) }
    }

    /// The trinary is-a verdict of `fqn` against `target`, with the engine
    /// catalog's `No` demoted when the project's PHP minor differs from the
    /// catalog's pin (ADR-0052 A11).
    fn is_a(&self, fqn: &str, target: &str) -> IsA {
        let walk = self.cx.supertype_walk(fqn, target);
        if walk.verdict == IsA::No && walk.catalog && self.cx.a11_demote_catalog() {
            return IsA::Unknown;
        }
        walk.verdict
    }
}
