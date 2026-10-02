//! The operator sites (ADR-0099 §4.3, issue #859): the syntactic forms through
//! which the engine may run a user method on an operand, with no call written.
//! Converting an object to a string, fetching or storing a property on it,
//! indexing it, iterating it and cloning it each reach a magic method or an
//! interface method of the operand's class.
//!
//! A site records the operands that can be an object, as [`ArgShape`]s, and
//! what names each operand's class without a flow environment
//! ([`SiteKind::Operator::receivers`]). Which class that is, and what runs
//! there, is the resolver's question: this lowering is syntactic.
//!
//! **A site is not emitted** when every operand's shape shows it holds no
//! object ([`holds_no_object`]): an object-free expression or an array. A
//! variable is never skipped here, whatever the frame writes into it: the
//! resolver decides, since a call may take it by reference.
//!
//! **A comparison is stricter** (`==`, `!=`, `<>`, `<`, `<=`, `>`, `>=`,
//! `<=>`, `switch`): an array compares element-wise with another array, so
//! `[$o] == ['x']`, `[$o] < ['y']` and a `switch` over `[$o]` with `case ['x']`
//! run `__toString` on the element (witnessed on PHP 8.5). Only an operand that
//! holds no object at any depth qualifies: an object-free expression. A
//! comparison with a non-string scalar literal (`null`, a boolean, an integer
//! or a float) on either side is skipped whatever the other side holds: an
//! object converts to a string for a string operand only, and an array
//! against a scalar compares without touching its elements (also witnessed on
//! 8.5).
//!
//! A property or offset access takes the role its context gives it
//! ([`OperatorConstruct`]): [`chain`] walks an lvalue-like expression and
//! gives the outermost access the role and every access under it the role an
//! intermediate fetch has.

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, ArrayElement, AssignmentOperator, Binary, BinaryOperator,
    ClassLikeMemberSelector, DocumentString, Expression, FunctionCall, FunctionLikeParameterList,
    Literal, Node, StringPart, SwitchCase, UnaryPrefix, UnaryPrefixOperator, Variable,
};

use super::{SiteScope, scan_sites};
use crate::ast::{
    ArgShape, ConstArgs, EffectRecv, OperatorConstruct as C, OperatorFamily as F, SiteKind,
    SiteOrigin,
};
use crate::lower_arg_shape::{arg_shape, container_shape_of};
use crate::lower_expr::{effect_recv_of_object_declared, method_name_of};
use crate::stack_guard;
use crate::{bytes_to_string, strip_dollar, to_span};

/// The sites a constructor's promoted parameters run in its prologue, before any
/// statement of its body: a property hook on a promoted parameter (PHP 8.4) runs
/// its `set` hook when the constructor promotes the argument, a write to `$this`
/// that no statement spells. One MagicProp `Write` site on `$this` per hooked
/// promoted parameter, naming it, which resolves as an explicit `$this->p = …`
/// does (ADR-0099 §4.3): the hooked property is a gap. A promoted parameter with
/// no hook is not one: its write runs nothing unless a subclass hooks the
/// property, which is the explicit write's §4.4 question and is not asked here.
pub(crate) fn promoted_hook_sites(params: &FunctionLikeParameterList<'_>) -> Vec<SiteOrigin> {
    params
        .parameters
        .iter()
        .filter(|p| p.is_promoted_property() && p.hooks.is_some())
        .map(|p| SiteOrigin {
            span: to_span(p.span()),
            kind: SiteKind::Operator {
                family: F::MagicProp,
                construct: C::Write,
                receivers: vec![Some(EffectRecv::This)],
                member: Some(strip_dollar(bytes_to_string(p.variable.name))),
            },
            guards: Vec::new(),
            operands: Some(vec![ArgShape::Unknown]),
            ref_targets: None,
            const_args: ConstArgs::default(),
        })
        .collect()
}

