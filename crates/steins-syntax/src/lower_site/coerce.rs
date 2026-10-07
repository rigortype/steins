//! The coercion sites (ADR-0099 §4.3's Coerce row, issue #868): the three places a
//! user type declaration converts an object to a string with no `(string)` written.
//!
//! * **A call's arguments** are not a site of their own: the call's site records them
//!   ([`SiteOrigin::args`], [`call_args`]) and the resolver reads them against the
//!   callee's parameter types, which only it can resolve.
//! * **`return <expr>`** in a function-like whose return type admits `string` is an
//!   operator site ([`OperatorConstruct::Return`]) carrying the type as written.
//! * **`$this->p = <expr>`** in a constructor is an operator site
//!   ([`OperatorConstruct::PropertyValue`]) naming the property, whose type the
//!   resolver reads off the class chain. Anywhere else the write is already a
//!   structural state construct, and a constructor's is exempt from it.
//!
//! Whether the file is coercive, which hint admits what, and what the operand's class
//! runs are the resolver's questions: nothing here reads strictness or a type's
//! meaning, only whether a `string` member is spelled.

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, ArgumentList, AssignmentOperator, Expression, FunctionLikeParameterList, Hint,
    PartialArgument, PartialArgumentList, Return, Variable,
};

use super::SiteScope;
use crate::ast::{
    ArgShape, CallArg, OperatorConstruct as C, OperatorFamily as F, SiteKind, SiteOrigin,
};
use crate::lower_arg_shape::arg_shape;
use crate::lower_effect::EffectScanCx;
use crate::lower_expr::{effect_recv_of_object_declared, method_name_of};
use crate::{bytes_to_string, to_span};

/// The return type `hint` as the resolver reads it, when it spells a `string` member: its
/// members joined by `|` (an intersection by `&`, `null` for a `?` prefix), a class name as
/// written. `None` for a type with no `string` member, which converts nothing.
pub(crate) fn return_hint_text(hint: &Hint<'_>) -> Option<String> {
    let mut members = Vec::new();
    collect(hint, &mut members);
    members.iter().any(|m| m == "string").then(|| members.join("|"))
}

/// Append the members of `hint` to `out`, lowercasing the keyword types.
fn collect(hint: &Hint<'_>, out: &mut Vec<String>) {
    let keyword = |name: &str| name.to_owned();
    match hint {
        Hint::Union(u) => {
            collect(u.left, out);
            collect(u.right, out);
        }
        Hint::Parenthesized(p) => collect(p.hint, out),
        Hint::Nullable(n) => {
            collect(n.hint, out);
            out.push(keyword("null"));
        }
        Hint::Intersection(_) => out.push(intersection(hint)),
        Hint::Identifier(id) => out.push(bytes_to_string(id.value())),
        Hint::Null(_) => out.push(keyword("null")),
        Hint::True(_) => out.push(keyword("true")),
        Hint::False(_) => out.push(keyword("false")),
        Hint::Array(_) => out.push(keyword("array")),
        Hint::Callable(_) => out.push(keyword("callable")),
        Hint::Static(_) => out.push(keyword("static")),
        Hint::Self_(_) => out.push(keyword("self")),
        Hint::Parent(_) => out.push(keyword("parent")),
        Hint::Void(_) => out.push(keyword("void")),
        Hint::Never(_) => out.push(keyword("never")),
        Hint::Float(_) => out.push(keyword("float")),
        Hint::Bool(_) => out.push(keyword("bool")),
        Hint::Integer(_) => out.push(keyword("int")),
        Hint::String(_) => out.push(keyword("string")),
        Hint::Object(_) => out.push(keyword("object")),
        Hint::Mixed(_) => out.push(keyword("mixed")),
        Hint::Iterable(_) => out.push(keyword("iterable")),
    }
}

/// An intersection type as `A&B`: a member the object satisfies only by satisfying every
/// conjunct, which the resolver does not read as any one class.
fn intersection(hint: &Hint<'_>) -> String {
    match hint {
        Hint::Intersection(i) => format!("{}&{}", intersection(i.left), intersection(i.right)),
        Hint::Parenthesized(p) => intersection(p.hint),
        Hint::Identifier(id) => bytes_to_string(id.value()),
        _ => "?".to_owned(),
    }
}

/// What a call's arguments are, for the coercion the callee's parameters run on them. Empty
/// when none can hold an object: every argument is a positional expression shown to be an
/// object-free value or an array, which no parameter converts.
pub(super) fn call_args(list: &ArgumentList<'_>, sx: &SiteScope<'_>) -> Vec<CallArg> {
    let arguments = list.arguments.iter().map(|argument| match argument {
        Argument::Positional(p) => (None, p.ellipsis.is_some(), p.value),
        Argument::Named(n) => (Some(bytes_to_string(n.name.value)), false, n.value),
    });
    args_of(arguments, sx)
}

