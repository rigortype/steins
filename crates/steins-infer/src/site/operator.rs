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
//! out of scope (ADR-0099 §7.4), and destructors are ADR-0099 §7.1's.

mod chain;
mod family;

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
    let mut op = Operator {
        cx,
        frame,
        gap: gap_kind(family),
        family,
        construct,
        member,
        out: ResolvedSite::default(),
    };
    let shapes = site.operands.as_deref().unwrap_or(&[]);
    for (position, shape) in shapes.iter().enumerate() {
        match op.subject(shape, receivers.get(position).and_then(Option::as_ref)) {
            Subject::NoObject => {}
            Subject::Opaque => op.gap(),
            Subject::Classes(bounds) => bounds.iter().for_each(|bound| op.class(bound, 0)),
        }
    }
    op.out
}

/// The `__call` and `__callStatic` bodies a method call on an *exact* class the
/// chain does not declare the method on runs (ADR-0099 §4.3's Call family):
/// the edges that replace the [`GapKind::MethodNotFound`] of a class that
/// declares one. Empty for a bound receiver, whose subclass may declare the
/// missing method itself, and for a class declaring neither.
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
    ["__call", "__callStatic"]
        .into_iter()
        .filter_map(|method| match chain::lookup(cx, None, &class, method) {
            chain::Lookup::Found { sym, .. } => Some(sym),
            _ => None,
        })
        .collect()
}

/// The coverage gap a family's unpinned site records.
const fn gap_kind(family: F) -> GapKind {
    match family {
        F::ToString => GapKind::OperatorToString,
        F::MagicProp => GapKind::OperatorMagicProperty,
        F::ArrayAccess => GapKind::OperatorArrayAccess,
        F::Iterate => GapKind::OperatorIteration,
        F::Clone => GapKind::OperatorClone,
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

    /// What the operand `shape`, written `receiver`, may be.
    fn subject(&self, shape: &ArgShape, receiver: Option<&EffectRecv>) -> Subject {
        let comparison = matches!(self.construct, C::LooseCompare | C::OrderCompare | C::Switch);
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