/// Record the operator sites `node` is, if it is one. Returns `true` when this
/// already walked the node's children (a role-taking form: its targets are
/// walked by [`chain`], so the generic walk must not see them again).
pub(super) fn lower(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) -> bool {
    match node {
        Node::Binary(b) => binary(b, sx, out),
        Node::Assignment(a) => assignment(a, sx, out),
        Node::UnaryPrefix(u) => unary_prefix(u, sx, out),
        Node::UnaryPostfix(u) => {
            chain(u.operand, C::ReadWrite, sx, out);
            true
        }
        Node::IssetConstruct(i) => {
            i.values.iter().for_each(|v| chain(v, C::Isset, sx, out));
            true
        }
        Node::EmptyConstruct(e) => {
            chain(e.value, C::Empty, sx, out);
            true
        }
        Node::Unset(u) => {
            u.values.iter().for_each(|v| chain(v, C::Unset, sx, out));
            true
        }
        Node::ForeachValueTarget(t) => {
            target(t.value, sx, out);
            true
        }
        Node::ForeachKeyValueTarget(t) => {
            target(t.key, sx, out);
            target(t.value, sx, out);
            true
        }
        // A fetch in value position; a role-taking parent never lets the walk reach one.
        Node::PropertyAccess(pa) => {
            property_site(pa.object, &pa.property, to_span(pa.span()), C::Read, sx, out);
            selector_name(&pa.property, sx, out);
            false
        }
        Node::NullSafePropertyAccess(pa) => {
            property_site(pa.object, &pa.property, to_span(pa.span()), C::Read, sx, out);
            selector_name(&pa.property, sx, out);
            false
        }
        // `$$n`, `${$e}` and the name of `P::$$n`: the name is converted to a string.
        Node::Variable(v) => {
            variable_name(v, sx, out);
            false
        }
        Node::ArrayAccess(aa) => {
            offset_site(aa.array, to_span(aa.span()), C::Read, sx, out);
            false
        }
        Node::ArrayAppend(ap) => {
            offset_site(ap.array, to_span(ap.span()), C::Write, sx, out);
            false
        }
        _ => {
            string_conversion(node, sx, out);
            iterate_and_clone(node, sx, out);
            false
        }
    }
}

/// The ToString sites that are not a binary operator or an assignment: casts,
/// output, interpolation, `switch`.
fn string_conversion(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    match node {
        Node::Echo(e) => {
            let values: Vec<&Expression<'_>> = e.values.iter().map(|v| &**v).collect();
            push(F::ToString, C::Echo, e.span(), None, &values, sx, out);
        }
        Node::EchoTag(e) => {
            let values: Vec<&Expression<'_>> = e.values.iter().map(|v| &**v).collect();
            push(F::ToString, C::Echo, e.span(), None, &values, sx, out);
        }
        Node::PrintConstruct(p) => push(F::ToString, C::Print, p.span(), None, &[p.value], sx, out),
        Node::InterpolatedString(s) => {
            let parts = embedded(s.parts.iter());
            push(F::ToString, C::Interpolation, s.span(), None, &parts, sx, out);
        }
        Node::ShellExecuteString(s) => {
            let parts = embedded(s.parts.iter());
            push(F::ToString, C::Interpolation, s.span(), None, &parts, sx, out);
        }
        Node::DocumentString(d) => document(d, sx, out),
        Node::Switch(s) => {
            let cases: Vec<&Expression<'_>> = s
                .body
                .cases()
                .iter()
                .filter_map(|c| match c {
                    SwitchCase::Expression(c) => Some(c.expression),
                    SwitchCase::Default(_) => None,
                })
                .collect();
            if cases.iter().all(|c| non_string_scalar(c)) {
                return;
            }
            let mut operands = vec![s.expression];
            operands.extend(cases);
            let span = s.switch.span().join(s.right_parenthesis);
            push(F::ToString, C::Switch, span, None, &operands, sx, out);
        }
        _ => {}
    }
}

/// A heredoc's interpolated expressions (a nowdoc has none).
fn document(d: &DocumentString<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let parts = embedded(d.parts.iter());
    push(F::ToString, C::Heredoc, d.span(), None, &parts, sx, out);
}

/// The expressions a string's parts embed, in order.
fn embedded<'a, 'arena: 'a>(
    parts: impl Iterator<Item = &'a StringPart<'arena>>,
) -> Vec<&'a Expression<'arena>> {
    parts
        .filter_map(|p| match p {
            StringPart::Literal(_) => None,
            StringPart::Expression(e) => Some(*e),
            StringPart::BracedExpression(b) => Some(b.expression),
        })
        .collect()
}

/// `foreach`, `yield from`, a spread, and `clone`.
fn iterate_and_clone(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    match node {
        Node::Foreach(fe) => {
            // The site is the statement's, so a consumer holding the loop's span
            // (a loop region, ADR-0076) finds its own subject by that span.
            push(F::Iterate, C::Foreach, fe.span(), None, &[fe.expression], sx, out);
        }
        Node::YieldFrom(y) => {
            push(F::Iterate, C::YieldFrom, y.span(), None, &[y.iterator], sx, out);
        }
        Node::PositionalArgument(p) if p.ellipsis.is_some() => {
            push(F::Iterate, C::Spread, p.span(), None, &[p.value], sx, out);
        }
        Node::VariadicArrayElement(v) => {
            push(F::Iterate, C::Spread, v.span(), None, &[v.value], sx, out);
        }
        Node::Clone(c) => push(F::Clone, C::Clone, c.span(), None, &[c.object], sx, out),
        _ => {}
    }
}