/// [`call_args`] for the argument list of an anonymous class's `new`.
pub(super) fn partial_call_args(
    list: &PartialArgumentList<'_>,
    sx: &SiteScope<'_>,
) -> Vec<CallArg> {
    let arguments = list.arguments.iter().filter_map(|argument| match argument {
        PartialArgument::Positional(p) => Some((None, p.ellipsis.is_some(), p.value)),
        PartialArgument::Named(n) => Some((Some(bytes_to_string(n.name.value)), false, n.value)),
        _ => None,
    });
    args_of(arguments, sx)
}

/// The arguments as `(name, spread, expression)`, in source order.
fn args_of<'a, 'arena: 'a>(
    arguments: impl Iterator<Item = (Option<String>, bool, &'a Expression<'arena>)>,
    sx: &SiteScope<'_>,
) -> Vec<CallArg> {
    let args: Vec<CallArg> = arguments
        .map(|(name, spread, value)| CallArg {
            name,
            spread,
            shape: arg_shape(value, sx.cx),
            receiver: effect_recv_of_object_declared(value, sx.cx),
        })
        .collect();
    let holds_object = |a: &CallArg| a.name.is_some() || a.spread || !shows_no_object(&a.shape);
    if args.iter().any(holds_object) { args } else { Vec::new() }
}

/// The site of `return <expr>` under the frame's return type, when it spells `string`.
pub(super) fn return_site(r: &Return<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    if let (Some(hint), Some(value)) = (sx.cx.returns.as_deref(), r.value) {
        value_site(C::Return, Some(hint), (value, to_span(r.span())), sx, out);
    }
}

/// The site of an arrow function's body, which is its return value, under its return type.
pub(crate) fn arrow_return_site(
    body: &Expression<'_>,
    cx: &EffectScanCx,
    out: &mut Vec<SiteOrigin>,
) {
    let sx = SiteScope { cx, guards: &[], catch_scope: &[] };
    if let Some(hint) = cx.returns.as_deref() {
        value_site(C::Return, Some(hint), (body, to_span(body.span())), &sx, out);
    }
}

/// The site of the write `$this->p = <expr>` (or `??=`) in a constructor.
pub(super) fn property_value_site(
    a: &mago_syntax::cst::Assignment<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    if !matches!(a.operator, AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_)) {
        return;
    }
    if let Some(name) = constructor_property(a.lhs, sx) {
        value_site(C::PropertyValue, Some(&name), (a.rhs, to_span(a.span())), sx, out);
    }
}

/// The site of a constructor's `$this->p` as a destructuring or `foreach` target, which stores a
/// value nothing names (`[$this->p] = [$x]`, `foreach ($xs as $this->p)`).
pub(super) fn property_target_site(
    target: &Expression<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let Some(name) = constructor_property(target, sx) else { return };
    let kind = SiteKind::Operator {
        family: F::ToString,
        construct: C::PropertyValue,
        receivers: vec![None],
        member: Some(name),
    };
    let mut site = sx.site(to_span(target.span()), kind);
    site.operands = Some(vec![ArgShape::Unknown]);
    out.push(site);
}

/// The name of the property `$this->name` a constructor's `target` writes.
fn constructor_property(target: &Expression<'_>, sx: &SiteScope<'_>) -> Option<String> {
    if !sx.cx.constructor {
        return None;
    }
    let Expression::Access(Access::Property(pa)) = target.unparenthesized() else { return None };
    let this = matches!(pa.object.unparenthesized(),
        Expression::Variable(Variable::Direct(dv)) if bytes_to_string(dv.name) == "$this");
    if this { method_name_of(&pa.property) } else { None }
}

/// The sites of the defaults of a function-like's parameters, whose declared types convert them
/// when the argument is left out: `function f(string $s = new S)` runs `S::__toString` at the
/// call, in the file that declares the function. Each is the operator site of a return over the
/// default.
pub(crate) fn param_default_sites(
    params: &FunctionLikeParameterList<'_>,
    cx: &EffectScanCx,
    out: &mut Vec<SiteOrigin>,
) {
    let sx = SiteScope { cx, guards: &[], catch_scope: &[] };
    for param in params.parameters.iter() {
        let (Some(hint), Some(default)) = (param.hint.as_ref(), param.default_value.as_ref())
        else {
            continue;
        };
        if let Some(text) = return_hint_text(hint) {
            let span = to_span(default.value.span());
            value_site(C::Return, Some(&text), (default.value, span), &sx, out);
        }
    }
}

/// An operator site over the one operand `value`, unless it is shown to hold no object.
fn value_site(
    construct: C,
    member: Option<&str>,
    (value, span): (&Expression<'_>, crate::ast::Span),
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let shape = arg_shape(value, sx.cx);
    if shows_no_object(&shape) {
        return;
    }
    let kind = SiteKind::Operator {
        family: F::ToString,
        construct,
        receivers: vec![effect_recv_of_object_declared(value, sx.cx)],
        member: member.map(str::to_owned),
    };
    let mut site = sx.site(span, kind);
    site.operands = Some(vec![shape]);
    out.push(site);
}

/// Whether `shape` shows a value no declared type converts to a string: an object-free
/// value, or an array.
pub(super) fn shows_no_object(shape: &ArgShape) -> bool {
    matches!(shape, ArgShape::ObjectFree | ArgShape::Array)
}
