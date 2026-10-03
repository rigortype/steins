//! The property drop sites (ADR-0100 §7, issue #1003): a plain write to a property, or an
//! `unset` of one, releases the value it held, and a destructor that value reaches runs at
//! the statement. The holder is named without a flow environment: `$this`, or a static
//! property of `self`, `static`, `parent` or a named class. Which property that is and what
//! its declared hint reaches is the resolver's question; this lowering says where the
//! write is and, for a constructor, whether it is the first touch of its property.

use mago_syntax::cst::{
    Access, Assignment, ClassLikeMemberSelector, Expression, Variable,
};

use super::super::SiteScope;
use super::site;
use crate::ast::{EffectRecv, OperatorConstruct as C, SiteOrigin};
use crate::lower_expr::method_name_of;
use crate::names::name_ref;
use crate::{bytes_to_string, strip_dollar, to_span};

/// The property a `$this->p` fetch names, when `object` is `$this` and `p` is a literal.
pub(super) fn this_property(
    object: &Expression<'_>,
    selector: &ClassLikeMemberSelector<'_>,
) -> Option<String> {
    let Expression::Variable(Variable::Direct(dv)) = object.unparenthesized() else { return None };
    if strip_dollar(bytes_to_string(dv.name)) != "this" {
        return None;
    }
    match selector {
        ClassLikeMemberSelector::Identifier(_) => method_name_of(selector),
        _ => None,
    }
}

/// The property `expr` is, when it is `$this->p`.
pub(super) fn this_property_of(expr: &Expression<'_>) -> Option<String> {
    match expr.unparenthesized() {
        Expression::Access(Access::Property(pa)) => this_property(pa.object, &pa.property),
        _ => None,
    }
}

/// The holder and name of a property `expr` names: `$this->p`, or `C::$p` for `self`,
/// `static` (a static property's type is the same in every subclass), `parent` or a
/// named class.
fn holder(expr: &Expression<'_>) -> Option<(EffectRecv, String)> {
    match expr.unparenthesized() {
        Expression::Access(Access::Property(pa)) => {
            this_property(pa.object, &pa.property).map(|name| (EffectRecv::This, name))
        }
        Expression::Access(Access::StaticProperty(sp)) => {
            let Variable::Direct(dv) = &sp.property else { return None };
            let receiver = match sp.class.unparenthesized() {
                Expression::Self_(_) | Expression::Static(_) => EffectRecv::SelfKw,
                Expression::Parent(_) => EffectRecv::Parent,
                Expression::Identifier(id) => EffectRecv::ClassName(name_ref(id)),
                _ => return None,
            };
            Some((receiver, strip_dollar(bytes_to_string(dv.name))))
        }
        _ => None,
    }
}

/// The site of a plain write to a property.
pub(super) fn write(a: &Assignment<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let Some((receiver, member)) = holder(a.lhs) else { return };
    let span = to_span(mago_span::HasSpan::span(a));
    let init = receiver == EffectRecv::This && sx.cx.drops.is_init_write(span.start);
    let construct = if init { C::DropPropInit } else { C::DropPropWrite };
    out.push(site(sx, span, construct, &[Some(receiver)], Some(member)));
}

/// The site of an `unset` of `$this->p`. A static property cannot be unset.
pub(super) fn unset(value: &Expression<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let Some(member) = this_property_of(value) else { return };
    let span = to_span(mago_span::HasSpan::span(value));
    out.push(site(sx, span, C::DropPropUnset, &[Some(EffectRecv::This)], Some(member)));
}