/// `clone($o, [...])`: PHP 8.5's `clone` with a property list, which the parser
/// reads as a call of a function named `clone`. The call's own site is the
/// caller's; this adds the operator site over the cloned expression. A plain
/// `clone($o)` is a [`Node::Clone`] and not a call.
pub(super) fn clone_with(fc: &FunctionCall<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let Expression::Identifier(id) = fc.function else { return };
    if !bytes_to_string(id.value()).trim_start_matches('\\').eq_ignore_ascii_case("clone") {
        return;
    }
    let object = fc.argument_list.arguments.iter().find_map(|a| match a {
        Argument::Positional(p) if p.ellipsis.is_none() => Some(p.value),
        Argument::Named(n) if n.name.value.eq_ignore_ascii_case(b"object") => Some(n.value),
        _ => None,
    });
    if let Some(object) = object {
        push(F::Clone, C::CloneWith, fc.span(), None, &[object], sx, out);
    }
}

/// A binary operator: concatenation, a loose or ordering comparison, or `??`.
fn binary(b: &Binary<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) -> bool {
    let construct = match b.operator {
        BinaryOperator::StringConcat(_) => C::Concat,
        BinaryOperator::Equal(_)
        | BinaryOperator::NotEqual(_)
        | BinaryOperator::AngledNotEqual(_) => C::LooseCompare,
        BinaryOperator::LessThan(_)
        | BinaryOperator::LessThanOrEqual(_)
        | BinaryOperator::GreaterThan(_)
        | BinaryOperator::GreaterThanOrEqual(_)
        | BinaryOperator::Spaceship(_) => C::OrderCompare,
        BinaryOperator::NullCoalesce(_) => {
            chain(b.lhs, C::Coalesce, sx, out);
            scan_sites(&Node::Expression(b.rhs), sx, out);
            return true;
        }
        _ => return false,
    };
    let compares = construct != C::Concat;
    if !(compares && (non_string_scalar(b.lhs) || non_string_scalar(b.rhs))) {
        push(F::ToString, construct, b.span(), None, &[b.lhs, b.rhs], sx, out);
    }
    false
}

/// An assignment: its target takes the role the operator gives it, a
/// destructuring target is an offset access on the value, and `.=` also converts.
fn assignment(
    a: &mago_syntax::cst::Assignment<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) -> bool {
    match a.operator {
        AssignmentOperator::Assign(_) => {
            if let Some(elements) = pattern_elements(a.lhs) {
                let span = to_span(a.lhs.span());
                push_at(F::ArrayAccess, C::Destructure, span, None, &[a.rhs], sx, out);
                targets(elements, sx, out);
            } else {
                chain(a.lhs, C::Write, sx, out);
                offset_value(a.lhs, Some(a.rhs), sx, out);
            }
        }
        AssignmentOperator::Concat(_) => {
            let operands = [a.lhs, a.rhs];
            push(F::ToString, C::ConcatAssign, a.span(), None, &operands, sx, out);
            chain(a.lhs, C::ReadWrite, sx, out);
        }
        AssignmentOperator::Coalesce(_) => chain(a.lhs, C::CoalesceAssign, sx, out),
        _ => chain(a.lhs, C::ReadWrite, sx, out),
    }
    scan_sites(&Node::Expression(a.rhs), sx, out);
    true
}

/// A prefix operator: `&` references its operand, `++`/`--` read and write it,
/// and the string casts convert it.
fn unary_prefix(u: &UnaryPrefix<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) -> bool {
    match u.operator {
        UnaryPrefixOperator::Reference(_) => chain(u.operand, C::Reference, sx, out),
        UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_) => {
            chain(u.operand, C::ReadWrite, sx, out);
        }
        UnaryPrefixOperator::StringCast(..) | UnaryPrefixOperator::BinaryCast(..) => {
            push(F::ToString, C::Cast, u.span(), None, &[u.operand], sx, out);
            return false;
        }
        _ => return false,
    }
    true
}

/// The elements of a destructuring pattern (`[$a, $b]`, `list($a, $b)`).
fn pattern_elements<'a, 'arena>(
    expr: &'a Expression<'arena>,
) -> Option<Vec<&'a ArrayElement<'arena>>> {
    match expr.unparenthesized() {
        Expression::Array(a) => Some(a.elements.iter().collect()),
        Expression::LegacyArray(a) => Some(a.elements.iter().collect()),
        Expression::List(l) => Some(l.elements.iter().collect()),
        _ => None,
    }
}

/// The targets of a destructuring pattern: each element's target is written,
/// and a key is a read.
fn targets(elements: Vec<&ArrayElement<'_>>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    for element in elements {
        match element {
            ArrayElement::KeyValue(kv) => {
                scan_sites(&Node::Expression(kv.key), sx, out);
                target(kv.value, sx, out);
            }
            ArrayElement::Value(v) => target(v.value, sx, out),
            ArrayElement::Variadic(_) | ArrayElement::Missing(_) => {}
        }
    }
}

/// One assignment target: a nested pattern destructures an element of the value
/// (nothing names it, so its shape is unknown); anything else is written.
fn target(expr: &Expression<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    if let Some(elements) = pattern_elements(expr) {
        let span = to_span(expr.span());
        push_unknown(F::ArrayAccess, C::Destructure, span, sx, out);
        targets(elements, sx, out);
    } else {
        chain(expr, C::Write, sx, out);
        offset_value(expr, None, sx, out);
    }
}

/// Record an access chain `expr` under `role` and walk what is not part of the
/// chain. The outermost property or offset access takes `role`; each one under
/// it is an intermediate fetch ([`inner`]). Any other expression is walked as
/// the generic scan would.
fn chain(expr: &Expression<'_>, role: C, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    // This recursion does not go through `children`, which is the walk's depth
    // guard (issue #264), so it asks the guard itself.
    if stack_guard::exhausted() {
        return;
    }
    match expr.unparenthesized() {
        Expression::Access(Access::Property(pa)) => {
            property_site(pa.object, &pa.property, to_span(pa.span()), role, sx, out);
            chain(pa.object, inner(role), sx, out);
            selector(&pa.property, sx, out);
        }
        Expression::Access(Access::NullSafeProperty(pa)) => {
            property_site(pa.object, &pa.property, to_span(pa.span()), role, sx, out);
            chain(pa.object, inner(role), sx, out);
            selector(&pa.property, sx, out);
        }
        Expression::ArrayAccess(aa) => {
            offset_site(aa.array, to_span(aa.span()), role, sx, out);
            chain(aa.array, inner(role), sx, out);
            scan_sites(&Node::Expression(aa.index), sx, out);
        }
        Expression::ArrayAppend(ap) => {
            offset_site(ap.array, to_span(ap.span()), role, sx, out);
            chain(ap.array, inner(role), sx, out);
        }
        other => scan_sites(&Node::Expression(other), sx, out),
    }
}

/// The role of an access under another: `isset`, `empty`, `??` and `??=` fetch
/// their intermediates the way `isset` does (`__isset`, then `__get`; for an
/// offset, `offsetExists` first); every other context reads them.
fn inner(role: C) -> C {
    match role {
        C::Isset | C::Empty | C::Coalesce | C::CoalesceAssign => C::Isset,
        _ => C::Read,
    }
}

/// Walk a dynamic property name (`->$n`, `->{expr}`), which the engine converts
/// to a string; an identifier has nothing to walk.
fn selector(selector: &ClassLikeMemberSelector<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    if !matches!(selector, ClassLikeMemberSelector::Identifier(_)) {
        selector_name(selector, sx, out);
        scan_sites(&Node::ClassLikeMemberSelector(selector), sx, out);
    }
}

/// The ToString site of a dynamic property name: `$o->$n` and `$o->{$e}` convert
/// the name expression's value, which is an object's `__toString` when it is one.
/// A method name is not here: `$o->$n()` is a dynamic callee already.
fn selector_name(
    selector: &ClassLikeMemberSelector<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    match selector {
        ClassLikeMemberSelector::Variable(v) => {
            let name = Expression::Variable(v.clone());
            push_at(F::ToString, C::Name, to_span(v.span()), None, &[&name], sx, out);
        }
        ClassLikeMemberSelector::Expression(e) => {
            push_at(F::ToString, C::Name, to_span(e.span()), None, &[e.expression], sx, out);
        }
        ClassLikeMemberSelector::Identifier(_) | ClassLikeMemberSelector::Missing(_) => {}
    }
}

/// The ToString site of a variable-variable's name: `$$n` converts the value of
/// `$n`, `${$e}` the value of `$e`. The same node is the static property name in
/// `P::$$n`, which the construct lowering hands to the scan.
fn variable_name(variable: &Variable<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let span = to_span(variable.span());
    match variable {
        Variable::Direct(_) => {}
        Variable::Indirect(iv) => push_at(F::ToString, C::Name, span, None, &[iv.expression], sx, out),
        Variable::Nested(nv) => {
            let name = Expression::Variable((*nv.variable).clone());
            push_at(F::ToString, C::Name, span, None, &[&name], sx, out);
        }
    }
}

/// The value an offset write `$c[k] = v` stores, which a string container
/// converts to a string ([`C::OffsetValue`]). `value` is `None` for a target
/// whose value nothing names (a destructuring or `foreach` target): shown as an
/// unknown operand. Not emitted when the value is shown to hold no object, and
/// never for an append (`$c[] = v` is a fatal error on a string, not a conversion).
fn offset_value(
    target: &Expression<'_>,
    value: Option<&Expression<'_>>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let Expression::ArrayAccess(aa) = target.unparenthesized() else { return };
    let (shape, receiver) = match value {
        Some(v) => (arg_shape(v, &sx.cx.bindings), effect_recv_of_object_declared(v, sx.cx)),
        None => (ArgShape::Unknown, None),
    };
    if holds_no_object(C::OffsetValue, &shape) {
        return;
    }
    let container = container_shape_of(aa.array, &sx.cx.bindings);
    let kind = SiteKind::Operator {
        family: F::ToString,
        construct: C::OffsetValue,
        receivers: vec![receiver, None],
        member: None,
    };
    emit(kind, to_span(aa.span()), vec![shape, container], sx, out);
}

/// A property access on `object` under `role`.
fn property_site(
    object: &Expression<'_>,
    property: &ClassLikeMemberSelector<'_>,
    span: crate::ast::Span,
    role: C,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    push_at(F::MagicProp, role, span, method_name_of(property), &[object], sx, out);
}

/// An offset access on `container` under `role`.
fn offset_site(
    container: &Expression<'_>,
    span: crate::ast::Span,
    role: C,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    push_at(F::ArrayAccess, role, span, None, &[container], sx, out);
}

/// Whether `shape` shows the operand needs no site under `construct`: an
/// object-free expression, or an array (conversion and element access leave its
/// elements alone; a comparison touches them, so there only an object-free
/// expression qualifies). A variable never does, a local no more than a
/// parameter: a named call of the frame may take it by reference and store an
/// object into it, which only the resolver can tell (`Frame::held`).
fn holds_no_object(construct: C, shape: &ArgShape) -> bool {
    let comparison = matches!(construct, C::LooseCompare | C::OrderCompare | C::Switch);
    match shape {
        ArgShape::ObjectFree => true,
        ArgShape::Array => !comparison,
        _ => false,
    }
}

/// Whether `expr` is a `null`, boolean, integer or float literal (signed or
/// not), which an object compares with without being converted to a string.
fn non_string_scalar(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(Literal::String(_)) => false,
        Expression::Literal(_) => true,
        Expression::UnaryPrefix(u) => {
            matches!(u.operator, UnaryPrefixOperator::Negation(_) | UnaryPrefixOperator::Plus(_))
                && matches!(
                    u.operand.unparenthesized(),
                    Expression::Literal(Literal::Integer(_) | Literal::Float(_))
                )
        }
        _ => false,
    }
}

/// Append an operator site over `operands`, unless none can hold an object.
fn push(
    family: F,
    construct: C,
    span: mago_span::Span,
    member: Option<String>,
    operands: &[&Expression<'_>],
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    push_at(family, construct, to_span(span), member, operands, sx, out);
}

/// [`push`] at an already converted span.
fn push_at(
    family: F,
    construct: C,
    span: crate::ast::Span,
    member: Option<String>,
    operands: &[&Expression<'_>],
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let shapes: Vec<ArgShape> = operands.iter().map(|e| arg_shape(e, &sx.cx.bindings)).collect();
    if shapes.iter().all(|shape| holds_no_object(construct, shape)) {
        return;
    }
    let receivers = operands.iter().map(|e| effect_recv_of_object_declared(e, sx.cx)).collect();
    let kind = SiteKind::Operator { family, construct, receivers, member };
    emit(kind, span, shapes, sx, out);
}

/// A site over one operand nothing is shown about (a destructured element).
fn push_unknown(
    family: F,
    construct: C,
    span: crate::ast::Span,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let kind = SiteKind::Operator { family, construct, receivers: vec![None], member: None };
    emit(kind, span, vec![ArgShape::Unknown], sx, out);
}

/// Append the site, carrying `shapes` as its operands.
fn emit(
    kind: SiteKind,
    span: crate::ast::Span,
    shapes: Vec<ArgShape>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let mut site = sx.site(span, kind);
    site.operands = Some(shapes);
    out.push(site);
}
